// Type oracle — resolves Rust generic API signatures by querying TyCtxt directly.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def::DefKind;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::def_id::DefId;
use rustc_span::sym;

/// Walk local HIR definitions to find a struct named `name`.
/// Also resolves `pub use` re-exports to the original struct DefId.
pub fn find_local_struct_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // First: check local struct definitions
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) == DefKind::Struct {
            if tcx.item_name(def_id).as_str() == name {
                return Some(def_id);
            }
        }
    }
    // Second: check module children for re-exports (pub use)
    find_reexported_type(tcx, name)
}

/// Search local module children for a `pub use` re-export matching `name`.
fn find_reexported_type(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    use rustc_hir::def::Res;
    // module_children_local works for local modules
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if tcx.def_kind(local_def_id.to_def_id()) != DefKind::Mod { continue; }
        for child in tcx.module_children_local(local_def_id) {
            if child.ident.as_str() == name {
                if let Res::Def(DefKind::Struct, target_def_id) = child.res {
                    return Some(target_def_id);
                }
            }
        }
    }
    // Also check the crate root
    for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
        if child.ident.as_str() == name {
            if let Res::Def(DefKind::Struct, target_def_id) = child.res {
                return Some(target_def_id);
            }
        }
    }
    None
}

pub fn find_local_struct_ty<'tcx>(tcx: TyCtxt<'tcx>, name: &str) -> Option<ty::Ty<'tcx>> {
    let def_id = find_local_struct_def_id(tcx, name)?;
    let adt_def = tcx.adt_def(def_id);
    Some(ty::Ty::new_adt(tcx, adt_def, ty::List::empty()))
}

/// Find a named method in a type's inherent impls.
pub fn find_inherent_method(tcx: TyCtxt<'_>, type_def_id: DefId, method: &str) -> Option<DefId> {
    for &impl_id in tcx.inherent_impls(type_def_id) {
        for &item_id in tcx.associated_item_def_ids(impl_id) {
            if tcx.item_name(item_id).as_str() == method {
                return Some(item_id);
            }
        }
    }
    None
}

/// Look up a Rust type's DefId by name.
/// Checks well-known diagnostic items first, then falls back to local/re-exported items.
pub fn find_rust_type_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // Well-known types via diagnostic items
    let diag = match name {
        "Vec" => tcx.get_diagnostic_item(sym::Vec),
        _ => None,
    };
    if diag.is_some() { return diag; }

    // Fall back to local definitions and pub use re-exports
    find_local_struct_def_id(tcx, name)
}
