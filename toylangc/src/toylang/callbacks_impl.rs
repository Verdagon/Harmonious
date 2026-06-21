//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};

use rustc_lang_facade::{LangCallbacks, MonomorphizeTypeResult};
use crate::toylang::registry::ToylangRegistry;

/// Tier 3 #7.2: populate the facade's `SkyUniverse` from a `ToylangRegistry`.
/// Called from both the upstream-sidecar load path (`on_sky_lib_loaded`) and
/// the local-crate validation path (`after_rust_analysis` rlib-compile
/// branch) so the universe sees every Sky item visible to this rustc
/// invocation. Idempotent: re-inserting an existing entry is a no-op.
fn populate_sky_universe_from_registry(registry: &ToylangRegistry) {
    rustc_lang_facade::with_sky_universe_mut(|u| {
        // Tier 3 #8: also stash the full ToyStruct so `monomorphize_type`
        // can serve cross-Sky-crate layout queries (Case 6 / case6_lib's
        // `Box`) from the universe instead of a consumer-side mutex-
        // protected mirror.
        for (name, toy_struct) in &registry.structs {
            u.type_names.insert(name.clone());
            u.insert_struct_info(name.clone(), std::sync::Arc::new(toy_struct.clone()));
        }
        for (name, func) in &registry.functions {
            // Only body-bearing fns are "consumer fns" — body-less ones are
            // extern Rust fn declarations toylang binds to. The vtable
            // predicate `is_consumer_fn` enforces this; mirror the rule here
            // so universe-driven lookups agree.
            if func.body.is_some() {
                u.fn_names.insert(name.clone());
                // Toylang-specific: `main` is exposed to rustc via the stub
                // rlib as `__toylang_main` (the symbol the user-bin shim
                // calls). The universe must know the stub-side name because
                // facade predicates fire on `tcx.item_name(def_id)`-derived
                // strings, which see the stub name. Mirrors the special
                // case the vtable predicate (`is_consumer_fn`) used pre-#7.3.
                if name == "main" {
                    u.fn_names.insert(crate::oracle::TOYLANG_MAIN.to_string());
                }
            }
        }
        for &typeid in registry.typeid_table.keys() {
            u.typeids.insert(typeid);
        }
    });
}

/// A structured log entry for each callback rustc makes into toylang.
/// The `name` fields are consumed via `{:?}` formatting when
/// `TOYLANG_LOG_PATH` is set (see `generate_and_compile`); rustc's
/// dead-code analysis doesn't see through Debug derives, so the
/// `allow` is required.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum CallbackLog {
    MonomorphizeType { name: String },
    /// `args_fingerprint` is the Debug print of `instance.args` recorded
    /// at the moment the `per_instance_mir` callback fires. Under Approach A
    /// (course-correct.md item #1), this is the load-bearing positive
    /// evidence that the query fired per concrete Instance with substituted
    /// args, distinguishing it from Approach B's per-DefId firing on
    /// identity args. Tests that exercise multiple monomorphizations of
    /// the same consumer fn assert on distinct fingerprints; tests that
    /// exercise a single concrete instantiation assert the fingerprint
    /// contains the concrete type name (not "Param(").
    CollectGenericRustDeps { name: String, args_fingerprint: String },
    NotifyConcreteEntryPoint { name: String },
    AfterRustAnalysis,
    /// Fired once per upstream Sky-marked rlib loaded into this compile.
    /// `crate_name` is the rlib's crate name (e.g. `"__lang_stubs"`).
    /// `n_structs` / `n_functions` are the counts after deserialization —
    /// useful for the S.4 smoke test, which asserts the loaded registry is
    /// non-empty. Sidecars are loaded from the user-bin compile (the rlib
    /// compile has no upstream Sky-marked deps under wrapper mode), so
    /// this entry appears only in user-bin runs.
    OnSkyLibLoaded { crate_name: String, n_structs: usize, n_functions: usize },
    /// Fired once per user-bin compile (post-Workstream-A oracle sweep
    /// completion: course-correct.md items #11 + #15 prep). Counts how
    /// many of the registry's body-less (extern) functions
    /// `find_extern_fn_def_id` could resolve via the cross-crate fallback
    /// path in `oracle.rs::find_extern_fn_in_stub_rlib`. `total` is the
    /// number of body-less fns probed; `resolved` is the number whose
    /// DefId was successfully recovered from the upstream `__lang_stubs`
    /// rlib's `module_children`. Under a correctly-functioning fallback
    /// every body-less fn resolves (`resolved == total`); a regression in
    /// the fallback (e.g. someone reverts it to local-only) trips the
    /// smoke test by surfacing a mismatch.
    OracleCrossCrateProbe { resolved: usize, total: usize },
    /// Phase E Path 2 / Phase 2 — verifies the const-generic-u64 round-trip
    /// against a real `TyCtxt`. The rlib compile builds an opaque-args
    /// instance with a sentinel typeid via `build_opaque_args`, then decodes
    /// it back via `extract_typeid_from_args`. Equal values mean the encoder
    /// and decoder agree on representation; mismatch (or wrapper-not-found)
    /// surfaces as `opaque_def_id_found = false` so the integration test can
    /// assert end-to-end Phase 2 correctness. Fires at the rlib compile only
    /// (where `is_user_bin_compile == false`) since that's the first compile
    /// to load the stub rlib with the wrapper available.
    Phase2RoundTripProbe {
        opaque_def_id_found: bool,
        encoded_typeid: u64,
        decoded_typeid: Option<u64>,
    },
    GenerateAndCompile,
}

/// A toylang function instance discovered during the deep monomorphization walk.
///
/// `stub_def_id` carries the rustc DefId of the `pub fn <name>` shell in the
/// `__lang_stubs` rlib's source. It's `Some` when populated at the user-bin
/// compile via Workstream A's registry-driven discovery path; the codegen
/// pass uses it to construct an `Instance::new_raw(def_id, empty_args)` for
/// One Sky-emitted function destined for the codegen pass.
///
/// `extern_symbol`: the name external callers (Rust source or other Sky
/// items reached via rustc) use to reach this function. Under Path B this
/// is the rustc-default mangled name for items rustc references, and a
/// Sky-internal name for items only Sky's bitcode references.
///
/// `internal_symbol`: the Sky-internal name for the simple-ABI body.
/// Always distinct from `extern_symbol` for rustc-visible items (the
/// extern wrapper delegates to the internal); equal to `extern_symbol`
/// for Sky-internal items (which never get a wrapper).
///
/// `stub_def_id`: ABI-aware extern-wrapper emission needs an `Instance`.
/// At the rlib compile this field stays `None` (CGU-walk-driven discovery
/// surfaces real `Instance`s directly, and the old code path didn't need
/// this).
#[derive(Clone)]
pub struct ToylangInstance {
    pub extern_symbol: String,
    pub internal_symbol: String,
    pub resolved_func: crate::toylang::registry::ToyFunction,
    pub stub_def_id: Option<rustc_span::def_id::DefId>,
    /// Concrete args for the rustc Instance. Empty `Vec` is the degenerate
    /// (non-generic) case of the general path — falls out without a branch.
    /// For generic items (e.g. `<Wrapper<i32> as Clone>::clone` discovered
    /// via Option B sidecar) carries the `instance.args.types()` snapshot used
    /// to build the rustc `Instance` at codegen time via
    /// `oracle::build_generic_args_for_item`.
    pub instance_args: Vec<crate::toylang::typed_ast::ResolvedType>,
}

/// Mutable state accumulated during compilation. Stored in the facade's global
/// mutex and passed as `&mut dyn Any` to every callback. The facade ensures
/// single-threaded execution — no locking needed on the consumer side.
#[derive(Default)]
pub struct ToylangState {
    pub log: Vec<CallbackLog>,
    /// Toylang function instances discovered during the internal-callee walk.
    /// Populated by `walk_and_stash_internal_callees`, consumed by
    /// `generate_with_tcx`.
    pub toylang_instances: Vec<ToylangInstance>,
    /// Extern symbols already walked for internal-callee stashing. Persists
    /// across `notify_concrete_entry_point` calls so shared internal callees
    /// are stashed exactly once per compilation. The `collect_generic_rust_deps`
    /// path does NOT share this set — it uses a local cycle guard per call so
    /// transitively-reached deps from a second entry point are re-collected
    /// rather than silently skipped.
    pub walked_entry_points: HashSet<String>,
    /// Registries deserialized from upstream Sky-marked rlibs' `.sky-meta`
    /// sidecars at facade-load time. Populated by `on_sky_lib_loaded`
    /// (S.4 of the course-correct quarter-of-work plan). Keyed by the
    /// upstream crate name. S.4 only LANDS the loader — the registries
    /// are not yet consumed by codegen here; Workstream A.3 will read
    /// them at the user-bin compile to populate the codegen queue.
    /// `BTreeMap` (rather than `HashMap`) so iteration order is
    /// deterministic — same reasoning as `ToylangRegistry` (S.2 §7.4).
    pub upstream_registries: BTreeMap<String, ToylangRegistry>,
}

pub struct ToylangCallbacks {
    pub registry: Arc<ToylangRegistry>,
    // Phase 2 step 3 retired `llvm_paths`: the new inline-codegen path
    // (consumer_emit_modules → patch-(c) extra_modules hook) emits bitcode
    // bytes in-memory; no per-build .ll/.o temp paths needed.
    // Tier 3 #7.4 retired `upstream_fn_names` + `upstream_type_names`.
    // Their sole purpose was backing the `LangPredicates` impl's
    // `is_consumer_fn` / `is_consumer_type` methods. Those predicates now
    // read from the facade's `SkyUniverse` (populated in `on_sky_lib_loaded`
    // via `populate_sky_universe_from_registry`), so the per-callbacks
    // mirror is redundant.
    // Tier 3 #8 retired `upstream_structs`. Full ToyStruct field info now
    // lives in the facade's `SkyUniverse.struct_infos` (type-erased
    // `Arc<dyn Any + Send + Sync>`); `monomorphize_type` downcasts on read.
    // The Case 6 cross-Sky-crate layout path Session 9 patched goes through
    // the same single source of truth as local layouts.
    /// True at the user-bin compile under two-crate wrapper mode (course-
    /// correct.md items #11 + #15, Workstream A). The renaming + semantic
    /// flip from the prior `is_downstream_of_stubs` is part of A's
    /// codegen-ownership pivot:
    ///
    /// - **rlib compile (false):** owns validation in `after_rust_analysis`
    ///   and sidecar write (S.3). Skips codegen — `llvm_paths` is None
    ///   so `generate_and_compile` short-circuits before
    ///   `populate_toylang_instances_from_cgus` and the Inkwell pass.
    /// - **user-bin compile (true):** owns codegen. The
    ///   `populate_toylang_instances_from_cgus` step iterates the
    ///   registry directly (the CGU walk that worked at the rlib compile
    ///   finds nothing here — rustc doesn't queue extern non-generic
    ///   items for local mono). Validation is skipped (already ran upstream).
    pub is_user_bin_compile: bool,
}

/// Downcast `&mut dyn Any` to `&mut ToylangState`.
fn state(s: &mut dyn Any) -> &mut ToylangState {
    s.downcast_mut::<ToylangState>().expect("consumer state is not ToylangState")
}

impl ToylangCallbacks {
    /// Rust-dep discovery for a consumer function DefId. Pure read with
    /// respect to `ToylangState` (only `log` is appended); internal-callee
    /// stashing happens in `notify_concrete_entry_point_inner`.
    ///
    /// Per @SyMINCZ, the returned `(DefId, GenericArgsRef)` pairs become
    /// `ReifyFnPointer` casts in the `per_instance_mir` override's synthesized
    /// body, which is what forces rustc's mono collector to emit the Rust
    /// symbols. Under Approach A, this provider receives a fully concrete
    /// `Instance<'tcx>` and substitutes its `args` Sky-side before walking the
    /// body — every returned `GenericArgsRef` is concrete in toylang's universe
    /// (no `ty::TyKind::Param` placeholders).
    pub fn collect_generic_rust_deps_inner<'tcx>(
        &self,
        state: &mut ToylangState,
        name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
        state.log.push(CallbackLog::CollectGenericRustDeps {
            name: name.to_string(),
            args_fingerprint: format!("{:?}", instance.args),
        });

