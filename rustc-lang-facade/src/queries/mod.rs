//! Query override installation.
//!
//! Rustc's compilation is driven by a demand-driven query system. Each query
//! (layout_of, mir_built, mir_borrowck, mir_shims) has a provider function.
//! We replace four providers with our own, saving the originals so we can
//! fall through for non-consumer items.
//!
//! The providers are plain function pointers — not closures — because that's
//! what rustc's `Providers` struct requires. This means our override functions
//! can't capture any state. They read consumer data through the global vtable
//! installed by `install_callbacks` (see `lib.rs`).

pub mod borrowck;
pub mod drop_glue;
pub mod layout;
pub mod mir_build;
pub mod per_instance;
pub mod symbol_name;

/// Install all four query overrides. Called from `LangDriver::config`.
///
/// Must be a plain function pointer (not a closure) because
/// `Config::override_queries` has type `fn(&Session, &mut Providers)`.
/// This is a rustc constraint we cannot change.
///
/// Each override follows the same pattern:
/// 1. Check if the item is a consumer-defined type/function (by name)
/// 2. If yes: call through the vtable to the consumer's callback
/// 3. If no: fall through to rustc's saved default provider
pub fn lang_override_queries(
    _session: &rustc_session::Session,
    providers: &mut rustc_middle::util::Providers,
) {
    layout::save_default(providers.layout_of);
    borrowck::save_default(providers.mir_borrowck);
    mir_build::save_default(providers.mir_built);
    drop_glue::save_default(providers.mir_shims);
    symbol_name::save_default(providers.symbol_name);

    providers.layout_of        = layout::toy_layout_of;
    providers.mir_borrowck     = borrowck::toy_mir_borrowck;
    providers.mir_built        = mir_build::toy_mir_built;
    providers.mir_shims        = drop_glue::toy_mir_shims;
    providers.per_instance_mir = per_instance::lang_per_instance_mir;
    providers.symbol_name      = symbol_name::lang_symbol_name;
}
