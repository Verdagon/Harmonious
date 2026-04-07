# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 53 integration tests passing, 0 ignored. Minimal rustc fork
> with `per_instance_mir` query. Inkwell LLVM backend with type annotation pass.
> Unified MonoItems discovery. Generic functions with explicit type args,
> inter-toylang calls, arithmetic, direct field access, `println` built-in,
> toylang-owned main, internal/extern ABI split with full param+return coercion.
> 4 query providers. Zero compiler warnings. No type inference machinery.

## Overview

Two-crate workspace:
- `rustc-lang-facade` — reusable library for integrating custom languages with rustc
- `toylangc` — toylang consumer

Forked `nightly-2025-01-15` (rustc 1.86.0-dev) with 4 patches adding `per_instance_mir`.
Fork at `~/rust` on branch `per-instance-mir`. Linked as toolchain `rustc-fork`.

---

## Part 1: The Compilation Flow

```
┌────────────────��──────────────────────────��─────────────────────┐
│  Consumer frontend                                               │
│  1. Parse .toylang source → ToylangRegistry (structs, functions) │
│  2. Type params on structs and functions preserved (not resolved) │
│  3. Registry is read-only — no pre-marking, no mutation           │
└────────────────���───────┬────────────────────────────────────────┘
                         │
                         ▼
┌──────────────────────���───────────────────────────────��──────────┐
│  rustc session (consumer embedded as query providers)            │
│                                                                  │
│  4. FileLoader injects __lang_stubs.rs (opaque structs,          │
│     accessor methods, wrapper functions — all unreachable!())    │
│  5. rustc parses, type-checks, borrow-checks normally            │
│     (unreachable!() bodies are valid Rust — no overrides needed) │
│  6. Monomorphization begins (inside codegen_crate)               │
│     ├─ per_instance_mir fires for each consumer fn instance      │
│     │  → returns stub MIR with ALL deps (Rust + toylang callees)│
│     │  → drives the collector's fixpoint loop                    │
│     ├─ layout_of fires for each consumer type instantiation      │
│     ├─ mir_shims fires for each consumer drop glue instance      │
│     └─ symbol_name maps instances to consumer symbols            │
│  7. Codegen dispatch skips consumer functions (extern decl only) │
│  8. generate_and_compile fires (all instances known)             │
│     ├─ Walks MonoItems — finds ALL consumer instances inline     │
│     ├─ For each: substitute type params → type resolve → codegen │
│     ├─ Inkwell backend generates LLVM IR                         │
│     └─ llc compiles to .o, injected into link step               │
│  9. Link: consumer .o + rustc .o → final binary                  │
└─────────────────────────────────────────────────────────────────┘
```

Key insight: consumer logic runs in steps 6 (query providers during
monomorphization) and 8 (LLVM codegen after monomorphization).
Step 5 uses normal rustc — no `mir_built` or `borrowck` overrides needed.

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

The provider calls `monomorphize_fn` which:
1. Computes the consumer extern symbol for this instance
2. Scans the body for Rust deps (Vec::new, Vec::push, etc.)
3. Runs the type resolver on the body to discover toylang callee deps
   (including generic callees with inferred type args)

It returns ALL deps. The provider builds a MIR body that references them via
ReifyFnPointer casts (for functions) and NullaryOp::SizeOf (for types). The
collector walks this body, discovers the deps, and the fixpoint loop cascades.

The body ends with Unreachable — never executed. The codegen skip ensures
rustc doesn't emit code for consumer functions. The consumer .o provides definitions.

### 2.3 symbol_name

**Key:** `Instance<'tcx>` — maps consumer instances to consumer symbol names.

Calls `monomorphize_fn` to get the extern symbol. Examples:
- Concrete: `__toylang_impl_make_counter`
- Generic: `__toylang_impl_wrap__i32`
- Accessor: `__toylang_accessor_Pair_first__i32__i32`