        // Accessor methods ("StructName.field_name") have no body and no deps.
        if name.contains('.') {
            return vec![];
        }

        let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main" } else { name };
        // §5.5 Step 1: build an effective registry merging local + all
        // upstreams so callees defined in upstream Sky libs are found.
        // Under cross-Sky-crate generic instantiation (DQ-D: dqd_app
        // instantiates dqd_lib_a::wrap<dqd_lib_b::Thing>), the cascade
        // at dqd_app's user_bin compile fires per_instance_mir for
        // `wrap` whose def_id is in dqd_lib_a. dqd_app's local
        // registry doesn't have `wrap`; we need to look it up in
        // dqd_lib_a's upstream registry loaded via on_sky_lib_loaded.
        // The recursive walker `collect_rust_deps_recursive` also
        // looks up callees via `registry.functions.get`, so we pass
        // the same merged registry there.
        let effective_registry = {
            let mut effective = (*self.registry).clone();
            for upstream in state.upstream_registries.values() {
                for (name, func) in &upstream.functions {
                    effective.functions
                        .entry(name.clone())
                        .or_insert_with(|| func.clone());
                }
                for (name, st) in &upstream.structs {
                    effective.structs
                        .entry(name.clone())
                        .or_insert_with(|| st.clone());
                }
            }
            effective
        };
        let toy_fn = effective_registry.functions.get(registry_name)
            .unwrap_or_else(|| panic!("[toylang] collect_generic_rust_deps: function '{}' not in local or upstream registries", registry_name));

        // Extern declarations (body-less) have no walkable body.
        if toy_fn.body.is_none() {
            return vec![];
        }

        // Sky-side substitution (Approach A, course-correct.md item #1): map
        // each of `caller_fn.type_params` to the concrete `ResolvedType`
        // recovered from `instance.args`. The resolved body has no remaining
        // `TypeParam` references — downstream oracle helpers see only
        // concrete types, and the deps the walker collects carry concrete
        // `GenericArgsRef`s. The retired Approach B path constructed
        // `identity_for_item` here and installed an `ActiveParamMap`
        // thread-local so the converter could rebuild Params; that whole
        // mechanism is gone.
        let resolved_caller = resolve_caller_from_instance(toy_fn, instance, tcx);
        let extern_symbol = compute_fn_symbol(registry_name, tcx, instance);
        // Local cycle guard — prevents infinite recursion on cyclic consumer
        // code. Intentionally NOT shared with `state.walked_entry_points`; see
        // @GCMLZ context and the commit introducing the callback split for the
        // rationale.
        let mut cycle_guard: HashSet<String> = HashSet::new();
        cycle_guard.insert(extern_symbol);
        collect_rust_deps_recursive(
            tcx, &effective_registry, &resolved_caller, registry_name, &mut cycle_guard,
        )
    }

    /// Concrete-entry-point notification. Returns the extern symbol the
    /// consumer has chosen for this Instance. Pure (aside from the log
    /// push) — the former side-effecting internal-callee walk and
    /// `state.toylang_instances` stashing moved to
    /// `populate_toylang_instances_from_cgus`, which runs up front in
    /// `generate_and_compile` instead of as a byproduct of rustc's
    /// query firing. Removing the side effect resolves B6 (rustc's
    /// incremental cache can short-circuit per-item queries; consumer
    /// `.o` went empty when it did).
    ///
    // Tier 3 #3 Phase 1c: `notify_concrete_entry_point_inner` retired.
    // It was the codegen-side helper for the CGU walk's accessor branch,
    // adding a `CallbackLog::NotifyConcreteEntryPoint` log push around
    // `compute_consumer_symbol`. With the accessor branch retired (Phase
    // 1c), the only remaining symbol mangler is the stateless
    // `compute_consumer_symbol`; the `NotifyConcreteEntryPoint` log
    // variant likewise becomes vestigial. Tests on the log shape no
    // longer rely on accessor entries.

    /// Path B / single-symbol architecture (Phase 4.5): return the
    /// rustc-default mangled symbol name for `instance`.
    ///
    /// Sky's bitcode now emits every rustc-visible consumer body (exports,
    /// accessors, trait-impl methods) under the rustc-mangled name rustc
    /// would have given the stub fn. Call sites and definitions share one
    /// symbol; ThinLTO sees a single definition and inlines it cross-
    /// module. The previous two-symbol scheme (`__toylang_impl_*` vs
    /// rustc-mangled stub) confused ThinLTO into inlining the
    /// `unreachable!()` stub body.
    ///
    /// `crate::default_symbol_name()` returns the saved upstream
    /// `SymbolNameFn`. Calling it directly (not `tcx.symbol_name(...)`)
    /// dodges re-entrance through the facade's `lang_symbol_name`
    /// override.
    ///
    /// The `_name` parameter (callback-name shape from the facade) is now
    /// unused for routing but kept in the signature so the trait method
    /// stays stable until Tier 3 #12 retires the
    /// `consumer_symbol_for_callback_name` callback entirely.
    pub fn compute_consumer_symbol<'tcx>(
        &self,
        _name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> String {
        let default = rustc_lang_facade::default_symbol_name();
        default(tcx, instance).name.to_string()
    }

    /// Populate `state.toylang_instances` + `state.walked_entry_points` by
    /// walking the partitioner's CGU list + recursively resolving each
    /// consumer entry point's internal callees. Runs at
    /// `generate_and_compile` time, once per compile. Replaces the former
    /// side-effect-accumulation pattern where each `symbol_name` call
    /// would incrementally populate these via
    /// `notify_concrete_entry_point_inner`.
    ///
    /// The former pattern broke under rustc's incremental cache: on cache
    /// hit the query provider is skipped, the walk doesn't fire, state
    /// stays empty, consumer `.o` goes symbol-less, link fails. Deriving
    /// everything from the post-mono CGU list up front makes the rebuild
    /// story incremental-safe — the CGU list itself is a rustc query,
    /// but its result is what we'd have ended up with anyway. Cached
    /// equals fresh equals correct.
    ///
    /// §5.5 narrower revision (Step 2 of A.1+A.2+patch-5 retirement chain):
    /// each compile session emits the items its crate OWNS. The historical
    /// "all Sky bodies emit at user_bin only" gate is retired; both
    /// compile sessions populate, with filtering that routes each Instance
    /// to the right session.
    ///
    /// - **Stub_rlib compile (`is_user_bin_compile = false`):** iterate the
    ///   LOCAL registry only; emit local non-generic exports + accessors
    ///   + non-generic trait-impl methods (uniformly: try empty-args
    ///   substitution, accept if the resolved body has no remaining
    ///   Params).
    /// - **User-bin compile (`is_user_bin_compile = true`):** skip the
    ///   registry walk entirely (LOCAL items emit at THIS package's
    ///   stub_rlib compile; UPSTREAM items at their own stub_rlib
    ///   compiles). Drain `discovered_trait_impl_instances` for entries
    ///   with non-empty `concrete_args` only — those are generic
    ///   instantiations whose concrete args originated at user_bin, so
    ///   the owning crate couldn't pre-emit them.
    pub fn populate_toylang_instances_from_cgus<'tcx>(
        &self,
        state: &mut ToylangState,
        tcx: TyCtxt<'tcx>,
    ) {
        // Phase C: entry-point walk replacing the registry walk.
        //
        // Architecture: §20.4 ("Sky's codegen queue populated from entry
        // points: main + exports + impl methods, transitively"). Roots are
        // export items only. Non-export Sky items reach codegen
        // transitively via `walk_and_stash_internal_callees` from an
        // exported caller (per §9.5).
        //
        // Behavior change vs Workstream A's registry walk: local
        // non-export non-generic code with no caller from any export is
        // no longer emitted. Aligned with §9.3 (non-exports not surfaced
        // to rustc at all). No existing fixture relies on the dead-code
        // emission (it'd be unreachable in any case).
        //
        // Roots (in order, dedup via `state.walked_entry_points`):
        //   1+2. Local main (implicitly export, per parser.rs:382) + local
        //        export free fns.
        //   3.   Local export impl-block methods.
        //   4.   Upstream export free fns.
        //   5.   Upstream export impl-block methods.
        //
        // Generic exports stay non-roots — they have no concrete args at
        // root. They reach codegen via Channel A (the CGU walk in
        // llvm_gen.rs's accessor/stash path), driven by caller-site
        // Instances surfaced by rustc's mono collector.
        //
        // Upstream registries are cloned upfront so we hold one immutable
        // borrow for the outer iteration while mutating `state` for the
        // inner walk-and-stash.
        let upstream_clones: Vec<crate::toylang::registry::ToylangRegistry> =
            state.upstream_registries.values().cloned().collect();
        // §5.5 Step 1: build an effective registry once (local + all
        // upstreams) for `walk_and_stash_internal_callees` to use. This
        // lets the recursive walker discover cross-Sky-crate Sky-internal
        // callees (e.g., dqd_app's main calling dqd_lib_a::wrap<dqd_lib_b::Thing>;
        // wrap isn't in dqd_app's local registry but IS in dqd_lib_a's
        // upstream registry). Without the effective registry, walk_and_stash
        // skips cross-crate Sky-internal callees → their bodies aren't
        // emitted → link error.
        let effective_registry = {
            let mut effective = (*self.registry).clone();
            for upstream in &upstream_clones {
                for (name, func) in &upstream.functions {
                    effective.functions
                        .entry(name.clone())
                        .or_insert_with(|| func.clone());
                }
                for (name, st) in &upstream.structs {
                    effective.structs
                        .entry(name.clone())
                        .or_insert_with(|| st.clone());
                }
            }
            effective
        };
        // §5.5 Step 2 (narrower): at stub_rlib compile, emit LOCAL items
        // only — every upstream's items emit at THAT upstream's own
        // stub_rlib compile (this same function in that rustc invocation).
        // At user_bin compile, skip the registry walk entirely; only
        // generic instantiations need user_bin codegen and they flow
        // through the `discovered_trait_impl_instances` drain below.
        let registries: Vec<&crate::toylang::registry::ToylangRegistry> = if self.is_user_bin_compile {
            vec![]
        } else {
            vec![self.registry.as_ref()]
        };
        for reg in registries {
            // Export free fns (and main).
            for (name, toy_fn) in &reg.functions {
                if !toy_fn.is_export
                    || toy_fn.body.is_none()
                    || toy_fn.has_abstract_args()
                {
                    continue;
                }
                // Path B: lookup stub_def_id FIRST so we can build an
                // Instance and compute the rustc-default mangled name.
                // Non-generic exports are rustc-visible — rustc-emitted
                // call sites query `symbol_name` for them, and Sky's
                // bitcode must emit the body under the same symbol so
                // ThinLTO sees one definition.
                //
                // The stub's name uses toylang's mangling convention —
                // `main` becomes `__toylang_main` (per `oracle::TOYLANG_MAIN`),
                // everything else stays as-is.
                let stub_name = if name == "main" {
                    crate::oracle::TOYLANG_MAIN.to_string()
                } else {
                    name.clone()
                };
                let stub_def_id = crate::oracle::find_stub_fn_in_stub_rlib(tcx, &stub_name);
                let extern_symbol = if let Some(def_id) = stub_def_id {
                    // arch-fence-allow: degenerate-case-fast-path
                    // (non-generic exports filtered by has_abstract_args above —
                    // empty args feeds Instance::new_raw cleanly).
                    let instance = ty::Instance::new_raw(def_id, ty::GenericArgs::empty());
                    compute_fn_symbol(name, tcx, instance)
                } else {
                    // No stub shell ⇒ no rustc reference. Sky-internal naming
                    // is sufficient (and required, since we have no Instance).
                    compute_internal_symbol_from_type_args(name, &[])
                };
                if !state.walked_entry_points.insert(extern_symbol.clone()) {
                    continue;
                }
                let internal_symbol = compute_internal_symbol_from_type_args(name, &[]);
                state.toylang_instances.push(ToylangInstance {
                    extern_symbol,
                    internal_symbol,
                    resolved_func: toy_fn.clone(),
                    stub_def_id,
                    instance_args: vec![],
                });
                // Transitive walk: surfaces non-export Sky callees + generic
                // monomorphizations reachable from this root.
                // §5.5 Step 1: walk via effective_registry so cross-Sky-crate
                // callees are found (e.g., dqd_app main calling dqd_lib_a::wrap).
                // local_registry is self.registry so walk_and_stash can
                // discriminate owning-crate (Step 2's rule).
                walk_and_stash_internal_callees(tcx, &effective_registry, self.registry.as_ref(), toy_fn, name, state);
            }
            // Trait-impl method monomorphizations flow uniformly through the
            // Option B discovered-instances loop below — captured at the
            // upstream stub-rlib compile's mono cascade for every concrete
            // instantiation rustc actually queues, regardless of impl-block
            // genericity. The prior `for toy_impl in &reg.trait_impls`
            // populate channel only pushed an empty-args mono per method
            // — which is meaningful only for impls with zero type params,
            // i.e. it was a non-generic special case. CLAUDE.md compiler
            // law: non-generic is the degenerate case of generic; the cascade
            // captures it uniformly (case4's `<Widget as Clone>::clone`
            // appears in the captured list with `concrete_args = []`).
            // Tier 3 #3 Phase 1b: accessor functions. Each (struct, field)
            // pair becomes a regular Sky `ToyFunction` (synthesised on the
            // fly via `synthesize_accessor_fn`) that flows through the
            // standard ToylangInstance pipeline. The dedicated
            // `codegen_accessor_inline` path retires; standard
            // `codegen_internal_function` + `codegen_extern_wrapper`
            // handle accessors uniformly.
            //
            // Generic struct accessors (struct has type_params) are
            // skipped here — their concrete instantiations only become
            // visible after rustc's mono walk surfaces them. The CGU
            // walk's accessor branch in llvm_gen handles those (Phase 1c
            // rewrites it to also call `synthesize_accessor_fn`).
            for (struct_name, field_name) in &reg.accessor_pairs {
                let Some(toy_struct) = reg.structs.get(struct_name) else { continue; };
                let Some(field) = toy_struct.fields.iter().find(|f| &f.name == field_name)
                    else { continue; };
                let resolved_func = crate::toylang::registry::synthesize_accessor_fn(
                    struct_name, toy_struct, field,
                );
                // Mirrors the trait_impls loop's gate at the same depth.
                // Skips abstract-args (generic struct) accessors here;
                // concrete instantiations get surfaced by rustc's mono walk
                // (Case 1b path) and the CGU walk's accessor branch picks
                // them up with concrete args.
                if resolved_func.body.is_none() || resolved_func.has_abstract_args() {
                    continue;
                }
                let Some(struct_def_id) = crate::oracle::find_local_struct_def_id(tcx, struct_name)
                    else { continue; };
                let Some(stub_def_id) = crate::oracle::find_inherent_method(
                    tcx, struct_def_id, field_name,
                ) else { continue; };
                let instance = ty::Instance::new_raw(stub_def_id, ty::GenericArgs::empty());
                let extern_symbol = compute_fn_symbol(field_name, tcx, instance);
                if !state.walked_entry_points.insert(extern_symbol.clone()) {
                    continue;
                }
                // Sky-internal name qualified by `(struct, field)` so
                // accessors don't collide with free fns or each other.
                let internal_symbol = format!(
                    "__toylang_internal__accessor__{}__{}",
                    struct_name, field_name,
                );
                state.toylang_instances.push(ToylangInstance {
                    extern_symbol,
                    internal_symbol,
                    resolved_func,
                    stub_def_id: Some(stub_def_id),
                    instance_args: vec![],
                });
            }
            // §5.5 Step 2 (narrower-§5.5): eagerly emit LOCAL trait-impl
            // methods that are fully concrete at this compile (no impl-
            // block-level type params, no method-level type params). This
            // covers cases like `impl Clone for Box` in case6_lib where
            // case6_lib has no `main`, so its `per_instance_mir` cascade
            // never queues the method — but case6_lib still OWNS the body
            // and must emit it for downstream compiles to link against.
            //
            // Uniform formulation (NNGZ-compliant): try empty-args
            // substitution; if the substituted body has remaining Params
            // (generic impl block or method-level type params), skip —
            // discovery cascade at the using crate will provide concrete
            // args via `discovered_trait_impl_instances` and the body
            // emits at user_bin per Step 3. Non-generic impls fall out
            // of the same path: empty substitution is identity, no
            // Params remain, emit.
            //
            // The historical comment block above (lines ~441-451) retired
            // a prior trait_impls populate channel on NNGZ grounds (it
            // gated emission on whether the source had any type params).
            // The Step 2 form here does NOT inspect the source's type
            // params directly; it checks whether the SUBSTITUTED body
            // still carries Params, which is the same uniform check used
            // everywhere else in this function.
            for toy_impl in &reg.trait_impls {
                for method in &toy_impl.methods {
                    if method.func.body.is_none() {
                        continue;
                    }
                    // §5.5 Step 2 eager-emit gate.
                    // `substitute_type_params` panics on any unresolved
                    // Param, so the NNGZ-uniform "try identity subst,
                    // accept if no Params remain" formulation isn't
                    // available — substitute panics before returning.
                    // Forced exception: gate eager-emit on the combined
                    // impl-block + method-level params (per
                    // `resolve_caller_from_instance` doc comment). Empty
                    // means safe to substitute with empty args; non-empty
                    // means we need concrete args from a cascade (Step 3).
                    // arch-fence-allow: Approach-A substituted-vs-unsubstituted invariant
                    if !method.func.type_params.is_empty() {
                        continue;
                    }
                    let resolved_func = resolve_caller_from_type_args(
                        &method.func,
                        &std::collections::HashMap::new(),
                    );
                    let Some(stub_def_id) = crate::oracle::find_trait_impl_method_def_id(
                        tcx,
                        &toy_impl.trait_name,
                        &toy_impl.self_type_name,
                        &method.name,
                    ) else { continue };
                    let instance = ty::Instance::new_raw(stub_def_id, ty::GenericArgs::empty());
                    let extern_symbol = compute_fn_symbol(&method.name, tcx, instance);
                    if !state.walked_entry_points.insert(extern_symbol.clone()) {
                        continue;
                    }
                    let internal_symbol = format!(
                        "__toylang_internal__{}__{}__{}",
                        toy_impl.self_type_name, toy_impl.trait_name, method.name,
                    );
                    state.toylang_instances.push(ToylangInstance {
                        extern_symbol,
                        internal_symbol,
                        resolved_func: resolved_func.clone(),
                        stub_def_id: Some(stub_def_id),
                        instance_args: vec![],
                    });
                    // §5.5 Step 1: use effective_registry for cross-crate callees.
                    walk_and_stash_internal_callees(
                        tcx, &effective_registry, self.registry.as_ref(),
                        &resolved_func, &method.name, state,
                    );
                }
            }
        }
        // §5.5 Step 3: the historical user_bin drain of upstream
        // sidecars' `discovered_trait_impl_instances` retired here.
        // Cascade-firing crates (stub_rlibs) now drain their own
        // discoveries inline in `consumer_fill_modules`'s
        // !is_user_bin_compile branch — bodies emit at the same
        // compile session that captured them, so no cross-session
        // ship-and-replay is needed. The sidecar discoveries list is
        // still written (for diagnostic / future tooling use) but is
        // no longer load-bearing for emission.
    }
}

