//! rustc-lang-facade: a library for integrating custom languages with rustc.
//!
//! Consumers implement the `LangCallbacks` trait and call `run_compiler()`.
//! The library handles query overrides, `CodegenBackend` wrapping, and the
//! rustc driver lifecycle. Stub-crate generation is the consumer's
//! responsibility — under the two-crate architecture (stage 5b/5c.4) the
//! `__lang_stubs` rlib is produced on disk by the consumer's build step
//! and compiled by cargo as ordinary Rust, so the facade has no stub-
//! injection surface.

#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_codegen_llvm;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hashes;
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
pub mod mir_helpers;
pub mod queries;

mod cgu_stash;
pub use cgu_stash::upstream_cgus;
pub(crate) use cgu_stash::{clear_upstream_cgus, stash_upstream_cgus};

use std::path::PathBuf;

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
/// The library identifies consumer items by the crate they live in — every
/// stub item is in the `__lang_stubs` rlib — and by the `is_consumer_type`
/// / `is_consumer_fn` predicates the consumer supplies on `LangPredicates`.
/// Under the two-crate architecture (stage 5b/5c.4) `__lang_stubs` is a
/// real on-disk rlib produced by the consumer's build step; the prior
/// `generate_stubs` FileLoader-injected shape is retired.
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

    // Stage 4c retired `visibility_override`: the partitioner override in
    // `queries::partition` now forces `(External, Default)` on
    // `__lang_stubs` items directly. No trait method needed.
    //
    // Stage 5c.4 retired `generate_stubs`: under the two-crate architecture
    // the stub rlib's `src/lib.rs` is written by the consumer's build-mode
    // path (e.g. `toylangc build`'s `write_stub_crate`), not injected into
    // rustc at compile time. The facade no longer needs to know how to
    // generate stubs.
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
    ///
    /// Stateless: no `&mut dyn Any state` param. Called from
    /// `lang_layout_of`, which can re-enter during
    /// `generate_and_compile` (whose trampoline holds MUTABLE_STATE).
    /// Under rustc incremental cache + warm rebuild, `layout_of` queries
    /// that fired cold during the mono walk get skipped on cache hit and
    /// fire later when `codegen_extern_wrapper` calls
    /// `coerced_return_type_for_instance → fn_abi_of_instance → layout_of`
    /// — now inside the outer mutex. A stateful callback would re-lock
    /// MUTABLE_STATE from `call_monomorphize_type` and deadlock. The
    /// stateless signature lets the facade dispatcher skip the lock.
    ///
    /// Correspondingly, no per-call logging can happen here; the former
    /// `CallbackLog::MonomorphizeType` entry is retired (unused by any
    /// test or diagnostic).
    fn monomorphize_type<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>;

    /// Called from the `per_instance_mir` query override for each concrete
    /// consumer Instance. Returns the Rust items this consumer Instance
    /// transitively depends on, as `(DefId, GenericArgsRef)` pairs.
    ///
    /// **Approach A contract (rust-interop-architecture.md §3.1, §19.1).** The
    /// caller passes a fully concrete `Instance<'tcx>` — `instance.args` are
    /// already substituted Sky-side. The returned `GenericArgsRef`s for each
    /// dep must likewise be concrete in Sky's universe; Param-bearing args
    /// returned here trip a `debug_assert` in the facade's `build_dependency_body`.
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
        instance: ty::Instance<'tcx>,
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

    /// Called once per upstream Sky-marked rlib loaded into the local
    /// compile, BEFORE `after_rust_analysis`. The facade discovers the
    /// rlib by walking `tcx.crates(())` and checking each crate root for
    /// the `__lang_stubs` marker (Phase 3 E.1 will replace the hardcoded
    /// crate-name check with `__SKY_STUBS_MARKER` per the architecture
    /// doc §4.5 / §6.3), then locates the adjacent `.sky-meta` sidecar
    /// via `tcx.used_crate_source(c).rlib` with the extension swapped,
    /// reads the file, and invokes this callback with the raw bytes.
    ///
    /// The facade deliberately knows nothing about the consumer's payload
    /// shape — it just hands over the bytes. The consumer is responsible
    /// for deserialization (toylang routes through
    /// `crate::sidecar::deserialize_sidecar`) and for merging the loaded
    /// universe into its own state.
    ///
    /// Per the S.4 Workstream-S task (course-correct.md quarter-of-work
    /// plan): the loader lands here; downstream A.3 will consume the
    /// loaded registries to populate the codegen queue at the user-bin
    /// compile. S.4 itself does NOT change codegen behavior — toylang
    /// just stashes the registry for later workstreams.
    fn on_sky_lib_loaded<'tcx>(
        &self,
        state: &mut dyn Any,
        tcx: TyCtxt<'tcx>,
        crate_name: &str,
        sidecar_bytes: &[u8],
    );

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
    // `monomorphize_type` takes no state (unlike the other entries in
    // this vtable). Dispatch via `call_monomorphize_type` skips the
    // mutex — see trait method's doc comment for the re-entrancy
    // rationale.
    monomorphize_type: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx>,

    collect_generic_rust_deps: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        &str,
        TyCtxt<'tcx>,
        ty::Instance<'tcx>,
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

    on_sky_lib_loaded: for<'tcx> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        TyCtxt<'tcx>,
        &str,
        &[u8],
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
// Mutable state (consumer_state) is behind its own Mutex.
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