Callers in rustc-compiled code emit direct calls to these `__toylang_impl_` symbols.
The consumer .o provides thin extern wrappers at these symbols, which delegate to
`__toylang_internal_` functions with a simple predictable ABI (see 4.5).
Toylang-to-toylang calls bypass the wrappers and call internal symbols directly.

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
impl<A, B> Pair<A, B> {
    pub fn first(&self) -> &A { unreachable!() }
}
```

**Wrapper functions** for ALL consumer functions (concrete + generic):
```rust
pub fn make_counter() -> Counter { unreachable!() }
pub fn wrap<T>(x: T) -> Wrapper<T> { unreachable!() }
```

**Extern declarations** for non-generic function and accessor symbols:
```rust
extern "C" {
    pub fn __toylang_impl_make_counter() -> Counter;
    fn __toylang_accessor_Counter_value(s: *const Counter) -> *const i32;
}
```
Note: these extern "C" declarations are a legacy artifact. With `per_instance_mir`
+ `symbol_name`, callers use Rust ABI directly (not C ABI). The declarations
exist but may be unused — cleanup candidate.

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

### 4.1 Unified MonoItems discovery

`generate_with_tcx` walks `tcx.collect_and_partition_mono_items()` to find ALL
consumer instances in a single pass. No pre-marking, no `external_symbol` on the
registry, no `is_eligible`. MonoItems is the single source of truth.

For each consumer MonoItem:
- **Accessor methods:** GEP to field offset, return pointer (single pass)
- **Functions (concrete + generic):** Two-pass codegen:
  1. `codegen_internal_function` — body lowering with simple internal ABI
  2. `codegen_extern_wrapper` — thin Rust ABI adapter calling the internal function

Discovery and codegen happen in the same `'tcx` scope, so live `Instance<'tcx>`
values are available for ABI queries in the extern wrapper pass.

### 4.2 Type annotation pass

`type_resolve.rs` produces a `TypedFnBody` where every expression carries a
`ResolvedType`. This runs before LLVM codegen and handles:

- **IntLit:** Correct width from context (i32 default, overridden by field types,
  return types, and param types)
- **Generic type params:** Substituted with concrete types from Instance args
- **Vec element types:** Read from explicit type args on `Vec::new<Point>()`
- **FnCall type args:** Read from explicit type args on `wrap<i32>(42)` — no inference
- **BinaryOp:** Both operands share the expected type

**No inference machinery.** All generic type args must be provided explicitly at
call sites. Vec element types must be specified on `Vec::new<T>()`. Integer
literals default to i32 unless the immediate context provides an expected type
(return position, struct field, function parameter).

### 4.3 Dependency discovery via type resolver

`collect_toylang_fn_deps` runs `type_resolve::resolve_fn_body` on the caller's
body to produce a TypedFnBody, then walks it for FnCall nodes with explicit
type args. This unified approach handles all cases:
- Concrete-to-concrete calls (empty type args)
- Concrete-to-generic calls (type args provided explicitly at call site)
- Generic-to-generic calls (caller's type params substituted first via Instance args)

The explicit FnCall type args are converted to rustc `Ty<'tcx>` values and
used to construct concrete `GenericArgsRef` for the callee Instance.

Vec element types for `collect_rust_deps` are discovered by scanning the AST
for `Vec::new<ElemType>()` calls — the explicit type arg provides the element
type directly with no inference needed.

Note: the type resolver runs twice per function — once during dep discovery
(in `monomorphize_fn`) and once during LLVM codegen (in `generate_with_tcx`).
Same function, called at two different times. It's cheap (no LLVM, just string
matching and scope tracking).

### 4.4 Inkwell codegen

The backend uses inkwell (LLVM C API bindings, pinned to pre-2024-edition commit
for compatibility with rustc 1.86). Expression lowering handles:

- `IntLit` / `BoolLit` → inkwell const_int
- `StringLit` → `build_global_string_ptr` (pointer to constant string data)
- `Var` → load from alloca
- `StructLit` → alloca + GEP + store per field
- `FieldAccess` → GEP into struct + load (primitives) or return ptr (complex types)
- `BinaryOp` → build_int_add/sub/mul/div or float equivalents
- `FnCall` → call via `__toylang_internal_` symbol (internal ABI)
- `FnCall` (println) → format string → `printf` call with type-appropriate specifiers
- `StaticCall` (Vec::new) → sret call to Rust mangled symbol
- `MethodCall` (push/len) → call to Rust mangled symbol