// Workstream A retired three helpers:
//   - emit_layout_log_for_instance
//   - walk_ty_for_layout_log
//   - compute_layout_size_align
// They used to fire from the per-Instance CGU walk in
// populate_toylang_instances_from_cgus, emitting the
// `[toylang] layout_of intercepted for: <ty> size=N align=M` log line
// per consumer ADT in each Instance's signature. Under Workstream A's
// registry-driven discovery there are no per-Instance signatures to
// walk, so this code path went dark. The layout-probe tests
// (test_point_layout etc.) continue to pass because the `lang_layout_of`
// query provider in the facade emits its own log line on each
// interception. If a future test surfaces a gap (e.g., layouts hit by
// transitive callees but not by direct query firings), the re-engineering
// option is to construct an Instance per registry item and walk its
// signature — but it's not currently needed.

// Tier 3 #7.4 retired `impl LangPredicates for ToylangCallbacks`. The two
// methods (`is_consumer_type`, `is_consumer_fn`) used to feed the facade's
// predicate vtable; the universe-based `is_consumer_type` /
// `is_consumer_fn` in the facade now reads from `SkyUniverse` directly.
// Population happens in `on_sky_lib_loaded` (upstream) and
// `after_rust_analysis` (local) via `populate_sky_universe_from_registry`.
//
// Stage 4c retired the `visibility_override` trait method: the facade's
// partitioner override now forces `(External, Default)` on `__lang_stubs`
// items directly in the CGU slice. Stage 5c.4 retired the `generate_stubs`
// trait method: wrapper mode's `build::write_stub_crate` calls
// `stub_gen::generate` directly.

/// Step 5 / Option B — record stashed in the facade-owned `SkyUniverse`
/// at `on_sky_lib_loaded` time, read by
/// `synthesize_upstream_monomorphizations`. The `crate_name` is captured
/// here so the stateless synthesis can look up the upstream's `CrateNum`
/// via `tcx.crates(())` without a side channel.
struct StashedDiscovery {
    crate_name: String,
    self_type_name: String,
    trait_name: String,
    method_name: String,
    concrete_args: Vec<crate::toylang::typed_ast::ResolvedType>,
}

impl LangCallbacks for ToylangCallbacks {
    fn create_state(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(ToylangState::default())
    }

