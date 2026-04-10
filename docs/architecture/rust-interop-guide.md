# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 95 integration tests + 37 unit tests passing, 0 ignored.
> Minimal rustc fork with `per_instance_mir` query. Inkwell LLVM backend.
> Deep monomorphization walk — internal toylang functions never exposed to rustc.
> Global mutex serializes all consumer code (single-threaded execution).
> Unified `ResolvedType` everywhere. Explicit typed literals. Typed error enums.
> Full ABI coverage. Generic functions with explicit type args. Mutable assignment,
> else if, boolean operators (&&/||), unary negation. Roguelike integration test.
>
> **Next milestone:** `toylang.toml` project manifest, trait method resolution,
> Rust free function calls, byte string literals, and standalone test projects
> linking against 9 Rust crates (rand, regex, uuid, clap, serde, toml, glob,
> indexmap, reqwest) — all without glue code or derive macros.

## Overview

Two-crate workspace:
- `rustc-lang-facade` — reusable library for integrating custom languages with rustc
- `toylangc` — toylang consumer

Forked `nightly-2025-01-15` (rustc 1.86.0-dev) with 4 patches adding `per_instance_mir`.
Fork at `~/rust` on branch `per-instance-mir`. Linked as toolchain `rustc-fork`.

---

## Part 1: The Compilation Flow

```
┌──────────────────────────────────────────────────────────────────┐
│  Consumer frontend                                               │
│  1. Parse .toylang source → ToylangRegistry (structs, functions) │
│  2. Type params on structs and functions preserved (not resolved) │
│  3. Registry is read-only — no pre-marking, no mutation           │
└───────────────┬──────────────────────────────────────────────────┘
                │
                ▼
┌──────────────────────────────────────────────────────────────────┐
│  rustc session (consumer embedded as query providers)            │
│                                                                  │
│  4. FileLoader injects __lang_stubs.rs (opaque structs,          │
│     accessor methods, wrapper functions — all unreachable!())    │
│  5. rustc parses, type-checks, borrow-checks normally            │
│     (unreachable!() bodies are valid Rust — no overrides needed) │
│  6. Monomorphization begins (inside codegen_crate)               │
│     ├─ per_instance_mir fires for each ENTRY-POINT instance      │
│     │  → deep walk finds ALL transitive Rust deps                │
│     │  → internal toylang callees stashed in ToylangState        │
│     │  → only Rust deps returned to rustc's collector            │
│     ├─ layout_of fires for each consumer type instantiation      │
│     ├─ mir_shims fires for each consumer drop glue instance      │
│     └─ symbol_name maps entry-point instances to consumer symbols│
│  7. Codegen dispatch skips consumer functions (extern decl only) │
│  8. generate_and_compile fires (all instances known)             │
│     ├─ Entry-point fns from MonoItems (with Instance for ABI)    │
│     ├─ Internal fns from ToylangState.toylang_instances           │
│     ├─ Two-pass codegen: internal fns, then extern wrappers      │
│     └─ llc compiles to .o, injected into link step               │
│  9. Link: consumer .o + rustc .o → final binary                  │
└──────────────────────────────────────────────────────────────────┘
```

Key insight: internal toylang functions (those only called by other toylang
functions) are never exposed to rustc. The deep monomorphization walk in step 6
discovers them and their transitive Rust deps in a single pass. Rustc only sees
entry-point functions and Rust deps.

---

## Part 2: The Four Query Providers

Only 4 query overrides. Consumer functions in `__lang_stubs` have `unreachable!()`
bodies that pass rustc's normal pipeline. `per_instance_mir` replaces them at
monomorphization time.

### 2.1 layout_of

**Key:** `Ty<'tcx>` — fires per concrete type instantiation.

Reports 0 fields in `FieldsShape` (opaque memory blob). Size and alignment come
from the consumer's `monomorphize_type` callback, which returns concrete field types.
The library computes C-style layout from those.

