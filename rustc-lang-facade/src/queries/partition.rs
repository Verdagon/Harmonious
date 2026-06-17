//! `collect_and_partition_mono_items` override — filter consumer items out of
//! rustc's CGU list.
//!
//! Rustc's default partitioner runs the mono collector and assigns MonoItems
//! to CodegenUnits. We delegate to that default, then reconstruct the CGUs
//! with consumer-owned items removed. Consumer items (those matching
//! `is_consumer_codegen_target`) flow through the consumer's own LLVM backend
//! path (see `toylangc::llvm_gen::generate_with_tcx`), not rustc's. The
//! unfiltered CGU slice is stashed via `stash_upstream_cgus` so the consumer
//! can still walk it when constructing its own Instances — the stash is
//! lifetime-erased for storage but reconstituted as `&'tcx [CodegenUnit<'tcx>]`
//! by callers holding a live `tcx`. See `upstream_cgus` in `lib.rs`.
//!
//! Why return `reachable` unchanged: downstream queries
//! (`upstream_monomorphizations_for`, `explicit_linkage`, etc.) inspect the
//! reachable DefIdSet to make their own decisions. Removing consumer items
//! from reachable would alter behavior for those downstream callers. The
//! filter applies only to CGU placement.
//!
//! Stage 4a's CGU filter replaced the `CODEGEN_SKIP_HOOK` fork patch (stage
//! 4b retired the hook once the filter was proven exhaustive — both stages
//! now shipped).
//!
//! **course-correct.md #2 — partitioner linkage-mutation retirement.**
//! Prior to Workstream A this file also forced
//! `(Linkage::External, Visibility::Default)` on every item that survived
//! the filter but still lived inside `__lang_stubs` (namely the Phase-6
//! `#[inline(never)]` generic wrappers like `__toylang_option_unwrap<T>`).
//! That was the B2 partitioner-timing risk from `docs/architecture/risks.md`
//! — the mutation depended on the assumption that `internalize_symbols` ran
//! INSIDE upstream's partitioner before our override saw the result, AND
//! that the LLVM backend read `data.linkage` from the returned CGU slice
//! without re-deriving from attributes. Both assumptions held empirically
//! but were unannounced rustc internals.
//!
//! Per architecture §5.2, Workstream A's "binary compile codegens
//! everything" model eliminates the need. The Phase-6 wrappers are emitted
//! into one or another stub rlib; toylang's code that calls them is
//! emitted into the SAME final binary at the user-bin compile (no longer
//! into a per-lib `.o`). Linker sees both ends at final link, so default
//! `Hidden` linkage on a `pub fn` in an rlib suffices. The mutation is
//! removed; if a Phase-6 unwrap test starts failing with a link error
//! after a nightly bump, the right diagnosis is "incremental partitioner
//! has changed something else," not "we need the External mutation back."
//!
//! `is_from_lang_stubs` (now marker-based per E.1) is the canonical
//! cross-phase-safe predicate — see `@DPSFDOZ`. Do not introduce
//! `def_path_str`-based matching here; partitioner-time is one of the
//! non-diagnostic contexts where `def_path_str` ICEs.

use rustc_middle::mir::mono::{CodegenUnit, MonoItemPartitions};
use rustc_middle::ty::TyCtxt;

pub type CollectAndPartitionFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    (),
) -> MonoItemPartitions<'tcx>;

pub fn lang_collect_and_partition_mono_items<'tcx>(
    tcx: TyCtxt<'tcx>,
    key: (),
) -> MonoItemPartitions<'tcx> {
    let upstream = crate::default_collect_and_partition();
    let MonoItemPartitions {
        codegen_units: upstream_cgus,
        all_mono_items: reachable,
    } = upstream(tcx, key);

    // Tier 3 #3 Phase 3: lifetime-erased CGU stash retired. The consumer
    // re-calls `default_collect_and_partition()` from inside
    // `codegen_crate` (live `'tcx`) when it needs the unfiltered slice —
    // see `toylangc::llvm_gen::generate_with_tcx`. Accessors are no
    // longer discovered through this walk (Phase 1c retired the
    // accessor branch); only Case 1b generic instantiations from Rust
    // callers remain, and re-calling the upstream provider gives them a
    // sound `'tcx`-bound slice with no unsafe pointer manipulation.

    // Reconstruct each CGU with consumer items removed. Items that survive
    // the filter (notably the Phase-6 `#[inline(never)]` generic wrappers
    // in __lang_stubs) flow through with their default
    // `MonoItemData` — no linkage mutation. See module docs for the §B2
    // history this retires.
    let mut filtered_cgus: Vec<CodegenUnit<'tcx>> = Vec::with_capacity(upstream_cgus.len());
    for cgu in upstream_cgus.iter() {
        let mut new_cgu = CodegenUnit::new(cgu.name());
        for (&mono_item, &data) in cgu.items() {
            let def_id = mono_item.def_id();
            if crate::is_consumer_codegen_target(tcx, def_id) {
                continue;
            }
            new_cgu.items_mut().insert(mono_item, data);
        }
        if cgu.is_primary() {
            new_cgu.make_primary();
        }
        if cgu.is_code_coverage_dead_code_cgu() {
            new_cgu.make_code_coverage_dead_code_cgu();
        }
        new_cgu.compute_size_estimate();
        filtered_cgus.push(new_cgu);
    }

    MonoItemPartitions {
        codegen_units: tcx.arena.alloc_from_iter(filtered_cgus),
        all_mono_items: reachable,
    }
}
