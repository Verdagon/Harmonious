use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;

use crate::toylang::registry::ToylangRegistry;
use crate::toylang::typed_ast::ResolvedType;

/// Convert a ResolvedType to a syn::Type for use in Rust stub code.
fn resolved_type_to_syn(ty: &ResolvedType) -> syn::Type {
    match ty {
        ResolvedType::I32 => parse_quote!(i32),
        ResolvedType::I64 => parse_quote!(i64),
        ResolvedType::F64 => parse_quote!(f64),
        ResolvedType::Bool => parse_quote!(bool),
        ResolvedType::Usize => parse_quote!(usize),
        ResolvedType::Void => parse_quote!(()),
        ResolvedType::TypeParam(name) => {
            let ident = format_ident!("{}", name);
            parse_quote!(#ident)
        }
        ResolvedType::StructRef { name, type_args }
        | ResolvedType::Struct { name, type_args, .. }
        | ResolvedType::RustType { name, type_args } => {
            let ident = format_ident!("{}", name);
            if type_args.is_empty() {
                parse_quote!(#ident)
            } else {
                let args: Vec<syn::Type> = type_args.iter().map(resolved_type_to_syn).collect();
                parse_quote!(#ident<#(#args),*>)
            }
        }
        ResolvedType::Ref { inner } => {
            let inner_ty = resolved_type_to_syn(inner);
            parse_quote!(&#inner_ty)
        }
        ResolvedType::Str => parse_quote!(&str),
        ResolvedType::ByteSlice => parse_quote!([u8]),
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
            let field_ty = resolved_type_to_syn(&field.rust_type);

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
                let pty = resolved_type_to_syn(&p.ty);
                quote! { #pname: #pty }
            }).collect();

            let ret: syn::Type = match &toy_fn.return_ty {
                Some(ty) => resolved_type_to_syn(ty),
                None => parse_quote!(()),
            };

            extern_fns.push(quote! {
                pub fn #fn_ident(#(#extern_params),*) -> #ret;
            });
        }

        // Public wrapper function (user-facing signature) — for ALL functions
        let ret: syn::Type = match &toy_fn.return_ty {
            Some(ty) => resolved_type_to_syn(ty),
            None => parse_quote!(()),
        };
        // Per @MBMRVZ, `main` is renamed to `__toylang_main` so the
        // hand-written Rust shim (`fn main() { __toylang_main(); }`)
        // can call it with a fixed void-return signature. This is the
        // reason toylang `fn main()` must have a void tail — the shim
        // won't pass an sret buffer.
        let wrapper_name = if _name == "main" { crate::oracle::TOYLANG_MAIN } else { _name.as_str() };
        let wrapper_ident = format_ident!("{}", wrapper_name);
        let user_params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
            let pname = format_ident!("{}", p.name);
            let pty = resolved_type_to_syn(&p.ty);
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

    // Phase 6: stdlib-method wrappers. Each takes the receiver by raw pointer
    // (so toylang's existing recv_ptr calling convention matches verbatim) and
    // uses ptr::read to consume the value before calling the inline method.
    // #[inline(never)] is mandatory — without it rustc could inline the wrapper
    // itself, putting us back at "no callable symbol." External linkage is
    // forced by the rustc-fork patch in partitioning.rs (Phase 6 step 1).
    items.push(parse_quote! {
        #[inline(never)]
        pub unsafe fn __toylang_option_unwrap<T>(o: *mut core::option::Option<T>) -> T {
            core::ptr::read(o).unwrap()
        }
    });
    items.push(parse_quote! {
        #[inline(never)]
        pub unsafe fn __toylang_result_unwrap<T, E: core::fmt::Debug>(r: *mut core::result::Result<T, E>) -> T {
            core::ptr::read(r).unwrap()
        }
    });

    // Assemble, validate, and format
    let tokens = quote! { #(#items)* };
    let file: syn::File = syn::parse2(tokens)
        .expect("stub_gen produced invalid Rust — this is a bug");
    prettyplease::unparse(&file)
}
