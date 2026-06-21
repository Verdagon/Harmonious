//! `codegen_fn_attrs` query override — retires the A.3 partition filter
//! by marking consumer-defined items with `AvailableExternally` linkage.
//!
//! ## The problem
//!
//! Consumer `stub_gen` emits `pub fn add_one(x: i32) -> i32 { unreachable!() }`
//! for every consumer export. Rustc's default codegen would compile that body
//! to machine code (a `panic("unreachable")` blob in `.rcgu.o`). The
//! consumer's `fill_extra_modules` hook (rustc fork patch 4) emits the REAL
//! body for the same symbol with `External` linkage. Two `.o` files, same
//! mangled symbol — link collision.
//!
//! Historical fix: the A.3 partition filter (formerly in `queries/partition.rs`)
//! removed consumer-defined items from rustc's CGU list before codegen, so
//! rustc emitted no machine code for them. ~107 lines of "Sky censors rustc's
//! pipeline."
//!
//! ## The fix (this file)
//!
//! Override `codegen_fn_attrs` to set
//! `linkage = Some(Linkage::AvailableExternally)` on every consumer-defined
//! item. LLVM emits the body for inlining purposes but produces no `.o`
//! symbol. The consumer's `fill_extra_modules` body becomes the sole `.o`
//! definition; the linker resolves cleanly. No partition filter needed.
//!
//! The partitioner short-circuits at
//! `rustc_monomorphize::partitioning.rs::mono_item_linkage_and_visibility`
//! when `mono_item.explicit_linkage(tcx)` returns Some, so this override is
//! sufficient — no need to also mutate `MonoItemData.linkage` post-partition.
//! `explicit_linkage` consults `codegen_instance_attrs(...).linkage`, which
//! reads through to our overridden `codegen_fn_attrs`.
//!
//! ## Why this preserves pass-through
//!
//! Gated on `is_consumer_codegen_target` which only fires for items in
//! marker-bearing crates. Pure-Rust crates compiled via the forked rustc
//! binary delegate to the default provider — byte-identical to vanilla.
//!
//! ## Why this preserves cross-language LTO inlining (F1's promise)
//!
//! `available_externally` linkage means the body IS in the IR, just not in
//! the `.o`. LTO's IR linker can still inline the body across crate
//! boundaries — that's the whole point of the linkage kind. The actual body
//! the LTO inliner sees is the one the consumer's `fill_extra_modules` hook
//! contributed (single-symbol architecture per arch §F.2). The stub's
//! `unreachable!()` body emitted by rustc carries `AvailableExternally`
//! linkage (set by this override), so LLVM's IR linker unambiguously
//! prefers the `External`-linkage real body from `fill_extra_modules`.
//! The earlier `#![no_builtins]` LTO-pool-exclusion belt-and-suspenders
//! mechanism was retired 2026-06-21 (§5.5 Round 2 E2 verified the matrix
//! passes cleanly without it).
//!
//! ## Interaction with cross_crate_inlinable (F2 override)
//!
//! `cross_crate_inlinable.rs` forces that query to false for Sky-active
//! compiles, so rustc doesn't pick `available_externally` linkage on its own
//! for `#[inline]`-style items. This override sets `available_externally`
//! EXPLICITLY for consumer-defined items only. The two overrides target
//! opposite directions:
//!
//! - F2 / B16: rustc's call sites can't resolve to consumer items if those
//!   items are `available_externally` without a real `.o` symbol → force them
//!   to emit real symbols.
//! - Option 4: consumer's `fill_extra_modules` emits real `.o` symbols for
//!   consumer items; rustc shouldn't ALSO emit machine code for the stub's
//!   `unreachable!()` body → force `available_externally` so rustc emits IR
//!   only.
//!
//! Both gate on `consumer_lang_active(())` and `is_consumer_codegen_target`
//! respectively, so they don't conflict.

use rustc_hir::attrs::Linkage;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrs;
use rustc_middle::ty::TyCtxt;

pub type CodegenFnAttrsFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> CodegenFnAttrs;
pub type ExternCodegenFnAttrsFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> CodegenFnAttrs;

/// Override for `providers.queries.codegen_fn_attrs` (local items).
/// Calls the default provider, then forces
/// `linkage = Some(AvailableExternally)` on consumer-defined items so
/// rustc's LLVM backend emits IR (for cross-module inlining) but no `.o`
/// symbol. The consumer's separately-emitted body becomes the sole def.
pub fn lang_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CodegenFnAttrs {
    let mut attrs = crate::default_codegen_fn_attrs()(tcx, def_id);
    if crate::is_consumer_codegen_target(tcx, def_id.to_def_id()) {
        attrs.linkage = Some(Linkage::AvailableExternally);
    }
    attrs
}

/// Override for `providers.extern_queries.codegen_fn_attrs` (upstream items
/// read from rmeta). Same semantics — consumer items reached via the
/// upstream's cstore also need `AvailableExternally` so the downstream
/// (user_bin) compile doesn't emit competing `.o` symbols for them.
pub fn lang_extern_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
) -> CodegenFnAttrs {
    let mut attrs = crate::default_extern_codegen_fn_attrs()(tcx, def_id);
    if crate::is_consumer_codegen_target(tcx, def_id) {
        attrs.linkage = Some(Linkage::AvailableExternally);
    }
    attrs
}
