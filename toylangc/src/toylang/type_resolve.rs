//! Type resolution pass — annotates every AST expression with its concrete type.
//!
//! Runs after parsing, before LLVM codegen. Walks the untyped AST and produces
//! a TypedFnBody where every expression carries a ResolvedType.

use std::collections::HashMap;

use super::ast::{Expr, FnBody, Stmt};
use super::registry::{ToylangRegistry, ToyFieldType, ToyFunction};
use super::typed_ast::*;

// ============================================================================
// Public entry point
// ============================================================================

/// Resolve all types in a function body, producing a TypedFnBody.
pub fn resolve_fn_body(
    registry: &ToylangRegistry,
    func: &ToyFunction,
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

    // Pre-scan: infer Vec element types for let bindings from forward usage.
    // For `let v = Vec::new(); v.push(x); ToyShip { wings: v }`,
    // we need to know Vec<what> at the point of Vec::new().
    let vec_inferences = infer_vec_types(body, &ret_ty, registry);

    // Resolve statements
    let stmts: Vec<TypedStmt> = body.stmts.iter()
        .map(|stmt| resolve_stmt(stmt, &mut scope, &ret_ty, registry, &vec_inferences))
        .collect();

    // Resolve return expression
    let ret = body.ret.as_ref().map(|expr| {
        resolve_expr(expr, &ret_ty, &scope, registry, &vec_inferences)
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
                let elem = parse_type_string(args[0], registry);
                return ResolvedType::Vec { elem: Box::new(elem) };
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
            field_types,
        };
    }

    panic!("type_resolve: cannot parse type string '{}'", s);
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
            match name.as_str() {
                "Vec" => {
                    let elem = resolve_field_type(&args[0], subst, registry);
                    ResolvedType::Vec { elem: Box::new(elem) }
                }
                other => panic!("unsupported Rust generic type '{}' in type_resolve", other),
            }
        }
    }
}

// ============================================================================
// Vec type inference (forward scan)
// ============================================================================

/// Pre-scan the function body to infer Vec element types for let bindings.
/// Returns a map: variable name → Vec element ResolvedType.
fn infer_vec_types(
    body: &FnBody,
    ret_ty: &ResolvedType,
    registry: &ToylangRegistry,
) -> HashMap<String, ResolvedType> {
    let mut inferences: HashMap<String, ResolvedType> = HashMap::new();

    // Find all let bindings that are Vec::new()
    let vec_vars: Vec<String> = body.stmts.iter()
        .filter_map(|stmt| {
            if let Stmt::Let { name, expr } = stmt {
                if is_vec_new(expr) {
                    return Some(name.clone());
                }
            }
            None
        })
        .collect();

    for var_name in &vec_vars {
        // Strategy 1: look for push calls to infer from argument type
        if let Some(elem_ty) = infer_from_push(body, var_name, registry) {
            inferences.insert(var_name.clone(), elem_ty);
            continue;
        }

        // Strategy 2: look for usage in struct literal to infer from field type
        if let Some(elem_ty) = infer_from_struct_field(body, var_name, ret_ty, registry) {
            inferences.insert(var_name.clone(), elem_ty);
            continue;
        }

        // Strategy 3: infer from function return type
        if let ResolvedType::Vec { elem } = ret_ty {
            inferences.insert(var_name.clone(), *elem.clone());
            continue;
        }
    }

    inferences
}

