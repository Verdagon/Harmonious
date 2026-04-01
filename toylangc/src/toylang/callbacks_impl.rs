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
            }
        }).collect();

        MonomorphizeTypeResult {
            field_types,
            rust_deps: vec![],
        }
    }

    fn monomorphize_fn<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
    ) -> MonomorphizeFnResult<'tcx> {
        let toy_fn = self.registry.functions.get(name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_fn: function '{}' not in registry", name));

        let extern_symbol = toy_fn.external_symbol.clone()
            .unwrap_or_else(|| format!("__toylang_impl_{}", name));

        // Collect Rust generic dependencies by scanning the AST body.
        let rust_deps = if let Some(ref fn_body) = toy_fn.body {
            collect_rust_deps(tcx, def_id, fn_body)
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

        let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(tcx, &self.registry);
        std::fs::write(ll_path, &llvm_ir)
            .expect("toylang: failed to write .ll file");
        eprintln!("[toylang] compiling LLVM IR: {} → {}", ll_path.display(), obj_path.display());
        crate::compile_llvm_ir(ll_path, obj_path);

        Some((obj_path.clone(), rust_symbols))
    }
}

// ============================================================================
// Toylang-specific helpers (moved from queries/mir_build.rs)
// ============================================================================

/// Scan a Toylang function body for Rust generic function dependencies.
fn collect_rust_deps<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    fn_body: &FnBody,
) -> Vec<(DefId, ty::GenericArgsRef<'tcx>)> {
    let fn_sig = tcx.fn_sig(def_id).instantiate_identity().skip_binder();
    let elem_ty = find_vec_elem_ty(tcx, fn_sig);

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