    fn after_rust_analysis<'tcx>(&self, s: &mut dyn Any, tcx: TyCtxt<'tcx>) {
        state(s).log.push(CallbackLog::AfterRustAnalysis);

        // Tier 3 #7.2: mirror the LOCAL registry into the facade's
        // `SkyUniverse`. Done unconditionally (both rlib + user-bin compiles)
        // so query predicates fired downstream see the local items. Upstream
        // items were populated earlier in `on_sky_lib_loaded`.
        //
        // Idempotent + cheap; called at most once per rustc invocation.
        populate_sky_universe_from_registry(&self.registry);

        // Workstream A: validation lives at the rlib compile only. The
        // user-bin compile trusts the rlib-compile's validation as the
        // single source of truth — any invalid registry would have
        // already aborted upstream. The cross-crate oracle sweep
        // (find_extern_fn_def_id et al.) means user-bin lookups would
        // also work mechanically, but re-running validation would just
        // duplicate work.
        if self.is_user_bin_compile {
            // Workstream-A oracle sweep probe: exercise the cross-crate
            // fallback in `find_extern_fn_def_id` (and by extension the
            // shared `find_extern_fn_in_stub_rlib` helper). The fallback's
            // surfaces (codegen + dep-walker) don't fire at user-bin time
            // YET — they will once A.4 inverts the codegen gating — so
            // without this probe the cross-crate path is dark code. The
            // probe iterates the registry's body-less (extern) fns and
            // counts how many resolve. A correctly-functioning fallback
            // resolves all of them.
            let mut resolved: usize = 0;
            let mut total: usize = 0;
            for (name, func) in &self.registry.functions {
                if func.body.is_some() { continue; }
                total += 1;
                if crate::oracle::find_extern_fn_def_id(tcx, name).is_some() {
                    resolved += 1;
                }
            }
            state(s).log.push(CallbackLog::OracleCrossCrateProbe { resolved, total });
            return;
        }

        let mut errors: Vec<String> = Vec::new();

        // Check 1: Every toylang struct is visible to rustc
        for (name, _) in &self.registry.structs {
            if crate::oracle::find_local_struct_def_id(tcx, name).is_none() {
                errors.push(format!("struct '{}' not found in rustc (stub generation may have failed)", name));
            }
        }

        // Check 2: Every EXPORT toylang function with a body has a stub.
        // Session 10 — Sky architecture §9: non-export items don't get a
        // rustc-visible stub by design; their bodies live entirely Sky-side.
        // The check applies only to items the source explicitly exposed.
        // `main` is implicitly export (handled by the parser).
        for (name, func) in &self.registry.functions {
            if func.body.is_none() { continue; }
            if !func.is_export { continue; }
            let stub_name = if name == "main" { crate::oracle::TOYLANG_MAIN } else { name.as_str() };
            if find_stub_fn_def_id(tcx, stub_name).is_none() {
                errors.push(format!("function '{}' has no stub in __lang_stubs (expected '{}')", name, stub_name));
            }
        }

        // Check 3: Rust types referenced in field types exist
        for (struct_name, toy_struct) in &self.registry.structs {
            for field in &toy_struct.fields {
                for rust_name in collect_rust_type_names(&field.rust_type) {
                    if crate::oracle::find_rust_type_def_id(tcx, &rust_name).is_none() {
                        errors.push(format!(
                            "struct '{}' field '{}': Rust type '{}' not found",
                            struct_name, field.name, rust_name
                        ));
                    }
                }
            }
        }

        // Check 4: Extern functions exist in Rust
        for (name, func) in &self.registry.functions {
            if func.body.is_some() { continue; }
            if crate::oracle::find_extern_fn_def_id(tcx, name).is_none() {
                errors.push(format!("extern function '{}' not found in Rust code", name));
            }
        }

        // Phase 3 E.5: build an "effective registry" that merges the local
        // registry with any upstream toylang library registries loaded via
        // S.4's `on_sky_lib_loaded`. The merge is read-only — `self.registry`
        // is unchanged — and is used ONLY by the type-resolve pass below.
        // Validation Checks 1-4 above intentionally stay on the local
        // registry: they verify that every LOCAL toylang item has a rustc-
        // visible stub, which is a per-crate invariant. Upstream items are
        // verified at their own crate's compile.
        //
        // Collision policy: local entries win silently. A proper diagnostic
        // for name collisions is E.6 follow-up work.
        let effective_registry = {
            let mut effective = (*self.registry).clone();
            for upstream in state(s).upstream_registries.values() {
                for (name, func) in &upstream.functions {
                    effective.functions
                        .entry(name.clone())
                        .or_insert_with(|| func.clone());
                }
                for (name, st) in &upstream.structs {
                    effective.structs
                        .entry(name.clone())
                        .or_insert_with(|| st.clone());
                }
            }
            effective
        };

        // Check 5: Type-resolve non-generic function bodies
        for (name, func) in &self.registry.functions {
            // Phase B: typecheck generic bodies too. Rust trait/method
            // queries with TypeParam args now return RustTypeDeferred (via
            // oracle's contains_type_param guard), which `is_deferred()`
            // silently skips below.
            if func.body.is_none() { continue; }
            let rust_method_ret = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<crate::toylang::typed_ast::ResolvedType, crate::oracle::UnresolvedRustType> {
                if type_name.is_empty() {
                    crate::oracle::rust_free_fn_return_type(tcx, method, type_args)
                        .map(|opt| opt.unwrap_or(crate::toylang::typed_ast::ResolvedType::Void))
                } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                    let receiver_ty = &type_args[0];
                    let explicit_args = &type_args[1..];
                    crate::oracle::rust_trait_method_return_type(tcx, trait_name, method, receiver_ty, explicit_args)
                } else {
                    crate::oracle::rust_method_return_type(tcx, type_name, method, type_args)
                }
            };
            let rust_param_types = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, crate::oracle::UnresolvedRustType> {
                if type_name.is_empty() {
                    crate::oracle::rust_free_fn_param_types(tcx, method, type_args)
                } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                    crate::oracle::rust_trait_method_param_types(tcx, trait_name, method, &type_args[0], &type_args[1..])
                } else {
                    crate::oracle::rust_method_param_types(tcx, type_name, method, type_args)
                }
            };
            // Per @IVTDBTZ, trait-vs-inherent dispatch predicate — asks the
            // oracle directly whether `name` is a `use`-imported Rust trait.
            let is_rust_trait = |name: &str| {
                crate::oracle::find_use_imported_trait_def_id(tcx, name).is_some()
            };
            match crate::toylang::type_resolve::resolve_fn_body(&effective_registry, func, &rust_method_ret, &rust_param_types, &is_rust_trait) {
                Err(e) if e.is_deferred() => {
                    // Workstream B — query needs concrete args; the per-Instance
                    // substituted pass will redo it. Don't surface as user error.
                }
                Err(e) => errors.push(format!("function '{}': {:?}", name, e)),
                Ok(typed) => {
                    // Per @MBMRVZ, if main has no declared return type (so its
                    // extern wrapper is pinned to `fn __toylang_main() -> ()`),
                    // the body's tail must also be void. Declaring `fn main() -> T`
                    // explicitly is fine — both forms agree on T. The mismatch
                    // only arises when declared=void but tail=non-void.
                    if name == "main" && func.return_ty.is_none() {
                        if let Some(ret_expr) = &typed.ret {
                            if ret_expr.ty != crate::toylang::typed_ast::ResolvedType::Void {
                                errors.push(format!(
                                    "function 'main': {:?}",
                                    crate::toylang::type_resolve::TypeResolveError::MainMustReturnVoid {
                                        got: ret_expr.ty.clone(),
                                    }
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Check 6 (Phase 2 C.3): Type-resolve impl-block method bodies. Each
        // method's `self` parameter was elevated to an explicit
        // `self: &ToyStruct` by the parser (C.1), so the existing fn-body
        // type-resolver handles them with no method-specific code path.
        //
        // Skip methods whose impl's self-type was not declared at parse time
        // — that error was already raised in C.1; here we just avoid double-
        // reporting. Generic methods (non-empty type_params) are deferred
        // to the per-Instance substituted pass, same as generic free fns.
        for toy_impl in &self.registry.trait_impls {
            for method in &toy_impl.methods {
                // Phase B: typecheck generic impl-method bodies too.
                if method.func.body.is_none() {
                    continue;
                }
                let rust_method_ret = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<crate::toylang::typed_ast::ResolvedType, crate::oracle::UnresolvedRustType> {
                    if type_name.is_empty() {
                        crate::oracle::rust_free_fn_return_type(tcx, method, type_args)
                            .map(|opt| opt.unwrap_or(crate::toylang::typed_ast::ResolvedType::Void))
                    } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                        let receiver_ty = &type_args[0];
                        let explicit_args = &type_args[1..];
                        crate::oracle::rust_trait_method_return_type(tcx, trait_name, method, receiver_ty, explicit_args)
                    } else {
                        crate::oracle::rust_method_return_type(tcx, type_name, method, type_args)
                    }
                };
                let rust_param_types = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, crate::oracle::UnresolvedRustType> {
                    if type_name.is_empty() {
                        crate::oracle::rust_free_fn_param_types(tcx, method, type_args)
                    } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                        crate::oracle::rust_trait_method_param_types(tcx, trait_name, method, &type_args[0], &type_args[1..])
                    } else {
                        crate::oracle::rust_method_param_types(tcx, type_name, method, type_args)
                    }
                };
                let is_rust_trait = |name: &str| {
                    crate::oracle::find_use_imported_trait_def_id(tcx, name).is_some()
                };
                match crate::toylang::type_resolve::resolve_fn_body(
                    &effective_registry,
                    &method.func,
                    &rust_method_ret,
                    &rust_param_types,
                    &is_rust_trait,
                ) {
                    Err(e) if e.is_deferred() => { /* same defer policy as Check 5 */ }
                    Err(e) => errors.push(format!(
                        "impl {} for {}::{}: {:?}",
                        toy_impl.trait_name, toy_impl.self_type_name, method.name, e,
                    )),
                    Ok(_typed) => {
                        // Phase 2 C.3 intentionally does NOT cross-check the
                        // method signature against the Rust trait's signature
                        // yet. That validation belongs at stub_gen time
                        // (C.4–C.6) when the impl block is materialised — if
                        // the toylang sig diverges, rustc itself will report
                        // the mismatch on the generated stub source, which is
                        // the standard "rustc error on stub rlib is a Sky
                        // bug" path (course-correct.md §23.2). Adding a Sky-
                        // side cross-check earlier is a future tightening.
                    }
                }
            }
        }

        if !errors.is_empty() {
            eprintln!("[toylang] validation failed with {} error(s):", errors.len());
            for e in &errors {
                eprintln!("  - {}", e);
            }
            panic!("[toylang] aborting due to validation errors");
        }

        // Phase E Path 2 / Phase 2 — round-trip probe. With Phase 1's wrapper
        // emission in stub_gen and Phase 2's encode/decode helpers in oracle,
        // verify the const-u64 plumbing works against the actual rustc Const
        // API on the pinned nightly. Sentinel typeid is the same one Phase 1
        // pinned for `Widget` so a regression in either the helper layer or
        // the rustc API surface surfaces consistently across both tests.
        let encoded_typeid: u64 = 0x48723b0bb65d86f7;
        let opaque_def_id_opt = crate::oracle::find_toylang_opaque_def_id(tcx);
        let decoded_typeid = opaque_def_id_opt.map(|opaque_def_id| {
            let args = crate::oracle::build_opaque_args(tcx, opaque_def_id, encoded_typeid);
            crate::oracle::extract_typeid_from_args(args)
        }).flatten();
        state(s).log.push(CallbackLog::Phase2RoundTripProbe {
            opaque_def_id_found: opaque_def_id_opt.is_some(),
            encoded_typeid,
            decoded_typeid,
        });

        // S.3 (course-correct quarter-of-work plan): write the `.sky-meta`
        // sidecar adjacent to the rlib that rustc is about to emit. The
        // user_bin compile reads it via the facade's sidecar loader (S.4),
        // populating the universe needed for binary-compile codegen (A.3 / A.4).
        //
        // We hook here in `after_rust_analysis` because (a) the registry is
        // fully populated by this point, (b) rustc has computed
        // `output_filenames` so the rlib path is known, and (c) this fires
        // ONCE per rlib compile (gated above on `!is_user_bin_compile`),
        // which is exactly when the sidecar should be produced.
        //
        // `OutputFilenames::with_extension` joins out_directory + filestem
        // and sets the extension — yielding a path whose basename matches
        // the rlib's exactly except for the `.sky-meta` extension. This is
        // what `docs/architecture/sidecar-format.md` requires.
        let sidecar_path = tcx.output_filenames(()).with_extension("sky-meta");
        // Phase E Path 2 / Phase 1.3 — populate the typeid table just
        // before serialization. Cheap (one BLAKE3 hash per struct) and keeps
        // the table fresh against any registry edits earlier in the typing
        // pass. We clone to populate because `after_rust_analysis` takes
        // `&self`; serialization is otherwise pure with respect to the
        // registry. Architecture §10.8: the table ships in the sidecar so
        // downstream compiles can decode upstream typeids.
        let mut registry_for_sidecar: ToylangRegistry = (*self.registry).clone();
        registry_for_sidecar.populate_typeid_table();
        let bytes = crate::sidecar::serialize_sidecar(&registry_for_sidecar)
            .unwrap_or_else(|e| panic!("[toylang] sidecar serialize failed: {}", e));
        std::fs::write(&sidecar_path, &bytes).unwrap_or_else(|e| {
            panic!(
                "[toylang] sidecar write failed at {}: {}",
                sidecar_path.display(),
                e,
            )
        });
        // Diagnostic eprintln gated on TOYLANG_LOG_PATH so it doesn't
        // pollute the build stderr that layout-probe tests grep.
        if std::env::var("TOYLANG_LOG_PATH").is_ok() {
            eprintln!(
                "[toylang] wrote sidecar: {} ({} bytes)",
                sidecar_path.display(),
                bytes.len(),
            );
        }
    }

    fn on_sky_lib_loaded<'tcx>(
        &self,
        s: &mut dyn Any,
        _tcx: TyCtxt<'tcx>,
        crate_name: &str,
        sidecar_bytes: &[u8],
    ) {
        let ts = state(s);
        // Deserialize unconditionally. The facade's missing-file path
        // already panicked if the sidecar wasn't readable; a deserialize
        // failure here means the bytes are present but malformed, which
        // is a hard-error condition per architecture doc §7.6.
        let registry = crate::sidecar::deserialize_sidecar(sidecar_bytes)
            .unwrap_or_else(|e| {
                panic!(
                    "[toylang] failed to deserialize sidecar for crate `{}`: {}",
                    crate_name, e,
                )
            });
        let n_structs = registry.structs.len();
        let n_functions = registry.functions.len();
        ts.log.push(CallbackLog::OnSkyLibLoaded {
            crate_name: crate_name.to_string(),
            n_structs,
            n_functions,
        });
        // Insertion order matters only for diagnostics; cross-crate name
        // collisions between Sky libs are out of scope until Phase 3 E
        // (multi-crate). For now we trust the facade to call us at most
        // once per crate.
        // Phase 3 E.6: mirror the body-bearing fn names + struct names into
        // the callbacks-level sets so `is_consumer_fn` / `is_consumer_type`
        // (called via the predicate vtable, which doesn't see state) recognize
        // them. The `symbol_name` query override then redirects cross-crate
        // consumer-fn calls (e.g. the user-bin's main calling case6_lib::double_it)
        // to the consumer emitter's `__toylang_impl_*` symbol that codegen
        // produces from the populate-upstream iteration.
        //
        // Tier 3 #7.4 + #8: all mirrors retired. The facade's `SkyUniverse`
        // carries (a) body-bearing fn names + type names for predicates,
        // (b) full ToyStruct field info via the type-erased `struct_infos`
        // map for `monomorphize_type`'s Case-6 cross-Sky-crate layout
        // queries. `populate_sky_universe_from_registry` writes both in
        // one pass.
        populate_sky_universe_from_registry(&registry);
        // Step 5 / Option B: push the upstream's captured discoveries into
        // the facade-owned SkyUniverse so the stateless
        // `lang_upstream_monomorphizations_for` query override can
        // synthesise the args→CrateNum map without re-locking
        // MUTABLE_STATE (@GCMLZ).
        rustc_lang_facade::with_sky_universe_mut(|u| {
            for d in &registry.discovered_trait_impl_instances {
                u.push_discovery(std::sync::Arc::new(StashedDiscovery {
                    crate_name: crate_name.to_string(),
                    self_type_name: d.self_type_name.clone(),
                    trait_name: d.trait_name.clone(),
                    method_name: d.method_name.clone(),
                    concrete_args: d.concrete_args.clone(),
                }));
            }
        });
        ts.upstream_registries.insert(crate_name.to_string(), registry);
    }

    fn monomorphize_type<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx> {
        // Stateless: facade's `call_monomorphize_type` skips the mutex
        // so `lang_layout_of` can re-enter during `generate_and_compile`
        // without deadlocking. Former `CallbackLog::MonomorphizeType`
        // log push retired — wasn't consumed by any test.
        //
        // Tier 3 #8: single source of truth via the facade's `SkyUniverse`.
        // `populate_sky_universe_from_registry` (called both at local-
        // registry build and at upstream sidecar load) deposits every Sky
        // struct's `ToyStruct` here type-erased. Downcast to recover the
        // typed metadata. The previous dual-source dance (local registry +
        // mutex-protected `upstream_structs` mirror) retires; Case 6 cross-
        // Sky-crate layouts still resolve because case6_lib's structs were
        // populated into the universe at `on_sky_lib_loaded` time.
        let info = rustc_lang_facade::sky_universe()
            .get_struct_info(name)
            .unwrap_or_else(|| panic!(
                "[toylang] monomorphize_type: struct '{}' not in SkyUniverse \
                 (likely a populate-ordering bug or a missing sidecar load)",
                name,
            ));
        let toy_struct: &crate::toylang::registry::ToyStruct = info
            .downcast_ref()
            .expect("SkyUniverse.struct_infos entry is not a ToyStruct");

        // Build type-param substitution at the rustc Ty level (no round-trip through ResolvedType).
        // Compiler-law: general zip-and-collect form handles the zero-param
        // degenerate case naturally (empty params → empty zip → empty subst).
        // No branch on param count.
        let args = if let TyKind::Adt(_, args) = ty.kind() {
            *args
        } else {
            ty::GenericArgs::empty()
        };
        let ty_subst: HashMap<&str, Ty<'tcx>> = toy_struct.type_params.iter()
            .zip(args.types())
            .map(|(name, arg_ty)| (name.as_str(), arg_ty))
            .collect();

        // Convert each field's ResolvedType to rustc Ty, substituting TypeParams directly.
        let field_types: Vec<Ty<'tcx>> = toy_struct.fields.iter().map(|field| {
            resolved_to_rustc_ty_with_subst(tcx, &field.rust_type, &ty_subst)
        }).collect();

        MonomorphizeTypeResult {
            field_types,
        }
    }

    fn collect_generic_rust_deps<'tcx>(
        &self,
        s: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
        self.collect_generic_rust_deps_inner(state(s), name, tcx, instance)
    }

    fn consumer_symbol_for_callback_name<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> String {
        // Tier 3 #9: stateless. Pure function of (self.registry, name,
        // tcx, instance). No `&mut dyn Any state`; no `MUTABLE_STATE`
        // lock at the facade level. The trampoline takes no `state`
        // either (mirrors `monomorphize_type`).
        self.compute_consumer_symbol(name, tcx, instance)
    }

    fn consumer_fill_modules<'tcx>(
        &self,
        s: &mut dyn Any,
        tcx: TyCtxt<'tcx>,
        factory: &mut rustc_lang_facade::LlvmModuleFactory,
    ) {
        // Approach B (patch 4 rev 2): the facade hands us a fresh
        // LLVMContextRef + LLVMModuleRef through `factory.fill_module(name,
        // |handles| { ... })`; rustc owns both. Toylang's choice is Inkwell
        // for IR emission, so we wrap the raw pointers in suppressed-Drop
        // Inkwell handles inside the closure. (Sky's planned codegen uses
        // C++ via FFI instead and skips the Inkwell wrap.)
        //
        // Per @SBMNBIZ, the real Sky bodies emitted here (External
        // linkage) shadow the AvailableExternally stub bodies that rustc
        // emits via the codegen_fn_attrs override and the per_instance_mir
        // synthetic body. At every compile session where rustc emits an
        // AvailableExternally body for a Sky item, a real body MUST also
        // be emitted here OR no caller of the symbol may exist in this
        // session's IR. Step 2's emission shift relies on @F.13's gate
        // ensuring per_instance_mir doesn't fire for non-generic upstream
        // items at user_bin compile — so the AvailableExternally body
        // isn't emitted there either, and the empty IR pool is safe.

        let ts = state(s);
        ts.log.push(CallbackLog::GenerateAndCompile);

        // S.4: dump the callback log so user-bin entries (OnSkyLibLoaded etc.)
        // reach disk. Mirrors generate_and_compile's behavior. Runs in BOTH
        // user-bin and rlib compile (rlib has no Sky body codegen but still
        // emits log entries).
        //
        // §5.5 Round 2 V7/DQ-J: every entry is tagged with `[compile=rlib]`
        // or `[compile=userbin]` so tests can discriminate which compile
        // produced which entry. Defensive against any future change that
        // causes both compiles to fire entries with the same Debug shape
        // (e.g., a §5.5 revision that moves more frontend work to rlib).
        // Today entries with identical shapes between compiles would silently
        // collide; tagging forces them apart by structural prefix.
        if let Ok(path) = std::env::var("TOYLANG_LOG_PATH") {
            use std::fs::OpenOptions;
            use std::io::Write;
            let tag = if self.is_user_bin_compile { "[compile=userbin] " } else { "[compile=rlib] " };
            let lines: Vec<String> = ts.log.iter().map(|entry| format!("{}{:?}", tag, entry)).collect();
            let mut blob = lines.join("\n");
            blob.push('\n');
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("failed to open callback log for append");
            f.write_all(blob.as_bytes()).expect("failed to write callback log");
        }

        if !self.is_user_bin_compile {
            // Stub rlib compile: no Sky `.o` (Workstream A), but THIS is the
            // window where rustc's mono walker has completed its cascade
            // from Sky's `per_instance_mir` synthetic bodies — so concrete
            // monomorphizations like `<Wrapper<i32> as Clone>::clone`
            // appear in the unfiltered partition. Capture them into the
            // registry and overwrite the sidecar so the downstream binary
            // compile can:
            //   (a) pick them up at populate (Sky emits the body), and
            //   (b) synthesise `upstream_monomorphizations_for` so rustc's
            //       v0 mangler picks `__lang_stubs` as the
            //       instantiating-crate disambig (matching the stub rlib's
            //       `duplicate<Wrapper<i32>>` body's reference).
            //
            // Done HERE, not at `after_rust_analysis`, because
            // `default_collect_and_partition` triggers mono walk which
            // re-enters `collect_generic_rust_deps` — @GCMLZ. The mutex
            // is owned by `after_rust_analysis`'s trampoline; we're inside
            // `consumer_emit_modules`'s trampoline which already owns it
            // exclusively, and mono walk has long since completed.
            let mut effective_registry: ToylangRegistry = (*self.registry).clone();
            capture_discovered_trait_impl_instances(tcx, &mut effective_registry);
            // Sort for sidecar byte-determinism (see registry doc).
            effective_registry.discovered_trait_impl_instances.sort_by(|a, b| {
                let arg_key = |args: &Vec<crate::toylang::typed_ast::ResolvedType>| -> String {
                    args.iter()
                        .map(crate::oracle::resolved_type_to_mangled_name)
                        .collect::<Vec<_>>()
                        .join("__")
                };
                (&a.self_type_name, arg_key(&a.concrete_args), &a.trait_name, &a.method_name).cmp(
                    &(&b.self_type_name, arg_key(&b.concrete_args), &b.trait_name, &b.method_name),
                )
            });
            effective_registry.populate_typeid_table();
            // Overwrite the sidecar written by `after_rust_analysis` —
            // same path, now carrying discoveries.
            let sidecar_path = tcx.output_filenames(()).with_extension("sky-meta");
            let bytes = crate::sidecar::serialize_sidecar(&effective_registry)
                .unwrap_or_else(|e| panic!("[toylang] sidecar (post-discovery) serialize failed: {}", e));
            std::fs::write(&sidecar_path, &bytes).unwrap_or_else(|e| {
                panic!(
                    "[toylang] sidecar (post-discovery) write failed at {}: {}",
                    sidecar_path.display(),
                    e,
                )
            });
            if std::env::var("TOYLANG_LOG_PATH").is_ok() {
                eprintln!(
                    "[toylang] rewrote sidecar with {} discovered trait-impl instance(s): {} ({} bytes)",
                    effective_registry.discovered_trait_impl_instances.len(),
                    sidecar_path.display(),
                    bytes.len(),
                );
            }
            // §5.5 Step 3: drain the cascade-discovered trait-impl
            // instances HERE (at the stub_rlib compile that captured
            // them), not at user_bin. This retires A.1.X — the
            // capture-ship-replay layer the prior architecture used to
            // bridge cascade firing (always at a stub_rlib compile per
            // F.13) and Sky body emission (under §5.5 the same compile
            // session now). The discovered instances become
            // ToylangInstances pushed to state; fill_module emits the
            // bodies into THIS rlib's .o. Downstream compiles link
            // against them naturally — no sidecar-mediated drain at
            // user_bin needed.
            //
            // Note: state.upstream_registries[<other stub_rlib>]'s own
            // discoveries (from THAT crate's cascade) were already
            // drained at THAT crate's own compile under this same rule.
            // We don't re-drain them here.
            //
            // upstream_clones is a snapshot; constructed here to avoid
            // recomputing the merge for each entry's impl lookup.
            let upstream_clones_for_drain: Vec<crate::toylang::registry::ToylangRegistry> =
                ts.upstream_registries.values().cloned().collect();
            for inst in &effective_registry.discovered_trait_impl_instances {
                // Find the matching ToyImpl across all registries
                // (cross-Sky-crate; case6_lib pattern).
                let mut toy_impl_found = None;
                for r in std::iter::once(self.registry.as_ref()).chain(upstream_clones_for_drain.iter()) {
                    if let Some(found) = r.trait_impls.iter().find(|imp| {
                        imp.self_type_name == inst.self_type_name
                            && imp.trait_name == inst.trait_name
                    }) {
                        toy_impl_found = Some(found);
                        break;
                    }
                }
                let Some(toy_impl) = toy_impl_found else { continue };
                let Some(method) = toy_impl.methods.iter().find(|m| m.name == inst.method_name)
                    else { continue };
                let Some(stub_def_id) = crate::oracle::find_trait_impl_method_def_id(
                    tcx, &inst.trait_name, &inst.self_type_name, &inst.method_name,
                ) else { continue };
                let rustc_type_args: Vec<ty::GenericArg<'tcx>> = inst.concrete_args.iter()
                    .map(|a| ty::GenericArg::from(
                        crate::oracle::resolved_to_rustc_ty(tcx, a)
                    ))
                    .collect();
                let args = crate::oracle::build_generic_args_for_item(
                    tcx, stub_def_id, &rustc_type_args,
                );
                let instance = ty::Instance::new_raw(stub_def_id, args);
                let extern_symbol = compute_fn_symbol(&inst.method_name, tcx, instance);
                if !ts.walked_entry_points.insert(extern_symbol.clone()) {
                    continue;
                }
                let mut internal_symbol = format!(
                    "__toylang_internal__{}__{}__{}",
                    inst.self_type_name, inst.trait_name, inst.method_name,
                );
                for arg in &inst.concrete_args {
                    internal_symbol.push_str("__");
                    internal_symbol.push_str(&crate::oracle::resolved_type_to_mangled_name(arg));
                }
                let resolved_func = resolve_caller_from_instance(&method.func, instance, tcx);
                // Build an effective registry for walk_and_stash to use
                // (covers cross-Sky-crate impl-body callees).
                let effective_for_walk = {
                    let mut effective = (*self.registry).clone();
                    for upstream in &upstream_clones_for_drain {
                        for (name, func) in &upstream.functions {
                            effective.functions
                                .entry(name.clone())
                                .or_insert_with(|| func.clone());
                        }
                        for (name, st) in &upstream.structs {
                            effective.structs
                                .entry(name.clone())
                                .or_insert_with(|| st.clone());
                        }
                    }
                    effective
                };
                ts.toylang_instances.push(ToylangInstance {
                    extern_symbol,
                    internal_symbol,
                    resolved_func: resolved_func.clone(),
                    stub_def_id: Some(stub_def_id),
                    instance_args: inst.concrete_args.clone(),
                });
                walk_and_stash_internal_callees(
                    tcx, &effective_for_walk, self.registry.as_ref(),
                    &resolved_func, &inst.method_name, ts,
                );
            }
            // §5.5 Step 2: sidecar capture done. Fall through to populate +
            // fill_module — this stub_rlib compile now emits LOCAL
            // non-generic Sky bodies + the cascade-discovered trait-impl
            // method bodies (Step 3 just above) so downstream compiles
            // link against them naturally (vs the historical "everything
            // emits at user_bin via A.1.X capture-ship-replay" pattern).
        }

        self.populate_toylang_instances_from_cgus(ts, tcx);

        // No early-return on empty `toylang_instances`: the CGU walk in
        // `llvm_gen::fill_module` independently discovers Case 1b generic
        // toylang fns instantiated from Rust callers (e.g.
        // `wrap<LocalRustType>` reached only via rustc's mono walker, not
        // populate). Skipping fill_module here would lose those.

        // Same effective-registry construction as generate_and_compile.
        let effective_registry = {
            let mut effective = (*self.registry).clone();
            for upstream in ts.upstream_registries.values() {
                for (name, func) in &upstream.functions {
                    effective.functions
                        .entry(name.clone())
                        .or_insert_with(|| func.clone());
                }
                for (name, st) in &upstream.structs {
                    effective.structs
                        .entry(name.clone())
                        .or_insert_with(|| st.clone());
                }
            }
            effective
        };

        // One CGU per Sky module, named per CodegenUnitNameBuilder so it
        // doesn't clash with rustc's own CGUs.
        let cgu_name = build_sky_cgu_name(tcx);

        // Approach B: ask the facade for a fresh rustc-owned LLVM module
        // under our CGU name. Inside the closure we wrap the raw pointers
        // in suppressed-Drop Inkwell handles and hand them to llvm_gen.
        // rustc retains ownership; we return without producing any bytes.
        factory.fill_module(&cgu_name, |handles| {
            use std::mem::ManuallyDrop;
            use inkwell::context::Context;
            use inkwell::module::Module;
            let ctx_owner: ManuallyDrop<Context> = unsafe {
                ManuallyDrop::new(Context::new(
                    handles.context as inkwell::llvm_sys::prelude::LLVMContextRef,
                ))
            };
            let module_owner: ManuallyDrop<Module<'_>> = unsafe {
                Module::new_borrowed(
                    handles.module as inkwell::llvm_sys::prelude::LLVMModuleRef,
                )
            };
            let _rust_symbols = crate::llvm_gen::fill_module(
                tcx,
                &effective_registry,
                self,
                ts,
                &*ctx_owner,
                &*module_owner,
            );
        });
    }

    /// Step 5: stateless synthesis driver. Walks the discoveries stashed
    /// in `SkyUniverse` (lock-free) and for each captured
    /// `(self_type, trait, method, concrete_args)` quartet returns:
    ///   - The DefId of the impl-method (looked up via
    ///     `oracle::find_trait_impl_method_def_id` — filters on
    ///     `is_from_lang_stubs` so name collisions with stdlib impls don't
    ///     hijack — ATAFLBZ).
    ///   - The concrete `GenericArgsRef`.
    ///   - The upstream `CrateNum` matching the stashed `crate_name`.
    /// The facade's whole-map override slots each record into the
    /// `DefIdMap<UnordMap<...>>` rustc returns, augmenting the default.
    fn synthesize_upstream_monomorphizations<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
    ) -> Vec<(rustc_hir::def_id::DefId, ty::GenericArgsRef<'tcx>, rustc_span::def_id::CrateNum)> {
        let stash = rustc_lang_facade::sky_universe().discoveries_clone();
        let mut records: Vec<(
            rustc_hir::def_id::DefId,
            ty::GenericArgsRef<'tcx>,
            rustc_span::def_id::CrateNum,
        )> = Vec::new();
        for arc in &stash {
            let Some(d) = arc.downcast_ref::<StashedDiscovery>() else { continue; };
            // Look up the impl-method DefId from (self_type, trait, method).
            let Some(def_id) = crate::oracle::find_trait_impl_method_def_id(
                tcx, &d.trait_name, &d.self_type_name, &d.method_name,
            ) else { continue; };
            // Build GenericArgsRef from the stored ResolvedType list.
            let rustc_type_args: Vec<ty::GenericArg<'tcx>> = d.concrete_args.iter()
                .map(|a| ty::GenericArg::from(
                    crate::oracle::resolved_to_rustc_ty(tcx, a)
                ))
                .collect();
            let args = crate::oracle::build_generic_args_for_item(
                tcx, def_id, &rustc_type_args,
            );
            // Map stashed `crate_name` to its `CrateNum`. `tcx.crates(())`
            // returns all loaded upstream crates; match by name.
            let mut crate_num: Option<rustc_span::def_id::CrateNum> = None;
            for &c in tcx.crates(()).iter() {
                if tcx.crate_name(c).as_str() == d.crate_name {
                    crate_num = Some(c);
                    break;
                }
            }
            let Some(crate_num) = crate_num else { continue; };
            records.push((def_id, args, crate_num));
        }
        records
    }
}