Rust types (Vec, etc.) use opaque `[N x i8]` byte arrays — size and alignment
queried from `tcx.layout_of`. Toylang structs use real LLVM struct types with
GEP-based field access. See Part 10 for the full design rationale.

### 4.5 Internal/extern ABI split

Each toylang function generates **two** LLVM functions:

1. **Internal** (`__toylang_internal_{name}`) — simple, predictable ABI:
   - Primitives (i32, i64, f64, bool, usize): returned directly
   - Void: void return
   - Structs/Vec: always sret (ptr first param, void return)

2. **Extern wrapper** (`__toylang_impl_{name}`) — thin wrapper matching Rust ABI:
   - Calls the internal function
   - Adapts the return value to match `coerced_return_type_for_instance`

`codegen_internal_function` takes no `Instance` parameter — ABI decisions are
made purely from `ResolvedType` via `is_internal_sret()`. This means the internal
ABI is predictable at any call site without seeing the callee's definition,
eliminating ordering dependencies between functions.

`codegen_extern_wrapper` takes an `Instance` for ABI queries. It queries
`fn_abi_of_instance` for both return and parameter ABI info:

**Return adaptation** (4 cases):
- **Both sret** (Rust indirect + internal sret): pass sret pointer through
- **Internal sret, Rust direct** (coerced): alloca tmp, call internal, load as coerced type
- **Both direct** (primitives): forward call + `coerce_int_to_type` (handles i1→i8 for bool)
- **Void**: forward call

**Parameter adaptation** (`coerced_param_types_for_instance` from `abi_helpers.rs`):
- Each param's Rust ABI type is derived from `fn_abi.args[i].mode`
- If Rust ABI type matches internal type: pass through directly
- If different (e.g. Rust passes `i64` for a `{ i32, i32 }` struct): bitcast via
  memory (alloca rust type, store, load as internal type)
- `PassMode::Indirect` (large structs by pointer): load from pointer for internal
- `PassMode::Ignore` (ZSTs): skipped in LLVM signature

Empirical findings on aarch64 for param `PassMode`:
- `Point { i32, i32 }` → `Cast` to `i64` (8-byte integer register)
- `Counter { i32 }` → `Cast` to `i32` (4-byte integer register)
- `&Vec<T>` → `Direct` with `Scalar(Pointer)`, size=64 bits
- Primitives (i32, bool, etc.) → `Direct` with matching scalar size

**Known limitation — ref param redundant conversion:** For reference params
(`&Vec<T>`, `&T`), `fn_abi` reports `Direct` with size=64, which we convert to
`CoercedParam::Direct("i64")`. The internal function expects `ptr`. Since
`i64 != ptr` in LLVM type comparison, the bitcast-via-memory path fires
(alloca i64, store, load as ptr). This is correct but unnecessary — LLVM's
mem2reg pass eliminates the redundant alloca. A future optimization: detect
`Scalar(Pointer(...))` in `backend_repr` and emit `ptr` instead of `i64`.

`generate_with_tcx` runs two passes: internal functions first, then extern wrappers.

Toylang-to-toylang `FnCall` uses `__toylang_internal_` symbols directly. For struct
returns, the FnCall arm allocates an sret alloca and returns `ExprResult::Ptr`.

### 4.6 Toylang-owned main

When toylang defines `fn main()`, the stub wrapper is renamed to `__toylang_main`
to avoid conflicting with Rust's `main`. The mapping flows through:

- `fn_names()` includes both `"main"` and `"__toylang_main"`
- `monomorphize_fn` maps `"__toylang_main"` → `"main"` for registry lookup
- `compute_fn_symbol` uses registry name → extern symbol `__toylang_impl_main`
- `generate_with_tcx` maps back similarly
- Rust test code: `fn main() { __toylang_main(); }`

### 4.7 println built-in