/// Mutable state: consumer-owned state.
pub(crate) struct FacadeMutableState {
    consumer_state: Box<dyn Any + Send + Sync>,
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
// No DEFAULT_PER_INSTANCE_MIR: the upstream default returns None unconditionally
// (see comment near `default_collect_and_partition`).
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

/// Check if a DefId is from a Sky stub rlib (i.e., its containing crate
/// exposes `__SKY_STUBS_MARKER` at the crate root).
///
/// **Marker-based detection** (Phase 3 E.1; architecture §4.5, §6.3, §6.5):
/// rather than matching the crate name against the hardcoded literal
/// `"__lang_stubs"`, the predicate walks the crate root's children for
/// a `pub const __SKY_STUBS_MARKER: () = ();` declaration. This decouples
/// "is this a Sky stub rlib?" from the cargo package name, which is the
/// gating change for the multi-Sky-library shape Phase 3 builds toward
/// (each Sky library publishes its own stub rlib named after the library,
/// not a shared `__lang_stubs` crate).
///
/// Local vs cross-crate: during the rlib compile, the stub crate IS the
/// LOCAL crate, so we walk `module_children_local(CRATE_DEF_ID)`. During
/// the user-bin compile, the stub crate is loaded as an extern rlib, so
/// we walk `module_children(crate_root_def_id)`. Both yield the same
/// answer for the same logical crate; the local/extern split is just
/// rustc-internal API plumbing.
///
/// Per-`CrateNum` cached because the predicate fires in hot paths
/// (every consumer-item filter call, every accessor lookup). The cache
/// is per rustc process; cargo spawns one rustc subprocess per crate, so
/// the cache is effectively per-invocation.
pub fn is_from_lang_stubs(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let cnum = def_id.krate;
    let cache = SKY_STUBS_CRATES.get_or_init(|| {
        std::sync::Mutex::new(rustc_data_structures::fx::FxHashMap::default())
    });
    if let Some(&cached) = cache.lock().unwrap().get(&cnum) {
        return cached;
    }
    let result = crate_has_sky_marker(tcx, cnum);
    cache.lock().unwrap().insert(cnum, result);
    result
}

/// Per-`CrateNum` cache for `is_from_lang_stubs`. See its doc.
static SKY_STUBS_CRATES: OnceLock<
    std::sync::Mutex<rustc_data_structures::fx::FxHashMap<rustc_span::def_id::CrateNum, bool>>,
> = OnceLock::new();

/// The `__SKY_STUBS_MARKER` walk that backs `is_from_lang_stubs`.
///
/// We require the marker to be *defined* in the crate being checked, not
/// merely re-exported into it. Toylang's user-bin emits
/// `use __lang_stubs::*;` at the crate root for ergonomics; that glob
/// brings `__SKY_STUBS_MARKER` into the user-bin's own `module_children`
/// listing as a re-export. Without the parentage check the user-bin would
/// look like a stub rlib to the predicate, and the CGU filter would
/// remove `fn main` from codegen, producing a missing-`_main` link error.
///
/// The check: among children named `__SKY_STUBS_MARKER`, accept only ones
/// whose `Res::Def(_, def_id)` lives in the crate we're walking. The
/// re-export's DefId points back at the stub rlib's marker (different
/// crate), so it's correctly rejected.
fn crate_has_sky_marker(tcx: TyCtxt<'_>, cnum: rustc_span::def_id::CrateNum) -> bool {
    use rustc_hir::def::Res;
    let marker = rustc_span::Symbol::intern("__SKY_STUBS_MARKER");
    if cnum == rustc_hir::def_id::LOCAL_CRATE {
        tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID)
            .iter()
            .any(|c| {
                c.ident.name == marker
                    && matches!(c.res.expect_non_local::<rustc_hir::def_id::DefId>(), Res::Def(_, def_id) if def_id.krate == cnum)
            })
    } else {
        tcx.module_children(cnum.as_def_id())
            .iter()
            .any(|c| {
                c.ident.name == marker
                    && matches!(c.res, Res::Def(_, def_id) if def_id.krate == cnum)
            })
    }
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
/// synthesize a dep-discovery body for the item in the `per_instance_mir`
/// override (which drives rustc's monomorphization collector) and (b)
/// to remove the item from the CGU slice in the partitioner override
/// so rustc's codegen backend never sees it. Items inside
/// `__lang_stubs` that are NOT consumer fns — notably the Phase-6
/// `#[inline(never)]` wrappers like `__toylang_option_unwrap` — fall
/// through to rustc's default codegen; they are real Rust functions
/// whose symbol must be callable at link time.
pub fn is_consumer_codegen_target<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    if !is_from_lang_stubs(tcx, def_id) {
        return false;
    }
    let Some(name) = tcx.opt_item_name(def_id) else {
        return false;
    };
    if is_consumer_fn(&name.to_string()) {
        return true;
    }
    if is_consumer_accessor_safe(tcx, def_id) {
        return true;
    }
    // Phase 2 C.6 — trait-impl methods on consumer types are also
    // consumer-owned codegen targets.
    is_consumer_trait_impl_method(tcx, def_id).is_some()
}

