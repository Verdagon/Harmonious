//! Implementation of LangCallbacks for Toylang.
//! This is the consumer side — all toylang-specific logic lives here.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};

use rustc_lang_facade::{LangCallbacks, MonomorphizeTypeResult, MonomorphizeFnResult};
use crate::toylang::ast::FnBody;
use crate::toylang::registry::ToylangRegistry;

pub struct ToylangCallbacks {
    pub registry: Arc<ToylangRegistry>,
    /// (ll_path, obj_path) for LLVM compilation. None if no external codegen.
    pub llvm_paths: Option<(PathBuf, PathBuf)>,
}

impl LangCallbacks for ToylangCallbacks {
    fn type_names(&self) -> std::collections::HashSet<String> {
        self.registry.structs.keys().cloned().collect()
    }

    fn fn_names(&self) -> std::collections::HashSet<String> {
        // Only include toylang-defined functions (with bodies).
        // Extern functions (body-less) are real Rust functions and must not be intercepted.
        let mut names: std::collections::HashSet<String> = self.registry.functions.iter()
            .filter(|(_, f)| f.body.is_some())
            .map(|(name, _)| name.clone())
            .collect();
        if names.contains("main") {
            names.insert(crate::oracle::TOYLANG_MAIN.to_string());
        }
        names
    }

