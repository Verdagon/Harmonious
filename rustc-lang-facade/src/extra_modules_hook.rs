//! Phase 2 (inline-codegen plan): hook that submits consumer-emitted bitcode
//! modules into rustc's optimization + ThinLTO + emission pipeline.
//!
//! The rustc fork patch (`per-instance-mir` branch tip `0e5a98b900f`) adds
//! `ExtraBackendMethods::extra_modules` called from
//! `rustc_codegen_ssa::base::codegen_crate` once per invocation between the
//! CGU loop and `codegen_finished`. `LlvmCodegenBackend`'s override consults
//! a process-global hook installed via
//! `rustc_codegen_llvm::set_extra_modules_hook`. The patch also adds
//! `ModuleLlvm::parse_from_tcx(tcx, name, buffer)` so the hook can parse
//! bitcode without a worker-thread CodegenContext.
//!
//! TODO(revisit): the hook is stored as a process-global OnceLock in
//! `rustc_codegen_llvm::EXTRA_MODULES_HOOK`, not in per-Session storage.
//! This was forced by crate-graph constraints — Session can't typecheck a
//! `TyCtxt`-bearing fn pointer because rustc_session is upstream of
//! rustc_middle. The closest "Sky-like" pattern would be a Session-attached
//! bitcode-bytes-returning hook (no TyCtxt in return type) that the codegen
//! backend parses to ModuleLlvm. Worth revisiting if (a) we ship Sky and
//! want per-Session isolation for parallel test runs in the same process,
//! (b) we add another codegen backend (cranelift) that would need its own
//! parallel hook. For toylang today, process-global is fine because each
//! rustc invocation is a separate process.

use std::ffi::CString;

use rustc_codegen_llvm::ModuleLlvm;
use rustc_codegen_ssa::ModuleCodegen;
use rustc_middle::ty::TyCtxt;

/// Phase 2 hook. Calls into the consumer's `consumer_emit_modules` callback
/// (via the facade trampoline) and parses each returned bitcode buffer into
/// a `ModuleCodegen<ModuleLlvm>` via `ModuleLlvm::parse_from_tcx`.
pub fn consumer_modules_hook<'tcx>(tcx: TyCtxt<'tcx>) -> Vec<ModuleCodegen<ModuleLlvm>> {
    let probe = std::env::var("LANG_FACADE_EXTRA_MODULES_PROBE").is_ok();

    let bitcode_modules = crate::call_consumer_emit_modules(tcx);
    if probe {
        eprintln!(
            "[lang-facade] extra_modules hook fired; consumer returned {} module(s)",
            bitcode_modules.len()
        );
    }

    bitcode_modules
        .into_iter()
        .map(|(name, bitcode)| {
            let cname = CString::new(name.clone())
                .expect("module name contains NUL");
            let module_llvm = ModuleLlvm::parse_from_tcx(tcx, &cname, &bitcode);
            ModuleCodegen::new_regular(name, module_llvm)
        })
        .collect()
}

/// Register the hook with rustc_codegen_llvm. Idempotent.
/// Call once during `LangDriver::config` alongside `override_queries`.
pub fn install_consumer_modules_hook() {
    rustc_codegen_llvm::set_extra_modules_hook(consumer_modules_hook);
}
