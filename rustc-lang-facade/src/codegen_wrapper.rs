
use rustc_codegen_ssa::{CodegenResults, CompiledModule, ModuleKind};
use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_middle::util::Providers;
use rustc_session::config::OutputFilenames;
use rustc_session::Session;
use std::any::Any;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Global paths set by after_analysis, read by join_codegen.
static TOYLANG_OBJ_PATH: OnceLock<PathBuf> = OnceLock::new();
/// Mangled Rust symbols that need to be globalized in the Rust .o files.
static GLOBALIZE_SYMBOLS: OnceLock<Vec<String>> = OnceLock::new();

pub fn set_lang_compiled_object(obj_path: PathBuf, symbols_to_globalize: Vec<String>) {
    let _ = TOYLANG_OBJ_PATH.set(obj_path);
    let _ = GLOBALIZE_SYMBOLS.set(symbols_to_globalize);
}

/// A thin CodegenBackend wrapper that delegates everything to LlvmCodegenBackend,
/// then injects Toylang's compiled module into CodegenResults during join_codegen.
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
        self.inner.codegen_crate(tcx, metadata, need_metadata_module)
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
        if let Some(obj_path) = TOYLANG_OBJ_PATH.get() {
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
