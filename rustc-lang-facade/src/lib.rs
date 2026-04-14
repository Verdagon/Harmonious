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
use std::any::Any;

pub trait LangCallbacks: Send + Sync {
    /// Create the consumer's mutable state. Called once at startup.
    /// The facade stores this in its global and passes `&mut dyn Any` to every
    /// callback. The consumer downcasts to its concrete state type.
    fn create_state(&self) -> Box<dyn Any + Send + Sync>;

    /// Check if a type name belongs to the consumer's language.
    fn is_consumer_type(&self, name: &str) -> bool;

    /// Check if a function name belongs to the consumer's language.
    fn is_consumer_fn(&self, name: &str) -> bool;

    /// Generate the Rust source code to inject via FileLoader.
    fn generate_stubs(&self) -> String;

    /// Monomorphize a consumer type for concrete type args.
    /// `state` is the consumer's mutable state (downcast to your concrete type).
    fn monomorphize_type<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>;

    /// Monomorphize a consumer function for a concrete instantiation.
    /// `state` is the consumer's mutable state (downcast to your concrete type).
    fn monomorphize_fn<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx>;

    /// Called after rustc's analysis phase completes.
    /// `state` is the consumer's mutable state (downcast to your concrete type).
    fn after_rust_analysis<'tcx>(&self, state: &mut dyn Any, tcx: TyCtxt<'tcx>);

    /// Compile the consumer's function bodies and return the path to the .o file.
    /// `state` is the consumer's mutable state (downcast to your concrete type).
    fn generate_and_compile<'tcx>(&self, state: &mut dyn Any, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)>;
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

use std::sync::OnceLock;

/// Manual vtable for LangCallbacks, using higher-ranked function pointers
/// to handle the `'tcx` lifetime without requiring object safety.
struct CallbackVtable {
    is_consumer_type: fn(&(dyn Any + Send + Sync), &str) -> bool,
    is_consumer_fn: fn(&(dyn Any + Send + Sync), &str) -> bool,

    monomorphize_type: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>,

    monomorphize_fn: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        LocalDefId,
        ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx>,

    after_rust_analysis: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        TyCtxt<'tcx>,
    ),

    generate_and_compile: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        TyCtxt<'tcx>,
    ) -> Option<(PathBuf, Vec<String>)>,
}

// ============================================================================
// Global state, split into immutable config and mutable state (@GCMLZ).
//
// Immutable config (callbacks, vtable, default query providers) is stored in
// OnceLock statics — set once during init, never changes, no locking needed
// for reads. This allows query providers to read config without contending
// with the mutable state mutex.
//
// Mutable state (consumer_state, lang_obj_path) is behind its own Mutex.
// Only callbacks that need &mut consumer_state lock this mutex.
//
// This separation prevents deadlocks: generate_and_compile holds the mutable
// state mutex, but query providers triggered during codegen (e.g. symbol_name,
// layout_of) only need immutable config — they never touch the state mutex.
// See docs/arcana/GenerateCompileMutexLock-GCMLZ.md for the full analysis.
// ============================================================================

/// Immutable config: callbacks + vtable. Set once by install_callbacks.
pub(crate) struct FacadeConfig {
    callbacks: Box<dyn Any + Send + Sync>,
    vtable: CallbackVtable,
}

// Safety: callbacks is Box<dyn Any + Send + Sync>, vtable contains plain fn pointers.
unsafe impl Send for FacadeConfig {}
unsafe impl Sync for FacadeConfig {}

/// Mutable state: consumer-owned state + codegen output path.
pub(crate) struct FacadeMutableState {
    consumer_state: Box<dyn Any + Send + Sync>,
    pub lang_obj_path: Option<PathBuf>,
}

// Safety: consumer_state is Box<dyn Any + Send + Sync>.
unsafe impl Send for FacadeMutableState {}
unsafe impl Sync for FacadeMutableState {}

/// Immutable config (callbacks + vtable). Set once, never changes.
static CONFIG: OnceLock<FacadeConfig> = OnceLock::new();

/// Default query providers saved from rustc. Set once, never changes.
static DEFAULT_LAYOUT_OF: OnceLock<queries::layout::LayoutOfFn> = OnceLock::new();
static DEFAULT_MIR_SHIMS: OnceLock<queries::drop_glue::MirShimsFn> = OnceLock::new();
static DEFAULT_SYMBOL_NAME: OnceLock<queries::symbol_name::SymbolNameFn> = OnceLock::new();

/// Mutable state. Locked only by callbacks that need &mut consumer_state.
static MUTABLE_STATE: OnceLock<std::sync::Mutex<FacadeMutableState>> = OnceLock::new();

/// Check if a type name belongs to the consumer's language.
/// Per @GCMLZ, reads from CONFIG (no lock) — safe during generate_and_compile.
pub(crate) fn is_consumer_type(name: &str) -> bool {
    let c = CONFIG.get().expect("config not installed");
    (c.vtable.is_consumer_type)(&*c.callbacks, name)
}

/// Check if a DefId is from the __lang_stubs module (the consumer's injected stubs).
pub fn is_from_lang_stubs(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let path = tcx.def_path_str(def_id);
    path.starts_with("__lang_stubs::")
}

