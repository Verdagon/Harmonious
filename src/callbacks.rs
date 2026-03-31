#![allow(unused)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;

use rustc_driver::Compilation;
use rustc_interface::Config;
use rustc_middle::ty::TyCtxt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::toylang::registry::ToylangRegistry;

pub struct ToyCallbacks {
    registry: Arc<ToylangRegistry>,
    stubs: String,
    /// If Some, the LLVM backend will generate IR and compile to .o during after_analysis.
    /// (ll_path, obj_path)
    llvm_paths: Option<(PathBuf, PathBuf)>,
}

impl ToyCallbacks {
    pub fn new(
        registry: Arc<ToylangRegistry>,
        stubs: String,
        llvm_paths: Option<(PathBuf, PathBuf)>,
    ) -> Self {
        Self { registry, stubs, llvm_paths }
    }
}

impl rustc_driver::Callbacks for ToyCallbacks {
    fn config(&mut self, config: &mut Config) {
        config.file_loader = Some(Box::new(
            crate::file_loader::ToylangFileLoader::new(self.stubs.clone())
        ));
        crate::queries::layout::install_registry(self.registry.clone());
        crate::queries::borrowck::install_registry(self.registry.clone());
        crate::queries::mir_build::install_registry(self.registry.clone());
        crate::queries::drop_glue::install_registry(self.registry.clone());
        config.override_queries = Some(crate::queries::toy_override_queries);

        // Install the CodegenBackend wrapper that will inject Toylang's
        // compiled module into rustc's LTO pipeline.
        if self.llvm_paths.is_some() {
            config.make_codegen_backend = Some(Box::new(|_opts| {
                crate::codegen_wrapper::ToylangCodegenBackend::new()
            }));
        }
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        if std::env::var("TOYLANG_DUMP_TYPES").is_ok() {
            crate::oracle::dump_toylang_oracle(tcx, &self.registry);
        }

        // Generate LLVM IR for externally-compiled functions.
        if let Some((ref ll_path, ref obj_path)) = self.llvm_paths {
            let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(tcx, &self.registry);
            std::fs::write(ll_path, &llvm_ir)
                .expect("toylang: failed to write .ll file");
            crate::compile_llvm_ir(ll_path, obj_path);

            crate::codegen_wrapper::set_toylang_paths(obj_path.clone(), rust_symbols);
        }

        Compilation::Continue
    }
}