Skips types with unresolved type params (`has_param` check) — these are generic
definitions, not concrete instantiations.

### 2.2 per_instance_mir (rustc fork)

**Key:** `Instance<'tcx>` — fires per concrete function instantiation.

Four-patch fork of rustc:
1. Query definition in `rustc_middle/src/queries.rs`
2. Collector check in `rustc_monomorphize/src/collector.rs` (before `instance_mir`)
3. Codegen skip in `rustc_codegen_ssa/src/mono_item.rs` (if Some, skip `codegen_instance`)
4. Default provider in `rustc_mir_transform/src/shim.rs` (returns None)

The provider calls `monomorphize_fn` which triggers the **deep monomorphization
walk**:
1. Substitutes type params with concrete args from the Instance
2. Type-resolves the body
3. Walks the typed AST for deps
4. For toylang callees: recursively walks their bodies (stashes them in
   `ToylangState.toylang_instances`)
5. For Rust deps (extern fns, Rust methods): returns them to rustc
6. The `visited_symbols` set in ToylangState prevents re-walking shared callees

Only Rust deps are returned to rustc. Internal toylang functions never appear in
rustc's MonoItems. The consumer discovers them independently during codegen.

### 2.3 symbol_name

**Key:** `Instance<'tcx>` — maps consumer instances to consumer symbol names.

Calls `monomorphize_fn` to get the extern symbol. On repeated calls (e.g., after
`per_instance_mir` already processed the function), the `visited_symbols` cache
causes an early return with zero work — no re-walking.

Symbol examples:
- Concrete: `__toylang_impl_make_counter`
- Generic: `__toylang_impl_wrap__i32`
- Accessor: `__toylang_accessor_Pair_first__i32__i32`

Type args in symbols use `_LT_` / `_GT_` delimiters for collision safety:
`Vec<i32>` → `Vec_LT_i32_GT_`.

### 2.4 mir_shims (drop glue)

**Key:** `InstanceKind::DropGlue(_, Some(ty))` — per concrete type instantiation.

Builds a MIR body calling `__toylang_drop_TypeName(ptr)`. The consumer provides
the destructor in its .o file. `set_required_consts` and `set_mentioned_items` must
be called (with empty vecs) on shim bodies — `mir_promoted` doesn't run for shims.

---

## Part 3: Opaque Stubs

### 3.1 What gets generated

`stub_gen.rs` produces `__lang_stubs.rs` containing:

**Opaque structs** with 0-field layout:
```rust
pub struct Counter(());
pub struct Pair<A, B>(std::marker::PhantomData<(A, B)>);
```

**Accessor methods** with unreachable bodies:
```rust
impl Counter {
    pub fn value(&self) -> &i32 { unreachable!() }
}
```

**Wrapper functions** for ALL consumer functions (concrete + generic):
```rust
pub fn make_counter() -> Counter { unreachable!() }
pub fn wrap<T>(x: T) -> Wrapper<T> { unreachable!() }
```

### 3.2 Module-qualified matching

All query overrides use `is_from_lang_stubs(tcx, def_id)` which checks
`tcx.def_path_str(def_id).starts_with("__lang_stubs::")`. This prevents
name collisions with user-defined types/functions sharing a name.

### 3.3 Why unreachable!() works

Consumer functions have `unreachable!()` bodies. Rustc compiles them normally
through `mir_built` and `borrowck` — no overrides needed. `per_instance_mir`
replaces the bodies at monomorphization time. Codegen skips them. The
`unreachable!()` code is never emitted in the final binary.

---

## Part 4: The LLVM Backend

### 4.1 Instance discovery

`generate_with_tcx` discovers toylang function instances from two sources:

1. **MonoItems walk** — finds entry-point functions (Rust calls them) and accessor
   methods. Entry-point functions have a rustc `Instance` (needed for extern
   wrapper ABI queries).

