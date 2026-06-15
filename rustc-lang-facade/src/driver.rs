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

    fn after_expansion<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        // Course-correct #5 / architecture §20.3: Sky's frontend runs at
        // `after_expansion`, BEFORE rustc's typecheck walks the stub bodies.
        // Sky's universe is populated here so any rustc query that consults
        // Sky's predicates / overrides during analysis sees a ready universe.
        //
        // The stub rlibs' Rust source is parsed + expanded by this point; all
        // extern crates (including upstream Sky-marked rlibs) have been loaded
        // by rustc's metadata machinery, so `tcx.crates(())` enumerates every
        // external crate this compile depends on (transitive deps included).
        // ADT defs, fn sigs, and `module_children` walks are available at
        // this point, which is what the oracle / `load_upstream_sidecars`
        // path needs.
        //
        // S.4: load each upstream Sky-marked rlib's adjacent `.sky-meta`
        // sidecar and hand the bytes to the consumer. Runs BEFORE
        // `call_after_rust_analysis` so the consumer's after-analysis pass
        // has access to upstream universes.
        //
        // Detection uses marker-based `crate::is_from_lang_stubs` (Phase 3
        // E.1; `__SKY_STUBS_MARKER` at the crate root per §4.5 / §6.3).
        load_upstream_sidecars(tcx);

        // Sky's typecheck-and-codegen-queue pass (toylang: Check 1–5 in
        // `after_rust_analysis`). The consumer can query tcx for Rust type
        // info but doesn't know which concrete instantiations will be
        // requested yet — those surface via `collect_generic_rust_deps`
        // during monomorphization.
        crate::call_after_rust_analysis(tcx);

        // Phase 3 (generate_and_compile) is called later, from
        // LangCodegenBackend::codegen_crate, AFTER monomorphization completes.
        // This ensures the consumer has seen all collect_generic_rust_deps /
        // notify_concrete_entry_point callbacks and can compile with full
        // knowledge of what deps exist.

        Compilation::Continue
    }
}

/// Walk every upstream crate; for each one that's a Sky-marked rlib,
/// locate the adjacent `.sky-meta` sidecar, read its bytes, and hand them
/// to the consumer via `on_sky_lib_loaded`. See `after_analysis` for the
/// design rationale; this function is the mechanical implementation.
///
/// Missing-sidecar policy: per architecture doc §7.6 ("Missing sidecar is
/// a hard error"), if a marker-bearing rlib has no adjacent sidecar we
/// panic with a clear message. The marker means Sky machinery was
/// supposed to be active during the rlib's compile; an absent sidecar
/// indicates corrupted or out-of-sync target state and proceeding would
/// produce runtime panics from `unreachable!()` stub bodies.
fn load_upstream_sidecars(tcx: TyCtxt<'_>) {
    use rustc_hir::def_id::CRATE_DEF_INDEX;
    use rustc_span::def_id::DefId;

    for cnum in tcx.crates(()).iter().copied() {
        let crate_root = DefId { krate: cnum, index: CRATE_DEF_INDEX };
        if !crate::is_from_lang_stubs(tcx, crate_root) {
            continue;
        }
        let crate_name = tcx.crate_name(cnum).to_string();
        let source = tcx.used_crate_source(cnum);
        // CrateSource carries an `Option<PathBuf>` per artifact kind.
        // Prefer rlib; fall back to rmeta only if rlib is absent (won't
        // happen in practice — Sky libs always ship rlibs — but be
        // defensive).
        let Some(rlib_path) = source.rlib.as_ref().or(source.rmeta.as_ref()) else {
            panic!(
                "[facade] Sky-marked crate `{}` has no rlib/rmeta path; \
                 cannot locate adjacent .sky-meta sidecar",
                crate_name,
            );
        };
        // Map the rlib path to its adjacent `.sky-meta` sidecar. Cargo's
        // rlib filename carries a `lib` prefix (`liblang_stubs-HASH.rlib`),
        // but S.3 writes the sidecar via `tcx.output_filenames(())
        // .with_extension("sky-meta")` whose filestem is the bare crate
        // name (no `lib` prefix). So we strip the `lib` prefix from the
        // rlib's filename before swapping extension. (`.rmeta` paths
        // from a metadata-only build follow the same convention.)
        let sidecar_path = {
            let dir = rlib_path.parent().unwrap_or(std::path::Path::new("."));
            let stem = rlib_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let stripped = stem.strip_prefix("lib").unwrap_or(stem);
            dir.join(format!("{}.sky-meta", stripped))
        };
        let sidecar_bytes = std::fs::read(&sidecar_path).unwrap_or_else(|e| {
            panic!(
                "[facade] Sky sidecar missing for crate `{}`\n  \
                 expected at: {}\n  \
                 crate marker present: yes\n  \
                 hint: rebuild `{}` with the Sky toolchain (architecture doc §7.6)\n  \
                 underlying error: {}",
                crate_name,
                sidecar_path.display(),
                crate_name,
                e,
            )
        });
        crate::call_on_sky_lib_loaded(tcx, &crate_name, &sidecar_bytes);
    }
}
