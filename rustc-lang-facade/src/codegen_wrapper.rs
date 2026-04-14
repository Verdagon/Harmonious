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
//! The .o path is communicated via a global `OnceLock`. This is necessary because
//! `after_analysis` (where the consumer compiles the .o) and `join_codegen`
//! (where we inject it) are different phases — the codegen backend has no direct
//! reference to the consumer's state. See `docs/historical/design-codegen-integration.md`
//! for the investigation of alternative approaches (Fat LTO, linker-plugin LTO,
//! objcopy, ld -r, ExtraBackendMethods) and why they all failed.
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

/// Store the path to the consumer's compiled .o file for later injection.
/// Called from `LangCodegenBackend::codegen_crate` after the consumer's
/// `generate_and_compile` callback returns.
pub fn set_lang_compiled_object(obj_path: PathBuf, _rust_symbols: Vec<String>) {
    crate::set_lang_obj_path(obj_path);
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
        metadata: rustc_metadata::EncodedMetadata,
        need_metadata_module: bool,
    ) -> Box<dyn Any> {
        // Phase 1: inner.codegen_crate runs monomorphization (collect_and_partition_mono_items)
        // then compiles Rust code to LLVM. Our monomorphize_fn/monomorphize_type callbacks
        // fire during this phase. Query providers (symbol_name, layout_of, etc.) also fire
        // here — their results get cached in rustc's query system.
        let result = self.inner.codegen_crate(tcx, metadata, need_metadata_module);

        // Phase 2: generate_and_compile. Per @GCMLZ, this locks MUTABLE_STATE for the
        // entire duration. Query providers triggered during codegen read only from
        // CONFIG and DEFAULT_* OnceLocks, avoiding deadlock.
        if let Some((obj_path, rust_symbols)) = crate::call_generate_and_compile(tcx) {
            set_lang_compiled_object(obj_path, rust_symbols);
        }

        result
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
        let (mut results, work_products) = self.inner.join_codegen(ongoing_codegen, sess, outputs);

        // Inject Toylang's compiled object as an additional module.
        // Fat LTO will merge this with the Rust modules.
        // Add Toylang's compiled object as an additional module.
        let obj_path = crate::get_lang_obj_path();
        if let Some(ref obj_path) = obj_path {
            eprintln!("[toylang] injecting module: {}", obj_path.display());
            results.modules.push(CompiledModule {
                name: "toylang_external".to_string(),
                kind: ModuleKind::Regular,
                object: Some(obj_path.clone()),
                dwarf_object: None,
                bytecode: None,
                assembly: None,
                llvm_ir: None,
            });
        }

        (results, work_products)
    }

    fn link(
        &self,
        sess: &Session,
        codegen_results: CodegenResults,
        outputs: &OutputFilenames,
    ) {
        self.inner.link(sess, codegen_results, outputs);
    }
}
