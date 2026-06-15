use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;

use crate::toylang::registry::ToylangRegistry;
use crate::toylang::typed_ast::ResolvedType;

/// Course-correct #17: zero-param is the degenerate case of N-param.
/// Returns `(impl_generics, ty_generics)` for an `impl ... Name ... { … }`
/// block. For zero type params both are empty token streams; for N params
/// they are `<A, B, …>` (so `impl<A, B> Name<A, B>`).
fn generics_for_impl_block(type_params: &[String]) -> (TokenStream, TokenStream) {
    // arch-fence-allow: degenerate-case-fast-path (empty token stream encodes "no generics").
    if type_params.is_empty() {
        return (TokenStream::new(), TokenStream::new());
    }
    let idents: Vec<syn::Ident> = type_params.iter().map(|p| format_ident!("{}", p)).collect();
    let impl_generics = quote! { <#(#idents),*> };
    let ty_generics = quote! { <#(#idents),*> };
    (impl_generics, ty_generics)
}

/// Course-correct #17: returns the `<A, B>` generics clause for a function
/// declaration. Empty token stream for zero params (the degenerate case),
/// `<A, B, …>` for N.
fn fn_generics_clause(type_params: &[String]) -> TokenStream {
    // arch-fence-allow: degenerate-case-fast-path (empty token stream encodes "no generics").
    if type_params.is_empty() {
        return TokenStream::new();
    }
    let idents: Vec<syn::Ident> = type_params.iter().map(|p| format_ident!("{}", p)).collect();
    quote! { <#(#idents),*> }
}

// Course-correct #17 — three of four divergences in this file are closed.
// One remains, gated by Rust syntax:
//
//   1. Struct shape: CLOSED (Phase E completion). Universal shape
//      `pub struct Foo<P...>(PhantomData<(P...)>);` at every N including
//      N=0. Was previously gated by a rustc debuginfo-walker ICE on
//      opaque non-generic ADTs with any source-level field; ICE
//      eliminated by fork patch 4 (`e67de69ef35` on per-instance-mir)
//      which clamps build_struct_type_di_node's source-field walk to
//      min(source.len(), layout.fields.count()).
//
//   2. Extern decls (~lines 167, 220): only emitted for non-generic
//      items. Rust's `extern "C" { ... }` doesn't permit generic items;
//      the symbol-per-monomorphization problem (architecture §5.1
//      Option B failure) is real and there's no syntactic workaround.
//      Sky's locked design retires this entire mechanism by emitting
//      all Sky bodies in the binary compile via the codegen plugin (no
//      per-symbol extern decl needed); that work waits on the deeper
//      half of course-correct #4 (Sky's `codegen_crate` walks the queue
//      inline via Inkwell). Until then toylang's emission still needs
//      the extern decls for the non-generic path. The two sites carry
//      `arch-fence-allow: extern-C-cannot-be-generic` markers.
//
// The split here is documented mechanism, not the compiler-law
// violation the original course-correct entry flagged. The cosmetic
// divergences (impl-block header, fn declaration header) ARE unified
// above.

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
            // arch-fence-allow: degenerate-case-fast-path (skip the `<>` decoration when empty).
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

    // Emit the `__SKY_STUBS_MARKER` (architecture §6.3). The facade's
    // `is_from_lang_stubs` predicate walks crate-root children for this
    // marker rather than matching against the literal crate name, so
    // future per-Sky-library stub rlibs (Phase 3 E.2/E.3, named after
    // the Sky library rather than the shared `__lang_stubs`) are still
    // recognized as Sky stub rlibs without a predicate change.
    items.push(parse_quote! { pub const __SKY_STUBS_MARKER: () = (); });

    // Emit pub use for each toylang import
    for import_path in &registry.imports {
        let path: syn::Path = syn::parse_str(import_path)
            .unwrap_or_else(|e| panic!("invalid import path '{}': {}", import_path, e));
        items.push(parse_quote! { pub use #path; });
    }

    // Generate opaque struct definitions + accessor methods
    for (name, toy_struct) in &registry.structs {
        let ident = format_ident!("{}", name);

        // Phase E completion: universal struct shape, non-generic is the
        // degenerate case of generic (CLAUDE.md compiler law).
        //
        //   0 type params: `pub struct Foo(PhantomData<()>);`
        //   1 type param:  `pub struct Foo<T>(PhantomData<(T)>);`
        //   N type params: `pub struct Foo<A, B, ...>(PhantomData<(A, B, ...)>);`
        //
        // Single PhantomData<(P1, P2, ...)> source field at every N. The
        // empty-tuple `()` case is what previously ICE'd on rustc's
        // debuginfo walker (build_struct_type_di_node iterated source
        // FieldDefs and queried fields.offset(i) for i out of bounds when
        // layout_of reports 0 fields). Fork patch 4 (`e67de69ef35` on
        // per-instance-mir) clamps that walk to min(source.len(),
        // layout.fields.count()), making the universal shape safe.
        //
        // PhantomData was already the chosen wrapper for the generic case
        // because of rustc's "all generics must be used" rule (E0392). At
        // N=0 the rule is vacuously satisfied; PhantomData<()> still works
        // and keeps the shape uniform.
        let generics_clause = fn_generics_clause(&toy_struct.type_params);
        let type_params_idents: Vec<syn::Ident> = toy_struct.type_params.iter()
            .map(|p| format_ident!("{}", p))
            .collect();
        let item: syn::ItemStruct = parse_quote! {
            pub struct #ident #generics_clause (std::marker::PhantomData<(#(#type_params_idents),*)>);
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

            // (Phase E follow-up experiment: removed accessor extern decl.
            // The accessor symbol is emitted by Sky's codegen path, not
            // referenced by name from any Rust source in the stub rlib.
            // Both generic and non-generic accessors use this same path.)
        }

        if !accessor_methods.is_empty() {
            // Course-correct #17: one universal impl-block shape. Zero type
            // params is the degenerate case of N — `generics_for_impl_block`
            // returns an empty `<>` clause and an empty self-type generic
            // list when N=0, producing `impl Foo { … }` exactly as the prior
            // non-generic branch did.
            let (impl_generics, ty_generics) = generics_for_impl_block(&toy_struct.type_params);
            let impl_block: syn::Item = parse_quote! {
                impl #impl_generics #ident #ty_generics {
                    #(#accessor_methods)*
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

        // Session 10 — Sky architecture §9. Non-export body-bearing fns get
        // NO `pub fn` shell in the stub rlib (no DefId for rustc to name).
        // `main` is implicitly export (the parser already sets is_export=true
        // for it). The non-export discovery path runs entirely Sky-side via
        // `walk_and_stash_internal_callees` + `__toylang_internal_*` symbols.
        if !toy_fn.is_export {
            continue;
        }

        // (Phase E follow-up experiment: removed __toylang_impl_* extern
        // decl. The symbol is emitted by Sky's codegen path; Rust callers
        // reach it via the `symbol_name` query override rewriting
        // `__lang_stubs::<name>` → `__toylang_impl_<name>`. No name-based
        // lookup from Rust source. Removing this decl unifies the
        // generic and non-generic emission paths.)

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

        // Course-correct #17: one universal wrapper-fn shape. Zero type
        // params is the degenerate case — `fn_generics_clause` is empty for
        // N=0, producing `pub fn foo(…)` exactly as the prior non-generic
        // branch did.
        let fn_generics = fn_generics_clause(&toy_fn.type_params);
        let wrapper: syn::Item = parse_quote! {
            pub fn #wrapper_ident #fn_generics (#(#user_params),*) -> #ret {
                unreachable!()
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
    // Phase 2 C.4: emit one Rust `impl <RustTrait> for <ToyStruct> { fn … }`
    // block per toylang impl. The trait's short name (`Clone`) is what
    // toylang source spells; rustc's name-resolution walks the stub rlib's
    // `pub use` re-exports (emitted by the imports loop above) to find the
    // full path. The method body is `unreachable!()` exactly like every
    // other stub fn body — the consumer's per_instance_mir override (or its
    // codegen-backend successor) supplies the real body at the user-bin
    // compile.
    //
    // `&self` is reified to a `&Self` parameter in the generated Rust
    // (rustc requires the receiver syntax, not a `self: &Self` explicit
    // form). The remaining params and return type pass through
    // `resolved_type_to_syn` unchanged.
    for toy_impl in &registry.trait_impls {
        // Session 10 — Sky architecture §9. Non-export impl blocks get no
        // stub-rlib presence. Today every Rust caller path through a trait
        // impl method requires the impl to be export (rustc dispatches via
        // its DefId). A non-export impl would still emit Sky-internal
        // method bodies via the codegen queue but never surface to rustc.
        if !toy_impl.is_export {
            continue;
        }
        let trait_ident = format_ident!("{}", toy_impl.trait_name);
        let self_ident = format_ident!("{}", toy_impl.self_type_name);
        let method_items: Vec<syn::TraitItemFn> = toy_impl.methods.iter().map(|m| {
            let m_name = format_ident!("{}", m.name);
            // Skip the elevated `self: &Self` first param when rendering;
            // emit it as the receiver instead.
            let user_params: Vec<TokenStream> = m.func.params.iter().skip(1).map(|p| {
                let pname = format_ident!("{}", p.name);
                let pty = resolved_type_to_syn(&p.ty);
                quote! { #pname: #pty }
            }).collect();
            let ret: syn::Type = match &m.func.return_ty {
                Some(ty) => resolved_type_to_syn(ty),
                None => parse_quote!(()),
            };
            parse_quote! {
                fn #m_name(&self, #(#user_params),*) -> #ret {
                    unreachable!()
                }
            }
        }).collect();
        let impl_block: syn::ItemImpl = parse_quote! {
            impl #trait_ident for #self_ident {
                #(#method_items)*
            }
        };
        items.push(syn::Item::Impl(impl_block));
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toylang::registry::ToylangRegistry;

    /// Regression guard for Phase 3 E.1: the stub rlib's generated source
    /// must declare `__SKY_STUBS_MARKER` at the crate root. The facade's
    /// `is_from_lang_stubs` predicate walks for this marker; if stub_gen
    /// silently stops emitting it the predicate returns false everywhere
    /// and tests fail in mysterious unrelated ways (no consumer items
    /// filtered out of CGUs, no `per_instance_mir` override fires, etc.).
    #[test]
    fn marker_emitted_at_crate_root() {
        let reg = ToylangRegistry::default();
        let src = generate(&reg);
        assert!(
            src.contains("pub const __SKY_STUBS_MARKER"),
            "stub_gen output missing __SKY_STUBS_MARKER:\n{}",
            src
        );
    }

    /// Session 10 — Sky architecture §9. The architectural commitment:
    /// non-export body-bearing fns get NO `pub fn` shell in the stub rlib.
    /// This test fences the property at the stub_gen output level — if a
    /// future change accidentally emits a shell for a non-export item, the
    /// test fails loudly. Mirror of architecture A.5's spirit (the
    /// byte-identical-pass-through invariant): a CI-enforceable
    /// architectural commitment.
    #[test]
    fn non_export_body_bearing_fn_gets_no_stub_shell() {
        use crate::toylang::registry::{ToyFunction, ToyParam};
        use crate::toylang::typed_ast::ResolvedType;
        use crate::toylang::ast::Block;

        let mut reg = ToylangRegistry::default();
        // An export fn — should appear in the stub source.
        reg.functions.insert("exported_fn".to_string(), ToyFunction {
            type_params: vec![],
            params: vec![ToyParam { name: "x".to_string(), ty: ResolvedType::I32 }],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: None }),
            is_export: true,
        });
        // A NON-export fn — must NOT appear.
        reg.functions.insert("internal_only".to_string(), ToyFunction {
            type_params: vec![],
            params: vec![ToyParam { name: "x".to_string(), ty: ResolvedType::I32 }],
            return_ty: Some(ResolvedType::I32),
            body: Some(Block { stmts: vec![], ret: None }),
            is_export: false,
        });

        let src = generate(&reg);

        // Sanity: export emitted.
        assert!(
            src.contains("pub fn exported_fn"),
            "stub source should include exported fn, got:\n{}", src,
        );
        // Architectural property: non-export NOT emitted.
        assert!(
            !src.contains("pub fn internal_only"),
            "ARCHITECTURAL REGRESSION: non-export fn `internal_only` leaked into stub source. \
             Sky §9 / Session 10 commitment: non-export items must not surface to rustc. \
             Got:\n{}",
            src,
        );
        // Also assert the symbol isn't anywhere as a fn declaration —
        // catches creative patterns that aren't `pub fn` but still surface
        // a DefId rustc can name.
        assert!(
            !src.contains("fn internal_only"),
            "ARCHITECTURAL REGRESSION: any form of `fn internal_only` in stub source. \
             Got:\n{}",
            src,
        );
    }

    /// Phase 2 C.4: a `ToyImpl` entry in the registry produces an
    /// `impl Trait for Self { fn name(&self, …) -> Ret { unreachable!() } }`
    /// block in the generated stub source.
    #[test]
    fn impl_block_emitted_for_toy_impl() {
        use crate::toylang::registry::{
            ToyField, ToyFunction, ToyImpl, ToyImplMethod, ToyParam, ToyStruct,
        };
        use crate::toylang::typed_ast::ResolvedType;

        let mut reg = ToylangRegistry::default();
        reg.structs.insert("Widget".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField { name: "id".to_string(), rust_type: ResolvedType::I32 }],
        });
        reg.imports.push("std::clone::Clone".to_string());
        reg.trait_impls.push(ToyImpl {
            trait_name: "Clone".to_string(),
            self_type_name: "Widget".to_string(),
            is_export: true,
            methods: vec![ToyImplMethod {
                name: "clone".to_string(),
                func: ToyFunction {
                    type_params: vec![],
                    is_export: true,
                    params: vec![
                        ToyParam {
                            name: "self".to_string(),
                            ty: ResolvedType::Ref { inner: Box::new(
                                ResolvedType::StructRef {
                                    name: "Widget".to_string(),
                                    type_args: vec![],
                                },
                            ) },
                        },
                    ],
                    return_ty: Some(ResolvedType::StructRef {
                        name: "Widget".to_string(), type_args: vec![],
                    }),
                    body: None, // body irrelevant for stub emission (always unreachable!())
                },
            }],
        });

        let src = generate(&reg);
        assert!(src.contains("impl Clone for Widget"),
            "stub_gen output missing `impl Clone for Widget`:\n{}", src);
        assert!(src.contains("fn clone(&self)"),
            "stub_gen output missing `fn clone(&self)`:\n{}", src);
        assert!(src.contains("unreachable!()"),
            "stub_gen output missing `unreachable!()` body:\n{}", src);
    }
}
