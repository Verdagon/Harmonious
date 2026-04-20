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
# Build the compiler
cargo +nightly-2026-01-20 build -p toylangc

# Run the test suite (210 tests: 67 unit + 128 integration + 15 standalone)
cargo +nightly-2026-01-20 test -p toylangc
```

Then try a minimal toylang project:

```bash
mkdir -p /tmp/smoke && cd /tmp/smoke
cat > toylang.toml <<EOF
[project]
name = "smoke"
source = "main.toylang"
EOF
echo 'fn main() {}' > main.toylang

DYLD_LIBRARY_PATH=$(rustc +nightly-2026-01-20 --print sysroot)/lib \
  /path/to/target/debug/toylangc build
./.toylang-build/target/debug/smoke; echo "exit: $?"
```

## Building a project with `toylang.toml`

For real projects (not single-file integration tests), create a `toylang.toml`
manifest next to your `.toylang` source:

```toml
[project]
name = "my_app"
source = "main.toylang"

[rust-dependencies]
rand = "0.8"
regex = { version = "1", features = ["unicode"] }
```

Then run `toylangc build`. It generates a hidden `.toylang-build/` Cargo
project, lets cargo resolve the `[rust-dependencies]`, and produces a binary
at `.toylang-build/target/debug/<name>`.

**How `toylang.toml` is used.** The manifest plays two roles, and both are
important to understand:

1. **User-facing manifest.** `toylangc build` parses it to generate the
   internal Cargo project (`Cargo.toml`, `src/main.rs` shim,
   `rust-toolchain.toml`) and to spawn `cargo +nightly-2026-01-20 build`.
2. **Side-channel for wrapper mode.** Cargo then invokes `toylangc` itself as
   a `RUSTC_WORKSPACE_WRAPPER` for each crate. When it reaches your primary
   crate, wrapper-mode `toylangc` re-reads the same `toylang.toml` (one
   directory up from the generated `.toylang-build/`) to locate the
   `.toylang` source. This keeps the manifest as the single source of truth —
   no environment variable side-channels, no hidden state.

The trade-off is that the manifest is parsed twice per build (microseconds,
irrelevant). In exchange, everything the build system needs is visible in one
user-editable file. See
[`docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md`](docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md)
for the full cross-cutting rationale.

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
