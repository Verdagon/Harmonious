//! Typed AST — every expression carries its resolved concrete type.
//!
//! Produced by the type resolution pass (`type_resolve.rs`) from the untyped
//! AST (`ast.rs`). Consumed by the LLVM backend (`llvm_gen.rs`).
//!
//! ## Two-enum split (Option B, 2026-06-25)
//!
//! Types in this compiler live in one of two enums depending on where in the
//! pipeline the value sits:
//!
//! - [`SourceType`] — **parser-shape**. Carries `StructRef { name, type_args }`
//!   for any Sky struct reference. Field info has NOT been looked up yet.
//!   Produced by the parser, stored in the registry, rendered by stub_gen,
//!   serialized in the sidecar. The shape rustc gives us back (`tcx.Ty` →
//!   `oracle::rustc_ty_to_source_type`) also lands here because rustc, like
//!   the parser, hands us "a name + args" without field expansion.
//!
//! - [`ResolvedType`] — **resolved-shape**. Sky structs appear as
//!   `Struct { name, type_args, field_types }` with `field_types` MANDATORY
//!   and fully expanded. No `StructRef` variant exists; the parser-shape
//!   form is unrepresentable. Consumed by the typed AST, codegen,
//!   layout queries, the substitution walk.
//!
//! The boundary between them is a single chokepoint: [`oracle::resolve_source_type`].
//! Going `SourceType → ResolvedType` looks up struct fields (recursively),
//! substitutes TypeParam args, and produces the resolved form. Going the
//! other direction (`ResolvedType → SourceType`) is a structural map — fields
//! are simply discarded — implemented as [`ResolvedType::to_source_type`]
//! when sidecar / stub_gen consumers need to round-trip.
//!
//! The motivation for the split lives in `handoff.md` under "Option B" and in
//! `rust-interop-architecture.md` §F.20 surprise #3: pre-split, a pure-AST
//! walk that produced fresh `ResolvedType` values without remembering to chain
//! `resolve_struct_fields` would silently produce a `StructRef`-bearing typed
//! AST that codegen would treat as opaque/zero-sized. The split makes that
//! state unrepresentable — codegen and substitution simply cannot see a
//! `StructRef` because it's not a `ResolvedType` variant.

use serde::{Deserialize, Serialize};

/// Parser-shape type representation. Carries `StructRef` because field info
/// hasn't been looked up. See the module doc-comment for the two-enum design.
///
/// `SourceType` lives in:
/// - the parser AST (`ast.rs`'s `Expr::*::type_args`),
/// - the registry (`ToyField.rust_type`, `ToyParam.ty`, `ToyFunction.return_ty`,
///   `typeid_table` args, `synthesize_accessor_fn` body),
/// - stub_gen's rendering layer,
/// - the sidecar (via the registry),
/// - the immediate output of `oracle::rustc_ty_to_source_type`.
///
/// Promote to [`ResolvedType`] via `oracle::resolve_source_type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SourceType {
    I32,
    I64,
    F64,
    Bool,
    Void,
    Usize,
    /// Unresolved type parameter (e.g. "T"). Stays a TypeParam through
    /// promotion to [`ResolvedType::TypeParam`] — it's a normalization
    /// terminal in both forms.
    TypeParam(String),
    /// Reference to a struct by name — field layout not yet resolved.
    /// Becomes [`ResolvedType::Struct`] (with mandatory `field_types`)
    /// after promotion.
    StructRef {
        name: String,
        type_args: Vec<SourceType>,
    },
    RustType {
        name: String,
        type_args: Vec<SourceType>,
    },
    Ref {
        inner: Box<SourceType>,
    },
    /// The unsized string type `str`. Always appears inside `Ref` as `&str`.
    /// Per @UTAIRZ.
    Str,
    /// The unsized byte slice type `[u8]`. Always appears inside `Ref` as `&[u8]`.
    /// Per @UTAIRZ.
    ByteSlice,
}