    fn after_rust_analysis<'tcx>(&self, tcx: TyCtxt<'tcx>) {
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
            let rust_method_ret = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> crate::toylang::typed_ast::ResolvedType {
                crate::oracle::rust_method_return_type(tcx, type_name, method, type_args)
            };
            if let Err(e) = crate::toylang::type_resolve::resolve_fn_body(&self.registry, func, &rust_method_ret) {
                errors.push(format!("function '{}': {:?}", name, e));
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

    fn generate_stubs(&self) -> String {
        crate::stub_gen::generate(&self.registry)
    }

    fn monomorphize_type<'tcx>(
        &self,
        name: &str,
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
    ) -> MonomorphizeTypeResult<'tcx> {
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
        name: &str,
        tcx: TyCtxt<'tcx>,
        _def_id: LocalDefId,
        instance: ty::Instance<'tcx>,
    ) -> MonomorphizeFnResult<'tcx> {
        // Accessor methods come in as "StructName.field_name"
        if let Some((struct_name, field_name)) = name.split_once('.') {
            // Use Instance to build a unique symbol per concrete instantiation
            let sym = rustc_lang_facade::queries::per_instance::accessor_symbol_for_instance(
                tcx, instance, struct_name, field_name,
            );
            return MonomorphizeFnResult {
                extern_symbol: sym,
                rust_deps: vec![],
            };
        }

        let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main" } else { name };
        let toy_fn = self.registry.functions.get(registry_name)
            .unwrap_or_else(|| panic!("[toylang] monomorphize_fn: function '{}' not in registry", registry_name));

        // Compute symbol on the fly. For generic functions, include mangled type args.
        let extern_symbol = compute_fn_symbol(registry_name, tcx, instance);

        // Collect ALL dependencies from the typed AST walk (both toylang and Rust method deps)
        let rust_deps = if let Some(ref fn_body) = toy_fn.body {
            collect_toylang_fn_deps(tcx, fn_body, &self.registry, toy_fn, instance)
        } else {
            vec![]
        };

        MonomorphizeFnResult {
            extern_symbol,
            rust_deps,
        }
    }

    fn generate_and_compile<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)> {
        let (ref ll_path, ref obj_path) = self.llvm_paths.as_ref()?;

        // Walk MonoItems and codegen each consumer instance inline (same 'tcx scope).
        let (llvm_ir, rust_symbols) = crate::llvm_gen::generate_with_tcx(
            tcx, &self.registry, self,
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

/// Scan a function body for calls to other toylang functions and return their
/// DefId + GenericArgs so the monomorphization collector discovers them.
fn collect_toylang_fn_deps<'tcx>(
    tcx: TyCtxt<'tcx>,
    _body: &FnBody,
    registry: &ToylangRegistry,
    caller_fn: &crate::toylang::registry::ToyFunction,
    instance: ty::Instance<'tcx>,
) -> Vec<(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)> {
    // Unified approach: run the type resolver on the caller's body to get
    // concrete type args for all FnCall nodes, then construct callee Instances.

    // If the caller is generic, substitute type params with concrete args first.
    let resolved_caller = if !caller_fn.type_params.is_empty() {
        let mut subst = std::collections::HashMap::new();
        for (i, param_name) in caller_fn.type_params.iter().enumerate() {
            if let Some(arg) = instance.args.get(i) {
                if let ty::GenericArgKind::Type(ty) = arg.unpack() {
                    subst.insert(param_name.clone(), crate::oracle::rustc_ty_to_resolved_type(tcx, ty));
                }
            }
        }
        crate::toylang::registry::ToyFunction {
            name: caller_fn.name.clone(),
            type_params: vec![],
            params: caller_fn.params.iter().map(|p| crate::toylang::registry::ToyParam {
                name: p.name.clone(),
                ty: crate::toylang::type_resolve::substitute_type_params(&p.ty, &subst),
            }).collect(),
            return_ty: caller_fn.return_ty.as_ref()
                .map(|rt| crate::toylang::type_resolve::substitute_type_params(rt, &subst)),
            body: caller_fn.body.as_ref().map(|b| {
                crate::toylang::type_resolve::substitute_type_params_in_body(b, &subst)
            }),
        }
    } else {
        caller_fn.clone()
    };

    // Query rustc for Rust method return types
    let rust_method_ret = |type_name: &str, method: &str, type_args: &[crate::toylang::typed_ast::ResolvedType]| -> crate::toylang::typed_ast::ResolvedType {
        crate::oracle::rust_method_return_type(tcx, type_name, method, type_args)
    };
    let typed_body = crate::toylang::type_resolve::resolve_fn_body(registry, &resolved_caller, &rust_method_ret)
        .expect("type resolution should succeed (already validated)");

    // Walk typed body for both toylang FnCall deps and Rust method deps
    let mut deps = Vec::new();
    let mut fn_calls = Vec::new();
    let mut rust_method_deps = Vec::new();
    walk_typed_body_for_deps(&typed_body, &mut fn_calls, &mut rust_method_deps);

    // Resolve function call deps (both toylang and extern)
    for (callee_name, type_args) in &fn_calls {
        let Some(callee_fn) = registry.functions.get(callee_name.as_str()) else { continue };

        if callee_fn.body.is_some() {
            // Toylang function — find its stub wrapper
            let Some(callee_def_id) = find_stub_fn_def_id(tcx, callee_name) else { continue };
            if type_args.is_empty() {
                let args = tcx.mk_args(&[]);
                deps.push((callee_def_id, args));
            } else {
                let ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                    .map(|rt| ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(tcx, rt)))
                    .collect();
                let args = tcx.mk_args(&ty_args);
                deps.push((callee_def_id, args));
            }
        } else {
            // Extern function — find the real Rust function
            let Some(def_id) = crate::oracle::find_extern_fn_def_id(tcx, callee_name) else { continue };
            let args = tcx.mk_args(&[]);
            deps.push((def_id, args));
        }
    }

    // Resolve Rust method deps
    for dep in &rust_method_deps {
        let type_def_id = crate::oracle::find_rust_type_def_id(tcx, &dep.type_name)
            .unwrap_or_else(|| panic!("Rust type '{}' not found", dep.type_name));
        let method_def_id = crate::oracle::find_inherent_method(tcx, type_def_id, &dep.method_name)
            .unwrap_or_else(|| panic!("method '{}' not found on '{}'", dep.method_name, dep.type_name));

        // Build generic args: convert ResolvedType type_args to rustc Ty (all explicit)
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
}

/// Walk a TypedFnBody and collect toylang FnCall deps and Rust method deps.
fn walk_typed_body_for_deps(
    body: &crate::toylang::typed_ast::TypedFnBody,
    fn_calls: &mut Vec<(String, Vec<crate::toylang::typed_ast::ResolvedType>)>,
    rust_method_deps: &mut Vec<RustMethodDep>,
) {
    use crate::toylang::typed_ast::*;
    for stmt in &body.stmts {
        match stmt {
            TypedStmt::Let { expr, .. } => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
            TypedStmt::ExprStmt(expr) => walk_typed_expr_for_deps(expr, fn_calls, rust_method_deps),
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
            // Static call on a Rust type (e.g. Vec::new)
            rust_method_deps.push(RustMethodDep {
                type_name: ty.clone(),
                method_name: method.clone(),
                type_args: type_args.clone(),
            });
            for arg in args {
                walk_typed_expr_for_deps(arg, fn_calls, rust_method_deps);
            }
        }
        _ => {} // IntLit, BoolLit, Var, StringLit — no children
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
fn compute_fn_symbol<'tcx>(name: &str, tcx: TyCtxt<'tcx>, instance: ty::Instance<'tcx>) -> String {
    let mut sym = format!("__toylang_impl_{}", name);
    for arg in instance.args.iter() {
        if let ty::GenericArgKind::Type(ty) = arg.unpack() {
            sym.push_str(&format!("__{}", mangle_ty_for_symbol(tcx, ty)));
        }
    }
    sym
}

fn mangle_ty_for_symbol<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>) -> String {
    match ty.kind() {
        TyKind::Int(int_ty) => int_ty.name_str().to_string(),
        TyKind::Uint(uint_ty) => uint_ty.name_str().to_string(),
        TyKind::Float(float_ty) => float_ty.name_str().to_string(),
        TyKind::Bool => "bool".to_string(),
        TyKind::Adt(adt_def, args) => {
            let name = tcx.item_name(adt_def.did()).to_string();
            if args.is_empty() {
                name
            } else {
                let arg_strs: Vec<String> = args.iter()
                    .filter_map(|a| match a.unpack() {
                        ty::GenericArgKind::Type(t) => Some(mangle_ty_for_symbol(tcx, t)),
                        _ => None,
                    })
                    .collect();
                format!("{}_{}", name, arg_strs.join("_"))
            }
        }
        TyKind::Str => "str".to_string(),
        TyKind::Ref(_, inner_ty, _) => format!("ref_{}", mangle_ty_for_symbol(tcx, *inner_ty)),
        TyKind::RawPtr(inner_ty, _) => format!("ptr_{}", mangle_ty_for_symbol(tcx, *inner_ty)),
        TyKind::Slice(elem_ty) => format!("slice_{}", mangle_ty_for_symbol(tcx, *elem_ty)),
        TyKind::Tuple(tys) => {
            let parts: Vec<String> = tys.iter().map(|t| mangle_ty_for_symbol(tcx, t)).collect();
            format!("tuple_{}", parts.join("_"))
        }
        _ => panic!("mangle_ty_for_symbol: unsupported type {:?}", ty),
    }
}


