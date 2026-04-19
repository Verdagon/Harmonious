//! `upstream_monomorphizations_for` override — force local mono for consumer items.
//!
//! When consumer stubs live in their own rlib (stage 4c's separate-crate
//! model), the user bin's mono collector sees the stub crate as "upstream"
//! and asks `upstream_monomorphizations_for` whether a given generic
//! monomorphization should be linked from the upstream crate rather than
//! emitted locally. For consumer generic wrappers (e.g., the Phase-6
//! `#[inline(never)]` `__toylang_option_unwrap<T>` / `__toylang_result_unwrap<T, E>`
//! functions), the upstream rlib doesn't contain any concrete monomorphizations
//! — it has no local callers of the generic, so rustc's collector never
//! reached the generic from inside the rlib's compile. Default behavior
//! would route the user bin's link to a nonexistent upstream mono → link
//! fails with "cannot find function ...".
//!
//! Fix: return `None` for consumer DefIds so the user bin's collector
//! emits the mono locally. The plugin's backend then codegens the wrapper
//! body directly via toylang's Inkwell backend (for consumer fns) or via
//! the stage-4a partitioner pass-through (for non-consumer items that
//! happen to live in `__lang_stubs`, like the Phase-6 unwrap wrappers
//! whose bodies are real Rust and flow through rustc's codegen).
//!
//! POC #2 §4.3.D identifies this as risk #1's remedy; spike findings §4.1
//! confirms it's ~5 LoC of wiring. No rustc fork patch required — this is
//! a standard `Config::override_queries` override.

use rustc_data_structures::unord::UnordMap;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{GenericArgsRef, TyCtxt};
use rustc_span::def_id::CrateNum;

pub type UpstreamMonomorphizationsForFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    LocalDefId,
) -> Option<&'tcx UnordMap<GenericArgsRef<'tcx>, CrateNum>>;

pub fn lang_upstream_monomorphizations_for<'tcx>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
) -> Option<&'tcx UnordMap<GenericArgsRef<'tcx>, CrateNum>> {
    let def_id = local_def_id.to_def_id();
    // Force local mono for anything in `__lang_stubs`: the consumer's
    // generic wrappers (Phase-6 unwraps + any future cross-crate generic
    // stubs) must be emitted in whatever crate currently needs them, not
    // routed to a nonexistent upstream instantiation.
    if crate::is_from_lang_stubs_safe(tcx, def_id) {
        return None;
    }
    let default = crate::default_upstream_monomorphizations_for();
    default(tcx, local_def_id)
}
