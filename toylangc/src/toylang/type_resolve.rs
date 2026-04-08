//! Type resolution pass — annotates every AST expression with its concrete type.
//!
//! Runs after parsing, before LLVM codegen. Walks the untyped AST and produces
//! a TypedFnBody where every expression carries a ResolvedType.

use std::collections::HashMap;

use super::ast::{Expr, Stmt};
#[cfg(test)]
use super::ast::FnBody;
use super::registry::{ToylangRegistry, ToyFieldType, ToyFunction};
use super::typed_ast::*;

// ============================================================================
// Public entry point
// ============================================================================

/// Resolve all types in a function body, producing a TypedFnBody.
/// Resolve just the return type of a function without resolving the full body.
pub fn resolve_return_type(
    registry: &ToylangRegistry,
    func: &ToyFunction,
) -> ResolvedType {
    func.return_ty.as_deref()
        .map(|s| parse_type_string(s, registry))
        .unwrap_or(ResolvedType::Void)
}

pub fn resolve_fn_body(
    registry: &ToylangRegistry,
    func: &ToyFunction,
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> ResolvedType,
) -> TypedFnBody {
    let body = func.body.as_ref().expect("function has no body");
    let ret_ty = func.return_ty.as_deref()
        .map(|s| parse_type_string(s, registry))
        .unwrap_or(ResolvedType::Void);

    let mut scope: HashMap<String, ResolvedType> = HashMap::new();

    // Add function parameters to scope
    for p in &func.params {
        let ty = parse_type_string(&p.ty, registry);
        scope.insert(p.name.clone(), ty);
    }

    // Resolve statements
    let stmts: Vec<TypedStmt> = body.stmts.iter()
        .map(|stmt| resolve_stmt(stmt, &mut scope, &ret_ty, registry, rust_method_ret))
        .collect();

    // Resolve return expression
    let ret = body.ret.as_ref().map(|expr| {
        resolve_expr(expr, &ret_ty, &scope, registry, rust_method_ret)
    });

    TypedFnBody { stmts, ret }
}

// ============================================================================
// Type string parsing
// ============================================================================

/// Parse a type string like "i32", "Pair<i32, i64>", "Vec<Point>", "&Vec<Point>"
/// into a ResolvedType. Uses the registry to resolve struct names.
pub fn parse_type_string(s: &str, registry: &ToylangRegistry) -> ResolvedType {
    let s = s.trim();

    // Reference types
    if s.starts_with("&mut ") {
        return ResolvedType::Ref {
            inner: Box::new(parse_type_string(&s[5..], registry)),
        };
    }
    if s.starts_with("&") {
        return ResolvedType::Ref {
            inner: Box::new(parse_type_string(&s[1..], registry)),
        };
    }

    // Pointer types (treat as ref for now)
    if s.starts_with("*const ") || s.starts_with("*mut ") {
        let inner_start = if s.starts_with("*const ") { 7 } else { 5 };
        return ResolvedType::Ref {
            inner: Box::new(parse_type_string(&s[inner_start..], registry)),
        };
    }

    // Primitives
    match s {
        "i32" => return ResolvedType::I32,
        "i64" => return ResolvedType::I64,
        "f64" => return ResolvedType::F64,
        "bool" => return ResolvedType::Bool,
        "usize" => return ResolvedType::Usize,
        "()" => return ResolvedType::Void,
        _ => {}
    }

    // Generic types: "Vec<i32>", "Pair<i32, i64>"
    if let Some(open) = s.find('<') {
        if s.ends_with('>') {
            let base = &s[..open];
            let args_str = &s[open + 1..s.len() - 1];
            let args = split_type_args(args_str);

            if base == "Vec" {
                let resolved_args: Vec<ResolvedType> = args.iter()
                    .map(|a| parse_type_string(a, registry))
                    .collect();
                return ResolvedType::RustType { name: "Vec".to_string(), type_args: resolved_args };
            }

            // Generic struct: look up in registry and substitute type params
            if let Some(toy_struct) = registry.structs.get(base) {
                let resolved_args: Vec<ResolvedType> = args.iter()
                    .map(|a| parse_type_string(a, registry))
                    .collect();
                let subst: HashMap<&str, &ResolvedType> = toy_struct.type_params.iter()
                    .zip(resolved_args.iter())
                    .map(|(param, resolved)| (param.as_str(), resolved))
                    .collect();
                let field_types: Vec<ResolvedType> = toy_struct.fields.iter()
                    .map(|f| resolve_field_type(&f.rust_type, &subst, registry))
                    .collect();
                return ResolvedType::Struct {
                    name: base.to_string(),
                    type_args: resolved_args,
                    field_types,
                };
            }
        }
    }

    // Non-generic struct
    if let Some(toy_struct) = registry.structs.get(s) {
        let field_types: Vec<ResolvedType> = toy_struct.fields.iter()
            .map(|f| resolve_field_type(&f.rust_type, &HashMap::new(), registry))
            .collect();
        return ResolvedType::Struct {
            name: s.to_string(),
            type_args: vec![],
            field_types,
        };
    }

    // Unknown non-generic name — treat as an opaque Rust type (e.g. "Global")
    ResolvedType::RustType { name: s.to_string(), type_args: vec![] }
}