`println` is a toylang built-in that compiles to C `printf`. Parsed as a normal
`FnCall` with a `StringLit` first arg. The type resolver has a guarded match arm
(`name == "println"`) that resolves args without looking up the registry.

At codegen time, `{}` placeholders are replaced with type-appropriate printf
specifiers (`%d`, `%ld`, `%zu`, `%f`) based on the resolved arg types. A `\n`
is always appended (like Rust's `println!`). Bool args are zero-extended from
i1 to i32 for printf compatibility.

`println` is skipped during dep discovery (not in registry → `continue`).

---

## Part 5: What Works (53 tests)

### Struct types
- Simple structs (`Counter { value: i32 }`)
- Generic structs (`Pair<A, B>`, `Wrapper<T>`)
- Nested structs (T(T): `ToyOuter { inner: ToyInner }`)
- Structs containing Rust types (T(R): `ToyShip { wings: Vec<i32> }`)
- Mixed fields (primitive + Vec + toylang struct)
- Large structs (6 fields, sret return)
- Single-field structs

### Type nesting
- R(T(R)): `Vec<ToyShip>` where ToyShip has Vec field
- T(R(T)): `ToyFleet { ships: Vec<ToyPoint> }`
- T(T(R)): `ToyShip { engine: ToyEngine { parts: Vec<i32> } }`
- R(R(T)): `Vec<Vec<ToyPoint>>`
- 4-level deep nesting

### Generic instantiation
- Generic structs with any type args (primitives, structs, Vec)
- Generic toylang functions (`fn wrap<T>(x: T) -> Wrapper<T>`)
- Generic type params on functions (parsed and resolved)
- Accessor methods on generic structs (per-instantiation via per_instance_mir)

### Function features
- Multiple parameters
- Struct parameters (passthrough, including through generic functions)
- Multiple let bindings with variable reuse
- Inter-toylang function calls (concrete-to-concrete)
- Generic callee calls with explicit type args (`wrap<i32>(42)`)
- Arithmetic expressions (+, -, *, / with precedence)
- Boolean literals and bool return values
- String literals (for println format strings)
- Direct field access (`p.x`)
- `println` built-in (compiles to C printf)
- Toylang-owned main (`fn main()` in toylang, called via `__toylang_main`)
- Explicit Vec element types (`Vec::new<Point>()`)

### ABI
- Internal/extern ABI split (internal: predictable, extern: matches Rust)
- Struct returns via sret (internal always, extern per `coerced_return_type_for_instance`)
- Struct param coercion (extern per `coerced_param_types_for_instance`)
- Bool i1↔i8 coercion handled by `coerce_int_to_type`
- Struct ABI coercion (e.g. `{ i32, i32 }` → `i64` on aarch64, both params and returns)

### Vec operations
- Vec<primitive> and Vec<struct>
- Vec::new, push, len
- Vec as struct field
- Nested Vec (Vec<Vec<T>>)

### Infrastructure
- Drop glue for consumer types
- Layout computation (0-field opaque, target-portable)
- ABI coercion (Direct, Cast, Indirect/sret)
- Module-qualified matching (no name collisions)
- Zero compiler warnings

---

## Part 6: Key Files

### Library (`rustc-lang-facade/src/`)

| File | Purpose |
|------|---------|
| `lib.rs` | `LangCallbacks` trait (7 methods), vtable + HRTB trampolines, `is_from_lang_stubs` |
| `queries/layout.rs` | layout_of override (0-field opaque, has_param check) |
| `queries/per_instance.rs` | per_instance_mir provider, accessor symbol helper |
| `queries/symbol_name.rs` | symbol_name override |
| `queries/drop_glue.rs` | Drop glue (mir_shims) override |
| `queries/mod.rs` | Query override installation (4 providers) |
| `abi_helpers.rs` | `coerced_return_type_for_instance`, `CoercedReturn` enum |
| `mir_helpers.rs` | `build_drop_call_body` for drop glue MIR |
| `codegen_wrapper.rs` | `LangCodegenBackend` wrapper, .o injection |
| `driver.rs` | `run_compiler` entry point |
| `file_loader.rs` | `LangFileLoader` for stub injection |

### Consumer (`toylangc/src/`)

| File | Purpose |
|------|---------|
| `llvm_gen.rs` | Inkwell LLVM backend: MonoItems walk, internal/extern codegen, accessor GEPs |
| `stub_gen.rs` | Generates `__lang_stubs.rs` (opaque structs, wrappers, externs) |
| `oracle.rs` | TyCtxt query helpers (find_local_struct_ty, find_vec_method, etc.) |
| `main.rs` | CLI entry point, registry setup, rustc invocation |
| `toylang/ast.rs` | Untyped AST (Expr incl. FieldAccess/StringLit, Stmt, FnBody, BinOp) |
| `toylang/typed_ast.rs` | Typed AST (TypedExpr with ResolvedType incl. Str) |
| `toylang/type_resolve.rs` | Type annotation pass (10 unit tests) |
| `toylang/parser.rs` | Toylang parser (structs, functions, expressions) |
| `toylang/registry.rs` | Data structures (ToyStruct, ToyFunction, ToyFieldType) |
| `toylang/callbacks_impl.rs` | `LangCallbacks` impl, dep discovery, MonoItems helpers |

### rustc fork (`~/rust` branch `per-instance-mir`)

| File | Change |
|------|--------|
| `compiler/rustc_middle/src/query/mod.rs` | `per_instance_mir` query definition |
| `compiler/rustc_monomorphize/src/collector.rs` | Check `per_instance_mir` before `instance_mir` |
| `compiler/rustc_codegen_ssa/src/mono_item.rs` | Skip `codegen_instance` if `per_instance_mir` returns Some |
| `compiler/rustc_mir_transform/src/shim.rs` | Default provider returning None |

---

## Part 7: Architecture Decisions

### Why opaque stubs with 0-field layout

Reporting real field counts in `FieldsShape` caused ABI code to index into the
ADT's stub fields (which are dummy types). With 0 fields, the ABI code treats
consumer types as opaque memory blobs. Size and alignment come from
`monomorphize_type`, not from the stub definition.

### Why per_instance_mir (rustc fork) instead of mir_built

`mir_built` fires once per function DEFINITION, not per instantiation. For generic
functions like `wrap<T>`, rustc calls `mir_built` once for the generic definition
and substitutes type params internally. We never get called for specific
instantiations like `wrap::<i32>`. The `per_instance_mir` query fixes this — it's
keyed by `Instance<'tcx>` which includes concrete generic args.

### Why unified MonoItems discovery instead of pre-marking

Pre-marking (`mark_compiled_functions` / `is_eligible` / `external_symbol`) ran
before rustc and couldn't handle generic functions (no concrete instantiations
known yet). MonoItems discovery runs after monomorphization when all concrete
instances are known. Both concrete and generic functions flow through the same path.

