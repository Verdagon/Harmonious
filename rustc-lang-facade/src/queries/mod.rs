//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! these providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `per_instance_mir` (per-Instance synthetic dep-registering bodies for
//! consumer fns), `symbol_name` (consumer symbol mapping),
//! `codegen_fn_attrs` (AvailableExternally linkage for consumer items —
//! retires the partition filter, see Option 4 / arch §F.14), and
//! `cross_crate_inlinable` (forces real `.o` symbols for cross-crate
//! inlinable items in consumer-active compiles — closes B16).
//!
//! `upstream_monomorphizations{_for}` overrides retired 2026-06-21 (A.2
//! retirement under §5.5 Step 3 — see handoff.md).
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
// `upstream_monomorphization` module retired 2026-06-21 (A.2 retirement).

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
    // A.2's `upstream_monomorphizations{_for}` overrides retired
    // 2026-06-21 (§5.5 Step 3 — see handoff.md).
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
    // Patch 5 (consumer_lang_active query + share-generics escape clause)
    // STAYS. Empirical 2026-06-21 finding: the escape clause is load-bearing
    // for RUSTC's NATURAL `upstream_monomorphizations_for` map (which has
    // entries for Rust generic items like `Vec::<i32>::new` emitted at
    // __lang_stubs's share-generics-true compile). At user_bin's
    // share_generics=false compile, without the escape clause, the
    // mangler short-circuits → picks LOCAL_CRATE disambig → mismatch
    // with where the body actually lives. Symptom: case5 fixtures
    // crash at runtime with `unreachable!()` from the stub body.
    //
    // The earlier "Step 3 makes patch 5 a no-op" claim was specific to
    // A.2's augmented map for Sky trait-impl methods (which IS empty
    // post-Step-3). For Rust items, rustc's natural map provides the
    // entries and patch 5 is genuinely load-bearing.
    //
    // Implementation note: lang_consumer_lang_active now just calls
    // crate::is_sky_active(tcx) — consolidates the marker-walk logic
    // (previously duplicated). The provider remains so rustc-internal
    // consumers (the fork's instance.rs gate) see the right value.
    providers.queries.consumer_lang_active = lang_consumer_lang_active;
}

/// Provider for the `consumer_lang_active` query (forked rustc patch 5).
/// Identical semantics to `crate::is_sky_active(tcx)`; kept as a query
/// shim until the rustc-fork patch 5 retirement lands (then this can
/// delete along with the query declaration).
pub fn lang_consumer_lang_active<'tcx>(
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
    _: (),
) -> bool {
    crate::is_sky_active(tcx)
}
