#![allow(unused)]


use rustc_hir::def_id::LocalDefId;
use rustc_middle::mir::BorrowCheckResult;
use rustc_middle::ty::TyCtxt;
use std::sync::OnceLock;

type BorrowckFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx BorrowCheckResult<'tcx>;

static DEFAULT_MIR_BORROWCK: OnceLock<BorrowckFn> = OnceLock::new();

pub fn save_default(f: BorrowckFn) {
    let _ = DEFAULT_MIR_BORROWCK.set(f);
}

pub fn toy_mir_borrowck<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx BorrowCheckResult<'tcx> {
    if is_consumer_item(tcx, def_id) {
        // Skip borrow checking — hand-built MIR bodies won't pass it.
        tcx.arena.alloc(BorrowCheckResult {
            concrete_opaque_types: Default::default(),
            closure_requirements: None,
            used_mut_upvars: Default::default(),
            tainted_by_errors: None,
        })
    } else {
        let default = DEFAULT_MIR_BORROWCK.get().expect("default mir_borrowck not saved");
        default(tcx, def_id)
    }
}

fn is_consumer_item(tcx: TyCtxt<'_>, def_id: LocalDefId) -> bool {
    // Check by file extension (for future .toylang files).
    let span = tcx.def_span(def_id);
    let file = tcx.sess.source_map().lookup_source_file(span.lo());
    if file.name.prefer_local().to_string().ends_with(".toylang") {
        return true;
    }
    // Fallback: name-based lookup via the consumer's registered function names.
    if let Some(name) = tcx.opt_item_name(def_id.to_def_id()) {
        return crate::is_consumer_fn(name.as_str());
    }
    false
}
