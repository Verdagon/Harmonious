extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, GenericArg, Ty, TyCtxt, TyKind};
use rustc_span::sym;

use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::registry::{ToylangRegistry, ToyFieldType, ToyStruct};

const TARGET_DATALAYOUT: &str = "e-m:o-i64:64-i128:128-n32:64-S128-Fn32";
const TARGET_TRIPLE: &str = "arm64-apple-macosx11.0.0";

/// Check if a function is eligible for external LLVM compilation.
/// Must have a body. Return type must resolve to a known type.
fn is_eligible(
    _fn_name: &str,
    func: &crate::toylang::registry::ToyFunction,
    registry: &ToylangRegistry,
) -> bool {
    if func.body.is_none() {
        return false;
    }

    let ret_ty = match &func.return_ty {
        Some(t) => t,
        None => return false,
    };

    // Known return types: primitives or concrete structs or Vec<Struct>
    match ret_ty.as_str() {
        "usize" | "i32" | "i64" | "f64" | "bool" => return true,
        _ => {}
    }
    // Concrete struct in registry
    if registry.structs.contains_key(ret_ty.as_str()) {
        return true;
    }
    // Vec<T> where T is a known struct
    if ret_ty.starts_with("Vec<") && ret_ty.ends_with('>') {
        let inner = &ret_ty[4..ret_ty.len()-1];
        return registry.structs.contains_key(inner);
    }
    // Generic struct like Pair<i32, i32> — base struct must be in registry
    if let Some((base, _args)) = parse_generic_type(ret_ty) {
        return registry.structs.contains_key(base);
    }
    false
}

/// Parse "Pair<i32, i32>" → Some(("Pair", ["i32", "i32"])).
/// Returns None for non-generic types.
fn parse_generic_type(ty: &str) -> Option<(&str, Vec<&str>)> {
    let open = ty.find('<')?;
    if !ty.ends_with('>') { return None; }
    let base = &ty[..open];
    let args_str = &ty[open+1..ty.len()-1];
    let args: Vec<&str> = args_str.split(',').map(|s| s.trim()).collect();
    Some((base, args))
}

fn body_uses_vec(body: &FnBody) -> bool {
    body.stmts.iter().any(stmt_uses_vec)
        || body.ret.as_ref().map_or(false, expr_uses_vec)
}

fn stmt_uses_vec(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Let { expr, .. } => expr_uses_vec(expr),
        Stmt::ExprStmt(expr) => expr_uses_vec(expr),
    }
}

fn expr_uses_vec(expr: &Expr) -> bool {
    match expr {
        Expr::StaticCall { ty, .. } if ty == "Vec" => true,
        Expr::MethodCall { method, .. } if method == "push" || method == "len" => true,
        Expr::MethodCall { receiver, .. } => expr_uses_vec(receiver),
        _ => false,
    }
}

/// Map a ToyFieldType to its LLVM IR type name.
fn llvm_type(ft: &ToyFieldType) -> &'static str {
    match ft {
        ToyFieldType::I32 => "i32",
        ToyFieldType::I64 => "i64",
        ToyFieldType::F64 => "double",
        ToyFieldType::Bool => "i1",
        ToyFieldType::TypeParam(_) => panic!("TypeParam not supported in LLVM gen"),
    }
}

/// Generate the LLVM struct type string for a ToyStruct, e.g. "{ i32 }" or "{ i32, i32 }".
fn llvm_struct_type(s: &ToyStruct) -> String {
    let fields: Vec<&str> = s.fields.iter().map(|f| llvm_type(&f.rust_type)).collect();
    format!("{{ {} }}", fields.join(", "))
}

/// Resolve a generic struct's LLVM type by substituting type params.
/// E.g. Pair<A,B> with args ["i32","i32"] → "{ i32, i32 }"
fn resolve_generic_struct_type(toy_struct: &ToyStruct, type_args: &[&str]) -> String {
    let fields: Vec<String> = toy_struct.fields.iter().map(|f| {
        resolve_field_type(&f.rust_type, &toy_struct.type_params, type_args).to_string()
    }).collect();
    format!("{{ {} }}", fields.join(", "))
}

