//! Phase 1 smoke test for the `extra_modules` rustc fork patch.
//!
//! The patch (fork commit `c857f968801` in `~/rust` on `per-instance-mir`)
//! adds `ExtraBackendMethods::extra_modules`, called from
//! `rustc_codegen_ssa::base::codegen_crate` once per invocation between the
//! CGU loop and `codegen_finished`. `LlvmCodegenBackend`'s override consults
//! a process-global hook installed via
//! `rustc_codegen_llvm::set_extra_modules_hook`.
//!
//! Phase 1's hook returns an empty Vec — the goal is to validate that the
//! patch wiring works end-to-end (install + read + invoke + return) without
//! crashing the suite. Set `LANG_FACADE_EXTRA_MODULES_PROBE=1` to enable an
//! eprintln that confirms the hook fires per invocation.
//!
//! Phase 2 replaces this with toylang's real `ModuleCodegen<ModuleLlvm>`
//! emission via bitcode round-trip through `ModuleLlvm::parse`.

use rustc_codegen_ssa::ModuleCodegen;
use rustc_codegen_llvm::ModuleLlvm;
use rustc_middle::ty::TyCtxt;

/// Phase 1 smoke-test hook. Returns no extra modules; eprintln'd when probe env var set.
pub fn phase1_smoke_hook<'tcx>(_tcx: TyCtxt<'tcx>) -> Vec<ModuleCodegen<ModuleLlvm>> {
    if std::env::var("LANG_FACADE_EXTRA_MODULES_PROBE").is_ok() {
        eprintln!("[lang-facade] extra_modules hook fired (phase 1 smoke; empty)");
    }
    Vec::new()
}

/// Register the smoke-test hook with rustc_codegen_llvm. Idempotent.
/// Call once during `LangDriver::config` alongside `override_queries`.
pub fn install_phase1_smoke_hook() {
    rustc_codegen_llvm::set_extra_modules_hook(phase1_smoke_hook);
}
