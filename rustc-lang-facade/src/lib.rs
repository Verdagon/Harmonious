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
extern crate rustc_monomorphize;
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

// ============================================================================
// Two callback families.
//
// `LangPredicates` — pure callbacks that are functions of `(self, tcx, ...)`
// only. The facade dispatches them through `PredicateVtable`; their bridge
// fns and trampolines NEVER touch `MUTABLE_STATE`. Adding a hook here means
// "this hook must not need consumer state, and must not lock."
//
// `LangCallbacks: LangPredicates` — stateful callbacks that mutate consumer
// state. Each takes `&mut dyn Any state`. The facade dispatches them through
// `StatefulVtable`; their helpers lock `MUTABLE_STATE` for the duration of
// the call.
//
// The split exists so the type system enforces the locking story: a hook in
// the predicate family literally cannot lock, because its signature has no
// `state` to mutate. New override-style hooks (visibility, layout overrides,
// etc.) go on `LangPredicates`. New monomorphization or codegen hooks that
// need state go on `LangCallbacks`. See `docs/architecture/rust-interop-guide.md`
// Part 2 for the family taxonomy and @GCMLZ for the locking history.
// ============================================================================

/// Pure callbacks: functions of `(self, tcx, ...)` only, no consumer state.
/// Bridge fns for these MUST NOT lock `MUTABLE_STATE`.
pub trait LangPredicates: Send + Sync {
    /// Check if a type name belongs to the consumer's language.
    fn is_consumer_type(&self, name: &str) -> bool;

    /// Check if a function name belongs to the consumer's language.
    fn is_consumer_fn(&self, name: &str) -> bool;

    /// Generate the Rust source code to inject via FileLoader.
    fn generate_stubs(&self) -> String;

    /// Optionally override an item's linkage and visibility during CGU
    /// partitioning. Returning `None` defers to rustc's normal logic;
    /// returning `Some((linkage, vis))` short-circuits `mono_item_visibility`
    /// and prevents internalization.
    ///
    /// Used to keep symbols in the consumer's stub module visible to the
    /// externally-linked consumer .o file (rustc's CGU partitioner cannot
    /// see those references and would otherwise internalize the symbols).
    ///
    /// Default impl returns `None` — consumers that don't need this hook
    /// can ignore it.
    fn visibility_override<'tcx>(
        &self,
        _tcx: TyCtxt<'tcx>,
        _instance: ty::Instance<'tcx>,
    ) -> Option<(rustc_middle::mir::mono::Linkage, rustc_middle::mir::mono::Visibility)> {
        None
    }
}

/// Stateful callbacks: each takes `&mut dyn Any state` (downcast to your
/// concrete state type). Bridge fns for these lock `MUTABLE_STATE` for the
/// duration of the call.
pub trait LangCallbacks: LangPredicates {
    /// Create the consumer's mutable state. Called once at startup.
    /// The facade stores this in its global and passes `&mut dyn Any` to every
    /// stateful callback. The consumer downcasts to its concrete state type.
    fn create_state(&self) -> Box<dyn Any + Send + Sync>;

    /// Monomorphize a consumer type for concrete type args.
    fn monomorphize_type<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>;

    /// Monomorphize a consumer function for a concrete instantiation.
    fn monomorphize_fn<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx>;

    /// Called after rustc's analysis phase completes.
    fn after_rust_analysis<'tcx>(&self, state: &mut dyn Any, tcx: TyCtxt<'tcx>);

    /// Compile the consumer's function bodies and return the path to the .o file.
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

/// Vtable for the pure (`LangPredicates`) callback family. Bridge fns and
/// helpers reading from this vtable NEVER lock `MUTABLE_STATE`.
struct PredicateVtable {
    is_consumer_type: fn(&(dyn Any + Send + Sync), &str) -> bool,
    is_consumer_fn: fn(&(dyn Any + Send + Sync), &str) -> bool,
    generate_stubs: fn(&(dyn Any + Send + Sync)) -> String,

    visibility_override: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        TyCtxt<'tcx>,
        ty::Instance<'tcx>,
    ) -> Option<(rustc_middle::mir::mono::Linkage, rustc_middle::mir::mono::Visibility)>,
}

