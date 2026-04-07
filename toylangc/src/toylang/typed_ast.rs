//! Typed AST — every expression carries its resolved concrete type.
//!
//! Produced by the type resolution pass (`type_resolve.rs`) from the untyped
//! AST (`ast.rs`). Consumed by the LLVM backend (`llvm_gen.rs`).

/// A concrete, fully-resolved type. No TypeParam, no unresolved generics.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedType {
    I32,
    I64,
    F64,
    Bool,
    Void,
    Usize,
    Struct {
        name: String,
        /// Concrete type args (e.g. [I64, I64] for Pair<i64, i64>). Empty for non-generic structs.
        type_args: Vec<ResolvedType>,
        /// Resolved field types (TypeParams substituted with concrete types).
        field_types: Vec<ResolvedType>,
    },
    Vec {
        elem: Box<ResolvedType>,
    },
    Ref {
        inner: Box<ResolvedType>,
    },
    Str,
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
        type_args: Vec<String>,
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
}

#[derive(Clone, Debug)]
pub enum TypedStmt {
    Let { name: String, expr: TypedExpr },
    ExprStmt(TypedExpr),
}

#[derive(Clone, Debug)]
pub struct TypedFnBody {
    pub stmts: Vec<TypedStmt>,
    pub ret: Option<TypedExpr>,
}