/// Accessor-method structural check (cross-crate-safe). Shared between
/// the partitioner-override consumer filter, the `per_instance_mir`
/// override's consumer filter, and `queries/symbol_name.rs`. Walks
/// `opt_associated_item` to find the impl's self type structurally
/// (via `instantiate_identity` — inspection, not instantiation) and
/// compares its ADT name against `is_consumer_type`. Safe from any
/// phase — the `def_path_str` trap (@DPSFDOZ) isn't reached.
///
/// Phase 2 C.6: excludes trait-impl methods (where `impl_trait_ref` is
/// Some). Those route through `is_consumer_trait_impl_method` instead and
/// get a distinct mangled symbol (`__toylang_impl__<Self>__<Trait>__<m>`)
/// so that body codegen can find them under a different key than
/// inherent-impl accessors.
pub(crate) fn is_consumer_accessor_safe<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    let Some(assoc_item) = tcx.opt_associated_item(def_id) else {
        return false;
    };
    let impl_def_id = assoc_item.container_id(tcx);
    // Phase 2 C.6 — discriminate inherent from trait impls. Trait impls go
    // through is_consumer_trait_impl_method.
    if tcx.impl_opt_trait_ref(impl_def_id).is_some() {
        return false;
    }
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

/// Phase 2 C.6 — discriminate a trait-impl method on a consumer type.
/// Returns `Some((self_type_name, trait_short_name, method_name))` when
/// `def_id` is a method inside an `impl <RustTrait> for <ConsumerType>`
/// block; None otherwise.
///
/// Used by `queries/symbol_name.rs` to build a trait-impl-specific callback
/// name (so the consumer's `notify_concrete_entry_point_inner` can mangle
/// to `__toylang_impl__<Self>__<Trait>__<m>` instead of the accessor
/// pattern). Also used by the consumer-codegen-target filter.
pub fn is_consumer_trait_impl_method<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
) -> Option<(String, String, String)> {
    let assoc_item = tcx.opt_associated_item(def_id)?;
    let impl_def_id = assoc_item.container_id(tcx);
    let trait_ref = tcx.impl_opt_trait_ref(impl_def_id)?;
    // instantiate_identity: structural inspection only — we want the impl's
    // self type with its own params as placeholders so we can read the ADT
    // name. Not producing a concrete type here.
    let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
    let ty::TyKind::Adt(adt_def, _) = self_ty.kind() else { return None; };
    let struct_name = tcx.item_name(adt_def.did()).to_string();
    if !is_consumer_type(&struct_name) {
        return None;
    }
    let trait_def_id = trait_ref.skip_binder().def_id;
    let trait_name = tcx.item_name(trait_def_id).to_string();
    let method_name = tcx.item_name(def_id).to_string();
    Some((struct_name, trait_name, method_name))
}