/// Split "i32, i64" into ["i32", "i64"], handling nested generics.
fn split_type_args(s: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                args.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        args.push(last);
    }
    args
}

/// Resolve a ToyFieldType to a ResolvedType, substituting type params.
fn resolve_field_type(
    ft: &ToyFieldType,
    subst: &HashMap<&str, &ResolvedType>,
    registry: &ToylangRegistry,
) -> ResolvedType {
    match ft {
        ToyFieldType::I32 => ResolvedType::I32,
        ToyFieldType::I64 => ResolvedType::I64,
        ToyFieldType::F64 => ResolvedType::F64,
        ToyFieldType::Bool => ResolvedType::Bool,
        ToyFieldType::TypeParam(name) => {
            (*subst.get(name.as_str())
                .unwrap_or_else(|| panic!("unresolved type param '{}'", name)))
                .clone()
        }
        ToyFieldType::ToyStruct(name) => {
            parse_type_string(name, registry)
        }
        ToyFieldType::RustGeneric(name, args) => {
            let resolved_args: Vec<ResolvedType> = args.iter()
                .map(|a| resolve_field_type(a, subst, registry))
                .collect();
            ResolvedType::RustType { name: name.clone(), type_args: resolved_args }
        }
    }
}

// ============================================================================
// Expression resolution
// ============================================================================