fn is_vec_new(expr: &Expr) -> bool {
    matches!(expr, Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new")
}

/// Look for `var_name.push(expr)` and infer Vec element type from the push argument.
fn infer_from_push(
    body: &FnBody,
    var_name: &str,
    registry: &ToylangRegistry,
) -> Option<ResolvedType> {
    for stmt in &body.stmts {
        if let Stmt::ExprStmt(Expr::MethodCall { receiver, method, args, .. })
            | Stmt::Let { expr: Expr::MethodCall { receiver, method, args, .. }, .. } = stmt
        {
            if method == "push" {
                if let Expr::Var(recv_name) = receiver.as_ref() {
                    if recv_name == var_name && !args.is_empty() {
                        return infer_expr_type(&args[0], registry);
                    }
                }
            }
        }
    }
    None
}

/// Infer the type of an expression from its shape (without full type resolution).
/// Used for forward-scanning push arguments.
fn infer_expr_type(expr: &Expr, registry: &ToylangRegistry) -> Option<ResolvedType> {
    match expr {
        Expr::IntLit(_) => Some(ResolvedType::I32),
        Expr::BoolLit(_) => Some(ResolvedType::Bool),
        Expr::StructLit { name, .. } => Some(parse_type_string(name, registry)),
        _ => None,
    }
}

/// Look for `StructName { ..., field: var_name, ... }` and infer Vec element type
/// from the struct field's declared type.
fn infer_from_struct_field(
    body: &FnBody,
    var_name: &str,
    ret_ty: &ResolvedType,
    registry: &ToylangRegistry,
) -> Option<ResolvedType> {
    // Check the return expression
    if let Some(ref ret_expr) = body.ret {
        if let Some(ty) = infer_from_struct_lit(ret_expr, var_name, ret_ty, registry) {
            return Some(ty);
        }
    }
    None
}

fn infer_from_struct_lit(
    expr: &Expr,
    var_name: &str,
    expected_ty: &ResolvedType,
    registry: &ToylangRegistry,
) -> Option<ResolvedType> {
    if let Expr::StructLit { name, fields } = expr {
        if let ResolvedType::Struct { field_types, .. } = expected_ty {
            if let Some(_toy_struct) = registry.structs.get(name.as_str()) {
                for (i, (_field_name, field_expr)) in fields.iter().enumerate() {
                    if let Expr::Var(v) = field_expr {
                        if v == var_name {
                            // This variable is assigned to this struct field
                            if i < field_types.len() {
                                if let ResolvedType::Vec { elem } = &field_types[i] {
                                    return Some(*elem.clone());
                                }
                            }
                        }
                    }
                    // Recurse into nested struct literals
                    if let Some(ty) = infer_from_struct_lit(
                        field_expr, var_name,
                        field_types.get(i).unwrap_or(&ResolvedType::Void),
                        registry,
                    ) {
                        return Some(ty);
                    }
                }
            }
        }
    }
    None
}

// ============================================================================
// Expression resolution
// ============================================================================

fn resolve_expr(
    expr: &Expr,
    expected_ty: &ResolvedType,
    scope: &HashMap<String, ResolvedType>,
    registry: &ToylangRegistry,
    vec_inferences: &HashMap<String, ResolvedType>,
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
                    let typed = resolve_expr(field_expr, expected, scope, registry, vec_inferences);
                    (field_name.clone(), typed)
                })
                .collect();

            TypedExpr {
                kind: TypedExprKind::StructLit { name: name.clone(), fields: typed_fields },
                ty: resolved_ty,
            }
        }

        Expr::FnCall { name, args } => {
            let func = registry.functions.get(name.as_str())
                .unwrap_or_else(|| panic!("function '{}' not found in registry", name));

            if !func.type_params.is_empty() {
                // Generic function call — infer type args from expected return type.
                // Build a substitution map: type param name → concrete type string.
                let type_arg_subst = infer_type_args_from_expected(
                    func, expected_ty, registry,
                );
                let ret_ty = expected_ty.clone();
                let typed_args: Vec<TypedExpr> = args.iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let param_ty_str = &func.params[i].ty;
                        // Substitute type params in param type
                        let resolved_param_str = substitute_type_params(param_ty_str, &type_arg_subst);
                        let expected = parse_type_string(&resolved_param_str, registry);
                        resolve_expr(a, &expected, scope, registry, vec_inferences)
                    })
                    .collect();
                // Extract type args in order for symbol mangling
                let type_args: Vec<String> = func.type_params.iter()
                    .filter_map(|p| type_arg_subst.get(p).cloned())
                    .collect();
                TypedExpr {
                    kind: TypedExprKind::FnCall { name: name.clone(), type_args, args: typed_args },
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
                        resolve_expr(a, &expected, scope, registry, vec_inferences)
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
            let typed_left = resolve_expr(left, &operand_ty, scope, registry, vec_inferences);
            let typed_right = resolve_expr(right, &operand_ty, scope, registry, vec_inferences);
            TypedExpr {
                kind: TypedExprKind::BinaryOp {
                    op: *op,
                    left: Box::new(typed_left),
                    right: Box::new(typed_right),
                },
                ty: operand_ty,
            }
        }

        Expr::StaticCall { ty, method, args: _ } => {
            match (ty.as_str(), method.as_str()) {
                ("Vec", "new") => {
                    // Vec element type comes from expected_ty or inference
                    let elem_ty = match expected_ty {
                        ResolvedType::Vec { elem } => *elem.clone(),
                        _ => ResolvedType::Void, // will be refined
                    };
                    let vec_ty = ResolvedType::Vec { elem: Box::new(elem_ty) };
                    TypedExpr {
                        kind: TypedExprKind::StaticCall {
                            ty: ty.clone(),
                            method: method.clone(),
                            args: vec![],
                        },
                        ty: vec_ty,
                    }
                }
                _ => panic!("unsupported static call: {}::{}", ty, method),
            }
        }

        Expr::MethodCall { receiver, method, args } => {
            let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, vec_inferences);

            match method.as_str() {
                "push" => {
                    let elem_ty = match &typed_recv.ty {
                        ResolvedType::Vec { elem } => *elem.clone(),
                        _ => panic!("push on non-Vec type: {:?}", typed_recv.ty),
                    };
                    let typed_args: Vec<TypedExpr> = args.iter()
                        .map(|a| resolve_expr(a, &elem_ty, scope, registry, vec_inferences))
                        .collect();
                    TypedExpr {
                        kind: TypedExprKind::MethodCall {
                            receiver: Box::new(typed_recv),
                            method: method.clone(),
                            args: typed_args,
                        },
                        ty: ResolvedType::Void,
                    }
                }
                "len" => {
                    TypedExpr {
                        kind: TypedExprKind::MethodCall {
                            receiver: Box::new(typed_recv),
                            method: method.clone(),
                            args: vec![],
                        },
                        ty: ResolvedType::Usize,
                    }
                }
                _ => panic!("unsupported method: .{}", method),
            }
        }
    }
}