/// Check if a function name belongs to the consumer's language.
/// Per @GCMLZ, reads from CONFIG (no lock) — safe during generate_and_compile.
pub(crate) fn is_consumer_fn(name: &str) -> bool {
    let c = CONFIG.get().expect("config not installed");
    (c.vtable.is_consumer_fn)(&*c.callbacks, name)
}

/// Call the consumer's monomorphize_type. Holds the mutable state mutex for the entire call.
pub(crate) fn call_monomorphize_type<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.vtable.monomorphize_type;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    // Safety: callbacks is immutable (from CONFIG, no lock needed).
    // state is mutable (from MUTABLE_STATE, lock held for the entire call).
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, ty)
}

/// Call the consumer's monomorphize_fn. Holds the mutable state mutex for the entire call.
pub(crate) fn call_monomorphize_fn<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    instance: ty::Instance<'tcx>,
) -> MonomorphizeFnResult<'tcx> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.vtable.monomorphize_fn;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, def_id, instance)
}

/// Call the consumer's after_rust_analysis. Holds the mutable state mutex for the entire call.
pub(crate) fn call_after_rust_analysis<'tcx>(tcx: TyCtxt<'tcx>) {
    let c = CONFIG.get().expect("config not installed");
    let func = c.vtable.after_rust_analysis;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx)
}

/// Call the consumer's generate_and_compile. Per @GCMLZ, holds MUTABLE_STATE
/// for the entire call. Query providers triggered during codegen read only from
/// CONFIG and DEFAULT_* OnceLocks (no lock), so no deadlock.
pub(crate) fn call_generate_and_compile<'tcx>(
    tcx: TyCtxt<'tcx>,
) -> Option<(PathBuf, Vec<String>)> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.vtable.generate_and_compile;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx)
}

/// Read saved default query providers. Per @GCMLZ, no locking — stored in
/// OnceLock so they're safe to call during generate_and_compile.
pub(crate) fn default_layout_of() -> queries::layout::LayoutOfFn {
    *DEFAULT_LAYOUT_OF.get().expect("default layout_of not saved")
}

pub(crate) fn default_mir_shims() -> queries::drop_glue::MirShimsFn {
    *DEFAULT_MIR_SHIMS.get().expect("default mir_shims not saved")
}

pub(crate) fn default_symbol_name() -> queries::symbol_name::SymbolNameFn {
    *DEFAULT_SYMBOL_NAME.get().expect("default symbol_name not saved")
}

/// Store the compiled .o path after generate_and_compile.
pub(crate) fn set_lang_obj_path(obj_path: PathBuf) {
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    g.lang_obj_path = Some(obj_path);
}

/// Read the compiled .o path for injection into CodegenResults.
pub(crate) fn get_lang_obj_path() -> Option<PathBuf> {
    MUTABLE_STATE.get()
        .and_then(|m| m.lock().unwrap().lang_obj_path.clone())
}

// Trampoline functions — monomorphized for a specific C, then stored as fn pointers.

fn trampoline_is_consumer_type<C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    name: &str,
) -> bool {
    data.downcast_ref::<C>().unwrap().is_consumer_type(name)
}

fn trampoline_is_consumer_fn<C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    name: &str,
) -> bool {
    data.downcast_ref::<C>().unwrap().is_consumer_fn(name)
}

fn trampoline_monomorphize_type<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_type(state, name, tcx, ty)
}

fn trampoline_monomorphize_fn<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    instance: ty::Instance<'tcx>,
) -> MonomorphizeFnResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_fn(state, name, tcx, def_id, instance)
}

fn trampoline_after_rust_analysis<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
) {
    data.downcast_ref::<C>().unwrap().after_rust_analysis(state, tcx)
}

fn trampoline_generate_and_compile<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
) -> Option<(PathBuf, Vec<String>)> {
    data.downcast_ref::<C>().unwrap().generate_and_compile(state, tcx)
}

/// Install callbacks for use by query overrides. Phase 1 of globals init.
pub(crate) fn install_callbacks<C: LangCallbacks + 'static>(
    callbacks: C,
) {
    let consumer_state = callbacks.create_state();
    let _ = CONFIG.set(FacadeConfig {
        callbacks: Box::new(callbacks),
        vtable: CallbackVtable {
            is_consumer_type: trampoline_is_consumer_type::<C>,
            is_consumer_fn: trampoline_is_consumer_fn::<C>,
            monomorphize_type: trampoline_monomorphize_type::<C>,
            monomorphize_fn: trampoline_monomorphize_fn::<C>,
            after_rust_analysis: trampoline_after_rust_analysis::<C>,
            generate_and_compile: trampoline_generate_and_compile::<C>,
        },
    });
    let _ = MUTABLE_STATE.set(std::sync::Mutex::new(FacadeMutableState {
        consumer_state,
        lang_obj_path: None,
    }));
}

/// Save the original query providers. Phase 2 of globals init.
pub(crate) fn install_query_defaults(
    layout_of: queries::layout::LayoutOfFn,
    mir_shims: queries::drop_glue::MirShimsFn,
    symbol_name: queries::symbol_name::SymbolNameFn,
) {
    let _ = DEFAULT_LAYOUT_OF.set(layout_of);
    let _ = DEFAULT_MIR_SHIMS.set(mir_shims);
    let _ = DEFAULT_SYMBOL_NAME.set(symbol_name);
}
