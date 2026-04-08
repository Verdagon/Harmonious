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

pub fn generate(registry: &ToylangRegistry) -> String {
    let mut items: Vec<syn::Item> = Vec::new();
    let mut extern_fns: Vec<TokenStream> = Vec::new();
    let mut wrapper_fns: Vec<syn::Item> = Vec::new();
    let mut impl_blocks: Vec<syn::Item> = Vec::new();

    // Emit pub use for each toylang import
    for import_path in &registry.imports {
        let path: syn::Path = syn::parse_str(import_path)
            .unwrap_or_else(|e| panic!("invalid import path '{}': {}", import_path, e));
        items.push(parse_quote! { pub use #path; });
    }

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

        let mut accessor_methods: Vec<TokenStream> = Vec::new();

        for field in &toy_struct.fields {
            let field_ident = format_ident!("{}", field.name);
            let field_ty = field_type_to_syn(&field.rust_type);

            // unreachable!() body — per_instance_mir handles all accessor instances.
            accessor_methods.push(quote! {
                pub fn #field_ident(&self) -> &#field_ty {
                    unreachable!()
                }
            });

            // Extern declaration only for non-generic (mir_built intercepts these)
            if !is_generic {
                let accessor_sym = format_ident!("__toylang_accessor_{}_{}", name, field.name);
                extern_fns.push(quote! {
                    fn #accessor_sym(s: *const #ident) -> *const #field_ty;
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

    // Generate public wrapper functions for ALL toylang functions (with bodies).
    // For non-generic functions with external_symbol, also emit extern "C" declarations.
    // The wrapper has an unreachable!() body — per_instance_mir/mir_built intercepts it.
    for (_name, toy_fn) in &registry.functions {
        if toy_fn.body.is_none() {
            continue;
        }

        // Extern declaration only for concrete (non-generic) functions.
        // Generic functions go through per_instance_mir (no extern needed).
        if toy_fn.type_params.is_empty() {
            let sym = format!("__toylang_impl_{}", _name);
            let fn_ident = format_ident!("{}", sym);
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
        }

        // Public wrapper function (user-facing signature) — for ALL functions
        let ret: syn::Type = match toy_fn.return_ty.as_deref() {
            Some(ty) => syn::parse_str(ty)
                .unwrap_or_else(|e| panic!("invalid return type '{}': {}", ty, e)),
            None => parse_quote!(()),
        };
        let wrapper_name = if _name == "main" { "__toylang_main" } else { _name.as_str() };
        let wrapper_ident = format_ident!("{}", wrapper_name);
        let user_params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
            let pname = format_ident!("{}", p.name);
            let pty: syn::Type = syn::parse_str(&p.ty)
                .unwrap_or_else(|e| panic!("invalid param type '{}': {}", p.ty, e));
            quote! { #pname: #pty }
        }).collect();

        let wrapper: syn::Item = if toy_fn.type_params.is_empty() {
            parse_quote! {
                pub fn #wrapper_ident(#(#user_params),*) -> #ret {
                    unreachable!()
                }
            }
        } else {
            let fn_type_params: Vec<syn::Ident> = toy_fn.type_params.iter()
                .map(|p| format_ident!("{}", p))
                .collect();
            parse_quote! {
                pub fn #wrapper_ident<#(#fn_type_params),*>(#(#user_params),*) -> #ret {
                    unreachable!()
                }
            }
        };
        wrapper_fns.push(wrapper);
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