fn resolve_stmt(
    stmt: &Stmt,
    scope: &mut HashMap<String, ResolvedType>,
    _ret_ty: &ResolvedType,
    registry: &ToylangRegistry,
    vec_inferences: &HashMap<String, ResolvedType>,
) -> TypedStmt {
    match stmt {
        Stmt::Let { name, expr } => {
            // For Vec::new(), use the inferred element type
            let expected = if is_vec_new(expr) {
                vec_inferences.get(name.as_str())
                    .map(|elem| ResolvedType::Vec { elem: Box::new(elem.clone()) })
                    .unwrap_or(ResolvedType::Void)
            } else {
                ResolvedType::Void
            };

            let typed_expr = resolve_expr(expr, &expected, scope, registry, vec_inferences);
            scope.insert(name.clone(), typed_expr.ty.clone());
            TypedStmt::Let { name: name.clone(), expr: typed_expr }
        }
        Stmt::ExprStmt(expr) => {
            let typed_expr = resolve_expr(expr, &ResolvedType::Void, scope, registry, vec_inferences);
            TypedStmt::ExprStmt(typed_expr)
        }
    }
}

// ============================================================================
// Generic function type arg inference
// ============================================================================

/// Infer type arg substitutions for a generic function call from the expected return type.
fn infer_type_args_from_expected(
    func: &ToyFunction,
    expected_ty: &ResolvedType,
    registry: &ToylangRegistry,
) -> HashMap<String, String> {
    let mut subst = HashMap::new();
    let ret_str = match &func.return_ty {
        Some(s) => s.clone(),
        None => return subst,
    };

    // Direct type param: "T" → match against expected type
    if func.type_params.contains(&ret_str) {
        subst.insert(ret_str.clone(), resolved_type_to_string(expected_ty));
        return subst;
    }

    // Generic struct pattern: "Wrapper<T>" → match struct type params
    if let Some(open) = ret_str.find('<') {
        if ret_str.ends_with('>') {
            let base = &ret_str[..open];
            let args_str = &ret_str[open + 1..ret_str.len() - 1];
            let _ret_args: Vec<&str> = split_type_args(args_str);

            // Get the struct's type params to know the mapping
            if let Some(toy_struct) = registry.structs.get(base) {
                // The expected_ty has resolved field types — we need the concrete type args.
                // For Wrapper<T> → Wrapper<i32>: field_types = [I32], type_params = ["T"]
                if let ResolvedType::Struct { field_types, .. } = expected_ty {
                    for (i, param_name) in toy_struct.type_params.iter().enumerate() {
                        if i < field_types.len() {
                            subst.insert(param_name.clone(), resolved_type_to_string(&field_types[i]));
                        }
                    }
                }
            }
        }
    }
    subst
}

