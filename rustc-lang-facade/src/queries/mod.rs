//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! these providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `per_instance_mir` (per-Instance synthetic dep-registering bodies for
//! consumer fns), `symbol_name` (consumer symbol mapping),
//! `collect_and_partition_mono_items` (filter consumer items out of rustc's
//! CGU list), and `cross_crate_inlinable` (forces real `.o` symbols for
//! cross-crate inlinable items in consumer-active compiles — closes B16).
//!
//! `upstream_monomorphizations{_for}` overrides retired 2026-06-21 (A.2
//! retirement under §5.5 Step 3 — see handoff.md).
//!
//! `codegen_fn_attrs` override (Option 4) retired 2026-06-22 in favor of
//! the partition filter (restored from a51bd7c~1). Empirically the
//! AvailableExternally body Option 4 left in IR created the CGU-placement
//! hazard that patch 5 papered over; restoring the partition filter
//! eliminates the hazard structurally (no AvailableExternally body to
//! protect against) and lets patch 5 retire from the fork. See arch §F.14
//! / §F.17 design history.
//!
//! `consumer_lang_active` override retired 2026-06-22 together with rustc
//! fork patch 5. With no AvailableExternally body in IR there's no CGU
//! to misplace, so the gated escape clause in `Instance::upstream_monomorphization`
//! is no longer needed. The query declaration was deleted from the rustc
//! fork.
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
//! backend provides the real definitions. The `collect_and_partition_mono_items`
//! override filters consumer items out of rustc's CGU list so the LLVM
//! backend emits no `.o` symbol for them; the consumer's `fill_extra_modules`
//! body (rustc fork patch 4) is the sole def at link time.

pub mod cross_crate_inlinable;
pub mod drop_glue;
pub mod layout;
pub mod partition;
pub mod per_instance;
pub mod symbol_name;
// `upstream_monomorphization` module retired 2026-06-21 (A.2 retirement).
// `codegen_fn_attrs` module retired 2026-06-22 (Option 4 retirement; see
// arch §F.14.1 design history).

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
    );

    providers.queries.layout_of        = layout::lang_layout_of;
    providers.queries.mir_shims        = drop_glue::lang_mir_shims;
    providers.queries.per_instance_mir = per_instance::lang_per_instance_mir;
    providers.queries.symbol_name      = symbol_name::lang_symbol_name;
    // Partition filter restored 2026-06-22 (Option 4 + patch 5 joint
    // retirement). Filters consumer items out of rustc's CGU list so
    // rustc's LLVM backend never sees them. The consumer's
    // `fill_extra_modules` body is the sole def at link time.
    providers.queries.collect_and_partition_mono_items =
        partition::lang_collect_and_partition_mono_items;
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
    // retired 2026-06-22 alongside Option 4. With the partition filter
    // restored, rustc never emits the consumer-stub `unreachable!()` body
    // to LLVM IR, so there's no AvailableExternally body to misplace and
    // no CGU-placement hazard to paper over. The query declaration was
    // deleted from the rustc fork. See arch §F.14.1 / §F.17 design history.
}