/// Resolve a single field type, substituting type params with concrete LLVM types.
fn resolve_field_type(ft: &ToyFieldType, type_params: &[String], type_args: &[&str]) -> &'static str {
    match ft {
        ToyFieldType::I32 => "i32",
        ToyFieldType::I64 => "i64",
        ToyFieldType::F64 => "double",
        ToyFieldType::Bool => "i1",
        ToyFieldType::TypeParam(name) => {
            let idx = type_params.iter().position(|p| p == name)
                .unwrap_or_else(|| panic!("type param '{}' not found in struct", name));
            llvm_param_type(type_args[idx])
        }
    }
}

/// Lower a return expression for a generic struct (substituting type params).
fn lower_ret_expr_generic(
    expr: &Expr,
    struct_ty: &str,
    toy_struct: &ToyStruct,
    type_args: &[&str],
) -> String {
    match expr {
        Expr::StructLit { fields, .. } => {
            let all_const = fields.iter().all(|(_, e)| matches!(e, Expr::IntLit(_)));
            if all_const {
                let mut field_vals = Vec::new();
                for (field_name, field_expr) in fields {
                    let toy_field = toy_struct.fields.iter()
                        .find(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                    let ty = resolve_field_type(&toy_field.rust_type, &toy_struct.type_params, type_args);
                    field_vals.push(lower_field_val(field_expr, ty));
                }
                format!("  ret {} {{ {} }}", struct_ty, field_vals.join(", "))
            } else {
                let mut lines = Vec::new();
                for (i, (field_name, field_expr)) in fields.iter().enumerate() {
                    let toy_field = toy_struct.fields.iter()
                        .find(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                    let ty = resolve_field_type(&toy_field.rust_type, &toy_struct.type_params, type_args);
                    let val = match field_expr {
                        Expr::IntLit(n) => format!("{}", n),
                        Expr::Var(name) => format!("%{}", name),
                        _ => panic!("LLVM gen: unsupported expr in struct field"),
                    };
                    let src = if i == 0 { "undef".to_string() } else { format!("%agg.{}", i - 1) };
                    lines.push(format!("  %agg.{} = insertvalue {} {}, {} {}, {}",
                        i, struct_ty, src, ty, val, i));
                }
                let last_idx = fields.len() - 1;
                lines.push(format!("  ret {} %agg.{}", struct_ty, last_idx));
                lines.join("\n")
            }
        }
        _ => panic!("LLVM gen: only StructLit return supported for generic structs"),
    }
}

/// Find a Rust function's LocalDefId by name.
fn find_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<LocalDefId> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if matches!(tcx.def_kind(local_def_id), DefKind::Fn) {
            if tcx.item_name(local_def_id.to_def_id()).as_str() == name {
                return Some(local_def_id);
            }
        }
    }
    None
}

/// Lower a struct return expression using the alloca+GEP+store+load pattern
/// for ABI-correct returns. The struct is built in memory, then loaded as
/// the coerced scalar type (e.g., i64 for { i32, i32 } on aarch64).
fn lower_ret_coerced(
    expr: &Expr,
    struct_ty: &str,
    coerced_ty: &str,
    field_resolver: &dyn Fn(&str) -> String,
) -> String {
    match expr {
        Expr::StructLit { fields, .. } => {
            let mut lines = Vec::new();
            lines.push(format!("  %retval = alloca {}, align 4", struct_ty));

            for (i, (field_name, field_expr)) in fields.iter().enumerate() {
                let ty = field_resolver(field_name);
                let val = match field_expr {
                    Expr::IntLit(n) => format!("{}", n),
                    Expr::Var(name) => format!("%{}", name),
                    _ => panic!("LLVM gen: unsupported field expr: {:?}", field_expr),
                };
                let gep = format!("retval_f{}", i);
                lines.push(format!(
                    "  %{} = getelementptr inbounds {}, ptr %retval, i32 0, i32 {}",
                    gep, struct_ty, i
                ));
                lines.push(format!("  store {} {}, ptr %{}", ty, val, gep));
            }

            lines.push(format!("  %result = load {}, ptr %retval, align 4", coerced_ty));
            lines.push(format!("  ret {} %result", coerced_ty));
            lines.join("\n")
        }
        _ => panic!("LLVM gen: only StructLit return supported for coerced returns"),
    }
}

