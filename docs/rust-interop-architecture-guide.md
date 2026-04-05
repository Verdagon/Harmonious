# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 40 integration tests passing. Minimal rustc fork with
> `per_instance_mir` query. Inkwell LLVM backend with type annotation pass.
> Unified MonoItems discovery. Generic functions, inter-toylang calls, arithmetic.
> 4 query providers. Zero compiler warnings.

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

Key insight: steps 5 and 8 are the only places consumer logic runs.
Step 5 uses normal rustc (no `mir_built` or `borrowck` overrides needed).
Step 8 discovers and compiles everything in one pass.

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

The provider calls `monomorphize_fn` to get:
- The consumer extern symbol for this instance
- ALL dependencies (Rust functions, Rust types, AND other consumer functions)

It builds a MIR body that references these deps via ReifyFnPointer casts (for
functions) and NullaryOp::SizeOf (for types). The collector walks this body,
discovers the deps, and the fixpoint loop handles cascading.

The body ends with Unreachable — it's never executed. The codegen skip ensures
rustc doesn't emit code for consumer functions. The consumer .o provides definitions.

### 2.3 symbol_name

**Key:** `Instance<'tcx>` — maps consumer instances to consumer symbol names.

Calls `monomorphize_fn` to get the extern symbol. Examples:
- Concrete: `__toylang_impl_make_counter`
- Generic: `__toylang_impl_wrap__i32`
- Accessor: `__toylang_accessor_Pair_first__i32__i32`

Callers in rustc-compiled code emit direct calls to these symbols. The consumer .o
provides the definitions. Zero overhead — no wrappers, no trampolines.

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

**Extern declarations** for non-generic function symbols only:
```rust
extern "C" {
    pub fn __toylang_impl_make_counter() -> Counter;
    fn __toylang_accessor_Counter_value(s: *const Counter) -> *const i32;
}
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

### 4.1 Unified MonoItems discovery

`generate_with_tcx` walks `tcx.collect_and_partition_mono_items()` to find ALL
consumer instances in a single pass. No pre-marking, no `external_symbol` on the
registry, no `is_eligible`. MonoItems is the single source of truth.

For each consumer MonoItem:
- **Accessor methods:** GEP to field offset, return pointer
- **Functions (concrete + generic):** Full body codegen via inkwell

Discovery and codegen happen in the same `'tcx` scope, so live `Instance<'tcx>`
values are available for ABI queries.

### 4.2 Type annotation pass

`type_resolve.rs` produces a `TypedFnBody` where every expression carries a
`ResolvedType`. This runs before LLVM codegen and handles:

- **IntLit:** Correct width from context (i32 default, overridden by field types)
- **Generic type params:** Substituted with concrete types from Instance args
- **Vec element types:** Inferred from forward usage (push args, struct fields)
- **FnCall type args:** Inferred from expected return type (for generic callees)
- **BinaryOp:** Both operands share the expected type

### 4.3 Dependency discovery via type resolver

`collect_toylang_fn_deps` runs the type resolver on the caller's body to find
all FnCall nodes with their resolved type args. This unified approach handles:
- Concrete-to-concrete calls (empty type args)
- Concrete-to-generic calls (type args inferred from context)
- Generic-to-generic calls (caller's type params substituted first)

The resolved FnCall type args are converted to rustc `Ty<'tcx>` values and
used to construct concrete `GenericArgsRef` for the callee Instance.

### 4.4 Inkwell codegen

The backend uses inkwell (LLVM C API bindings, pinned to pre-2024-edition commit
for compatibility with rustc 1.86). Expression lowering handles:

- `IntLit` / `BoolLit` → inkwell const_int
- `Var` → load from alloca
- `StructLit` → alloca + GEP + store per field
- `BinaryOp` → build_int_add/sub/mul/div or float equivalents
- `FnCall` → declare extern + call (symbol computed from type_args)
- `StaticCall` (Vec::new) → sret call to Rust mangled symbol
- `MethodCall` (push/len) → call to Rust mangled symbol

### 4.5 ABI handling