fn resolved_type_to_string(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::I32 => "i32".to_string(),
        ResolvedType::I64 => "i64".to_string(),
        ResolvedType::F64 => "f64".to_string(),
        ResolvedType::Bool => "bool".to_string(),
        ResolvedType::Usize => "usize".to_string(),
        ResolvedType::Void => "()".to_string(),
        ResolvedType::Struct { name, .. } => name.clone(),
        ResolvedType::Vec { elem } => format!("Vec<{}>", resolved_type_to_string(elem)),
        ResolvedType::Ref { inner } => format!("&{}", resolved_type_to_string(inner)),
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
        ToylangRegistry { structs, functions: std::collections::HashMap::new() }
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
        assert!(matches!(ty, ResolvedType::Struct { ref name, ref field_types }
            if name == "Counter" && field_types == &[ResolvedType::I32]));
    }

    #[test]
    fn test_parse_generic_struct() {
        let reg = make_registry();
        let ty = parse_type_string("Pair<i32, i64>", &reg);
        match ty {
            ResolvedType::Struct { name, field_types } => {
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
        assert!(matches!(ty, ResolvedType::Vec { ref elem } if **elem == ResolvedType::I32));
    }

    #[test]
    fn test_parse_ref() {
        let reg = make_registry();
        let ty = parse_type_string("&Vec<Point>", &reg);
        match ty {
            ResolvedType::Ref { inner } => {
                match *inner {
                    ResolvedType::Vec { elem } => {
                        assert!(matches!(*elem, ResolvedType::Struct { ref name, .. } if name == "Point"));
                    }
                    _ => panic!("expected Vec"),
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
            params: vec![],
            return_ty: Some("i32".to_string()),
            body: Some(FnBody { stmts: vec![], ret: Some(Expr::IntLit(42)) }),
        };
        let typed = resolve_fn_body(&reg, &func);
        let ret = typed.ret.unwrap();
        assert!(matches!(ret.kind, TypedExprKind::IntLit(42)));
        assert_eq!(ret.ty, ResolvedType::I32);
    }

    #[test]
    fn test_resolve_generic_struct_lit() {
        let reg = make_registry();
        let func = ToyFunction {
            name: "f".to_string(),
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
        let typed = resolve_fn_body(&reg, &func);
        let ret = typed.ret.unwrap();
        // The struct should be resolved as Pair with [I32, I64]
        match &ret.ty {
            ResolvedType::Struct { name, field_types } => {
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
        let typed = resolve_fn_body(&reg, &func);
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
        let typed = resolve_fn_body(&reg, &func);
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
            params: vec![],
            return_ty: Some("ToyShip".to_string()),
            body: Some(FnBody {
                stmts: vec![
                    Stmt::Let {
                        name: "v".to_string(),
                        expr: Expr::StaticCall { ty: "Vec".to_string(), method: "new".to_string(), args: vec![] },
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
        let typed = resolve_fn_body(&reg, &func);

        // Vec::new should be typed as Vec<I32>
        if let TypedStmt::Let { expr, .. } = &typed.stmts[0] {
            assert!(matches!(&expr.ty, ResolvedType::Vec { elem } if **elem == ResolvedType::I32));
        }

        // push arg should be I32
        if let TypedStmt::ExprStmt(expr) = &typed.stmts[1] {
            if let TypedExprKind::MethodCall { args, .. } = &expr.kind {
                assert_eq!(args[0].ty, ResolvedType::I32);
            }
        }
    }
}
