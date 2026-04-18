//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! four providers: `layout_of` (type layout), `mir_shims` (drop glue),
//! `optimized_mir` (synthetic dep-registering bodies for consumer fns), and
//! `symbol_name` (consumer symbol mapping).
//!
//! Consumer functions in `__lang_stubs` have `unreachable!()` bodies that
//! pass rustc's normal `mir_built` and borrowck pipeline. Our
//! `optimized_mir` override replaces those bodies during monomorphization
//! with a synthetic body mentioning each transitive Rust dep via
//! `ReifyFnPointer` so rustc's collector queues them; the consumer's own
//! backend provides the real definitions and rustc's codegen is skipped via
//! `rustc_codegen_ssa::mono_item::CODEGEN_SKIP_HOOK`.
//!
//! Stage-3 migration note: before commit, this file installed a custom
//! `per_instance_mir` query that lived behind a 4-patch rustc fork. That
//! hook has been retired in favor of the sanctioned `override_queries`
//! path on `optimized_mir`; see `handoff-optimized-mir-migration.md` and
//! `docs/reasoning/rustc-fork-design-space.md` §4.1 for context.

pub mod drop_glue;
pub mod layout;
pub mod optimized_mir;
pub mod partition;
pub mod symbol_name;

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
    );

    providers.layout_of     = layout::lang_layout_of;
    providers.mir_shims     = drop_glue::lang_mir_shims;
    providers.optimized_mir = optimized_mir::lang_optimized_mir;
    providers.symbol_name   = symbol_name::lang_symbol_name;
    providers.collect_and_partition_mono_items = partition::lang_collect_and_partition_mono_items;
}
