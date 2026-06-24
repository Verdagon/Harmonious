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
pub mod extra_modules_hook;
pub mod mir_helpers;
pub mod queries;

// Tier 3 #3 Phase 3: `cgu_stash` retired. The lifetime-erased CGU stash
// (87 lines of `'static`/`*const CodegenUnit`/unsafe pointer cast) is
// gone. The consumer's `codegen_crate` re-calls
// `default_collect_and_partition()` to get a sound `'tcx`-bound CGU
// slice for its Case 1b generic-from-Rust discovery walk; accessors no
// longer need CGU discovery at all (Phase 1c synthesises them at
// populate time via `synthesize_accessor_pairs`). Architecture
// risks.md §B5 closed.
//
// Option 4 (arch §F.14, 2026-06-20): the `collect_and_partition_mono_items`
// override that used to filter consumer items out of the CGU list is also
// retired. The saved upstream provider is still preserved so
// `default_collect_and_partition()` keeps working unchanged for callers;
// it just no longer differs from `tcx.collect_and_partition_mono_items(())`
// because no override is installed.

use rustc_codegen_llvm::ModuleLlvm;
use rustc_codegen_ssa::traits::ExtraModuleAllocator;
use rustc_middle::ty::{self, GenericArgsRef, Ty, TyCtxt};
use rustc_span::def_id::DefId;

/// Borrowed raw LLVM resources owned by rustc, surfaced to consumers in an
/// LLVM-API-agnostic shape. Valid only for the duration of the surrounding
/// [`LlvmModuleFactory::fill_module`] closure. Consumers MUST NOT call
/// `LLVMContextDispose` / `LLVMDisposeModule` on these pointers — rustc owns
/// the lifecycle and disposes the resources through its own pipeline after
/// `fill_extra_modules` returns.
///
/// The pointers are type-erased to `*mut c_void` so the facade does not take a
/// position on which LLVM API the consumer uses:
///
/// - **Inkwell**: cast `context` to `inkwell::llvm_sys::prelude::LLVMContextRef`
///   and pass to `Context::new`; same for `module` / `Module::new_borrowed`.
///   Wrap both in `ManuallyDrop` so Inkwell's Drop is suppressed.
/// - **`llvm-sys` direct**: cast to `LLVMContextRef` / `LLVMModuleRef` and
///   pass straight to any `llvm-sys` function. No wrapping needed.
/// - **C++ via FFI**: pass the raw pointers to an `extern "C"` function that
///   `reinterpret_cast`s them to `llvm::LLVMContext*` / `llvm::Module*`. This
///   is the canonical LLVM C-to-C++ conversion (`DEFINE_SIMPLE_CONVERSION_FUNCTIONS`),
///   so it is zero-cost and well-defined.
#[derive(Debug, Clone, Copy)]
pub struct BorrowedLlvmModule {
    /// `LLVMContextRef`. Cast / wrap / pass-through depending on which LLVM
    /// API the consumer uses (see [`BorrowedLlvmModule`] docs).
    pub context: *mut std::ffi::c_void,
    /// `LLVMModuleRef`. Same: cast / wrap / pass-through as needed.
    pub module: *mut std::ffi::c_void,
}

/// Mediates between rustc's `ExtraModuleAllocator<ModuleLlvm>` (facade-internal)
/// and consumer code (sees only [`BorrowedLlvmModule`]).
///
/// Consumers receive `&mut LlvmModuleFactory` in their
/// [`LangCallbacks::consumer_fill_modules`] impl and call
/// [`LlvmModuleFactory::fill_module`] once per Sky CGU to obtain borrowed
/// LLVM pointers and fill in IR.
pub struct LlvmModuleFactory<'a> {
    // Private. `ModuleLlvm` + `ExtraModuleAllocator` are facade-internal types
    // the consumer never names.
    inner: &'a mut (dyn ExtraModuleAllocator<ModuleLlvm> + 'a),
}