2. **`state.toylang_instances`** — internal functions discovered during the deep
   monomorphization walk. These have no rustc `Instance` and only get an internal
   ABI function (no extern wrapper).

Each function instance carries:
- `resolved_func: ToyFunction` — the type-substituted function definition
- `instance: Option<Instance<'tcx>>` — `Some` for entry points, `None` for internal
- `extern_symbol: String` — the mangled symbol name

### 4.2 Two-pass codegen

**Pass 1:** `codegen_internal_function` for ALL items (entry + internal).
Simple ABI: primitives direct, structs always sret.

**Pass 2:** `codegen_extern_wrapper` for entry-point items ONLY (those with
`Some(instance)`). Thin wrapper matching Rust ABI, delegates to internal function.

Accessor methods are generated inline via `codegen_accessor_inline` (GEP to field
offset, return pointer).

### 4.3 Type annotation pass

`type_resolve.rs` produces a `TypedBlock` where every expression carries a
`ResolvedType`. This runs before LLVM codegen and handles:

- **IntLit:** Type carried on the AST node from lexer (suffixes: `42i32`, `42i64`,
  `42usize`; default i32, auto-promote to i64 if value overflows i32)
- **BoolLit:** Always `ResolvedType::Bool`
- **UnaryNeg:** Desugared to `BinaryOp::Sub(zero, inner)` with zero matching
  the inner expression's type
- **Generic type params:** Substituted with concrete types from Instance args
  (including in body expressions via `substitute_type_params_in_body`)
- **BinaryOp:** Left operand's type propagates to right. Comparison/boolean ops
  (`==`, `!=`, `<`, `<=`, `>`, `>=`, `&&`, `||`) return `Bool`.
- **Assignment:** RHS type checked against variable's existing type
  (`AssignTypeMismatch` error)

**No inference machinery.** All type args must be provided explicitly at call
sites. Integer literals carry their own type from the parser.

**Error handling:** Returns `Result<T, TypeResolveError>` with 12 typed error
variants. `after_rust_analysis` validates non-generic function bodies and reports
all errors before aborting.

### 4.4 Deep dependency discovery

`collect_toylang_fn_deps_inner` runs `resolve_fn_body` on a function body, then
walks the typed AST for deps. The walk collects:

- **Toylang function calls:** `(name, type_args)` pairs. For each, if the callee
  has a body: substitute type params, recurse, stash as `ToylangInstance`.
  Do NOT report to rustc.
- **Extern function calls:** Report `(DefId, GenericArgsRef)` to rustc.
- **Rust method calls:** `find_inherent_method` for DefId + generic args → report
  to rustc.

The `visited_symbols: HashSet<String>` in `ToylangState` persists across all
`monomorphize_fn` calls. Each function body is walked exactly once, regardless of
how many entry points reach it or how many times rustc calls `monomorphize_fn`.

### 4.5 Inkwell codegen

The backend uses inkwell (LLVM C API bindings). Expression lowering handles:

- `IntLit` / `BoolLit` → inkwell const_int
- `StringLit` → `build_global_string_ptr`
- `Var` → load from alloca
- `StructLit` → alloca + GEP + store per field
- `FieldAccess` → GEP into struct + load (primitives) or return ptr (complex)
- `BinaryOp` → build_int_add/sub/mul/div, comparisons, and/or, or float equivalents
- `If` → conditional branch with phi nodes for expression-valued if/else
- `While` → header/body/exit blocks with store-back for rebound variables
- `Assign` → store into existing alloca (no new alloca)
- `FnCall` → call via `__toylang_internal_` symbol (internal ABI)
- `StaticCall` / `MethodCall` → looked up in `rust_method_info`, sret or direct,
  with null appended for `#[track_caller]` hidden param (see @TCHAPZ)

Rust types (Vec, etc.) use opaque `[N x i8]` byte arrays — size and alignment
queried from `tcx.layout_of`. Toylang structs use real LLVM struct types with
GEP-based field access.

