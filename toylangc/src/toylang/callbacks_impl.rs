//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, GenericArg, Ty, TyCtxt, TyKind};
use rustc_span::def_id::DefId;
use rustc_span::sym;

use rustc_lang_facade::{LangCallbacks, MonomorphizeTypeResult, MonomorphizeFnResult};
use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::registry::{ToylangRegistry, ToyFieldType};

pub struct ToylangCallbacks {
    pub registry: Arc<ToylangRegistry>,
    /// (ll_path, obj_path) for LLVM compilation. None if no external codegen.
    pub llvm_paths: Option<(PathBuf, PathBuf)>,
}

impl LangCallbacks for ToylangCallbacks {
    fn type_names(&self) -> std::collections::HashSet<String> {
        self.registry.structs.keys().cloned().collect()
    }

    fn fn_names(&self) -> std::collections::HashSet<String> {
        self.registry.functions.keys().cloned().collect()
    }

    fn after_rust_analysis<'tcx>(&self, _tcx: TyCtxt<'tcx>) {
        // TODO: toylang type checking against Rust types
    }

    fn generate_stubs(&self) -> String {
        crate::stub_gen::generate(&self.registry)
    }

    fn monomorphize_type<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx> {
        let toy_struct = self.registry.structs.get(name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_type: struct '{}' not in registry", name));

        // Build type-param substitution from the ADT's generic args.
        let subst: HashMap<&str, Ty<'tcx>> = if !toy_struct.type_params.is_empty() {
            if let TyKind::Adt(_, args) = ty.kind() {
                toy_struct.type_params.iter()
                    .enumerate()
                    .map(|(i, name)| (name.as_str(), args[i].expect_ty()))
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Map each field to a rustc Ty.
        let field_types: Vec<Ty<'tcx>> = toy_struct.fields.iter().map(|field| {
            match &field.rust_type {
                ToyFieldType::I32 => tcx.types.i32,
                ToyFieldType::I64 => tcx.types.i64,
                ToyFieldType::F64 => tcx.types.f64,
                ToyFieldType::Bool => tcx.types.bool,
                ToyFieldType::TypeParam(name) => {
                    *subst.get(name.as_str()).expect("type param not in subst")
                }
                ToyFieldType::ToyStruct(struct_name) => {
                    crate::oracle::find_local_struct_ty(tcx, struct_name)
                        .unwrap_or_else(|| panic!("[toylang] monomorphize_type: struct '{}' not found", struct_name))
                }
                ToyFieldType::RustGeneric(type_name, type_args) => {
                    resolve_rust_generic_ty(tcx, type_name, type_args, &subst)
                }
            }
        }).collect();

        MonomorphizeTypeResult {
            field_types,
        }
    }

    fn monomorphize_fn<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx> {
        // Accessor methods come in as "StructName.field_name"
        if let Some((struct_name, field_name)) = name.split_once('.') {
            // Use Instance to build a unique symbol per concrete instantiation
            let sym = rustc_lang_facade::queries::per_instance::accessor_symbol_for_instance(
                tcx, instance, struct_name, field_name,
            );
            return MonomorphizeFnResult {
                extern_symbol: sym,
                rust_deps: vec![],
            };
        }

        let toy_fn = self.registry.functions.get(name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_fn: function '{}' not in registry", name));

        // Compute symbol on the fly. For generic functions, include mangled type args.
        let extern_symbol = compute_fn_symbol(name, tcx, instance);

        // Collect Rust generic dependencies by scanning the AST body.
        let rust_deps = if let Some(ref fn_body) = toy_fn.body {
            collect_rust_deps(tcx, def_id, fn_body, &self.registry)
        } else {
            vec![]
        };

        MonomorphizeFnResult {
            extern_symbol,
            rust_deps,
        }
    }

    fn generate_and_compile<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)> {
        let (ref ll_path, ref obj_path) = self.llvm_paths.as_ref()?;

        // Walk MonoItems and codegen each consumer instance inline (same 'tcx scope).
        let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(
            tcx, &self.registry, self,
        );
        std::fs::write(ll_path, &llvm_ir)
            .expect("toylang: failed to write .ll file");
        eprintln!("[toylang] compiling LLVM IR: {} → {}", ll_path.display(), obj_path.display());
        crate::compile_llvm_ir(ll_path, obj_path);

        Some((obj_path.clone(), rust_symbols))
    }
}

