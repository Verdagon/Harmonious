# erw (Embed Rust Well)

A framework for embedding custom languages into rustc's compilation pipeline via query providers.

## Background

Two-crate workspace:
- **`rustc-lang-facade`** — reusable library that hooks into rustc via 4 query providers (`layout_of`, `optimized_mir`, `symbol_name`, `mir_shims`)
- **`toylangc`** — example consumer compiler for "toylang" that uses the facade

Built against vanilla `nightly-2025-01-15` — zero rustc fork patches. All rustc integration flows through `Config::override_queries`: `optimized_mir` supplies synthetic dep-registering bodies, `collect_and_partition_mono_items` filters consumer items out of rustc's CGU list and forces `(External, Default)` linkage on `__lang_stubs` items directly, `symbol_name` rewrites to toylang-mangled names, `layout_of` / `mir_shims` / `upstream_monomorphizations_for` round out the set. No fork, no hook statics.

Consumer types appear to rustc as opaque stubs with `unreachable!()` bodies. Internal consumer functions are never exposed to rustc — they are discovered via deep monomorphization walk and compiled separately by an Inkwell LLVM backend. A global mutex serializes all consumer code (single-threaded).

`ResolvedType` is the unified type representation used everywhere (no string-based types). Explicit type args required at all call sites (no inference).

**Compiler law: non-generic is the degenerate case of generic.** Never branch on "does this function/type have type parameters?" A non-generic item is simply one with zero type args — it goes through the same instantiation path as a generic one. Code that special-cases the non-generic path creates false distinctions and latent bugs when items gain type params or the code is reused in a more general context. Always write the general path; zero args is a valid input to it.

**Prefer treating `self` as just another parameter.** When possible, avoid separate code paths for self vs non-self parameters. If syntax separates the receiver (e.g., dot notation), reassemble the full parameter list so downstream logic can handle everything uniformly.

**`instantiate_identity()` requires a comment.** `EarlyBinder::instantiate_identity()` is a no-op unwrap — it discards the binder and returns the inner value with all `ty::Param` placeholders intact. It is only correct for structural inspection (e.g., "is this impl for a consumer type?"), never for producing a concrete type at a call site. Every call to `instantiate_identity()` must have a comment explaining why we are intentionally not substituting real values for the generic parameters.

## Key docs

- [Architecture guide](docs/architecture/rust-interop-guide.md) — full compilation flow, query providers, LLVM backend, ABI handling
- [Known tech debt](docs/architecture/known-tech-debt.md) — tracked debt items
- [Documentation strategy](docs/meta.md) — how docs are organized (categories, conventions, discovery)

## Building & testing

```bash
cargo +nightly-2025-01-15 test          # run all tests (67 unit + 127 integration_projects + 15 standalone = 209)
cargo +nightly-2025-01-15 test --test integration_tests test_name  # run one test
```