### 4.6 Internal/extern ABI split

Each entry-point toylang function generates **two** LLVM functions:

1. **Internal** (`__toylang_internal_{name}`) — simple, predictable ABI:
   - Primitives (i32, i64, f64, bool, usize): returned directly
   - Void: void return
   - Structs/Vec: always sret (ptr first param, void return)

2. **Extern wrapper** (`__toylang_impl_{name}`) — thin wrapper matching Rust ABI:
   - Calls the internal function
   - Adapts return/params to match `fn_abi_of_instance`

Internal-only toylang functions generate only the internal function (no wrapper).
Toylang-to-toylang calls use `__toylang_internal_` symbols directly.

### 4.7 Toylang-owned main

When toylang defines `fn main()`, the stub wrapper is renamed to `__toylang_main`
to avoid conflicting with Rust's `main`. The mapping flows through:
- `is_consumer_fn` returns true for both `"main"` and `"__toylang_main"`
- `monomorphize_fn` maps `"__toylang_main"` → `"main"` for registry lookup
- `compute_fn_symbol` → extern symbol `__toylang_impl_main`

### 4.8 `use` imports

Toylang supports `use` statements: `use std::alloc::Global`. The parser stores
the path in `registry.imports`. The stub generator emits `pub use` in
`__lang_stubs.rs`. `find_rust_type_def_id` finds re-exported types via
`module_children_local`.

---

## Part 5: Global State and Threading

### 5.1 Single global, single mutex

All facade and consumer state lives in one global:

```
GLOBALS: OnceLock<Mutex<FacadeGlobals>>
```

`FacadeGlobals` contains:
- `callbacks: Box<dyn Any>` — the type-erased `ToylangCallbacks`
- `consumer_state: Box<dyn Any>` — the type-erased `ToylangState`
- `vtable: CallbackVtable` — HRTB function pointers for dispatch
- Saved default query providers (`layout_of`, `mir_shims`, `symbol_name`)
- `lang_obj_path: Option<PathBuf>` — compiled .o path for link injection

### 5.2 Locking protocol

Every `call_*` function in the facade locks the global mutex for the entire
callback invocation. Consumer state is passed as `&mut dyn Any` — the consumer
downcasts to `&mut ToylangState`.

This ensures all toylang code runs single-threaded, even when rustc's query
providers fire on Rayon worker threads. No locking needed on the consumer side.

`is_consumer_type` / `is_consumer_fn` also lock briefly (just a vtable call to
check the registry).

### 5.3 Reentrancy avoidance

`generate_and_compile` calls `generate_with_tcx` which calls
`callbacks.monomorphize_fn_inner()` — this is NOT the trait method (which would
re-lock). It's a direct method on `ToylangCallbacks` that takes `&mut ToylangState`
as a parameter, bypassing the mutex entirely.

### 5.4 Consumer state (`ToylangState`)

```rust
pub struct ToylangState {
    pub log: Vec<CallbackLog>,
    pub toylang_instances: Vec<ToylangInstance>,
    pub visited_symbols: HashSet<String>,
}
```

- `log` — structured record of every callback from rustc (for test assertions)
- `toylang_instances` — functions discovered during deep walk (for codegen)
- `visited_symbols` — deduplication across all `monomorphize_fn` calls

### 5.5 Callback log

`CallbackLog` enum records each rustc→toylang callback:
```rust
pub enum CallbackLog {
    MonomorphizeType { name: String },
    MonomorphizeFn { name: String },
    AfterRustAnalysis,
    GenerateAndCompile,
}
```

Tests can set `TOYLANG_LOG_PATH` env var to dump the log to a file, then assert
that internal functions do NOT appear in `MonomorphizeFn` entries.

---

## Part 6: What Works (95 tests)

### Struct types
- Simple, generic, nested, mixed-field, large, single-field structs
- Structs containing Rust types
- All nesting patterns: T(R), R(T), T(T(R)), R(R(T)), 4-level deep

