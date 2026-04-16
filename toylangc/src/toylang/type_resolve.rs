//! Type resolution pass — annotates every AST expression with its concrete type.
//!
//! Runs after parsing, before LLVM codegen. Walks the untyped AST and produces
//! a TypedBlock where every expression carries a ResolvedType.

use std::collections::HashMap;

use super::ast::{Expr, Stmt};
#[cfg(test)]
use super::ast::Block;
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
}

impl From<crate::oracle::UnresolvedRustType> for TypeResolveError {
    fn from(e: crate::oracle::UnresolvedRustType) -> Self {
        TypeResolveError::RustTypeNotImported {
            name: e.name,
            context: e.context.to_string(),
        }
    }
}

// ============================================================================
// Public entry point
// ============================================================================

/// Resolve all types in a function body, producing a TypedBlock.
/// Resolve just the return type of a function without resolving the full body.
pub fn resolve_return_type(
    registry: &ToylangRegistry,
    func: &ToyFunction,
) -> Result<ResolvedType, TypeResolveError> {
    match func.return_ty.as_ref() {
        Some(rt) => resolve_struct_fields(rt, registry),
        None => Ok(ResolvedType::Void),
    }
}

pub fn resolve_fn_body(
    registry: &ToylangRegistry,
    func: &ToyFunction,
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<ResolvedType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType>,
) -> Result<TypedBlock, TypeResolveError> {
    let body = func.body.as_ref().expect("function has no body");
    let ret_ty = match func.return_ty.as_ref() {
        Some(rt) => resolve_struct_fields(rt, registry)?,
        None => ResolvedType::Void,
    };

    let mut scope: HashMap<String, ResolvedType> = HashMap::new();

    // Add function parameters to scope (resolve StructRef → Struct)
    for p in &func.params {
        scope.insert(p.name.clone(), resolve_struct_fields(&p.ty, registry)?);
    }

    // Resolve statements
    let stmts: Vec<TypedStmt> = body.stmts.iter()
        .map(|stmt| resolve_stmt(stmt, &mut scope, &ret_ty, registry, rust_method_ret, rust_param_types))
        .collect::<Result<Vec<_>, _>>()?;

    // Resolve return expression
    let ret = body.ret.as_ref()
        .map(|expr| resolve_expr(expr, &ret_ty, &scope, registry, rust_method_ret, rust_param_types))
        .transpose()?;

    Ok(TypedBlock { stmts, ret })
}

// ============================================================================
// Type string parsing
// ============================================================================