`coerced_return_type_for_instance` queries `fn_abi_of_instance` with the concrete
Instance to determine:
- `PassMode::Direct` / `PassMode::Cast` → return coerced type in registers
- `PassMode::Indirect` (sret) → first param is output pointer, return void

Reference params (`&Vec<T>`) are loaded from their alloca before passing to
method calls (pointer-to-value, not pointer-to-pointer).

---

## Part 5: What Works (40 tests)

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
- Struct parameters (passthrough)
- Multiple let bindings with variable reuse
- Inter-toylang function calls (concrete-to-concrete)
- Generic callee calls (concrete calling generic, type args inferred)
- Arithmetic expressions (+, -, *, / with precedence)
- Boolean literals

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
| `llvm_gen.rs` | Inkwell LLVM backend: MonoItems walk, codegen, accessor GEPs |
| `stub_gen.rs` | Generates `__lang_stubs.rs` (opaque structs, wrappers, externs) |
| `oracle.rs` | TyCtxt query helpers (find_local_struct_ty, find_vec_method, etc.) |
| `main.rs` | CLI entry point, registry setup, rustc invocation |
| `toylang/ast.rs` | Untyped AST (Expr, Stmt, FnBody, BinOp) |
| `toylang/typed_ast.rs` | Typed AST (TypedExpr with ResolvedType) |
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

`collect_toylang_fn_deps` runs the type resolver on the caller's body to find
callee FnCall nodes with resolved type args. This is the same type resolver that
runs during codegen — reused, not duplicated. It handles all cases uniformly:
return position, let bindings, nested expressions, generic callee inference.

### Why no mir_built or borrowck overrides

Consumer functions have `unreachable!()` bodies — valid Rust that passes all
checks normally. No need to intercept `mir_built` or skip `borrowck`. This was
a significant simplification: 2 query overrides removed, ~350 lines deleted,
`build_extern_call_body` eliminated.

---

## Part 8: Session Handoff

### What was accomplished

Started with 5 tests passing (string-based LLVM backend, no fork, manual
`mark_compiled_functions`). Ended with 40 tests passing.

**Major architectural changes:**
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

**Query surface reduced from 6 providers to 4:**
- Removed: `mir_built`, `mir_borrowck`
- Kept: `layout_of`, `mir_shims`, `per_instance_mir`, `symbol_name`

### What remains (5 ignored tests)

| Test | Blocker |
|------|---------|
| `test_toylang_main_with_struct` | Needs `println` function + direct field access (`p.x`) |
| `test_toylang_main_with_vec` | Same: `println` + field access |
| `test_toylang_main_calls_toylang_fn` | Same: `println` + field access + toylang-owned main |
| `test_generic_callee_in_let` | Needs forward type inference for let bindings |
| `test_generic_callee_with_struct` | Struct passthrough ABI for generic identity function |

### Recommended next steps

1. **Direct field access** (`p.x` instead of `p.x()`) — new AST node `FieldAccess`,
   parser change, GEP in inkwell. ~30 min. Unblocks 3 aspirational tests.

2. **Comparison operators + if/else** — `==`, `<`, `>` tokens + `if expr { } else { }`
   AST + conditional branches in inkwell. Makes toylang do real logic.

3. **Loops** (`while`) — back-edge branches in inkwell. With arithmetic + comparisons
   + loops, toylang is Turing-complete.

4. **Forward type inference** — pre-scan return expression to propagate expected types
   backward to let bindings. Replaces the i32 default heuristic.

5. **`println` from toylang** — string literals + calling C printf or Rust IO.
   Unblocks 3 aspirational tests.

6. **Trait implementations** — `impl Trait for ConsumerType` in stubs. The
   `per_instance_mir` foundation supports it. Biggest remaining architectural feature.

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
# All non-ignored tests (should be 40 passed, 0 failed):
cargo +rustc-fork test -p toylangc --test integration_tests

# Including ignored (shows what still needs work):
cargo +rustc-fork test -p toylangc --test integration_tests -- --include-ignored

# Type resolver unit tests:
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve
```
