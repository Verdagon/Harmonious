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

// Course-correct #17 — all four documented divergences in this file are
// CLOSED as of Session 11. Recorded here for archaeological reference:
//
//   1. Struct shape: CLOSED via Phase E completion. Universal shape
//      `pub struct Foo<P...>(PhantomData<(P...)>);` at every N including
//      N=0. Was previously gated by a rustc debuginfo-walker ICE on
//      opaque non-generic ADTs with any source-level field; ICE
//      eliminated by fork patch 4 (`e67de69ef35` on per-instance-mir)
//      which clamps build_struct_type_di_node's source-field walk to
//      min(source.len(), layout.fields.count()). See `phase-e-investigation.md`.
//
//   2. Cosmetic impl-block + wrapper-fn header divergences: CLOSED via
//      `generics_for_impl_block` / `fn_generics_clause` helpers — both
//      return an empty TokenStream for N=0 so the caller doesn't branch.
//
//   3. `__toylang_impl_*` extern decl (Sky-emitted wrapper symbols):
//      CLOSED via removal. Vestigial — Sky's `symbol_name` query
//      override is the bridge between Rust callers and Sky-emitted
//      symbols; no forward declaration is needed (architecture §6.2).
//      Predated the symbol_name routing and never got cleaned up until
//      the Session 11 audit.
//
//   4. `__toylang_accessor_*` extern decl (struct field accessor symbols):
//      CLOSED via removal. Vestigial — no Rust source ever referenced
//      these by name; Sky's codegen emitted them as concrete symbols.
//
// The `extern "C" { ... }` block that remains carries only body-less
// toylang fn decls (e.g., `fn println_int(x: i32);` in toylang source
// → `extern "C" { pub fn println_int(...); }`). These describe REAL Rust
// functions the user provides elsewhere (e.g., test_helpers); the linker
// resolves them at final link. By their nature these decls can't take
// toylang-source generics — they're declaring foreign Rust fns whose
// ABIs are fixed. The remaining `is_generic`-style branches in this file
// (helpers + cosmetic emission of `Foo` vs `Foo<T>`) carry
// `arch-fence-allow: degenerate-case-fast-path` markers.
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

    // Phase E Path 2 — emit the universal opaque wrapper (architecture §10.6).
    // Every Sky struct's stub representation contains a `__ToylangOpaque<HASH>`
    // field carrying its content-addressed typeid. The wrapper itself has zero
    // source fields, so the rustc debuginfo walker iterates zero times when
    // it recurses into a wrapper instantiation — sidestepping the
    // source-vs-layout-field-count assumption (§10.4.5) that fork patch 4
    // patches defensively. Phase 3 starts using this declaration; Phase 1.2
    // just emits it without any current consumer.
    items.push(parse_quote! { pub struct __ToylangOpaque<const T: u64>; });

    // Emit pub use for each toylang import
    for import_path in &registry.imports {
        let path: syn::Path = syn::parse_str(import_path)
            .unwrap_or_else(|e| panic!("invalid import path '{}': {}", import_path, e));
        items.push(parse_quote! { pub use #path; });
    }

    // Generate opaque struct definitions + accessor methods
    for (name, toy_struct) in &registry.structs {
        let ident = format_ident!("{}", name);

        // Phase E Path 2 (Phase 3) — wrapper-as-field newtype shape
        // (architecture §10.4.5 path 2 / §10.6). Each Sky struct's stub
        // representation contains a `__ToylangOpaque<HASH>` field carrying
        // the struct's content-addressed typeid, plus (for generics) a
        // `PhantomData<(P1, P2, ...)>` carrier so the generic params are
        // "used" per rustc's E0392 rule:
        //
        //   0 type params: `pub struct Foo(__ToylangOpaque<HASH>);`
        //   1+ type params: `pub struct Foo<P...>(__ToylangOpaque<HASH>, PhantomData<(P...)>);`
        //
        // Layout matches source — `queries/layout.rs` reports 1 field
        // (non-generic) or 2 (generic), so the debuginfo walker's source-
        // vs-layout-field-count assumption (§10.4.5) holds without fork
        // patch 4. The wrapper itself has zero source fields; when the
        // walker recurses into it (Phase 4's intercept supplies the layout)
        // it iterates zero times, also safely.
        //
        // The Sky struct keeps its own DefId, so all existing
        // `impl Trait for Foo` blocks below work unchanged.
        let generics_clause = fn_generics_clause(&toy_struct.type_params);
        let type_params_idents: Vec<syn::Ident> = toy_struct.type_params.iter()
            .map(|p| format_ident!("{}", p))
            .collect();
        let typeid = crate::typeid::compute(name, &[]);
        let typeid_lit = syn::LitInt::new(&format!("{}u64", typeid), proc_macro2::Span::call_site());
        // Compiler-law: emit one universal struct shape regardless of N.
        // For N=0 this renders as
        //   `pub struct Foo(__ToylangOpaque<HASH>, std::marker::PhantomData<()>);`
        // (rustc's E0392 only triggers on *declared* generic params that
        // aren't used, so non-generic structs with `PhantomData<()>` are
        // fine — the unit tuple is a valid ZST type with no params).
        // For N>0 it renders as
        //   `pub struct Foo<A, B>(__ToylangOpaque<HASH>, std::marker::PhantomData<(A, B)>);`
        // — `fn_generics_clause` returns an empty TokenStream (not `<>`) for
        // N=0, so the same template handles both.
        //
        // The downstream `layout_of` query reports a matching 2-source-field
        // count at every N; see `queries/layout.rs`.
        let item: syn::ItemStruct = parse_quote! {
            pub struct #ident #generics_clause (
                __ToylangOpaque<#typeid_lit>,
                std::marker::PhantomData<(#(#type_params_idents),*)>,
            );
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
            //
            // Historical note (F1 investigation 2026-06-20): this site (and
            // the wrapper_fn + trait-impl-method sites below) previously
            // emitted `#[inline(never)]`. The stated rationales were:
            //   (1) Trip the `!share_generics()` early-return at -O>=2 so
            //       the v0 mangler picks `__lang_stubs` as the disambig.
            //   (2) Prevent rustc's MIR inliner from pulling the
            //       `unreachable!()` body cross-crate into Rust callers.
            // Both are obsolete under the current architecture:
            //   (1) Patch 5 (consumer_lang_active query + gated escape in
            //       Instance::upstream_monomorphization) + the __lang_stubs
            //       share_generics heuristic close B14 directly. See arch
            //       §25.2 B14 and §3.2 patch 5.
            //   (2) Arch §F.1 explicitly calls this concern wrong: the
            //       LTO inlining race was fixed structurally by Option 4's
            //       AvailableExternally linkage on consumer items. The
            //       earlier `#![no_builtins]` belt-and-suspenders mechanism
            //       was retired 2026-06-21 (§5.5 Round 2 E2 — matrix passes
            //       cleanly without it).
            // The remaining valid concern (arch §25.2 B12) is a vanilla
            // Rust crate consuming a Sky stub rlib without the Sky toolchain
            // — but v1 catches that case via build.rs's SKY_TOOLCHAIN_ACTIVE
            // check (§21.4) before codegen. The B12 protection acquires
            // meaning only under v2's opt-in precompiled-bodies feature
            // (§21.7), which is years away. When that work begins,
            // re-emit `#[inline(never)]` here (and at the two sites below)
            // gated on a precompiled-rlib-mode flag.
            //
            // Empirical payoff for removing the attribute: Sky's body
            // inlines into the user_bin's main at thin/fat LTO + -O3,
            // matching the architecturally-promised "interop is free with
            // LTO" perf claim. The inlining matrix's
            // SKY_EXPORT_LTO_INLINING_FINDING markers retire.
            // Phase C/D (Decision 3): tag accessor methods as Category B —
            // Sky's `fill_extra_modules` emits the real body; the partition
            // filter's two-gate predicate
            //   is_from_lang_stubs(tcx, def_id)
            //     && tcx.has_attrs_with_path(def_id, &[toylang, emit_consumer_body])
            // removes the `unreachable!()` placeholder from rustc's CGU list
            // before LLVM codegen. The marker is registered crate-wide via
            // `#![register_tool(toylang)]` in build.rs.
            accessor_methods.push(quote! {
                #[toylang::emit_consumer_body]
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
        //
        // No `#[inline(never)]` — see the accessor emission site above
        // for the full rationale (F1 investigation 2026-06-20). Both
        // historical reasons are obsolete: patch 5 + the __lang_stubs
        // share_generics heuristic close B14 (replacing reason 1), and
        // arch §F.1 says `#[inline(never)]` was never the correct fix
        // for the LTO inlining race (reason 2). The v2-precompiled-rlib
        // concern (B12) is gated by build.rs's SKY_TOOLCHAIN_ACTIVE
        // check in v1; restore the attribute (gated on a mode flag) when
        // v2 work begins.
        let fn_generics = fn_generics_clause(&toy_fn.type_params);
        // Phase C/D (Decision 3): tag exported wrapper fns as Category B —
        // Sky's body comes from `fill_extra_modules`; partition filter
        // removes the `unreachable!()` placeholder. See the accessor
        // emission site above for the full discipline + cross-crate
        // encoding rationale.
        let wrapper: syn::Item = parse_quote! {
            #[toylang::emit_consumer_body]
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

        // Impl-block generics: `impl<T: Clone> Trait for Self<T>`. Falls out
        // for non-generic impls as empty TokenStreams (the degenerate-case
        // shape of the general path). Bounds are merged in per param.
        let impl_generics: TokenStream = if toy_impl.type_params.is_empty() { // arch-fence-allow: degenerate-case-fast-path (empty token stream — Rust syntax forbids `impl<> ...`).
            quote! {}
        } else {
            let entries: Vec<TokenStream> = toy_impl.type_params.iter().map(|p| {
                let pident = format_ident!("{}", p);
                let bounds: Vec<TokenStream> = toy_impl.type_param_bounds.iter()
                    .filter(|(name, _)| name == p)
                    .map(|(_, bound)| {
                        let bident = format_ident!("{}", bound);
                        quote! { #bident }
                    }).collect();
                if bounds.is_empty() {
                    quote! { #pident }
                } else {
                    quote! { #pident: #(#bounds)+* }
                }
            }).collect();
            quote! { <#(#entries),*> }
        };
        let self_ty_generics: TokenStream = if toy_impl.self_type_args.is_empty() { // arch-fence-allow: degenerate-case-fast-path (empty token stream — Rust syntax forbids `Foo<>` at use sites).
            quote! {}
        } else {
            let args: Vec<TokenStream> = toy_impl.self_type_args.iter().map(|a| {
                let aident = format_ident!("{}", a);
                quote! { #aident }
            }).collect();
            quote! { <#(#args),*> }
        };

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
            // No `#[inline(never)]` — see the accessor emission site
            // for the full rationale (F1 investigation 2026-06-20).
            //
            // Phase A: `is_self_mut` flips the receiver token from `&self`
            // to `&mut self`. Today only `Drop::drop` needs this; the body
            // is still `unreachable!()` (the override / per_instance_mir
            // path supplies the real semantics).
            //
            // Phase C/D (Decision 3): tag trait-impl methods on consumer
            // types as Category B. Sky's `fill_extra_modules` emits the
            // real body (case4's Clone pattern; case6's cross-Sky-crate
            // trait impl; the §15.7 `<Widget as Drop>::drop` path). The
            // partition filter's two-gate predicate removes the
            // `unreachable!()` placeholder from rustc's CGU list before
            // LLVM codegen.
            if m.is_self_mut {
                parse_quote! {
                    #[toylang::emit_consumer_body]
                    fn #m_name(&mut self, #(#user_params),*) -> #ret {
                        unreachable!()
                    }
                }
            } else {
                parse_quote! {
                    #[toylang::emit_consumer_body]
                    fn #m_name(&self, #(#user_params),*) -> #ret {
                        unreachable!()
                    }
                }
            }
        }).collect();
        let impl_block: syn::ItemImpl = parse_quote! {
            impl #impl_generics #trait_ident for #self_ident #self_ty_generics {
                #(#method_items)*
            }
        };
        items.push(syn::Item::Impl(impl_block));
    }

    // Phase E.d — re-export `Drop` from the stub rlib so the
    // compiler-synthesized scope-end drop calls can resolve the
    // trait DefId. Skipped when the user's toylang source already has
    // a `use ...::Drop` import (`registry.imports` contains a path
    // ending in `::Drop`) — re-exporting twice is a name collision.
    //
    // The synthesized scope-end calls are `Drop::drop(&local)`; the
    // dispatch path in `lower_typed_expr` and the dep-walker both
    // route through `find_use_imported_trait_def_id("Drop")`, which
    // walks this stub rlib's `pub use` re-exports.
    let user_already_imports_drop = registry.imports.iter().any(|p| {
        p.split("::").last() == Some("Drop")
    });
    if !user_already_imports_drop {
        items.push(parse_quote! {
            #[allow(unused_imports)]
            pub use core::ops::Drop;
        });
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

    /// Phase E Path 2 — every stub rlib declares the universal opaque wrapper
    /// (architecture §10.6). The wrapper itself has zero source fields and is
    /// referenced by every Sky struct's representation under Phase 3+ as the
    /// `(__ToylangOpaque<HASH>, …)` newtype carrier. If stub_gen stops
    /// emitting it, downstream Sky struct emission can't reference it and the
    /// stub compile fails with E0412. Mirror of `marker_emitted_at_crate_root`.
    #[test]
    fn toylang_opaque_wrapper_emitted_at_crate_root() {
        let reg = ToylangRegistry::default();
        let src = generate(&reg);
        assert!(
            src.contains("pub struct __ToylangOpaque"),
            "stub_gen output missing __ToylangOpaque wrapper:\n{}",
            src
        );
        // The wrapper has a const u64 generic parameter — verify the shape
        // hasn't drifted (e.g. someone replaced it with a type param `<T>`).
        assert!(
            src.contains("const T : u64") || src.contains("const T: u64"),
            "wrapper signature drifted from `<const T: u64>`:\n{}",
            src,
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
            type_params: vec![],
            type_param_bounds: vec![],
            self_type_args: vec![],
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
                is_self_mut: false,
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

    /// Phase C/D (handoff Decision 3) — the 1:1 invariant fence.
    ///
    /// Every Category B item (exported toylang fn / accessor method /
    /// trait-impl method on a consumer type) must carry the
    /// `#[toylang::emit_consumer_body]` tool attribute. The facade's
    /// `is_consumer_codegen_target` predicate is now a two-gate
    /// conjunction
    ///   `is_from_lang_stubs(tcx, def_id)
    ///     && tcx.has_attrs_with_path(def_id, &[toylang, emit_consumer_body])`,
    /// so any Category B emission missing the tag would let rustc's
    /// `unreachable!()` body reach LLVM (competing with Sky's real body)
    /// → runtime panic or non-deterministic LTO IR-linker tiebreak.
    ///
    /// Equally important: items that AREN'T Category B (the marker
    /// const, the `__ToylangOpaque` wrapper, `pub use` re-exports, the
    /// Phase-6 `__toylang_*_unwrap` helpers, `extern "C"` decls) must
    /// NOT carry the tag, or the partition filter would remove them
    /// from rustc's CGU list and the binary would fail to link
    /// (rustc emits no body; Sky emits no body; symbol undefined).
    ///
    /// This test exercises both directions on a representative
    /// registry covering all three Category B shapes.
    #[test]
    fn emit_consumer_body_tags_only_category_b_items() {
        use crate::toylang::registry::{
            ToyField, ToyFunction, ToyImpl, ToyImplMethod, ToyParam, ToyStruct,
        };
        use crate::toylang::typed_ast::ResolvedType;

        let mut reg = ToylangRegistry::default();
        // Struct with a field → triggers accessor method emission.
        reg.structs.insert("Widget".to_string(), ToyStruct {
            type_params: vec![],
            fields: vec![ToyField { name: "id".to_string(), rust_type: ResolvedType::I32 }],
        });
        // Exported toylang fn → triggers wrapper fn emission.
        reg.functions.insert("make_widget".to_string(), ToyFunction {
            type_params: vec![],
            params: vec![ToyParam { name: "id".to_string(), ty: ResolvedType::I32 }],
            return_ty: Some(ResolvedType::StructRef {
                name: "Widget".to_string(), type_args: vec![],
            }),
            body: Some(crate::toylang::ast::Block { stmts: vec![], ret: None }),
            is_export: true,
        });
        // Body-less fn → triggers extern "C" decl (Category A, NOT tagged).
        reg.functions.insert("println_i32".to_string(), ToyFunction {
            type_params: vec![],
            params: vec![ToyParam { name: "x".to_string(), ty: ResolvedType::I32 }],
            return_ty: None,
            body: None,
            is_export: false,
        });
        // Trait impl → triggers trait-impl-method emission.
        reg.imports.push("std::ops::Drop".to_string());
        reg.trait_impls.push(ToyImpl {
            trait_name: "Drop".to_string(),
            self_type_name: "Widget".to_string(),
            is_export: true,
            type_params: vec![],
            type_param_bounds: vec![],
            self_type_args: vec![],
            methods: vec![ToyImplMethod {
                name: "drop".to_string(),
                is_self_mut: true,
                func: ToyFunction {
                    type_params: vec![],
                    is_export: true,
                    params: vec![ToyParam {
                        name: "self".to_string(),
                        ty: ResolvedType::Ref { inner: Box::new(
                            ResolvedType::StructRef {
                                name: "Widget".to_string(), type_args: vec![],
                            },
                        ) },
                    }],
                    return_ty: None,
                    body: None,
                },
            }],
        });

        let src = generate(&reg);

        // Category B: every fn whose body is `unreachable!()` must carry the
        // tag. Walk the source; for each `fn ... { unreachable!()`, the
        // preceding ~4 lines must include `#[toylang::emit_consumer_body]`.
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("unreachable!()") {
                // Scan up to 6 lines back looking for the tag.
                let window_start = i.saturating_sub(6);
                let window: String = lines[window_start..i].join("\n");
                assert!(
                    window.contains("#[toylang::emit_consumer_body]"),
                    "Category B 1:1 invariant violation: `unreachable!()` body \
                     at line {} is missing the `#[toylang::emit_consumer_body]` \
                     tag in the preceding 6 lines. Without the tag, the \
                     partition filter won't remove this stub from rustc's CGU \
                     list, and rustc's emitted body will compete with Sky's \
                     `fill_extra_modules` body at link time.\n\
                     Window:\n{}\n\
                     Full source:\n{}",
                    i + 1, window, src,
                );
            }
        }

        // Category A inverse: the tag must NOT appear on the marker const,
        // the __ToylangOpaque wrapper, the Phase-6 unwrap helpers, the
        // `pub use` re-export, or the `extern "C"` block. Asserting line-
        // by-line: every line carrying the tag must be followed (within ~5
        // lines) by `unreachable!()`. If the tag attached to a
        // Category A item, the partition filter would silently delete it,
        // breaking the build.
        for (i, line) in lines.iter().enumerate() {
            if line.contains("#[toylang::emit_consumer_body]") {
                let window_end = (i + 8).min(lines.len());
                let window: String = lines[i..window_end].join("\n");
                assert!(
                    window.contains("unreachable!()"),
                    "Category A 1:1 invariant violation: \
                     `#[toylang::emit_consumer_body]` at line {} is NOT \
                     followed by `unreachable!()` within 8 lines, suggesting \
                     it's attached to a real-bodied (Category A) item. The \
                     partition filter would remove this item from rustc's CGU \
                     list, but no Sky emission replaces it → undefined symbol \
                     at link time.\n\
                     Window:\n{}\n\
                     Full source:\n{}",
                    i + 1, window, src,
                );
            }
        }

        // Spot-checks: the three known Category B items must each be tagged
        // exactly once. If stub_gen drifts in a way that produces zero or
        // two tags for the same item, surface it loudly.
        let tag_count = src.matches("#[toylang::emit_consumer_body]").count();
        assert_eq!(
            tag_count, 3,
            "expected exactly 3 `#[toylang::emit_consumer_body]` tags \
             (1 accessor + 1 wrapper fn + 1 trait-impl method), got {}.\n\
             Full source:\n{}",
            tag_count, src,
        );

        // And the crate-level `#![register_tool(toylang)]` must NOT live in
        // stub_gen's output — it's added by build.rs before stubs_with_features
        // is written. If a future change moves it INTO stub_gen, this assert
        // will catch the duplication.
        assert!(
            !src.contains("#![register_tool(toylang)]"),
            "stub_gen.generate() emitted `#![register_tool(toylang)]` — \
             that attribute belongs at the build.rs prepend layer (which is \
             where it currently lives). Two crate-level register_tool attrs \
             is a duplicate-attribute error.",
        );
    }
}
