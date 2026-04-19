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

mod cgu_stash;
pub use cgu_stash::upstream_cgus;
pub(crate) use cgu_stash::{clear_upstream_cgus, stash_upstream_cgus};

use std::path::PathBuf;

use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, GenericArgsRef, Ty, TyCtxt};
use rustc_span::def_id::DefId;

/// Result of monomorphizing a consumer type for a specific set of type args.
pub struct MonomorphizeTypeResult<'tcx> {
    /// The concrete field types for this instantiation, in declaration order.
    /// The library calls tcx.layout_of() on each to compute struct layout.
    /// E.g. for MyStruct<i32>: field_types might be [tcx.types.i32, Vec<i32>].
    pub field_types: Vec<Ty<'tcx>>,
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

    // Stage 4c retired `visibility_override`: the partitioner override in
    // `queries::partition` now forces `(External, Default)` on
    // `__lang_stubs` items directly. No trait method needed.
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

    /// Called from the `optimized_mir` query override for each consumer
    /// function `LocalDefId`. Returns the Rust items this consumer function
    /// transitively depends on, as `(DefId, GenericArgsRef)` pairs. The
    /// returned args may contain `ty::TyKind::Param` placeholders; rustc's
    /// monomorphization collector substitutes them per caller during its
    /// own walk of the synthesized MIR body.
    ///
    /// Because rustc does not see internal consumer→consumer callees, the
    /// implementation must walk the consumer side transitively to gather
    /// deps reachable through those callees.
    ///
    /// Must NOT populate consumer state related to internal-callee codegen.
    /// Internal-callee discovery is the job of `notify_concrete_entry_point`
    /// instead. For cycle-breaking during recursive traversal, use a local
    /// `HashSet` — do not reuse persistent dedup state from
    /// `notify_concrete_entry_point`.
    fn collect_generic_rust_deps<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
    ) -> Vec<(DefId, GenericArgsRef<'tcx>)>;

    /// Called from the `symbol_name` query provider for each concrete
    /// consumer entry-point Instance. Returns the extern symbol the consumer
    /// has chosen for this Instance.
    ///
    /// May mutate state — specifically, this is the hook that drives internal
    /// consumer→consumer transitive discovery and stashing into codegen state.
    /// Implementations must dedup across calls so shared internal callees are
    /// stashed exactly once per compilation.
    fn notify_concrete_entry_point<'tcx>(
        &self,
        state: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> String;

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

    collect_generic_rust_deps: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        LocalDefId,
    ) -> Vec<(DefId, GenericArgsRef<'tcx>)>,

    notify_concrete_entry_point: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        ty::Instance<'tcx>,
    ) -> String,

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
static DEFAULT_OPTIMIZED_MIR: OnceLock<queries::optimized_mir::OptimizedMirFn> = OnceLock::new();
static DEFAULT_COLLECT_AND_PARTITION: OnceLock<queries::partition::CollectAndPartitionFn> =
    OnceLock::new();
static DEFAULT_UPSTREAM_MONOMORPHIZATIONS_FOR:
    OnceLock<queries::upstream_monomorphization::UpstreamMonomorphizationsForFn> = OnceLock::new();

/// Mutable state. Locked only by callbacks that need &mut consumer_state.
static MUTABLE_STATE: OnceLock<std::sync::Mutex<FacadeMutableState>> = OnceLock::new();

/// Check if a type name belongs to the consumer's language.
/// Per @GCMLZ, reads from CONFIG (no lock) — safe during generate_and_compile.
pub(crate) fn is_consumer_type(name: &str) -> bool {
    let c = CONFIG.get().expect("config not installed");
    (c.predicate_vtable.is_consumer_type)(&*c.callbacks, name)
}