/// Compute the LLVM struct type for an accessor's concrete self type.
pub fn compute_llvm_struct_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
    toy_struct: &crate::toylang::registry::ToyStruct,
    registry: &ToylangRegistry,
    pointer_bits: u64,
) -> String {
    // Get the concrete self type from the instance's generic args
    let sig = tcx.fn_sig(instance.def_id()).instantiate(tcx, instance.args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    let self_ref_ty = sig.inputs()[0]; // &Self

    if let TyKind::Ref(_, self_ty, _) = self_ref_ty.kind() {
        if let TyKind::Adt(_, args) = self_ty.kind() {
            if !args.is_empty() && !toy_struct.type_params.is_empty() {
                // Generic struct — resolve type params to LLVM types
                let mut subst = std::collections::HashMap::new();
                for (i, param_name) in toy_struct.type_params.iter().enumerate() {
                    let concrete_ty = args[i].expect_ty();
                    let llvm_ty = crate::llvm_gen::rust_ty_to_llvm_str(
                        tcx, concrete_ty, registry, pointer_bits,
                    );
                    subst.insert(param_name.clone(), llvm_ty);
                }
                let fields: Vec<String> = toy_struct.fields.iter()
                    .map(|f| crate::llvm_gen::resolve_field_type_with_subst(
                        &f.rust_type, registry, pointer_bits, &subst,
                    ))
                    .collect();
                return format!("{{ {} }}", fields.join(", "));
            }
        }
    }

    // Non-generic fallback
    crate::llvm_gen::llvm_struct_type_full_pub(toy_struct, registry, pointer_bits)
}

// ============================================================================
// Toylang-specific helpers (moved from queries/mir_build.rs)
// ============================================================================

/// Scan a Toylang function body for Rust generic function dependencies.
fn collect_rust_deps<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    fn_body: &FnBody,
    registry: &ToylangRegistry,
) -> Vec<(DefId, ty::GenericArgsRef<'tcx>)> {
    let fn_sig = tcx.fn_sig(def_id).instantiate_identity().skip_binder();

    // First try: find Vec element type from the function signature
    let mut elem_ty = find_vec_elem_ty(tcx, fn_sig);

    // Second try: if return type is a toylang struct, search its fields for Vec types
    if elem_ty.is_none() {
        if let TyKind::Adt(adt_def, args) = fn_sig.output().kind() {
            let struct_name = tcx.item_name(adt_def.did()).to_string();
            if let Some(toy_struct) = registry.structs.get(&struct_name) {
                let subst: HashMap<&str, Ty<'tcx>> = toy_struct.type_params.iter()
                    .enumerate()
                    .filter_map(|(i, name)| {
                        args.get(i).and_then(|a| a.as_type()).map(|ty| (name.as_str(), ty))
                    })
                    .collect();
                elem_ty = find_vec_in_fields_recursive(tcx, toy_struct, &subst, registry);
            }
        }
    }

    let elem_ty = match elem_ty {
        Some(t) => t,
        None => return vec![],
    };

    let mut deps = Vec::new();
    let mut needs_new = false;
    let mut needs_push = false;
    let mut needs_len = false;

    scan_body_vec_ops(fn_body, &mut needs_new, &mut needs_push, &mut needs_len);

    let new_def_id = crate::oracle::find_vec_method(tcx, "new");
    let global_ty = new_def_id.and_then(|nid| {
        crate::oracle::extract_global_ty(tcx, elem_ty, nid)
    });

    if needs_new {
        if let Some(nid) = new_def_id {
            let args = tcx.mk_args(&[GenericArg::from(elem_ty)]);
            deps.push((nid, args));
        }
    }
    if needs_push {
        if let (Some(pid), Some(gt)) = (crate::oracle::find_vec_method(tcx, "push"), global_ty) {
            let args = tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(gt)]);
            deps.push((pid, args));
        }
    }
    if needs_len {
        if let (Some(lid), Some(gt)) = (crate::oracle::find_vec_method(tcx, "len"), global_ty) {
            let args = tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(gt)]);
            deps.push((lid, args));
        }
    }

    deps
}