### Functions
- Multiple parameters, struct parameters, generic functions
- Inter-toylang calls (concrete and generic, including deep call chains)
- Extern function calls (body-less `fn` declarations)
- Mutable assignment (`x = expr;`)
- `if`/`else` expressions, `else if` sugar
- `while` loops
- Boolean operators (`&&`, `||` with correct precedence)
- Unary negation (`-expr`)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Arithmetic (`+`, `-`, `*`, `/`)

### ABI
- Internal/extern ABI split
- Struct sret, param coercion, bool i1↔i8, ScalarPair
- `PassMode::Indirect` (byval) handled as Indirect

### Rust interop
- Any Rust type (Vec, etc.) with explicit type args
- Any inherent method (signatures queried from rustc)
- `use` imports
- Drop glue

### Deep monomorphization
- Internal toylang functions not exposed to rustc
- Deep chains (a→b→c→Rust), diamond patterns, shared callees
- Generic deep walks with type substitution
- Two entry points sharing internal functions
- `visited_symbols` prevents redundant walks across calls

### Validation
- Parser: duplicate names, reserved `__toylang_` prefix, all error variants tested
- Type resolver: 12 error variants, all tested (assignment type mismatch, undefined
  variable, wrong type arg count, etc.)
- `after_rust_analysis`: 5 validation checks

---

## Part 7: Key Files

### Library (`rustc-lang-facade/src/`)

| File | Purpose |
|------|---------|
| `lib.rs` | `LangCallbacks` trait, `FacadeGlobals`, vtable + trampolines, `is_from_lang_stubs` |
| `queries/layout.rs` | layout_of override |
| `queries/per_instance.rs` | per_instance_mir provider |
| `queries/symbol_name.rs` | symbol_name override |
| `queries/drop_glue.rs` | Drop glue (mir_shims) override |
| `queries/mod.rs` | Query override installation (4 providers) |
| `abi_helpers.rs` | ABI coercion helpers (includes hidden `#[track_caller]` param, see @TCHAPZ) |
| `mir_helpers.rs` | Drop glue MIR builder |
| `codegen_wrapper.rs` | CodegenBackend wrapper, .o injection |
| `driver.rs` | `run_compiler` entry point |
| `file_loader.rs` | Stub injection via FileLoader |

### Consumer (`toylangc/src/`)

| File | Purpose |
|------|---------|
| `llvm_gen.rs` | Inkwell LLVM backend: instance discovery, two-pass codegen, Rust method resolution |
| `stub_gen.rs` | Generates `__lang_stubs.rs` |
| `oracle.rs` | TyCtxt query helpers, type conversion, symbol mangling (`resolved_type_to_mangled_name`) |
| `main.rs` | CLI entry point |
| `toylang/ast.rs` | Untyped AST (Expr, Stmt incl. Assign, Block, BinOp incl. And/Or, UnaryNeg) |
| `toylang/typed_ast.rs` | `ResolvedType` enum, `TypedBlock`, `TypedStmt` incl. Assign |
| `toylang/type_resolve.rs` | Type annotation pass, `TypeResolveError` (12 variants, 33 unit tests) |
| `toylang/parser.rs` | Parser — `ParseError` (10 variants, 11 unit tests), precedence: `\|\|` < `&&` < comparison < additive < multiplicative |
| `toylang/registry.rs` | `ToyStruct`, `ToyFunction` (no redundant `name` field), `ToyParam` |
| `toylang/callbacks_impl.rs` | `LangCallbacks` impl, `ToylangState`, deep monomorphization walk, `CallbackLog` |

---

## Part 8: Architecture Decisions

### Why deep monomorphization walk