/// Strip Mach-O bitcode wrapper (20-byte header) if present, returning the
/// raw bitcode that rustc's `LLVMRustParseBitcodeForLTO` accepts.
/// Option B sidecar capture (called at the stub-rlib `consumer_emit_modules`
/// gate). Walks the unfiltered partition for trait-impl method Instances and
/// records `(self_type, trait, method, concrete_args)` tuples into the
/// registry's `discovered_trait_impl_instances` list. The downstream binary
/// compile uses these for:
///   (a) populate, to push a `ToylangInstance` per discovered
///       monomorphization so Sky emits the body; and
///   (b) `lang_upstream_monomorphizations_for` synthesis (Step 5), so
///       rustc's v0 mangler picks `__lang_stubs` as the
///       instantiating-crate disambig.
///
/// Calls `default_collect_and_partition` (bypasses the in-memory query
/// cache that would return Sky's filtered result); see partition.rs and
/// Tier 3 #3 for the same pattern.
fn capture_discovered_trait_impl_instances<'tcx>(
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
    registry: &mut ToylangRegistry,
) {
    use crate::toylang::registry::DiscoveredTraitImplInstance;
    let partitions = rustc_lang_facade::default_collect_and_partition()(tcx, ());
    for cgu in partitions.codegen_units.iter() {
        for (&mono_item, _) in cgu.items() {
            let rustc_middle::mir::mono::MonoItem::Fn(instance) = mono_item else { continue };
            let def_id = instance.def_id();
            // Pre-filter on `is_from_lang_stubs` to skip items in other
            // crates (std, hashbrown, etc.). `is_consumer_trait_impl_method`
            // assumes the container is an impl block; for assoc items
            // whose container is a trait def (e.g. `hashbrown::TagSliceExt`
            // method defaults) `impl_opt_trait_ref` panics with
            // "expected Impl for DefId(...)". Sky trait-impl method bodies
            // live in the stub rlib (§6.2), so the filter is also a
            // semantic match.
            if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
                continue;
            }
            let Some((self_type_name, trait_name, method_name)) =
                rustc_lang_facade::is_consumer_trait_impl_method(tcx, def_id)
            else { continue };
            // Extract concrete type args (the impl block's type params,
            // substituted). Skip the receiver/self-type args of the
            // method itself — those are subsumed by the impl block's args.
            let concrete_args: Vec<crate::toylang::typed_ast::ResolvedType> = instance
                .args
                .iter()
                .filter_map(|a| a.as_type())
                .map(|ty| crate::oracle::rustc_ty_to_resolved_type(tcx, ty))
                .collect();
            registry.discovered_trait_impl_instances.push(DiscoveredTraitImplInstance {
                self_type_name,
                trait_name,
                method_name,
                concrete_args,
            });
        }
    }
}