fn scan_body_vec_ops(body: &FnBody, new: &mut bool, push: &mut bool, len: &mut bool) {
    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::ExprStmt(expr) => scan_expr_vec_ops(expr, new, push, len),
        }
    }
    if let Some(ref ret) = body.ret {
        scan_expr_vec_ops(ret, new, push, len);
    }
}

fn scan_expr_vec_ops(expr: &Expr, new: &mut bool, push: &mut bool, len: &mut bool) {
    match expr {
        Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new" => {
            *new = true;
        }
        Expr::MethodCall { receiver, method, .. } if method == "push" => {
            *push = true;
            scan_expr_vec_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, method, .. } if method == "len" => {
            *len = true;
            scan_expr_vec_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, .. } => {
            scan_expr_vec_ops(receiver, new, push, len);
        }
        _ => {}
    }
}

/// Resolve a ToyFieldType to a rustc Ty, handling all variants.
fn resolve_field_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    ft: &ToyFieldType,
    subst: &HashMap<&str, Ty<'tcx>>,
) -> Ty<'tcx> {
    match ft {
        ToyFieldType::I32 => tcx.types.i32,
        ToyFieldType::I64 => tcx.types.i64,
        ToyFieldType::F64 => tcx.types.f64,
        ToyFieldType::Bool => tcx.types.bool,
        ToyFieldType::TypeParam(name) => {
            *subst.get(name.as_str()).expect("type param not in subst")
        }
        ToyFieldType::ToyStruct(struct_name) => {
            crate::oracle::find_local_struct_ty(tcx, struct_name)
                .unwrap_or_else(|| panic!("[toylang] resolve_field_ty: struct '{}' not found", struct_name))
        }
        ToyFieldType::RustGeneric(type_name, type_args) => {
            resolve_rust_generic_ty(tcx, type_name, type_args, subst)
        }
    }
}

/// Construct a rustc Ty for a Rust generic type like Vec<i32> or HashMap<K, V>.
fn resolve_rust_generic_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    type_args: &[ToyFieldType],
    subst: &HashMap<&str, Ty<'tcx>>,
) -> Ty<'tcx> {
    // Find the DefId for the type name
    let def_id = match type_name {
        "Vec" => tcx.get_diagnostic_item(sym::Vec)
            .expect("[toylang] Vec not found via diagnostic item"),
        other => {
            // Fallback: search local definitions
            crate::oracle::find_local_struct_ty(tcx, other)
                .map(|ty| {
                    if let TyKind::Adt(adt_def, _) = ty.kind() {
                        adt_def.did()
                    } else {
                        panic!("[toylang] resolve_rust_generic_ty: '{}' is not an ADT", other)
                    }
                })
                .unwrap_or_else(|| panic!("[toylang] resolve_rust_generic_ty: type '{}' not found", other))
        }
    };
    let adt_def = tcx.adt_def(def_id);
    let mut args: Vec<GenericArg<'tcx>> = type_args.iter()
        .map(|arg| GenericArg::from(resolve_field_ty(tcx, arg, subst)))
        .collect();

    // Some Rust types have hidden type params with defaults (e.g., Vec<T, A = Global>).
    // If we provided fewer args than the ADT expects, fill in the defaults.
    let expected_params = tcx.generics_of(def_id).count();
    if args.len() < expected_params && type_name == "Vec" {
        // Vec<T, A = Global> — get the Global allocator type
        let elem_ty = args[0].expect_ty();
        let new_def_id = crate::oracle::find_vec_method(tcx, "new")
            .expect("[toylang] Vec::new not found");
        let global_ty = crate::oracle::extract_global_ty(tcx, elem_ty, new_def_id)
            .expect("[toylang] could not extract Global allocator type");
        args.push(GenericArg::from(global_ty));
    }

    Ty::new_adt(tcx, adt_def, tcx.mk_args(&args))
}

