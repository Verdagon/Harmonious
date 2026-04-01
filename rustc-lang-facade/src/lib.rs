//! rustc-lang-facade: a library for integrating custom languages with rustc.
//!
//! Consumers implement the `LangCallbacks` trait and call `run_compiler()`.
//! The library handles query overrides, stub injection, codegen backend
//! wrapping, and the rustc driver lifecycle.

#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_codegen_llvm;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_interface;
extern crate rustc_metadata;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_target;

pub mod abi_helpers;
pub mod codegen_wrapper;
pub mod driver;
pub mod file_loader;
pub mod mir_helpers;
pub mod queries;

use std::collections::HashSet;
use std::path::PathBuf;

use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_span::def_id::DefId;

/// Result of monomorphizing a consumer type for a specific set of type args.
pub struct MonomorphizeTypeResult<'tcx> {
    /// The concrete field types for this instantiation, in declaration order.
    /// The library calls tcx.layout_of() on each to compute struct layout.
    /// E.g. for MyStruct<i32>: field_types might be [tcx.types.i32, Vec<i32>].
    pub field_types: Vec<Ty<'tcx>>,
    /// Rust generic instantiations (types or functions) this type depends on.
    /// E.g. if the struct contains a Vec<i32> field, the consumer might want
    /// to trigger monomorphization of Vec<i32>'s drop glue or methods.
    pub rust_deps: Vec<(DefId, ty::GenericArgsRef<'tcx>)>,
}

/// Result of monomorphizing a consumer function for a specific set of type args.
pub struct MonomorphizeFnResult<'tcx> {
    /// The extern symbol name for this monomorphized function.
    /// The library builds a MIR call stub that calls this symbol.
    /// E.g. "__mylang_impl_make_counter" or "__mylang_impl_wrap_i32".
    pub extern_symbol: String,
    /// Rust generic instantiations (types or functions) this body depends on.
    /// The library emits phantom casts in the MIR stub so rustc's
    /// monomorphizer will stamp these out. Can include both Rust function
    /// instantiations (e.g. Vec::push<i32>) and Rust type instantiations
    /// (e.g. HashMap<MyKey, MyValue>).
    pub rust_deps: Vec<(DefId, ty::GenericArgsRef<'tcx>)>,
}

/// The main interface between the library and a consumer language.
///
/// The library identifies consumer items automatically by tracking which DefIds
/// came from the stub file (injected via generate_stubs). The consumer does not
/// need to provide is_lang_type / is_lang_fn methods.
///
/// Must be Send + Sync because rustc query providers run on Rayon worker threads.
pub trait LangCallbacks: Send + Sync {
    /// Return the names of all consumer-defined types.
    /// Used by the library to detect consumer items in query overrides.
    ///
    /// Future: if generate_stubs returned structured data (e.g. a list of
    /// TypeStub/FnStub with names + source), the library could extract names
    /// from there and this method wouldn't be needed.
    fn type_names(&self) -> HashSet<String>;

    /// Return the names of all consumer-defined functions.
    /// Used by the library to detect consumer items in query overrides.
    /// See type_names() for future simplification note.
    fn fn_names(&self) -> HashSet<String>;

    /// Generate the Rust source code to inject via FileLoader.
    /// Must contain struct definitions and extern "C" declarations that
    /// make the consumer's types visible to rustc's type checker.
    /// Called once before rustc parsing begins.
    fn generate_stubs(&self) -> String;

    /// Monomorphize a consumer type for concrete type args.
    /// Called from the layout_of override when rustc needs the layout of a
    /// consumer type (possibly a generic instantiation like MyStruct<i32>).
    ///
    /// `ty` is the concrete type (e.g. `Pair<i32, i32>`), with generic args
    /// already substituted. The consumer can extract type args from it via
    /// `if let TyKind::Adt(_, args) = ty.kind() { args[0].expect_ty() }`.
    ///
    /// Returns the concrete field types for this instantiation. The library
    /// calls tcx.layout_of() on each field type to compute the struct layout.
    fn monomorphize_type<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>;

    /// Monomorphize a consumer function for concrete type args.
    /// Called from the mir_built override when rustc's monomorphizer
    /// encounters a consumer function.
    ///
    /// This is the hook point for the consumer's own monomorphizer. The flow:
    /// 1. Rustc's monomorphizer wants MIR for e.g. `wrap<i32>`
    /// 2. The library's mir_built override calls this method
    /// 3. The consumer monomorphizes its generic IR for `wrap` with `T = i32`
    /// 4. During that, the consumer discovers it needs `Vec::push<i32>`
    /// 5. The consumer picks a symbol name for the monomorphized body
    /// 6. The consumer returns MonomorphizeFnResult with the symbol + deps
    /// 7. The library builds a MIR call stub targeting that symbol
    /// 8. Rustc's monomorphizer sees phantom casts → monomorphizes the deps
    fn monomorphize_fn<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
    ) -> MonomorphizeFnResult<'tcx>;

    /// Called after rustc's analysis phase completes (after type checking,
    /// before monomorphization). Full tcx available.
    ///
    /// Use this to type-check your language against Rust types — e.g. resolve
    /// what methods Vec has, verify that trait bounds are satisfied, etc.
    /// Results should be stashed for later use by monomorphize_fn/generate_and_compile.
    fn after_rust_analysis<'tcx>(&self, tcx: TyCtxt<'tcx>);

    /// Compile the consumer's function bodies and return the path to the .o file.
    /// Called during after_analysis when the full TyCtxt is available for
    /// symbol name resolution, ABI queries, etc. Return None if no codegen needed.
    fn generate_and_compile<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)>;
}

