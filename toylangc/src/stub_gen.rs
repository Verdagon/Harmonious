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
        // Per @UTAIRZ, Str and ByteSlice render as bare Rust types; the `&` comes
        // from the `Ref` arm above when they're wrapped at use sites.
        ResolvedType::Str => parse_quote!(str),
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
        // into the ADT's fields via source-level walks. Source field count
        // must match the 0-field layout: `pub struct Foo;` (unit struct, 0
        // source fields) works; `pub struct Foo(());` (tuple struct with
        // one unit-typed field) silently breaks when the opaque type gets
        // monomorphized inside Vec<Foo> etc. — rustc's debuginfo walker
        // (`build_struct_type_di_node` + `build_generic_type_param_di_nodes`)
        // indexes FieldsShape by source position, our layout_of returns
        // FieldsShape::len()==0, the walk panics with "index out of bounds:
        // the len is 0 but the index is 0". Diagnosed during 5c.2 via the
        // Vec<Point, Global> migration tests.
        //
        // Generic case keeps PhantomData<(T, U, ...)> because the generic
        // params must be "used" somewhere in the struct body or rustc
        // errors with E0392 (`parameter `T` is never used`). The PhantomData
        // wrapper is a single source field, same source-vs-layout field
        // count mismatch in principle — but PhantomData<...> is itself a
        // ZST-wrapper whose layout rustc special-cases, so the debuginfo
        // walker doesn't recurse into it the same way it does for `()`.
        // Non-generic tests flex the ICE; generic-type-args-with-nested-Vec
        // tests haven't, at least in our current coverage. If a future
        // test hits the same ICE for generics, revisit — possible fixes
        // include splitting the struct declaration across all type params
        // as separate phantom fields or requesting a rustc-level opt-out
        // for opaque types.
        let item: syn::ItemStruct = if !is_generic {
            parse_quote! {
                pub struct #ident;
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

            // unreachable!() body — the `optimized_mir` override synthesizes
            // dep-registering bodies for every accessor DefId; the partitioner
            // override filters these accessor items out of the CGU slice so
            // rustc's LLVM backend never emits code for them. (Stage 4a/4b
            // retired `CODEGEN_SKIP_HOOK` in favor of this CGU-level filter.)
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
    // The wrapper has an unreachable!() body — the `optimized_mir` override
    // replaces it with a synthetic dep-registering body at monomorphization time.
    for (_name, toy_fn) in &registry.functions {
        if toy_fn.body.is_none() {
            // Stage 5c: body-less toylang fns are extern Rust fn declarations
            // (e.g. `fn println_int(x: i32);` in toylang source). Emit them
            // as `extern "C"` declarations in the stub rlib so:
            //   1. `find_extern_fn_def_id` resolves them to a local DefId
            //      during the rlib compile's consumer codegen.
            //   2. Toylang's emitted `.o` calls them via the unmangled symbol.
            //   3. A user-provided `#[no_mangle] pub extern "C" fn <name>` in
            //      a sibling crate (e.g. integration tests' `test_helpers`)
            //      satisfies the symbol at final link time.
            //
            // Only emit decls for fns whose signature is C-ABI-compatible.
            // For body-less toylang fns whose signatures use Option/Result
            // return types (a small handful of integration test fixtures),
            // a sret-style wrapper is needed instead — flagged for the
            // 5c.2 migration; for now just emit them as `extern "C"` and
            // accept rustc's `improper_ctypes` warning.
            let extern_params: Vec<TokenStream> = toy_fn.params.iter().map(|p| {
                let pname = format_ident!("{}", p.name);
                let pty = resolved_type_to_syn(&p.ty);
                quote! { #pname: #pty }
            }).collect();
            let ret: syn::Type = match &toy_fn.return_ty {
                Some(ty) => resolved_type_to_syn(ty),
                None => parse_quote!(()),
            };
            let fn_ident = format_ident!("{}", _name);
            extern_fns.push(quote! {
                pub fn #fn_ident(#(#extern_params),*) -> #ret;
            });
            continue;
        }

        // Extern declaration only for concrete (non-generic) functions.
        // Generic functions flow through the `optimized_mir` override at
        // monomorphization time (no extern needed; their symbols come from
        // the consumer's backend — the partitioner override filters them
        // out of rustc's CGU slice, replacing the retired `CODEGEN_SKIP_HOOK`).
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
    // forced by the facade's partitioner override, which mutates
    // `MonoItemData` to `(Linkage::External, Visibility::Default)` on items
    // surviving the consumer filter in `__lang_stubs`. Stage 4c retired the
    // prior `VISIBILITY_OVERRIDE_HOOK` rustc-fork patch in favor of this
    // post-partition mutation (see @DPSFDOZ-related partition.rs docs and
    // risks.md §B2 for the Outcome A assumption stack).
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
