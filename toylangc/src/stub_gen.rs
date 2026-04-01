use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;

use crate::toylang::registry::{ToylangRegistry, ToyFieldType};

pub fn generate(registry: &ToylangRegistry) -> String {
    let mut items: Vec<syn::Item> = Vec::new();

    // Generate struct definitions
    for (name, toy_struct) in &registry.structs {
        let ident = format_ident!("{}", name);

        let fields: Vec<TokenStream> = toy_struct.fields.iter().map(|f| {
            let fname = format_ident!("{}", f.name);
            let ftype: syn::Type = match &f.rust_type {
                ToyFieldType::I32 => parse_quote!(i32),
                ToyFieldType::I64 => parse_quote!(i64),
                ToyFieldType::F64 => parse_quote!(f64),
                ToyFieldType::Bool => parse_quote!(bool),
                ToyFieldType::TypeParam(p) => {
                    let p_ident = format_ident!("{}", p);
                    parse_quote!(#p_ident)
                }
            };
            quote! { pub #fname: #ftype }
        }).collect();

        let item: syn::ItemStruct = if toy_struct.type_params.is_empty() {
            parse_quote! {
                pub struct #ident {
                    #(#fields),*
                }
            }
        } else {
            let type_params: Vec<syn::Ident> = toy_struct.type_params.iter()
                .map(|p| format_ident!("{}", p))
                .collect();
            parse_quote! {
                pub struct #ident<#(#type_params),*> {
                    #(#fields),*
                }
            }
        };

        items.push(syn::Item::Struct(item));
    }

    // Generate extern "C" block for externally-compiled functions.
    // Every extern function gets exactly one extra `_deps: *const ()` parameter.
    let mut extern_fns: Vec<TokenStream> = Vec::new();

    for (_name, toy_fn) in &registry.functions {
        if let Some(ref sym) = toy_fn.external_symbol {
            let fn_ident = format_ident!("{}", sym);

            let mut params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
                let pname = format_ident!("{}", p.name);
                let pty: syn::Type = syn::parse_str(&p.ty)
                    .unwrap_or_else(|e| panic!("invalid param type '{}': {}", p.ty, e));
                quote! { #pname: #pty }
            }).collect();

            params.push(quote! { _deps: *const () });

            let ret: syn::Type = match toy_fn.return_ty.as_deref() {
                Some(ty) => syn::parse_str(ty)
                    .unwrap_or_else(|e| panic!("invalid return type '{}': {}", ty, e)),
                None => parse_quote!(()),
            };

            extern_fns.push(quote! {
                pub fn #fn_ident(#(#params),*) -> #ret;
            });
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

    // Assemble, validate, and format
    let tokens = quote! { #(#items)* };
    let file: syn::File = syn::parse2(tokens)
        .expect("stub_gen produced invalid Rust — this is a bug");
    prettyplease::unparse(&file)
}