impl<'a> LlvmModuleFactory<'a> {
    /// Construct a factory around rustc's allocator. Used by the
    /// `extra_modules_hook` bridge to wrap the rustc-supplied allocator
    /// before handing it to the consumer. Not for consumer use.
    #[doc(hidden)]
    pub fn new(
        inner: &'a mut (dyn ExtraModuleAllocator<ModuleLlvm> + 'a),
    ) -> Self {
        Self { inner }
    }

    /// Allocate a fresh rustc-owned LLVM module under `name` and invoke
    /// `fill` with borrowed pointers to its `LLVMContext` and `LLVMModule`.
    /// Rustc retains ownership of the resources; the pointers are valid
    /// only for the duration of the closure call.
    ///
    /// May be called multiple times to contribute multiple CGUs; each call
    /// borrows the allocator independently so the closure's borrow ends
    /// before the next `fill_module` runs.
    pub fn fill_module<F: FnOnce(BorrowedLlvmModule)>(&mut self, name: &str, fill: F) {
        let m = self.inner.allocate(name);
        let handles = BorrowedLlvmModule {
            context: m.llcx_raw_mut(),
            module: m.llmod_raw(),
        };
        fill(handles);
    }
}

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
// Consumer callback trait.
//
// History: pre-Tier-3 the trait was split into `LangPredicates` (lock-free
// readers — `is_consumer_type`, `is_consumer_fn`) and `LangCallbacks`
// (stateful writers). Tier 3 #7 migrated the predicates to read from the
// facade-owned `SkyUniverse` directly; #7.4 retired the trait and its
// vtable. Tier 3 #9 retired `notify_concrete_entry_point` (replaced by the
// stateless `consumer_symbol_for_callback_name`). Tier 3 #12 closed the
// architectural deadlock concern — see @GCMLZ for the current locking
// story.
//
// What remains: this one `LangCallbacks` trait. The methods split into
// two families by signature:
//
//   - Stateless: `monomorphize_type`, `consumer_symbol_for_callback_name`.
//     No `&mut dyn Any state` parameter; called from query providers
//     (`lang_layout_of`, `lang_symbol_name`) that can fire concurrently
//     on rustc's rayon workers. Dispatched without locking `MUTABLE_STATE`.
//
//   - Stateful: `create_state`, `collect_generic_rust_deps`,
//     `after_rust_analysis`, `on_sky_lib_loaded`, `consumer_fill_modules`.
//     Take `&mut dyn Any state`. The dispatch helper locks `MUTABLE_STATE`
//     for the duration. Serialises concurrent fires of
//     `collect_generic_rust_deps` (which runs on mono-walk worker threads)
//     and orderly main-thread fires of the others.
//
// See `docs/architecture/rust-interop-guide.md` Part 2 for the family
// taxonomy and `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` for the
// current locking contract.
// ============================================================================

/// Stateful callbacks: each takes `&mut dyn Any state` (downcast to your
/// concrete state type). Bridge fns for these lock `MUTABLE_STATE` for the
/// duration of the call.
pub trait LangCallbacks: Send + Sync {
    /// Create the consumer's mutable state. Called once at startup.
    /// The facade stores this in its global and passes `&mut dyn Any` to every
    /// stateful callback. The consumer downcasts to its concrete state type.
    fn create_state(&self) -> Box<dyn Any + Send + Sync>;

    /// Monomorphize a consumer type for concrete type args.
    ///
    /// Stateless: no `&mut dyn Any state` param. Called from
    /// `lang_layout_of`, which can re-enter during
    /// `consumer_fill_modules` (whose helper holds `MUTABLE_STATE`).
    /// Under rustc incremental cache + warm rebuild, `layout_of` queries
    /// that fired cold during the mono walk get skipped on cache hit and
    /// fire later when `codegen_extern_wrapper` calls
    /// `coerced_return_type_for_instance → fn_abi_of_instance → layout_of`
    /// — now inside the outer mutex. A stateful callback would re-lock
    /// `MUTABLE_STATE` from `call_monomorphize_type` and deadlock. The
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
    /// **Tier 3 #9 (stateless).** Pre-#9 this method was named
    /// `notify_concrete_entry_point` and held `&mut dyn Any state`; it
    /// served as a discovery side-channel that stashed Instances into
    /// codegen state. Phase C (Session 11) migrated discovery to the
    /// `after_expansion` entry-point walk, and #9 retires the side
    /// effect. The method now returns a symbol name as a pure function
    /// of `(self, callback_name, tcx, instance)` — no consumer state
    /// mutation. This is what unlocks dropping the @GCMLZ thread-local
    /// fat-pointer bypass (Session 5): without re-entrant state
    /// mutation through `symbol_name`, the trampoline never re-locks.
    ///
    /// The `callback_name` is a routing prefix the facade chose based on
    /// the Instance's kind:
    /// - `__impl_method__<Self>__<Trait>__<m>` for trait-impl methods
    /// - `<Self>.<field>` for accessor methods
    /// - `<fn_name>` for free fns
    ///
    /// The consumer mangles to its own symbol scheme. Toylang's
    /// `consumer_symbol_for_callback_name` defers to its existing
    /// `notify_concrete_entry_point_inner` helper (minus the log push)
    /// which already had the mangling logic.
    fn consumer_symbol_for_callback_name<'tcx>(
        &self,
        callback_name: &str,
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

