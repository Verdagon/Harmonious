//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. We override
//! four providers: layout_of (type layout), mir_shims (drop glue),
//! per_instance_mir (per-instantiation function stubs), and symbol_name
//! (consumer symbol mapping).
//!
//! Consumer functions in __lang_stubs have `unreachable!()` bodies that pass
//! rustc's normal mir_built and borrowck pipeline. per_instance_mir replaces
//! them at monomorphization time, and the codegen dispatch skips them.

pub mod drop_glue;
pub mod layout;
pub mod per_instance;
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
    );

    providers.layout_of        = layout::lang_layout_of;
    providers.mir_shims        = drop_glue::lang_mir_shims;
    providers.per_instance_mir = per_instance::lang_per_instance_mir;
    providers.symbol_name      = symbol_name::lang_symbol_name;
}