fn resolve_expr(
    expr: &Expr,
    expected_ty: &ResolvedType,
    scope: &HashMap<String, ResolvedType>,
    registry: &ToylangRegistry,
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> ResolvedType,
) -> TypedExpr {
    match expr {
        Expr::IntLit(n) => {
            // TODO: Replace this default-to-i32 heuristic with proper backward
            // type inference (Option 1 from the plan). A pre-scan of the return
            // expression should propagate the return type backward through
            // variables to their Let bindings, so `let a = 10` in a function
            // returning i64 would correctly infer a as i64. For now, default
            // to i32 (like C's integer literals) and rely on explicit context
            // from struct fields and return types to override when needed.
            let ty = match expected_ty {
                ResolvedType::I32 => ResolvedType::I32,
                ResolvedType::I64 => ResolvedType::I64,
                ResolvedType::Bool => ResolvedType::Bool,
                ResolvedType::Usize => ResolvedType::Usize,
                _ => ResolvedType::I32, // default to i32 (was i64)
            };
            TypedExpr { kind: TypedExprKind::IntLit(*n), ty }
        }

        Expr::BoolLit(b) => {
            TypedExpr { kind: TypedExprKind::BoolLit(*b), ty: ResolvedType::Bool }
        }

        Expr::StringLit(s) => TypedExpr {
            kind: TypedExprKind::StringLit(s.clone()),
            ty: ResolvedType::Str,
        },

        Expr::Var(name) => {
            let ty = scope.get(name.as_str())
                .cloned()
                .unwrap_or_else(|| panic!("variable '{}' not in scope during type resolution", name));
            TypedExpr { kind: TypedExprKind::Var(name.clone()), ty }
        }

        Expr::StructLit { name, fields } => {
            // Look up the struct and resolve field types
            let resolved_ty = if expected_ty != &ResolvedType::Void {
                // Use expected type (already resolved with type params)
                expected_ty.clone()
            } else {
                parse_type_string(name, registry)
            };

            let field_types = match &resolved_ty {
                ResolvedType::Struct { field_types, .. } => field_types.clone(),
                _ => panic!("expected struct type for StructLit, got {:?}", resolved_ty),
            };

            let toy_struct = registry.structs.get(name.as_str())
                .unwrap_or_else(|| panic!("struct '{}' not found", name));

            let typed_fields: Vec<(String, TypedExpr)> = fields.iter()
                .map(|(field_name, field_expr)| {
                    let field_idx = toy_struct.fields.iter()
                        .position(|f| f.name == *field_name)
                        .unwrap_or_else(|| panic!("field '{}' not found in '{}'", field_name, name));
                    let expected = &field_types[field_idx];
                    let typed = resolve_expr(field_expr, expected, scope, registry, rust_method_ret);
                    (field_name.clone(), typed)
                })
                .collect();

            TypedExpr {
                kind: TypedExprKind::StructLit { name: name.clone(), fields: typed_fields },
                ty: resolved_ty,
            }
        }

        Expr::FnCall { name, type_args: _, args } if name == "println" => {
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, rust_method_ret))
                .collect();
            TypedExpr {
                kind: TypedExprKind::FnCall {
                    name: "println".into(),
                    type_args: vec![],
                    args: typed_args,
                },
                ty: ResolvedType::Void,
            }
        }

        Expr::FnCall { name, type_args, args } => {
            let func = registry.functions.get(name.as_str())
                .unwrap_or_else(|| panic!("function '{}' not found in registry", name));

            if !func.type_params.is_empty() {
                // Generic function call — type args must be provided explicitly
                assert_eq!(type_args.len(), func.type_params.len(),
                    "function '{}' requires {} type args, got {}",
                    name, func.type_params.len(), type_args.len());

                // Build substitution map directly from explicit type args
                let type_arg_subst: HashMap<String, String> = func.type_params.iter()
                    .zip(type_args.iter())
                    .map(|(param, arg)| (param.clone(), arg.clone()))
                    .collect();

                // Compute return type from substitution
                let ret_ty = if let Some(ret_str) = &func.return_ty {
                    let resolved_ret_str = substitute_type_params(ret_str, &type_arg_subst);
                    parse_type_string(&resolved_ret_str, registry)
                } else {
                    ResolvedType::Void
                };

                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let param_ty_str = &func.params[i].ty;
                        let resolved_param_str = substitute_type_params(param_ty_str, &type_arg_subst);
                        let expected = parse_type_string(&resolved_param_str, registry);
                        resolve_expr(a, &expected, scope, registry, rust_method_ret)
                    })
                    .collect();

                TypedExpr {
                    kind: TypedExprKind::FnCall {
                        name: name.clone(),
                        type_args: type_args.clone(),
                        args: typed_args,
                    },
                    ty: ret_ty,
                }
            } else {
                // Concrete function call
                let ret_ty = func.return_ty.as_deref()
                    .map(|s| parse_type_string(s, registry))
                    .unwrap_or(ResolvedType::Void);
                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let expected = if i < func.params.len() {
                            parse_type_string(&func.params[i].ty, registry)
                        } else {
                            ResolvedType::Void
                        };
                        resolve_expr(a, &expected, scope, registry, rust_method_ret)
                    })
                    .collect();
                TypedExpr {
                    kind: TypedExprKind::FnCall { name: name.clone(), type_args: vec![], args: typed_args },
                    ty: ret_ty,
                }
            }
        }

        Expr::BinaryOp { op, left, right } => {
            // Both operands and result have the same type.
            // Use expected type, or infer from operands.
            let operand_ty = if expected_ty != &ResolvedType::Void {
                expected_ty.clone()
            } else {
                ResolvedType::I32 // default for arithmetic
            };
            let typed_left = resolve_expr(left, &operand_ty, scope, registry, rust_method_ret);
            let typed_right = resolve_expr(right, &operand_ty, scope, registry, rust_method_ret);
            TypedExpr {
                kind: TypedExprKind::BinaryOp {
                    op: *op,
                    left: Box::new(typed_left),
                    right: Box::new(typed_right),
                },
                ty: operand_ty,
            }
        }

        Expr::StaticCall { ty, method, type_args, args: _ } => {
            match (ty.as_str(), method.as_str()) {
                ("Vec", "new") => {
                    // Vec element type must be provided explicitly: Vec::new<Point>()
                    assert!(!type_args.is_empty(),
                        "Vec::new requires explicit element type: Vec::new<Point>()");
                    let resolved_type_args: Vec<ResolvedType> = type_args.iter()
                        .map(|a| parse_type_string(a, registry))
                        .collect();
                    let vec_ty = ResolvedType::RustType { name: "Vec".to_string(), type_args: resolved_type_args.clone() };
                    TypedExpr {
                        kind: TypedExprKind::StaticCall {
                            ty: ty.clone(),
                            method: method.clone(),
                            type_args: resolved_type_args,
                            args: vec![],
                        },
                        ty: vec_ty,
                    }
                }
                _ => panic!("unsupported static call: {}::{}", ty, method),
            }
        }

        Expr::FieldAccess { receiver, field } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret);
            let ResolvedType::Struct { name: struct_name, field_types, .. } = &typed_recv.ty else {
                panic!("field access on non-struct type: {:?}", typed_recv.ty);
            };
            let toy_struct = registry.structs.get(struct_name.as_str())
                .expect("struct not found in registry");
            let field_idx = toy_struct.fields.iter()
                .position(|f| f.name == *field)
                .unwrap_or_else(|| panic!("field '{}' not found in '{}'", field, struct_name));
            let field_ty = field_types[field_idx].clone();
            TypedExpr {
                kind: TypedExprKind::FieldAccess {
                    receiver: Box::new(typed_recv),
                    field: field.clone(),
                },
                ty: field_ty,
            }
        }

        Expr::MethodCall { receiver, method, args } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, rust_method_ret);

            // Extract the Rust type name and type_args, handling both direct and &ref receivers
            let (rust_name, rust_type_args) = match &typed_recv.ty {
                ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                ResolvedType::Ref { inner } => match inner.as_ref() {
                    ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                    _ => panic!("unsupported method call .{} on type {:?}", method, typed_recv.ty),
                },
                _ => panic!("unsupported method call .{} on type {:?}", method, typed_recv.ty),
            };

            let ret_ty = rust_method_ret(rust_name, method, rust_type_args);
            // For method args, use type_args[0] as expected type (covers push, insert, etc.)
            let typed_args: Vec<TypedExpr> = args.iter()
                .map(|a| {
                    let expected = if !rust_type_args.is_empty() {
                        rust_type_args[0].clone()
                    } else {
                        ResolvedType::Void
                    };
                    resolve_expr(a, &expected, scope, registry, rust_method_ret)
                })
                .collect();
            TypedExpr {
                kind: TypedExprKind::MethodCall {
                    receiver: Box::new(typed_recv),
                    method: method.clone(),
                    args: typed_args,
                },
                ty: ret_ty,
            }
        }
    }
}

