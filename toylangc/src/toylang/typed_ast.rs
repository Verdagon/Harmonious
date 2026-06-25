//! Typed AST — every expression carries its resolved concrete type.
//!
//! Produced by the type resolution pass (`type_resolve.rs`) from the untyped
//! AST (`ast.rs`). Consumed by the LLVM backend (`llvm_gen.rs`).

use serde::{Deserialize, Serialize};

/// A type representation used throughout the compiler.
/// Most variants are fully resolved. `TypeParam` appears only in the registry
/// for uninstantiated generic function params/return types and struct fields.
/// After type resolution, the typed AST never contains `TypeParam`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResolvedType {
    I32,
    I64,
    F64,
    Bool,
    Void,
    Usize,
    /// Unresolved type parameter (e.g. "T"). Only in registry, never in typed AST.
    TypeParam(String),
    /// Reference to a struct by name — field layout not yet resolved.
    /// Produced by the parser and stored in the registry. The type resolver
    /// converts these to `Struct` by looking up fields.
    StructRef {
        name: String,
        type_args: Vec<ResolvedType>,
    },
    /// Fully resolved struct with known field types. Only the type resolver
    /// and codegen should use this variant.
    Struct {
        name: String,
        /// Concrete type args (e.g. [I64, I64] for Pair<i64, i64>). Empty for non-generic structs.
        type_args: Vec<ResolvedType>,
        /// Resolved field types (TypeParams substituted with concrete types).
        field_types: Vec<ResolvedType>,
    },
    RustType {
        name: String,
        type_args: Vec<ResolvedType>,
    },
    Ref {
        inner: Box<ResolvedType>,
    },
    /// The unsized string type `str`. Always appears inside `Ref` as `&str`.
    /// Per @UTAIRZ, Str mirrors ByteSlice's six-touchpoint wiring exactly.
    Str,
    /// The unsized byte slice type `[u8]`. Always appears inside `Ref` as `&[u8]`.
    /// Per @UTAIRZ, ByteSlice is the template that Str follows.
    ByteSlice,
}

/// A typed expression — every node knows its concrete type.
#[derive(Clone, Debug)]
pub struct TypedExpr {
    pub kind: TypedExprKind,
    pub ty: ResolvedType,
}

#[derive(Clone, Debug)]
pub enum TypedExprKind {
    IntLit(i64),
    BoolLit(bool),
    Var(String),
    StructLit {
        name: String,
        fields: Vec<(String, TypedExpr)>,
    },
    FnCall {
        name: String,
        type_args: Vec<ResolvedType>,
        args: Vec<TypedExpr>,
    },
    BinaryOp {
        op: crate::toylang::ast::BinOp,
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    StaticCall {
        ty: String,
        method: String,
        type_args: Vec<ResolvedType>,
        #[allow(dead_code)]
        args: Vec<TypedExpr>,
    },
    MethodCall {
        receiver: Box<TypedExpr>,
        method: String,
        args: Vec<TypedExpr>,
    },
    FieldAccess {
        receiver: Box<TypedExpr>,
        field: String,
    },
    StringLit(String),
    ByteStringLit(Vec<u8>),
    If {
        cond: Box<TypedExpr>,
        then_stmts: Vec<TypedStmt>,
        then_expr: Option<Box<TypedExpr>>,
        else_stmts: Vec<TypedStmt>,
        else_expr: Option<Box<TypedExpr>>,
    },
    /// `&expr` — reference expression
    Ref(Box<TypedExpr>),
}

#[derive(Clone, Debug)]
pub enum TypedStmt {
    Let {
        name: String,
        expr: TypedExpr,
        /// Sunny-karp (2026-06-25) — true once `insert_scope_end_drops`
        /// has appended a `Drop::drop(&name)` call for this let's type.
        /// Eager pass at `after_rust_analysis` sets this for any local
        /// whose type's drop status is known then (concretely-typed +
        /// `Vec<T>`-style RustType with Param-bearing args also count
        /// because `tcx.adt_destructor` works on the ADT def). Bare
        /// `TypeParam` locals stay `false` — the late pass at mono
        /// re-checks them once the substituted type is concrete and
        /// synthesizes the drop if needed. Idempotent: the late pass
        /// skips any let already marked true.
        drop_synthesized: bool,
    },
    ExprStmt(TypedExpr),
    While { cond: TypedExpr, body: TypedBlock },
    Assign { name: String, expr: TypedExpr },
}

#[derive(Clone, Debug)]
pub struct TypedBlock {
    pub stmts: Vec<TypedStmt>,
    pub ret: Option<TypedExpr>,
}
