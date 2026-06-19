//! patch 4 rev 2 (Approach B): hook that fills consumer-emitted modules into
//! rustc's optimization + ThinLTO + emission pipeline.
//!
//! The rustc fork patch adds `ExtraBackendMethods::fill_extra_modules` called
//! from `rustc_codegen_ssa::base::codegen_crate` synchronously on the main
//! thread before `start_async_codegen`. `LlvmCodegenBackend`'s override
//! consults a process-global hook installed via
//! `rustc_codegen_llvm::set_fill_extra_modules_hook`. The backend allocates
//! each per-CGU `ModuleLlvm` (LLVMContext + LLVMModule + TargetMachine) on
//! the consumer's behalf via the `ExtraModuleAllocator` callback; the
//! consumer fills each module in place via its own IR API (e.g. Inkwell's
//! `ContextRef::new` + `Module::new_borrowed`), and rustc retains ownership
//! throughout.
//!
//! No bitcode serialization, no LLVM-context migration, no `parse_from_tcx`
//! round-trip — Approach B closes risks B9 / B10 / B11.
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

/// patch 4 rev 2 hook. Forwards directly into the consumer's
/// `consumer_fill_modules` callback (via the facade trampoline). The
/// `allocator` is provided by rustc — calling `allocator.allocate(name)`
/// returns a fresh rustc-owned `&mut ModuleLlvm` the consumer fills in place.
pub fn consumer_fill_modules_hook<'tcx>(
    tcx: TyCtxt<'tcx>,
    allocator: &mut dyn ExtraModuleAllocator<ModuleLlvm>,
) {
    let probe = std::env::var("LANG_FACADE_EXTRA_MODULES_PROBE").is_ok();
    if probe {
        eprintln!("[lang-facade] fill_extra_modules hook fired");
    }
    crate::call_consumer_fill_modules(tcx, allocator);
}

/// Register the hook with rustc_codegen_llvm. Idempotent.
/// Call once during `LangDriver::config` alongside `override_queries`.
pub fn install_consumer_modules_hook() {
    rustc_codegen_llvm::set_fill_extra_modules_hook(consumer_fill_modules_hook);
}