    /// Approach B (rustc-owns-lends, patch 4 rev 2): fill the consumer's
    /// function bodies into rustc-allocated LLVM modules. For each Sky CGU,
    /// the consumer calls `factory.fill_module(&name, |handles| { ... })`
    /// to obtain borrowed `LLVMContextRef` + `LLVMModuleRef` pointers (via
    /// [`BorrowedLlvmModule`]) and emit IR through whichever LLVM API the
    /// consumer prefers — Inkwell, `llvm-sys`, or C++ via FFI. Each filled
    /// module rides rustc's optimize → ThinLTO → emission pipeline as just
    /// another CGU.
    ///
    /// No bitcode serialization, no LLVM-context migration: rustc retains
    /// ownership of every allocated module throughout, and the consumer's
    /// IR-emission borrowed wrappers (suppressed-Drop / no-op-Drop) leave
    /// it untouched.
    ///
    /// Default no-ops. Override to participate in inline codegen.
    fn consumer_fill_modules<'tcx>(
        &self,
        _state: &mut dyn Any,
        _tcx: TyCtxt<'tcx>,
        _factory: &mut LlvmModuleFactory,
    ) {
        // Default: no modules contributed.
    }

    // `synthesize_upstream_monomorphizations` (A.2) retired 2026-06-21.
    // Under §5.5 Step 3, cascade-discovered trait-impl method bodies
    // emit at the cascade-firing crate (stub_rlib) inline; rustc's
    // natural map suffices for cross-crate disambig and the augmented
    // map this callback synthesized is no longer needed.
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

// Tier 3 #7.4: `PredicateVtable` retired. Predicates (`is_consumer_type`,
// `is_consumer_fn`) now read from the facade-owned `SkyUniverse` (see the
// SKY_UNIVERSE block above). The consumer no longer supplies a predicate
// vtable; #7.2's sidecar-load populate writes the universe instead.

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

    // Tier 3 #9: stateless successor to `notify_concrete_entry_point`. No
    // `&mut state` parameter; dispatched without locking `MUTABLE_STATE`,
    // matching the `monomorphize_type` pattern. The thread-local fat-pointer
    // bypass (Session 5) becomes obsolete because no consumer state is
    // mutated via the `symbol_name` re-entrance path.
    consumer_symbol_for_callback_name: for<'tcx> fn(
        &(dyn Any + Send + Sync),
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

    consumer_fill_modules: for<'tcx, 'a, 'b> fn(
        &(dyn Any + Send + Sync),
        &mut (dyn Any + Send + Sync),
        TyCtxt<'tcx>,
        &'b mut LlvmModuleFactory<'a>,
    ),
    // `synthesize_upstream_monomorphizations` slot retired 2026-06-21
    // (A.2 retirement under §5.5 Step 3).
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
// This separation prevents deadlocks: `consumer_fill_modules` (Phase 4.5's
// inline-codegen path, replacing the retired `generate_and_compile`) holds
// the mutable state mutex while consumer codegen runs, but query providers
// triggered during codegen (e.g. `symbol_name`, `layout_of`) only need
// immutable config + lock-free `SkyUniverse` reads — they never touch the
// state mutex. Tier 3 #12 closed the @GCMLZ trap-fence: after #7
// (predicates → universe), #9 (symbol_name made stateless), and Path B's
// `consumer_fill_modules` (replacing `generate_and_compile`), every
// re-entrance path that could have deadlocked is removed by construction.
// See `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` for the historical
// catalogue and the current locking contract.
// ============================================================================

/// Immutable config: callbacks + the stateful vtable. Set once by
/// `install_callbacks`. Dispatched while holding `MUTABLE_STATE`. Tier 3
/// #7.4 retired the previous `predicate_vtable` field (predicates now
/// read from `SkyUniverse`); a future #12 retires `MUTABLE_STATE` itself.
pub(crate) struct FacadeConfig {
    callbacks: Box<dyn Any + Send + Sync>,
    stateful_vtable: StatefulVtable,
}

// Safety: callbacks is Box<dyn Any + Send + Sync>, vtable contains plain fn pointers.
unsafe impl Send for FacadeConfig {}
unsafe impl Sync for FacadeConfig {}

// Tier 3 #12 close-out: `FacadeMutableState` collapsed into a plain
// `Box<dyn Any + Send + Sync>`. The struct was a single-field wrapper
// (`consumer_state`) and the unsafe Send + Sync impls were redundant
// because `Box<dyn Any + Send + Sync>` already provides them. Inlining
// the type erases the artifact without changing the mutex's semantics.

