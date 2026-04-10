# erw (Embed Rust Well)

A framework for embedding custom languages into rustc's compilation pipeline via query providers.

## Background

Two-crate workspace:
- **`rustc-lang-facade`** — reusable library that hooks into rustc via 4 query providers (`layout_of`, `per_instance_mir`, `symbol_name`, `mir_shims`)
- **`toylangc`** — example consumer compiler for "toylang" that uses the facade

Forked `nightly-2025-01-15` (rustc 1.86.0-dev) with `per_instance_mir` query. Toolchain `rustc-fork`.

Consumer types appear to rustc as opaque stubs with `unreachable!()` bodies. Internal consumer functions are never exposed to rustc — they are discovered via deep monomorphization walk and compiled separately by an Inkwell LLVM backend. A global mutex serializes all consumer code (single-threaded).

`ResolvedType` is the unified type representation used everywhere (no string-based types). Explicit type args required at all call sites (no inference).

## Key docs

- [Architecture guide](docs/architecture/rust-interop-guide.md) — full compilation flow, query providers, LLVM backend, ABI handling
- [Known tech debt](docs/architecture/known-tech-debt.md) — tracked debt items
- [Documentation strategy](docs/meta.md) — how docs are organized (categories, conventions, discovery)

## Building & testing

```bash
cargo +rustc-fork test          # run all tests (95 integration + 37 unit)
cargo +rustc-fork test --test integration_tests test_name  # run one test
```
