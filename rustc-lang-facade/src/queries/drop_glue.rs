#![allow(unused)]


use rustc_hir::def_id::DefId;
use rustc_middle::mir::Body;
use rustc_middle::ty::{self, TyCtxt};
use std::sync::OnceLock;

type MirShimsFn = for<'tcx> fn(TyCtxt<'tcx>, ty::InstanceKind<'tcx>) -> Body<'tcx>;

static DEFAULT_MIR_SHIMS: OnceLock<MirShimsFn> = OnceLock::new();

pub fn save_default(f: MirShimsFn) {
    let _ = DEFAULT_MIR_SHIMS.set(f);
}

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
    let default = DEFAULT_MIR_SHIMS.get().expect("default mir_shims not saved");
    default(tcx, instance)
}

/// If this type is a consumer-defined struct, return its name.
fn consumer_struct_name<'tcx>(tcx: TyCtxt<'tcx>, ty: ty::Ty<'tcx>) -> Option<String> {
    if let ty::TyKind::Adt(adt_def, _) = ty.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        if crate::is_consumer_type(&name) {
            Some(name)
        } else {
            None
        }
    } else {
        None
    }
}
