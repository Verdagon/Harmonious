use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::toylang::typed_ast::ResolvedType;

/// A Toylang struct field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyField {
    pub name: String,
    pub rust_type: ResolvedType,
}

/// A Toylang struct definition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyStruct {
    pub type_params: Vec<String>,   // e.g. ["A", "B"]; empty for non-generic
    pub fields: Vec<ToyField>,
}

/// All Toylang definitions visible to the current compilation.
///
/// `structs` and `functions` use `BTreeMap` rather than `HashMap` so iteration
/// is deterministic — load-bearing for sidecar byte-equality
/// (`docs/architecture/sidecar-format.md` "Determinism requirements"). The
/// `serialize_sidecar` / `deserialize_sidecar` machinery in
/// `crate::sidecar` round-trips this whole struct.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToylangRegistry {
    pub structs: BTreeMap<String, ToyStruct>,
    pub functions: BTreeMap<String, ToyFunction>,
    /// Rust `use` imports (e.g. "std::alloc::Global"). Emitted as `pub use` in stubs.
    pub imports: Vec<String>,
    /// Phase 2 C: toylang `impl rust_trait for toylang_type` blocks. Each entry
    /// is one source-level impl block; the methods inside are stored as
    /// `ToyFunction`s with the implicit `self` parameter elevated to an
    /// explicit `&ToyStruct` first parameter (architecture §6.2; Case 4).
    pub trait_impls: Vec<ToyImpl>,
}

/// Phase 2 C: a toylang `impl <RustTrait> for <ToyStruct> { fn … }` block.
/// `trait_name` is the short name of the Rust trait (e.g. "Clone"); it must
/// be `use`-imported elsewhere in the source so the oracle can resolve its
/// DefId. `self_type_name` is the toylang struct the impl is for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyImpl {
    pub trait_name: String,
    pub self_type_name: String,
    pub methods: Vec<ToyImplMethod>,
}

/// A method inside a `ToyImpl`. Stored as `ToyFunction` plus the method's
/// source-level name; the `params` of the inner function include `self` as a
/// `&ToyStruct` first parameter (synthesized by the parser from the
/// `&self` token).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyImplMethod {
    pub name: String,
    pub func: ToyFunction,
}

/// A parsed parameter in a Toylang function signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyParam {
    pub name: String,
    pub ty: ResolvedType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyFunction {
    pub type_params: Vec<String>,   // e.g. ["T"]; empty for non-generic functions
    pub params: Vec<ToyParam>,
    pub return_ty: Option<ResolvedType>,
    pub body: Option<crate::toylang::ast::Block>,
}
