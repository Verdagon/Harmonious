//! Drop glue override — provide destructor bodies for consumer-defined types.
//!
//! When rustc drops a value of a consumer type (e.g. dropping elements of
//! `Vec<ToyPoint>`), it generates `drop_in_place::<ToyPoint>()`. This override
//! intercepts that and builds a MIR body that calls `__toylang_drop_ToyPoint(ptr)`.
//!
//! The consumer provides the `__toylang_drop_*` function — either as a real
//! function in their compiled .o, or as a small C runtime stub for testing.
//! The consumer's destructor is responsible for dropping any fields that need it.
//!
//! If the `__toylang_drop_*` symbol isn't found (no Drop impl needed), the
//! generated body is a no-op return.
//!
//! IMPORTANT: mir_shims bodies MUST call `set_required_consts` and
//! `set_mentioned_items` (both to empty vecs). The `mir_promoted` pass does NOT
//! set these for shim bodies (unlike for `mir_built` bodies). Without this, the
//! monomorphization collector panics with "have already been set" or similar.
//! This was a hard-won debugging lesson.

#![allow(unused)]

use rustc_hir::def_id::DefId;
use rustc_middle::mir::Body;
use rustc_middle::ty::{self, TyCtxt};
pub type MirShimsFn = for<'tcx> fn(TyCtxt<'tcx>, ty::InstanceKind<'tcx>) -> Body<'tcx>;

/// Generate drop glue for consumer-defined types. Falls through to rustc's
/// default for Rust types.
///
/// Intercepts `InstanceKind::DropGlue(_, Some(ty))` where `ty` is a consumer
/// type. Generates a MIR body that calls `__toylang_drop_TypeName(ptr)`.
pub fn toy_mir_shims<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::InstanceKind<'tcx>,
) -> Body<'tcx> {
    if let ty::InstanceKind::DropGlue(def_id, Some(ty)) = instance {
        if let Some(struct_name) = consumer_struct_name(tcx, ty) {
            eprintln!("[toylang] mir_shims/DropGlue intercepted for: {}", struct_name);
            return crate::mir_helpers::build_drop_call_body(tcx, def_id, ty, &struct_name);
        }
    }
    let default = crate::default_mir_shims();
    default(tcx, instance)
}

/// If this type is a consumer-defined struct, return its name.
fn consumer_struct_name<'tcx>(tcx: TyCtxt<'tcx>, ty: ty::Ty<'tcx>) -> Option<String> {
    if let ty::TyKind::Adt(adt_def, _) = ty.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        if crate::is_consumer_type(&name) && crate::is_from_lang_stubs(tcx, adt_def.did()) {
            Some(name)
        } else {
            None
        }
    } else {
        None
    }
}