/// Build a deterministic Sky CGU name that won't clash with rustc's own CGUs.
fn build_sky_cgu_name<'tcx>(tcx: TyCtxt<'tcx>) -> String {
    let mut builder = rustc_middle::mir::mono::CodegenUnitNameBuilder::new(tcx);
    builder
        .build_cgu_name(
            rustc_hir::def_id::LOCAL_CRATE,
            &[rustc_span::Symbol::intern("sky")],
            Some(rustc_span::Symbol::intern("0")),
        )
        .to_string()
}


// ============================================================================
// Toylang-specific helpers (moved from queries/mir_build.rs)
// ============================================================================

/// Convert a ResolvedType to a rustc Ty, with direct TypeParam → Ty substitution.
/// Avoids round-tripping through ResolvedType for type args from rustc.
fn resolved_to_rustc_ty_with_subst<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: &crate::toylang::typed_ast::ResolvedType,
    subst: &HashMap<&str, Ty<'tcx>>,
) -> Ty<'tcx> {
    use crate::toylang::typed_ast::ResolvedType;
    match ty {
        ResolvedType::TypeParam(name) => {
            *subst.get(name.as_str())
                .unwrap_or_else(|| panic!("type param '{}' not in subst", name))
        }
        ResolvedType::StructRef { name, type_args }
        | ResolvedType::Struct { name, type_args, .. } => {
            let def_id = crate::oracle::find_local_struct_def_id(tcx, name)
                .unwrap_or_else(|| panic!("struct '{}' not found", name));
            let adt_def = tcx.adt_def(def_id);
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty_with_subst(tcx, ta, subst)))
                .collect();
            Ty::new_adt(tcx, adt_def, tcx.mk_args(&args))
        }
        ResolvedType::RustType { name, type_args } => {
            let def_id = crate::oracle::find_rust_type_def_id(tcx, name)
                .unwrap_or_else(|| panic!("Rust type '{}' not found", name));
            let adt_def = tcx.adt_def(def_id);
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty_with_subst(tcx, ta, subst)))
                .collect();
            Ty::new_adt(tcx, adt_def, tcx.mk_args(&args))
        }
        // Non-parameterized types delegate to the standard conversion
        other => crate::oracle::resolved_to_rustc_ty(tcx, other),
    }
}