/// Map a Toylang param type string to an LLVM type.
fn llvm_param_type(ty_str: &str) -> &'static str {
    match ty_str {
        "i32" => "i32",
        "i64" => "i64",
        "f64" => "double",
        "bool" => "i1",
        "usize" => "i64",
        _ => panic!("LLVM gen: unsupported param type '{}'", ty_str),
    }
}

/// Lower a field value expression to an LLVM IR value string (e.g. "i32 42" or "i32 %x").
fn lower_field_val(field_expr: &Expr, llvm_ty: &str) -> String {
    match field_expr {
        Expr::IntLit(n) => format!("{} {}", llvm_ty, n),
        Expr::Var(name) => format!("{} %{}", llvm_ty, name),
        _ => panic!("LLVM gen: unsupported expression in struct field: {:?}", field_expr),
    }
}

/// Lower a trailing return expression to LLVM IR instructions ending with `ret`.
/// Uses `insertvalue` when any field is a variable (not a constant).
fn lower_ret_expr(expr: &Expr, struct_ty: &str, toy_struct: &ToyStruct) -> String {
    match expr {
        Expr::StructLit { fields, .. } => {
            let all_const = fields.iter().all(|(_, e)| matches!(e, Expr::IntLit(_)));

            if all_const {
                let mut field_vals = Vec::new();
                for (field_name, field_expr) in fields {
                    let toy_field = toy_struct.fields.iter()
                        .find(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                    let ty = llvm_type(&toy_field.rust_type);
                    field_vals.push(lower_field_val(field_expr, ty));
                }
                format!("  ret {} {{ {} }}", struct_ty, field_vals.join(", "))
            } else {
                let mut lines = Vec::new();
                for (i, (field_name, field_expr)) in fields.iter().enumerate() {
                    let toy_field = toy_struct.fields.iter()
                        .find(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                    let ty = llvm_type(&toy_field.rust_type);
                    let val = match field_expr {
                        Expr::IntLit(n) => format!("{}", n),
                        Expr::Var(name) => format!("%{}", name),
                        _ => panic!("LLVM gen: unsupported expr in struct field: {:?}", field_expr),
                    };
                    let src = if i == 0 { "undef".to_string() } else { format!("%agg.{}", i - 1) };
                    lines.push(format!("  %agg.{} = insertvalue {} {}, {} {}, {}",
                        i, struct_ty, src, ty, val, i));
                }
                let last_idx = fields.len() - 1;
                lines.push(format!("  ret {} %agg.{}", struct_ty, last_idx));
                lines.join("\n")
            }
        }
        _ => panic!("LLVM gen: only StructLit return expressions supported for now"),
    }
}

/// Resolve a Rust function instance and get its mangled symbol name.
fn resolve_rust_symbol<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: rustc_span::def_id::DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> String {
    let instance = ty::Instance::expect_resolve(
        tcx,
        ty::TypingEnv::fully_monomorphized(),
        def_id,
        args,
        rustc_span::DUMMY_SP,
    );
    tcx.symbol_name(instance).name.to_string()
}

/// Generate LLVM IR using tcx for symbol name resolution.
/// Returns (llvm_ir_text, rust_symbols_to_globalize).
/// Called from after_analysis where we have full access to the type context.
pub fn generate_with_tcx<'tcx>(tcx: TyCtxt<'tcx>, registry: &ToylangRegistry) -> (String, Vec<String>) {
    let mut functions_ir = Vec::new();
    let mut declares = Vec::new();
    let mut rust_symbols = Vec::new();

    for (name, func) in &registry.functions {
        if func.external_symbol.is_none() {
            continue;
        }

        let symbol = func.external_symbol.as_ref().unwrap();
        let ret_ty_name = func.return_ty.as_ref().unwrap();
        let body = func.body.as_ref().unwrap();

        let uses_vec = body_uses_vec(body);

        if uses_vec && ret_ty_name.starts_with("Vec<") {
            let func_ir = generate_vec_function(
                tcx, registry, name, symbol, ret_ty_name, func, body,
                &mut declares, &mut rust_symbols,
            );
            functions_ir.push(func_ir);
        } else if uses_vec && (ret_ty_name == "usize" || ret_ty_name == "i32" || ret_ty_name == "i64") {
            let func_ir = generate_usize_function(
                tcx, registry, name, symbol, ret_ty_name, func, body,
                &mut declares, &mut rust_symbols,
            );
            functions_ir.push(func_ir);
        } else if let Some(toy_struct) = registry.structs.get(ret_ty_name.as_str()) {
            // Simple struct-returning function — query ABI for coerced return type
            let struct_ty = llvm_struct_type(toy_struct);
            let ret_expr = body.ret.as_ref()
                .unwrap_or_else(|| panic!("LLVM gen: function '{}' has no return expression", name));

            let fn_def_id = find_fn_def_id(tcx, name)
                .unwrap_or_else(|| panic!("LLVM gen: function '{}' not found in HIR", name));
            let coerced = crate::abi_helpers::coerced_return_type(tcx, fn_def_id);

            let mut params: Vec<String> = func.params.iter()
                .map(|p| format!("{} %{}", llvm_param_type(&p.ty), p.name))
                .collect();
            let phantom_count = count_phantom_deps(body);
            for i in 0..phantom_count {
                params.push(format!("ptr %_dep{}", i));
            }
            let params_str = params.join(", ");

            let (llvm_ret_ty, ret_inst) = match coerced {
                crate::abi_helpers::CoercedReturn::Direct(ref coerced_ty) if coerced_ty == &struct_ty => {
                    // Natural type matches coerced type — use direct return
                    (struct_ty.clone(), lower_ret_expr(ret_expr, &struct_ty, toy_struct))
                }
                crate::abi_helpers::CoercedReturn::Direct(ref coerced_ty) => {
                    // Coerced to a different type — use alloca+load pattern
                    let ts = toy_struct;
                    let resolver = move |field_name: &str| -> String {
                        let f = ts.fields.iter().find(|f| f.name == field_name).unwrap();
                        llvm_type(&f.rust_type).to_string()
                    };
                    (coerced_ty.clone(), lower_ret_coerced(ret_expr, &struct_ty, coerced_ty, &resolver))
                }
                _ => {
                    (struct_ty.clone(), lower_ret_expr(ret_expr, &struct_ty, toy_struct))
                }
            };

            functions_ir.push(format!(
                "define {} @{}({}) {{\n{}\n}}",
                llvm_ret_ty, symbol, params_str, ret_inst
            ));
        } else if let Some((base_name, type_args)) = parse_generic_type(ret_ty_name) {
            // Generic struct return: Pair<i32, i32> etc. — query ABI for coerced type
            if let Some(toy_struct) = registry.structs.get(base_name) {
                let struct_ty = resolve_generic_struct_type(toy_struct, &type_args);
                let ret_expr = body.ret.as_ref()
                    .unwrap_or_else(|| panic!("LLVM gen: function '{}' has no return expression", name));

                let fn_def_id = find_fn_def_id(tcx, name)
                    .unwrap_or_else(|| panic!("LLVM gen: function '{}' not found in HIR", name));
                let coerced = crate::abi_helpers::coerced_return_type(tcx, fn_def_id);

                let mut params: Vec<String> = func.params.iter()
                    .map(|p| format!("{} %{}", llvm_param_type(&p.ty), p.name))
                    .collect();
                let phantom_count = count_phantom_deps(body);
                for i in 0..phantom_count {
                    params.push(format!("ptr %_dep{}", i));
                }
                let params_str = params.join(", ");

                let (llvm_ret_ty, ret_inst) = match coerced {
                    crate::abi_helpers::CoercedReturn::Direct(ref coerced_ty) if coerced_ty == &struct_ty => {
                        let inst = lower_ret_expr_generic(ret_expr, &struct_ty, toy_struct, &type_args);
                        (struct_ty.clone(), inst)
                    }
                    crate::abi_helpers::CoercedReturn::Direct(ref coerced_ty) => {
                        let ts = toy_struct;
                        let ta: Vec<String> = type_args.iter().map(|s| s.to_string()).collect();
                        let resolver = move |field_name: &str| -> String {
                            let f = ts.fields.iter().find(|f| f.name == field_name).unwrap();
                            let ta_refs: Vec<&str> = ta.iter().map(|s| s.as_str()).collect();
                            resolve_field_type(&f.rust_type, &ts.type_params, &ta_refs).to_string()
                        };
                        (coerced_ty.clone(), lower_ret_coerced(ret_expr, &struct_ty, coerced_ty, &resolver))
                    }
                    _ => {
                        let inst = lower_ret_expr_generic(ret_expr, &struct_ty, toy_struct, &type_args);
                        (struct_ty.clone(), inst)
                    }
                };

                functions_ir.push(format!(
                    "define {} @{}({}) {{\n{}\n}}",
                    llvm_ret_ty, symbol, params_str, ret_inst
                ));
            } else {
                panic!("LLVM gen: base struct '{}' not found in registry", base_name);
            }
        } else if ret_ty_name == "usize" {
            let func_ir = generate_usize_function(
                tcx, registry, name, symbol, ret_ty_name, func, body,
                &mut declares, &mut rust_symbols,
            );
            functions_ir.push(func_ir);
        } else {
            panic!("LLVM gen: unsupported return type '{}' for function '{}'", ret_ty_name, name);
        }
    }

    let mut ir = String::new();
    ir.push_str(&format!("target datalayout = \"{}\"\n", TARGET_DATALAYOUT));
    ir.push_str(&format!("target triple = \"{}\"\n\n", TARGET_TRIPLE));
    // Deduplicate declares
    declares.sort();
    declares.dedup();
    for decl in &declares {
        ir.push_str(decl);
        ir.push('\n');
    }
    if !declares.is_empty() {
        ir.push('\n');
    }
    for func_ir in &functions_ir {
        ir.push_str(func_ir);
        ir.push('\n');
    }

    rust_symbols.sort();
    rust_symbols.dedup();
    (ir, rust_symbols)
}

fn count_phantom_deps(body: &FnBody) -> usize {
    let mut n = false;
    let mut p = false;
    let mut l = false;
    scan_vec_ops(body, &mut n, &mut p, &mut l);
    (n as usize) + (p as usize) + (l as usize)
}

fn scan_vec_ops(body: &FnBody, new: &mut bool, push: &mut bool, len: &mut bool) {
    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::ExprStmt(expr) => scan_expr_ops(expr, new, push, len),
        }
    }
    if let Some(ref ret) = body.ret {
        scan_expr_ops(ret, new, push, len);
    }
}

