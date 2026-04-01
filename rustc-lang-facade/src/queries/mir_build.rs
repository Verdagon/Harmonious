#![allow(unused)]


use rustc_data_structures::steal::Steal;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::mir::Body;
use rustc_middle::ty::TyCtxt;
use std::sync::OnceLock;

type MirBuiltFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>;

static DEFAULT_MIR_BUILT: OnceLock<MirBuiltFn> = OnceLock::new();

pub fn save_default(f: MirBuiltFn) {
    let _ = DEFAULT_MIR_BUILT.set(f);
}

pub fn toy_mir_built<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Steal<Body<'tcx>> {
    if let Some(fn_name) = consumer_fn_name(tcx, def_id) {
        eprintln!("[toylang] mir_built intercepted for: {}", fn_name);

        // Ask the consumer to monomorphize this function.
        // Returns the extern symbol + rust deps.
        let result = crate::call_monomorphize_fn(&fn_name, tcx, def_id);

        eprintln!("[toylang] external codegen for '{}': {} rust deps",
            fn_name, result.rust_deps.len());

        let body = crate::mir_helpers::build_extern_call_body(
            tcx, def_id, &result.extern_symbol, &result.rust_deps,
        );

        return tcx.arena.alloc(Steal::new(body));
    }

    let default = DEFAULT_MIR_BUILT.get().expect("default mir_built not saved");
    default(tcx, def_id)
}

/// If this function is a consumer-defined function, return its name.
fn consumer_fn_name(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<String> {
    let name = tcx.opt_item_name(def_id.to_def_id())?.to_string();
    if crate::is_consumer_fn(&name) {
        Some(name)
    } else {
        None
    }
}