/// Check if a function name belongs to the consumer's language.
/// Per @GCMLZ, reads from CONFIG (no lock) — safe during generate_and_compile.
pub(crate) fn is_consumer_fn(name: &str) -> bool {
    let c = CONFIG.get().expect("config not installed");
    (c.predicate_vtable.is_consumer_fn)(&*c.callbacks, name)
}

/// Call the consumer's monomorphize_type. Lock-free — reads only from
/// CONFIG (immutable `OnceLock`). Safe to call during
/// `generate_and_compile` (whose trampoline holds MUTABLE_STATE) without
/// risking re-entrant deadlock. The callback itself is stateless by
/// contract (see trait docs).
///
/// `pub` so consumers can invoke it from their generate-phase code to
/// log layout values directly (useful for tests that need a cache-
/// independent log source — `lang_layout_of`'s eprintln fires via
/// the query provider, which incremental can skip on cache hit).
pub fn call_monomorphize_type<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.monomorphize_type;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    // Safety: callbacks is immutable (from CONFIG, no lock needed).
    (func)(unsafe { &*callbacks_ptr }, name, tcx, ty)
}

/// Call the consumer's collect_generic_rust_deps. Holds the mutable state mutex
/// for the entire call.
///
/// Approach A: the caller (per_instance_mir override) passes a concrete
/// `Instance<'tcx>`; the consumer returns concrete deps after Sky-side
/// substitution.
pub(crate) fn call_collect_generic_rust_deps<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.collect_generic_rust_deps;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, instance)
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
    // @GCMLZ + Phase 3 E.6: if this call is reentering from inside
    // `trampoline_generate_and_compile` (i.e. via a query provider like
    // `lang_symbol_name`), MUTABLE_STATE is already held by this same
    // thread. Re-locking on std's Mutex would deadlock. Use the
    // trampoline-stashed state pointer instead. The pointer is only
    // dereferenced within the trampoline's stack frame so aliasing is
    // not a soundness issue — Rust query execution is single-threaded
    // per session.
    if let Some(state_ptr) = try_get_generate_and_compile_state() {
        return (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, instance);
    }
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

/// Call the consumer's on_sky_lib_loaded. Holds the mutable state mutex for
/// the entire call. Per S.4 (course-correct.md quarter-of-work plan): the
/// facade hands the consumer the raw sidecar bytes; the consumer deserializes.
pub(crate) fn call_on_sky_lib_loaded<'tcx>(
    tcx: TyCtxt<'tcx>,
    crate_name: &str,
    sidecar_bytes: &[u8],
) {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.on_sky_lib_loaded;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut *g.consumer_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx, crate_name, sidecar_bytes)
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

// No `default_per_instance_mir` accessor: vanilla rustc's default provider
// (installed by `rustc_mir_transform::provide` per the fork's patch 3) always
// returns None. There's no upstream behavior to delegate to. The
// `per_instance.rs` override returns None directly for non-consumer items;
// rustc's collector then queries `instance_mir` via the fork's patch 2.

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
    name: &str,
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
) -> MonomorphizeTypeResult<'tcx> {
    data.downcast_ref::<C>().unwrap().monomorphize_type(name, tcx, ty)
}

fn trampoline_collect_generic_rust_deps<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    data.downcast_ref::<C>().unwrap().collect_generic_rust_deps(state, name, tcx, instance)
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

fn trampoline_on_sky_lib_loaded<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
    crate_name: &str,
    sidecar_bytes: &[u8],
) {
    data.downcast_ref::<C>().unwrap().on_sky_lib_loaded(state, tcx, crate_name, sidecar_bytes)
}

