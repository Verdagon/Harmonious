# rustc-lang-facade

A library for integrating custom languages with rustc as a foundation compiler. Your language's types and functions compile alongside Rust code, with full access to Rust's type system, generics, and standard library.

## How it works

Your language compiles through rustc by:

1. **Stub injection** — your types and functions are declared as Rust stubs (struct definitions + extern declarations), injected into rustc via a custom FileLoader
2. **Query overrides** — rustc's internal queries (`layout_of`, `mir_built`, `borrowck`, `drop_glue`) are intercepted for your language's items, delegating to your callbacks
3. **External codegen** — your function bodies are compiled by your own backend (e.g. LLVM IR), producing a `.o` file that's injected into rustc's link step

## Usage

Implement the `LangCallbacks` trait and call `run_compiler`:

```rust
use rustc_lang_facade::{LangCallbacks, MonomorphizeTypeResult, MonomorphizeFnResult};

struct MyLang { /* your compiler state */ }

impl LangCallbacks for MyLang {
    // Names of your types and functions (for query override detection)
    fn type_names(&self) -> HashSet<String> { ... }
    fn fn_names(&self) -> HashSet<String> { ... }

    // Generate Rust stub source code (struct defs + extern declarations)
    fn generate_stubs(&self) -> String { ... }

    // Called after rustc's analysis phase — type-check your language
    // against Rust types using the full TyCtxt
    fn after_rust_analysis<'tcx>(&self, tcx: TyCtxt<'tcx>) { ... }

    // Return field types for a concrete type instantiation (for layout computation)
    fn monomorphize_type<'tcx>(&self, name: &str, tcx: TyCtxt<'tcx>, ty: Ty<'tcx>)
        -> MonomorphizeTypeResult<'tcx> { ... }

    // Monomorphize a function — return extern symbol name + Rust generic deps
    fn monomorphize_fn<'tcx>(&self, name: &str, tcx: TyCtxt<'tcx>, def_id: LocalDefId)
        -> MonomorphizeFnResult<'tcx> { ... }

    // Compile function bodies to a .o file (has full tcx for symbol resolution)
    fn generate_and_compile<'tcx>(&self, tcx: TyCtxt<'tcx>)
        -> Option<(PathBuf, Vec<String>)> { ... }
}

fn main() {
    let my_lang = MyLang::new("input.mylang");
    rustc_lang_facade::driver::run_compiler(my_lang, &rustc_args);
}
```

## Compilation lifecycle

```
Phase 1 — Before rustc
  Your frontend parses source files, produces an AST/IR.

Phase 1.5 — after_rust_analysis(tcx)
  Rustc has parsed all Rust crates. You can query tcx for Rust type info:
  what methods does Vec have, what's the signature of HashMap::insert, etc.
  Type-check your language against Rust types here.

Phase 2 — During rustc's monomorphization (on demand)
  monomorphize_type: rustc needs the layout of your type with concrete args.
    Return the field types (as rustc Ty values).
  monomorphize_fn: rustc needs MIR for your function with concrete args.
    Return the extern symbol name + any Rust generic deps to monomorphize.

Phase 3 — generate_and_compile(tcx)
  Compile all your function bodies (e.g. to LLVM IR → .o file).
  Use tcx for mangled Rust symbol names and ABI coercion queries.
```

## What the library handles

- **layout_of override** — computes struct layout from field types you provide
- **mir_built override** — builds MIR call stubs targeting your extern symbols, with phantom casts to trigger Rust generic monomorphization
- **borrowck override** — skips borrow checking for your items (your type checker handles safety)
- **drop_glue override** — generates drop shims that call your destructors (`__yourlang_drop_TypeName`)
- **FileLoader** — injects your generated Rust stubs as a virtual file
- **CodegenBackend wrapper** — injects your compiled `.o` into the link step

## What you handle

- Parsing your language's source files
- Type checking (using `tcx` in `after_rust_analysis`)
- Generating Rust stub source code (struct defs + extern declarations)
- Monomorphizing your generic types and functions
- Compiling function bodies to native code (e.g. via LLVM)

## The five rustc mechanisms

The library intercepts five rustc query/hook points:

### 1. `layout_of` — struct layout computation

