//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! these providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `per_instance_mir` (per-Instance synthetic dep-registering bodies for
//! consumer fns), `symbol_name` (consumer symbol mapping),
//! `codegen_fn_attrs` (AvailableExternally linkage for consumer items —
//! retires the partition filter, see Option 4 / arch §F.14),
//! `cross_crate_inlinable` (forces real `.o` symbols for cross-crate
//! inlinable items in consumer-active compiles — closes B16),
//! `upstream_monomorphizations_for` (force local mono for consumer items
//! under the two-crate architecture), and `upstream_monomorphizations`
//! (augment with consumer trait-impl synthesised entries).
//!
//! `per_instance_mir` is a custom query added by the rustc fork
//! (`rust-interop-architecture.md` §3.2). It replaces the retired
//! `optimized_mir` override (Approach B) per `course-correct.md` item #1.
//! The default rustc provider returns `None`; this facade override returns
//! `Some(synthetic_body)` for consumer-owned items with `instance.args`
//! substituted Sky-side before the MIR is constructed. See @SyMINCZ for why
//! ReifyFnPointer casts in that body — not symbol_name reads — are the
//! mechanism that forces the collector to codegen referenced Rust generics.
//!
//! Consumer functions in `__lang_stubs` have `unreachable!()` bodies that
//! pass rustc's normal `mir_built` and borrowck pipeline. The
//! `per_instance_mir` override replaces those bodies during monomorphization
//! with a synthetic body mentioning each transitive Rust dep via
//! `ReifyFnPointer` so rustc's collector queues them; the consumer's own
//! backend provides the real definitions. The `codegen_fn_attrs` override
//! marks those consumer items with `AvailableExternally` linkage so rustc's
//! LLVM backend emits no `.o` symbol for them, leaving the consumer's
//! `fill_extra_modules` body (rustc fork patch 4) as the sole def at link
//! time. This replaces the prior CGU-list filter (formerly
//! `queries::partition::lang_collect_and_partition_mono_items`), see arch
//! §F.14 for the retirement rationale.

pub mod codegen_fn_attrs;
pub mod cross_crate_inlinable;
pub mod drop_glue;
pub mod layout;
pub mod per_instance;
pub mod symbol_name;
pub mod upstream_monomorphization;