// ============================================================================
// Vtable + trampoline machinery for storing dyn LangCallbacks in globals.
//
// The trait has generic lifetime methods (<'tcx>), which makes it not
// object-safe (can't use dyn LangCallbacks). We work around this by:
// 1. Storing the callbacks as Box<dyn Any + Send + Sync> (type-erased)
// 2. Creating a manual vtable of HRTB function pointers
// 3. run_compiler<C>() monomorphizes trampolines for the concrete C,
//    storing them as function pointers in the vtable
// 4. Query overrides call through the vtable, which downcasts and dispatches
// ============================================================================

use std::any::Any;
use std::sync::OnceLock;

/// Manual vtable for LangCallbacks, using higher-ranked function pointers
/// to handle the `'tcx` lifetime without requiring object safety.
pub(crate) struct CallbackVtable {
    pub monomorphize_type: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>,

    pub monomorphize_fn: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        LocalDefId,
    ) -> MonomorphizeFnResult<'tcx>,

    pub after_rust_analysis: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        TyCtxt<'tcx>,
    ),

    pub generate_and_compile: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        TyCtxt<'tcx>,
    ) -> Option<(PathBuf, Vec<String>)>,
}

pub(crate) static CALLBACKS: OnceLock<Box<dyn Any + Send + Sync>> = OnceLock::new();
pub(crate) static VTABLE: OnceLock<CallbackVtable> = OnceLock::new();

/// Names of consumer-defined types and functions, for query override detection.
pub(crate) static CONSUMER_TYPE_NAMES: OnceLock<HashSet<String>> = OnceLock::new();
pub(crate) static CONSUMER_FN_NAMES: OnceLock<HashSet<String>> = OnceLock::new();

/// Check if a type name belongs to the consumer's language.
pub(crate) fn is_consumer_type(name: &str) -> bool {
    CONSUMER_TYPE_NAMES.get().map_or(false, |s| s.contains(name))
}

/// Check if a function name belongs to the consumer's language.
pub(crate) fn is_consumer_fn(name: &str) -> bool {
    CONSUMER_FN_NAMES.get().map_or(false, |s| s.contains(name))
}

/// Call the consumer's monomorphize_type through the vtable.
pub(crate) fn call_monomorphize_type<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    let vtable = VTABLE.get().expect("vtable not installed");
    let data = CALLBACKS.get().expect("callbacks not installed");
    (vtable.monomorphize_type)(data.as_ref(), name, tcx, ty)
}

/// Call the consumer's monomorphize_fn through the vtable.
pub(crate) fn call_monomorphize_fn<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> MonomorphizeFnResult<'tcx> {
    let vtable = VTABLE.get().expect("vtable not installed");
    let data = CALLBACKS.get().expect("callbacks not installed");
    (vtable.monomorphize_fn)(data.as_ref(), name, tcx, def_id)
}

/// Call the consumer's after_rust_analysis through the vtable.
pub(crate) fn call_after_rust_analysis<'tcx>(tcx: TyCtxt<'tcx>) {
    let vtable = VTABLE.get().expect("vtable not installed");
    let data = CALLBACKS.get().expect("callbacks not installed");
    (vtable.after_rust_analysis)(data.as_ref(), tcx)
}

/// Call the consumer's generate_and_compile through the vtable.
pub(crate) fn call_generate_and_compile<'tcx>(
    tcx: TyCtxt<'tcx>,
) -> Option<(PathBuf, Vec<String>)> {
    let vtable = VTABLE.get().expect("vtable not installed");
    let data = CALLBACKS.get().expect("callbacks not installed");
    (vtable.generate_and_compile)(data.as_ref(), tcx)
}

// Trampoline functions — monomorphized for a specific C, then stored as fn pointers.

fn trampoline_monomorphize_type<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_type(name, tcx, ty)
}

fn trampoline_monomorphize_fn<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> MonomorphizeFnResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_fn(name, tcx, def_id)
}

fn trampoline_after_rust_analysis<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
) {
    data.downcast_ref::<C>().unwrap().after_rust_analysis(tcx)
}

fn trampoline_generate_and_compile<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
) -> Option<(PathBuf, Vec<String>)> {
    data.downcast_ref::<C>().unwrap().generate_and_compile(tcx)
}

/// Install callbacks for use by query overrides.
/// Called from run_compiler<C>() with the concrete consumer type.
pub(crate) fn install_callbacks<C: LangCallbacks + 'static>(
    callbacks: C,
    type_names: HashSet<String>,
    fn_names: HashSet<String>,
) {
    let _ = CALLBACKS.set(Box::new(callbacks));
    let _ = VTABLE.set(CallbackVtable {
        monomorphize_type: trampoline_monomorphize_type::<C>,
        monomorphize_fn: trampoline_monomorphize_fn::<C>,
        after_rust_analysis: trampoline_after_rust_analysis::<C>,
        generate_and_compile: trampoline_generate_and_compile::<C>,
    });
    let _ = CONSUMER_TYPE_NAMES.set(type_names);
    let _ = CONSUMER_FN_NAMES.set(fn_names);
}
