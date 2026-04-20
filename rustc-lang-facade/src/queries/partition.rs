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
//! Stage 4 intent: once this override is shipping and consumer items never
//! reach rustc's codegen dispatch, the existing `CODEGEN_SKIP_HOOK` becomes
//! vestigial — it fires only if the filter missed something. Sub-stage 4b
//! retires the hook once 4a's filter is proven exhaustive.

use rustc_hir::def_id::DefIdSet;
use rustc_middle::mir::mono::{CodegenUnit, Linkage, MonoItemData, Visibility};
use rustc_middle::ty::TyCtxt;

pub type CollectAndPartitionFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    (),
) -> (&'tcx DefIdSet, &'tcx [CodegenUnit<'tcx>]);

pub fn lang_collect_and_partition_mono_items<'tcx>(
    tcx: TyCtxt<'tcx>,
    key: (),
) -> (&'tcx DefIdSet, &'tcx [CodegenUnit<'tcx>]) {
    let upstream = crate::default_collect_and_partition();
    let (reachable, upstream_cgus) = upstream(tcx, key);

    // Stash the unfiltered upstream slice for the consumer's own MonoItems
    // walk in `generate_with_tcx`. Without this, the consumer couldn't find
    // concrete consumer Instances — rustc's CGU list is the only source for
    // collector-discovered Instances of accessor methods and consumer fns.
    crate::stash_upstream_cgus(upstream_cgus);

    // Reconstruct each CGU with consumer items removed. For items that
    // survive the filter but still live inside `__lang_stubs` (namely the
    // Phase-6 `#[inline(never)]` generic wrappers like
    // `__toylang_option_unwrap<T>` / `__toylang_result_unwrap<T, E>` —
    // real Rust functions whose bodies rustc must codegen), force
    // `(Linkage::External, Visibility::Default)`. That's what retires
    // fork patch 5 (`VISIBILITY_OVERRIDE_HOOK`): the hook used to apply
    // this linkage via `mono_item_linkage_and_visibility`; now the plugin
    // applies it directly in the CGU slice the LLVM backend reads. The
    // linkage stored in `MonoItemData` is what rustc_codegen_llvm reads
    // at emission time — it is never re-derived, so the override
    // survives to the final `.o`.
    let mut filtered_cgus: Vec<CodegenUnit<'tcx>> = Vec::with_capacity(upstream_cgus.len());
    for cgu in upstream_cgus.iter() {
        let mut new_cgu = CodegenUnit::new(cgu.name());
        for (&mono_item, &data) in cgu.items() {
            let def_id = mono_item.def_id();
            if crate::is_consumer_codegen_target(tcx, def_id) {
                continue;
            }
            let final_data = if crate::is_from_lang_stubs(tcx, def_id) {
                MonoItemData {
                    linkage: Linkage::External,
                    visibility: Visibility::Default,
                    ..data
                }
            } else {
                data
            };
            new_cgu.items_mut().insert(mono_item, final_data);
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

    (reachable, tcx.arena.alloc_from_iter(filtered_cgus))
}
