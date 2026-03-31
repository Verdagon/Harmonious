extern crate rustc_codegen_llvm;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_metadata;
extern crate rustc_session;
extern crate rustc_middle;

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

pub fn set_toylang_paths(obj_path: PathBuf, symbols_to_globalize: Vec<String>) {
    let _ = TOYLANG_OBJ_PATH.set(obj_path);
    let _ = GLOBALIZE_SYMBOLS.set(symbols_to_globalize);
}

/// A thin CodegenBackend wrapper that delegates everything to LlvmCodegenBackend,
/// then injects Toylang's compiled module into CodegenResults during join_codegen.
pub struct ToylangCodegenBackend {
    inner: Box<dyn CodegenBackend>,
}

impl ToylangCodegenBackend {
    pub fn new() -> Box<dyn CodegenBackend> {
        let inner = rustc_codegen_llvm::LlvmCodegenBackend::new();
        Box::new(Self { inner })
    }
}

impl CodegenBackend for ToylangCodegenBackend {
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

/// Merge two object files using `ld -r` (partial/incremental link).
/// The result replaces the first file. Internal symbols from both files
/// become resolvable within the merged object.
fn merge_objects(rust_obj: &std::path::Path, toylang_obj: &std::path::Path) {
    let merged = rust_obj.with_extension("merged.o");
    eprintln!("[toylang] merging {} + {} → {}",
        rust_obj.display(), toylang_obj.display(), merged.display());
    let status = std::process::Command::new("ld")
        .arg("-r")                    // partial link (relocatable output)
        .arg("-o").arg(&merged)
        .arg(rust_obj.as_os_str())
        .arg(toylang_obj.as_os_str())
        .status()
        .expect("failed to run ld -r");
    assert!(status.success(), "ld -r failed");
    std::fs::rename(&merged, rust_obj).expect("failed to replace .o with merged");
}
