//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! six providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `per_instance_mir` (per-Instance synthetic dep-registering bodies for
//! consumer fns), `symbol_name` (consumer symbol mapping),
//! `collect_and_partition_mono_items` (CGU filtering), and
//! `upstream_monomorphizations_for` (force local mono for consumer items
//! under the two-crate architecture).
//!
//! `per_instance_mir` is a custom query added by the 3-patch rustc fork
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
//! backend provides the real definitions. The partitioner override in
//! `partition` removes consumer items from rustc's CGU slice before codegen
//! dispatch sees them.

pub mod drop_glue;
pub mod layout;
pub mod partition;
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
    );

    providers.queries.layout_of        = layout::lang_layout_of;
    providers.queries.mir_shims        = drop_glue::lang_mir_shims;
    providers.queries.per_instance_mir = per_instance::lang_per_instance_mir;
    providers.queries.symbol_name      = symbol_name::lang_symbol_name;
    providers.queries.collect_and_partition_mono_items = partition::lang_collect_and_partition_mono_items;
    providers.queries.upstream_monomorphizations_for =
        upstream_monomorphization::lang_upstream_monomorphizations_for;
}
