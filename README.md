# erw — Embed Rust Well

A framework for integrating custom programming languages with Rust's compiler as a foundation.
Your language's types and functions compile alongside Rust code, with full access to Rust's
type system, generics, monomorphization, and standard library.

## Workspace

| Crate | Description |
|-------|-------------|
| [`rustc-lang-facade`](rustc-lang-facade/) | Reusable library. Implements rustc query overrides, stub injection, codegen backend wrapping. Consumers implement the `LangCallbacks` trait and call `run_compiler()`. |
| [`toylangc`](toylangc/) | Example consumer. A toy language ("Toylang") with structs, functions, Vec operations, and generic types, compiled through the facade. |

## Quick start

```bash
# Build both crates
cargo build

# Run the host test (Vec<Point> creation and length check)
DYLD_LIBRARY_PATH=$(rustup run nightly-2025-01-15 rustc --print=sysroot)/lib \
  ./target/debug/toylangc --edition 2021 \
  --toylang-input toylangc/tests/point.toylang \
  toylangc/tests/host_test.rs -o /tmp/host_test && /tmp/host_test
```

## How it works

1. Your language's frontend parses source files and produces type/function definitions
2. `rustc-lang-facade` injects these as Rust stubs (struct defs + extern declarations) into rustc
3. Rustc's query system is intercepted — layout, MIR, borrow checking, and drop glue are
   provided by your `LangCallbacks` implementation
4. Your function bodies are compiled by your own backend (e.g. LLVM IR) and linked in

See [`rustc-lang-facade/README.md`](rustc-lang-facade/README.md) for the library API.

## Documentation

| Document | Description |
|----------|-------------|
| [`rustc-lang-facade/README.md`](rustc-lang-facade/README.md) | Library API, compilation lifecycle, debugging, nightly management |
| [`docs/rust-interop-architecture-guide.md`](docs/rust-interop-architecture-guide.md) | Full architecture for production language integration |
| [`docs/implemented.md`](docs/implemented.md) | Detailed description of what's implemented |

## Requirements

- Rust nightly (pinned in `rust-toolchain.toml`)
- `rustc-dev`, `rust-src`, `llvm-tools-preview` components
