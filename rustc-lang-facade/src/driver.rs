#![allow(unused)]

use rustc_driver::Compilation;
use rustc_interface::Config;
use rustc_middle::ty::TyCtxt;

use crate::LangCallbacks;

/// Entry point for consumers. Takes a concrete `LangCallbacks` implementation
/// and rustc command-line arguments. Handles the full driver lifecycle:
/// stub injection, query overrides, codegen backend wrapping.
pub fn run_compiler<C: LangCallbacks + 'static>(
    callbacks: C,
    rustc_args: &[String],
) {
    let stubs = callbacks.generate_stubs();
    let type_names = callbacks.type_names();
    let fn_names = callbacks.fn_names();
    crate::install_callbacks(callbacks, type_names, fn_names);
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
        // Phase 1.5: let consumer type-check against Rust types
        crate::call_after_rust_analysis(tcx);

        // Phase 3: consumer codegen
        if let Some((obj_path, rust_symbols)) = crate::call_generate_and_compile(tcx) {
            crate::codegen_wrapper::set_lang_compiled_object(obj_path, rust_symbols);
        }

        Compilation::Continue
    }
}
