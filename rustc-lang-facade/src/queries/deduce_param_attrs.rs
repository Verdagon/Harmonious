//! `deduced_param_attrs` query override — closes a latent silent-UB vector
//! caused by rustc inferring `readonly` + `captures(none)` from Sky's stub
//! `unreachable!()` MIR body.
//!
//! ## The problem
//!
//! Sky's stub source emits exported functions with
//! `pub fn foo(x: T) -> R { unreachable!() }` bodies. The real body is
//! supplied by Sky's `fill_extra_modules` hook at LLVM IR time, but rustc
//! sees the `unreachable!()` source for MIR analysis. Rustc's
//! `deduced_param_attrs` query (`compiler/rustc_mir_transform/src/deduce_param_attrs.rs`)
//! analyzes the function's MIR body to infer LLVM param attrs like
//! `readonly`, `noalias`, `nocapture`, `dereferenceable`. These attrs
//! propagate to LLVM IR via `apply_deduced_attributes`
//! (`compiler/rustc_ty_utils/src/abi.rs:646-672`) and unlock alias analysis
//! + DCE + register promotion around call sites.
//!
//! For Sky exports, `unreachable!()` lowers to a single `Call` terminator
//! to `core::panicking::panic` that doesn't touch param locals at all.
//! `UsageSummary` stays `empty()` → rustc concludes "the function neither
//! mutates, captures, drops, nor shared-borrows its param." For
//! `PassMode::Indirect` params (large structs by value, `&mut LargeStruct`,
//! `*mut OpaqueSky` in some shapes), `apply_deduced_attributes` then sets
//! `ReadOnly` + `CapturesNone` on the LLVM call instruction at every Rust
//! caller.
//!
//! If Sky's actual `fill_extra_modules`-emitted body MUTATES an
//! indirect-passed param — e.g., `fn process(data: &mut LargeStruct)` that
//! writes through `data` — the `readonly` attr LLVM applies is a LIE. LLVM
//! trusts the attr; the verifier checks shape, not semantics. **Silent UB
//! at -O2+.**
//!
//! Same shape as B10 (rustc trusts stub MIR; stub "lies" about behavior);
//! worse failure mode (silent miscompile vs B10's loud compile error).
//! Currently latent — Sky has no fixture with indirect-passed mutable
//! params. A real Sky 0.1.0 release with any `fn f(x: &mut LargeStruct)`
//! export would hit silent UB.
//!
//! ## The fix
//!
//! For items where `is_consumer_codegen_target(tcx, def_id)` returns true
//! (i.e. `#[toylang::emit_consumer_body]`-tagged items in marker-bearing
//! crates), return `&[]` (no attrs claimed). The empty slice is the
//! conservative safe default: `apply_deduced_attributes` sees no entries
//! and applies no `readonly`/`captures(none)` attrs to the call site. LLVM
//! optimizes more cautiously around Sky calls, but soundness is preserved
//! at all opt levels.
//!
//! Non-tagged items in marker-bearing crates (the Phase 6 `#[inline(never)]`
//! wrappers, `__SKY_STUBS_MARKER`, `__ToylangOpaque<T>`, pub-use re-exports,
//! extern "C" blocks, Sky struct declarations) fall through to the default
//! provider — they ARE real Rust source whose deduced attrs are correct.
//! Pure-Rust crates compiled through Sky's rustc binary see byte-identical
//! behavior to vanilla rustc because `is_consumer_codegen_target` returns
//! false for every DefId in them.
//!
//! ## Perf recovery (deferred)
//!
//! The override gives up downstream alias-analysis optimization for Sky's
//! indirect-passed params. Phase Q-adjacent path-b emission (emit
//! Sky-ground-truth attrs directly in `codegen_extern_wrapper`) recovers
//! the perf opportunity using Sky's typechecker as the source of truth.
//! Deferred until a `&LargeStruct`-shaped perf bench shows the gap is
//! material. The Bench 1 `add(i32, i32) -> i32` shape is all
//! `PassMode::Direct` and doesn't expose the indirect-only application
//! path; no current bench measures the loss.
//!
//! ## Why the override is narrow (predicate-gated) rather than broad
//!
//! `cross_crate_inlinable.rs` uses a broad `is_sky_active(tcx)` gate
//! because the question "force real .o symbols for inlinable items" is a
//! crate-wide invariant Sky needs everywhere it's active. `deduced_param_attrs`
//! is per-item: Sky's stubs need conservative defaults, but Sky-active
//! compiles can also include normal Rust source (toylangc's user_bin
//! shim, `__lang_stubs`'s wrapper fns + accessor methods + extern decls)
//! whose real MIR bodies CAN be analyzed soundly. Predicate-gating to
//! `#[toylang::emit_consumer_body]`-tagged items leaves all that
//! analyzable code on the default optimization path.
//!
//! ## Why this preserves pass-through
//!
//! `is_consumer_codegen_target` returns false for any DefId not in a
//! marker-bearing crate (the cross-crate-safe `is_from_lang_stubs` check)
//! and additionally for any item without the `#[toylang::emit_consumer_body]`
//! attribute. Pure-Rust crates compiled through Sky's rustc binary see
//! the default provider unconditionally → byte-identical to vanilla
//! rustc for every Rust caller's call site.
//!
//! ## Verification (2026-06-25)
//!
//! IR inspection of the existing `bench4_largestruct_byval_thin` fixture
//! confirms the override fires. The fixture has Sky exports
//! `make_large(i64) -> LargeStruct` and `first_field(LargeStruct) -> i64`
//! where `LargeStruct` is 32 bytes (`PassMode::Indirect`). Without the
//! override, `apply_deduced_attributes` would set `ReadOnly` + `CapturesNone`
//! on the LLVM IR at every Rust caller's call site (the stub
//! `unreachable!()` body produces an empty `UsageSummary` → rustc concludes
//! the function neither reads nor captures its params).
//!
//! Post-fix IR (from `tmp/bench4_main.ll`):
//!   declare void @make_large(ptr dead_on_unwind noalias noundef writable
//!                            sret([32 x i8]) align 8 captures(address)
//!                            dereferenceable(32), i64 noundef)
//!   declare i64 @first_field(ptr dead_on_return noalias noundef align 8
//!                            captures(address) dereferenceable(32))
//!
//! The params have `captures(address)` (the default for `&T`/sret), NOT
//! `captures(none)` (which `deduce_param_attrs` would have added). Likewise
//! NO `readonly` attr present. The override is suppressing the deduced
//! attrs as intended. The remaining attrs (`noalias`, `noundef`, `align`,
//! `dereferenceable`, `dead_on_*`, `writable`, `sret`) come from rustc's
//! standard ABI emission for references and indirect returns — not from
//! `deduced_param_attrs` — so they correctly stay.
//!
//! cache-audit: deduced_param_attrs's upstream declaration in
//! `rustc_middle/src/query/mod.rs:2709-2712` has NO `cache_on_disk_if`
//! modifier; rustc's macro emits the default policy of `false`, so
//! results are NEVER cached to disk between compile sessions. Has
//! `separate_provide_extern` so the extern provider auto-decodes from
//! rmeta. Sky's local-side override returns the conservative `&[]` value
//! which gets encoded into the stub_rlib's rmeta; downstream user_bin
//! compiles read it back via the extern provider with no further override
//! needed. The override's return value depends on
//! `is_consumer_codegen_target(tcx, def_id)` (a marker walk + attribute
//! check), which reflects the current compile's universe state — no
//! staleness risk. See `toylangc/tests/cache_audit.rs` for the full audit
//! table.

use rustc_hir::def_id::LocalDefId;
use rustc_middle::middle::deduced_param_attrs::DeducedParamAttrs;
use rustc_middle::ty::TyCtxt;

pub type DeducedParamAttrsFn =
    for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx [DeducedParamAttrs];

/// Override for `providers.queries.deduced_param_attrs` (local items).
/// Returns `&[]` for `#[toylang::emit_consumer_body]`-tagged items in
/// marker-bearing crates; delegates to the default provider otherwise.
pub fn lang_deduced_param_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx [DeducedParamAttrs] {
    if crate::is_consumer_codegen_target(tcx, def_id.to_def_id()) {
        // Conservative safe default — claim no attrs. LLVM applies no
        // readonly/captures(none) at Sky call sites, which is sound for
        // Sky bodies that may mutate indirect-passed params.
        return &[];
    }
    crate::default_deduced_param_attrs()(tcx, def_id)
}
