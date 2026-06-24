//! patch 4 rev 3 (Approach B): hook that fills consumer-emitted modules into
//! rustc's optimization + ThinLTO + emission pipeline.
//!
//! The rustc fork patch adds `ExtraBackendMethods::fill_extra_modules` called
//! from `rustc_codegen_ssa::base::codegen_crate` synchronously on the main
//! thread before `start_async_codegen`. `LlvmCodegenBackend`'s override
//! consults a process-global hook installed via
//! `rustc_codegen_llvm::set_fill_extra_modules_hook`. The backend allocates
//! each per-CGU `ModuleLlvm` (LLVMContext + LLVMModule + TargetMachine) on
//! the consumer's behalf via the `ExtraModuleAllocator` callback.
//!
//! This hook is the boundary where rustc's `ExtraModuleAllocator<ModuleLlvm>`
//! becomes the facade-owned [`crate::LlvmModuleFactory`] surface the consumer
//! sees. From the consumer's perspective, `ModuleLlvm` and
//! `ExtraModuleAllocator` do not exist — only [`crate::BorrowedLlvmModule`]
//! raw pointers (`*mut c_void`) the consumer wraps via whichever LLVM API it
//! prefers (Inkwell, `llvm-sys`, C++ via FFI).
//!
//! No bitcode serialization, no LLVM-context migration, no `parse_from_tcx`
//! round-trip — Approach B closes risks B9 / B10 / B11.
//!
//! **Rev-3 ABI change (2026-06-24, Phase H / handoff Decision 5).** Rustc's
//! allocator surface became a `#[repr(C)]` function-pointer struct
//! (`ExtraModuleAllocator<M> { state, allocate: unsafe extern "C" fn }`)
//! instead of `&mut dyn ExtraModuleAllocator<M>`. The struct is two
//! stable-ABI fields (one opaque pointer + one extern-C fn pointer), so the
//! hook signature now takes `&ExtraModuleAllocator<ModuleLlvm>` (immutable
//! reference — the struct doesn't need to be mutated; the state pointer it
//! carries is what holds the mutable allocator state, opaque to us). The
//! facade is still static-linked into toylangc today; rev 3 keeps the
//! shape FFI-safe so the cdylib refactor (Sky-proper architecture / handoff
//! Phase G) doesn't have to revisit the patch.
//!
//! TODO(revisit): the hook is stored as a process-global OnceLock in
//! `rustc_codegen_llvm::FILL_EXTRA_MODULES_HOOK`, not in per-Session storage.
//! This is forced by crate-graph constraints — `Session` can't typecheck a
//! `TyCtxt`-bearing fn pointer because `rustc_session` is upstream of
//! `rustc_middle`. Process-global is fine because each rustc invocation is
//! a separate process; revisit if we ship Sky and want per-Session
//! isolation for parallel test runs in the same process, or if we add
//! another codegen backend (cranelift) that would need its own parallel
//! hook.

use rustc_codegen_llvm::ModuleLlvm;
use rustc_codegen_ssa::traits::ExtraModuleAllocator;
use rustc_middle::ty::TyCtxt;

use crate::LlvmModuleFactory;

/// patch 4 rev 3 hook. Wraps rustc's `ExtraModuleAllocator<ModuleLlvm>` in
/// an [`LlvmModuleFactory`] (the LLVM-API-agnostic facade surface) and
/// forwards into the consumer's `consumer_fill_modules` callback via the
/// facade trampoline.
pub fn consumer_fill_modules_hook<'tcx>(
    tcx: TyCtxt<'tcx>,
    allocator: &ExtraModuleAllocator<ModuleLlvm>,
) {
    let probe = std::env::var("LANG_FACADE_EXTRA_MODULES_PROBE").is_ok();
    if probe {
        eprintln!("[lang-facade] fill_extra_modules hook fired");
    }
    let mut factory = LlvmModuleFactory::new(allocator);
    crate::call_consumer_fill_modules(tcx, &mut factory);
}

/// Register the hook with rustc_codegen_llvm. Idempotent.
/// Call once during `LangDriver::config` alongside `override_queries`.
pub fn install_consumer_modules_hook() {
    rustc_codegen_llvm::set_fill_extra_modules_hook(consumer_fill_modules_hook);
}
