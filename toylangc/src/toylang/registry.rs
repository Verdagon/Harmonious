use std::collections::HashMap;

/// A Toylang struct field.
#[derive(Clone, Debug)]
pub struct ToyField {
    pub name: String,
    /// The Rust type of this field, as a string that rustc can resolve.
    /// For now we only support primitive Rust types.
    pub rust_type: ToyFieldType,
}

#[derive(Clone, Debug)]
pub enum ToyFieldType {
    I32,
    I64,
    F64,
    Bool,
    TypeParam(String),   // e.g. "A", "B"
}

/// A Toylang struct definition.
#[derive(Clone, Debug)]
pub struct ToyStruct {
    #[allow(dead_code)]
    pub name: String,
    pub type_params: Vec<String>,   // e.g. ["A", "B"]; empty for non-generic
    pub fields: Vec<ToyField>,
}

/// All Toylang definitions visible to the current compilation.
pub struct ToylangRegistry {
    pub structs: HashMap<String, ToyStruct>,
    pub functions: HashMap<String, ToyFunction>,
}


/// A parsed parameter in a Toylang function signature.
#[derive(Clone, Debug)]
pub struct ToyParam {
    pub name: String,
    pub ty: String,
}

#[derive(Clone, Debug)]
pub struct ToyFunction {
    #[allow(dead_code)]
    pub name: String,
    pub params: Vec<ToyParam>,
    pub return_ty: Option<String>,
    pub body: Option<crate::toylang::ast::FnBody>,
    /// If set, this function was compiled by the Toylang LLVM backend.
    /// The value is the external symbol name (e.g. "__toylang_impl_make_counter").
    pub external_symbol: Option<String>,
}
