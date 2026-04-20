//! rustc driver lifecycle management.
//!
//! This module owns the full compilation lifecycle: it takes the consumer's
//! `LangCallbacks` implementation, sets up the rustc session with all the
//! necessary hooks (query overrides + CodegenBackend wrapper), and runs the
//! compilation.
//!
//! The consumer never interacts with rustc directly — they implement the
//! `LangCallbacks` trait and call `run_compiler`. Everything else is handled
//! here and in the query/codegen modules.
//!
//! Stage 5c.4 retired `FileLoader` stub injection. Under the current two-
//! crate architecture the stub rlib is a real on-disk crate compiled by
//! cargo; there's nothing to intercept. The `generate_stubs` callback is
//! retired along with it — wrapper mode's `build::write_stub_crate`
//! generates the stub rlib's `src/lib.rs` directly via `stub_gen::generate`.

use rustc_driver::Compilation;
use rustc_interface::Config;
use rustc_middle::ty::TyCtxt;

use crate::LangCallbacks;

/// Entry point for consumers. Takes a concrete `LangCallbacks` implementation
/// and rustc command-line arguments. Handles the full driver lifecycle:
/// query overrides + codegen backend wrapping.
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
    crate::install_callbacks(callbacks);
    // Always install the codegen backend wrapper. If generate_and_compile
    // returns None at runtime, no .o gets injected — no harm done.
    let mut driver = LangDriver;
    rustc_driver::run_compiler(rustc_args, &mut driver);
}

struct LangDriver;

impl rustc_driver::Callbacks for LangDriver {
    fn config(&mut self, config: &mut Config) {
        // Clear any CGU stash left from a prior compile session (e.g., an
        // earlier integration test that ran through the same process). The
        // stash holds a raw pointer into a TyCtxt's arena; that arena no
        // longer exists after the session ends, so we must invalidate the
        // stash before another session starts populating it. Prior versions
        // cleared inside `LangCodegenBackend::codegen_crate`, but under
        // stage 5b two-crate the partitioner fires before that call (during
        // rlib metadata setup) — clearing there wiped valid stash data. See
        // `codegen_wrapper.rs` for the full reasoning.
        crate::clear_upstream_cgus();

        config.override_queries = Some(crate::queries::lang_override_queries);
        config.make_codegen_backend = Some(Box::new(|_opts, _target| {
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
        // This ensures the consumer has seen all collect_generic_rust_deps /
        // notify_concrete_entry_point callbacks and can compile with full
        // knowledge of what deps exist.

        Compilation::Continue
    }
}
