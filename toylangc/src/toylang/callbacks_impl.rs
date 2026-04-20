//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use rustc_hir::def_id::LocalDefId;
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
    CollectGenericRustDeps { name: String },
    NotifyConcreteEntryPoint { name: String },
    AfterRustAnalysis,
    GenerateAndCompile,
}

/// A toylang function instance discovered during the deep monomorphization walk.
#[derive(Clone)]
pub struct ToylangInstance {
    pub extern_symbol: String,
    pub resolved_func: crate::toylang::registry::ToyFunction,
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
}

pub struct ToylangCallbacks {
    pub registry: Arc<ToylangRegistry>,
    /// (ll_path, obj_path) for LLVM compilation. None if no external codegen.
    pub llvm_paths: Option<(PathBuf, PathBuf)>,
    /// Stage 5b: true when this callbacks instance is the downstream user-bin
    /// compile in the two-crate wrapper mode. The stub rlib's compile has
    /// already walked the consumer side and produced `.o`; here we install
    /// facade overrides so cross-crate queries route correctly, but skip the
    /// internal-callee walk in `notify_concrete_entry_point_inner` (callees
    /// are already in the rlib's `.o`) and skip `after_rust_analysis`
    /// validation (already validated upstream). `llvm_paths` is also forced
    /// to None upstream of construction so `generate_and_compile` short-
    /// circuits without producing a colliding `.o`. False in direct mode and
    /// in the rlib's own compile.
    pub is_downstream_of_stubs: bool,
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
    /// Per @SMINCZ, the returned `(DefId, GenericArgsRef)` pairs become
    /// `ReifyFnPointer` casts in the `optimized_mir` override's synthesized
    /// body, which is what forces rustc's mono collector to emit the Rust
    /// symbols. Args may contain `ty::TyKind::Param` placeholders — rustc's
    /// collector substitutes per caller during its walk.
    pub fn collect_generic_rust_deps_inner<'tcx>(
        &self,
        state: &mut ToylangState,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
    ) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
        state.log.push(CallbackLog::CollectGenericRustDeps { name: name.to_string() });

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

        // Build the param-name → Param-index map from the caller's identity
        // generics. The walker below runs the UNSUBSTITUTED body (Params in
        // place of concrete types); as the body is type-resolved and deps
        // are built, oracle helpers consult this map to rebuild rustc
        // `TyKind::Param` values when they encounter `ResolvedType::TypeParam`.
        // Dropped at end of this function, clearing the thread-local.
        let identity_args = ty::GenericArgs::identity_for_item(tcx, def_id.to_def_id());
        let generics = tcx.generics_of(def_id.to_def_id());
        let mut param_name_to_index: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for param in generics.own_params.iter() {
            if matches!(param.kind, ty::GenericParamDefKind::Type { .. }) {
                param_name_to_index.insert(param.name.to_string(), param.index);
            }
        }
        let _param_guard = crate::oracle::ActiveParamMap::install(param_name_to_index);

        // `resolve_caller_from_identity_args` is an identity-args substitution:
        // each type_param `T` maps to `ResolvedType::TypeParam("T")`, so the
        // body is effectively unchanged. Kept as a single code path so
        // `collect_rust_deps_recursive`'s signature (which takes an already-
        // resolved `ToyFunction`) doesn't need to branch.
        let resolved_caller = resolve_caller_from_identity_args(toy_fn);
        let identity_instance = ty::Instance::new(def_id.to_def_id(), identity_args);
        let extern_symbol = compute_fn_symbol(registry_name, tcx, identity_instance);
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
                if let ty::GenericArgKind::Type(ty) = arg.unpack() {
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
    /// Gated on `!self.is_downstream_of_stubs`: the user-bin compile
    /// doesn't own consumer codegen and shouldn't populate the queue
    /// (would leave stale state with no `generate_and_compile` to drain).
    pub fn populate_toylang_instances_from_cgus<'tcx>(
        &self,
        state: &mut ToylangState,
        tcx: TyCtxt<'tcx>,
    ) {
        if self.is_downstream_of_stubs {
            return;
        }

        let cgus = rustc_lang_facade::upstream_cgus(tcx);
        for cgu in cgus.iter() {
            for (&mono_item, _) in cgu.items() {
                let rustc_middle::mir::mono::MonoItem::Fn(instance) = mono_item else { continue };
                let def_id = instance.def_id();
                if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
                    continue;
                }

                // Accessors (associated items) are handled inline in
                // `generate_with_tcx`; the instance list here is only
                // for consumer fns with bodies.
                if tcx.opt_associated_item(def_id).is_some() {
                    continue;
                }

                let name = tcx.item_name(def_id).to_string();
                let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main".to_string() } else { name.clone() };
                let Some(toy_fn) = self.registry.functions.get(registry_name.as_str()) else {
                    // Non-consumer fn in __lang_stubs (e.g., Phase-6
                    // `__toylang_option_unwrap<T>` wrappers). Skip.
                    continue;
                };
                if toy_fn.body.is_none() {
                    continue;
                }

                let extern_symbol = compute_fn_symbol(&registry_name, tcx, instance);
                if !state.walked_entry_points.insert(extern_symbol.clone()) {
                    continue;
                }

                let resolved_caller = resolve_caller_from_instance(toy_fn, instance, tcx);
                state.toylang_instances.push(ToylangInstance {
                    extern_symbol: extern_symbol.clone(),
                    resolved_func: resolved_caller.clone(),
                });
                walk_and_stash_internal_callees(
                    tcx, &self.registry, &resolved_caller, &registry_name, state,
                );

                // Emit `[toylang] layout_of intercepted for: <ty>
                // size=N align=M` for each consumer type in this
                // Instance's signature. `lang_layout_of`'s eprintln
                // fires from the query provider, which incremental
                // cache skips on hit; emitting here (during populate,
                // always runs) makes the layout log incremental-safe
                // for tests that assert on the stderr — e.g.
                // `test_point_layout`. Lock-free now that
                // `monomorphize_type` is stateless.
                emit_layout_log_for_instance(tcx, instance);
            }
        }
    }
}