/// Resolved-shape type representation. Sky structs MUST appear as `Struct`
/// with `field_types` fully expanded; no `StructRef` variant exists.
/// See the module doc-comment for the two-enum design.
///
/// `ResolvedType` lives in:
/// - the typed AST (`TypedExpr.ty`),
/// - codegen's input,
/// - the layout query,
/// - the substitution walk's output,
/// - the cached typed bodies on `ToylangState.typed_bodies`.
///
/// Demote to [`SourceType`] via [`ResolvedType::to_source_type`] when
/// crossing back into sidecar / stub_gen territory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResolvedType {
    I32,
    I64,
    F64,
    Bool,
    Void,
    Usize,
    /// Unresolved type parameter (e.g. "T"). Remains a TypeParam through the
    /// typed AST; substitution at mono replaces it with a concrete
    /// `ResolvedType`. Sunny-karp: oracle queries accept Param-bearing args
    /// via `caller_type_params` lookup → `ty::Ty::new_param`.
    TypeParam(String),
    /// Fully resolved Sky struct. `field_types` is mandatory and is the
    /// recursively-resolved type of each field, with TypeParam substitution
    /// applied per `type_args`.
    Struct {
        name: String,
        /// Concrete type args (e.g. `[I64, I64]` for `Pair<i64, i64>`).
        /// Empty for non-generic structs.
        type_args: Vec<ResolvedType>,
        /// Resolved field types. Must match `registry.structs[name].fields`
        /// length; field i's type is the corresponding field's declared
        /// `SourceType` with `type_args` substituted in and promoted.
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
    Str,
    /// The unsized byte slice type `[u8]`. Always appears inside `Ref` as `&[u8]`.
    ByteSlice,
}

impl ResolvedType {
    /// Demote to parser-shape `SourceType` by dropping `Struct.field_types`.
    /// Inverse of `oracle::resolve_source_type` (modulo the field info).
    ///
    /// Used when a resolved type needs to cross back into a registry- or
    /// sidecar-facing position. Most pipeline code should NOT need this —
    /// the typed AST and codegen path stay in `ResolvedType`. The current
    /// caller is `DiscoveredTraitImplInstance.concrete_args` which stays
    /// `ResolvedType` (it's set from rustc Instance args via the
    /// promotion chain), so even that doesn't need demotion today.
    pub fn to_source_type(&self) -> SourceType {
        match self {
            ResolvedType::I32 => SourceType::I32,
            ResolvedType::I64 => SourceType::I64,
            ResolvedType::F64 => SourceType::F64,
            ResolvedType::Bool => SourceType::Bool,
            ResolvedType::Void => SourceType::Void,
            ResolvedType::Usize => SourceType::Usize,
            ResolvedType::TypeParam(n) => SourceType::TypeParam(n.clone()),
            ResolvedType::Struct { name, type_args, .. } => SourceType::StructRef {
                name: name.clone(),
                type_args: type_args.iter().map(|t| t.to_source_type()).collect(),
            },
            ResolvedType::RustType { name, type_args } => SourceType::RustType {
                name: name.clone(),
                type_args: type_args.iter().map(|t| t.to_source_type()).collect(),
            },
            ResolvedType::Ref { inner } => SourceType::Ref {
                inner: Box::new(inner.to_source_type()),
            },
            ResolvedType::Str => SourceType::Str,
            ResolvedType::ByteSlice => SourceType::ByteSlice,
        }
    }
}

impl SourceType {
    /// True iff the source-shape type or any of its descendants contains a
    /// `TypeParam`. Used by the eager type-resolve to decide whether the
    /// surrounding fn's body has any abstract args still in play.
    ///
    /// Lives on `SourceType` rather than as a free fn because the recursion
    /// stays inside parser-shape — promoting just to check would be wasteful.
    #[allow(dead_code)]
    pub fn contains_type_param(&self) -> bool {
        match self {
            SourceType::TypeParam(_) => true,
            SourceType::I32
            | SourceType::I64
            | SourceType::F64
            | SourceType::Bool
            | SourceType::Void
            | SourceType::Usize
            | SourceType::Str
            | SourceType::ByteSlice => false,
            SourceType::StructRef { type_args, .. } | SourceType::RustType { type_args, .. } => {
                type_args.iter().any(|t| t.contains_type_param())
            }
            SourceType::Ref { inner } => inner.contains_type_param(),
        }
    }
}

/// A typed expression — every node knows its concrete (resolved) type.
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