/// Resolve a ToyFunction for a concrete rustc Instance by substituting type params.
///
/// Compiler-law: no branch on param count. For N=0 the zip yields
/// nothing → empty subst → `resolve_caller_from_type_args` runs
/// `substitute_type_params` with an empty map (identity) and returns a
/// caller_fn-equivalent. The zero-param case falls out of the general path.
pub fn resolve_caller_from_instance<'tcx>(
    caller_fn: &crate::toylang::registry::ToyFunction,
    instance: ty::Instance<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> crate::toylang::registry::ToyFunction {
    let subst: std::collections::HashMap<String, crate::toylang::typed_ast::ResolvedType> =
        caller_fn.type_params.iter()
            .zip(instance.args.types())
            .map(|(param_name, ty)| {
                (param_name.clone(), crate::oracle::rustc_ty_to_resolved_type(tcx, ty))
            })
            .collect();
    resolve_caller_from_type_args(caller_fn, &subst)
}

// `resolve_caller_from_identity_args` retired in W3 (course-correct.md item #1
// Approach A restoration). Its sole caller was `collect_generic_rust_deps_inner`'s
// identity-args path; under Approach A that function uses
// `resolve_caller_from_instance` with concrete args directly.

/// Resolve a ToyFunction by substituting type params with concrete ResolvedTypes.
fn resolve_caller_from_type_args(
    caller_fn: &crate::toylang::registry::ToyFunction,
    subst: &std::collections::HashMap<String, crate::toylang::typed_ast::ResolvedType>,
) -> crate::toylang::registry::ToyFunction {
    crate::toylang::registry::ToyFunction {
        type_params: vec![],
        params: caller_fn.params.iter().map(|p| crate::toylang::registry::ToyParam {
            name: p.name.clone(),
            ty: crate::toylang::type_resolve::substitute_type_params(&p.ty, subst),
        }).collect(),
        return_ty: caller_fn.return_ty.as_ref()
            .map(|rt| crate::toylang::type_resolve::substitute_type_params(rt, subst)),
        body: caller_fn.body.as_ref().map(|b| {
            crate::toylang::type_resolve::substitute_type_params_in_body(b, subst)
        }),
        // Substituted callee inherits the original's export status.
        is_export: caller_fn.is_export,
    }
}

/// Compute a Sky-internal symbol from a function name and ResolvedType type args.
///
/// Used during the deep walker (Sky→Sky callees) where we don't have a rustc
/// Instance. These items are NEVER referenced by rustc-emitted code — only
/// Sky's own bitcode calls them — so we choose a Sky-internal name and don't
/// need to match anything rustc would emit. The wrapper loop in llvm_gen skips
/// them (no `stub_def_id` ⇒ no `Instance` ⇒ no extern wrapper); they're
/// emitted only via the internal-function loop.
pub fn compute_internal_symbol_from_type_args(
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> String {
    let mut sym = format!("__toylang_internal_{}", name);
    for ta in type_args {
        sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(ta)));
    }
    sym
}

/// Type-resolve a consumer function body given its already-substituted
/// `ToyFunction`. Shared primitive for both walkers below; kept read-only
/// with respect to `ToylangState`.
fn type_resolve_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    resolved_fn: &crate::toylang::registry::ToyFunction,
    fn_name: &str,
) -> crate::toylang::typed_ast::TypedBlock {
    let rust_method_ret = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<crate::toylang::typed_ast::ResolvedType, crate::oracle::UnresolvedRustType> {
        if type_name.is_empty() {
            crate::oracle::rust_free_fn_return_type(tcx, method, type_args)
                .map(|opt| opt.unwrap_or(crate::toylang::typed_ast::ResolvedType::Void))
        } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
            let receiver_ty = &type_args[0];
            let explicit_args = &type_args[1..];
            crate::oracle::rust_trait_method_return_type(tcx, trait_name, method, receiver_ty, explicit_args)
        } else {
            crate::oracle::rust_method_return_type(tcx, type_name, method, type_args)
        }
    };
    let rust_param_types = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, crate::oracle::UnresolvedRustType> {
        if type_name.is_empty() {
            crate::oracle::rust_free_fn_param_types(tcx, method, type_args)
        } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
            crate::oracle::rust_trait_method_param_types(tcx, trait_name, method, &type_args[0], &type_args[1..])
        } else {
            crate::oracle::rust_method_param_types(tcx, type_name, method, type_args)
        }
    };
    // Per @IVTDBTZ, trait-vs-inherent dispatch predicate — asks the oracle
    // directly whether `name` is a `use`-imported Rust trait.
    let is_rust_trait = |name: &str| {
        crate::oracle::find_use_imported_trait_def_id(tcx, name).is_some()
    };
    crate::toylang::type_resolve::resolve_fn_body(registry, resolved_fn, &rust_method_ret, &rust_param_types, &is_rust_trait)
        .unwrap_or_else(|e| panic!("[toylang] type error in '{}': {:?}", fn_name, e))
}

/// Substitute a toylang callee's body given call-site type args.
///
/// Compiler-law: no branch on param count. For N=0 the zip yields
/// nothing → empty subst → `resolve_caller_from_type_args` returns a
/// callee_fn-equivalent. The zero-param case falls out of the general path.
fn resolve_toylang_callee(
    callee_fn: &crate::toylang::registry::ToyFunction,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> crate::toylang::registry::ToyFunction {
    let subst: std::collections::HashMap<String, crate::toylang::typed_ast::ResolvedType> =
        callee_fn.type_params.iter().zip(type_args.iter())
            .map(|(param, arg)| (param.clone(), arg.clone()))
            .collect();
    resolve_caller_from_type_args(callee_fn, &subst)
}

/// Walker A: collect the transitive Rust deps of a consumer function body.
/// Recurses into consumer→consumer callees with a local cycle guard; does NOT
/// mutate `ToylangState`.
///
/// Per @SMINCZ, each returned `(def_id, args)` pair is what ends up as a
/// `ReifyFnPointer` cast inside the `optimized_mir` override's synthesized
/// body. That's the mechanism that forces rustc's mono collector to emit the
/// Rust symbol.
/// `llvm_gen.rs`'s `tcx.symbol_name` reads are only valid if the matching dep
/// was registered here first.
fn collect_rust_deps_recursive<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    resolved_fn: &crate::toylang::registry::ToyFunction,
    fn_name: &str,
    cycle_guard: &mut HashSet<String>,
) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
    let _body = resolved_fn.body.as_ref()
        .expect("collect_rust_deps_recursive called on extern fn");

    let typed_body = type_resolve_body(tcx, registry, resolved_fn, fn_name);

    let mut deps = Vec::new();
    let mut fn_calls = Vec::new();
    let mut rust_method_deps = Vec::new();
    walk_typed_body_for_deps(&typed_body, &mut fn_calls, &mut rust_method_deps);

    for (callee_name, type_args) in &fn_calls {
        let Some(callee_fn) = registry.functions.get(callee_name.as_str()) else {
            // Not a toylang fn — use-imported free function.
            if let Some(def_id) = crate::oracle::find_use_imported_fn_def_id(tcx, callee_name) {
                let ty_arg_refs: Vec<ty::GenericArg<'_>> = type_args.iter()
                    .map(|ta| ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(tcx, ta)))
                    .collect();
                // @ELASZ
                let args = crate::oracle::build_generic_args_for_item(tcx, def_id, &ty_arg_refs);
                deps.push((def_id, args));
            }
            continue;
        };

        if callee_fn.body.is_some() {
            // Toylang callee — recurse to find its transitive Rust deps. Use
            // the local cycle guard to avoid infinite loops on cyclic code;
            // do NOT share with `walked_entry_points` (that's for the stashing
            // walker) — two entry points reaching the same internal helper
            // must each collect its deps, since rustc's collector dedups Rust
            // items independently.
            let callee_symbol = compute_internal_symbol_from_type_args(callee_name, type_args);
            if cycle_guard.insert(callee_symbol) {
                let resolved_callee = resolve_toylang_callee(callee_fn, type_args);
                let transitive_deps = collect_rust_deps_recursive(
                    tcx, registry, &resolved_callee, callee_name, cycle_guard,
                );
                deps.extend(transitive_deps);
            }
        } else {
            // Extern function — report to rustc
            let Some(def_id) = crate::oracle::find_extern_fn_def_id(tcx, callee_name) else { continue };
            let args = tcx.mk_args(&[]);
            deps.push((def_id, args));
        }
    }

    for dep in &rust_method_deps {
        if let Some(receiver_ty) = &dep.receiver_ty {
            // Trait static call: look up trait, find impl for receiver type
            if let Some(trait_def_id) = crate::oracle::find_use_imported_trait_def_id(tcx, &dep.type_name) {
                let self_resolved = crate::oracle::strip_ref(receiver_ty);
                let self_ty = crate::oracle::resolved_to_rustc_ty(tcx, self_resolved);

                // Per @TVIMDGAZ, use trait definition method DefId with [Self, ...] args
                let trait_method_def_id = tcx.associated_item_def_ids(trait_def_id)
                    .iter()
                    .find(|&&id| tcx.item_name(id).as_str() == dep.method_name)
                    .copied()
                    .unwrap_or_else(|| panic!("method '{}' not defined on trait '{}'", dep.method_name, dep.type_name));

                let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
                for ta in &dep.type_args {
                    all_ty_args.push(ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(tcx, ta)));
                }
                // @ELASZ
                let args = crate::oracle::build_generic_args_for_item(tcx, trait_method_def_id, &all_ty_args);
                deps.push((trait_method_def_id, args));
                continue;
            }
            // Fall through to inherent method lookup if trait not found
        }

        // Phase 6: redirect to wrapper if applicable. The wrapper Instance
        // (not the inline stdlib method) lands in rust_deps so the
        // `optimized_mir` override reifies a fn-pointer to it, forcing
        // rustc's mono collector to codegen the wrapper. Without this,
        // `Option::unwrap` and friends produce no callable symbol.
        if let Some((wdef, wargs)) = crate::oracle::redirect_to_wrapper(
            tcx, &dep.type_name, &dep.method_name, &dep.type_args,
        ) {
            deps.push((wdef, wargs));
            continue;
        }

        // Inherent method call
        let type_def_id = crate::oracle::find_rust_type_def_id(tcx, &dep.type_name)
            .unwrap_or_else(|| panic!("Rust type '{}' not found", dep.type_name));
        let method_def_id = crate::oracle::find_inherent_method(tcx, type_def_id, &dep.method_name)
            .unwrap_or_else(|| panic!("method '{}' not found on '{}'", dep.method_name, dep.type_name));

        let all_ty_args: Vec<ty::GenericArg<'tcx>> = dep.type_args.iter()
            .map(|ta| ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(tcx, ta)))
            .collect();
        // @ELASZ
        let args = crate::oracle::build_generic_args_for_item(tcx, method_def_id, &all_ty_args);
        deps.push((method_def_id, args));
    }

    deps
}