fn scan_expr_ops(expr: &Expr, new: &mut bool, push: &mut bool, len: &mut bool) {
    match expr {
        Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new" => *new = true,
        Expr::MethodCall { receiver, method, .. } if method == "push" => {
            *push = true;
            scan_expr_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, method, .. } if method == "len" => {
            *len = true;
            scan_expr_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, .. } => scan_expr_ops(receiver, new, push, len),
        _ => {}
    }
}

/// Generate LLVM IR for a function that returns Vec<T> and uses Vec operations.
fn generate_vec_function<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    fn_name: &str,
    symbol: &str,
    ret_ty_name: &str,
    func: &crate::toylang::registry::ToyFunction,
    body: &FnBody,
    declares: &mut Vec<String>,
    rust_symbols: &mut Vec<String>,
) -> String {
    // Extract element type name from "Vec<Point>"
    let elem_name = &ret_ty_name[4..ret_ty_name.len()-1]; // "Point"
    let toy_struct = &registry.structs[elem_name];
    let elem_llvm_ty = llvm_struct_type(toy_struct);

    // Resolve mangled Rust symbol names via tcx
    let elem_ty = crate::oracle::find_local_struct_ty(tcx, elem_name)
        .unwrap_or_else(|| panic!("LLVM gen: struct '{}' not found", elem_name));
    let new_def_id = crate::oracle::find_vec_method(tcx, "new").unwrap();
    let push_def_id = crate::oracle::find_vec_method(tcx, "push").unwrap();
    let global_ty = crate::oracle::extract_global_ty(tcx, elem_ty, new_def_id).unwrap();

    let new_args = tcx.mk_args(&[GenericArg::from(elem_ty)]);
    let push_args = tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(global_ty)]);

    let new_sym = resolve_rust_symbol(tcx, new_def_id, new_args);
    let push_sym = resolve_rust_symbol(tcx, push_def_id, push_args);
    rust_symbols.push(new_sym.clone());
    rust_symbols.push(push_sym.clone());

    // Vec is 3 pointers = { i64, i64, i64 } on 64-bit
    let vec_llvm_ty = "{ i64, i64, i64 }";

    // Declare Rust Vec functions by mangled name.
    // Vec::new returns Vec via sret (24 bytes > 16 on aarch64 C ABI).
    // Vec::push takes &mut Vec + T by value.
    // With Fat LTO, these are merged into the same module so visibility is not an issue.
    declares.push(format!("declare void @{}(ptr sret({}) align 8)", new_sym, vec_llvm_ty));
    declares.push(format!("declare void @{}(ptr, ptr)", push_sym));

    // Count phantom deps for extra args
    let phantom_count = count_phantom_deps(body);
    let mut phantom_params = String::new();
    for i in 0..phantom_count {
        phantom_params.push_str(&format!(", ptr %_dep{}", i));
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "define void @{}(ptr sret({}) align 8 %retval{}) {{",
        symbol, vec_llvm_ty, phantom_params
    ));

    // Lower the function body: let stmts + trailing return
    let mut tmp_counter = 0usize;

    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { name, expr } => {
                lower_vec_expr(
                    &mut lines, &mut tmp_counter, expr, name,
                    &elem_llvm_ty, &new_sym, &push_sym, toy_struct,
                );
            }
            Stmt::ExprStmt(expr) => {
                let tmp = format!("_stmt{}", tmp_counter);
                tmp_counter += 1;
                lower_vec_expr(
                    &mut lines, &mut tmp_counter, expr, &tmp,
                    &elem_llvm_ty, &new_sym, &push_sym, toy_struct,
                );
            }
        }
    }

    // Trailing return: for make_vec, this is just Var("v") → copy to retval
    if let Some(ref ret_expr) = body.ret {
        match ret_expr {
            Expr::Var(name) => {
                // Copy the local Vec to retval
                lines.push(format!("  call void @llvm.memcpy.p0.p0.i64(ptr %retval, ptr %{}, i64 24, i1 false)", name));
            }
            _ => panic!("LLVM gen: unsupported return expression for Vec function: {:?}", ret_expr),
        }
    }

    lines.push("  ret void".to_string());
    lines.push("}".to_string());

    // Need llvm.memcpy intrinsic
    declares.push("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)".to_string());

    lines.join("\n")
}

