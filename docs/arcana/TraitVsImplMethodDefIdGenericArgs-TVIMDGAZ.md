# Trait Vs Impl Method DefId Generic Args (TVIMDGAZ)

When calling a Rust trait method from toylang, you must use the **trait
definition's** method DefId with `[Self, ...]` as generic args — NOT the
impl's method DefId with the impl's type params. These are different DefIds
with different generic parameter structures, and using the wrong one causes
"type parameter out of range" ICEs.

## Where

- `toylangc/src/oracle.rs` — `rust_trait_method_return_type` queries `fn_sig`
  using the trait definition's method DefId
- `toylangc/src/llvm_gen.rs` — `get_or_resolve_rust_method` builds Instance
  using the trait definition's method DefId
- `toylangc/src/toylang/callbacks_impl.rs` — dep collection uses the trait
  definition's method DefId for reporting to rustc's monomorphization collector

## Cross-cutting effect

For an inherent method like `Vec::push`, the method DefId is from the impl
block, and the generic args are the ADT's type params: `[i32, Global]`.

For a trait method like `Clone::clone`, there are TWO DefIds:
1. **Trait definition** DefId (`core::clone::Clone::clone`) — generic params
   start with `Self`, so args are `[Vec<i32, Global>]`
2. **Impl** DefId (`alloc::vec::{impl#11}::clone`) — generic params are the
   impl's params `[T, A]`, so args are `[i32, Global]`

If you use the impl DefId with `[Vec<i32, Global>]`, rustc expects `[i32, Global]`
and panics: `type parameter A/#1 out of range when instantiating, args=[Vec<i32, Global>]`.

`Instance::expect_resolve` handles the mapping from trait-level args to
impl-level args automatically — but only when given the trait definition's
method DefId.

Additionally, the receiver type must be stripped of `&ref` wrappers before
being used as Self. `Clone::clone(&v)` has receiver type `&Vec<i32, Global>`,
but Self is `Vec<i32, Global>`. The `strip_ref()` function in oracle.rs
handles this.

## Why it exists

Rustc's trait system separates the trait definition (which defines method
signatures in terms of `Self`) from impl blocks (which substitute concrete
types for `Self`). These are different items in the HIR with different DefIds
and different generic parameter counts. The consumer must use the right DefId
for the operation: trait definition for ABI/signature queries and
`Instance::expect_resolve`, not the impl's DefId.

## See also

- `docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md` — governs the
  upstream decision of *whether* a call goes through the trait path
  (and therefore whether this arcana applies) vs. the inherent path.