/// Install query overrides. Called from `LangDriver::config`.
pub fn lang_override_queries(
    _session: &rustc_session::Session,
    providers: &mut rustc_middle::util::Providers,
) {
    // The rustc nightly-2026-01-20 bump restructured `rustc_middle::util::Providers`
    // from a flat struct into `{ queries, extern_queries, hooks }` sub-structs.
    // `per_instance_mir` is fork-added (rust-interop-architecture.md §3.2) and lives
    // in `queries`; it has no `separate_provide_extern` modifier (the consumer's
    // synthetic bodies live in the consumer's own .o and are linked at final-binary
    // time — no extern-rlib path). Its upstream default returns None unconditionally,
    // so install_query_defaults does not save it.
    crate::install_query_defaults(
        providers.queries.layout_of,
        providers.queries.mir_shims,
        providers.queries.symbol_name,
        providers.queries.collect_and_partition_mono_items,
        providers.queries.upstream_monomorphizations_for,
        providers.queries.upstream_monomorphizations,
        providers.queries.cross_crate_inlinable,
        providers.extern_queries.cross_crate_inlinable,
        providers.queries.codegen_fn_attrs,
        providers.extern_queries.codegen_fn_attrs,
    );

    providers.queries.layout_of        = layout::lang_layout_of;
    providers.queries.mir_shims        = drop_glue::lang_mir_shims;
    providers.queries.per_instance_mir = per_instance::lang_per_instance_mir;
    providers.queries.symbol_name      = symbol_name::lang_symbol_name;
    // Option 4 / arch §F.14 (2026-06-20): mark consumer-defined items with
    // `AvailableExternally` linkage so rustc's LLVM backend emits no `.o`
    // symbol for them. The consumer's `fill_extra_modules` body is the sole
    // `.o` def at link time. Retires the prior `collect_and_partition_mono_items`
    // override that filtered consumer items out of the CGU list (~107 lines).
    providers.queries.codegen_fn_attrs =
        codegen_fn_attrs::lang_codegen_fn_attrs;
    providers.extern_queries.codegen_fn_attrs =
        codegen_fn_attrs::lang_extern_codegen_fn_attrs;
    // §5.5 Step 3 A/B test: A.2 (lang_upstream_monomorphizations*) is
    // suspected redundant under Step 3 because Sky's body for cross-
    // crate trait-impl methods now emits at the cascade-firing crate's
    // own compile (where the v0 mangler at both the call site AND the
    // emission site uses LOCAL_CRATE = __lang_stubs as the disambig
    // naturally; the augmented map was load-bearing only when Sky's
    // body emitted at user_bin and the mangler had to be told the
    // disambig). Re-enable if tests fail to re-prove load-bearing.
    // providers.queries.upstream_monomorphizations_for =
    //     upstream_monomorphization::lang_upstream_monomorphizations_for;
    // F2 fix (2026-06-20): force `cross_crate_inlinable` to false in
    // Sky-active compiles so rustc emits real .o symbols rather than
    // `available_externally` declarations. Sky's emitted call sites
    // reference these symbols via direct LLVM calls and can't inline
    // through rustc's IR-side inliner. See the override file for the
    // full rationale.
    providers.queries.cross_crate_inlinable =
        cross_crate_inlinable::lang_cross_crate_inlinable;
    providers.extern_queries.cross_crate_inlinable =
        cross_crate_inlinable::lang_extern_cross_crate_inlinable;
    // §5.5 Step 3 A/B test: see above. A.2 is disabled to verify
    // retirement.
    // providers.queries.upstream_monomorphizations =
    //     upstream_monomorphization::lang_upstream_monomorphizations;
    // §5.5 Step 3 finding: consumer_lang_active stays — it's load-bearing
    // for F2's `cross_crate_inlinable` override (B16) and for codegen_fn_attrs
    // gating (Option 4). Patch 5's share-generics escape clause (which
    // queries consumer_lang_active in the fork) is effectively a no-op
    // under Step 3 because A.2's augmented map is empty (we disabled it),
    // so the escape clause's `contains_key` always returns false → same
    // as no-escape path. The fork patch 5 itself stays but as a no-op
    // under Step 3. A future Sky-frontend cleanup might retire patch 5
    // proper (one fewer fork patch); that's a separate cleanup not
    // priced into this chain.
    providers.queries.consumer_lang_active = lang_consumer_lang_active;
}

/// Provider for the `consumer_lang_active` query (forked rustc patch 5).
/// Returns `true` iff *any* loaded crate (LOCAL_CRATE or any upstream rlib
/// pulled in via `extern crate`) carries `__SKY_STUBS_MARKER`. This covers
/// three compile shapes:
///
/// - **Stub rlib compile.** LOCAL_CRATE itself is a Sky stub rlib with the
///   marker → returns `true`. The stub rlib's mangler reads the augmented
///   gate, picks the right disambig for upstream-monomorphized items.
///
/// - **User-bin compile.** LOCAL_CRATE is a plain Rust bin with no marker,
///   BUT it depends on the Sky stub rlib (which has the marker). Walk
///   `tcx.crates(())` to find the marker upstream → returns `true`. The
///   user-bin's mangler then picks the upstream's disambig for the items
///   the stub rlib emitted.
///
/// - **Pure-Rust crate compiled by this rustc binary.** No marker anywhere
///   → returns `false`. Byte-identical pass-through preserved.
///
/// The default query provider returns `false`, so vanilla rustc never sees
/// `true`. The facade installs this override on every compile.
pub fn lang_consumer_lang_active<'tcx>(
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
    _: (),
) -> bool {
    use rustc_hir::def_id::{CRATE_DEF_ID, LOCAL_CRATE};
    // Local crate first (covers stub rlib compiles).
    if crate::is_from_lang_stubs(tcx, CRATE_DEF_ID.to_def_id()) {
        return true;
    }
    // Walk upstream crates (covers user-bin and Sky-lib consumer compiles).
    // `tcx.crates(())` returns every loaded extern crate, transitive deps
    // included; for each, `is_from_lang_stubs` consults the cached marker
    // check.
    for &cnum in tcx.crates(()).iter() {
        if cnum == LOCAL_CRATE {
            continue;
        }
        // Construct a synthetic DefId for the crate root.
        if crate::is_from_lang_stubs(
            tcx,
            rustc_hir::def_id::DefId { krate: cnum, index: rustc_hir::def_id::CRATE_DEF_INDEX },
        ) {
            return true;
        }
    }
    false
}