/// Lower a Vec-related expression to LLVM IR lines.
fn lower_vec_expr(
    lines: &mut Vec<String>,
    tmp_counter: &mut usize,
    expr: &Expr,
    dest_name: &str,
    elem_llvm_ty: &str,
    new_sym: &str,
    push_sym: &str,
    toy_struct: &ToyStruct,
) {
    match expr {
        Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new" => {
            // Allocate stack space for the Vec and call Vec::new
            lines.push(format!("  %{} = alloca {{ i64, i64, i64 }}, align 8", dest_name));
            lines.push(format!("  call void @{}(ptr sret({{ i64, i64, i64 }}) align 8 %{})",
                new_sym, dest_name));
        }
        Expr::MethodCall { receiver, method, args } if method == "push" => {
            // Receiver must be a Var
            let recv_name = match receiver.as_ref() {
                Expr::Var(n) => n.as_str(),
                _ => panic!("LLVM gen: push receiver must be a variable"),
            };
            // Lower the argument (should be a StructLit)
            let arg = &args[0];
            let arg_name = format!("_push_arg{}", *tmp_counter);
            *tmp_counter += 1;
            lower_struct_lit_to_alloca(lines, arg, &arg_name, elem_llvm_ty, toy_struct);
            // Call push: push(&mut vec, &point)
            lines.push(format!("  call void @{}(ptr %{}, ptr %{})",
                push_sym, recv_name, arg_name));
        }
        _ => panic!("LLVM gen: unsupported Vec expression: {:?}", expr),
    }
}