fn resolve_stmt(
    stmt: &Stmt,
    scope: &mut HashMap<String, ResolvedType>,
    _ret_ty: &ResolvedType,
    registry: &ToylangRegistry,
    rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> ResolvedType,
) -> TypedStmt {
    match stmt {
        Stmt::Let { name, expr } => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret);
            scope.insert(name.clone(), typed_expr.ty.clone());
            TypedStmt::Let { name: name.clone(), expr: typed_expr }
        }
        Stmt::ExprStmt(expr) => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, rust_method_ret);
            TypedStmt::ExprStmt(typed_expr)
        }
    }
}

fn substitute_type_params(ty_str: &str, subst: &HashMap<String, String>) -> String {
    if let Some(replacement) = subst.get(ty_str) {
        return replacement.clone();
    }
    if let Some(open) = ty_str.find('<') {
        if ty_str.ends_with('>') {
            let base = &ty_str[..open];
            let args_str = &ty_str[open + 1..ty_str.len() - 1];
            let args: Vec<&str> = split_type_args(args_str);
            let resolved: Vec<String> = args.iter()
                .map(|a| substitute_type_params(a, subst))
                .collect();
            return format!("{}<{}>", base, resolved.join(", "));
        }
    }
    ty_str.to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toylang::registry::*;

    /// Test callback for Rust method return types.
    fn test_rust_method_ret(_type_name: &str, method: &str, _type_args: &[ResolvedType]) -> ResolvedType {
        match method {
            "new" => ResolvedType::RustType {
                name: _type_name.to_string(),
                type_args: _type_args.to_vec(),
            },
            "push" => ResolvedType::Void,
            "len" => ResolvedType::Usize,
            _ => panic!("unknown Rust method '{}' in test", method),
        }
    }

    fn make_registry() -> ToylangRegistry {
        let mut structs = std::collections::HashMap::new();
        structs.insert("Counter".to_string(), ToyStruct {
            name: "Counter".to_string(),
            type_params: vec![],
            fields: vec![ToyField { name: "value".to_string(), rust_type: ToyFieldType::I32 }],
        });
        structs.insert("Point".to_string(), ToyStruct {
            name: "Point".to_string(),
            type_params: vec![],
            fields: vec![
                ToyField { name: "x".to_string(), rust_type: ToyFieldType::I32 },
                ToyField { name: "y".to_string(), rust_type: ToyFieldType::I32 },
            ],
        });
        structs.insert("Pair".to_string(), ToyStruct {
            name: "Pair".to_string(),
            type_params: vec!["A".to_string(), "B".to_string()],
            fields: vec![
                ToyField { name: "first".to_string(), rust_type: ToyFieldType::TypeParam("A".to_string()) },
                ToyField { name: "second".to_string(), rust_type: ToyFieldType::TypeParam("B".to_string()) },
            ],
        });
        structs.insert("ToyInner".to_string(), ToyStruct {
            name: "ToyInner".to_string(),
            type_params: vec![],
            fields: vec![ToyField { name: "x".to_string(), rust_type: ToyFieldType::I32 }],
        });
        structs.insert("ToyOuter".to_string(), ToyStruct {
            name: "ToyOuter".to_string(),
            type_params: vec![],
            fields: vec![ToyField { name: "inner".to_string(), rust_type: ToyFieldType::ToyStruct("ToyInner".to_string()) }],
        });
        structs.insert("ToyShip".to_string(), ToyStruct {
            name: "ToyShip".to_string(),
            type_params: vec![],
            fields: vec![ToyField {
                name: "wings".to_string(),
                rust_type: ToyFieldType::RustGeneric("Vec".to_string(), vec![ToyFieldType::I32]),
            }],
        });
        ToylangRegistry { structs, functions: std::collections::HashMap::new(), imports: vec![] }
    }

    #[test]
    fn test_parse_primitive() {
        let reg = make_registry();
        assert_eq!(parse_type_string("i32", &reg), ResolvedType::I32);
        assert_eq!(parse_type_string("i64", &reg), ResolvedType::I64);
        assert_eq!(parse_type_string("bool", &reg), ResolvedType::Bool);
        assert_eq!(parse_type_string("usize", &reg), ResolvedType::Usize);
    }

    #[test]
    fn test_parse_struct() {
        let reg = make_registry();
        let ty = parse_type_string("Counter", &reg);
        assert!(matches!(ty, ResolvedType::Struct { ref name, ref field_types, .. }
            if name == "Counter" && field_types == &[ResolvedType::I32]));
    }

    #[test]
    fn test_parse_generic_struct() {
        let reg = make_registry();
        let ty = parse_type_string("Pair<i32, i64>", &reg);
        match ty {
            ResolvedType::Struct { name, field_types, .. } => {
                assert_eq!(name, "Pair");
                assert_eq!(field_types, vec![ResolvedType::I32, ResolvedType::I64]);
            }
            _ => panic!("expected Struct, got {:?}", ty),
        }
    }

    #[test]
    fn test_parse_vec() {
        let reg = make_registry();
        let ty = parse_type_string("Vec<i32>", &reg);
        assert!(matches!(ty, ResolvedType::RustType { ref name, ref type_args }
            if name == "Vec" && type_args == &[ResolvedType::I32]));
    }

    #[test]
    fn test_parse_ref() {
        let reg = make_registry();
        let ty = parse_type_string("&Vec<Point>", &reg);
        match ty {
            ResolvedType::Ref { inner } => {
                match *inner {
                    ResolvedType::RustType { ref name, ref type_args } if name == "Vec" => {
                        assert!(matches!(&type_args[0], ResolvedType::Struct { ref name, .. } if name == "Point"));
                    }
                    _ => panic!("expected RustType Vec"),
                }
            }
            _ => panic!("expected Ref"),
        }
    }

    #[test]
    fn test_resolve_int_lit() {
        let reg = make_registry();
        let func = ToyFunction {
            name: "f".to_string(),
            type_params: vec![],
            params: vec![],
            return_ty: Some("i32".to_string()),
            body: Some(FnBody { stmts: vec![], ret: Some(Expr::IntLit(42)) }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret);
        let ret = typed.ret.unwrap();
        assert!(matches!(ret.kind, TypedExprKind::IntLit(42)));
        assert_eq!(ret.ty, ResolvedType::I32);
    }

    #[test]
    fn test_resolve_generic_struct_lit() {
        let reg = make_registry();
        let func = ToyFunction {
            name: "f".to_string(),
            type_params: vec![],
            params: vec![],
            return_ty: Some("Pair<i32, i64>".to_string()),
            body: Some(FnBody {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "Pair".to_string(),
                    fields: vec![
                        ("first".to_string(), Expr::IntLit(10)),
                        ("second".to_string(), Expr::IntLit(20)),
                    ],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret);
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
            name: "f".to_string(),
            type_params: vec![],
            params: vec![ToyParam { name: "x".to_string(), ty: "i32".to_string() }],
            return_ty: Some("Counter".to_string()),
            body: Some(FnBody {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "Counter".to_string(),
                    fields: vec![("value".to_string(), Expr::Var("x".to_string()))],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret);
        let ret = typed.ret.unwrap();
        if let TypedExprKind::StructLit { fields, .. } = &ret.kind {
            assert_eq!(fields[0].1.ty, ResolvedType::I32);
        }
    }

    #[test]
    fn test_resolve_nested_struct() {
        let reg = make_registry();
        let func = ToyFunction {
            name: "f".to_string(),
            type_params: vec![],
            params: vec![],
            return_ty: Some("ToyOuter".to_string()),
            body: Some(FnBody {
                stmts: vec![],
                ret: Some(Expr::StructLit {
                    name: "ToyOuter".to_string(),
                    fields: vec![
                        ("inner".to_string(), Expr::StructLit {
                            name: "ToyInner".to_string(),
                            fields: vec![("x".to_string(), Expr::IntLit(42))],
                        }),
                    ],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret);
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
            name: "f".to_string(),
            type_params: vec![],
            params: vec![],
            return_ty: Some("ToyShip".to_string()),
            body: Some(FnBody {
                stmts: vec![
                    Stmt::Let {
                        name: "v".to_string(),
                        expr: Expr::StaticCall { ty: "Vec".to_string(), method: "new".to_string(), type_args: vec!["i32".to_string()], args: vec![] },
                    },
                    Stmt::ExprStmt(Expr::MethodCall {
                        receiver: Box::new(Expr::Var("v".to_string())),
                        method: "push".to_string(),
                        args: vec![Expr::IntLit(1)],
                    }),
                ],
                ret: Some(Expr::StructLit {
                    name: "ToyShip".to_string(),
                    fields: vec![("wings".to_string(), Expr::Var("v".to_string()))],
                }),
            }),
        };
        let typed = resolve_fn_body(&reg, &func, &test_rust_method_ret);

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
}
