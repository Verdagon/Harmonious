//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};

use rustc_lang_facade::{LangCallbacks, LangPredicates, MonomorphizeTypeResult, MonomorphizeFnResult};
use crate::toylang::registry::ToylangRegistry;

/// A structured log entry for each callback rustc makes into toylang.
#[derive(Clone, Debug)]
pub enum CallbackLog {
    MonomorphizeType { name: String },
    MonomorphizeFn { name: String },
    AfterRustAnalysis,
    GenerateAndCompile,
}

/// A toylang function instance discovered during the deep monomorphization walk.
#[derive(Clone)]
pub struct ToylangInstance {
    pub registry_name: String,
    pub extern_symbol: String,
    pub resolved_func: crate::toylang::registry::ToyFunction,
}

/// Mutable state accumulated during compilation. Stored in the facade's global
/// mutex and passed as `&mut dyn Any` to every callback. The facade ensures
/// single-threaded execution — no locking needed on the consumer side.
#[derive(Default)]
pub struct ToylangState {
    pub log: Vec<CallbackLog>,
    /// Toylang function instances discovered during deep monomorphization walk.
    /// Populated by collect_toylang_fn_deps_inner, consumed by generate_with_tcx.
    pub toylang_instances: Vec<ToylangInstance>,
    /// Extern symbols already visited during deep walks. Persists across
    /// monomorphize_fn calls so shared callees are only walked once.
    pub visited_symbols: std::collections::HashSet<String>,
}

pub struct ToylangCallbacks {
    pub registry: Arc<ToylangRegistry>,
    /// (ll_path, obj_path) for LLVM compilation. None if no external codegen.
    pub llvm_paths: Option<(PathBuf, PathBuf)>,
}

/// Downcast `&mut dyn Any` to `&mut ToylangState`.
fn state(s: &mut dyn Any) -> &mut ToylangState {
    s.downcast_mut::<ToylangState>().expect("consumer state is not ToylangState")
}

impl ToylangCallbacks {
    /// The actual monomorphize_fn logic. No locking — caller must hold the lock.
    /// Called by the trait entry point (which locks) and by generate_with_tcx
    /// (which is already inside generate_and_compile's lock).
    pub fn monomorphize_fn_inner<'tcx>(
        &self,
        state: &mut ToylangState,
        name: &str,
        tcx: TyCtxt<'tcx>,
        _def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx> {
        state.log.push(CallbackLog::MonomorphizeFn { name: name.to_string() });
        // Accessor methods come in as "StructName.field_name"
        if let Some((struct_name, field_name)) = name.split_once('.') {
            let mut sym = format!("__toylang_accessor_{}_{}", struct_name, field_name);
            for arg in instance.args.iter() {
                if let ty::GenericArgKind::Type(ty) = arg.unpack() {
                    let resolved = crate::oracle::rustc_ty_to_resolved_type(tcx, ty);
                    sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(&resolved)));
                }
            }
            return MonomorphizeFnResult {
                extern_symbol: sym,
                rust_deps: vec![],
            };
        }

        let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main" } else { name };
        let toy_fn = self.registry.functions.get(registry_name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_fn: function '{}' not in registry", registry_name));

        let extern_symbol = compute_fn_symbol(registry_name, tcx, instance);

        let rust_deps = if toy_fn.body.is_some() {
            if state.visited_symbols.contains(&extern_symbol) {
                // Already walked this function (e.g., symbol_name re-calling after
                // per_instance_mir). Rust deps were reported on the first call.
                vec![]
            } else {
                // First time seeing this entry point. Resolve and deep walk.
                let resolved_caller = resolve_caller_from_instance(toy_fn, instance, tcx);

                state.visited_symbols.insert(extern_symbol.clone());
                state.toylang_instances.push(ToylangInstance {
                    registry_name: registry_name.to_string(),
                    extern_symbol: extern_symbol.clone(),
                    resolved_func: resolved_caller.clone(),
                });

                collect_toylang_fn_deps_inner(
                    tcx, &self.registry, &resolved_caller, registry_name, state,
                )
            }
        } else {
            vec![]
        };

        MonomorphizeFnResult {
            extern_symbol,
            rust_deps,
        }
    }
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

    fn generate_stubs(&self) -> String {
        crate::stub_gen::generate(&self.registry)
    }

    fn visibility_override<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        instance: ty::Instance<'tcx>,
    ) -> Option<(rustc_middle::mir::mono::Linkage, rustc_middle::mir::mono::Visibility)> {
        use rustc_hir::definitions::DefPathData;
        use rustc_middle::mir::mono::{Linkage, Visibility};
        // Force `(External, Default)` for any item whose DefPath contains
        // `__lang_stubs::`. Without this, the CGU partitioner internalizes
        // generic `#[inline(never)]` wrappers (e.g. `__toylang_option_unwrap<T>`)
        // because it can't see the references from the externally-linked
        // toylang `.o` file.
        //
        // Walks `tcx.def_path(...).data` directly. Cannot use
        // `rustc_lang_facade::is_from_lang_stubs` here — it calls `def_path_str`,
        // which ICEs in normal (non-diagnostic) compilation contexts, and the
        // partitioner runs outside `generate_and_compile` (see @DPSFDOZ).
        let in_lang_stubs = tcx.def_path(instance.def_id()).data.iter().any(|d| {
            matches!(d.data, DefPathData::TypeNs(name) if name.as_str() == "__lang_stubs")
        });
        if in_lang_stubs {
            Some((Linkage::External, Visibility::Default))
        } else {
            None
        }
    }
}

