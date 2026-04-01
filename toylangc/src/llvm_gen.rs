extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, GenericArg, TyCtxt};

use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::registry::{ToylangRegistry, ToyFieldType, ToyStruct};


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
fn resolve_generic_struct_type(toy_struct: &ToyStruct, type_args: &[&str], pointer_bits: u64) -> String {
    let fields: Vec<String> = toy_struct.fields.iter().map(|f| {
        resolve_field_type(&f.rust_type, &toy_struct.type_params, type_args, pointer_bits)
    }).collect();
    format!("{{ {} }}", fields.join(", "))
}

/// Resolve a single field type, substituting type params with concrete LLVM types.
fn resolve_field_type(ft: &ToyFieldType, type_params: &[String], type_args: &[&str], pointer_bits: u64) -> String {
    match ft {
        ToyFieldType::I32 => "i32".to_string(),
        ToyFieldType::I64 => "i64".to_string(),
        ToyFieldType::F64 => "double".to_string(),
        ToyFieldType::Bool => "i1".to_string(),
        ToyFieldType::TypeParam(name) => {
            let idx = type_params.iter().position(|p| p == name)
                .unwrap_or_else(|| panic!("type param '{}' not found in struct", name));
            llvm_param_type(type_args[idx], pointer_bits)
        }
    }
}