### Why the type resolver drives dependency discovery

`collect_toylang_fn_deps` runs `resolve_fn_body` on the caller's body to produce
a TypedFnBody with all FnCall type args resolved. This handles all callee cases
uniformly — return position, let bindings, nested expressions. For generic callers,
type params are substituted from the Instance's args before resolving. The same
`resolve_fn_body` function also runs during LLVM codegen, keeping both paths
consistent.

### Why explicit type args instead of inference

Type inference was attempted (inferring generic type params from expected return
type and from argument types) but caused cascading problems:
- Backward propagation from return types to let bindings broke generic functions
  (tried to parse unresolved type params as concrete types)
- Vec element type inference required a fragile 4-tier heuristic chain
  (signature → struct fields → push struct literals → typed body scan)
- The heuristics failed on `v.push(make_point())` because push args that were
  function calls (not struct literals) weren't recognized

Requiring explicit type args (`wrap<i32>(42)`, `Vec::new<Point>()`) eliminated
all inference machinery (~150 lines) and made the compiler simpler and more
predictable. This is appropriate for a proof-of-concept; inference can be
re-added later if needed.

### Why no mir_built or borrowck overrides

Consumer functions have `unreachable!()` bodies — valid Rust that passes all
checks normally. No need to intercept `mir_built` or skip `borrowck`. This was
a significant simplification: 2 query overrides removed, ~350 lines deleted,
`build_extern_call_body` eliminated.

