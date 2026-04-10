use std::collections::HashMap;
use crate::toylang::typed_ast::ResolvedType;

/// A Toylang struct field.
#[derive(Clone, Debug)]
pub struct ToyField {
    pub name: String,
    pub rust_type: ResolvedType,
}

/// A Toylang struct definition.
#[derive(Clone, Debug)]
pub struct ToyStruct {
    pub type_params: Vec<String>,   // e.g. ["A", "B"]; empty for non-generic
    pub fields: Vec<ToyField>,
}

/// All Toylang definitions visible to the current compilation.
pub struct ToylangRegistry {
    pub structs: HashMap<String, ToyStruct>,
    pub functions: HashMap<String, ToyFunction>,
    /// Rust `use` imports (e.g. "std::alloc::Global"). Emitted as `pub use` in stubs.
    pub imports: Vec<String>,
}

/// A parsed parameter in a Toylang function signature.
#[derive(Clone, Debug)]
pub struct ToyParam {
    pub name: String,
    pub ty: ResolvedType,
}

#[derive(Clone, Debug)]
pub struct ToyFunction {
    pub type_params: Vec<String>,   // e.g. ["T"]; empty for non-generic functions
    pub params: Vec<ToyParam>,
    pub return_ty: Option<ResolvedType>,
    pub body: Option<crate::toylang::ast::Block>,
}