/// Immutable config (callbacks + vtable). Set once, never changes.
static CONFIG: OnceLock<FacadeConfig> = OnceLock::new();

/// Type alias for the saved upstream `collect_and_partition_mono_items`
/// provider. Used by the `default_collect_and_partition()` accessor. The
/// override was retired in Option 4 (arch §F.14, 2026-06-20); the saved
/// default is preserved so consumers (notably `toylangc::llvm_gen` and
/// `toylangc::toylang::callbacks_impl::collect_consumer_trait_impl_instances`)
/// can re-call the upstream provider directly when they need the CGU
/// slice for their own walks — `tcx.collect_and_partition_mono_items(())`
/// also works and returns the same result now that no override is installed,
/// but the accessor avoids any incremental-cache shape surprises.
pub type CollectAndPartitionFn = for<'tcx> fn(
    rustc_middle::ty::TyCtxt<'tcx>,
    (),
) -> rustc_middle::mir::mono::MonoItemPartitions<'tcx>;

/// Default query providers saved from rustc. Set once, never changes.
static DEFAULT_LAYOUT_OF: OnceLock<queries::layout::LayoutOfFn> = OnceLock::new();
static DEFAULT_MIR_SHIMS: OnceLock<queries::drop_glue::MirShimsFn> = OnceLock::new();
static DEFAULT_SYMBOL_NAME: OnceLock<queries::symbol_name::SymbolNameFn> = OnceLock::new();
// No DEFAULT_PER_INSTANCE_MIR: the upstream default returns None unconditionally
// (see comment near `default_collect_and_partition`).
static DEFAULT_COLLECT_AND_PARTITION: OnceLock<CollectAndPartitionFn> = OnceLock::new();
// DEFAULT_UPSTREAM_MONOMORPHIZATIONS{_FOR} OnceLocks retired 2026-06-21
// (A.2 retirement under §5.5 Step 3 — the lang_upstream_monomorphizations{_for}
// overrides are no longer installed, so saving the defaults is moot).
static DEFAULT_CROSS_CRATE_INLINABLE:
    OnceLock<queries::cross_crate_inlinable::CrossCrateInlinableFn> = OnceLock::new();
static DEFAULT_EXTERN_CROSS_CRATE_INLINABLE:
    OnceLock<queries::cross_crate_inlinable::ExternCrossCrateInlinableFn> = OnceLock::new();
// DEFAULT_CODEGEN_FN_ATTRS / DEFAULT_EXTERN_CODEGEN_FN_ATTRS retired
// 2026-06-22 (Option 4 retirement — see arch §F.14.1 design history).

/// Mutable state. Locked only by callbacks that need &mut consumer_state.
static MUTABLE_STATE: OnceLock<std::sync::Mutex<Box<dyn Any + Send + Sync>>> = OnceLock::new();

// ============================================================================
// SkyUniverse — content-addressed Sky-item registry owned by the facade.
//
// Per architecture §7.1–7.5, §8, §9.4, §10.8, §10.9 and course-correct #7:
// the facade owns a content-addressed registry of every Sky item visible to
// the current rustc invocation (typeids of Sky types, names of Sky fns +
// types). It is populated at sidecar-load time from each upstream Sky-marked
// rlib's `.sky-meta` payload, plus from the LOCAL crate's just-built
// registry once Sky's frontend has typechecked it.
//
// Predicates like "is `Widget` a Sky type?" become O(1) lock-free reads of
// this structure — no vtable hop, no consumer callback. This is the
// foundation Tier 3 #9 (retire symbol_name side-effect channel), #3 (retire
// cgu_stash), and #12 (close out the @GCMLZ deadlock concern) all build on.
//
// Sky-locked design has read-mostly access: sidecar loads + local-registry
// build are the only writes, both serialized by the rustc-invocation
// lifecycle (each happens at a known pipeline phase before queries fire).
// `RwLock` gives us lock-free reads; the lock is uncontended during
// `consumer_fill_modules` (consistent with @GCMLZ's "no consumer-callback
// lock during codegen" rule).
//
// #7.1: structure + accessors only. Call sites still went through the
// vtable predicates. #7.2 populates; #7.3 migrates call sites; #7.4
// retires the vtable. Tier 3 #8 added `struct_infos` for type-erased
// consumer struct metadata.
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Content-addressed registry of every Sky item visible to this rustc
/// invocation. See module-level comment above `SKY_UNIVERSE`.
#[derive(Default, Debug)]
pub struct SkyUniverse {
    /// Typeids (BLAKE3-truncated-to-u64 per `toylangc/src/typeid.rs`).
    /// Identifies Sky types by content, not by source name. Future-proof for
    /// Sky's comptime-derived types whose source name may collide with
    /// rustc-known types.
    pub typeids: HashSet<u64>,
    /// Source-level names of Sky fns (for `is_consumer_fn`-style lookups
    /// against rustc DefId names).
    pub fn_names: HashSet<String>,
    /// Source-level names of Sky types (for `is_consumer_type`-style lookups).
    pub type_names: HashSet<String>,
    /// Tier 3 #8: consumer-defined struct metadata, type-erased so the
    /// facade stays consumer-agnostic. The consumer (toylang or future Sky)
    /// inserts an `Arc<ToyStruct>` (or its `ResolvedType`-bearing analog) via
    /// `insert_struct_info`; `monomorphize_type` reads via
    /// `get_struct_info` + downcast. Retires the prior consumer-side
    /// `upstream_structs: HashMap<String, ToyStruct>` mutex-mirror that
    /// duplicated this surface to handle cross-Sky-crate layouts (Case 6
    /// sharpening).
    pub struct_infos: HashMap<String, std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    // `discoveries` field retired 2026-06-21 along with A.2 / the
    // SkyUniverse-mediated synthesis path.
}