Previously, `collect_toylang_fn_deps` reported toylang callees to rustc, causing
rustc to process internal functions through `per_instance_mir` / `symbol_name`.
The deep walk eliminates this: `collect_toylang_fn_deps_inner` recursively walks
toylang callees, only returning Rust deps. Internal functions are stashed in
`ToylangState.toylang_instances` for direct codegen. Each function body is walked
exactly once via `visited_symbols`.

### Why one global mutex

Rustc's query providers fire on Rayon worker threads. Rather than making each
piece of consumer state individually thread-safe, a single `Mutex<FacadeGlobals>`
serializes all consumer code. The mutex is locked at every rustc→consumer entry
point. This guarantees single-threaded execution of all toylang code with zero
concurrency complexity.

### Why consumer state is `dyn Any` in the facade

The facade is generic over `C: LangCallbacks` but can't store `dyn LangCallbacks`
(the `'tcx` lifetime on methods breaks object safety). Consumer state is stored as
`Box<dyn Any + Send + Sync>` and passed to callbacks as `&mut dyn Any`. The
consumer downcasts to its concrete type. This keeps the facade library-agnostic.

### Why `is_consumer_type` / `is_consumer_fn` are callbacks

Originally the facade copied consumer name sets into static `HashSet` globals.
Now these are vtable callbacks — the facade asks the consumer directly "is this
yours?" via `is_consumer_type` / `is_consumer_fn`. No duplicated state.

### Why opaque stubs with 0-field layout

Reporting real field counts in `FieldsShape` caused ABI code to index into the
ADT's stub fields (which are dummy types). With 0 fields, the ABI code treats
consumer types as opaque memory blobs.

### Why per_instance_mir (rustc fork) instead of mir_built

`mir_built` fires once per function DEFINITION, not per instantiation. For generic
functions, rustc calls `mir_built` once for the generic definition and substitutes
internally. `per_instance_mir` fires per concrete `Instance<'tcx>`.

### Why explicit type args instead of inference

Type inference was attempted but caused cascading problems (backward propagation,
fragile heuristics for Vec element types). Explicit type args eliminated ~150 lines
of inference machinery.

### Why no mir_built or borrowck overrides

Consumer functions have `unreachable!()` bodies — valid Rust that passes all
checks normally. No need to intercept.

---

## Part 9: Known Technical Debt

See `known-tech-debt.md` for full details. Open items:

- **Generic function body validation** — blocked on trait bounds. Generic
  functions with bugs that are never called won't be caught until
  monomorphization time (same as C++ templates).

---

## Part 10: Planned — toylang.toml and Crate Dependencies

### 10.1 Motivation

Toylang currently compiles through rustc but can only use `std` types because
there's no Cargo integration. The goal is to let toylang use arbitrary Rust
crates (rand, regex, uuid, clap, serde, etc.) with Cargo as an invisible
implementation detail. Toylang controls its own build story.

### 10.2 toylang.toml manifest

Each toylang project has a `toylang.toml`:

```toml
[project]
name = "my-app"
source = "main.toylang"
edition = "2021"
features = ["allocator_api"]

[rust-dependencies]
rand = "0.8"
regex = { version = "1.10", features = ["unicode"] }
```

The `[rust-dependencies]` section uses Cargo.toml `[dependencies]` syntax
verbatim — no translation layer. Users run `toylangc build`, not
`RUSTC=toylangc cargo build`. No `Cargo.toml`, no `main.rs`, no glue files
visible to the user.

**Why `[rust-dependencies]` not `[dependencies]`?** Leaves room for
`[toylang-dependencies]` when toylang has its own package ecosystem.

**Why a separate manifest instead of Cargo.toml?** Toylang controls the UX.
Cargo is a tool in toylang's toolbox, not the other way around. If toylang
moves away from rustc someday, the user-facing contract doesn't change.

### 10.3 Build orchestration

`toylangc build` reads `toylang.toml` and orchestrates everything:

