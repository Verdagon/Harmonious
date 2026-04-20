//! Lifetime-erased stash of the upstream partitioner's CGU slice.
//!
//! **Why this exists.** Stage 4's `collect_and_partition_mono_items` override
//! filters consumer items out of the CGU slice returned to rustc. The consumer's
//! LLVM backend (`toylangc::llvm_gen::generate_with_tcx`) still needs to walk
//! the full, unfiltered CGU slice to discover concrete consumer Instances
//! (accessor methods keyed by `symbol_name` queries, entry-point consumer fns
//! whose Instances carry ABI info the extern-wrapper codegen needs). If the
//! consumer re-queried `tcx.collect_and_partition_mono_items` it would see the
//! filtered result — the exact items it needs are gone. So the partitioner
//! override stashes the upstream slice here; the consumer reads it back.
//!
//! **Safety invariant.** The stash stores a raw pointer into `tcx.arena`.
//! Callers MUST only dereference within the same `tcx` scope that populated
//! the stash. This is enforced at the type level by `upstream_cgus(tcx)`
//! taking a live `TyCtxt<'tcx>` and reconstituting the slice as
//! `&'tcx [CodegenUnit<'tcx>]` — the caller is responsible for matching `'tcx`.
//!
//! `LangDriver::config` clears the stash at the start of every compile
//! session so a new compilation starts fresh. Clearing cannot be deferred
//! to `LangCodegenBackend::codegen_crate`: under the stage 5b two-crate
//! architecture the partitioner override fires during rlib metadata setup
//! (before `codegen_crate` runs) and clearing there would wipe a valid
//! pre-codegen stash. The stash pointer itself lives in `tcx.arena`, which
//! survives for the lifetime of the `TyCtxt` it came from; reads within
//! that `TyCtxt` are sound.
//!
//! See `docs/architecture/risks.md` §B5 for the long-term risk profile of
//! the lifetime-erased stash (category B: bounded-cost redesign if rustc's
//! `CodegenUnit<'tcx>` arena semantics shift).

use rustc_middle::mir::mono::CodegenUnit;
use rustc_middle::ty::TyCtxt;
use std::sync::{Mutex, OnceLock};

/// Raw handle to the upstream CGU slice. Lifetime-erased to `'static` for
/// storage; readers reconstitute under a live `'tcx` via `upstream_cgus`.
struct UpstreamCgusStash {
    ptr: *const CodegenUnit<'static>,
    len: usize,
}

// Safety: the pointer is treated as opaque between stash and read. Actual
// dereference happens inside `upstream_cgus` under a caller-provided `TyCtxt`
// asserting the pointer's original `'tcx` is still live.
unsafe impl Send for UpstreamCgusStash {}

static STASH: OnceLock<Mutex<Option<UpstreamCgusStash>>> = OnceLock::new();

/// Populate the stash. Called by `queries::partition::lang_collect_and_partition_mono_items`
/// with the upstream provider's unfiltered slice.
pub(crate) fn stash_upstream_cgus<'tcx>(cgus: &'tcx [CodegenUnit<'tcx>]) {
    let slot = STASH.get_or_init(|| Mutex::new(None));
    let mut g = slot.lock().unwrap();
    *g = Some(UpstreamCgusStash {
        // Cast away the `'tcx` lifetime for storage. Reader must re-supply it.
        ptr: cgus.as_ptr() as *const CodegenUnit<'static>,
        len: cgus.len(),
    });
}

/// Clear the stash. Called from `LangDriver::config` at the start of every
/// compile session so a new compilation doesn't see a stale pointer from
/// a prior one. See module-level docs for why clearing lives in `config`
/// rather than `codegen_crate`.
pub(crate) fn clear_upstream_cgus() {
    if let Some(slot) = STASH.get() {
        let mut g = slot.lock().unwrap();
        *g = None;
    }
}

/// Read the stashed upstream CGU slice under a live `TyCtxt`. Panics if no
/// stash has been populated (which would mean the partitioner override never
/// fired — a bug in query wiring).
pub fn upstream_cgus<'tcx>(_tcx: TyCtxt<'tcx>) -> &'tcx [CodegenUnit<'tcx>] {
    let slot = STASH.get().expect("upstream CGU stash uninitialized");
    let g = slot.lock().unwrap();
    let raw = g.as_ref().expect(
        "upstream CGU stash empty — partitioner override did not fire before consumer read",
    );
    // Safety: the caller holds a `TyCtxt<'tcx>`; by the facade's usage
    // contract (both stash and read inside the same `codegen_crate` call on
    // the same `TyCtxt`), `'tcx` here matches the `'tcx` that populated the
    // stash. The pointer still points into that arena.
    unsafe { std::slice::from_raw_parts(raw.ptr as *const CodegenUnit<'tcx>, raw.len) }
}