impl SkyUniverse {
    pub fn contains_type(&self, name: &str) -> bool {
        self.type_names.contains(name)
    }
    pub fn contains_fn(&self, name: &str) -> bool {
        self.fn_names.contains(name)
    }
    pub fn contains_typeid(&self, typeid: u64) -> bool {
        self.typeids.contains(&typeid)
    }
    /// Tier 3 #8: register consumer-side metadata for a Sky struct. Stored
    /// type-erased so the facade has no compile-time dependency on the
    /// consumer's typed-AST type. The consumer downcasts on read.
    pub fn insert_struct_info(
        &mut self,
        name: String,
        info: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) {
        self.struct_infos.insert(name, info);
    }
    /// Tier 3 #8: retrieve the consumer-side struct metadata previously
    /// inserted via `insert_struct_info`. Returns a clone of the `Arc` so
    /// the read can outlive the `SkyUniverse` read guard.
    pub fn get_struct_info(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.struct_infos.get(name).cloned()
    }
    // `push_discovery` + `discoveries_clone` retired 2026-06-21 with A.2.
}

static SKY_UNIVERSE: OnceLock<RwLock<SkyUniverse>> = OnceLock::new();

/// Read-only access to the Sky universe. Lock-free in the common case
/// (RwLock read guard). Initialises an empty universe on first use so
/// callers can read safely before populate has happened (returns "not
/// present" for everything, which is the right answer).
pub fn sky_universe() -> std::sync::RwLockReadGuard<'static, SkyUniverse> {
    SKY_UNIVERSE
        .get_or_init(|| RwLock::new(SkyUniverse::default()))
        .read()
        .expect("SkyUniverse RwLock poisoned")
}

/// Mutating access for populate paths only (sidecar load, local registry
/// build). The caller's closure receives an exclusive lock.
pub fn with_sky_universe_mut<R>(f: impl FnOnce(&mut SkyUniverse) -> R) -> R {
    let lock = SKY_UNIVERSE.get_or_init(|| RwLock::new(SkyUniverse::default()));
    let mut guard = lock.write().expect("SkyUniverse RwLock poisoned");
    f(&mut guard)
}