```
toylang.toml + main.toylang
         ↓
  toylangc build
         ↓
  generates .toylang-build/
    ├─ Cargo.toml         (from [rust-dependencies])
    └─ src/main.rs        (auto-generated shim)
         ↓
  cargo +rustc-fork build
    with RUSTC_WORKSPACE_WRAPPER=toylangc
    and  TOYLANG_INPUT=<path to .toylang source>
         ↓
  Dependencies compile with real rustc
  Workspace crate compiles through toylangc
         ↓
  .toylang-build/target/debug/<binary>
```

`toylangc` operates in three modes:
1. **Build mode**: `toylangc build` — reads toylang.toml, orchestrates cargo
2. **Wrapper mode**: invoked as `RUSTC_WORKSPACE_WRAPPER` by cargo. Reads
   `TOYLANG_INPUT` env var, compiles the workspace crate through the toylang
   pipeline.
3. **Direct mode**: `toylangc --toylang-input foo.toylang main.rs -o binary` —
   the existing compilation flow for integration tests.

This follows the same pattern as Clippy (`cargo-clippy` sets
`RUSTC_WORKSPACE_WRAPPER=clippy-driver`) and Miri. Dependencies compile with
real rustc; only the workspace crate goes through toylangc.

### 10.4 Trait method calls (explicit trait qualification)

Toylang calls trait methods using explicit trait qualification via the existing
`StaticCall` syntax (`Trait::method(receiver, args)`):

```
use std::io::Write
use std::io::Stdout

fn main() {
    let out = stdout()
    Write::write_all(&out, b"hello\n")
}
```

The caller specifies which trait provides the method. The compiler:
1. Looks up `Write` as a trait by name (via `find_use_imported_trait_def_id`)
2. Finds `write_all` in the trait's associated items
3. Resolves the concrete impl for the receiver's type (`Stdout`)
4. Gets the method's `DefId` from the impl, queries ABI, emits the call

This reuses the existing `StaticCall` AST node. The key change is distinguishing
"is this name a type or a trait?" in the oracle. For types, we do inherent
method lookup (existing behavior). For traits, we do trait impl lookup (new).

No trait bounds, no `where` clauses, no `dyn` dispatch. Toylang's generics are
late-bound (checked at monomorphization time, like C++ templates), so no trait
system is needed in the language itself.

**Possible future improvement — duck-typed trait method resolution:** Instead of
requiring explicit trait qualification, the compiler could search all trait impls
automatically when `receiver.method(args)` doesn't find an inherent method.
Using `tcx.all_traits()` and `tcx.for_each_relevant_impl()`, it would iterate
all traits to find one with a matching method that has an impl for the receiver's
concrete type. This would let users write `out.write_all(bytes)` instead of
`Write::write_all(&out, bytes)` — cleaner syntax at the cost of searching
potentially thousands of traits. Deferred because explicit qualification is
simpler to implement and avoids ambiguity when multiple traits define the same
method name.

### 10.5 Rust free function calls

Toylang currently supports calling Rust methods on types (`v.push(x)`,
`Vec::new()`) but not Rust free functions from modules. With `use` imports,
toylang can call free functions like `std::io::stdout()` or `rand::thread_rng()`:

```
use std::io::stdout

fn main() {
    let out = stdout()
    ...
}
```

The stub generator already emits `pub use std::io::stdout;` — the function is
visible to rustc. A new `find_use_imported_fn_def_id` oracle function searches
`module_children_local` for `DefKind::Fn` re-exports (parallel to
`find_reexported_type` for types). The `rust_method_ret` callback uses an
empty-string convention for the type name to signal a free function lookup.

### 10.6 Byte string literals

`b"hello\n"` syntax produces `&[u8]` values. The lexer recognizes the `b"`
prefix, supports escape sequences (`\n`, `\t`, `\\`, `\0`, `\"`), and produces
`Token::ByteStringLit(Vec<u8>)`. The type is `ResolvedType::Ref { inner:
ByteSlice }`.

