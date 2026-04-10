//! rustc driver lifecycle management.
//!
//! This module owns the full compilation lifecycle: it takes the consumer's
//! `LangCallbacks` implementation, sets up the rustc session with all the
//! necessary hooks (FileLoader, query overrides, CodegenBackend wrapper),
//! and runs the compilation.
//!
//! The consumer never interacts with rustc directly — they implement the
//! `LangCallbacks` trait and call `run_compiler`. Everything else is handled
//! here and in the query/codegen modules.

#![allow(unused)]

use rustc_driver::Compilation;
use rustc_interface::Config;
use rustc_middle::ty::TyCtxt;

use crate::LangCallbacks;

/// Entry point for consumers. Takes a concrete `LangCallbacks` implementation
/// and rustc command-line arguments. Handles the full driver lifecycle:
/// stub injection, query overrides, codegen backend wrapping.
///
/// This function is generic over `C: LangCallbacks` — it monomorphizes the
/// vtable trampolines for the concrete consumer type at compile time. Internally
/// the callbacks are stored in a global `Box<dyn Any>` with a manual vtable of
/// HRTB function pointers. This is necessary because:
/// 1. Rustc's query overrides are plain function pointers (not closures)
/// 2. Function pointers can't capture state
/// 3. So the callbacks must be in a global
/// 4. But `dyn LangCallbacks` isn't allowed (generic `'tcx` methods break object safety)
/// 5. So we type-erase to `dyn Any` and use trampolines to downcast + dispatch
///
/// See `docs/historical/library-split-implementation.md` for the full design discussion
/// including why `dyn`, `Arc<Mutex>`, and other approaches were rejected.
///
/// The `'static` bound is required because the callbacks are stored in a global
/// `Box<dyn Any>`. In practice this is always satisfied — consumer callbacks hold
/// owned data like `Arc<Registry>` and `PathBuf`.
pub fn run_compiler<C: LangCallbacks + 'static>(
    callbacks: C,
    rustc_args: &[String],
) {
    let stubs = callbacks.generate_stubs();
    crate::install_callbacks(callbacks);
    // Always install the codegen backend wrapper. If generate_and_compile
    // returns None at runtime, no .o gets injected — no harm done.
    let mut driver = LangDriver::new(stubs);
    rustc_driver::RunCompiler::new(rustc_args, &mut driver).run();
}

struct LangDriver {
    stubs: String,
}

impl LangDriver {
    fn new(stubs: String) -> Self {
        Self { stubs }
    }
}

impl rustc_driver::Callbacks for LangDriver {
    fn config(&mut self, config: &mut Config) {
        config.file_loader = Some(Box::new(
            crate::file_loader::LangFileLoader::new(self.stubs.clone())
        ));
        config.override_queries = Some(crate::queries::lang_override_queries);
        config.make_codegen_backend = Some(Box::new(|_opts| {
            crate::codegen_wrapper::LangCodegenBackend::new()
        }));
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        // Phase 1.5: let consumer type-check against Rust types.
        // This runs BEFORE monomorphization — the consumer can query tcx for
        // Rust type info but doesn't know which concrete instantiations will
        // be requested yet.
        crate::call_after_rust_analysis(tcx);

        // Phase 3 (generate_and_compile) is called later, from
        // LangCodegenBackend::codegen_crate, AFTER monomorphization completes.
        // This ensures the consumer has seen all monomorphize_fn callbacks
        // and can compile with full knowledge of what deps exist.

        Compilation::Continue
    }
}