/// Lower a StructLit expression to an alloca + store.
fn lower_struct_lit_to_alloca(
    lines: &mut Vec<String>,
    expr: &Expr,
    dest_name: &str,
    struct_llvm_ty: &str,
    toy_struct: &ToyStruct,
) {
    match expr {
        Expr::StructLit { fields, .. } => {
            lines.push(format!("  %{} = alloca {}, align 4", dest_name, struct_llvm_ty));
            // Store each field
            for (i, (field_name, field_expr)) in fields.iter().enumerate() {
                let toy_field = toy_struct.fields.iter()
                    .find(|f| f.name == *field_name)
                    .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                let ty = llvm_type(&toy_field.rust_type);
                let val = match field_expr {
                    Expr::IntLit(n) => format!("{}", n),
                    Expr::Var(name) => format!("%{}", name),
                    _ => panic!("LLVM gen: unsupported field expression"),
                };
                let gep_name = format!("{}_{}", dest_name, field_name);
                lines.push(format!("  %{} = getelementptr inbounds {}, ptr %{}, i32 0, i32 {}",
                    gep_name, struct_llvm_ty, dest_name, i));
                lines.push(format!("  store {} {}, ptr %{}", ty, val, gep_name));
            }
        }
        _ => panic!("LLVM gen: expected StructLit for push argument"),
    }
}

