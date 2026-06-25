//! `codegen_fn_attrs` query override — stamps `NEVER_UNWIND` on Sky's
//! tagged exports for free landing-pad elimination at every Rust caller.
//!
//! ## The opportunity
//!
//! Sky enforces `panic = "abort"` globally per arch §16.1. Every Sky
//! export is therefore genuinely never-unwind: there is no panic path
//! that propagates out across the Sky/Rust boundary. Rustc's default
//! `codegen_fn_attrs` for Sky stubs leaves the `NEVER_UNWIND` flag off
//! (the stub source doesn't carry a `#[rustc_nounwind]` or equivalent
//! attribute), so every Rust caller's call site to a Sky export gets
//! emitted with full unwind machinery — landing pads, cleanup blocks,
//! personality functions. Significant cost for tight callee-rich loops
//! under panic=unwind.
//!
//! ## The fix
//!
//! For items where `is_consumer_codegen_target(tcx, def_id)` returns
//! true (i.e. `#[toylang::emit_consumer_body]`-tagged items in
//! marker-bearing crates), this override clones the default attrs and
//! sets the `NEVER_UNWIND` flag. LLVM picks up the `nounwind` attribute
//! on the function declaration and at every call site — landing pads
//! around Sky calls eliminate.
//!
//! Non-tagged items in marker-bearing crates (Phase 6 `#[inline(never)]`
//! wrappers, `__SKY_STUBS_MARKER`, `__ToylangOpaque<T>`, pub-use
//! re-exports, extern "C" blocks, Sky struct declarations) fall through
//! to the default provider — they may unwind (they're real Rust source
//! with no Sky-enforced posture).
//!
//! ## Correctness
//!
//! `NEVER_UNWIND` is sound ONLY when the actual emitted body genuinely
//! cannot unwind. Sky's panic=abort posture (§16.1) is what makes this
//! true. If Sky's panic posture ever changes (it shouldn't per §16.1,
//! but hypothetically), this override needs to gate on the per-compile
//! panic strategy: only return `NEVER_UNWIND` when `tcx.sess.panic_strategy()`
//! is `PanicStrategy::Abort`. Currently always Abort-or-equivalent for
//! Sky-active compiles; the override is unconditional.
//!
//! Lying with `NEVER_UNWIND` on a body that CAN unwind is UB: LLVM
//! optimizes away cleanup paths that callers would have needed to drop
//! state. Same UB class as @ACRTFDZ's "wrong ABI types" or
//! `deduce_param_attrs`'s "wrong readonly claim" — a contract LLVM
//! trusts but Sky's body violates.
//!
//! ## Extensions (deferred)
//!
//! `FFI_PURE` / `FFI_CONST` — would unlock LLVM pure-function
//! optimizations for Sky exports the typechecker proves are pure. Not
//! exposed by Sky's surface today.
//!
//! `Cold` — annotation for unlikely-path Sky exports. Requires Sky
//! source annotation propagating through stub_gen.
//!
//! Explicit `target_features` — niche; helps when LLVM's call-site
//! feature-compat check would otherwise refuse `InlineHint`. Not
//! currently surfacing in Sky benches.
//!
//! ## Why this preserves pass-through
//!
//! `is_consumer_codegen_target` returns false for any DefId not in a
//! marker-bearing crate, and additionally for any item without the
//! `#[toylang::emit_consumer_body]` attribute. Pure-Rust crates
//! compiled through Sky's rustc binary see the default provider
//! unconditionally → byte-identical to vanilla rustc for every Rust
//! caller's call site.
//!
//! cache-audit: codegen_fn_attrs's upstream declaration in
//! `rustc_middle/src/query/mod.rs` has `cache_on_disk_if { def_id.is_local() }`
//! — disk-cached for local DefIds. Sky's override returns deterministic
//! values per (def_id, is_consumer_codegen_target). The predicate's
//! value is determined by source code (attribute presence + crate
//! marker), which is stable per source — same source produces same
//! cached value across compiles. Safe under disk caching. If Sky's
//! universe-state-driven decisions ever feed back into the override
//! (e.g., per-instance attrs), this analysis must be revisited. See
//! `toylangc/tests/cache_audit.rs` for the full audit table.

use rustc_hir::def_id::LocalDefId;
use rustc_middle::middle::codegen_fn_attrs::{CodegenFnAttrFlags, CodegenFnAttrs};
use rustc_middle::ty::TyCtxt;

pub type CodegenFnAttrsFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> CodegenFnAttrs;

/// Override for `providers.queries.codegen_fn_attrs`. Stamps
/// `NEVER_UNWIND` on `#[toylang::emit_consumer_body]`-tagged Sky exports;
/// delegates to the default provider otherwise.
pub fn lang_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CodegenFnAttrs {
    let mut attrs = crate::default_codegen_fn_attrs()(tcx, def_id);
    if crate::is_consumer_codegen_target(tcx, def_id.to_def_id()) {
        attrs.flags |= CodegenFnAttrFlags::NEVER_UNWIND;
    }
    attrs
}