/// Walker B: stash consumer→consumer internal callees into
/// `state.toylang_instances` so `generate_and_compile` can emit them. Recurses
/// transitively using `state.walked_entry_points` as persistent dedup so
/// shared callees are stashed exactly once per compilation. Ignores Rust
/// dependencies — those flow through `collect_rust_deps_recursive` instead.
///
/// §5.5 Step 1 + Step 2 interaction: `registry` is the EFFECTIVE
/// registry (LOCAL + all UPSTREAMs merged) so cross-Sky-crate callees
/// are found. `local_registry` is just LOCAL — used to discriminate
/// "we own this callee" vs "upstream owns this callee". Under Step 2's
/// owning-crate-emits rule, we skip emitting non-generic upstream-owned
/// callees (they already live in their owning crate's stub_rlib.rlib);
/// we still emit generic-with-concrete-args upstream-owned callees
/// (those couldn't be pre-emitted upstream without the args).
fn walk_and_stash_internal_callees<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    local_registry: &ToylangRegistry,
    resolved_fn: &crate::toylang::registry::ToyFunction,
    fn_name: &str,
    state: &mut ToylangState,
) {
    let _body = resolved_fn.body.as_ref()
        .expect("walk_and_stash_internal_callees called on extern fn");

    let typed_body = type_resolve_body(tcx, registry, resolved_fn, fn_name);

    let mut fn_calls = Vec::new();
    let mut rust_method_deps = Vec::new();
    walk_typed_body_for_deps(&typed_body, &mut fn_calls, &mut rust_method_deps);

    for (callee_name, type_args) in &fn_calls {
        let Some(callee_fn) = registry.functions.get(callee_name.as_str()) else {
            continue; // Rust free fn — not our concern.
        };
        if callee_fn.body.is_none() {
            continue; // Extern fn — not our concern.
        }
        // §5.5 Step 1+2: discriminate owning-crate. If the callee is
        // defined in an upstream Sky crate AND the call-site args are
        // fully concrete from upstream's perspective, the upstream's
        // stub_rlib compile has already eagerly emitted the body. Skip
        // to avoid duplicate definitions (fat LTO catches duplicates
        // with an IR linker error; non-LTO would silently pick one).
        // Generic callees with downstream-supplied concrete args still
        // emit here — the upstream couldn't pre-emit without knowing
        // args.
        let is_local_owned = local_registry.functions.contains_key(callee_name.as_str());
        let upstream_could_have_emitted = type_args.is_empty(); // arch-fence-allow: Step 1/2 cross-crate emission discrimination — "could the upstream have pre-emitted this body?" falls out as "no args needed", i.e. the upstream's eager-emit path covers it.
        if !is_local_owned && upstream_could_have_emitted {
            continue;
        }
        let callee_symbol = compute_internal_symbol_from_type_args(callee_name, type_args);
        if state.walked_entry_points.insert(callee_symbol.clone()) {
            let resolved_callee = resolve_toylang_callee(callee_fn, type_args);
            // Compiler-law audit B1: pass `type_args` (captured at the call
            // site) as `instance_args` rather than hardcoding `vec![]`.
            // For non-generic callees `type_args` is empty (degenerate case
            // of the general path); for generic callees it carries the
            // concrete instantiation so any downstream codegen that builds
            // a rustc Instance via `build_generic_args_for_item` produces
            // the right args. Today `stub_def_id: None` means the Instance
            // is never reconstructed for transitive callees, but threading
            // the args here makes the unification hold structurally —
            // i.e. the next time we need `stub_def_id` for an internal-
            // only generic chain (e.g. ABI lookup), the args are present.
            state.toylang_instances.push(ToylangInstance {
                extern_symbol: callee_symbol.clone(),
                internal_symbol: callee_symbol,
                resolved_func: resolved_callee.clone(),
                stub_def_id: None,
                instance_args: type_args.clone(),
            });
            walk_and_stash_internal_callees(
                tcx, registry, local_registry, &resolved_callee, callee_name, state,
            );
        }
    }
}

/// A Rust method dependency: (type_name, method_name, type_args of the receiver's RustType)
struct RustMethodDep {
    type_name: String,
    method_name: String,
    type_args: Vec<crate::toylang::typed_ast::ResolvedType>,
    /// For trait static calls (e.g., Write::write_all(&out, ...)), the receiver type.
    /// None for inherent static calls and method calls.
    receiver_ty: Option<crate::toylang::typed_ast::ResolvedType>,
}

/// Walk a TypedBlock and collect toylang FnCall deps and Rust method deps.
fn walk_typed_body_for_deps(
    body: &crate::toylang::typed_ast::TypedBlock,
    fn_calls: &mut Vec<(String, Vec<crate::toylang::typed_ast::ResolvedType>)>,
    rust_method_deps: &mut Vec<RustMethodDep>,
) {
    use crate::toylang::typed_ast::*;
    for stmt in &body.stmts {
        match stmt {
            TypedStmt::Let { expr, .. } => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
            TypedStmt::ExprStmt(expr) => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
            TypedStmt::While { cond, body } => {
                walk_typed_expr_for_deps(cond, fn_calls, rust_method_deps);
                walk_typed_body_for_deps(body, fn_calls, rust_method_deps);
            }
            TypedStmt::Assign { expr, .. } => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
        }
    }
    if let Some(ref ret) = body.ret {
        walk_typed_expr_for_deps(ret, fn_calls, rust_method_deps);
    }
}

fn walk_typed_expr_for_deps(
    expr: &crate::toylang::typed_ast::TypedExpr,
    fn_calls: &mut Vec<(String, Vec<crate::toylang::typed_ast::ResolvedType>)>,
    rust_method_deps: &mut Vec<RustMethodDep>,
) {
    use crate::toylang::typed_ast::*;
    match &expr.kind {
        TypedExprKind::FnCall { name, type_args, args } => {
            fn_calls.push((name.clone(), type_args.clone()));
            for arg in args {
                walk_typed_expr_for_deps(arg, fn_calls, rust_method_deps);
            }
        }
        TypedExprKind::StructLit { fields, .. } => {
            for (_, expr) in fields {
                walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps);
            }
        }
        TypedExprKind::MethodCall { receiver, method, args } => {
            // Check if receiver is a Rust type (direct or via &ref)
            let rust_type_info = match &receiver.ty {
                ResolvedType::RustType { name, type_args } => Some((name.clone(), type_args.clone())),
                ResolvedType::Ref { inner } => match inner.as_ref() {
                    ResolvedType::RustType { name, type_args } => Some((name.clone(), type_args.clone())),
                    _ => None,
                },
                _ => None,
            };
            if let Some((type_name, type_args)) = rust_type_info {
                rust_method_deps.push(RustMethodDep {
                    type_name,
                    method_name: method.clone(),
                    type_args,
                    receiver_ty: None,
                });
            }
            walk_typed_expr_for_deps(receiver, fn_calls, rust_method_deps);
            for arg in args {
                walk_typed_expr_for_deps(arg, fn_calls, rust_method_deps);
            }
        }
        TypedExprKind::FieldAccess { receiver, .. } => {
            walk_typed_expr_for_deps(receiver, fn_calls, rust_method_deps);
        }
        TypedExprKind::BinaryOp { left, right, .. } => {
            walk_typed_expr_for_deps(left, fn_calls, rust_method_deps);
            walk_typed_expr_for_deps(right, fn_calls, rust_method_deps);
        }
        TypedExprKind::StaticCall { ty, method, type_args, args } => {
            // Static call on a Rust type (e.g. Vec::new) or trait (e.g. Write::write_all)
            let receiver_ty = args.first().map(|a| a.ty.clone());
            rust_method_deps.push(RustMethodDep {
                type_name: ty.clone(),
                method_name: method.clone(),
                type_args: type_args.clone(),
                receiver_ty,
            });
            for arg in args {
                walk_typed_expr_for_deps(arg, fn_calls, rust_method_deps);
            }
        }
        TypedExprKind::If { cond, then_stmts, then_expr, else_stmts, else_expr } => {
            walk_typed_expr_for_deps(cond, fn_calls, rust_method_deps);
            for stmt in then_stmts {
                match stmt {
                    TypedStmt::Let { expr, .. } | TypedStmt::Assign { expr, .. } => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
                    TypedStmt::ExprStmt(expr) => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
                    TypedStmt::While { cond, body } => {
                        walk_typed_expr_for_deps(cond, fn_calls, rust_method_deps);
                        walk_typed_body_for_deps(body, fn_calls, rust_method_deps);
                    }
                }
            }
            if let Some(e) = then_expr { walk_typed_expr_for_deps(e, fn_calls, rust_method_deps); }
            for stmt in else_stmts {
                match stmt {
                    TypedStmt::Let { expr, .. } | TypedStmt::Assign { expr, .. } => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
                    TypedStmt::ExprStmt(expr) => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
                    TypedStmt::While { cond, body } => {
                        walk_typed_expr_for_deps(cond, fn_calls, rust_method_deps);
                        walk_typed_body_for_deps(body, fn_calls, rust_method_deps);
                    }
                }
            }
            if let Some(e) = else_expr { walk_typed_expr_for_deps(e, fn_calls, rust_method_deps); }
        }
        TypedExprKind::Ref(inner) => {
            walk_typed_expr_for_deps(inner, fn_calls, rust_method_deps);
        }
        _ => {} // IntLit, BoolLit, Var, StringLit, ByteStringLit — no children
    }
}

/// Find a function's DefId in __lang_stubs by name.
///
/// Walks the local crate's items and filters by `is_from_lang_stubs`.
/// Under the two-crate architecture (stage 5b onwards) this only
/// resolves during the stub rlib's own compile — where the items are
/// at LOCAL_CRATE's root and LOCAL_CRATE is `__lang_stubs`. In the
/// user-bin compile the stub items aren't local, so this walker
/// returns `None` and callers fall back to cross-crate resolution via
/// the facade's extern-crate walker. `is_from_lang_stubs`'s simple
/// crate-name check is safe from any phase; see `@DPSFDOZ` for why
/// the former structural-walk `is_from_lang_stubs_safe` helper was
/// unnecessary once the two-crate shape made the stub rlib always its
/// own compilation unit.
fn find_stub_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<rustc_span::def_id::DefId> {
    use rustc_hir::def::DefKind;
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if !matches!(tcx.def_kind(def_id), DefKind::Fn) {
            continue;
        }
        if tcx.item_name(def_id).as_str() != name {
            continue;
        }
        if rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
    }
    None
}

/// Recursively collect all RustType names referenced in a ResolvedType.
fn collect_rust_type_names(ty: &crate::toylang::typed_ast::ResolvedType) -> Vec<String> {
    use crate::toylang::typed_ast::ResolvedType;
    let mut names = Vec::new();
    match ty {
        ResolvedType::RustType { name, type_args } => {
            names.push(name.clone());
            for ta in type_args { names.extend(collect_rust_type_names(ta)); }
        }
        ResolvedType::StructRef { type_args, .. } | ResolvedType::Struct { type_args, .. } => {
            for ta in type_args { names.extend(collect_rust_type_names(ta)); }
        }
        ResolvedType::Ref { inner } => { names.extend(collect_rust_type_names(inner)); }
        _ => {}
    }
    names
}

/// Path B: compute the rustc-default mangled symbol for a consumer Instance.
///
/// Used at populate sites where Sky's bitcode emission needs to match the
/// symbol rustc would have given the stub fn — so call sites and definition
/// share one symbol. Calls `default_symbol_name()` directly (not
/// `tcx.symbol_name(...)`) to dodge re-entrance through the facade's
/// `lang_symbol_name` override.
///
/// The `_name` parameter (registry name) is unused for naming but kept for
/// observability / future log entries.
pub fn compute_fn_symbol<'tcx>(_name: &str, tcx: TyCtxt<'tcx>, instance: ty::Instance<'tcx>) -> String {
    let default = rustc_lang_facade::default_symbol_name();
    default(tcx, instance).name.to_string()
}


