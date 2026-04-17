# Extra Type Args Are Silently Truncated (ETASTZ)

`oracle::build_generic_args_for_item` silently discards any
user-supplied type args that extend beyond the item's own `Type`
slots in `generics_of(def_id)`. This is intentional: toylang's
call-site syntax names the **type's** generics, but the helper
walks the **method's** generics, which may be narrower when the
impl block fixes some type-level parameters.

## The convention

When toylang source writes `Vec::new<I32, Global>()`, the user is
naming Vec-the-type's generics `[T, A]` — two arguments. But
`Vec::new` itself lives on `impl<T> Vec<T, Global>`, so the
method's `generics_of` has only `[T]` (A is baked into the impl,
not a parameter of the method). The helper fills `T = I32`,
then runs out of slots. The trailing `Global` is silently dropped
and rustc picks `A = Global` from the impl block — consistent
with what the user wrote, so the result is correct.

The same shape applies to every inherent method whose impl block
fixes a defaulted type-level parameter:

| Toylang call site | Type's generics | Method's generics | Truncated |
|---|---|---|---|
| `Vec::new<T, A>()` | `[T, A]` | `[T]` | `A` |
| `Vec::with_capacity<T, A>(n)` | `[T, A]` | `[T]` | `A` |
| `IndexMap::new<K, V, S>()` | `[K, V, S]` | `[K, V, S]` | nothing |
| `HashMap::new<K, V, S>()` | `[K, V, S]` | `[K, V]` | `S` |

## Where

- `toylangc/src/oracle.rs:~510` — the truncation lives in
  `build_generic_args_for_item`, in the trailing comment after
  the `for_item` call that explains the behavior. This is the
  single anchor for @ETASTZ; every caller reaches the same helper
  via @ELASZ (the sibling arcana on the helper's synthesis-side
  behavior), so @ELASZ markers at the 10 call sites double as
  @ETASTZ cross-references.

## Latent soundness hole

Silent truncation is load-bearing for every Vec-based test in the
corpus — the toylang convention names type-level defaulted params,
and rustc's defaults line up with what the user wrote. But if
toylang ever gains a way to name a **non-default** parent-type arg
(a custom allocator for `Vec`, a custom hasher for `HashMap`, a
non-default `S` for `IndexMap` the user wants to override), the
silent truncation becomes a real bug: `Vec::new<I32, MyAllocator>()`
would silently become `Vec<I32, Global>` with no error — the user
asked for MyAllocator, got Global instead.

Tracked as known tech debt. The fix, when needed: query
`generics_of(parent_def_id).params[idx].default_value(tcx)` for
each truncated slot, compare against the user-supplied type, and
either accept (matches the default) or error out (user wanted
something else — but the impl block doesn't support it, so the
whole call site is invalid).

No test exercises this today because toylang has no syntax for
naming a non-default allocator/hasher; all generic type args that
could collide are defaults.

## Why it exists

The call-site convention ("name the type's generics") is simpler
for toylang users than the alternative ("name the method's
generics"), because:

- The type's generics are visible in `use Vec`, `use HashMap`
  statements.
- The method's generics depend on which `impl` block the method
  lives on — an implementation detail of the Rust crate. Users
  shouldn't have to know that `Vec::new` lives on a narrower impl
  than `Vec::with_capacity` (they both do, but users shouldn't
  care).
- For the common case (all defaulted parent-type args), the
  convention "just works" because rustc's impl-block defaults
  match the user's intent.

The truncation happens where user intent meets the method's
narrower slot list. Silent is correct in the common case; the
latent hole is paid as tech debt.

## Related

- @ELASZ — sibling concern on the same helper: lifetime slots
  under-populated by user types are synthesized as `re_erased`.
  This arcana covers the opposite end: extras over-populated by
  the user are truncated.
- @TVIMDGAZ — governs the Self-arg placement for trait method
  calls; extras beyond the method's generics still come after
  the Self arg.
- Tech debt in `docs/architecture/known-tech-debt.md` (item #28,
  open): the latent soundness hole if toylang gains non-default
  parent-type arg naming.

## See also

- [Early-bound Lifetime Args Synthesized (@ELASZ)](./EarlyBoundLifetimeArgsSynthesized-ELASZ.md)
- [Trait vs Impl Method DefId Generic Args (@TVIMDGAZ)](./TraitVsImplMethodDefIdGenericArgs-TVIMDGAZ.md)