When rustc needs the size/alignment of your type (e.g. to compute `Vec<YourType>`'s layout), it calls `layout_of`. The library intercepts this and calls your `monomorphize_type`, which returns field types as rustc `Ty` values. The library then computes field offsets, padding, and total size.

**Gotcha:** `layout_of` is called for *every* type rustc encounters — including `*mut YourType`, `&YourType`, `Option<YourType>`, etc. The library filters to only intercept `TyKind::Adt` types matching your registered names. If you match too broadly, you'll corrupt derived type layouts and get ICEs in codegen.

### 2. `mir_built` — MIR body construction

When rustc needs the MIR for your function, the library calls your `monomorphize_fn` to get the extern symbol name and Rust generic deps. It then builds a MIR call stub that:
- Calls your extern symbol (the externally-compiled function body)
- Includes phantom `ReifyFnPointer` casts for each Rust dep, which triggers rustc's monomorphizer to stamp out those generic instantiations

### 3. `mir_borrowck` — selective borrow check skip

Hand-built MIR won't pass rustc's borrow checker. The library skips borrow checking for your items (identified by name). Your language's type checker is responsible for safety.

### 4. `mir_shims` (drop glue)

When rustc generates `drop_in_place::<YourType>()`, the library intercepts it and builds a MIR body that calls `__yourlang_drop_TypeName(ptr)`. You provide this function in your compiled `.o` or a small runtime library.

### 5. Type oracle (consumer-side)

To resolve Rust generic APIs (e.g. "what's the DefId of `Vec::push`?"), query `tcx` directly. The library provides `tcx` in `after_rust_analysis`, `monomorphize_fn`, and `generate_and_compile`. Common patterns:

```rust
// Find Vec's DefId
let vec_did = tcx.get_diagnostic_item(rustc_span::sym::Vec).unwrap();

// Find Vec::push
for &impl_id in tcx.inherent_impls(vec_did) {
    for &item_id in tcx.associated_item_def_ids(impl_id) {
        if tcx.item_name(item_id).as_str() == "push" { /* found it */ }
    }
}

// Get the monomorphized signature of Vec<Point>::push
let args = tcx.mk_args(&[GenericArg::from(point_ty)]);
let sig = tcx.fn_sig(push_did).instantiate(tcx, args).skip_binder();
```

## Debugging

### Useful environment variables

```bash
# Dump MIR for all functions (see your injected MIR bodies)
RUSTFLAGS="-Zdump-mir=all" ./target/debug/yourdriver ...

# Enable MIR validation (catches structural errors in injected bodies)
RUSTFLAGS="-Zvalidate-mir" ./target/debug/yourdriver ...

# Pretty-print the HIR (see how rustc parsed your stubs)
RUSTFLAGS="-Zunpretty=hir-tree" ./target/debug/yourdriver ...

# Trace query executions (verbose — filter to what you need)
RUSTC_LOG=rustc_query_system=debug ./target/debug/yourdriver ... 2>&1 | grep layout_of
```

### Common MIR validation errors

| Error | Cause |
|-------|-------|
| `StorageLive(_N) not found` | Missing `StorageLive` before first use of local N |
| `return type mismatch` | `Local(0)` type doesn't match `tcx.fn_sig(def_id).output()` |
| `use of uninitialized local` | A local is read before being assigned |
| `terminator missing` | A `BasicBlockData` has `terminator: None` |

### MIR body requirements

Every MIR body the library constructs (via `mir_helpers`) follows these rules:
- `Local(0)` is the return place — its type must match the function's return type
- `Local(1)` through `Local(arg_count)` are parameters
- Every non-argument local needs `StorageLive` before first use, `StorageDead` after last use
- Every basic block must have a terminator (`Return`, `Goto`, `Call`, etc.)
- Use `SourceInfo::outermost(span)` for spans — `DUMMY_SP` can trigger ICEs

## Nightly management

### Pin strictly

Use `rust-toolchain.toml` with an exact nightly date. `channel = "nightly"` without a date will break on the next rustc update.

### What typically changes between nightlies

| Component | Change frequency |
|-----------|-----------------|
| `Callbacks` trait | Rarely |
| `override_queries` mechanism | Rarely |
| `TyCtxt` query names | Rarely |
| `Body`, `BasicBlock`, `Statement` | Occasionally |
| `LayoutData` constructor | Frequently |
| `TerminatorKind` variant fields | Occasionally |
| `BorrowCheckResult` fields | Occasionally |

### Update process

1. Change the date in `rust-toolchain.toml`
2. `cargo build` — fix compilation errors
3. Run full test suite
4. Commit as a standalone commit for easy bisection

### Stable MIR

The `rustc_public` / Stable MIR project (https://github.com/rust-lang/project-stable-mir) aims to stabilize the APIs this library uses. When it ships, the nightly requirement goes away.

## Requirements

- Rust nightly (uses `#![feature(rustc_private)]`)
- Pinned nightly version in `rust-toolchain.toml`
- Components: `rustc-dev`, `rust-src`, `llvm-tools-preview`

## Example

See `toylangc/` in this workspace for a complete consumer implementation.
