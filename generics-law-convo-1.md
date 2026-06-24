# Generics-law conversation 1

Timestamp: Mon 2026-06-15 05:34:04 UTC

---

## 👤 User

and generic functions and non-generic functions work exactly the same way right?

---

## 🤖 Claude

Honestly, **no** — there are several real branches on `type_params.is_empty()` that haven't been unified. Let me give the honest map:

## Branches that ARE gated on genericity today

**1. Registry-driven discovery (`populate_toylang_instances_from_cgus`)** — `callbacks_impl.rs:313`:
```rust
if !toy_fn.type_params.is_empty() { continue; }
```
The registry walk skips generic fns entirely. The comment is explicit: "emitting them from a registry-only walk would require concrete type args to substitute, but those come from caller-site Instances which the user-bin CGU list doesn't surface for extern non-generic items either."

So generic toylang fns get discovered by a *different* path: rustc's CGU walk at user-bin time (Workstream A's `cgu_stash`-fed loop at `llvm_gen.rs:1938`), which sees `identity::<i32>` queued because rust_caller named the Instance.

**Same branch repeated for trait-impl methods** (Session 8 work, callbacks_impl.rs):
```rust
if method.func.body.is_none() || !method.func.type_params.is_empty() { continue; }
```

**2. Check 5 typecheck** — `callbacks_impl.rs:488`:
```rust
if func.body.is_none() || !func.type_params.is_empty() { continue; }
```
Generic fn bodies aren't typechecked eagerly because resolving `T.field` or `Clone::clone::<T>(...)` needs a concrete T. They're typechecked at the substituted-pass time, called from `codegen_internal_function` which runs `resolve_fn_body` per Instance.

**3. stub_gen extern decls** — `stub_gen.rs:198`:
```rust
if toy_fn.type_params.is_empty() {
    extern_fns.push(quote! { pub fn #fn_ident(#(#extern_params),*) -> #ret; });
}
```
Only non-generics get the `extern "C" { pub fn ... }` declaration. Documented (#17): "Rust's `extern "C"` doesn't permit generic items" — a hard syntactic constraint that Sky retires only when #4's inline-codegen rewrite lands.

**4. stub_gen struct shape** — `stub_gen.rs:147`:
```rust
let item: syn::ItemStruct = if !is_generic {
    parse_quote! { pub struct #ident; }
} else {
    parse_quote! { pub struct #ident<#(#type_params),*>(std::marker::PhantomData<...>); }
};
```
Documented: forced by a rustc debuginfo-walker ICE on opaque non-generic ADTs with any source-level field.

## Branches that have been unified

- **stub_gen impl-block header** + **wrapper-fn header** (Session 7's #17 work) — `generics_for_impl_block` + `fn_generics_clause` helpers return empty token streams for zero params, so the single template handles both.
- **Substitution mechanism** (Approach A): `resolve_caller_from_instance` has a fast-path return for empty type_params but is otherwise identical — `instance.args` is just empty for non-generics.
- **Body codegen** once the body is resolved: `codegen_internal_function` doesn't care about genericity; it operates on a resolved body with concrete types in the registry's substituted form.
- **Symbol mangling**: generics get type-arg suffixes (`__toylang_internal_wrap__i32`); non-generics get no suffix. Same code path, the suffix loop just iterates zero items.

## Honest assessment

The CLAUDE.md compiler law — "non-generic is the degenerate case of generic; never branch on type-params" — is **violated** in the four sites above. Two are gated by genuine external constraints (Rust syntax for `extern "C"`, rustc debuginfo ICE). Two are implementation pragmas (the discovery split and the eager-typecheck skip) that could in principle be unified but aren't.

The mechanism-level claim is still true: **Approach A substitution + body resolution + codegen are uniform**. The pragma-level reality: **discovery, typecheck timing, and stub-rlib emission shape all have a non-generic-vs-generic split**.

Some of this should bother us:

- The **discovery split** (populate skips generics; rustc's CGU walk catches them via rust_caller) means a non-generic-with-no-Rust-caller works but a generic-with-no-Rust-caller would silently drop. That's brittle and depends on caller-site naming.
- The **typecheck timing split** means generic fns get no Sky-side type error until codegen — error messages surface later and from a different code path.

The structural splits (extern decls, struct shape) are forced — those wait on #4's deeper rewrite.

Want me to fence the broken non-uniformity (discovery + typecheck) under a new architectural-property test the way Session 10 did for export? That'd at least pin the asymmetry so future drift makes it more uniform, not less.
