// Type oracle — resolves Rust generic API signatures by querying TyCtxt directly.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def::DefKind;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::def_id::DefId;
use rustc_span::sym;

/// Walk local HIR definitions to find a struct named `name`.
/// Returns the Ty<'tcx> for it (no generic args).
pub fn find_local_struct_ty<'tcx>(tcx: TyCtxt<'tcx>, name: &str) -> Option<ty::Ty<'tcx>> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) == DefKind::Struct {
            if tcx.item_name(def_id).as_str() == name {
                let adt_def = tcx.adt_def(def_id);
                return Some(ty::Ty::new_adt(tcx, adt_def, ty::List::empty()));
            }
        }
    }
    None
}

/// Find a named method in Vec's inherent impls.
pub fn find_vec_method(tcx: TyCtxt<'_>, method: &str) -> Option<DefId> {
    let vec_def_id = tcx.get_diagnostic_item(sym::Vec)?;
    for &impl_id in tcx.inherent_impls(vec_def_id) {
        for &item_id in tcx.associated_item_def_ids(impl_id) {
            if tcx.item_name(item_id).as_str() == method {
                return Some(item_id);
            }
        }
    }
    None
}

/// Extract the `Global` allocator type from Vec::new's return type.
pub fn extract_global_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    point_ty: ty::Ty<'tcx>,
    new_def_id: DefId,
) -> Option<ty::Ty<'tcx>> {
    let args = tcx.mk_args(&[ty::GenericArg::from(point_ty)]);
    let sig = tcx.fn_sig(new_def_id).instantiate(tcx, args).skip_binder();
    if let ty::TyKind::Adt(_, adt_args) = sig.output().kind() {
        Some(adt_args[1].expect_ty())
    } else {
        None
    }
}