/// Check if a DefId is from the consumer's `__lang_stubs` (injected module
/// or — under stage-5 two-crate — the rlib crate of that name).
///
/// Per @DPSFDOZ, this uses `def_path_str` and is therefore safe ONLY from
/// callers that run inside `generate_and_compile` (where rustc's diagnostic
/// machinery is permissive). Calling this from a query provider, the
/// partitioner, or any other normal-compilation hot path will ICE with
/// `'trimmed_def_paths' called, diagnostics were expected but none were
/// emitted`. For those contexts, use `is_from_lang_stubs_safe` which walks
/// `DefPathData` structurally.
pub fn is_from_lang_stubs(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    // Stage 5b two-crate fast path: the DefId lives in a crate named
    // `__lang_stubs`. Handles both the rlib's own compile (items local to
    // LOCAL_CRATE, which IS `__lang_stubs`) and the user-bin compile
    // (items in an extern crate of that name). `def_path_str` trims the
    // crate prefix for local items at the crate root, so the string check
    // below can't cover this case — we take the crate name directly.
    if tcx.crate_name(def_id.krate).as_str() == "__lang_stubs" {
        return true;
    }
    // Single-crate FileLoader (direct mode, and wrapper mode pre-stage-5b):
    // items are nested in `mod __lang_stubs {}` inside the user crate. The
    // textual path includes the module name; check for it. Stage 5d retires
    // this branch when FileLoader is deleted.
    let path = tcx.def_path_str(def_id);
    path.starts_with("__lang_stubs::") || path.contains("::__lang_stubs::")
}

/// Cross-crate-safe variant of `is_from_lang_stubs`. Unlike that helper
/// (which uses `def_path_str` and is @DPSFDOZ-gated to diagnostic
/// contexts), this version inspects the crate name + walks `DefPathData`
/// structurally and is safe to call from any phase — the partitioner,
/// pre-`generate_and_compile` hooks, and any future cross-crate paths.
///
/// Slightly more expensive than `is_from_lang_stubs` (a small iterator walk
/// after a constant-time crate-name check vs a string check), but both are
/// dominated by the `tcx.def_path` query underneath so the difference is
/// imperceptible.
///
/// Prefer this over `is_from_lang_stubs` when the call site might run
/// outside `generate_and_compile`, or for the compile-time guarantee
/// that @DPSFDOZ cannot bite.
pub fn is_from_lang_stubs_safe(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    // Stage 5b two-crate: the stub items live in their own rlib whose crate
    // name is `__lang_stubs`. This handles both (a) the rlib's own compile
    // (items are local to LOCAL_CRATE, whose name is `__lang_stubs`) and
    // (b) the user-bin compile (items are in an extern crate of that name).
    if tcx.crate_name(def_id.krate).as_str() == "__lang_stubs" {
        return true;
    }
    // Single-crate FileLoader: items live in a `mod __lang_stubs {}` inside
    // some other (user) crate. Walk the def-path data looking for the
    // module name. Stage 5d retires this branch when FileLoader is deleted.
    use rustc_hir::definitions::DefPathData;
    tcx.def_path(def_id).data.iter().any(|d| {
        matches!(d.data, DefPathData::TypeNs(name) if name.as_str() == "__lang_stubs")
    })
}

/// Is this DefId a consumer-owned function whose real implementation
/// comes from the consumer's backend `.o` rather than from rustc's
/// codegen?
///
/// True iff all three hold:
/// 1. The DefId lives inside `__lang_stubs::` (cross-crate-safe check).
/// 2. The item has a simple name (anonymous impl items etc. are excluded).
/// 3. Either the name matches a consumer function (via `is_consumer_fn`)
///    or it's an accessor method on a consumer type.
///
/// This is the filter the facade uses both (a) to decide whether to
/// synthesize a dep-discovery body for the item (in the MIR override
/// that drives rustc's monomorphization collector) and (b) to tell
/// rustc's codegen to skip emitting a body at the item's symbol.
/// Items inside `__lang_stubs` that are NOT consumer fns — notably
/// the Phase-6 `#[inline(never)]` wrappers like `__toylang_option_unwrap`
/// — fall through to rustc's default codegen; they are real Rust
/// functions whose symbol must be callable at link time.
pub fn is_consumer_codegen_target<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    if !is_from_lang_stubs_safe(tcx, def_id) {
        return false;
    }
    let Some(name) = tcx.opt_item_name(def_id) else {
        return false;
    };
    if is_consumer_fn(&name.to_string()) {
        return true;
    }
    is_consumer_accessor_safe(tcx, def_id)
}

/// Accessor-method structural check (cross-crate-safe). Shared between
/// the codegen-skip hook, the `optimized_mir` override's consumer filter,
/// and `queries/symbol_name.rs`. Walks `opt_associated_item` to find the
/// impl's self type structurally (via `instantiate_identity` — inspection,
/// not instantiation) and compares its ADT name against
/// `is_consumer_type`. Safe from any phase — the `def_path_str` trap
/// (@DPSFDOZ) isn't reached.
pub(crate) fn is_consumer_accessor_safe<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    let Some(assoc_item) = tcx.opt_associated_item(def_id) else {
        return false;
    };
    let impl_def_id = assoc_item.container_id(tcx);
    // instantiate_identity: structural inspection only — we want the impl's
    // self type with its own params as placeholders so we can read the ADT
    // name. We are not producing a concrete type here.
    let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
    if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
        let struct_name = tcx.item_name(adt_def.did()).to_string();
        return is_consumer_type(&struct_name);
    }
    false
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