/// Recursively search struct fields for a Vec type, resolving type params via subst.
fn find_vec_in_fields_recursive<'tcx>(
    tcx: TyCtxt<'tcx>,
    toy_struct: &crate::toylang::registry::ToyStruct,
    subst: &HashMap<&str, Ty<'tcx>>,
    registry: &ToylangRegistry,
) -> Option<Ty<'tcx>> {
    for field in &toy_struct.fields {
        match &field.rust_type {
            ToyFieldType::RustGeneric(type_name, type_args) if type_name == "Vec" && !type_args.is_empty() => {
                let resolved = resolve_field_ty(tcx, &type_args[0], subst);
                return Some(resolved);
            }
            ToyFieldType::TypeParam(param_name) => {
                // Check if the resolved type is a Vec
                if let Some(&resolved) = subst.get(param_name.as_str()) {
                    if let TyKind::Adt(adt_def, args) = resolved.kind() {
                        if Some(adt_def.did()) == tcx.get_diagnostic_item(rustc_span::sym::Vec) {
                            return Some(args[0].expect_ty());
                        }
                    }
                }
            }
            ToyFieldType::ToyStruct(struct_name) => {
                if let Some(inner) = registry.structs.get(struct_name.as_str()) {
                    if let Some(ty) = find_vec_in_fields_recursive(tcx, inner, subst, registry) {
                        return Some(ty);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Compute the extern symbol for a consumer function instance.
/// Concrete: "__toylang_impl_make_counter"
/// Generic: "__toylang_impl_wrap__i32"
fn compute_fn_symbol<'tcx>(name: &str, tcx: TyCtxt<'tcx>, instance: ty::Instance<'tcx>) -> String {
    let mut sym = format!("__toylang_impl_{}", name);
    for arg in instance.args.iter() {
        if let ty::GenericArgKind::Type(ty) = arg.unpack() {
            sym.push_str(&format!("__{}", mangle_ty_for_symbol(tcx, ty)));
        }
    }
    sym
}

fn mangle_ty_for_symbol<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>) -> String {
    match ty.kind() {
        TyKind::Int(int_ty) => int_ty.name_str().to_string(),
        TyKind::Uint(uint_ty) => uint_ty.name_str().to_string(),
        TyKind::Float(float_ty) => float_ty.name_str().to_string(),
        TyKind::Bool => "bool".to_string(),
        TyKind::Adt(adt_def, args) => {
            let name = tcx.item_name(adt_def.did()).to_string();
            if args.is_empty() {
                name
            } else {
                let arg_strs: Vec<String> = args.iter()
                    .filter_map(|a| match a.unpack() {
                        ty::GenericArgKind::Type(t) => Some(mangle_ty_for_symbol(tcx, t)),
                        _ => None,
                    })
                    .collect();
                format!("{}_{}", name, arg_strs.join("_"))
            }
        }
        _ => format!("{:?}", ty),
    }
}

/// Convert a rustc Ty to a type string usable by the parser/type resolver (e.g. "i32", "Point").
pub fn rustc_ty_to_type_string<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>) -> String {
    match ty.kind() {
        TyKind::Int(int_ty) => int_ty.name_str().to_string(),
        TyKind::Uint(uint_ty) => uint_ty.name_str().to_string(),
        TyKind::Float(float_ty) => float_ty.name_str().to_string(),
        TyKind::Bool => "bool".to_string(),
        TyKind::Adt(adt_def, args) => {
            let name = tcx.item_name(adt_def.did()).to_string();
            if args.is_empty() {
                name
            } else {
                let arg_strs: Vec<String> = args.iter()
                    .filter_map(|a| match a.unpack() {
                        ty::GenericArgKind::Type(t) => Some(rustc_ty_to_type_string(tcx, t)),
                        _ => None,
                    })
                    .collect();
                format!("{}<{}>", name, arg_strs.join(", "))
            }
        }
        _ => format!("{:?}", ty),
    }
}

fn find_vec_elem_ty<'tcx>(tcx: TyCtxt<'tcx>, fn_sig: ty::FnSig<'tcx>) -> Option<Ty<'tcx>> {
    for &ty in fn_sig.inputs().iter().chain(std::iter::once(&fn_sig.output())) {
        let inner = match ty.kind() {
            TyKind::Ref(_, inner, _) => *inner,
            _ => ty,
        };
        if let TyKind::Adt(adt_def, args) = inner.kind() {
            if Some(adt_def.did()) == tcx.get_diagnostic_item(sym::Vec) {
                return Some(args[0].expect_ty());
            }
        }
    }
    None
}
