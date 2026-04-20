//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! six providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `optimized_mir` (synthetic dep-registering bodies for consumer fns),
//! `symbol_name` (consumer symbol mapping), `collect_and_partition_mono_items`
//! (CGU filtering, stage 4a), and `upstream_monomorphizations_for` (force
//! local mono for consumer items under the two-crate architecture).
//!
//! Consumer functions in `__lang_stubs` have `unreachable!()` bodies that
//! pass rustc's normal `mir_built` and borrowck pipeline. Our
//! `optimized_mir` override replaces those bodies during monomorphization
//! with a synthetic body mentioning each transitive Rust dep via
//! `ReifyFnPointer` so rustc's collector queues them (see `@SMINCZ` for
//! why this is the only mechanism that forces codegen of a generic
//! Instance); the consumer's own backend provides the real definitions.
//! The partitioner override in `partition` removes consumer items from
//! rustc's CGU slice before codegen dispatch sees them — stage 4a's
//! replacement for the retired `CODEGEN_SKIP_HOOK` fork patch.

pub mod drop_glue;
pub mod layout;
pub mod optimized_mir;
pub mod partition;
pub mod symbol_name;
pub mod upstream_monomorphization;

/// Install query overrides. Called from `LangDriver::config`.
pub fn lang_override_queries(
    _session: &rustc_session::Session,
    providers: &mut rustc_middle::util::Providers,
) {
    crate::install_query_defaults(
        providers.layout_of,
        providers.mir_shims,
        providers.symbol_name,
        providers.optimized_mir,
        providers.collect_and_partition_mono_items,
        providers.upstream_monomorphizations_for,
    );

    providers.layout_of     = layout::lang_layout_of;
    providers.mir_shims     = drop_glue::lang_mir_shims;
    providers.optimized_mir = optimized_mir::lang_optimized_mir;
    providers.symbol_name   = symbol_name::lang_symbol_name;
    providers.collect_and_partition_mono_items = partition::lang_collect_and_partition_mono_items;
    providers.upstream_monomorphizations_for =
        upstream_monomorphization::lang_upstream_monomorphizations_for;
}