impl LangCallbacks for ToylangCallbacks {
    fn create_state(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(ToylangState::default())
    }

    fn after_rust_analysis<'tcx>(&self, s: &mut dyn Any, tcx: TyCtxt<'tcx>) {
        state(s).log.push(CallbackLog::AfterRustAnalysis);
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
        s: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx> {
        state(s).log.push(CallbackLog::MonomorphizeType { name: name.to_string() });
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

    fn monomorphize_fn<'tcx>(
        &self,
        s: &mut dyn Any,
        name: &str,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx> {
        self.monomorphize_fn_inner(state(s), name, tcx, def_id, instance)
    }

    fn generate_and_compile<'tcx>(&self, s: &mut dyn Any, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)> {
        let ts = state(s);
        ts.log.push(CallbackLog::GenerateAndCompile);

        // Dump callback log to file if requested (for test assertions).
        // Done before codegen so the log captures only the monomorphization-phase callbacks.
        if let Ok(path) = std::env::var("TOYLANG_LOG_PATH") {
            let lines: Vec<String> = ts.log.iter().map(|entry| format!("{:?}", entry)).collect();
            std::fs::write(&path, lines.join("\n")).expect("failed to write callback log");
        }

        let (ref ll_path, ref obj_path) = self.llvm_paths.as_ref()?;

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

/// Deep recursive walk: type-resolve a function body, collect transitive Rust deps,
/// and stash discovered toylang instances in state. Only returns Rust deps to rustc.
///
/// Per @SMINCZ, this function is the codegen-driving site for every Rust
/// callee toylang references. Each `(def_id, args)` pushed into `deps`
/// becomes a `ReifyFnPointer` cast inside `per_instance_mir`'s synthesized
/// MIR body, which forces rustc's mono collector to emit the symbol.
/// llvm_gen.rs's `tcx.symbol_name` calls are read-only — they only work
/// if the matching dep was registered here first.
fn collect_toylang_fn_deps_inner<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    resolved_fn: &crate::toylang::registry::ToyFunction,
    fn_name: &str,
    state: &mut ToylangState,
) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
    let _body = resolved_fn.body.as_ref().expect("collect_toylang_fn_deps_inner called on extern fn");

    // Type-resolve the already-substituted body
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
    let typed_body = crate::toylang::type_resolve::resolve_fn_body(registry, resolved_fn, &rust_method_ret, &rust_param_types, &is_rust_trait)
        .unwrap_or_else(|e| panic!("[toylang] type error in '{}': {:?}", fn_name, e));

    // Walk typed body for fn_calls and rust_method_deps
    let mut deps = Vec::new();
    let mut fn_calls = Vec::new();
    let mut rust_method_deps = Vec::new();
    walk_typed_body_for_deps(&typed_body, &mut fn_calls, &mut rust_method_deps);

    // Process function call deps
    for (callee_name, type_args) in &fn_calls {
        let Some(callee_fn) = registry.functions.get(callee_name.as_str()) else {
            // Not a toylang fn — check if it's a use-imported free function
            if let Some(def_id) = crate::oracle::find_use_imported_fn_def_id(tcx, callee_name) {
                let ty_arg_refs: Vec<ty::GenericArg<'_>> = type_args.iter()
                    .map(|ta| ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(tcx, ta)))
                    .collect();
                deps.push((def_id, tcx.mk_args(&ty_arg_refs)));
            }
            continue;
        };

        if callee_fn.body.is_some() {
            // Toylang callee — recurse instead of reporting to rustc
            let callee_symbol = compute_fn_symbol_from_type_args(callee_name, type_args);
            if state.visited_symbols.insert(callee_symbol.clone()) {
                // Substitute type params in callee's body
                let resolved_callee = if !callee_fn.type_params.is_empty() {
                    let subst: std::collections::HashMap<String, crate::toylang::typed_ast::ResolvedType> =
                        callee_fn.type_params.iter().zip(type_args.iter())
                            .map(|(param, arg)| (param.clone(), arg.clone()))
                            .collect();
                    resolve_caller_from_type_args(callee_fn, &subst)
                } else {
                    callee_fn.clone()
                };

                // Recurse to find transitive Rust deps
                let transitive_deps = collect_toylang_fn_deps_inner(
                    tcx, registry, &resolved_callee, callee_name, state,
                );
                deps.extend(transitive_deps);

                // Stash for generate_with_tcx
                state.toylang_instances.push(ToylangInstance {
                    registry_name: callee_name.clone(),
                    extern_symbol: callee_symbol,
                    resolved_func: resolved_callee,
                });
            }
        } else {
            // Extern function — report to rustc
            let Some(def_id) = crate::oracle::find_extern_fn_def_id(tcx, callee_name) else { continue };
            let args = tcx.mk_args(&[]);
            deps.push((def_id, args));
        }
    }

    // Resolve Rust method deps (inherent and trait)
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
                let expected_count = tcx.generics_of(trait_method_def_id).count();
                let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
                deps.push((trait_method_def_id, args));
                continue;
            }
            // Fall through to inherent method lookup if trait not found
        }

        // Phase 6: redirect to wrapper if applicable. The wrapper Instance
        // (not the inline stdlib method) lands in rust_deps so per_instance_mir
        // reifies a fn-pointer to it, forcing rustc's mono collector to
        // codegen the wrapper. Without this, `Option::unwrap` and friends
        // produce no callable symbol.
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
        let expected_count = tcx.generics_of(method_def_id).count();
        let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
        deps.push((method_def_id, args));
    }

    deps
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

/// Convert a type string to a rustc Ty.
/// Find a function's DefId in __lang_stubs by name.
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


