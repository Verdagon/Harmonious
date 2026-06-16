//! Thin `CodegenBackend` wrapper.
//!
//! Phase 2 step 3 (inline-codegen plan) retired this module's main job — the
//! post-hoc `.o` injection in `join_codegen` — once the consumer's
//! `consumer_emit_modules` started feeding Sky's CGU into rustc's own
//! optimization + ThinLTO + emission pipeline via the patch-(c)
//! `extra_modules` hook. What remains here is a vestigial pass-through:
//! the wrapper exists only to satisfy `config.make_codegen_backend`'s
//! interface, and every method delegates to the inner `LlvmCodegenBackend`.
//!
//! Tier 3 #12 (a future workstream) will likely retire this whole wrapper
//! and have `make_codegen_backend` return `LlvmCodegenBackend::new()`
//! directly — the patch-(c) hook is the integration point.

use rustc_codegen_ssa::CodegenResults;
use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_middle::util::Providers;
use rustc_session::config::OutputFilenames;
use rustc_session::Session;
use std::any::Any;

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
        // Sky's CGU is contributed via the `extra_modules` hook installed in
        // `LangDriver::config` (see `extra_modules_hook` module). No
        // post-hoc work needed here.
        self.inner.codegen_crate(tcx)
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
        self.inner.join_codegen(ongoing_codegen, sess, outputs)
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