/// Parse a type string like "i32", "Pair<i32, i64>", "Vec<Point>", "&Vec<Point>"
/// into a ResolvedType. Uses the registry to resolve struct names.
/// Convert a `StructRef` → `Struct` by looking up fields in the registry.
/// Recursively resolves nested struct references.
pub fn resolve_struct_fields(ty: &ResolvedType, registry: &ToylangRegistry) -> Result<ResolvedType, TypeResolveError> {
    match ty {
        ResolvedType::StructRef { name, type_args } => {
            let toy_struct = registry.structs.get(name.as_str())
                .ok_or_else(|| TypeResolveError::UndefinedStruct { name: name.clone() })?;
            // Build substitution map from type_args
            let subst: HashMap<String, ResolvedType> = toy_struct.type_params.iter()
                .zip(type_args.iter())
                .map(|(param, arg)| (param.clone(), arg.clone()))
                .collect();
            let resolved_fields: Vec<ResolvedType> = toy_struct.fields.iter()
                .map(|f| {
                    let substituted = substitute_type_params(&f.rust_type, &subst);
                    resolve_struct_fields(&substituted, registry)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ResolvedType::Struct {
                name: name.clone(),
                type_args: type_args.clone(),
                field_types: resolved_fields,
            })
        }
        ResolvedType::Ref { inner } => {
            Ok(ResolvedType::Ref { inner: Box::new(resolve_struct_fields(inner, registry)?) })
        }
        other => Ok(other.clone()),
    }
}

/// Semantic type equality. Handles StructRef vs Struct equivalence: both represent the same
/// type, but StructRef is the parser's unresolved form and Struct is the resolved form with
/// field_types filled in. The oracle produces StructRef (via rustc_ty_to_resolved_type) while
/// the type resolver produces Struct (via resolve_struct_fields).
fn types_match(a: &ResolvedType, b: &ResolvedType) -> bool {
    match (a, b) {
        (ResolvedType::StructRef { name: na, type_args: ta },
         ResolvedType::Struct { name: nb, type_args: tb, .. }) |
        (ResolvedType::Struct { name: na, type_args: ta, .. },
         ResolvedType::StructRef { name: nb, type_args: tb }) => {
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
        ResolvedType::StructRef { name, type_args } => {
            ResolvedType::StructRef {
                name: name.clone(),
                type_args: type_args.iter().map(|a| substitute_type_params(a, subst)).collect(),
            }
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

/// Walk an AST body and substitute TypeParams in all embedded type_args.
/// Used when monomorphizing a generic function before type resolution.
pub fn substitute_type_params_in_body(
    body: &super::ast::Block,
    subst: &HashMap<String, ResolvedType>,
) -> super::ast::Block {
    use super::ast::*;
    fn subst_expr(expr: &Expr, subst: &HashMap<String, ResolvedType>) -> Expr {
        match expr {
            Expr::IntLit(n, ty) => Expr::IntLit(*n, super::type_resolve::substitute_type_params(ty, subst)),
            Expr::BoolLit(b) => Expr::BoolLit(*b),
            Expr::StringLit(s) => Expr::StringLit(s.clone()),
            Expr::ByteStringLit(b) => Expr::ByteStringLit(b.clone()),
            Expr::Var(name) => Expr::Var(name.clone()),
            Expr::StructLit { name, type_args, fields } => Expr::StructLit {
                name: name.clone(),
                type_args: type_args.iter().map(|ta| super::type_resolve::substitute_type_params(ta, subst)).collect(),
                fields: fields.iter().map(|(n, e)| (n.clone(), subst_expr(e, subst))).collect(),
            },
            Expr::FnCall { name, type_args, args } => Expr::FnCall {
                name: name.clone(),
                type_args: type_args.iter().map(|ta| super::type_resolve::substitute_type_params(ta, subst)).collect(),
                args: args.iter().map(|a| subst_expr(a, subst)).collect(),
            },
            Expr::StaticCall { ty, method, type_args, args } => Expr::StaticCall {
                ty: ty.clone(),
                method: method.clone(),
                type_args: type_args.iter().map(|ta| super::type_resolve::substitute_type_params(ta, subst)).collect(),
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
    _expected_ty: &ResolvedType,
    scope: &HashMap<String, ResolvedType>,
    registry: &ToylangRegistry,
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<ResolvedType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType>,
) -> Result<TypedExpr, TypeResolveError> {
    match expr {
        Expr::IntLit(n, ty) => {
            Ok(TypedExpr { kind: TypedExprKind::IntLit(*n), ty: ty.clone() })
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
            let resolved_ty = resolve_struct_fields(
                &ResolvedType::StructRef { name: name.clone(), type_args: type_args.clone() },
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
                    let typed = resolve_expr(field_expr, expected, scope, registry, rust_method_ret, rust_param_types)?;
                    Ok((field_name.clone(), typed))
                })
                .collect::<Result<Vec<_>, TypeResolveError>>()?;

            Ok(TypedExpr {
                kind: TypedExprKind::StructLit { name: name.clone(), fields: typed_fields },
                ty: resolved_ty,
            })
        }

        Expr::FnCall { name, type_args, args } => {
            if let Some(func) = registry.functions.get(name.as_str()) {
            if !func.type_params.is_empty() {
                if type_args.len() != func.type_params.len() {
                    return Err(TypeResolveError::WrongTypeArgCount {
                        func_name: name.clone(),
                        expected: func.type_params.len(),
                        got: type_args.len(),
                    });
                }

                let type_arg_subst: HashMap<String, ResolvedType> = func.type_params.iter()
                    .zip(type_args.iter())
                    .map(|(param, arg)| (param.clone(), arg.clone()))
                    .collect();

                let ret_ty = if let Some(ret) = &func.return_ty {
                    let substituted = substitute_type_params(ret, &type_arg_subst);
                    resolve_struct_fields(&substituted, registry)?
                } else {
                    ResolvedType::Void
                };

                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let substituted = substitute_type_params(&func.params[i].ty, &type_arg_subst);
                        let expected = resolve_struct_fields(&substituted, registry)?;
                        let typed = resolve_expr(a, &expected, scope, registry, rust_method_ret, rust_param_types)?;
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
                        type_args: type_args.clone(),
                        args: typed_args,
                    },
                    ty: ret_ty,
                })
            } else {
                let ret_ty = match func.return_ty.as_ref() {
                    Some(rt) => resolve_struct_fields(rt, registry)?,
                    None => ResolvedType::Void,
                };
                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let expected = if i < func.params.len() {
                            resolve_struct_fields(&func.params[i].ty, registry)?
                        } else {
                            ResolvedType::Void
                        };
                        let typed = resolve_expr(a, &expected, scope, registry, rust_method_ret, rust_param_types)?;
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
                    kind: TypedExprKind::FnCall { name: name.clone(), type_args: vec![], args: typed_args },
                    ty: ret_ty,
                })
            }
            } else {
                // Free function: use rust_param_types as existence check (None → not found)
                let param_types = rust_param_types("", name, type_args)?
                    .ok_or_else(|| TypeResolveError::UndefinedFunction { name: name.clone() })?;
                let ret_ty = rust_method_ret("", name, type_args)?;
                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let expected = param_types.get(i).cloned().unwrap_or(ResolvedType::Void);
                        let typed = resolve_expr(a, &expected, scope, registry, rust_method_ret, rust_param_types)?;
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
                    kind: TypedExprKind::FnCall { name: name.clone(), type_args: type_args.clone(), args: typed_args },
                    ty: ret_ty,
                })
            }
        }

        Expr::BinaryOp { op, left, right } => {
            use crate::toylang::ast::BinOp;
            let is_comparison = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or);
            let typed_left = resolve_expr(left, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            let operand_ty = typed_left.ty.clone();
            let typed_right = resolve_expr(right, &operand_ty, scope, registry, rust_method_ret, rust_param_types)?;
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
            // Resolve args first — for trait calls, the first arg (receiver) determines
            // which concrete impl to use for return type resolution.
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types))
                .collect::<Result<Vec<_>, _>>()?;

            // Try inherent method first (e.g., Vec::new). If that fails (returns Void
            // and no type found), it might be a trait call (e.g., Write::write_all).
            // The callback returns the return type; for trait calls it uses a
            // "__trait::" prefix convention to signal the oracle.
            let is_trait_call = !typed_args.is_empty() && {
                // If `ty` is not a known struct/type, it might be a trait
                !registry.structs.contains_key(ty.as_str())
            };

            let (ret_ty, param_types) = if is_trait_call {
                // Trait call: pass "__trait::TraitName" as the type_name,
                // with the receiver type appended to type_args
                let receiver_ty = &typed_args[0].ty;
                let mut extended_type_args = vec![receiver_ty.clone()];
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
                    type_args: type_args.clone(),
                    args: typed_args,
                },
                ty: ret_ty,
            })
        }

        Expr::FieldAccess { receiver, field } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            let ResolvedType::Struct { name: struct_name, field_types, .. } = &typed_recv.ty else {
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
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;

            let (rust_name, rust_type_args) = match &typed_recv.ty {
                ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                ResolvedType::Ref { inner } => match inner.as_ref() {
                    ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                    _ => return Err(TypeResolveError::MethodCallOnUnsupportedType {
                        ty: typed_recv.ty.clone(), method: method.clone(),
                    }),
                },
                _ => return Err(TypeResolveError::MethodCallOnUnsupportedType {
                    ty: typed_recv.ty.clone(), method: method.clone(),
                }),
            };

            let ret_ty = rust_method_ret(rust_name, method, rust_type_args)?;
            let param_types = rust_param_types(rust_name, method, rust_type_args)?
                .unwrap_or_default();
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types))
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
            let typed_inner = resolve_expr(inner, _expected_ty, scope, registry, rust_method_ret, rust_param_types)?;
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
            let typed_inner = resolve_expr(inner, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            let ty = ResolvedType::Ref { inner: Box::new(typed_inner.ty.clone()) };
            Ok(TypedExpr {
                kind: TypedExprKind::Ref(Box::new(typed_inner)),
                ty,
            })
        }

        Expr::If { cond, then_body, else_body } => {
            let typed_cond = resolve_expr(cond, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            if typed_cond.ty != ResolvedType::Bool {
                return Err(TypeResolveError::IfConditionNotBool { ty: typed_cond.ty.clone() });
            }

            // Resolve then branch in cloned scope (branch-scoped)
            let mut then_scope = scope.clone();
            let then_stmts: Vec<TypedStmt> = then_body.stmts.iter()
                .map(|s| resolve_stmt(s, &mut then_scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types))
                .collect::<Result<Vec<_>, _>>()?;
            let then_expr = then_body.ret.as_ref()
                .map(|e| resolve_expr(e, &ResolvedType::Void, &then_scope, registry, rust_method_ret, rust_param_types))
                .transpose()?;

            // Resolve else branch in cloned scope (branch-scoped)
            let (else_stmts, else_expr) = if let Some(else_body) = else_body {
                let mut else_scope = scope.clone();
                let stmts: Vec<TypedStmt> = else_body.stmts.iter()
                    .map(|s| resolve_stmt(s, &mut else_scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types))
                    .collect::<Result<Vec<_>, _>>()?;
                let expr = else_body.ret.as_ref()
                    .map(|e| resolve_expr(e, &ResolvedType::Void, &else_scope, registry, rust_method_ret, rust_param_types))
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
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<ResolvedType, crate::oracle::UnresolvedRustType>,
    rust_param_types: &dyn Fn(&str, &str, &[ResolvedType]) -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType>,
) -> Result<TypedStmt, TypeResolveError> {
    match stmt {
        Stmt::Let { name, expr } => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            scope.insert(name.clone(), typed_expr.ty.clone());
            Ok(TypedStmt::Let { name: name.clone(), expr: typed_expr })
        }
        Stmt::ExprStmt(expr) => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            Ok(TypedStmt::ExprStmt(typed_expr))
        }
        Stmt::While { cond, body } => {
            let typed_cond = resolve_expr(cond, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types)?;
            if typed_cond.ty != ResolvedType::Bool {
                return Err(TypeResolveError::WhileConditionNotBool { ty: typed_cond.ty.clone() });
            }
            // Resolve body in current scope (NOT cloned — let rebindings persist across iterations)
            let body_stmts: Vec<TypedStmt> = body.stmts.iter()
                .map(|s| resolve_stmt(s, scope, &ResolvedType::Void, registry, rust_method_ret, rust_param_types))
                .collect::<Result<Vec<_>, _>>()?;
            let body_ret = body.ret.as_ref()
                .map(|e| resolve_expr(e, &ResolvedType::Void, scope, registry, rust_method_ret, rust_param_types))
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
            let typed_expr = resolve_expr(expr, &existing_ty, scope, registry, rust_method_ret, rust_param_types)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toylang::registry::*;

    /// Test callback for Rust method return types.
    fn test_rust_method_ret(_type_name: &str, method: &str, _type_args: &[ResolvedType]) -> Result<ResolvedType, crate::oracle::UnresolvedRustType> {
        Ok(match method {
            "new" => ResolvedType::RustType {
                name: _type_name.to_string(),
                type_args: _type_args.to_vec(),
            },
            "push" => ResolvedType::Void,
            "len" => ResolvedType::Usize,
            _ => panic!("unknown Rust method '{}' in test", method),
        })
    }

    /// Test callback for Rust param types. Returns Ok(None) for unknown methods.
    /// Returns sig.inputs() including self — self is just another param.
    fn test_rust_param_types(type_name: &str, method: &str, type_args: &[ResolvedType]) -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType> {
        let self_ref = || ResolvedType::Ref {
            inner: Box::new(ResolvedType::RustType {
                name: type_name.to_string(),
                type_args: type_args.to_vec(),
            }),
        };
        Ok(match method {
            "new" => Some(vec![]),  // associated fn, no self
            "push" => Some(vec![self_ref(), ResolvedType::I32]),
            "len" => Some(vec![self_ref()]),
            _ => None,
        })
    }

    fn make_registry() -> ToylangRegistry {
        let mut structs = std::collections::HashMap::new();
        structs.insert("Counter".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField { name: "value".to_string(), rust_type: ResolvedType::I32 }],
        });
        structs.insert("Point".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![
                ToyField { name: "x".to_string(), rust_type: ResolvedType::I32 },
                ToyField { name: "y".to_string(), rust_type: ResolvedType::I32 },
            ],
        });
        structs.insert("Pair".to_string(), ToyStruct {
            type_params: vec!["A".to_string(), "B".to_string()],
            fields: vec![
                ToyField { name: "first".to_string(), rust_type: ResolvedType::TypeParam("A".to_string()) },
                ToyField { name: "second".to_string(), rust_type: ResolvedType::TypeParam("B".to_string()) },
            ],
        });
        structs.insert("ToyInner".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField { name: "x".to_string(), rust_type: ResolvedType::I32 }],
        });
        structs.insert("ToyOuter".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField { name: "inner".to_string(), rust_type: ResolvedType::StructRef { name: "ToyInner".to_string(), type_args: vec![] } }],
        });
        structs.insert("ToyShip".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField {
                name: "wings".to_string(),
                rust_type: ResolvedType::RustType { name: "Vec".to_string(), type_args: vec![ResolvedType::I32] },
            }],
        });
        let mut functions = std::collections::HashMap::new();
        functions.insert("wrap".to_string(), ToyFunction {

            type_params: vec!["T".to_string()],
            params: vec![ToyParam { name: "x".to_string(), ty: ResolvedType::TypeParam("T".to_string()) }],
            return_ty: Some(ResolvedType::TypeParam("T".to_string())),
            body: Some(Block { stmts: vec![], ret: Some(Expr::Var("x".to_string())) }),
        });
        ToylangRegistry { structs, functions, imports: vec![] }
    }

    #[test]
    fn test_resolve_struct_fields_simple() {
        let reg = make_registry();
        let ty = resolve_struct_fields(
            &ResolvedType::StructRef { name: "Counter".to_string(), type_args: vec![] },
            &reg,
        ).unwrap();
        assert!(matches!(ty, ResolvedType::Struct { ref name, ref field_types, .. }
            if name == "Counter" && field_types == &[ResolvedType::I32]));
    }

    #[test]
    fn test_resolve_struct_fields_generic() {
        let reg = make_registry();
        let ty = resolve_struct_fields(
            &ResolvedType::StructRef {
                name: "Pair".to_string(),
                type_args: vec![ResolvedType::I32, ResolvedType::I64],
            },
            &reg,
        ).unwrap();
        match ty {
            ResolvedType::Struct { name, field_types, .. } => {
                assert_eq!(name, "Pair");
                assert_eq!(field_types, vec![ResolvedType::I32, ResolvedType::I64]);
            }
            _ => panic!("expected Struct, got {:?}", ty),
        }
    }

    #[test]
    fn test_resolve_struct_fields_ref() {
        let reg = make_registry();
        // resolve_struct_fields on a Ref containing a StructRef
        let ty = resolve_struct_fields(
            &ResolvedType::Ref {
                inner: Box::new(ResolvedType::StructRef { name: "Point".to_string(), type_args: vec![] }),
            },
            &reg,
        ).unwrap();
        match ty {
            ResolvedType::Ref { inner } => {
                assert!(matches!(*inner, ResolvedType::Struct { ref name, .. } if name == "Point"));
            }
            _ => panic!("expected Ref, got {:?}", ty),
        }
    }

    #[test]
    fn test_substitute_type_params() {
        let mut subst = HashMap::new();
        subst.insert("T".to_string(), ResolvedType::I32);
        let result = substitute_type_params(&ResolvedType::TypeParam("T".to_string()), &subst);
        assert_eq!(result, ResolvedType::I32);
    }

    #[test]
    fn test_primitives_pass_through() {
        // Primitives are already resolved, no struct fields to fill in
        let reg = make_registry();
        assert_eq!(resolve_struct_fields(&ResolvedType::I32, &reg).unwrap(), ResolvedType::I32);
        assert_eq!(resolve_struct_fields(&ResolvedType::Bool, &reg).unwrap(), ResolvedType::Bool);
    }

    #[test]
    fn test_resolve_int_lit() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::IntLit(42, ResolvedType::I32)) }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        assert!(matches!(ret.kind, TypedExprKind::IntLit(42)));
        assert_eq!(ret.ty, ResolvedType::I32);
    }

    #[test]
    fn test_resolve_generic_struct_lit() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::StructRef { name: "Pair".to_string(), type_args: vec![ResolvedType::I32, ResolvedType::I64] }),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "Pair".to_string(),
                    type_args: vec![ResolvedType::I32, ResolvedType::I64],
                    fields: vec![
                        ("first".to_string(), Expr::IntLit(10, ResolvedType::I32)),
                        ("second".to_string(), Expr::IntLit(20, ResolvedType::I64)),
                    ],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        // The struct should be resolved as Pair with [I32, I64]
        match &ret.ty {
            ResolvedType::Struct { name, field_types, .. } => {
                assert_eq!(name, "Pair");
                assert_eq!(field_types, &[ResolvedType::I32, ResolvedType::I64]);
            }
            _ => panic!("expected Struct, got {:?}", ret.ty),
        }
        // The fields should have correct types
        if let TypedExprKind::StructLit { fields, .. } = &ret.kind {
            assert_eq!(fields[0].1.ty, ResolvedType::I32);
            assert_eq!(fields[1].1.ty, ResolvedType::I64);
        } else {
            panic!("expected StructLit");
        }
    }

    #[test]
    fn test_resolve_var_from_param() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![ToyParam { name: "x".to_string(), ty: ResolvedType::I32 }],
            return_ty: Some(ResolvedType::StructRef { name: "Counter".to_string(), type_args: vec![] }),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "Counter".to_string(),
                    type_args: vec![],
                    fields: vec![("value".to_string(), Expr::Var("x".to_string()))],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        if let TypedExprKind::StructLit { fields, .. } = &ret.kind {
            assert_eq!(fields[0].1.ty, ResolvedType::I32);
        }
    }

    #[test]
    fn test_resolve_nested_struct() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::StructRef { name: "ToyOuter".to_string(), type_args: vec![] }),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "ToyOuter".to_string(),
                    type_args: vec![],
                    fields: vec![
                        ("inner".to_string(), Expr::StructLit {
                            name: "ToyInner".to_string(),
                            type_args: vec![],
                            fields: vec![("x".to_string(), Expr::IntLit(42, ResolvedType::I32))],
                        }),
                    ],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        if let TypedExprKind::StructLit { fields, .. } = &ret.kind {
            let inner = &fields[0].1;
            assert!(matches!(&inner.ty, ResolvedType::Struct { name, .. } if name == "ToyInner"));
            if let TypedExprKind::StructLit { fields: inner_fields, .. } = &inner.kind {
                assert_eq!(inner_fields[0].1.ty, ResolvedType::I32);
            }
        }
    }

    #[test]
    fn test_resolve_struct_with_vec_field() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::StructRef { name: "ToyShip".to_string(), type_args: vec![] }),
            body: Some(Block {
                stmts: vec![
                    Stmt::Let {
                        name: "v".to_string(),
                        expr: Expr::StaticCall { ty: "Vec".to_string(), method: "new".to_string(), type_args: vec![ResolvedType::I32], args: vec![] },
                    },
                    Stmt::ExprStmt(Expr::MethodCall {
                        receiver: Box::new(Expr::Var("v".to_string())),
                        method: "push".to_string(),
                        args: vec![Expr::IntLit(1, ResolvedType::I32)],
                    }),
                ],
                ret: Some(Expr::StructLit {
                    name: "ToyShip".to_string(),
                    type_args: vec![],
                    fields: vec![("wings".to_string(), Expr::Var("v".to_string()))],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();

        // Vec::new should be typed as Vec<I32>
        if let TypedStmt::Let { expr, .. } = &typed.stmts[0] {
            assert!(matches!(&expr.ty, ResolvedType::RustType { ref name, ref type_args }
                if name == "Vec" && type_args == &[ResolvedType::I32]));
        }

        // push arg should be I32
        if let TypedStmt::ExprStmt(expr) = &typed.stmts[1] {
            if let TypedExprKind::MethodCall { args, .. } = &expr.kind {
                assert_eq!(args[0].ty, ResolvedType::I32);
            }
        }
    }

    #[test]
    fn test_undefined_variable_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::Var("x".to_string())) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::UndefinedVariable { name }) = result else { panic!("expected UndefinedVariable error") };
        assert_eq!(name, "x");
    }

    #[test]
    fn test_undefined_struct_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::StructRef { name: "Nonexistent".to_string(), type_args: vec![] }),
            body: Some(Block { stmts: vec![], ret: Some(Expr::IntLit(1, ResolvedType::I32)) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::UndefinedStruct { name }) = result else { panic!("expected UndefinedStruct error") };
        assert_eq!(name, "Nonexistent");
    }

    #[test]
    fn test_undefined_function_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::FnCall {
                name: "nonexistent".to_string(), type_args: vec![], args: vec![],
            }) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::UndefinedFunction { name }) = result else { panic!("expected UndefinedFunction error") };
        assert_eq!(name, "nonexistent");
    }

    #[test]
    fn test_field_not_found_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::StructRef { name: "Counter".to_string(), type_args: vec![] }),
            body: Some(Block { stmts: vec![], ret: Some(Expr::StructLit {
                name: "Counter".to_string(),
                type_args: vec![],
                fields: vec![("nonexistent".to_string(), Expr::IntLit(1, ResolvedType::I32))],
            }) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::FieldNotFound { struct_name, field_name }) = result else { panic!("expected FieldNotFound error") };
        assert_eq!(struct_name, "Counter");
        assert_eq!(field_name, "nonexistent");
    }

    #[test]
    fn test_undefined_struct_in_struct_lit_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::StructLit {
                name: "Nonexistent".to_string(),
                type_args: vec![],
                fields: vec![],
            }) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::UndefinedStruct { name }) = result else { panic!("expected UndefinedStruct error") };
        assert_eq!(name, "Nonexistent");
    }

    #[test]
    fn test_field_not_found_in_field_access_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Let {
                    name: "c".to_string(),
                    expr: Expr::StructLit {
                        name: "Counter".to_string(),
                        type_args: vec![],
                        fields: vec![("value".to_string(), Expr::IntLit(1, ResolvedType::I32))],
                    },
                }],
                ret: Some(Expr::FieldAccess {
                    receiver: Box::new(Expr::Var("c".to_string())),
                    field: "nonexistent".to_string(),
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::FieldNotFound { struct_name, field_name }) = result else { panic!("expected FieldNotFound error") };
        assert_eq!(struct_name, "Counter");
        assert_eq!(field_name, "nonexistent");
    }

    #[test]
    fn test_field_access_on_non_struct_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Let {
                    name: "x".to_string(),
                    expr: Expr::IntLit(42, ResolvedType::I32),
                }],
                ret: Some(Expr::FieldAccess {
                    receiver: Box::new(Expr::Var("x".to_string())),
                    field: "foo".to_string(),
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::FieldAccessOnNonStruct { ty, field }) = result else { panic!("expected FieldAccessOnNonStruct error") };
        assert_eq!(ty, ResolvedType::I32);
        assert_eq!(field, "foo");
    }

    #[test]
    fn test_method_call_on_struct_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Let {
                    name: "c".to_string(),
                    expr: Expr::StructLit {
                        name: "Counter".to_string(),
                        type_args: vec![],
                        fields: vec![("value".to_string(), Expr::IntLit(1, ResolvedType::I32))],
                    },
                }],
                ret: Some(Expr::MethodCall {
                    receiver: Box::new(Expr::Var("c".to_string())),
                    method: "push".to_string(),
                    args: vec![Expr::IntLit(1, ResolvedType::I32)],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::MethodCallOnUnsupportedType { method, .. }) = result else { panic!("expected MethodCallOnUnsupportedType error") };
        assert_eq!(method, "push");
    }

    #[test]
    fn test_method_call_on_primitive_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Let {
                    name: "x".to_string(),
                    expr: Expr::IntLit(42, ResolvedType::I32),
                }],
                ret: Some(Expr::MethodCall {
                    receiver: Box::new(Expr::Var("x".to_string())),
                    method: "push".to_string(),
                    args: vec![Expr::IntLit(1, ResolvedType::I32)],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::MethodCallOnUnsupportedType { ty, method }) = result else { panic!("expected MethodCallOnUnsupportedType error") };
        assert_eq!(ty, ResolvedType::I32);
        assert_eq!(method, "push");
    }

    #[test]
    fn test_wrong_type_arg_count_too_many_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::FnCall {
                name: "wrap".to_string(),
                type_args: vec![ResolvedType::I32, ResolvedType::I64],
                args: vec![Expr::IntLit(42, ResolvedType::I32)],
            }) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::WrongTypeArgCount { func_name, expected, got }) = result else { panic!("expected WrongTypeArgCount error") };
        assert_eq!(func_name, "wrap");
        assert_eq!(expected, 1);
        assert_eq!(got, 2);
    }

    #[test]
    fn test_wrong_type_arg_count_too_few_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: Some(Expr::FnCall {
                name: "wrap".to_string(),
                type_args: vec![],
                args: vec![Expr::IntLit(42, ResolvedType::I32)],
            }) }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::WrongTypeArgCount { func_name, expected, got }) = result else { panic!("expected WrongTypeArgCount error") };
        assert_eq!(func_name, "wrap");
        assert_eq!(expected, 1);
        assert_eq!(got, 0);
    }

    #[test]
    fn test_if_condition_not_bool_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::If {
                    cond: Box::new(Expr::IntLit(42, ResolvedType::I32)),
                    then_body: Box::new(Block { stmts: vec![], ret: Some(Expr::IntLit(1, ResolvedType::I32)) }),
                    else_body: Some(Box::new(Block { stmts: vec![], ret: Some(Expr::IntLit(0, ResolvedType::I32)) })),
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::IfConditionNotBool { ty }) = result else { panic!("expected IfConditionNotBool error") };
        assert_eq!(ty, ResolvedType::I32);
    }

    #[test]
    fn test_while_condition_not_bool_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::While {
                    cond: Expr::IntLit(42, ResolvedType::I32),
                    body: Box::new(Block { stmts: vec![], ret: None }),
                }],
                ret: Some(Expr::IntLit(0, ResolvedType::I32)),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::WhileConditionNotBool { ty }) = result else { panic!("expected WhileConditionNotBool error") };
        assert_eq!(ty, ResolvedType::I32);
    }

    #[test]
    fn test_if_else_type_mismatch_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::If {
                    cond: Box::new(Expr::BoolLit(true)),
                    then_body: Box::new(Block { stmts: vec![], ret: Some(Expr::IntLit(1, ResolvedType::I32)) }),
                    else_body: Some(Box::new(Block { stmts: vec![], ret: Some(Expr::BoolLit(true)) })),
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::IfElseTypeMismatch { then_ty, else_ty }) = result else { panic!("expected IfElseTypeMismatch error") };
        assert_eq!(then_ty, ResolvedType::I32);
        assert_eq!(else_ty, ResolvedType::Bool);
    }

    #[test]
    fn test_assign_type_mismatch_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![
                    Stmt::Let { name: "x".to_string(), expr: Expr::IntLit(0, ResolvedType::I32) },
                    Stmt::Assign { name: "x".to_string(), expr: Expr::BoolLit(true) },
                ],
                ret: Some(Expr::Var("x".to_string())),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::AssignTypeMismatch { name, expected, got }) = result
            else { panic!("expected AssignTypeMismatch error") };
        assert_eq!(name, "x");
        assert_eq!(expected, ResolvedType::I32);
        assert_eq!(got, ResolvedType::Bool);
    }

    // Helper: a registry with a non-generic fn `add(x: i32, y: i32) -> i32`
    fn make_registry_with_add() -> ToylangRegistry {
        let mut reg = make_registry();
        reg.functions.insert("add".to_string(), ToyFunction {
            type_params: vec![],
            params: vec![
                ToyParam { name: "x".to_string(), ty: ResolvedType::I32 },
                ToyParam { name: "y".to_string(), ty: ResolvedType::I32 },
            ],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::Var("x".to_string())),
            }),
        });
        reg
    }

    #[test]
    fn test_arg_type_mismatch_i32_vs_i64() {
        let reg = make_registry_with_add();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::IntLit(1, ResolvedType::I32),
                        Expr::IntLit(2, ResolvedType::I64), // wrong: i64 where i32 expected
                    ],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::ArgTypeMismatch { func_name, param_index, expected, got }) = result
            else { panic!("expected ArgTypeMismatch, got {:?}", result) };
        assert_eq!(func_name, "add");
        assert_eq!(param_index, 1);
        assert_eq!(expected, ResolvedType::I32);
        assert_eq!(got, ResolvedType::I64);
    }

    #[test]
    fn test_arg_type_mismatch_bool_vs_i32() {
        let reg = make_registry_with_add();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::BoolLit(true), // wrong: bool where i32 expected
                        Expr::IntLit(2, ResolvedType::I32),
                    ],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::ArgTypeMismatch { param_index, expected, got, .. }) = result
            else { panic!("expected ArgTypeMismatch, got {:?}", result) };
        assert_eq!(param_index, 0);
        assert_eq!(expected, ResolvedType::I32);
        assert_eq!(got, ResolvedType::Bool);
    }

    #[test]
    fn test_arg_type_mismatch_generic_fn() {
        // wrap<i32>(true) — passes bool where i32 expected after substitution
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "wrap".to_string(),
                    type_args: vec![ResolvedType::I32],
                    args: vec![Expr::BoolLit(true)],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::ArgTypeMismatch { func_name, param_index, expected, got }) = result
            else { panic!("expected ArgTypeMismatch, got {:?}", result) };
        assert_eq!(func_name, "wrap");
        assert_eq!(param_index, 0);
        assert_eq!(expected, ResolvedType::I32);
        assert_eq!(got, ResolvedType::Bool);
    }

    #[test]
    fn test_arg_type_correct_passes() {
        let reg = make_registry_with_add();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::IntLit(1, ResolvedType::I32),
                        Expr::IntLit(2, ResolvedType::I32),
                    ],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
    }

    #[test]
    fn test_arg_type_extra_args_no_crash() {
        // Extra args beyond declared params get Void expected — no ArgTypeMismatch
        let reg = make_registry_with_add();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::IntLit(1, ResolvedType::I32),
                        Expr::IntLit(2, ResolvedType::I32),
                        Expr::IntLit(3, ResolvedType::I32), // extra arg
                    ],
                }),
            }),
        };
        // Extra args are resolved with Void expected — no type error raised
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        assert!(result.is_ok(), "expected Ok for extra args, got {:?}", result);
    }

    // -----------------------------------------------------------------------
    // Free function call tests (Step 3)
    // -----------------------------------------------------------------------

    /// Mock rust_param_types that knows about a "free_add(i32, i32) -> i32" free fn.
    fn test_free_fn_param_types(type_name: &str, method: &str, _type_args: &[ResolvedType]) -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType> {
        if !type_name.is_empty() {
            return test_rust_param_types(type_name, method, _type_args);
        }
        Ok(match method {
            "free_add" => Some(vec![ResolvedType::I32, ResolvedType::I32]),
            "free_unit" => Some(vec![]),  // void-returning, zero params
            _ => None,
        })
    }

    fn test_free_fn_method_ret(type_name: &str, method: &str, type_args: &[ResolvedType]) -> Result<ResolvedType, crate::oracle::UnresolvedRustType> {
        if !type_name.is_empty() {
            return test_rust_method_ret(type_name, method, type_args);
        }
        Ok(match method {
            "free_add" => ResolvedType::I32,
            "free_unit" => ResolvedType::Void,
            _ => ResolvedType::Void,
        })
    }

    #[test]
    fn test_free_fn_not_found_gives_undefined_error() {
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "nonexistent_fn".to_string(),
                    type_args: vec![],
                    args: vec![],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_free_fn_method_ret, &test_free_fn_param_types);
        let Err(TypeResolveError::UndefinedFunction { name }) = result
            else { panic!("expected UndefinedFunction, got {:?}", result) };
        assert_eq!(name, "nonexistent_fn");
    }

    #[test]
    fn test_free_fn_void_returning_resolves_correctly() {
        // free_unit() returns void — must not be confused with "not found"
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: None,
            body: Some(Block {
                stmts: vec![Stmt::ExprStmt(Expr::FnCall {
                    name: "free_unit".to_string(),
                    type_args: vec![],
                    args: vec![],
                })],
                ret: None,
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_free_fn_method_ret, &test_free_fn_param_types);
        assert!(result.is_ok(), "void-returning free fn should resolve: {:?}", result);
    }

    #[test]
    fn test_free_fn_correct_args_pass() {
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "free_add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::IntLit(1, ResolvedType::I32),
                        Expr::IntLit(2, ResolvedType::I32),
                    ],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_free_fn_method_ret, &test_free_fn_param_types);
        assert!(result.is_ok(), "correct args should pass: {:?}", result);
        let typed = result.unwrap();
        assert_eq!(typed.ret.as_ref().unwrap().ty, ResolvedType::I32);
    }

    #[test]
    fn test_free_fn_with_args_type_checked() {
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::FnCall {
                    name: "free_add".to_string(),
                    type_args: vec![],
                    args: vec![
                        Expr::IntLit(1, ResolvedType::I32),
                        Expr::BoolLit(true), // wrong: bool where i32 expected
                    ],
                }),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_free_fn_method_ret, &test_free_fn_param_types);
        let Err(TypeResolveError::ArgTypeMismatch { func_name, param_index, expected, got }) = result
            else { panic!("expected ArgTypeMismatch, got {:?}", result) };
        assert_eq!(func_name, "free_add");
        assert_eq!(param_index, 1);
        assert_eq!(expected, ResolvedType::I32);
        assert_eq!(got, ResolvedType::Bool);
    }

    #[test]
    fn test_free_fn_return_type_propagates() {
        // Return value of a free fn used in a let binding
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Let {
                    name: "result".to_string(),
                    expr: Expr::FnCall {
                        name: "free_add".to_string(),
                        type_args: vec![],
                        args: vec![
                            Expr::IntLit(1, ResolvedType::I32),
                            Expr::IntLit(2, ResolvedType::I32),
                        ],
                    },
                }],
                ret: Some(Expr::Var("result".to_string())),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_free_fn_method_ret, &test_free_fn_param_types);
        assert!(result.is_ok(), "expected Ok: {:?}", result);
        // The let-bound variable should have type I32
        if let TypedStmt::Let { expr, .. } = &result.unwrap().stmts[0] {
            assert_eq!(expr.ty, ResolvedType::I32);
        }
    }

    #[test]
    fn test_assign_undefined_error() {
        let reg = make_registry();
        let func = ToyFunction {

            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block {
                stmts: vec![Stmt::Assign {
                    name: "x".to_string(),
                    expr: Expr::IntLit(5, ResolvedType::I32),
                }],
                ret: Some(Expr::IntLit(0, ResolvedType::I32)),
            }),
        };
        let result = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types);
        let Err(TypeResolveError::UndefinedVariable { name }) = result
            else { panic!("expected UndefinedVariable error") };
        assert_eq!(name, "x");
    }

    #[test]
    fn test_resolve_byte_string_lit() {
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::Ref { inner: Box::new(ResolvedType::ByteSlice) }),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::ByteStringLit(vec![104, 101, 108, 108, 111])),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        assert!(matches!(ret.kind, TypedExprKind::ByteStringLit(ref b) if b == &[104, 101, 108, 108, 111]));
        assert_eq!(ret.ty, ResolvedType::Ref { inner: Box::new(ResolvedType::ByteSlice) });
    }

    #[test]
    fn test_resolve_string_lit() {
        // Regression guard: "..." must type-resolve to Ref { Str } (sized fat pointer),
        // not bare Str. Mirrors test_resolve_byte_string_lit for regular strings.
        let reg = make_registry();
        let func = ToyFunction {
            type_params: vec![],
            params: vec![],
            return_ty: Some(ResolvedType::Ref { inner: Box::new(ResolvedType::Str) }),
            body: Some(Block {
                stmts: vec![],
                ret: Some(Expr::StringLit("hello".to_string())),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret, &test_rust_param_types).unwrap();
        let ret = typed.ret.unwrap();
        assert!(matches!(ret.kind, TypedExprKind::StringLit(ref s) if s == "hello"));
        assert_eq!(ret.ty, ResolvedType::Ref { inner: Box::new(ResolvedType::Str) });
    }
}
