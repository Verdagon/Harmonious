//! CodegenBackend wrapper that injects an external .o into rustc's link step.
//!
//! The consumer compiles its function bodies to a .o file (e.g. via LLVM IR).
//! This module wraps rustc's `LlvmCodegenBackend` and injects that .o as an
//! additional `CompiledModule` during `join_codegen`. Rustc's linker then links
//! it alongside the Rust-compiled modules, producing a single binary.
//!
//! Why a wrapper and not a hook? Rustc's `CodegenBackend` trait doesn't have
//! an "inject extra objects" method. The only way to add a module to
//! `CodegenResults` is to intercept `join_codegen`, which returns the results.
//! We wrap the real backend, delegate everything, and modify the results.
//!
//! The .o path travels from `codegen_crate` to `join_codegen` inside a
//! `LangOngoingCodegen` wrapper around rustc's own ongoing-codegen value. Per
//! architecture §5.3 (course-correct #4): no `OnceLock` / `Mutex` round-trip;
//! the path rides the same `Box<dyn Any>` rustc already threads between the two
//! phases. `join_codegen` downcasts, extracts both fields, delegates the inner
//! ongoing to `LlvmCodegenBackend::join_codegen`.
//!
//! The `-C codegen-units=16` flag is required alongside this wrapper. It forces
//! rustc's partitioner to give Rust generic instantiations external linkage
//! (needed for cross-CGU visibility), so the consumer's .o can call them by
//! mangled symbol name at link time.

use rustc_codegen_ssa::{CodegenResults, CompiledModule, ModuleKind};
use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_middle::util::Providers;
use rustc_session::config::OutputFilenames;
use rustc_session::Session;
use std::any::Any;
use std::path::PathBuf;

/// Carries rustc's ongoing-codegen Box plus the consumer's compiled `.o` path
/// from `codegen_crate` to `join_codegen` without a global channel.
struct LangOngoingCodegen {
    inner: Box<dyn Any>,
    lang_obj_path: Option<PathBuf>,
}

/// Thin wrapper around `LlvmCodegenBackend` that injects the consumer's .o file
/// into `CodegenResults` during `join_codegen`.
///
/// All methods delegate to the inner backend except `join_codegen`, which appends
/// the consumer's .o as a `CompiledModule` with `ModuleKind::Regular`.
///
/// Always installed by `run_compiler` — if the consumer's `generate_and_compile`
/// returns `None`, no .o path is set and `join_codegen` is a pure passthrough.
pub struct LangCodegenBackend {
    inner: Box<dyn CodegenBackend>,
}

impl LangCodegenBackend {
    pub fn new() -> Box<dyn CodegenBackend> {
        let inner = rustc_codegen_llvm::LlvmCodegenBackend::new();
        Box::new(Self { inner })
    }
}

impl CodegenBackend for LangCodegenBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn locale_resource(&self) -> &'static str {
        self.inner.locale_resource()
    }

    fn init(&self, sess: &Session) {
        self.inner.init(sess);
    }

    fn provide(&self, providers: &mut Providers) {
        self.inner.provide(providers);
    }

    fn codegen_crate<'tcx>(
        &self,
        tcx: rustc_middle::ty::TyCtxt<'tcx>,
    ) -> Box<dyn Any> {
        // Clear any stale upstream-CGU stash from a prior compilation (tests
        // may rerun `codegen_crate` with a fresh `tcx`). The stash will be
        // repopulated when the partitioner override fires inside
        // `inner.codegen_crate` below.
        // Note: the stash is cleared in `LangDriver::config` (once per compile
        // session) rather than here. The partitioner override may fire before
        // this function — notably under stage 5b two-crate, rustc queries
        // `collect_and_partition_mono_items` during rlib metadata setup, which
        // runs before `LangCodegenBackend::codegen_crate`. Clearing here would
        // wipe that pre-codegen stash, and the subsequent `inner.codegen_crate`
        // call would reuse the query's cached result without re-firing the
        // override — leaving the consumer's `generate_and_compile` with an
        // empty stash. Clearing once at `config()` time is both sufficient
        // (prevents stale pointers from prior TyCtxts across direct-mode
        // test runs) and correct (no mid-session wipe).

        // Phase 1: inner.codegen_crate runs monomorphization (collect_and_partition_mono_items)
        // then compiles Rust code to LLVM. Our collect_generic_rust_deps /
        // notify_concrete_entry_point / monomorphize_type callbacks fire during
        // this phase. Query providers (symbol_name, layout_of, etc.) also fire
        // here — their results get cached in rustc's query system.
        let inner = self.inner.codegen_crate(tcx);

        // Phase 2: generate_and_compile. Per @GCMLZ, this locks MUTABLE_STATE for the
        // entire duration. Query providers triggered during codegen read only from
        // CONFIG and DEFAULT_* OnceLocks, avoiding deadlock.
        let lang_obj_path = crate::call_generate_and_compile(tcx)
            .map(|(obj_path, _rust_symbols)| obj_path);

        Box::new(LangOngoingCodegen { inner, lang_obj_path })
    }

    fn join_codegen(
        &self,
        ongoing_codegen: Box<dyn Any>,
        sess: &Session,
        outputs: &OutputFilenames,
    ) -> (CodegenResults, rustc_data_structures::fx::FxIndexMap<
        rustc_middle::dep_graph::WorkProductId,
        rustc_middle::dep_graph::WorkProduct,
    >) {
        let LangOngoingCodegen { inner, lang_obj_path } = *ongoing_codegen
            .downcast::<LangOngoingCodegen>()
            .expect("LangOngoingCodegen wrapper");
        let (mut results, work_products) = self.inner.join_codegen(inner, sess, outputs);

        // Inject the consumer's compiled object as an additional module so
        // rustc's linker picks it up alongside the Rust-compiled ones.
        if let Some(ref obj_path) = lang_obj_path {
            eprintln!("[toylang] injecting module: {}", obj_path.display());
            results.modules.push(CompiledModule {
                name: "toylang_external".to_string(),
                kind: ModuleKind::Regular,
                object: Some(obj_path.clone()),
                dwarf_object: None,
                bytecode: None,
                assembly: None,
                llvm_ir: None,
                links_from_incr_cache: Vec::new(),
            });
        }

        (results, work_products)
    }

    fn link(
        &self,
        sess: &Session,
        codegen_results: CodegenResults,
        metadata: rustc_metadata::EncodedMetadata,
        outputs: &OutputFilenames,
    ) {
        self.inner.link(sess, codegen_results, metadata, outputs);
    }
}