/// Check if a type name belongs to the consumer's language.
///
/// **Tier 3 #7.3 (migrated to SkyUniverse).** Reads from the facade-owned
/// universe via `sky_universe()` — lock-free in the common case (read
/// guard on `SKY_UNIVERSE`'s RwLock). The previous predicate-vtable hop
/// is retired; #7.4 will delete the vtable slot itself.
///
/// Per @GCMLZ, this is still safe during `generate_and_compile`: the
/// universe is written only at sidecar-load + local-registry populate
/// (both before codegen starts); reads during codegen never block.
pub(crate) fn is_consumer_type(name: &str) -> bool {
    sky_universe().contains_type(name)
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

/// Is Sky machinery active in this compile?
///
/// Returns `true` iff *any* loaded crate (LOCAL_CRATE or any upstream
/// rlib pulled in via `extern crate`) carries `__SKY_STUBS_MARKER`.
/// Mirror of the historical `consumer_lang_active(())` rustc-fork
/// query (patch 5), but implemented as an inline marker-walk so the
/// fork patch can retire. Three compile shapes:
///
/// - **Stub rlib compile.** LOCAL_CRATE itself is a Sky stub rlib with
///   the marker → returns `true`.
/// - **User-bin compile.** LOCAL_CRATE is a plain Rust bin with no
///   marker, but it depends on the Sky stub rlib (which has the
///   marker). The crate-walk finds the marker upstream → returns `true`.
/// - **Pure-Rust crate compiled by this rustc binary.** No marker
///   anywhere → returns `false`. Byte-identical pass-through preserved.
///
/// Each individual marker-check is O(1) via `SKY_STUBS_CRATES`
/// thread-local cache, so the overall cost is O(crates) per call —
/// fast enough that no separate "is_sky_active" cache is needed today.
/// If profiling later shows it's a hot path, add a second OnceLock
/// caching the boolean result per-invocation.
pub fn is_sky_active(tcx: TyCtxt<'_>) -> bool {
    use rustc_hir::def_id::{CRATE_DEF_INDEX, DefId, LOCAL_CRATE};
    // Local crate first (covers stub rlib compiles).
    if is_from_lang_stubs(
        tcx,
        DefId { krate: LOCAL_CRATE, index: CRATE_DEF_INDEX },
    ) {
        return true;
    }
    // Walk upstream crates (covers user-bin and Sky-lib consumer compiles).
    tcx.crates(()).iter().any(|&cnum| {
        is_from_lang_stubs(
            tcx,
            DefId { krate: cnum, index: CRATE_DEF_INDEX },
        )
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
///
/// **Tier 3 #7.3 (migrated to SkyUniverse).** Reads from the facade-owned
/// universe via `sky_universe()` — lock-free in the common case.  Mirrors
/// `is_consumer_type` exactly. See its doc for the migration rationale.
pub(crate) fn is_consumer_fn(name: &str) -> bool {
    sky_universe().contains_fn(name)
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
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut **g;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, name, tcx, instance)
}

/// Call the consumer's `consumer_symbol_for_callback_name`. **Tier 3 #9
/// (stateless).** No `MUTABLE_STATE` lock; mirrors `call_monomorphize_type`.
/// The previous `call_notify_concrete_entry_point` held the mutex (with a
/// thread-local fat-pointer bypass for re-entrance via @GCMLZ) — both are
/// gone. The override is now a pure read.
pub(crate) fn call_consumer_symbol_for_callback_name<'tcx>(
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> String {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.consumer_symbol_for_callback_name;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    // Safety: callbacks is immutable (from CONFIG, no lock needed).
    (func)(unsafe { &*callbacks_ptr }, name, tcx, instance)
}

/// Call the consumer's after_rust_analysis. Holds the mutable state mutex for the entire call.
pub(crate) fn call_after_rust_analysis<'tcx>(tcx: TyCtxt<'tcx>) {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.after_rust_analysis;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut **g;
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
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut **g;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx, crate_name, sidecar_bytes)
}

/// Call the consumer's consumer_fill_modules. Holds MUTABLE_STATE for the
/// duration; same re-entrance discipline as the retired
/// `call_consumer_fill_modules` it replaces. Called from the
/// `fill_extra_modules` hook (patch 4 rev 2 / Approach B).
pub(crate) fn call_consumer_fill_modules<'tcx>(
    tcx: TyCtxt<'tcx>,
    factory: &mut LlvmModuleFactory<'_>,
) {
    let c = CONFIG.get().expect("config not installed");
    let func = c.stateful_vtable.consumer_fill_modules;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    let mut g = MUTABLE_STATE.get().expect("state not installed").lock().unwrap();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut **g;
    (func)(
        unsafe { &*callbacks_ptr },
        unsafe { &mut *state_ptr },
        tcx,
        factory,
    )
}

// `call_synthesize_upstream_monomorphizations` retired 2026-06-21
// (A.2 retirement under §5.5 Step 3).

/// Read saved default query providers. Per @GCMLZ, no locking — stored in
/// OnceLock so they're safe to call during generate_and_compile.
pub(crate) fn default_layout_of() -> queries::layout::LayoutOfFn {
    *DEFAULT_LAYOUT_OF.get().expect("default layout_of not saved")
}

pub(crate) fn default_mir_shims() -> queries::drop_glue::MirShimsFn {
    *DEFAULT_MIR_SHIMS.get().expect("default mir_shims not saved")
}

/// Returns the saved upstream `symbol_name` provider for direct call.
///
/// Pub (not pub(crate)) so consumers can call rustc's default mangler for
/// a concrete `Instance` without re-entering `lang_symbol_name` — Path B
/// (Phase 4.5 single-symbol architecture) uses this to make
/// `consumer_symbol_for_callback_name` return the rustc-mangled name
/// rather than synthesizing `__toylang_impl_*`.
pub fn default_symbol_name() -> queries::symbol_name::SymbolNameFn {
    *DEFAULT_SYMBOL_NAME.get().expect("default symbol_name not saved")
}

