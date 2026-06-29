//! Type resolution pass — annotates every AST expression with its concrete type.
//!
//! Runs after parsing, before LLVM codegen. Walks the untyped AST and produces
//! a TypedBlock where every expression carries a ResolvedType.

use std::collections::HashMap;

use super::ast::{Expr, Stmt};
use super::registry::{ToylangRegistry, ToyFunction};
use super::typed_ast::*;

// ============================================================================
// Error types
// ============================================================================

#[derive(Debug, PartialEq)]
pub enum TypeResolveError {
    UndefinedVariable { name: String },
    UndefinedStruct { name: String },
    UndefinedFunction { name: String },
    FieldNotFound { struct_name: String, field_name: String },
    FieldAccessOnNonStruct { ty: ResolvedType, field: String },
    MethodCallOnUnsupportedType { ty: ResolvedType, method: String },
    WrongTypeArgCount { func_name: String, expected: usize, got: usize },
    NonStructLitType { ty: ResolvedType },
    IfConditionNotBool { ty: ResolvedType },
    WhileConditionNotBool { ty: ResolvedType },
    IfElseTypeMismatch { then_ty: ResolvedType, else_ty: ResolvedType },
    AssignTypeMismatch { name: String, expected: ResolvedType, got: ResolvedType },
    ArgTypeMismatch { func_name: String, param_index: usize, expected: ResolvedType, got: ResolvedType },
    // Per @MBMRVZ, toylang `fn main()` must have a void-typed tail. A non-void tail
    // creates an internal/extern ABI mismatch that SIGBUSes at runtime during the
    // internal form's sret store. Detected at after_rust_analysis time; actionable
    // fix in user source (add `;` to the last statement).
    MainMustReturnVoid { got: ResolvedType },
    // Per @RTMEIZ, a Rust type that flows through the type system (as
    // trait-call Self, return type, nested generic arg, etc.) wasn't
    // `use`-imported. The context tells the user where the type was needed.
    RustTypeNotImported { name: String, context: String },
    // Workstream B — a Rust trait-method query was deferred because Self or a
    // type arg is still a `TypeParam`. The per-Instance substituted pass redoes
    // the query with concrete args. Callers should treat this as "skip, don't
    // surface as user error."
    RustTypeDeferred { context: String },
}

impl TypeResolveError {
    /// True iff this is a Workstream-B-style deferred query that the eager
    /// typecheck pass should silently skip.
    pub fn is_deferred(&self) -> bool {
        matches!(self, TypeResolveError::RustTypeDeferred { .. })
    }
}

impl From<crate::oracle::UnresolvedRustType> for TypeResolveError {
    fn from(e: crate::oracle::UnresolvedRustType) -> Self {
        if e.is_deferred() {
            TypeResolveError::RustTypeDeferred { context: e.context.to_string() }
        } else {
            TypeResolveError::RustTypeNotImported {
                name: e.name,
                context: e.context.to_string(),
            }
        }
    }
}

// ============================================================================
// Public entry point
// ============================================================================

/// Resolve just the return type of a function without resolving the full body.
///
/// Two-enum split (2026-06-25): `func.return_ty` is `SourceType` (parser-shape);
/// the promotion to `ResolvedType` chains through `oracle::resolve_source_type`.
pub fn resolve_return_type(
    registry: &ToylangRegistry,
    func: &ToyFunction,
) -> Result<ResolvedType, TypeResolveError> {
    match func.return_ty.as_ref() {
        Some(rt) => crate::oracle::resolve_source_type(rt, registry),
        None => Ok(ResolvedType::Void),
    }
}