/// Vtable for the stateful (`LangCallbacks`) callback family. Helpers
/// dispatching through this vtable lock `MUTABLE_STATE` for the duration of
/// the call.
struct StatefulVtable {
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

/// Immutable config: callbacks + the two vtables. Set once by `install_callbacks`.
/// `predicate_vtable` is dispatched lock-free; `stateful_vtable` is dispatched
/// while holding `MUTABLE_STATE`.
pub(crate) struct FacadeConfig {
    callbacks: Box<dyn Any + Send + Sync>,
    predicate_vtable: PredicateVtable,
    stateful_vtable: StatefulVtable,
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
    (c.predicate_vtable.is_consumer_type)(&*c.callbacks, name)
}

/// Generate the consumer's stub source. Predicate (lock-free).
pub(crate) fn generate_stubs() -> String {
    let c = CONFIG.get().expect("config not installed");
    (c.predicate_vtable.generate_stubs)(&*c.callbacks)
}

/// Check if a DefId is from the __lang_stubs module (the consumer's injected stubs).
///
/// Per @DPSFDOZ, this uses `def_path_str` and is therefore safe ONLY from
/// callers that run inside `generate_and_compile` (where rustc's diagnostic
/// machinery is permissive). Calling this from a query provider, the
/// partitioner, or any other normal-compilation hot path will ICE with
/// `'trimmed_def_paths' called, diagnostics were expected but none were
/// emitted`. For those contexts, walk `tcx.def_path(def_id).data` directly
/// and match `DefPathData::TypeNs("__lang_stubs")`.
pub fn is_from_lang_stubs(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let path = tcx.def_path_str(def_id);
    path.starts_with("__lang_stubs::")
}

/// Check if a function name belongs to the consumer's language.
/// Per @GCMLZ, reads from CONFIG (no lock) — safe during generate_and_compile.
pub(crate) fn is_consumer_fn(name: &str) -> bool {
    let c = CONFIG.get().expect("config not installed");
    (c.predicate_vtable.is_consumer_fn)(&*c.callbacks, name)
}

/// Call the consumer's monomorphize_type. Holds the mutable state mutex for the entire call.
pub(crate) fn call_monomorphize_type<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.monomorphize_type;
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
    let func = c.stateful_vtable.monomorphize_fn;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, def_id, instance)
}

/// Call the consumer's after_rust_analysis. Holds the mutable state mutex for the entire call.
pub(crate) fn call_after_rust_analysis<'tcx>(tcx: TyCtxt<'tcx>) {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.after_rust_analysis;
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
    let func = c.stateful_vtable.generate_and_compile;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx)
}

/// Bridge function registered into rustc_monomorphize's
/// `VISIBILITY_OVERRIDE_HOOK` static at startup. The partitioner calls
/// this as a plain `fn` pointer; it forwards through `predicate_vtable`
/// to the consumer's `visibility_override` impl.
///
/// Lock-free: dispatches a `LangPredicates` callback which by definition
/// has no `&mut state` to lock. This is what makes adding partitioner-time
/// hooks safe — the trait family enforces it structurally, no prose
/// invariant required. See @GCMLZ for the broader locking story this
/// pattern resolves.
///
/// Returns `None` if config isn't installed yet (very early init), so
/// the partitioner falls through to its default logic harmlessly.
fn facade_visibility_override<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_middle::mir::mono::Linkage, rustc_middle::mir::mono::Visibility)> {
    let c = CONFIG.get()?;
    let func = c.predicate_vtable.visibility_override;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    (func)(unsafe { &*callbacks_ptr }, tcx, instance)
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
//
// Predicate trampolines (LangPredicates) take no `state`; stateful trampolines
// (LangCallbacks) take `&mut (dyn Any + Send + Sync)`. The two-family split is
// expressed by signature: predicate trampolines literally cannot touch state.

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

fn trampoline_generate_stubs<C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
) -> String {
    data.downcast_ref::<C>().unwrap().generate_stubs()
}

fn trampoline_visibility_override<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_middle::mir::mono::Linkage, rustc_middle::mir::mono::Visibility)> {
    data.downcast_ref::<C>().unwrap().visibility_override(tcx, instance)
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
        predicate_vtable: PredicateVtable {
            is_consumer_type: trampoline_is_consumer_type::<C>,
            is_consumer_fn: trampoline_is_consumer_fn::<C>,
            generate_stubs: trampoline_generate_stubs::<C>,
            visibility_override: trampoline_visibility_override::<C>,
        },
        stateful_vtable: StatefulVtable {
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
    // Register the partitioner visibility-override hook so the rustc fork
    // can call back into the consumer. Idempotent — `set` returns Err if
    // already installed (e.g. test re-entry); we ignore.
    let _ = rustc_monomorphize::partitioning::VISIBILITY_OVERRIDE_HOOK
        .set(facade_visibility_override);
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