/// For each consumer ADT type reachable from the Instance's signature,
/// emit the `[toylang] layout_of intercepted for: <ty> size=N
/// align=M` log line. Safe inside `generate_and_compile` (MUTABLE_STATE
/// held) because `call_monomorphize_type` is lock-free.
fn emit_layout_log_for_instance<'tcx>(tcx: TyCtxt<'tcx>, instance: ty::Instance<'tcx>) {
    let fn_sig = tcx.fn_sig(instance.def_id()).instantiate(tcx, instance.args);
    let sig = fn_sig.skip_binder();
    let mut seen: HashSet<String> = HashSet::new();
    for ty in sig.inputs().iter().copied().chain(std::iter::once(sig.output())) {
        walk_ty_for_layout_log(tcx, ty, &mut seen);
    }
}

fn walk_ty_for_layout_log<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
    seen: &mut HashSet<String>,
) {
    use rustc_middle::ty::TypeVisitableExt;
    if let ty::TyKind::Adt(adt_def, args) = ty.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        let has_params = args.iter().any(|a| a.has_param());
        if !has_params
            && rustc_lang_facade::is_from_lang_stubs(tcx, adt_def.did())
        {
            if seen.insert(format!("{:?}", ty)) {
                let result = rustc_lang_facade::call_monomorphize_type(
                    &name, tcx, ty,
                );
                let (size, align) = compute_layout_size_align(tcx, &result.field_types);
                eprintln!(
                    "[toylang] layout_of intercepted for: {:?} size={} align={}",
                    ty, size, align,
                );
            }
        }
        // Recurse into generic args so consumer types nested in
        // `Vec<ToyPoint>` etc. get their log entries too.
        for arg in args.iter() {
            if let ty::GenericArgKind::Type(inner) = arg.unpack() {
                walk_ty_for_layout_log(tcx, inner, seen);
            }
        }
    }
}

/// Compute `(size, align)` for a consumer type given its resolved
/// field types. Mirrors `rustc-lang-facade::queries::layout::
/// build_layout`'s arithmetic; we just need the numbers, not a full
/// `TyAndLayout`.
fn compute_layout_size_align<'tcx>(
    tcx: TyCtxt<'tcx>,
    field_types: &[Ty<'tcx>],
) -> (u64, u64) {
    let mut offset = 0u64;
    let mut max_align = 1u64;
    for &field_ty in field_types {
        let layout = tcx.layout_of(
            rustc_middle::ty::PseudoCanonicalInput {
                value: field_ty,
                typing_env: rustc_middle::ty::TypingEnv::fully_monomorphized(),
            },
        ).expect("layout of field type");
        let fsz = layout.size.bytes();
        let falign = layout.align.abi.bytes();
        max_align = max_align.max(falign);
        offset = (offset + falign - 1) & !(falign - 1);
        offset += fsz;
    }
    let size = (offset + max_align - 1) & !(max_align - 1);
    (size, max_align)
}

impl LangPredicates for ToylangCallbacks {
    fn is_consumer_type(&self, name: &str) -> bool {
        self.registry.structs.contains_key(name)
    }