fn trampoline_generate_and_compile<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
) -> Option<(PathBuf, Vec<String>)> {
    // Phase 3 E.6 / @GCMLZ: expose the held state pointer via a thread-local
    // so that query providers (e.g. `lang_symbol_name`) which re-enter the
    // facade during `generate_and_compile` can reach the consumer state
    // WITHOUT re-locking MUTABLE_STATE.
    //
    // The deadlock this prevents: at the user-bin compile, when toylang's
    // codegen calls `tcx.symbol_name(instance)` for an upstream consumer
    // function (Case 6, an item from a Sky library), rustc fires
    // `lang_symbol_name`, which calls `call_notify_concrete_entry_point`,
    // which (without this bypass) tries to lock MUTABLE_STATE — already
    // held by THIS thread from the outer `call_generate_and_compile`. std's
    // Mutex isn't reentrant, so the same thread blocks waiting on itself.
    // (Single-project tests don't hit this because their symbol_name
    // queries get cached by rustc during the earlier mono walk, before
    // generate_and_compile starts.)
    let state_ptr: *mut (dyn Any + Send + Sync) = state;
    set_generate_and_compile_state(state_ptr);
    let result = data.downcast_ref::<C>().unwrap().generate_and_compile(state, tcx);
    clear_generate_and_compile_state();
    result
}

/// Width-erased fat-pointer storage for the trampoline's held state. The
/// thread-local stores both halves of `*mut dyn Trait`; the read site
/// transmutes back. The pointer is only valid while the trampoline's
/// stack frame is alive.
#[derive(Copy, Clone)]
struct GcState {
    data: *mut (),
    vtable: *mut (),
}

std::thread_local! {
    static GENERATE_AND_COMPILE_FAT_STATE: std::cell::Cell<Option<GcState>> = const {
        std::cell::Cell::new(None)
    };
}

fn set_generate_and_compile_state(s: *mut (dyn Any + Send + Sync)) {
    // SAFETY: `*mut dyn Trait` is `(*mut (), *mut ())` per Rust's current
    // ABI. We split via transmute, recombine on read. Pointer is only
    // dereferenced while the trampoline's stack frame is alive.
    let raw: [*mut (); 2] = unsafe { std::mem::transmute(s) };
    GENERATE_AND_COMPILE_FAT_STATE.with(|c| {
        c.set(Some(GcState { data: raw[0], vtable: raw[1] }))
    });
}

fn clear_generate_and_compile_state() {
    GENERATE_AND_COMPILE_FAT_STATE.with(|c| c.set(None));
}

/// If we're inside `generate_and_compile`'s trampoline (which already holds
/// MUTABLE_STATE), return the held state pointer so re-entrant callbacks
/// can mutate state without re-locking.
fn try_get_generate_and_compile_state() -> Option<*mut (dyn Any + Send + Sync)> {
    GENERATE_AND_COMPILE_FAT_STATE.with(|c| {
        c.get().map(|gc| {
            let raw: [*mut (); 2] = [gc.data, gc.vtable];
            // SAFETY: reverses the split in `set_generate_and_compile_state`.
            unsafe { std::mem::transmute::<[*mut (); 2], *mut (dyn Any + Send + Sync)>(raw) }
        })
    })
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
            on_sky_lib_loaded: trampoline_on_sky_lib_loaded::<C>,
            generate_and_compile: trampoline_generate_and_compile::<C>,
        },
    });
    let _ = MUTABLE_STATE.set(std::sync::Mutex::new(FacadeMutableState {
        consumer_state,
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
///
/// `per_instance_mir` is intentionally absent — its upstream default returns
/// None unconditionally (the fork's patch 3), so the override file returns
/// None directly instead of calling through a saved default.
pub(crate) fn install_query_defaults(
    layout_of: queries::layout::LayoutOfFn,
    mir_shims: queries::drop_glue::MirShimsFn,
    symbol_name: queries::symbol_name::SymbolNameFn,
    collect_and_partition: queries::partition::CollectAndPartitionFn,
    upstream_monomorphizations_for:
        queries::upstream_monomorphization::UpstreamMonomorphizationsForFn,
) {
    let _ = DEFAULT_LAYOUT_OF.set(layout_of);
    let _ = DEFAULT_MIR_SHIMS.set(mir_shims);
    let _ = DEFAULT_SYMBOL_NAME.set(symbol_name);
    let _ = DEFAULT_COLLECT_AND_PARTITION.set(collect_and_partition);
    let _ = DEFAULT_UPSTREAM_MONOMORPHIZATIONS_FOR.set(upstream_monomorphizations_for);
}
