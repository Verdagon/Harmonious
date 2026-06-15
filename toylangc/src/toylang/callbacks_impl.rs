//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};

use rustc_lang_facade::{LangCallbacks, LangPredicates, MonomorphizeTypeResult};
use crate::toylang::registry::ToylangRegistry;

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
    GenerateAndCompile,
}

/// A toylang function instance discovered during the deep monomorphization walk.
///
/// `stub_def_id` carries the rustc DefId of the `pub fn <name>` shell in the
/// `__lang_stubs` rlib's source. It's `Some` when populated at the user-bin
/// compile via Workstream A's registry-driven discovery path; the codegen
/// pass uses it to construct an `Instance::new_raw(def_id, empty_args)` for
/// ABI-aware extern-wrapper emission (`__toylang_impl_*`). At the rlib
/// compile this field stays `None` (CGU-walk-driven discovery surfaces
/// real `Instance`s directly, and the old code path didn't need this).
#[derive(Clone)]
pub struct ToylangInstance {
    pub extern_symbol: String,
    pub resolved_func: crate::toylang::registry::ToyFunction,
    pub stub_def_id: Option<rustc_span::def_id::DefId>,
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
    /// (ll_path, obj_path) for LLVM compilation. None if no external codegen.
    pub llvm_paths: Option<(PathBuf, PathBuf)>,
    /// Phase 3 E.6: names of body-bearing functions defined in UPSTREAM
    /// Sky-marked rlibs (loaded via S.4's `on_sky_lib_loaded`). The
    /// `is_consumer_fn` / `is_consumer_type` predicates are called from
    /// `symbol_name` and other queries that only see `&self` (no access
    /// to `ToylangState`), so we mirror the relevant subset of upstream
    /// state into this set as upstreams arrive.
    pub upstream_fn_names: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Same as `upstream_fn_names` but for consumer types (struct names).
    pub upstream_type_names: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
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
        let toy_fn = self.registry.functions.get(registry_name)
            .unwrap_or_else(|| panic!("[toylang] collect_generic_rust_deps: function '{}' not in registry", registry_name));

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
            tcx, &self.registry, &resolved_caller, registry_name, &mut cycle_guard,
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
    /// Called from the trait impl (holding the facade mutex) and also directly
    /// from `generate_with_tcx` for accessor symbol lookup (already inside
    /// `generate_and_compile`'s lock; see @GCMLZ).
    pub fn notify_concrete_entry_point_inner<'tcx>(
        &self,
        state: &mut ToylangState,
        name: &str,
        tcx: TyCtxt<'tcx>,
        _def_id: rustc_span::def_id::DefId,
        instance: ty::Instance<'tcx>,
    ) -> String {
        state.log.push(CallbackLog::NotifyConcreteEntryPoint { name: name.to_string() });

        // Accessor methods come in as "StructName.field_name"
        if let Some((struct_name, field_name)) = name.split_once('.') {
            let mut sym = format!("__toylang_accessor_{}_{}", struct_name, field_name);
            for arg in instance.args.iter() {
                if let ty::GenericArgKind::Type(ty) = arg.kind() {
                    let resolved = crate::oracle::rustc_ty_to_resolved_type(tcx, ty);
                    sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(&resolved)));
                }
            }
            return sym;
        }

        let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main" } else { name };
        compute_fn_symbol(registry_name, tcx, instance)
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
    /// Gated on `self.is_user_bin_compile` (Workstream A inversion): the
    /// rlib compile no longer owns consumer codegen. The user-bin compile
    /// is the sole codegen site. The rlib compile's `generate_and_compile`
    /// short-circuits via `llvm_paths = None` before reaching this populate.
    pub fn populate_toylang_instances_from_cgus<'tcx>(
        &self,
        state: &mut ToylangState,
        tcx: TyCtxt<'tcx>,
    ) {
        if !self.is_user_bin_compile {
            return;
        }

        // Workstream A discovery path + Phase 3 E.6: under the architecture's
        // locked "binary compile codegens every reachable Sky item across
        // all libs" invariant (rust-interop-architecture.md §5.5, §9.6),
        // at user-bin time rustc does NOT queue extern non-generic items
        // for local monomorphization, so the CGU walk finds zero stub
        // items. We iterate the LOCAL registry first, then EACH upstream
        // registry loaded by S.4. Upstream registries are cloned upfront
        // so we hold one immutable borrow for the outer iteration while
        // mutating `state` for the inner walk-and-stash.
        let upstream_clones: Vec<crate::toylang::registry::ToylangRegistry> =
            state.upstream_registries.values().cloned().collect();
        let registries: Vec<&crate::toylang::registry::ToylangRegistry> =
            std::iter::once(self.registry.as_ref())
                .chain(upstream_clones.iter())
                .collect();
        for reg in registries {
        for (name, toy_fn) in &reg.functions {
            if toy_fn.body.is_none() {
                continue;
            }
            // Skip generic consumer fns: emitting them from a registry-only
            // walk would require concrete type args to substitute, but those
            // come from caller-site Instances which the user-bin CGU list
            // doesn't surface for extern non-generic items either. Generic
            // surface to rustc is course-correct.md item #17 — out of scope
            // for Workstream A. The 5 `test_generic_*` integration tests are
            // expected to fail until that lands.
            if !toy_fn.type_params.is_empty() {
                continue;
            }
            let extern_symbol = compute_fn_symbol_from_type_args(name, &[]);
            if !state.walked_entry_points.insert(extern_symbol.clone()) {
                continue;
            }
            // Look up the `pub fn` shell in the upstream `__lang_stubs`
            // rlib so the codegen pass can construct an `Instance` and
            // emit the Rust-ABI extern wrapper. The stub's name uses
            // toylang's mangling convention — `main` becomes
            // `__toylang_main` (per `oracle::TOYLANG_MAIN`), everything
            // else stays as-is.
            let stub_name = if name == "main" {
                crate::oracle::TOYLANG_MAIN.to_string()
            } else {
                name.clone()
            };
            let stub_def_id = crate::oracle::find_stub_fn_in_stub_rlib(tcx, &stub_name);
            state.toylang_instances.push(ToylangInstance {
                extern_symbol,
                resolved_func: toy_fn.clone(),
                stub_def_id,
            });
            // Transitively walk the body for consumer-to-consumer callees.
            // This is what surfaces generic monomorphizations (e.g.
            // `wrap<i32>` called from non-generic `wrap_i32`) — the
            // registry walk above sees `wrap_i32` but the
            // `wrap<i32>::<__toylang_internal_wrap__i32>` symbol it calls
            // needs to be emitted too, and the only place it appears with
            // concrete args is at the call site.
            walk_and_stash_internal_callees(
                tcx, reg, toy_fn, name, state,
            );
        }
        }
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

impl LangPredicates for ToylangCallbacks {
    fn is_consumer_type(&self, name: &str) -> bool {
        if self.registry.structs.contains_key(name) { return true; }
        // Phase 3 E.6: also accept types defined in upstream Sky libs.
        self.upstream_type_names.lock().unwrap().contains(name)
    }

    fn is_consumer_fn(&self, name: &str) -> bool {
        if name == crate::oracle::TOYLANG_MAIN {
            return self.registry.functions.get("main").map_or(false, |f| f.body.is_some());
        }
        if self.registry.functions.get(name).map_or(false, |f| f.body.is_some()) {
            return true;
        }
        // Phase 3 E.6: also accept body-bearing fns from upstream Sky libs.
        self.upstream_fn_names.lock().unwrap().contains(name)
    }

    // Stage 4c retired the `visibility_override` trait method: the
    // facade's partitioner override now forces `(External, Default)` on
    // `__lang_stubs` items directly in the CGU slice. No consumer-side
    // predicate needed.
    //
    // Stage 5c.4 retired the `generate_stubs` trait method: wrapper mode's
    // `build::write_stub_crate` calls `stub_gen::generate` directly when
    // writing the stub rlib's `src/lib.rs`. No facade-level callback.
}

impl LangCallbacks for ToylangCallbacks {
    fn create_state(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(ToylangState::default())
    }

    fn after_rust_analysis<'tcx>(&self, s: &mut dyn Any, tcx: TyCtxt<'tcx>) {
        state(s).log.push(CallbackLog::AfterRustAnalysis);

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

        // Check 2: Every toylang function with a body has a stub
        for (name, func) in &self.registry.functions {
            if func.body.is_none() { continue; }
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
            if func.body.is_none() || !func.type_params.is_empty() { continue; }
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

        if !errors.is_empty() {
            eprintln!("[toylang] validation failed with {} error(s):", errors.len());
            for e in &errors {
                eprintln!("  - {}", e);
            }
            panic!("[toylang] aborting due to validation errors");
        }

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
        let bytes = crate::sidecar::serialize_sidecar(&self.registry)
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
        {
            let mut fns = self.upstream_fn_names.lock().unwrap();
            for (name, func) in &registry.functions {
                if func.body.is_some() { fns.insert(name.clone()); }
            }
        }
        {
            let mut tys = self.upstream_type_names.lock().unwrap();
            for name in registry.structs.keys() { tys.insert(name.clone()); }
        }
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
        let toy_struct = self.registry.structs.get(name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_type: struct '{}' not in registry", name));

        // Build type-param substitution at the rustc Ty level (no round-trip through ResolvedType).
        let ty_subst: HashMap<&str, Ty<'tcx>> = if !toy_struct.type_params.is_empty() {
            if let TyKind::Adt(_, args) = ty.kind() {
                toy_struct.type_params.iter()
                    .enumerate()
                    .map(|(i, name)| (name.as_str(), args[i].expect_ty()))
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

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

    fn notify_concrete_entry_point<'tcx>(
        &self,
        s: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> String {
        self.notify_concrete_entry_point_inner(state(s), name, tcx, instance.def_id(), instance)
    }

    fn generate_and_compile<'tcx>(&self, s: &mut dyn Any, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)> {
        let ts = state(s);
        ts.log.push(CallbackLog::GenerateAndCompile);

        // B6 fix: populate state.toylang_instances from the CGU list + a
        // transitive internal-callee walk, up front and deterministically.
        // Prior art accumulated state as a side effect of the per-item
        // `notify_concrete_entry_point` query firing — which rustc's
        // incremental cache could skip on cache hit, leaving state empty
        // and the emitted `.o` symbol-less. See risks.md §B6 for the
        // full diagnosis and risks.md §B6 RESOLVED marker for this fix.
        // Workstream A inverted the gate: short-circuits on the RLIB
        // compile (produces only stubs + sidecar); runs on USER-BIN
        // (the codegen site).
        self.populate_toylang_instances_from_cgus(ts, tcx);

        // S.4: dump the log BEFORE the `llvm_paths` early-return so
        // user-bin compiles (where `llvm_paths` is None) also surface
        // their entries (specifically `OnSkyLibLoaded` from the S.4
        // sidecar load). Append mode rather than overwrite so the rlib
        // compile's earlier entries (CollectGenericRustDeps for main,
        // NotifyConcreteEntryPoint, etc.) survive when the user-bin
        // compile runs second within the same `toylangc build`. Each
        // test wipes the build dir before invoking toylangc so the
        // append never inherits cross-test bleed-over.
        if let Ok(path) = std::env::var("TOYLANG_LOG_PATH") {
            use std::fs::OpenOptions;
            use std::io::Write;
            let lines: Vec<String> = ts.log.iter().map(|entry| format!("{:?}", entry)).collect();
            let mut blob = lines.join("\n");
            blob.push('\n');
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("failed to open callback log for append");
            f.write_all(blob.as_bytes()).expect("failed to write callback log");
        }

        let (ref ll_path, ref obj_path) = self.llvm_paths.as_ref()?;

        // Phase 3 E.6: build an effective registry merging local + upstream
        // so codegen treats upstream consumer fns (e.g. case6_lib's double_it
        // when compiling case6_app's user-bin) as toylang fns rather than
        // Rust extern decls. Without this, the FnCall codegen path at
        // `llvm_gen.rs:1161` sees `double_it` missing from the local
        // registry and emits a `declare i32 @__toylang_impl_double_it(i32)`
        // alongside the populate-iteration loop's `define i32 @__toylang_impl_double_it(...)`,
        // and LLVM disambiguates the latter to `.1` — leaving the
        // unmangled symbol undefined at link time.
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

        // Walk MonoItems and codegen each consumer instance inline (same 'tcx scope).
        let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(
            tcx, &effective_registry, self, ts,
        );
        std::fs::write(ll_path, &llvm_ir)
            .expect("toylang: failed to write .ll file");
        eprintln!("[toylang] compiling LLVM IR: {} → {}", ll_path.display(), obj_path.display());
        crate::compile_llvm_ir(ll_path, obj_path);

        Some((obj_path.clone(), rust_symbols))
    }
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
pub fn resolve_caller_from_instance<'tcx>(
    caller_fn: &crate::toylang::registry::ToyFunction,
    instance: ty::Instance<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> crate::toylang::registry::ToyFunction {
    if caller_fn.type_params.is_empty() {
        return caller_fn.clone();
    }
    let mut subst = std::collections::HashMap::new();
    for (i, param_name) in caller_fn.type_params.iter().enumerate() {
        if let Some(arg) = instance.args.get(i) {
            if let ty::GenericArgKind::Type(ty) = arg.kind() {
                subst.insert(param_name.clone(), crate::oracle::rustc_ty_to_resolved_type(tcx, ty));
            }
        }
    }
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
    }
}

/// Compute an extern symbol from a function name and ResolvedType type args.
/// Used during the deep walk where we don't have a rustc Instance.
fn compute_fn_symbol_from_type_args(
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> String {
    let mut sym = format!("__toylang_impl_{}", name);
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
fn resolve_toylang_callee(
    callee_fn: &crate::toylang::registry::ToyFunction,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> crate::toylang::registry::ToyFunction {
    if callee_fn.type_params.is_empty() {
        callee_fn.clone()
    } else {
        let subst: std::collections::HashMap<String, crate::toylang::typed_ast::ResolvedType> =
            callee_fn.type_params.iter().zip(type_args.iter())
                .map(|(param, arg)| (param.clone(), arg.clone()))
                .collect();
        resolve_caller_from_type_args(callee_fn, &subst)
    }
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
            let callee_symbol = compute_fn_symbol_from_type_args(callee_name, type_args);
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
fn walk_and_stash_internal_callees<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
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
        let callee_symbol = compute_fn_symbol_from_type_args(callee_name, type_args);
        if state.walked_entry_points.insert(callee_symbol.clone()) {
            let resolved_callee = resolve_toylang_callee(callee_fn, type_args);
            state.toylang_instances.push(ToylangInstance {
                extern_symbol: callee_symbol,
                resolved_func: resolved_callee.clone(),
                stub_def_id: None,
            });
            walk_and_stash_internal_callees(
                tcx, registry, &resolved_callee, callee_name, state,
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

/// Compute the extern symbol for a consumer function instance.
/// Concrete: "__toylang_impl_make_counter"
/// Generic: "__toylang_impl_wrap__i32"
pub fn compute_fn_symbol<'tcx>(name: &str, tcx: TyCtxt<'tcx>, instance: ty::Instance<'tcx>) -> String {
    let mut sym = format!("__toylang_impl_{}", name);
    for arg in instance.args.iter() {
        if let ty::GenericArgKind::Type(ty) = arg.kind() {
            let resolved = crate::oracle::rustc_ty_to_resolved_type(tcx, ty);
            sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(&resolved)));
        }
    }
    sym
}