// No `default_per_instance_mir` accessor: vanilla rustc's default provider
// (installed by `rustc_mir_transform::provide` per the fork's patch 3) always
// returns None. There's no upstream behavior to delegate to. The
// `per_instance.rs` override returns None directly for non-consumer items;
// rustc's collector then queries `instance_mir` via the fork's patch 2.

/// Tier 3 #3 Phase 2: pub so consumers can call rustc's UNFILTERED
/// `collect_and_partition_mono_items` provider directly. Bypasses the
/// in-memory query cache (which would return our filtered result).
/// Used by `llvm_gen::generate_with_tcx` to discover Case 1b generic
/// consumer instantiations (`__lang_stubs::wrap::<LocalThing>(42)`
/// from a `rust_caller.rs`) without going through the lifetime-erased
/// `upstream_cgus` stash. Cost: re-runs the mono collector once per
/// build (negligible for toylang fixtures, linear in crate size for
/// larger Sky projects).
pub fn default_collect_and_partition() -> CollectAndPartitionFn {
    *DEFAULT_COLLECT_AND_PARTITION
        .get()
        .expect("default collect_and_partition_mono_items not saved")
}

// `default_upstream_monomorphizations{_for}` accessors retired 2026-06-21
// (A.2 retirement under §5.5 Step 3).

// Trampoline functions — monomorphized for a specific C, then stored as fn pointers.
//
// Predicate trampolines (LangPredicates) take no `state`; stateful trampolines
// (LangCallbacks) take `&mut (dyn Any + Send + Sync)`. The two-family split is
// expressed by signature: predicate trampolines literally cannot touch state.

// Tier 3 #7.4 retired `trampoline_is_consumer_type` and
// `trampoline_is_consumer_fn`. Predicates now read from `SkyUniverse`; no
// vtable dispatch needed.

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

fn trampoline_consumer_symbol_for_callback_name<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> String {
    data.downcast_ref::<C>().unwrap().consumer_symbol_for_callback_name(name, tcx, instance)
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

fn trampoline_consumer_fill_modules<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
    factory: &mut LlvmModuleFactory<'_>,
) {
    data.downcast_ref::<C>().unwrap().consumer_fill_modules(state, tcx, factory)
}

// `trampoline_synthesize_upstream_monomorphizations` retired 2026-06-21
// (A.2 retirement under §5.5 Step 3).

/// Install callbacks for use by query overrides. Phase 1 of globals init.
pub(crate) fn install_callbacks<C: LangCallbacks + 'static>(
    callbacks: C,
) {
    let consumer_state = callbacks.create_state();
    let _ = CONFIG.set(FacadeConfig {
        callbacks: Box::new(callbacks),
        stateful_vtable: StatefulVtable {
            monomorphize_type: trampoline_monomorphize_type::<C>,
            collect_generic_rust_deps: trampoline_collect_generic_rust_deps::<C>,
            consumer_symbol_for_callback_name: trampoline_consumer_symbol_for_callback_name::<C>,
            after_rust_analysis: trampoline_after_rust_analysis::<C>,
            on_sky_lib_loaded: trampoline_on_sky_lib_loaded::<C>,
            consumer_fill_modules: trampoline_consumer_fill_modules::<C>,
        },
    });
    let _ = MUTABLE_STATE.set(std::sync::Mutex::new(consumer_state));
    // Stage 4c retired `VISIBILITY_OVERRIDE_HOOK`: under the post-Workstream-A
    // model the binary compile codegens every consumer item locally, so the
    // Phase-6 generic wrappers in `__lang_stubs` no longer cross a crate
    // boundary at link time and can keep rustc's default `Hidden` linkage.
    //
    // Stage 4b retired `CODEGEN_SKIP_HOOK`: the partitioner override
    // (formerly `queries::partition`) removed consumer items from rustc's
    // CGU slice before codegen dispatch ever saw them, so the hook was
    // unreachable. Option 4 (arch §F.14, 2026-06-20) then retired the
    // partitioner override itself; the `codegen_fn_attrs` override now
    // marks consumer items with `AvailableExternally` linkage so rustc's
    // LLVM backend emits no `.o` symbol for them, and the consumer's
    // `fill_extra_modules` body (patch 4) is the sole def at link.
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
    collect_and_partition: CollectAndPartitionFn,
    cross_crate_inlinable:
        queries::cross_crate_inlinable::CrossCrateInlinableFn,
    extern_cross_crate_inlinable:
        queries::cross_crate_inlinable::ExternCrossCrateInlinableFn,
) {
    let _ = DEFAULT_LAYOUT_OF.set(layout_of);
    let _ = DEFAULT_MIR_SHIMS.set(mir_shims);
    let _ = DEFAULT_SYMBOL_NAME.set(symbol_name);
    let _ = DEFAULT_COLLECT_AND_PARTITION.set(collect_and_partition);
    // upstream_monomorphizations{_for} params retired 2026-06-21 (A.2).
    // codegen_fn_attrs params retired 2026-06-22 (Option 4 retirement —
    // see arch §F.14.1 design history).
    let _ = DEFAULT_CROSS_CRATE_INLINABLE.set(cross_crate_inlinable);
    let _ = DEFAULT_EXTERN_CROSS_CRATE_INLINABLE.set(extern_cross_crate_inlinable);
}