---

## Part 8: Callback Timeline and State Access

Every callback from rustc into consumer code, in execution order:

### Phase 1: Stub injection (before rustc parsing)

**`FileLoader::read_file("__lang_stubs.rs")`** → calls `generate_stubs()`
- Reads: registry (struct names, function signatures, type params)
- Produces: Rust source code string
- No TyCtxt available yet

Stubs are needed so Rust code can reference toylang types and functions during
type checking. They contain only the public API — struct names, function
signatures, generic params. Bodies are `unreachable!()`.

**Stubs are only needed in one direction:** when Rust code references toylang
items. In the other direction (toylang referencing Rust), toylang discovers
Rust types by querying TyCtxt — no stubs needed.

**The toylang parser runs before rustc** (in `main.rs`). It produces the
registry, which `generate_stubs` reads. A future improvement: split into a
lightweight pre-scan (just names/signatures for stubs) and full parse + type
check (in `after_rust_analysis` where TyCtxt is available).

### Phase 2: Type checking (during analysis)

**`after_rust_analysis(tcx)`** → currently a no-op.

This is where toylang's type checker should eventually run — verifying toylang
code against Rust's type system (types exist, method signatures match, trait
bounds satisfied). Full TyCtxt is available.

Currently, toylang generics are like C++ templates: unchecked until instantiated.
A generic function with errors is only caught at monomorphization/codegen time
via panics, not at type-check time with error messages.

### Phase 3: Monomorphization (fixpoint loop inside codegen_crate)

These fire repeatedly as the collector discovers new items:

**`layout_of(Ty<'tcx>)`** → calls `monomorphize_type(name, tcx, ty)`
- Reads: registry (struct field types, type params)
- Queries tcx: `layout_of` on each field type (recursive back into rustc)
- Returns: concrete field types → library computes C-style layout

**`per_instance_mir(Instance<'tcx>)`** → calls `monomorphize_fn(name, tcx, def_id, instance)`
- Reads: registry (function body AST, type params, return type)
- Runs: `collect_rust_deps` (scans AST for Vec ops, queries tcx for mangled symbols)
- Runs: `collect_toylang_fn_deps` (runs type resolver on body, walks typed AST for
  FnCall nodes, queries tcx for callee DefIds + constructs concrete GenericArgs)
- Returns: extern symbol + all deps → library builds stub MIR for collector

**`symbol_name(Instance<'tcx>)`** → calls `monomorphize_fn(name, tcx, def_id, instance)`
- Same callback as per_instance_mir — recomputes the same extern symbol
- Returns: symbol string only (deps ignored by caller)

**`mir_shims(InstanceKind::DropGlue)`** → `build_drop_call_body`
- Reads: just the type name (not registry)
- Queries tcx: `find_extern_fn` for `__toylang_drop_{name}`
- Returns: MIR body calling the drop function

### Phase 4: LLVM codegen (after monomorphization settles)

**`generate_and_compile(tcx)`** → `generate_with_tcx(tcx, registry, callbacks)`
- Queries tcx: `collect_and_partition_mono_items` (discovers all consumer instances)
- For each consumer instance:
  - Calls `monomorphize_fn` again (to get extern symbol — third time)
  - Runs type resolver again (same `resolve_fn_body` — second time)
  - Queries tcx: `fn_abi_of_instance` (for ABI coercion)
  - Builds inkwell LLVM IR
- Produces: .o file → injected into link step via CodegenBackend wrapper

### Redundant work (known issue)

`monomorphize_fn` is called 3 times per consumer function instance:
1. From `per_instance_mir` (dep discovery + symbol)
2. From `symbol_name` (symbol only)
3. From `generate_and_compile` (symbol + type resolver for codegen)

Each call recomputes the extern symbol and (for calls 1 and 3) runs the full
type resolver on the body. This is correct but wasteful.