In LLVM codegen, a byte string literal becomes a global constant array with a
fat pointer `{ ptr, usize }`. When passed to Rust functions like `write_all`,
the `&[u8]` may be a `ScalarPair` in rustc ABI (ptr and len as separate
arguments rather than a single struct).

### 10.7 I/O without glue

With trait method resolution, free function calls, and byte string literals,
toylang can do I/O directly:

```
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let out = stdout()
    out.write_all(b"hello world\n")
}
```

This exercises:
- Free function call (`stdout()`)
- Trait method call (`.write_all()` from `Write` trait)
- Byte string literal (`b"hello world\n"`)
- Return value discarding (`Result` from `write_all` silently dropped by
  `ExprStmt` — the codegen does `let _ = lower_typed_expr(...)`)

No `println!()` macro, no glue.rs, no Rust shim. Toylang uses the same
functions `println!()` uses under the hood.

### 10.8 .unwrap() on Result/Option

`.unwrap()` is an inherent method on both `Result<T, E>` and `Option<T>` —
`find_inherent_method` finds it without needing trait resolution. The `E: Debug`
bound on `Result::unwrap()` isn't checked at method discovery time.

This unlocks most Rust crate APIs:
- `Regex::new("pattern").unwrap()` → `Regex`
- `glob::glob("*.txt").unwrap()` → `Paths`
- `reqwest::blocking::get(url).unwrap()` → `Response`
- `toml::from_str::<Value>(text).unwrap()` → `Value`

Full enum support (match expressions, variant construction) is a separate
future feature. `.unwrap()` is sufficient for the "prove toylang can link
against arbitrary crates" milestone.

### 10.9 No derive macros needed

Every target crate has an imperative API that avoids derive macros:

| Crate | Imperative API | Example |
|-------|---------------|---------|
| regex | Method calls | `Regex::new(pat).unwrap().is_match(text)` |
| clap | Builder pattern | `Command::new("app").arg(Arg::new("input")).get_matches()` |
| serde_json | `serde_json::Value` | `serde_json::from_str::<Value>(json).unwrap()` |
| toml | `toml::Value` | `toml::from_str::<Value>(text).unwrap()` |
| rand | Free fn + trait method | `thread_rng().gen::<i32>()` |
| uuid | Static method | `Uuid::new_v4()` |
| indexmap | Constructor + methods | `IndexMap::new()`, `.insert()`, `.len()` |
| glob | Free function | `glob::glob("*.txt").unwrap()` |
| reqwest | Free function | `reqwest::blocking::get(url).unwrap()` |

Derive macros are syntactic sugar for trait impls. The underlying APIs are
always available imperatively.

### 10.10 Test projects

Standalone test projects live under `tests/standalone/`, each with a
`toylang.toml` and `main.toylang`. No Rust files, no glue. Each project proves
toylang can link against and call a specific Rust crate.

The test harness (`toylangc/tests/standalone_tests.rs`) builds each project
via `toylangc build` and asserts expected output.

---

## Part 11: Building and Testing (Current)

### Building the forked toolchain

```bash
cd ~/rust
git checkout per-instance-mir
python3 x.py build compiler/rustc
python3 x.py dist rustc-dev
cd /tmp && tar xzf ~/rust/build/dist/rustc-dev-*.tar.gz
cd rustc-dev-*/ && bash install.sh --prefix=$HOME/rust/build/host/stage2
rm -rf ~/rust/build/host/stage2/lib/rustlib/rustc-src
cd ~/rust && python3 x.py build library --stage 2
rustup toolchain link rustc-fork ~/rust/build/host/stage2
```

### Running tests

```bash
# All integration tests (95 passed, 0 ignored):
cargo +rustc-fork test -p toylangc --test integration_tests

# Unit tests (37 passed):
cargo +rustc-fork test -p toylangc --bin toylangc

# Check for warnings:
cargo +rustc-fork check -p toylangc
```
