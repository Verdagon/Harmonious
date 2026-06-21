//! `cross_crate_inlinable` query override — addresses F2 (case 3 / case 5
//! disambig family) by forcing Sky-active compiles to emit local items
//! as real `.o` symbols rather than `available_externally` declarations.
//!
//! ## The problem
//!
//! Rustc's default behavior for items with `cross_crate_inlinable=true`
//! (i.e. `#[inline]` items or derived `#[derive(Clone)]`-style impl methods
//! that auto-attract the attribute) is to emit them with
//! `available_externally` LLVM linkage: the body is encoded in rmeta + LLVM
//! IR for inlining, but no `.o` symbol is produced. Callers that don't
//! inline the body would dangle. Normal Rust compiles dodge this because
//! `#[inline]` callees are nearly always inlined by LLVM at the call site
//! (the optimization is the point).
//!
//! Sky's emission flow breaks this assumption. Sky's
//! `__lang_stubs::clone_it<MyCounter>` body is emitted via `fill_module`
//! into the user_bin's CGU. The body's call to `<MyCounter as Clone>::clone`
//! is a real LLVM call instruction to the rustc-mangled symbol — Sky's
//! emitter doesn't have access to the rustc-side IR to inline at codegen
//! time. With `available_externally` linkage, rustc emits no `.o` symbol
//! for that Instance, so Sky's call resolves to nothing at link time.
//!
//! Symptoms: `Undefined symbols for architecture arm64:
//! <user_bin::MyCounter as core::clone::Clone>::clone` at -O>=1 (every
//! opt-level where rustc's MIR inliner / cross_crate_inlinable optimization
//! fires). At -O0 it doesn't bite because rustc defaults to share-generics
//! and emits everything locally.
//!
//! ## The fix
//!
//! When Sky's machinery is active (`consumer_lang_active(())` returns
//! true), this override returns `false` for every `cross_crate_inlinable`
//! query. The effect: rustc emits every cross-crate-inlinable item as a
//! real `.o` symbol with `LinkOnceODR` linkage rather than
//! `available_externally`. Sky's call sites resolve to a real definition.
//!
//! ## Why this preserves pass-through
//!
//! `consumer_lang_active(())` returns `false` for pure-Rust crates
//! (no marker locally, no marker upstream). The override delegates to the
//! default provider in that case, so pure-Rust crates compiled through
//! Sky's rustc binary see byte-identical behavior to vanilla rustc.
//!
//! ## Why the override is broad rather than narrow
//!
//! A narrower override would track only the def_ids Sky's per_instance_mir
//! cascade actually references. That requires Sky to thread the stashed
//! set into the override's lookup, which is more invasive. The broad
//! override has a measurable but bounded cost: in Sky-active compiles,
//! every cross-crate-inlinable item gets a real `.o` symbol. LLVM's normal
//! inliner still handles them at any opt-level >=1, and LTO still inlines
//! across modules. The only loss is "code size optimization via
//! available_externally on rarely-called cross_crate_inlinable items in
//! Sky-active compiles." For Sky's user-visible perf model this is
//! invisible; pure-Rust pass-through is unaffected.

use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_middle::ty::TyCtxt;

pub type CrossCrateInlinableFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> bool;
pub type ExternCrossCrateInlinableFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> bool;

/// Override for `providers.queries.cross_crate_inlinable` (local items).
/// Returns `false` when Sky machinery is active; delegates to the default
/// provider otherwise.
///
/// Patch 5 retirement (2026-06-21): switched from `tcx.consumer_lang_active(())`
/// to the inline `crate::is_sky_active(tcx)` marker-walk helper. Same
/// semantics, but doesn't require the consumer_lang_active rustc-fork
/// query patch — that patch could not be retired while this query call
/// was here.
pub fn lang_cross_crate_inlinable<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> bool {
    if crate::is_sky_active(tcx) {
        return false;
    }
    crate::default_cross_crate_inlinable()(tcx, def_id)
}

/// Override for `providers.extern_queries.cross_crate_inlinable` (upstream
/// items read from rmeta). Same semantics — Sky-active compiles get
/// `false` so rustc emits real .o symbols for cross-crate-inlinable items
/// referenced from Sky's emitted bodies (e.g. `<Vec<i64>>::new` in case 5).
pub fn lang_extern_cross_crate_inlinable<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
) -> bool {
    if crate::is_sky_active(tcx) {
        return false;
    }
    crate::default_extern_cross_crate_inlinable()(tcx, def_id)
}
