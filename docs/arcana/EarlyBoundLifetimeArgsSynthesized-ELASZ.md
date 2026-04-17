# Early-bound Lifetime Args Are Synthesized (ELASZ)

When toylang builds a `GenericArgs` for any Rust item, every slot
the item's `generics_of` declares must be populated — including
lifetime slots. User toylang code only supplies type args
(`from_str<Value>("null")`, `Vec::new<I32, Global>()`). Lifetime
slots, when present (early-bound lifetimes — those that appear in a
`where` bound on the item), are synthesized as `tcx.lifetimes.re_erased`
at monomorphization time.

The single helper `oracle::build_generic_args_for_item` uses
`ty::GenericArgs::for_item(tcx, def_id, |param, _| ...)` to let
rustc drive the per-param walk: user-supplied types fill `Type` slots
in declaration order, lifetime slots get `re_erased`, const slots
panic (not yet supported). Extras beyond the item's `Type` slots
are silently truncated — toylang's convention names type-level
defaulted params at the call site (e.g., `Vec::new<I32, Global>()`
names A=Global even though `Vec::new`'s own generics are just `[T]`;
A lives on the parent `Vec` type). This matches the pre-@ELASZ
truncate behaviour.

## Where

- `toylangc/src/oracle.rs:~472` — `build_generic_args_for_item`
  helper. Public, used by every site that builds a `GenericArgs`
  from user type args.
- `toylangc/src/oracle.rs:~361` — `redirect_to_wrapper`
  (Phase 6 unwrap wrapper dispatch).
- `toylangc/src/oracle.rs:~393` — `rust_method_return_type`.
- `toylangc/src/oracle.rs:~499` — `try_instantiate_free_fn_sig`
  (`rust_free_fn_return_type` and `rust_free_fn_param_types` both
  go through this helper).
- `toylangc/src/oracle.rs:~533` — `rust_method_param_types`.
- `toylangc/src/oracle.rs:~571` — `rust_trait_method_param_types`
  (args are `[Self, ...user_types]` per @TVIMDGAZ).
- `toylangc/src/oracle.rs:~665` — `rust_trait_method_return_type`.
- `toylangc/src/toylang/callbacks_impl.rs` — free-fn dep
  registration during `collect_rust_deps_recursive`.
- `toylangc/src/toylang/callbacks_impl.rs` — trait-method dep
  registration in `collect_rust_deps_recursive` (args are
  `[Self, ...]` per @TVIMDGAZ).
- `toylangc/src/toylang/callbacks_impl.rs` — inherent-method
  dep registration in `collect_rust_deps_recursive`.
- `toylangc/src/llvm_gen.rs:~328` — trait static call codegen path.
- `toylangc/src/llvm_gen.rs:~340` — trait-fallback inherent static
  call codegen path.
- `toylangc/src/llvm_gen.rs:~362` — inherent method call codegen path.
- `toylangc/src/llvm_gen.rs:~1170` — FnCall use-imported / extern
  free-fn codegen path.

All ten call sites feed their resolved `Vec<GenericArg>` into the
same helper. The helper handles parent generics transparently
because `for_item` walks parent params first, then the item's own
params.

## Why `re_erased` and not `'static`

`re_erased` is rustc's post-borrowck placeholder — the region it
uses itself during the monomorphization pass for elided lifetimes.
It's semantically correct for our phase: borrow-checking already
happened during stub typecheck of `__lang_stubs`, so lifetimes
carry no remaining meaning at codegen time. `'static` would
type-check but fail any trait impl that discriminates on lifetime
(`impl Deserialize<'static>` is strictly narrower than
`Deserialize<'de>` for all `'de`).

## Why this was latent

Every Rust free fn / method / trait method toylang had called
before 2026-04-16 used either no lifetime parameters or only
**late-bound** ones. Late-bound lifetimes live inside `Binder<FnSig>`
(they appear only in argument/return types, never in `where`
bounds), not in `generics_of`, so the hand-rolled truncate pattern
coincidentally produced correct args for them — the lifetime only
shows up after `instantiate`, when the binder is opened and the
late-bound region is folded through the args that already match
`generics_of`'s shape.

`serde_json::from_str<'a, T: Deserialize<'a>>` was the first call
site in the entire test corpus with an **early-bound** lifetime
(early-bound because `'a` appears in `where T: Deserialize<'a>`).
Toml's `from_str<T: DeserializeOwned>` dodged the issue; regex's
`Regex::new` has no generics at all; indexmap's `IndexMap::new<K,V,S>`
has three early-bound type params but no lifetime; uuid's `Uuid::new_v4`
has no generics.