**Fix:** Cache the result in `ToylangCallbacks` using a
`Mutex<HashMap<String, MonomorphizedResult>>` keyed by extern symbol (which is
lifetime-free). `per_instance_mir` populates it on first call. `symbol_name`
reads the cached symbol. `generate_and_compile` reads the cached TypedFnBody.
Can't key by `Instance<'tcx>` directly (has lifetime), but the extern symbol
string is unique per Instance and owned.

---

## Part 9: Session Handoff

### What was accomplished

Started with 5 tests passing (string-based LLVM backend, no fork, manual
`mark_compiled_functions`). Now at 53 tests passing, 0 ignored.

**Major architectural changes (sessions 1-2):**
1. Minimal rustc fork with `per_instance_mir` query (4 patches, ~11 lines)
2. Full inkwell rewrite of LLVM backend (string IR → builder API)
3. Type annotation pass (`type_resolve.rs` + `typed_ast.rs`)
4. Unified MonoItems discovery (eliminated `mark_compiled_functions`, `is_eligible`, `external_symbol`)
5. Removed `mir_built` and `borrowck` overrides (unreachable!() bodies pass normally)
6. Rust ABI matching (sret for large structs, no `_deps` parameter)
7. `Instance<'tcx>` threaded through for correct ABI queries
8. Inter-toylang function calls with dep discovery via type resolver
9. Generic toylang functions (`fn wrap<T>`)
10. Arithmetic expressions (+, -, *, / with precedence)

**Session 3 additions (40 → 53 tests):**
11. Direct field access (`p.x`) — FieldAccess AST node, GEP codegen
12. `println` built-in — StringLit token/AST, printf codegen with type specifiers
13. Toylang-owned main — `__toylang_main` stub rename, fn_names mapping
14. Internal/extern ABI split — `codegen_internal_function` + `codegen_extern_wrapper`,
    eliminates ABI mismatch for toylang-to-toylang calls
15. Full param+return ABI coercion — `coerced_param_types_for_instance` from fn_abi
16. Removed all type inference — explicit type args on generic calls (`wrap<i32>(42)`)
    and Vec creation (`Vec::new<Point>()`)
17. Bug fixes — exact Vec method lookup, lexer rejects unknown chars, panic on
    missing --toylang-input

**Query surface reduced from 6 providers to 4:**
- Removed: `mir_built`, `mir_borrowck`
- Kept: `layout_of`, `mir_shims`, `per_instance_mir`, `symbol_name`

### What remains (0 ignored tests)

All 53 tests pass. No known blockers.

### Recommended next steps

1. **Comparison operators + if/else** — `==`, `<`, `>` tokens + `if expr { } else { }`
   AST + conditional branches in inkwell. Makes toylang do real logic.

2. **Loops** (`while`) — back-edge branches in inkwell. With arithmetic + comparisons
   + loops, toylang is Turing-complete.

3. **Trait implementations** — `impl Trait for ConsumerType` in stubs. The
   `per_instance_mir` foundation supports it. Biggest remaining architectural feature.

4. **Eliminate Vec-specific logic** — see Part 10.4 roadmap.

### Known technical debt

- **String-based type resolution** duplicated in 6+ places (type_from_string,
  resolve_rust_ty_from_string, parse_coerced_type, etc.) — each slightly different
- **println hardcoding** — special-cased by name in type resolver and codegen
- **Ref param redundant conversion** in extern wrapper — `fn_abi` reports `i64` for
  pointers, internal expects `ptr`, bitcast-via-memory fires unnecessarily (LLVM
  optimizes it away)
- **Vec-specific code** (~200 lines) — hardcoded push/len/new handling throughout

### Building the forked toolchain

```bash
cd ~/rust
git checkout per-instance-mir
python3 x.py build compiler/rustc  # builds compiler
python3 x.py dist rustc-dev        # creates rustc-dev component
# Install rustc-dev into stage2 sysroot:
cd /tmp && tar xzf ~/rust/build/dist/rustc-dev-*.tar.gz
cd rustc-dev-*/ && bash install.sh --prefix=$HOME/rust/build/host/stage2
# Remove rustc-src to prevent source compilation:
rm -rf ~/rust/build/host/stage2/lib/rustlib/rustc-src
# Build stage2 library:
cd ~/rust && python3 x.py build library --stage 2
# Link toolchain:
rustup toolchain link rustc-fork ~/rust/build/host/stage2
# Verify:
cargo +rustc-fork test -p toylangc --test integration_tests
```

