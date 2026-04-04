use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;

use crate::toylang::registry::{ToylangRegistry, ToyFieldType};

/// Convert a ToyFieldType to a syn::Type for use in Rust stub code.
fn field_type_to_syn(ft: &ToyFieldType) -> syn::Type {
    match ft {
        ToyFieldType::I32 => parse_quote!(i32),
        ToyFieldType::I64 => parse_quote!(i64),
        ToyFieldType::F64 => parse_quote!(f64),
        ToyFieldType::Bool => parse_quote!(bool),
        ToyFieldType::TypeParam(p) => {
            let p_ident = format_ident!("{}", p);
            parse_quote!(#p_ident)
        }
        ToyFieldType::ToyStruct(name) => {
            let ident = format_ident!("{}", name);
            parse_quote!(#ident)
        }
        ToyFieldType::RustGeneric(name, args) => {
            let ident = format_ident!("{}", name);
            let arg_types: Vec<syn::Type> = args.iter().map(|a| field_type_to_syn(a)).collect();
            parse_quote!(#ident<#(#arg_types),*>)
        }
    }
}

/// Generate a Rust expression that computes the byte offset of field `field_idx`
/// in a C-style (repr(C)) struct layout, given field types as syn types.
/// Returns a TokenStream that evaluates to `usize`.
///
/// STOPGAP: This is used for generic struct accessors because mir_built fires once
/// per definition (not per instantiation), so we can't route generic accessors through
/// the extern call pipeline. The rustc fork (per_instance_mir) will replace this.
fn c_layout_offset_expr(fields: &[syn::Type], field_idx: usize) -> TokenStream {
    if field_idx == 0 {
        return quote! { 0usize };
    }
    let mut tokens = quote! { 0usize };
    for i in 0..field_idx {
        let prev_ty = &fields[i];
        let next_ty = &fields[i + 1];
        tokens = quote! {
            {
                let offset = #tokens + std::mem::size_of::<#prev_ty>();
                let align = std::mem::align_of::<#next_ty>();
                (offset + align - 1) & !(align - 1)
            }
        };
    }
    tokens
}

pub fn generate(registry: &ToylangRegistry) -> String {
    let mut items: Vec<syn::Item> = Vec::new();
    let mut extern_fns: Vec<TokenStream> = Vec::new();
    let mut wrapper_fns: Vec<syn::Item> = Vec::new();
    let mut impl_blocks: Vec<syn::Item> = Vec::new();

    // Generate opaque struct definitions + accessor methods
    for (name, toy_struct) in &registry.structs {
        let ident = format_ident!("{}", name);
        let is_generic = !toy_struct.type_params.is_empty();

        // Opaque struct — layout_of reports 0 fields, so rustc never indexes
        // into the ADT's fields. We just need PhantomData to "use" generic type params.
        let item: syn::ItemStruct = if !is_generic {
            parse_quote! {
                pub struct #ident(());
            }
        } else {
            let type_params: Vec<syn::Ident> = toy_struct.type_params.iter()
                .map(|p| format_ident!("{}", p))
                .collect();
            parse_quote! {
                pub struct #ident<#(#type_params),*>(std::marker::PhantomData<(#(#type_params),*)>);
            }
        };
        items.push(syn::Item::Struct(item));

        // Collect syn types for all fields (used by generic offset computation)
        let field_syn_types: Vec<syn::Type> = toy_struct.fields.iter()
            .map(|f| field_type_to_syn(&f.rust_type))
            .collect();

        let mut accessor_methods: Vec<TokenStream> = Vec::new();

        for (field_idx, field) in toy_struct.fields.iter().enumerate() {
            let field_ident = format_ident!("{}", field.name);
            let field_ty = field_type_to_syn(&field.rust_type);

            if !is_generic {
                // Non-generic: unreachable!() body intercepted by mir_built → extern call
                accessor_methods.push(quote! {
                    pub fn #field_ident(&self) -> &#field_ty {
                        unreachable!()
                    }
                });

                let accessor_sym = format_ident!("__toylang_accessor_{}_{}", name, field.name);
                extern_fns.push(quote! {
                    fn #accessor_sym(s: *const #ident) -> *const #field_ty;
                });
            } else {
                // Generic: inline Rust pointer math (STOPGAP until per_instance_mir fork).
                // Rustc monomorphizes this for each instantiation, computing correct offsets.
                let offset_expr = c_layout_offset_expr(&field_syn_types, field_idx);
                accessor_methods.push(quote! {
                    pub fn #field_ident(&self) -> &#field_ty {
                        unsafe {
                            let base = self as *const Self as *const u8;
                            let offset = #offset_expr;
                            &*(base.add(offset) as *const #field_ty)
                        }
                    }
                });
            }
        }

        if !accessor_methods.is_empty() {
            let impl_block: syn::Item = if !is_generic {
                parse_quote! {
                    impl #ident {
                        #(#accessor_methods)*
                    }
                }
            } else {
                let type_params: Vec<syn::Ident> = toy_struct.type_params.iter()
                    .map(|p| format_ident!("{}", p))
                    .collect();
                parse_quote! {
                    impl<#(#type_params),*> #ident<#(#type_params),*> {
                        #(#accessor_methods)*
                    }
                }
            };
            impl_blocks.push(impl_block);
        }
    }

    // Generate extern "C" declarations AND public wrapper functions for each
    // toylang function. The wrapper has an unreachable!() body — mir_built will
    // intercept it and replace the body with a MIR call stub.
    for (_name, toy_fn) in &registry.functions {
        if let Some(ref sym) = toy_fn.external_symbol {
            let fn_ident = format_ident!("{}", sym);

            // Extern declaration (matches Rust ABI — no _deps parameter)
            let extern_params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
                let pname = format_ident!("{}", p.name);
                let pty: syn::Type = syn::parse_str(&p.ty)
                    .unwrap_or_else(|e| panic!("invalid param type '{}': {}", p.ty, e));
                quote! { #pname: #pty }
            }).collect();

            let ret: syn::Type = match toy_fn.return_ty.as_deref() {
                Some(ty) => syn::parse_str(ty)
                    .unwrap_or_else(|e| panic!("invalid return type '{}': {}", ty, e)),
                None => parse_quote!(()),
            };

            extern_fns.push(quote! {
                pub fn #fn_ident(#(#extern_params),*) -> #ret;
            });

            // Public wrapper function (user-facing signature, no _deps)
            let wrapper_ident = format_ident!("{}", _name);
            let user_params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
                let pname = format_ident!("{}", p.name);
                let pty: syn::Type = syn::parse_str(&p.ty)
                    .unwrap_or_else(|e| panic!("invalid param type '{}': {}", p.ty, e));
                quote! { #pname: #pty }
            }).collect();

            let wrapper: syn::Item = parse_quote! {
                pub fn #wrapper_ident(#(#user_params),*) -> #ret {
                    unreachable!()
                }
            };
            wrapper_fns.push(wrapper);
        }
    }

    if !extern_fns.is_empty() {
        let extern_block: syn::ItemForeignMod = parse_quote! {
            extern "C" {
                #(#extern_fns)*
            }
        };
        items.push(syn::Item::ForeignMod(extern_block));
    }

    // Add wrapper functions and impl blocks
    items.extend(wrapper_fns);
    items.extend(impl_blocks);

    // Assemble, validate, and format
    let tokens = quote! { #(#items)* };
    let file: syn::File = syn::parse2(tokens)
        .expect("stub_gen produced invalid Rust — this is a bug");
    prettyplease::unparse(&file)
}