This is the **third** "latent until the right crate shape surfaces
it" pattern in Phase 7 — after @UTAIRZ (`&str` ABI surfaced by the
first `&str`-accepting Rust fn) and @IVTDBTZ (trait-vs-inherent
dispatch surfaced by the first inherent static call with args).

## Synthetic `impl Trait` slots

Rust's `impl Trait` in argument position desugars to a synthetic
early-bound type parameter. rustc exposes these in `generics_of`
alongside named params (they carry `param.kind = Type { synthetic:
true, .. }`). The helper makes no distinction: all `Type` slots —
synthetic or named — consume from the user's supplied type-args
list in declaration order. Named slots come first, synthetic slots
come second, matching rustc's turbofish convention.

This uniform treatment is **load-bearing**. If the helper were
"simplified" by special-casing synthetic slots (e.g., adding an
"infer from argument type" branch), two things would break:

1. Toylang's explicit-everywhere rule becomes inconsistent — named
   slots require user args, synthetic slots silently infer. Users
   would have to learn which shape they're calling.
2. `build_generic_args_for_item`'s single-pass walk via
   `ty::GenericArgs::for_item` would need access to function
   argument types (which it currently has no need for) to do the
   synthetic inference.

The clap smoke test (`toylangc/tests/standalone/clap_test/`) is the
canonical in-tree demonstration: `Command::new<&str>("app")` where
`Command::new`'s signature is `fn new(name: impl Into<Str>) -> Self`.
The synthetic slot takes `&str` (the **argument's concrete type**,
not the bound's target `Str`), and rustc handles the `Into::into`
conversion during monomorphization via the blanket `impl<T: Into<U>>`
chain it resolves itself.

This generalizes to every `impl Trait` arg across the Rust
ecosystem — `String::from(x)`, `PathBuf::from(x)`, `Box::new(x)`,
and any builder-pattern API that accepts `impl Into<T>` /
`impl AsRef<T>` / `impl Fn(...)`. All of them fill one extra slot
in toylang's call-site turbofish with the argument's concrete type.

**Meta-lesson this surfaced:** `clap_test` was documented in
`docs/historical/quest.md` and `docs/architecture/rust-interop-guide.md`
for weeks as "blocked on `impl Into<Str>` synthetic generic —
requires compiler work." The empirical probe (write the test and run it)
took 4 seconds and passed first-try. The "blocker" was a prior
author's reasoning-to-conclusion that encoded a presumed solution
shape (inference-based infrastructure) as a problem description.
When a future doc describes something as "blocked on compiler
support," prefer writing the minimal failing test to writing the
infrastructure plan.

## Cross-cutting effect

Every call site that builds a `GenericArgs` for a Rust item now
goes through the shared helper. A future Rust API with an
early-bound lifetime — `serde_json::from_slice<'a, T: Deserialize<'a>>`,
`toml::Deserializer::new<'b>`, any `Visitor<'de>` impl — works
automatically. A future Rust type constructor with an early-bound
lifetime (e.g., `std::slice::Iter<'a, T>::new`) would also be
covered when its method is called; ADT-construction paths in
`oracle::try_resolved_to_rustc_ty` and `callbacks_impl::resolved_to_rustc_ty`
still use plain `tcx.mk_args` because no test exercises
lifetime-bearing ADT types by name yet (e.g., no toylang source
writes `Iter<'a, I32>` directly). Those will be fixed if and when
that case arises.

## Related

- @ETASTZ — sibling concern on the same helper, opposite end:
  user-supplied types that exceed the item's `Type` slots are
  silently truncated (load-bearing for Vec-style call-site naming
  of type-level defaulted params).
- @RTMEIZ — type-level equivalent: "every Rust type flowing
  through the type system must be `use`-imported". ELASZ is the
  arg-shape equivalent: "every slot the item declares must be
  filled".
- @IVTDBTZ — same "latent until the right crate shape surfaces it"
  pattern; second instance.
- @UTAIRZ — first instance of the same latency pattern, with the
  `&str` ABI surfaced by the first real call.
- @TVIMDGAZ — governs Self-arg placement for trait method calls;
  the `[Self, ...user_types]` ordering ELASZ consumes at the two
  trait-call sites.
- `docs/usage/writing-main.md` Rule 3 — user-facing companion:
  how to fill synthetic `impl Trait` slots at the call site.