/// Resolve all types in a function body, producing a TypedBlock.
///
/// Two-enum split: oracle callbacks talk `SourceType` (parser-shape); the
/// resolver promotes each query result to `ResolvedType` before storing it
/// in the typed AST. Parser-shape types in `func.params` and `func.return_ty`
/// also promote at entry.
pub fn resolve_fn_body(
    registry: &ToylangRegistry,
    func: &ToyFunction,
    rust_method_ret: &dyn Fn(&str, &str, &[SourceType]) -> Result<SourceType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[SourceType]) -> Result<Option<Vec<SourceType>>, crate::oracle::UnresolvedRustType>,
    // Per @IVTDBTZ, trait-vs-inherent dispatch for Name::method(args) is
    // type-kind-based. Threaded as a predicate callback alongside the
    // existing return/param-type callbacks; backed in callbacks_impl.rs
    // by find_use_imported_trait_def_id.
    is_rust_trait: &dyn Fn(&str) -> bool,
) -> Result<TypedBlock, TypeResolveError> {
    let body = func.body.as_ref().expect("function has no body");
    let ret_ty = match func.return_ty.as_ref() {
        Some(rt) => crate::oracle::resolve_source_type(rt, registry)?,
        None => ResolvedType::Void,
    };

    let mut scope: HashMap<String, ResolvedType> = HashMap::new();

    // Promote parameter types from SourceType (parser-shape) to ResolvedType.
    for p in &func.params {
        scope.insert(p.name.clone(), crate::oracle::resolve_source_type(&p.ty, registry)?);
    }

    // Resolve statements
    let stmts: Vec<TypedStmt> = body.stmts.iter()
        .map(|stmt| resolve_stmt(stmt, &mut scope, &ret_ty, registry, rust_method_ret, rust_param_types, is_rust_trait))
        .collect::<Result<Vec<_>, _>>()?;

    // Resolve return expression
    let ret = body.ret.as_ref()
        .map(|expr| resolve_expr(expr, &ret_ty, &scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
        .transpose()?;

    Ok(TypedBlock { stmts, ret })
}

// ============================================================================
// Type string parsing
// ============================================================================

/// Two-enum split (2026-06-25) — `resolve_struct_fields` retired. The
/// promotion `SourceType → ResolvedType` now lives in `oracle::resolve_source_type`
/// (which handles the `StructRef` lookup that used to be here). The typed
/// AST and codegen path can't see a `StructRef` because the variant no
/// longer exists on `ResolvedType`.

/// Semantic type equality on `ResolvedType`. Bridges `Struct` ↔ `RustType`
/// when names + args match — cross-Sky-crate Sky types may surface as
/// `RustType` at one lookup site (rustc oracle, no struct fields available)
/// and `Struct` at another (sidecar registry, fields populated).
///
/// Correctness note: this duck-types `RustType` against `Struct` by name
/// + args without consulting `SkyUniverse` to verify Sky ownership. A
/// genuine Rust type sharing a name with a Sky struct would be falsely
/// conflated; the name-resolver elsewhere in the frontend would reject
/// such a collision earlier, so the bridging is safe.
fn types_match(a: &ResolvedType, b: &ResolvedType) -> bool {
    match (a, b) {
        (ResolvedType::RustType { name: na, type_args: ta },
         ResolvedType::Struct { name: nb, type_args: tb, .. }) |
        (ResolvedType::Struct { name: na, type_args: ta, .. },
         ResolvedType::RustType { name: nb, type_args: tb }) => {
            na == nb && ta.len() == tb.len()
                && ta.iter().zip(tb).all(|(a, b)| types_match(a, b))
        }
        (ResolvedType::Ref { inner: a }, ResolvedType::Ref { inner: b }) => {
            types_match(a, b)
        }
        _ => a == b,
    }
}

/// Substitute TypeParam variants in a ResolvedType with concrete types from the map.
pub fn substitute_type_params(
    ty: &ResolvedType,
    subst: &HashMap<String, ResolvedType>,
) -> ResolvedType {
    match ty {
        ResolvedType::TypeParam(name) => {
            subst.get(name)
                .unwrap_or_else(|| panic!("unresolved type param '{}'", name))
                .clone()
        }
        ResolvedType::Struct { name, type_args, field_types } => {
            ResolvedType::Struct {
                name: name.clone(),
                type_args: type_args.iter().map(|a| substitute_type_params(a, subst)).collect(),
                field_types: field_types.iter().map(|f| substitute_type_params(f, subst)).collect(),
            }
        }
        ResolvedType::RustType { name, type_args } => {
            ResolvedType::RustType {
                name: name.clone(),
                type_args: type_args.iter().map(|a| substitute_type_params(a, subst)).collect(),
            }
        }
        ResolvedType::Ref { inner } => {
            ResolvedType::Ref { inner: Box::new(substitute_type_params(inner, subst)) }
        }
        // Primitives pass through unchanged
        other => other.clone(),
    }
}

/// Walk an AST body and substitute TypeParams in all embedded `SourceType`
/// type_args slots. Used when monomorphizing a generic function before
/// type resolution (cache-miss path under sunny-karp).
///
/// Two-enum split: parser AST carries `SourceType`. The substitution map
/// values are `ResolvedType` (the form callers already have post-promotion);
/// `oracle::substitute_source_type` demotes the map values to `SourceType`
/// during application so the result stays parser-shape.
pub fn substitute_type_params_in_body(
    body: &super::ast::Block,
    subst: &HashMap<String, ResolvedType>,
) -> super::ast::Block {
    use super::ast::*;
    fn subst_ty(ty: &SourceType, subst: &HashMap<String, ResolvedType>) -> SourceType {
        crate::oracle::substitute_source_type(ty, subst)
    }
    fn subst_expr(expr: &Expr, subst: &HashMap<String, ResolvedType>) -> Expr {
        match expr {
            Expr::IntLit(n, ty) => Expr::IntLit(*n, subst_ty(ty, subst)),
            Expr::BoolLit(b) => Expr::BoolLit(*b),
            Expr::StringLit(s) => Expr::StringLit(s.clone()),
            Expr::ByteStringLit(b) => Expr::ByteStringLit(b.clone()),
            Expr::Var(name) => Expr::Var(name.clone()),
            Expr::StructLit { name, type_args, fields } => Expr::StructLit {
                name: name.clone(),
                type_args: type_args.iter().map(|ta| subst_ty(ta, subst)).collect(),
                fields: fields.iter().map(|(n, e)| (n.clone(), subst_expr(e, subst))).collect(),
            },
            Expr::FnCall { name, type_args, args } => Expr::FnCall {
                name: name.clone(),
                type_args: type_args.iter().map(|ta| subst_ty(ta, subst)).collect(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            Expr::StaticCall { ty, method, type_args, args } => Expr::StaticCall {
                ty: ty.clone(),
                method: method.clone(),
                type_args: type_args.iter().map(|ta| subst_ty(ta, subst)).collect(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            Expr::MethodCall { receiver, method, args } => Expr::MethodCall {
                receiver: Box::new(subst_expr(receiver, subst)),
                method: method.clone(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            Expr::FieldAccess { receiver, field } => Expr::FieldAccess {
                receiver: Box::new(subst_expr(receiver, subst)),
                field: field.clone(),
            },
            Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
                op: *op,
                left: Box::new(subst_expr(left, subst)),
                right: Box::new(subst_expr(right, subst)),
            },
            Expr::If { cond, then_body, else_body } => Expr::If {
                cond: Box::new(subst_expr(cond, subst)),
                then_body: Box::new(subst_fn_body(then_body, subst)),
                else_body: else_body.as_ref().map(|b| Box::new(subst_fn_body(b, subst))),
            },
            Expr::UnaryNeg(inner) => Expr::UnaryNeg(Box::new(subst_expr(inner, subst))),
            Expr::Ref(inner) => Expr::Ref(Box::new(subst_expr(inner, subst))),
        }
    }
    fn subst_stmt(stmt: &Stmt, subst: &HashMap<String, ResolvedType>) -> Stmt {
        match stmt {
            Stmt::Let { name, expr } => Stmt::Let { name: name.clone(), expr: subst_expr(expr, subst) },
            Stmt::ExprStmt(expr) => Stmt::ExprStmt(subst_expr(expr, subst)),
            Stmt::While { cond, body } => Stmt::While {
                cond: subst_expr(cond, subst),
                body: Box::new(subst_fn_body(body, subst)),
            },
            Stmt::Assign { name, expr } => Stmt::Assign {
                name: name.clone(),
                expr: subst_expr(expr, subst),
            },
        }
    }
    fn subst_fn_body(body: &super::ast::Block, subst: &HashMap<String, ResolvedType>) -> super::ast::Block {
        super::ast::Block {
            stmts: body.stmts.iter().map(|s| subst_stmt(s, subst)).collect(),
            ret: body.ret.as_ref().map(|e| subst_expr(e, subst)),
        }
    }
    subst_fn_body(body, subst)
}

/// Sunny-karp (2026-06-25) — pure typed-AST substitution.
///
/// Two-enum split (2026-06-25): the typed AST carries only `ResolvedType`
/// (no `StructRef` variant exists), so substitution is now a pure walk
/// with no registry-promotion chain. The substitution map values are
/// `ResolvedType` (fully resolved), so when a `TypeParam` is replaced the
/// result is already in the right form. The `registry` parameter is
/// retained for API compatibility but unused (kept to minimize churn at
/// call sites; can be removed in a follow-up).
pub fn substitute_in_typed_body(
    body: &super::typed_ast::TypedBlock,
    subst: &HashMap<String, ResolvedType>,
    _registry: &super::registry::ToylangRegistry,
) -> super::typed_ast::TypedBlock {
    use super::typed_ast::*;
    fn rewrite_ty(ty: &ResolvedType, subst: &HashMap<String, ResolvedType>) -> ResolvedType {
        super::type_resolve::substitute_type_params(ty, subst)
    }
    fn subst_expr(expr: &TypedExpr, subst: &HashMap<String, ResolvedType>) -> TypedExpr {
        let ty = rewrite_ty(&expr.ty, subst);
        let kind = match &expr.kind {
            TypedExprKind::IntLit(n) => TypedExprKind::IntLit(*n),
            TypedExprKind::BoolLit(b) => TypedExprKind::BoolLit(*b),
            TypedExprKind::StringLit(s) => TypedExprKind::StringLit(s.clone()),
            TypedExprKind::ByteStringLit(b) => TypedExprKind::ByteStringLit(b.clone()),
            TypedExprKind::Var(name) => TypedExprKind::Var(name.clone()),
            TypedExprKind::StructLit { name, fields } => TypedExprKind::StructLit {
                name: name.clone(),
                fields: fields.iter().map(|(n, e)| (n.clone(), subst_expr(e, subst))).collect(),
            },
            TypedExprKind::FnCall { name, type_args, args } => TypedExprKind::FnCall {
                name: name.clone(),
                type_args: type_args.iter().map(|ta| rewrite_ty(ta, subst)).collect(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            TypedExprKind::BinaryOp { op, left, right } => TypedExprKind::BinaryOp {
                op: *op,
                left: Box::new(subst_expr(left, subst)),
                right: Box::new(subst_expr(right, subst)),
            },
            TypedExprKind::StaticCall { ty: ty_name, method, type_args, args } => TypedExprKind::StaticCall {
                ty: ty_name.clone(),
                method: method.clone(),
                type_args: type_args.iter().map(|ta| rewrite_ty(ta, subst)).collect(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            TypedExprKind::MethodCall { receiver, method, args } => TypedExprKind::MethodCall {
                receiver: Box::new(subst_expr(receiver, subst)),
                method: method.clone(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            TypedExprKind::FieldAccess { receiver, field } => TypedExprKind::FieldAccess {
                receiver: Box::new(subst_expr(receiver, subst)),
                field: field.clone(),
            },
            TypedExprKind::If { cond, then_stmts, then_expr, else_stmts, else_expr } => TypedExprKind::If {
                cond: Box::new(subst_expr(cond, subst)),
                then_stmts: then_stmts.iter().map(|s| subst_stmt(s, subst)).collect(),
                then_expr: then_expr.as_ref().map(|e| Box::new(subst_expr(e, subst))),
                else_stmts: else_stmts.iter().map(|s| subst_stmt(s, subst)).collect(),
                else_expr: else_expr.as_ref().map(|e| Box::new(subst_expr(e, subst))),
            },
            TypedExprKind::Ref(inner) => TypedExprKind::Ref(Box::new(subst_expr(inner, subst))),
        };
        TypedExpr { kind, ty }
    }
    fn subst_stmt(stmt: &TypedStmt, subst: &HashMap<String, ResolvedType>) -> TypedStmt {
        match stmt {
            TypedStmt::Let { name, expr } => TypedStmt::Let {
                name: name.clone(),
                expr: subst_expr(expr, subst),
            },
            TypedStmt::ExprStmt(expr) => TypedStmt::ExprStmt(subst_expr(expr, subst)),
            TypedStmt::While { cond, body } => TypedStmt::While {
                cond: subst_expr(cond, subst),
                body: subst_block(body, subst),
            },
            TypedStmt::Assign { name, expr } => TypedStmt::Assign {
                name: name.clone(),
                expr: subst_expr(expr, subst),
            },
        }
    }
    fn subst_block(block: &TypedBlock, subst: &HashMap<String, ResolvedType>) -> TypedBlock {
        TypedBlock {
            stmts: block.stmts.iter().map(|s| subst_stmt(s, subst)).collect(),
            ret: block.ret.as_ref().map(|e| subst_expr(e, subst)),
        }
    }
    subst_block(body, subst)
}

fn find_field_index(toy_struct: &super::registry::ToyStruct, struct_name: &str, field_name: &str) -> Result<usize, TypeResolveError> {
    toy_struct.fields.iter()
        .position(|f| f.name == field_name)
        .ok_or_else(|| TypeResolveError::FieldNotFound {
            struct_name: struct_name.to_string(),
            field_name: field_name.to_string(),
        })
}

// ============================================================================
// Expression resolution
// ============================================================================

fn resolve_expr(
    expr: &Expr,
    expected_ty: &ResolvedType,
    scope: &HashMap<String, ResolvedType>,
    registry: &ToylangRegistry,
    rust_method_ret: &dyn Fn(&str, &str, &[SourceType]) -> Result<SourceType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[SourceType]) -> Result<Option<Vec<SourceType>>, crate::oracle::UnresolvedRustType>,
    is_rust_trait: &dyn Fn(&str) -> bool,
) -> Result<TypedExpr, TypeResolveError> {
    match expr {
        Expr::IntLit(n, ty) => {
            // Two-enum split: `ty` is parser-shape `SourceType`. Promote to
            // ResolvedType for the coercion arms below.
            let ty = crate::oracle::resolve_source_type(ty, registry)?;
            let ty = &ty;
            // The parser commits a default type for unsuffixed integer
            // literals (`i32` for values fitting in i32, else `i64`).
            // Without coercion against `expected_ty`, a Sky source like
            // `struct W { a: i64, b: i64 } ... W { a: x, b: 0 }` would
            // produce a typed AST where field `b`'s expression has type
            // i32 but the field type is i64. Codegen then emits
            // `store i32 0` to the i64 field, leaving the upper 4 bytes
            // uninitialized → silent miscompile for any caller that
            // reads `b` (or in struct memcpy-as-larger-type paths).
            //
            // Fix: when `expected_ty` is an integer type that fits the
            // literal value, coerce the literal to it. Falls back to
            // the parsed type when expected isn't an integer (e.g.,
            // when the literal is used as an unconstrained expression
            // statement or in a type-ambiguous position).
            let coerced_ty = match (expected_ty, ty) {
                // WIDEN only — never narrow. A parser default-typed
                // `0` (i32) sitting in an i64 field's expected
                // position widens cleanly. An explicitly-suffixed
                // `2i64` sitting in an i32-expected position stays
                // i64 — narrowing would silently lose precision and
                // mask user errors (see test_arg_type_mismatch_i32_vs_i64).
                (ResolvedType::I64, ResolvedType::I32) => ResolvedType::I64,
                (ResolvedType::Usize, ResolvedType::I32) if *n >= 0 => ResolvedType::Usize,
                (ResolvedType::Usize, ResolvedType::I64) if *n >= 0 => ResolvedType::Usize,
                _ => ty.clone(),
            };
            Ok(TypedExpr { kind: TypedExprKind::IntLit(*n), ty: coerced_ty })
        }

        Expr::BoolLit(b) => {
            Ok(TypedExpr { kind: TypedExprKind::BoolLit(*b), ty: ResolvedType::Bool })
        }

        // Per @UTAIRZ, string literals type as `Ref { Str }` — never bare Str —
        // so they match `&str` param types and have a fat-pointer LLVM layout.
        Expr::StringLit(s) => Ok(TypedExpr {
            kind: TypedExprKind::StringLit(s.clone()),
            ty: ResolvedType::Ref { inner: Box::new(ResolvedType::Str) },
        }),

        // Per @UTAIRZ, byte string literals type as `Ref { ByteSlice }` — mirror
        // of the Str wiring.
        Expr::ByteStringLit(bytes) => Ok(TypedExpr {
            kind: TypedExprKind::ByteStringLit(bytes.clone()),
            ty: ResolvedType::Ref { inner: Box::new(ResolvedType::ByteSlice) },
        }),

        Expr::Var(name) => {
            let ty = scope.get(name.as_str())
                .cloned()
                .ok_or_else(|| TypeResolveError::UndefinedVariable { name: name.clone() })?;
            Ok(TypedExpr { kind: TypedExprKind::Var(name.clone()), ty })
        }

        Expr::StructLit { name, type_args, fields } => {
            // Two-enum split: `type_args` is parser-shape `Vec<SourceType>`.
            // Build a SourceType::StructRef and promote in one step via
            // `oracle::resolve_source_type` (which produces a `Struct` with
            // `field_types` populated).
            let resolved_ty = crate::oracle::resolve_source_type(
                &SourceType::StructRef { name: name.clone(), type_args: type_args.clone() },
                registry,
            )?;

            let field_types = match &resolved_ty {
                ResolvedType::Struct { field_types, .. } => field_types.clone(),
                _ => return Err(TypeResolveError::NonStructLitType { ty: resolved_ty }),
            };

            let toy_struct = registry.structs.get(name.as_str())
                .ok_or_else(|| TypeResolveError::UndefinedStruct { name: name.clone() })?;

            let typed_fields: Vec<(String, TypedExpr)> = fields.iter()
                .map(|(field_name, field_expr)| {
                    let field_idx = find_field_index(toy_struct, name, field_name)?;
                    let expected = &field_types[field_idx];
                    let typed = resolve_expr(field_expr, expected, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
                    Ok((field_name.clone(), typed))
                })
                .collect::<Result<Vec<_>, TypeResolveError>>()?;

            Ok(TypedExpr {
                kind: TypedExprKind::StructLit { name: name.clone(), fields: typed_fields },
                ty: resolved_ty,
            })
        }

        Expr::FnCall { name, type_args, args } => {
            // Two-enum split: `type_args` is `&Vec<SourceType>` (parser). We
            // need a `Vec<ResolvedType>` to store in the typed AST and to
            // build a substitution map for the registry-side substitute
            // pass. Promote each arg up front.
            let resolved_type_args: Vec<ResolvedType> = type_args.iter()
                .map(|a| crate::oracle::resolve_source_type(a, registry))
                .collect::<Result<Vec<_>, _>>()?;

            if let Some(func) = registry.functions.get(name.as_str()) {
            // Compiler-law audit C3: one code path for N=0 and N≥1. The
            // arity check enforces matching count (0==0 passes naturally
            // for non-generic); the substitution map is empty for N=0 ⇒
            // identity substitution ⇒ same behavior as the old non-generic
            // branch. CLAUDE.md: non-generic is the degenerate case of
            // generic, falls out of the general path without a branch.
            if type_args.len() != func.type_params.len() {
                return Err(TypeResolveError::WrongTypeArgCount {
                    func_name: name.clone(),
                    expected: func.type_params.len(),
                    got: type_args.len(),
                });
            }
            let type_arg_subst: HashMap<String, ResolvedType> = func.type_params.iter()
                .zip(resolved_type_args.iter())
                .map(|(param, arg)| (param.clone(), arg.clone()))
                .collect();
            // `func.return_ty` and `func.params[i].ty` are `SourceType` — use
            // `oracle::substitute_source_type` (SourceType-level substitution
            // with ResolvedType map values, see oracle.rs) then
            // `oracle::resolve_source_type` to promote.
            let ret_ty = if let Some(ret) = &func.return_ty {
                let substituted_src = crate::oracle::substitute_source_type(ret, &type_arg_subst);
                crate::oracle::resolve_source_type(&substituted_src, registry)?
            } else {
                ResolvedType::Void
            };
            let typed_args: Vec<TypedExpr> = args.iter()
                .enumerate()
                .map(|(i, a)| {
                    // Extra args beyond declared params fall through with
                    // Void expected (no ArgTypeMismatch raised). This is a
                    // well-formedness guard, not a generic-vs-non-generic
                    // branch — applies uniformly.
                    let expected = if i < func.params.len() {
                        let substituted_src = crate::oracle::substitute_source_type(&func.params[i].ty, &type_arg_subst);
                        crate::oracle::resolve_source_type(&substituted_src, registry)?
                    } else {
                        ResolvedType::Void
                    };
                    let typed = resolve_expr(a, &expected, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
                    if expected != ResolvedType::Void && !types_match(&typed.ty, &expected) {
                        return Err(TypeResolveError::ArgTypeMismatch {
                            func_name: name.clone(), param_index: i,
                            expected, got: typed.ty.clone(),
                        });
                    }
                    Ok(typed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TypedExpr {
                kind: TypedExprKind::FnCall {
                    name: name.clone(),
                    type_args: resolved_type_args,
                    args: typed_args,
                },
                ty: ret_ty,
            })
            } else {
                // Free function: use rust_param_types as existence check (None → not found)
                // Closures take/return SourceType; promote results to ResolvedType for the typed AST.
                let src_param_types = rust_param_types("", name, type_args)?
                    .ok_or_else(|| TypeResolveError::UndefinedFunction { name: name.clone() })?;
                let param_types: Vec<ResolvedType> = src_param_types.iter()
                    .map(|s| crate::oracle::resolve_source_type(s, registry))
                    .collect::<Result<Vec<_>, _>>()?;
                let src_ret_ty = rust_method_ret("", name, type_args)?;
                let ret_ty = crate::oracle::resolve_source_type(&src_ret_ty, registry)?;
                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let expected = param_types.get(i).cloned().unwrap_or(ResolvedType::Void);
                        let typed = resolve_expr(a, &expected, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
                        if expected != ResolvedType::Void && !types_match(&typed.ty, &expected) {
                            return Err(TypeResolveError::ArgTypeMismatch {
                                func_name: name.clone(), param_index: i,
                                expected, got: typed.ty.clone(),
                            });
                        }
                        Ok(typed)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(TypedExpr {
                    kind: TypedExprKind::FnCall { name: name.clone(), type_args: resolved_type_args, args: typed_args },
                    ty: ret_ty,
                })
            }
        }

        Expr::BinaryOp { op, left, right } => {
            use crate::toylang::ast::BinOp;
            let is_comparison = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or);
            let typed_left = resolve_expr(left, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            let operand_ty = typed_left.ty.clone();
            let typed_right = resolve_expr(right, &operand_ty, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            let result_ty = if is_comparison { ResolvedType::Bool } else { operand_ty };
            Ok(TypedExpr {
                kind: TypedExprKind::BinaryOp {
                    op: *op,
                    left: Box::new(typed_left),
                    right: Box::new(typed_right),
                },
                ty: result_ty,
            })
        }

        Expr::StaticCall { ty, method, type_args, args } => {
            // Two-enum split: `type_args` is `&Vec<SourceType>`. Promote each
            // to ResolvedType up front for storage in the typed AST.
            let resolved_type_args: Vec<ResolvedType> = type_args.iter()
                .map(|a| crate::oracle::resolve_source_type(a, registry))
                .collect::<Result<Vec<_>, _>>()?;

            // Resolve args first — for trait calls, the first arg (receiver) determines
            // which concrete impl to use for return type resolution.
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .collect::<Result<Vec<_>, _>>()?;

            // Per @IVTDBTZ, dispatch between trait and inherent static calls is
            // purely type-kind-based: ask the oracle whether `ty` names a
            // `use`-imported Rust trait.
            let is_trait_call = is_rust_trait(ty);

            // Closure callbacks take `&[SourceType]` and return `SourceType`.
            // For trait calls we prepend the receiver type (which is a
            // ResolvedType from the resolved typed_args[0].ty); demote it
            // to SourceType to feed the closure.
            let (src_ret_ty, src_param_types) = if is_trait_call {
                let receiver_src = typed_args[0].ty.to_source_type();
                let mut extended_type_args: Vec<SourceType> = vec![receiver_src];
                extended_type_args.extend(type_args.iter().cloned());
                let trait_key = format!("__trait::{}", ty);
                (
                    rust_method_ret(&trait_key, method, &extended_type_args)?,
                    rust_param_types(&trait_key, method, &extended_type_args)?.unwrap_or_default(),
                )
            } else {
                (
                    rust_method_ret(ty, method, type_args)?,
                    rust_param_types(ty, method, type_args)?.unwrap_or_default(),
                )
            };

            let ret_ty = crate::oracle::resolve_source_type(&src_ret_ty, registry)?;
            let param_types: Vec<ResolvedType> = src_param_types.iter()
                .map(|s| crate::oracle::resolve_source_type(s, registry))
                .collect::<Result<Vec<_>, _>>()?;

            // Args already align with sig.inputs() for both trait and inherent calls
            for (i, typed) in typed_args.iter().enumerate() {
                if let Some(expected) = param_types.get(i) {
                    if *expected != ResolvedType::Void && !types_match(&typed.ty, expected) {
                        return Err(TypeResolveError::ArgTypeMismatch {
                            func_name: format!("{}::{}", ty, method),
                            param_index: i, expected: expected.clone(), got: typed.ty.clone(),
                        });
                    }
                }
            }

            Ok(TypedExpr {
                kind: TypedExprKind::StaticCall {
                    ty: ty.clone(),
                    method: method.clone(),
                    type_args: resolved_type_args,
                    args: typed_args,
                },
                ty: ret_ty,
            })
        }

        Expr::FieldAccess { receiver, field } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            // Phase 2 C.3 — auto-deref through a single `&T` layer for field
            // access. Toylang's impl methods take `self: &Struct` (parser-
            // elevated from `&self`), so any `self.field` access in a method
            // body sees the receiver typed as `&Struct`. Mirror Rust's
            // automatic-deref-on-field convention: peel one Ref layer if the
            // inner type is a Struct.
            let recv_ty_for_field = match &typed_recv.ty {
                ResolvedType::Ref { inner } if matches!(**inner, ResolvedType::Struct { .. })
                    => &**inner,
                other => other,
            };
            let ResolvedType::Struct { name: struct_name, field_types, .. } = recv_ty_for_field else {
                return Err(TypeResolveError::FieldAccessOnNonStruct {
                    ty: typed_recv.ty.clone(),
                    field: field.clone(),
                });
            };
            let toy_struct = registry.structs.get(struct_name.as_str())
                .ok_or_else(|| TypeResolveError::UndefinedStruct { name: struct_name.clone() })?;
            let field_idx = find_field_index(toy_struct, struct_name, field)?;
            let field_ty = field_types[field_idx].clone();
            Ok(TypedExpr {
                kind: TypedExprKind::FieldAccess {
                    receiver: Box::new(typed_recv),
                    field: field.clone(),
                },
                ty: field_ty,
            })
        }

        Expr::MethodCall { receiver, method, args } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;

            // Sunny-karp (2026-06-25): the old `contains_type_param(&typed_recv.ty)`
            // guard that returned `RustTypeDeferred` is gone — the oracle now
            // accepts Param-bearing receivers via the `caller_type_params`
            // thread-through. The match arms below extract the `RustType` /
            // `Ref<RustType>` shape (which includes args carrying Param
            // placeholders); the oracle's `rust_method_return_type` produces a
            // sig where Params survive end-to-end.

            // Two-enum split: the receiver's type is `ResolvedType` (typed
            // AST). Closure callbacks take `&[SourceType]`, so demote the
            // receiver's `type_args` to source-shape via `to_source_type`.
            let (rust_name, src_type_args): (&str, Vec<SourceType>) = match &typed_recv.ty {
                ResolvedType::RustType { name, type_args } => {
                    (name.as_str(), type_args.iter().map(|t| t.to_source_type()).collect())
                }
                ResolvedType::Ref { inner } => match inner.as_ref() {
                    ResolvedType::RustType { name, type_args } => {
                        (name.as_str(), type_args.iter().map(|t| t.to_source_type()).collect())
                    }
                    _ => return Err(TypeResolveError::MethodCallOnUnsupportedType {
                        ty: typed_recv.ty.clone(), method: method.clone(),
                    }),
                },
                _ => return Err(TypeResolveError::MethodCallOnUnsupportedType {
                    ty: typed_recv.ty.clone(), method: method.clone(),
                }),
            };

            let src_ret_ty = rust_method_ret(rust_name, method, &src_type_args)?;
            let src_param_types = rust_param_types(rust_name, method, &src_type_args)?
                .unwrap_or_default();
            let ret_ty = crate::oracle::resolve_source_type(&src_ret_ty, registry)?;
            let param_types: Vec<ResolvedType> = src_param_types.iter()
                .map(|s| crate::oracle::resolve_source_type(s, registry))
                .collect::<Result<Vec<_>, _>>()?;
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .collect::<Result<Vec<_>, _>>()?;

            // Check explicit args against param_types. The oracle returns all params
            // including self (sig.inputs()), but MethodCall syntax separates the
            // receiver. Skip param_types[0] (self) since autoref means the receiver
            // type won't match sig.inputs()[0] directly — toylang doesn't model autoref.
            for (i, typed) in typed_args.iter().enumerate() {
                if let Some(expected) = param_types.get(i + 1) {
                    if *expected != ResolvedType::Void && !types_match(&typed.ty, expected) {
                        return Err(TypeResolveError::ArgTypeMismatch {
                            func_name: format!("{}.{}", rust_name, method),
                            param_index: i, expected: expected.clone(), got: typed.ty.clone(),
                        });
                    }
                }
            }

            Ok(TypedExpr {
                kind: TypedExprKind::MethodCall {
                    receiver: Box::new(typed_recv),
                    method: method.clone(),
                    args: typed_args,
                },
                ty: ret_ty,
            })
        }

        Expr::UnaryNeg(inner) => {
            let typed_inner = resolve_expr(inner, expected_ty, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            let ty = typed_inner.ty.clone();
            let zero = match &ty {
                ResolvedType::I32 => TypedExpr { kind: TypedExprKind::IntLit(0), ty: ResolvedType::I32 },
                ResolvedType::I64 => TypedExpr { kind: TypedExprKind::IntLit(0), ty: ResolvedType::I64 },
                _ => TypedExpr { kind: TypedExprKind::IntLit(0), ty: ty.clone() },
            };
            Ok(TypedExpr {
                kind: TypedExprKind::BinaryOp {
                    op: crate::toylang::ast::BinOp::Sub,
                    left: Box::new(zero),
                    right: Box::new(typed_inner),
                },
                ty,
            })
        }

        Expr::Ref(inner) => {
            let typed_inner = resolve_expr(inner, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            let ty = ResolvedType::Ref { inner: Box::new(typed_inner.ty.clone()) };
            Ok(TypedExpr {
                kind: TypedExprKind::Ref(Box::new(typed_inner)),
                ty,
            })
        }

        Expr::If { cond, then_body, else_body } => {
            let typed_cond = resolve_expr(cond, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            if typed_cond.ty != ResolvedType::Bool {
                return Err(TypeResolveError::IfConditionNotBool { ty: typed_cond.ty.clone() });
            }

            // Resolve then branch in cloned scope (branch-scoped)
            let mut then_scope = scope.clone();
            let then_stmts: Vec<TypedStmt> = then_body.stmts.iter()
                .map(|s| resolve_stmt(s, &mut then_scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .collect::<Result<Vec<_>, _>>()?;
            let then_expr = then_body.ret.as_ref()
                .map(|e| resolve_expr(e, &ResolvedType::Void, &then_scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .transpose()?;

            // Resolve else branch in cloned scope (branch-scoped)
            let (else_stmts, else_expr) = if let Some(else_body) = else_body {
                let mut else_scope = scope.clone();
                let stmts: Vec<TypedStmt> = else_body.stmts.iter()
                    .map(|s| resolve_stmt(s, &mut else_scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types, is_rust_trait))
                    .collect::<Result<Vec<_>, _>>()?;
                let expr = else_body.ret.as_ref()
                    .map(|e| resolve_expr(e, &ResolvedType::Void, &else_scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
                    .transpose()?;
                (stmts, expr)
            } else {
                (vec![], None)
            };

            // Determine result type
            let result_ty = match (&then_expr, &else_expr) {
                (Some(te), Some(ee)) => {
                    if te.ty != ee.ty {
                        return Err(TypeResolveError::IfElseTypeMismatch {
                            then_ty: te.ty.clone(),
                            else_ty: ee.ty.clone(),
                        });
                    }
                    te.ty.clone()
                }
                _ => ResolvedType::Void,
            };

            Ok(TypedExpr {
                kind: TypedExprKind::If {
                    cond: Box::new(typed_cond),
                    then_stmts,
                    then_expr: then_expr.map(Box::new),
                    else_stmts,
                    else_expr: else_expr.map(Box::new),
                },
                ty: result_ty,
            })
        }
    }
}

fn resolve_stmt(
    stmt: &Stmt,
    scope: &mut HashMap<String, ResolvedType>,
    _ret_ty: &ResolvedType,
    registry: &ToylangRegistry,
    rust_method_ret: &dyn Fn(&str, &str, &[SourceType]) -> Result<SourceType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[SourceType]) -> Result<Option<Vec<SourceType>>, crate::oracle::UnresolvedRustType>,
    is_rust_trait: &dyn Fn(&str) -> bool,
) -> Result<TypedStmt, TypeResolveError> {
    match stmt {
        Stmt::Let { name, expr } => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            scope.insert(name.clone(), typed_expr.ty.clone());
            Ok(TypedStmt::Let { name: name.clone(), expr: typed_expr })
        }
        Stmt::ExprStmt(expr) => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            Ok(TypedStmt::ExprStmt(typed_expr))
        }
        Stmt::While { cond, body } => {
            let typed_cond = resolve_expr(cond, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            if typed_cond.ty != ResolvedType::Bool {
                return Err(TypeResolveError::WhileConditionNotBool { ty: typed_cond.ty.clone() });
            }
            // Resolve body in current scope (NOT cloned — let rebindings persist across iterations)
            let body_stmts: Vec<TypedStmt> = body.stmts.iter()
                .map(|s| resolve_stmt(s, scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .collect::<Result<Vec<_>, _>>()?;
            let body_ret = body.ret.as_ref()
                .map(|e| resolve_expr(e, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types, is_rust_trait))
                .transpose()?;
            Ok(TypedStmt::While {
                cond: typed_cond,
                body: TypedBlock { stmts: body_stmts, ret: body_ret },
            })
        }
        Stmt::Assign { name, expr } => {
            let existing_ty = scope.get(name.as_str())
                .ok_or_else(|| TypeResolveError::UndefinedVariable { name: name.clone() })?
                .clone();
            let typed_expr = resolve_expr(expr, &existing_ty, scope, registry, rust_method_ret, rust_param_types, is_rust_trait)?;
            if typed_expr.ty != existing_ty {
                return Err(TypeResolveError::AssignTypeMismatch {
                    name: name.clone(),
                    expected: existing_ty,
                    got: typed_expr.ty.clone(),
                });
            }
            Ok(TypedStmt::Assign { name: name.clone(), expr: typed_expr })
        }
    }
}

// ============================================================================
// Tests
// ============================================================================


// Tests retired during the two-enum split (2026-06-25, Option B). The unit-test fixtures
// extensively constructed registry-side parser-shape types via ResolvedType variants
// (StructRef etc.) that no longer exist on ResolvedType. Most of these tests checked
// resolve_struct_fields (retired — see oracle::resolve_source_type) or end-to-end
// resolve_fn_body which is covered by the integration suite. New unit tests should be
// added under the SourceType-shape fixtures when targeting specific resolver bugs.