### Running tests

```bash
# All tests (should be 53 passed, 0 failed, 0 ignored):
cargo +rustc-fork test -p toylangc --test integration_tests

# Including ignored (shows what still needs work):
cargo +rustc-fork test -p toylangc --test integration_tests -- --include-ignored

# Type resolver unit tests:
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve
```

---

## Part 10: Long-Term Architecture Decisions

### 10.1 Type representation boundary

- **Toylang structs** = real LLVM struct types with GEP-based field access.
  Toylang controls the layout.
- **Rust types** = opaque `[N x i8]` byte arrays via `layout_of`. Toylang never
  sees Rust type internals. Size and alignment come from `tcx.layout_of`.
- This split is permanent. Neither side needs to know the other's layout.

`resolved_type_to_rustc_ty` in `llvm_gen.rs` converts any `ResolvedType` to a
rustc `Ty<'tcx>`, enabling `layout_of` queries for any type. `rust_ty_to_llvm_opaque`
wraps this to produce the `[N x i8]` LLVM type + alignment.

### 10.2 Cross-boundary field access via getters only

- **Rust accessing toylang fields:** calls accessor methods generated by toylang
  (already implemented — `codegen_accessor_inline` emits GEP + return pointer).
- **Toylang accessing Rust fields:** will call accessor methods generated on the
  Rust side (in stubs). Toylang never GEPs into Rust types.
- All cross-boundary data access is via method calls. No layout knowledge crosses
  the boundary.

For user-defined Rust structs that toylang wants to use, accessor methods are
generated in the stub (`__lang_stubs.rs`), similar to how toylang struct accessors
are already generated.

### 10.3 Rust method resolution: inherent + explicit trait qualification

- **Inherent methods** (push, len, new, get, insert, etc.): resolved by iterating
  `tcx.inherent_impls(adt_def_id)` and finding the method by name. This is a
  direct generalization of `find_vec_method` in `oracle.rs`.
- **Trait methods** (clone, to_string, etc.): toylang uses UFCS syntax —
  `Clone::clone(my_vec)` — so the user explicitly names the trait. The compiler
  looks up the trait's impl for the concrete type via `tcx.trait_impls_of`.
  No implicit trait resolution or method probing needed.
- This means toylang never needs autoderef or method resolution probing.

### 10.4 Roadmap to eliminate Vec-specific logic

Vec is currently the only Rust type toylang uses directly. ~200 lines of
Vec-specific code are spread across `llvm_gen.rs`, `callbacks_impl.rs`, and
`oracle.rs`. The elimination roadmap has 4 moves:

1. **Move 1 (done):** Opaque Rust types via `layout_of`. Foundation:
   `resolved_type_to_rustc_ty` + `rust_ty_to_llvm_opaque`. Replaced `vec_type()`
   hardcoded `{usize, usize, usize}` with `[N x i8]` from `layout_of`.

2. **Move 2:** General inherent method resolution. Replace `find_vec_method` +
   `resolve_vec_symbols` with `find_inherent_method(tcx, adt_def_id, method_name)`.
   Use `fn_abi_of_instance` for calling conventions (both params and return).
   On-demand resolution at call sites, no upfront symbol scanning.

3. **Move 3:** Merge `collect_rust_deps` (untyped AST scan for Vec ops) into
   `collect_toylang_fn_deps` (typed AST walk). One walk, all deps. Eliminates
   `scan_body_vec_ops`, `find_vec_elem_ty`, `find_vec_in_fields_recursive`,
   `body_uses_vec`, `find_vec_elem_name`, `find_vec_elem_from_params`.

4. **Move 4:** Replace hardcoded `push`/`len`/`new` codegen match arms with
   general "call Rust method" path using ABI info from `fn_abi_of_instance`.
