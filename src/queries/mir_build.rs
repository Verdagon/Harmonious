#![allow(unused)]

extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_data_structures::steal::Steal;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_middle::mir::Body;
use rustc_middle::ty::{self, GenericArg, Ty, TyCtxt, TyKind};
use rustc_span::sym;
use std::sync::{Arc, OnceLock};
use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::registry::ToylangRegistry;

type MirBuiltFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>;

static REGISTRY: OnceLock<Arc<ToylangRegistry>> = OnceLock::new();
static DEFAULT_MIR_BUILT: OnceLock<MirBuiltFn> = OnceLock::new();

pub fn install_registry(r: Arc<ToylangRegistry>) {
    let _ = REGISTRY.set(r);
}

pub fn save_default(f: MirBuiltFn) {
    let _ = DEFAULT_MIR_BUILT.set(f);
}

pub fn toy_mir_built<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Steal<Body<'tcx>> {
    if let Some(fn_name) = toylang_fn_name(tcx, def_id) {
        eprintln!("[toylang] mir_built intercepted for: {}", fn_name);

        let body = if let Some(registry) = REGISTRY.get() {
            if let Some(toy_fn) = registry.functions.get(&fn_name) {
                if let Some(ref extern_sym) = toy_fn.external_symbol {
                    // External codegen path: determine if we need phantom deps
                    let rust_deps = if let Some(ref fn_body) = toy_fn.body {
                        collect_rust_deps(tcx, def_id, fn_body)
                    } else {
                        vec![]
                    };
                    eprintln!("[toylang] external codegen for '{}': {} rust deps", fn_name, rust_deps.len());

                    if rust_deps.is_empty() {
                        // Simple call stub (no Rust generic deps)
                        crate::mir_helpers::build_extern_call_body(tcx, def_id, extern_sym)
                    } else {
                        // Phantom struct call stub (triggers monomorphization)
                        crate::mir_helpers::build_phantom_call_body(
                            tcx, def_id, extern_sym, &rust_deps,
                        )
                    }
                } else {
                    // body: None — hardcoded fallback (e.g. get_x)
                    build_hardcoded(tcx, def_id, &fn_name)
                }
            } else {
                build_hardcoded(tcx, def_id, &fn_name)
            }
        } else {
            build_hardcoded(tcx, def_id, &fn_name)
        };

        return tcx.arena.alloc(Steal::new(body));
    }

    let default = DEFAULT_MIR_BUILT.get().expect("default mir_built not saved");
    default(tcx, def_id)
}

/// Scan a Toylang function body for Rust generic function dependencies.
/// Returns a list of (DefId, GenericArgs) that must be monomorphized.
fn collect_rust_deps<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    fn_body: &FnBody,
) -> Vec<(DefId, ty::GenericArgsRef<'tcx>)> {
    // Find Vec element type from function signature
    let fn_sig = tcx.fn_sig(def_id).instantiate_identity().skip_binder();
    let elem_ty = find_vec_elem_ty(tcx, fn_sig);

    let elem_ty = match elem_ty {
        Some(t) => t,
        None => return vec![], // No Vec in signature → no deps
    };

    // Look up Vec method DefIds
    let mut deps = Vec::new();
    let mut needs_new = false;
    let mut needs_push = false;
    let mut needs_len = false;

    scan_body_vec_ops(fn_body, &mut needs_new, &mut needs_push, &mut needs_len);

    // Get the Global allocator type for push/len (they take Vec<T, A>)
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
        Expr::StaticCall { ty, method, args } if ty == "Vec" && method == "new" => {
            *new = true;
        }
        Expr::MethodCall { receiver, method, args } if method == "push" => {
            *push = true;
            scan_expr_vec_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, method, args } if method == "len" => {
            *len = true;
            scan_expr_vec_ops(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, .. } => {
            scan_expr_vec_ops(receiver, new, push, len);
        }
        _ => {}
    }
}

/// Find the Vec element type from a function signature.
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

fn build_hardcoded<'tcx>(tcx: TyCtxt<'tcx>, def_id: LocalDefId, fn_name: &str) -> Body<'tcx> {
    match fn_name {
        "get_x" => crate::mir_helpers::build_const_i32_body(tcx, def_id, 42),
        name => panic!("[toylang] no body for '{}' and no AST body in registry", name),
    }
}

fn toylang_fn_name(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<String> {
    let name = tcx.opt_item_name(def_id.to_def_id())?.to_string();
    REGISTRY.get()?.functions.contains_key(&name).then_some(name)
}