/// Generate LLVM IR for a function returning usize (like vec_len).
fn generate_usize_function<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    fn_name: &str,
    symbol: &str,
    _ret_ty_name: &str,
    func: &crate::toylang::registry::ToyFunction,
    body: &FnBody,
    declares: &mut Vec<String>,
    rust_symbols: &mut Vec<String>,
) -> String {
    // vec_len(v: &Vec<Point>) -> usize
    // Find the element type from the param type "&Vec<Point>"
    let param = &func.params[0];
    let param_ty_str = &param.ty; // e.g. "&Vec<Point>"
    let inner = param_ty_str.trim_start_matches('&');
    let elem_name = &inner[4..inner.len()-1]; // "Point"

    let elem_ty = crate::oracle::find_local_struct_ty(tcx, elem_name)
        .unwrap_or_else(|| panic!("LLVM gen: struct '{}' not found", elem_name));
    let len_def_id = crate::oracle::find_vec_method(tcx, "len").unwrap();
    let new_def_id = crate::oracle::find_vec_method(tcx, "new").unwrap();
    let global_ty = crate::oracle::extract_global_ty(tcx, elem_ty, new_def_id).unwrap();

    let len_args = tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(global_ty)]);
    let len_sym = resolve_rust_symbol(tcx, len_def_id, len_args);
    rust_symbols.push(len_sym.clone());

    // Vec::len takes *const Vec and returns usize
    declares.push(format!("declare i64 @{}(ptr)", len_sym));

    let phantom_count = count_phantom_deps(body);
    let mut params = vec![format!("ptr %{}", param.name)];
    for i in 0..phantom_count {
        params.push(format!("ptr %_dep{}", i));
    }
    let params_str = params.join(", ");

    // The body should be v.len() → just call len
    let ret_expr = body.ret.as_ref().unwrap();
    match ret_expr {
        Expr::MethodCall { receiver, method, .. } if method == "len" => {
            let recv_name = match receiver.as_ref() {
                Expr::Var(n) => n.as_str(),
                _ => panic!("LLVM gen: len receiver must be a variable"),
            };
            format!(
                "define i64 @{}({}) {{\n  %result = call i64 @{}(ptr %{})\n  ret i64 %result\n}}",
                symbol, params_str, len_sym, recv_name
            )
        }
        _ => panic!("LLVM gen: unsupported return expression for usize function"),
    }
}

/// Mark functions that were compiled by the LLVM backend by setting their external_symbol.
/// Called from main.rs before stub generation.
pub fn mark_compiled_functions(registry: &mut ToylangRegistry) {
    let names: Vec<String> = registry.functions.keys().cloned().collect();
    for name in names {
        let func = registry.functions.get(&name).unwrap();
        if is_eligible(&name, func, registry) {
            let symbol = format!("__toylang_impl_{}", name);
            registry.functions.get_mut(&name).unwrap().external_symbol = Some(symbol);
        }
    }
}