    fn is_consumer_fn(&self, name: &str) -> bool {
        // Only toylang-defined functions (with bodies) are consumer functions.
        // Extern functions (body-less) are real Rust functions and must not be intercepted.
        if name == crate::oracle::TOYLANG_MAIN {
            return self.registry.functions.get("main").map_or(false, |f| f.body.is_some());
        }
        self.registry.functions.get(name).map_or(false, |f| f.body.is_some())
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

        // Stage 5b: validation already ran in the upstream stub-rlib compile,
        // where every consumer item + every Rust type is local. The downstream
        // user-bin compile sees those same items via the extern `__lang_stubs`
        // rlib's `module_children`; re-running the checks here would either
        // be redundant (type-resolution helpers from 5a route cross-crate
        // already) or fail (the local-definition walks in `find_extern_fn_def_id`
        // and friends only see the bin's `fn main()` shim). Bail to preserve
        // the rlib-compile's validation as the single source of truth.
        if self.is_downstream_of_stubs {
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
            match crate::toylang::type_resolve::resolve_fn_body(&self.registry, func, &rust_method_ret, &rust_param_types, &is_rust_trait) {
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
        def_id: LocalDefId,
    ) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
        self.collect_generic_rust_deps_inner(state(s), name, tcx, def_id)
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

        let (ref ll_path, ref obj_path) = self.llvm_paths.as_ref()?;

        // B6 fix: populate state.toylang_instances from the CGU list + a
        // transitive internal-callee walk, up front and deterministically.
        // Prior art accumulated state as a side effect of the per-item
        // `notify_concrete_entry_point` query firing — which rustc's
        // incremental cache could skip on cache hit, leaving state empty
        // and the emitted `.o` symbol-less. See risks.md §B6 for the
        // full diagnosis and risks.md §B6 RESOLVED marker for this fix.
        self.populate_toylang_instances_from_cgus(ts, tcx);

        // Dump callback log to file if requested (for test assertions).
        // Done before codegen so the log captures the monomorphization-
        // phase callbacks + the B6 population step, not any internal
        // codegen logs. `NotifyConcreteEntryPoint` entries come from
        // `notify_concrete_entry_point_inner`'s log push (still live);
        // under CARGO_INCREMENTAL=0 (test-harness stopgap) the query
        // provider fires freely so this is a reliable source.
        if let Ok(path) = std::env::var("TOYLANG_LOG_PATH") {
            let lines: Vec<String> = ts.log.iter().map(|entry| format!("{:?}", entry)).collect();
            std::fs::write(&path, lines.join("\n")).expect("failed to write callback log");
        }

        // Walk MonoItems and codegen each consumer instance inline (same 'tcx scope).
        let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(
            tcx, &self.registry, self, ts,
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
fn resolve_caller_from_instance<'tcx>(
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
            if let ty::GenericArgKind::Type(ty) = arg.unpack() {
                subst.insert(param_name.clone(), crate::oracle::rustc_ty_to_resolved_type(tcx, ty));
            }
        }
    }
    resolve_caller_from_type_args(caller_fn, &subst)
}

/// Identity-args variant of `resolve_caller_from_instance`: each type param `T`
/// substitutes to `ResolvedType::TypeParam("T")` so the returned `ToyFunction`
/// is structurally unchanged (body retains Param references). The
/// `type_params: vec![]` canonicalization mirrors the concrete-args path, so
/// `collect_rust_deps_recursive` and `type_resolve_body` see the same shape
/// regardless of path — Param resolution at dep sites is handled via the
/// `oracle::ActiveParamMap` thread-local installed in
/// `collect_generic_rust_deps_inner`.
fn resolve_caller_from_identity_args(
    caller_fn: &crate::toylang::registry::ToyFunction,
) -> crate::toylang::registry::ToyFunction {
    if caller_fn.type_params.is_empty() {
        return caller_fn.clone();
    }
    let mut subst = std::collections::HashMap::new();
    for param_name in caller_fn.type_params.iter() {
        subst.insert(
            param_name.clone(),
            crate::toylang::typed_ast::ResolvedType::TypeParam(param_name.clone()),
        );
    }
    resolve_caller_from_type_args(caller_fn, &subst)
}

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
/// Under the post-5c.4 two-crate architecture, in the stub rlib's
/// compile the items are at LOCAL_CRATE's root (whose name is
/// `__lang_stubs`); in the user-bin compile the items aren't local to
/// scan here (use the facade's extern-crate walker for that). Pre-5c.4
/// this function had to handle a FileLoader path where items lived
/// nested in `mod __lang_stubs {}` inside the user crate; that path
/// was retired along with direct mode, and the former name-dispatch
/// comment documenting the difference between safe/unsafe variants of
/// the predicate is moot now that `is_from_lang_stubs_safe` has
/// collapsed into `is_from_lang_stubs`.
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
        if let ty::GenericArgKind::Type(ty) = arg.unpack() {
            let resolved = crate::oracle::rustc_ty_to_resolved_type(tcx, ty);
            sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(&resolved)));
        }
    }
    sym
}