/// Accessor for the saved upstream `cross_crate_inlinable` provider.
/// Used by the override to delegate when Sky machinery is dormant.
pub fn default_cross_crate_inlinable() -> queries::cross_crate_inlinable::CrossCrateInlinableFn {
    *DEFAULT_CROSS_CRATE_INLINABLE.get()
        .expect("default_cross_crate_inlinable: not installed yet")
}

/// Accessor for the saved upstream extern `cross_crate_inlinable` provider.
pub fn default_extern_cross_crate_inlinable()
    -> queries::cross_crate_inlinable::ExternCrossCrateInlinableFn
{
    *DEFAULT_EXTERN_CROSS_CRATE_INLINABLE.get()
        .expect("default_extern_cross_crate_inlinable: not installed yet")
}

// default_codegen_fn_attrs / default_extern_codegen_fn_attrs accessors
// retired 2026-06-22 (Option 4 retirement — see arch §F.14.1 design history).

#[cfg(test)]
mod tests {
    use super::*;

    /// Tier 3 #7.5: the universe correctly answers `contains_*` after
    /// items are inserted. Exercises the pure data structure — no rustc,
    /// no static. The end-to-end "universe populated from a real sidecar
    /// lights up the predicates" path is covered by the 138-fixture
    /// integration suite, which depends on the universe at every consumer
    /// codegen site since #7.3.
    #[test]
    fn sky_universe_populate_and_query() {
        let mut u = SkyUniverse::default();
        u.type_names.insert("Widget".to_string());
        u.fn_names.insert("clone_widget".to_string());
        u.typeids.insert(0x48723b0bb65d86f7);

        assert!(u.contains_type("Widget"));
        assert!(u.contains_fn("clone_widget"));
        assert!(u.contains_typeid(0x48723b0bb65d86f7));

        // Misses return false (not panic / not unwrap).
        assert!(!u.contains_type("Unknown"));
        assert!(!u.contains_fn("unknown"));
        assert!(!u.contains_typeid(0));
    }

    /// Tier 3 #8: type-erased `struct_infos` round-trip — insert a typed
    /// value as `Arc<dyn Any>`, retrieve it, downcast back. Confirms the
    /// pattern `monomorphize_type` uses to recover consumer-side metadata
    /// without the facade needing the consumer's typed-AST type.
    #[test]
    fn sky_universe_struct_info_round_trip() {
        use std::sync::Arc;
        #[derive(Debug, PartialEq)]
        struct FakeToyStruct {
            field_count: usize,
            payload: &'static str,
        }
        let original = FakeToyStruct { field_count: 3, payload: "hello" };
        let mut u = SkyUniverse::default();
        u.insert_struct_info("FakeStruct".to_string(), Arc::new(original));

        let retrieved = u
            .get_struct_info("FakeStruct")
            .expect("just inserted, must be present");
        let downcast: &FakeToyStruct = retrieved
            .downcast_ref()
            .expect("inserted as FakeToyStruct, must downcast back");
        assert_eq!(downcast.field_count, 3);
        assert_eq!(downcast.payload, "hello");

        // Misses return None (not panic).
        assert!(u.get_struct_info("Unknown").is_none());
    }

    /// The static accessor returns a default-empty universe when nothing
    /// has been written. Guards against the OnceLock initialiser regressing
    /// to a panic/expect.
    #[test]
    fn sky_universe_static_starts_empty() {
        // Note: this test reads the global. Because the static is shared
        // across all tests in this binary, other tests must not write to
        // it before this test runs. The facade has no other tests today;
        // future writers should use a dedicated test crate or `serial_test`.
        let u = sky_universe();
        assert!(!u.contains_type("Widget"));
        assert!(!u.contains_fn("anything"));
    }
}
