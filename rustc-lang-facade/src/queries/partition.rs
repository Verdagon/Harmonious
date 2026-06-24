//! `collect_and_partition_mono_items` override — filter consumer items out of
//! rustc's CGU list.
//!
//! Rustc's default partitioner runs the mono collector and assigns MonoItems
//! to CodegenUnits. We delegate to that default, then reconstruct the CGUs
//! with consumer-owned items removed. Consumer items (those matching
//! `is_consumer_codegen_target`) flow through the consumer's own LLVM backend
//! path (see `toylangc::llvm_gen::generate_with_tcx`), not rustc's.
//!
//! Why return `reachable` unchanged: downstream queries inspect the reachable
//! DefIdSet to make their own decisions. Removing consumer items from
//! reachable would alter behavior for those downstream callers. The filter
//! applies only to CGU placement.
//!
//! cache-audit: collect_and_partition_mono_items's upstream declaration
//! in `rustc_middle/src/query/mod.rs` has NO `cache_on_disk_if` modifier
//! AND is marked `eval_always`. The query re-runs every compile and is
//! never disk-cached. Sky's universe state changes have no staleness
//! risk on this query. See `toylangc/tests/cache_audit.rs` for the full
//! audit table.
//!
//! **Resurrected 2026-06-22 from a51bd7c~1.** This file was retired
//! 2026-06-21 in favor of Option 4 (`codegen_fn_attrs` override stamping
//! `AvailableExternally` linkage), then restored as part of the
//! Option-4-and-patch-5 joint retirement. Empirically (§F.17 + the
//! 2026-06-22 patch-5-OFF probe at `tmp/patch5-empirical-2026-06-21/`),
//! Option 4's AvailableExternally body created a CGU-placement hazard
//! that patch 5 papered over: the intra-CGU inliner could grab the
//! `unreachable!()` stub body and inline it into `main`. The partition
//! filter avoids the hazard structurally by never letting rustc emit
//! the body to LLVM in the first place. Net: fork drops 5 → 4 patches;
//! @SBMNBIZ arcanum becomes vacuous (no AvailableExternally body to
//! protect against).
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
//! Per architecture §5.2, the "binary compile codegens everything" model
//! (now narrower per §5.5 Step 2: owning-crate codegens non-generics)
//! eliminates the need. The Phase-6 wrappers are emitted into one or
//! another stub rlib; toylang's code that calls them is emitted into the
//! same final binary at user-bin compile time (or at the stub rlib's
//! `fill_extra_modules` for owning-crate non-generics). Linker sees both
//! ends at final link, so default `Hidden` linkage on a `pub fn` in an
//! rlib suffices. The mutation is not restored; if a Phase-6 unwrap
//! test starts failing with a link error after a nightly bump, the
//! right diagnosis is "incremental partitioner has changed something
//! else," not "we need the External mutation back."
//!
//! `is_from_lang_stubs` (marker-based) is the canonical
//! cross-phase-safe predicate — see `@DPSFDOZ`. Do not introduce
//! `def_path_str`-based matching here; partitioner-time is one of the
//! non-diagnostic contexts where `def_path_str` ICEs.

use rustc_middle::mir::mono::{CodegenUnit, MonoItemPartitions};
use rustc_middle::ty::TyCtxt;

pub fn lang_collect_and_partition_mono_items<'tcx>(
    tcx: TyCtxt<'tcx>,
    key: (),
) -> MonoItemPartitions<'tcx> {
    let upstream = crate::default_collect_and_partition();
    let MonoItemPartitions {
        codegen_units: upstream_cgus,
        all_mono_items: reachable,
    } = upstream(tcx, key);

    // Reconstruct each CGU with consumer items removed. Items that survive
    // the filter (notably the Phase-6 `#[inline(never)]` generic wrappers
    // in __lang_stubs) flow through with their default `MonoItemData` — no
    // linkage mutation. See module docs for the §B2 history this retires.
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
