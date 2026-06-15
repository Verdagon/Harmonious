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