/// Call the consumer's collect_generic_rust_deps. Holds the mutable state mutex
/// for the entire call.
pub(crate) fn call_collect_generic_rust_deps<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.collect_generic_rust_deps;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, def_id)
}

/// Call the consumer's notify_concrete_entry_point. Holds the mutable state
/// mutex for the entire call.
pub(crate) fn call_notify_concrete_entry_point<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> String {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.notify_concrete_entry_point;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, instance)
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

pub(crate) fn default_optimized_mir() -> queries::optimized_mir::OptimizedMirFn {
    *DEFAULT_OPTIMIZED_MIR.get().expect("default optimized_mir not saved")
}

pub(crate) fn default_collect_and_partition() -> queries::partition::CollectAndPartitionFn {
    *DEFAULT_COLLECT_AND_PARTITION
        .get()
        .expect("default collect_and_partition_mono_items not saved")
}

pub(crate) fn default_upstream_monomorphizations_for()
    -> queries::upstream_monomorphization::UpstreamMonomorphizationsForFn
{
    *DEFAULT_UPSTREAM_MONOMORPHIZATIONS_FOR
        .get()
        .expect("default upstream_monomorphizations_for not saved")
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

fn trampoline_monomorphize_type<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_type(state, name, tcx, ty)
}

fn trampoline_collect_generic_rust_deps<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    data.downcast_ref::<C>().unwrap().collect_generic_rust_deps(state, name, tcx, def_id)
}

fn trampoline_notify_concrete_entry_point<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> String {
    data.downcast_ref::<C>().unwrap().notify_concrete_entry_point(state, name, tcx, instance)
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
        },
        stateful_vtable: StatefulVtable {
            monomorphize_type: trampoline_monomorphize_type::<C>,
            collect_generic_rust_deps: trampoline_collect_generic_rust_deps::<C>,
            notify_concrete_entry_point: trampoline_notify_concrete_entry_point::<C>,
            after_rust_analysis: trampoline_after_rust_analysis::<C>,
            generate_and_compile: trampoline_generate_and_compile::<C>,
        },
    });
    let _ = MUTABLE_STATE.set(std::sync::Mutex::new(FacadeMutableState {
        consumer_state,
        lang_obj_path: None,
    }));
    // Stage 4c retired `VISIBILITY_OVERRIDE_HOOK`: the partitioner override
    // in `queries::partition` now forces `(External, Default)` directly on
    // `__lang_stubs` items' `MonoItemData`. rustc_codegen_llvm reads the
    // linkage straight from the CGU struct at emission time and never
    // re-derives it, so the plugin-applied override survives to the final
    // `.o` — no fork hook needed.
    //
    // Stage 4b retired `CODEGEN_SKIP_HOOK`: the partitioner override in
    // `queries::partition` removes consumer items from rustc's CGU slice
    // before codegen dispatch ever sees them, so the hook is unreachable.
    // Fork patch 3 is deleted alongside this commit.
}

/// Save the original query providers. Phase 2 of globals init.
pub(crate) fn install_query_defaults(
    layout_of: queries::layout::LayoutOfFn,
    mir_shims: queries::drop_glue::MirShimsFn,
    symbol_name: queries::symbol_name::SymbolNameFn,
    optimized_mir: queries::optimized_mir::OptimizedMirFn,
    collect_and_partition: queries::partition::CollectAndPartitionFn,
    upstream_monomorphizations_for:
        queries::upstream_monomorphization::UpstreamMonomorphizationsForFn,
) {
    let _ = DEFAULT_LAYOUT_OF.set(layout_of);
    let _ = DEFAULT_MIR_SHIMS.set(mir_shims);
    let _ = DEFAULT_SYMBOL_NAME.set(symbol_name);
    let _ = DEFAULT_OPTIMIZED_MIR.set(optimized_mir);
    let _ = DEFAULT_COLLECT_AND_PARTITION.set(collect_and_partition);
    let _ = DEFAULT_UPSTREAM_MONOMORPHIZATIONS_FOR.set(upstream_monomorphizations_for);
}