/// Lower a return expression for a generic struct (substituting type params).
fn lower_ret_expr_generic(
    expr: &Expr,
    struct_ty: &str,
    toy_struct: &ToyStruct,
    type_args: &[&str],
    pointer_bits: u64,
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
                    let ty = resolve_field_type(&toy_field.rust_type, &toy_struct.type_params, type_args, pointer_bits);
                    field_vals.push(lower_field_val(field_expr, &ty));
                }
                format!("  ret {} {{ {} }}", struct_ty, field_vals.join(", "))
            } else {
                let mut lines = Vec::new();
                for (i, (field_name, field_expr)) in fields.iter().enumerate() {
                    let toy_field = toy_struct.fields.iter()
                        .find(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found", field_name));
                    let ty = resolve_field_type(&toy_field.rust_type, &toy_struct.type_params, type_args, pointer_bits);
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
    align: u64,
    field_resolver: &dyn Fn(&str) -> String,
) -> String {
    match expr {
        Expr::StructLit { fields, .. } => {
            let mut lines = Vec::new();
            lines.push(format!("  %retval = alloca {}, align {}", struct_ty, align));

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

            lines.push(format!("  %result = load {}, ptr %retval, align {}", coerced_ty, align));
            lines.push(format!("  ret {} %result", coerced_ty));
            lines.join("\n")
        }
        _ => panic!("LLVM gen: only StructLit return supported for coerced returns"),
    }
}

/// Map a Toylang param type string to an LLVM type.
fn llvm_param_type(ty_str: &str, pointer_bits: u64) -> String {
    match ty_str {
        "i32" => "i32".to_string(),
        "i64" => "i64".to_string(),
        "f64" => "double".to_string(),
        "bool" => "i1".to_string(),
        "usize" => format!("i{}", pointer_bits),
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

/// Compute the alignment of a field type from its LLVM representation.
/// For TypeParam, uses the resolved type arg's known alignment.
fn field_align(ft: &ToyFieldType, type_params: &[String], type_args: &[&str]) -> u64 {
    match ft {
        ToyFieldType::I32 => 4,
        ToyFieldType::I64 => 8,
        ToyFieldType::F64 => 8,
        ToyFieldType::Bool => 1,
        ToyFieldType::TypeParam(name) => {
            let idx = type_params.iter().position(|p| p == name)
                .unwrap_or_else(|| panic!("type param '{}' not found", name));
            let resolved = type_args[idx];
            match resolved {
                "i32" => 4,
                "i64" => 8,
                "f64" => 8,
                "bool" => 1,
                _ => panic!("field_align: unsupported resolved type '{}'", resolved),
            }
        }
    }
}

/// Compute the alignment of a non-generic ToyStruct.
fn struct_align(toy_struct: &ToyStruct) -> u64 {
    struct_align_with_args(toy_struct, &[])
}

/// Compute the alignment of a ToyStruct, resolving type params with the given args.
fn struct_align_with_args(toy_struct: &ToyStruct, type_args: &[&str]) -> u64 {
    toy_struct.fields.iter()
        .map(|f| field_align(&f.rust_type, &toy_struct.type_params, type_args))
        .max()
        .unwrap_or(1)
}

/// Generate LLVM IR using tcx for symbol name resolution.
/// Returns (llvm_ir_text, rust_symbols_to_globalize).
/// Called from after_analysis where we have full access to the type context.
pub fn generate_with_tcx<'tcx>(tcx: TyCtxt<'tcx>, registry: &ToylangRegistry) -> (String, Vec<String>) {
    let mut functions_ir = Vec::new();
    let mut declares = Vec::new();
    let mut rust_symbols = Vec::new();

    // Query target info from tcx instead of hardcoding.
    let dl = &tcx.data_layout;
    let pointer_bits = dl.pointer_size.bits();
    let pointer_size = dl.pointer_size.bytes();
    let pointer_align = dl.pointer_align.abi.bytes();

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
                pointer_bits, pointer_size, pointer_align,
            );
            functions_ir.push(func_ir);
        } else if uses_vec && (ret_ty_name == "usize" || ret_ty_name == "i32" || ret_ty_name == "i64") {
            let func_ir = generate_usize_function(
                tcx, registry, name, symbol, ret_ty_name, func, body,
                &mut declares, &mut rust_symbols,
                pointer_bits,
            );
            functions_ir.push(func_ir);
        } else if let Some(toy_struct) = registry.structs.get(ret_ty_name.as_str()) {
            // Simple struct-returning function — query ABI for coerced return type
            let struct_ty = llvm_struct_type(toy_struct);
            let align = struct_align(toy_struct);
            let ret_expr = body.ret.as_ref()
                .unwrap_or_else(|| panic!("LLVM gen: function '{}' has no return expression", name));

            let fn_def_id = find_fn_def_id(tcx, name)
                .unwrap_or_else(|| panic!("LLVM gen: function '{}' not found in HIR", name));
            let coerced = rustc_lang_facade::abi_helpers::coerced_return_type(tcx, fn_def_id);

            let mut params: Vec<String> = func.params.iter()
                .map(|p| format!("{} %{}", llvm_param_type(&p.ty, pointer_bits), p.name))
                .collect();
            params.push("ptr %_deps".to_string());
            let params_str = params.join(", ");

            let (llvm_ret_ty, ret_inst) = match coerced {
                rustc_lang_facade::abi_helpers::CoercedReturn::Direct(ref coerced_ty) if coerced_ty == &struct_ty => {
                    // Natural type matches coerced type — use direct return
                    (struct_ty.clone(), lower_ret_expr(ret_expr, &struct_ty, toy_struct))
                }
                rustc_lang_facade::abi_helpers::CoercedReturn::Direct(ref coerced_ty) => {
                    // Coerced to a different type — use alloca+load pattern
                    let ts = toy_struct;
                    let resolver = move |field_name: &str| -> String {
                        let f = ts.fields.iter().find(|f| f.name == field_name).unwrap();
                        llvm_type(&f.rust_type).to_string()
                    };
                    (coerced_ty.clone(), lower_ret_coerced(ret_expr, &struct_ty, coerced_ty, align, &resolver))
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
                let struct_ty = resolve_generic_struct_type(toy_struct, &type_args, pointer_bits);
                let align = struct_align_with_args(toy_struct, &type_args);
                let ret_expr = body.ret.as_ref()
                    .unwrap_or_else(|| panic!("LLVM gen: function '{}' has no return expression", name));

                let fn_def_id = find_fn_def_id(tcx, name)
                    .unwrap_or_else(|| panic!("LLVM gen: function '{}' not found in HIR", name));
                let coerced = rustc_lang_facade::abi_helpers::coerced_return_type(tcx, fn_def_id);

                let mut params: Vec<String> = func.params.iter()
                    .map(|p| format!("{} %{}", llvm_param_type(&p.ty, pointer_bits), p.name))
                    .collect();
                params.push("ptr %_deps".to_string());
                let params_str = params.join(", ");

                let (llvm_ret_ty, ret_inst) = match coerced {
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(ref coerced_ty) if coerced_ty == &struct_ty => {
                        let inst = lower_ret_expr_generic(ret_expr, &struct_ty, toy_struct, &type_args, pointer_bits);
                        (struct_ty.clone(), inst)
                    }
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(ref coerced_ty) => {
                        let ts = toy_struct;
                        let ta: Vec<String> = type_args.iter().map(|s| s.to_string()).collect();
                        let pb = pointer_bits;
                        let resolver = move |field_name: &str| -> String {
                            let f = ts.fields.iter().find(|f| f.name == field_name).unwrap();
                            let ta_refs: Vec<&str> = ta.iter().map(|s| s.as_str()).collect();
                            resolve_field_type(&f.rust_type, &ts.type_params, &ta_refs, pb)
                        };
                        (coerced_ty.clone(), lower_ret_coerced(ret_expr, &struct_ty, coerced_ty, align, &resolver))
                    }
                    _ => {
                        let inst = lower_ret_expr_generic(ret_expr, &struct_ty, toy_struct, &type_args, pointer_bits);
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
                pointer_bits,
            );
            functions_ir.push(func_ir);
        } else {
            panic!("LLVM gen: unsupported return type '{}' for function '{}'", ret_ty_name, name);
        }
    }

    let target_datalayout = tcx.sess.target.data_layout.to_string();
    let target_triple = tcx.sess.opts.target_triple.tuple();

    let mut ir = String::new();
    ir.push_str(&format!("target datalayout = \"{}\"\n", target_datalayout));
    ir.push_str(&format!("target triple = \"{}\"\n\n", target_triple));
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

/// Generate LLVM IR for a function that returns Vec<T> and uses Vec operations.
fn generate_vec_function<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    _fn_name: &str,
    symbol: &str,
    ret_ty_name: &str,
    _func: &crate::toylang::registry::ToyFunction,
    body: &FnBody,
    declares: &mut Vec<String>,
    rust_symbols: &mut Vec<String>,
    pointer_bits: u64,
    pointer_size: u64,
    pointer_align: u64,
) -> String {
    // Extract element type name from "Vec<Point>"
    let elem_name = &ret_ty_name[4..ret_ty_name.len()-1]; // "Point"
    let toy_struct = &registry.structs[elem_name];
    let elem_llvm_ty = llvm_struct_type(toy_struct);
    let elem_align = struct_align(toy_struct);

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

    // Vec is 3 pointers: { ptr-sized-int, ptr-sized-int, ptr-sized-int }
    let ptr_ty = format!("i{}", pointer_bits);
    let vec_llvm_ty = format!("{{ {0}, {0}, {0} }}", ptr_ty);
    let vec_size = pointer_size * 3;

    // Declare Rust Vec functions by mangled name.
    // Vec::new returns Vec via sret (large struct).
    // Vec::push takes &mut Vec + T by value.
    declares.push(format!("declare void @{}(ptr sret({}) align {})", new_sym, vec_llvm_ty, pointer_align));
    declares.push(format!("declare void @{}(ptr, ptr)", push_sym));

    let mut lines = Vec::new();
    lines.push(format!(
        "define void @{}(ptr sret({}) align {} %retval, ptr %_deps) {{",
        symbol, vec_llvm_ty, pointer_align
    ));

    // Lower the function body: let stmts + trailing return
    let mut tmp_counter = 0usize;

    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { name, expr } => {
                lower_vec_expr(
                    &mut lines, &mut tmp_counter, expr, name,
                    &elem_llvm_ty, &new_sym, &push_sym, toy_struct,
                    &vec_llvm_ty, pointer_align, elem_align,
                );
            }
            Stmt::ExprStmt(expr) => {
                let tmp = format!("_stmt{}", tmp_counter);
                tmp_counter += 1;
                lower_vec_expr(
                    &mut lines, &mut tmp_counter, expr, &tmp,
                    &elem_llvm_ty, &new_sym, &push_sym, toy_struct,
                    &vec_llvm_ty, pointer_align, elem_align,
                );
            }
        }
    }

    // Trailing return: for make_vec, this is just Var("v") → copy to retval
    if let Some(ref ret_expr) = body.ret {
        match ret_expr {
            Expr::Var(name) => {
                // Copy the local Vec to retval
                lines.push(format!(
                    "  call void @llvm.memcpy.p0.p0.i{}(ptr %retval, ptr %{}, i{} {}, i1 false)",
                    pointer_bits, name, pointer_bits, vec_size
                ));
            }
            _ => panic!("LLVM gen: unsupported return expression for Vec function: {:?}", ret_expr),
        }
    }

    lines.push("  ret void".to_string());
    lines.push("}".to_string());

    // Need llvm.memcpy intrinsic
    declares.push(format!(
        "declare void @llvm.memcpy.p0.p0.i{}(ptr, ptr, i{}, i1)",
        pointer_bits, pointer_bits
    ));

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
    vec_llvm_ty: &str,
    pointer_align: u64,
    elem_align: u64,
) {
    match expr {
        Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new" => {
            // Allocate stack space for the Vec and call Vec::new
            lines.push(format!("  %{} = alloca {}, align {}", dest_name, vec_llvm_ty, pointer_align));
            lines.push(format!("  call void @{}(ptr sret({}) align {} %{})",
                new_sym, vec_llvm_ty, pointer_align, dest_name));
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
            lower_struct_lit_to_alloca(lines, arg, &arg_name, elem_llvm_ty, toy_struct, elem_align);
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
    align: u64,
) {
    match expr {
        Expr::StructLit { fields, .. } => {
            lines.push(format!("  %{} = alloca {}, align {}", dest_name, struct_llvm_ty, align));
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
    _registry: &ToylangRegistry,
    _fn_name: &str,
    symbol: &str,
    _ret_ty_name: &str,
    func: &crate::toylang::registry::ToyFunction,
    body: &FnBody,
    declares: &mut Vec<String>,
    rust_symbols: &mut Vec<String>,
    pointer_bits: u64,
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

    let usize_ty = format!("i{}", pointer_bits);

    // Vec::len takes *const Vec and returns usize
    declares.push(format!("declare {} @{}(ptr)", usize_ty, len_sym));

    let mut params = vec![format!("ptr %{}", param.name)];
    params.push("ptr %_deps".to_string());
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
                "define {} @{}({}) {{\n  %result = call {} @{}(ptr %{})\n  ret {} %result\n}}",
                usize_ty, symbol, params_str, usize_ty, len_sym, recv_name, usize_ty
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
