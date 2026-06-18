//! `upstream_monomorphizations` and `upstream_monomorphizations_for` overrides.
//!
//! Two queries play together here:
//!
//! - **`upstream_monomorphizations(())`** (whole-map): builds the per-DefId
//!   table of `args → CrateNum` from the metadata of every loaded rlib.
//!   Returned as `&'tcx DefIdMap<UnordMap<GenericArgsRef<'tcx>, CrateNum>>`.
//!   Sky overrides this query (Step 5) to **augment** the default-built map
//!   with consumer trait-impl method synthesised entries — the stub rlib's
//!   metadata doesn't record consumer trait-impl mono items (Sky's
//!   partition override removed them from the CGU list), so the default
//!   alone leaves user-bin's v0 mangler picking the wrong instantiating
//!   crate.
//!
//! - **`upstream_monomorphizations_for(DefId)`** (per-DefId): looks up one
//!   DefId in the whole-map. Sky overrides this query to force `None` for
//!   Phase-6 generic wrappers (`__toylang_option_unwrap<T>` etc.) so the
//!   user bin emits them locally. For consumer trait-impl methods the
//!   override passes through to the default — which now finds Sky's
//!   synthesised entries thanks to the whole-map augmentation.

use rustc_data_structures::unord::UnordMap;
use rustc_hir::def_id::{DefIdMap, LocalDefId};
use rustc_middle::ty::{GenericArgsRef, TyCtxt};
use rustc_span::def_id::CrateNum;

pub type UpstreamMonomorphizationsForFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    LocalDefId,
) -> Option<&'tcx UnordMap<GenericArgsRef<'tcx>, CrateNum>>;

pub type UpstreamMonomorphizationsFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    (),
) -> DefIdMap<UnordMap<GenericArgsRef<'tcx>, CrateNum>>;

/// Step 5: augment rustc's default-built whole-map with consumer
/// trait-impl method synthesised entries. Returns by value; rustc's
/// `arena_cache` query modifier wraps the result.
pub fn lang_upstream_monomorphizations<'tcx>(
    tcx: TyCtxt<'tcx>,
    (): (),
) -> DefIdMap<UnordMap<GenericArgsRef<'tcx>, CrateNum>> {
    let default = crate::default_upstream_monomorphizations();
    let mut map: DefIdMap<UnordMap<GenericArgsRef<'tcx>, CrateNum>> = default(tcx, ());
    for (def_id, args, crate_num) in crate::call_synthesize_upstream_monomorphizations(tcx) {
        map.entry(def_id).or_default().insert(args, crate_num);
    }
    map
}

pub fn lang_upstream_monomorphizations_for<'tcx>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
) -> Option<&'tcx UnordMap<GenericArgsRef<'tcx>, CrateNum>> {
    let def_id = local_def_id.to_def_id();
    // Two distinct categories of `is_from_lang_stubs` items:
    //
    // 1. **Phase-6 generic wrappers** (`__toylang_option_unwrap<T>` etc.).
    //    Bodies are real Rust source; rustc codegens them. The stub rlib
    //    has no callers, so its mono walker never reached them → the
    //    default returns `None`. User-bin's collector falls back to local
    //    mono and rustc emits them. The override's None-return is a
    //    redundant guard here but doesn't harm.
    //
    // 2. **Consumer trait-impl methods** (e.g.
    //    `<Wrapper<i32> as Clone>::clone`). Pass through to the default —
    //    the default `upstream_monomorphizations_for` does
    //    `tcx.upstream_monomorphizations(()).get(&def_id)`, and our
    //    whole-map override above has augmented that map with the
    //    consumer's synthesised entries. So the per-DefId lookup now finds
    //    them, and user-bin's v0 mangler picks the stub rlib's crate as
    //    the disambiguator.
    if crate::is_from_lang_stubs(tcx, def_id)
        && crate::is_consumer_trait_impl_method(tcx, def_id).is_none()
    {
        return None;
    }
    let default = crate::default_upstream_monomorphizations_for();
    default(tcx, local_def_id)
}
