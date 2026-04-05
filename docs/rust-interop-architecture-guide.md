# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 28 integration tests passing. Minimal rustc fork with
> `per_instance_mir` query. Inkwell-based LLVM backend with type annotation pass.
> Unified MonoItems discovery for all function codegen (concrete + generic).

## Overview

Two-crate workspace:
- `rustc-lang-facade` — reusable library for integrating custom languages with rustc
- `toylangc` — toylang consumer

Pinned to a forked `nightly-2025-01-15` (rustc 1.86.0-dev) with 4 patches adding
`per_instance_mir`. The fork is at `~/rust` on branch `per-instance-mir`.

---

## Part 1: The Compilation Flow

```
┌─────────────────────────────────────────────────────────────────┐
│  Consumer frontend                                               │
│  1. Parse .toylang source → ToylangRegistry (structs, functions) │
│  2. Type params on structs and functions preserved (not resolved) │
└────────────────────────┬────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  rustc session (consumer embedded as query providers)            │
│                                                                  │
│  3. FileLoader injects __lang_stubs.rs (opaque structs,          │
│     accessor methods, wrapper functions — all unreachable!())    │
│  4. rustc parses and type-checks Rust source + stubs             │
│  5. Monomorphization begins (inside codegen_crate)               │
│     ├─ per_instance_mir fires for each consumer fn instance      │
│     │  → returns stub MIR with Rust deps (drives fixpoint loop)  │
│     ├─ layout_of fires for each consumer type instantiation      │
│     ├─ mir_shims fires for each consumer drop glue instance      │
│     └─ symbol_name maps instances to consumer symbols            │
│  6. Codegen dispatch skips consumer functions (extern decl only) │
│  7. generate_and_compile fires (all instances known)             │
│     ├─ Walks MonoItems to find all consumer instances             │
│     ├─ Type annotation pass resolves bodies with concrete types  │
│     ├─ Inkwell backend generates LLVM IR for each instance       │
│     └─ Compiles to .o, injected into link step                   │
│  8. Link: consumer .o + rustc .o → final binary                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Part 2: The Six Mechanisms

### 2.1 Opaque stubs via FileLoader

`__lang_stubs.rs` contains:
- **Opaque structs:** `pub struct Counter(());` or `pub struct Pair<A, B>(PhantomData<(A, B)>);`
- **Accessor methods:** `impl Counter { pub fn value(&self) -> &i32 { unreachable!() } }`
- **Wrapper functions:** `pub fn make_counter() -> Counter { unreachable!() }` (including generics)
- **Extern declarations:** For non-generic function symbols only (generic functions skip this)

All items live in `__lang_stubs`. Query overrides use `is_from_lang_stubs(tcx, def_id)`
to match by module path.

### 2.2 layout_of override

**Key:** `Ty<'tcx>` → fires per instantiation. Reports 0 fields in `FieldsShape`
(opaque blob). Size/alignment from `monomorphize_type` callback. Skips types with
unresolved type params (`has_param` check).

### 2.3 per_instance_mir (rustc fork)

**Key:** `Instance<'tcx>` → fires per concrete instantiation. Four-patch fork:
1. Query definition in `rustc_middle/src/queries.rs`
2. Collector patch in `rustc_monomorphize/src/collector.rs`
3. Codegen skip in `rustc_codegen_ssa/src/mono_item.rs`
4. Default provider in `rustc_mir_transform/src/shim.rs`

The provider returns MIR bodies that reference Rust deps (SizeOf for types,
ReifyFnPointer for functions), driving the collector's fixpoint loop.
Bodies end with Unreachable (never executed).

### 2.4 symbol_name override

Maps consumer instances to consumer symbols. For concrete functions:
`__toylang_impl_make_counter`. For generic instantiations:
`__toylang_impl_wrap__i32`. For accessors: `__toylang_accessor_Pair_first__i32__i32`.

Callers emit direct calls to these symbols. The consumer .o provides definitions.

### 2.5 mir_shims override (drop glue)

`InstanceKind::DropGlue(_, Some(ty))` → per instantiation. Builds MIR calling
`__toylang_drop_TypeName(ptr)`.

### 2.6 CodegenBackend wrapper

Wraps `rustc_codegen_llvm`. In `join_codegen`, injects consumer .o into
`CodegenResults`.

---

## Part 3: The LLVM Backend

### 3.1 Architecture

The backend uses inkwell (LLVM C API bindings). A single pass walks MonoItems
in `generate_with_tcx`, finds all consumer instances, and codegens each inline
with the live `Instance<'tcx>`.

No pre-marking, no `is_eligible`, no `external_symbol` on registry. MonoItems
is the single source of truth for what to compile.

### 3.2 Type annotation pass

`type_resolve.rs` runs before codegen, producing a `TypedFnBody` where every
expression carries a `ResolvedType`. Handles:
- IntLit → correct width from context (i32, i64, bool)
- Generic type param substitution (T → i32 for `wrap::<i32>`)
- Vec element type inference from forward usage / struct field types
- Nested struct field type resolution

### 3.3 Codegen flow per instance

For each consumer MonoItem:
1. Look up function in registry by name
2. For generic functions: substitute type params using Instance's concrete args
3. Run type annotation pass on the resolved body
4. Query ABI via `fn_abi_of_instance` with the concrete Instance
5. Build inkwell function with correct signature (sret for Indirect returns)
6. Walk TypedFnBody: lower statements, then return expression

### 3.4 ABI handling

- `PassMode::Direct` / `PassMode::Cast` → return coerced type in registers
- `PassMode::Indirect` (sret) → first param is output pointer, return void
- Reference params (`&Vec<T>`) → load pointer from alloca before method calls

---

## Part 4: What Works

28 tests passing, covering:
- Simple structs, generic structs, nested structs (T(T))
- Toylang structs containing Rust types (T(R): Vec<i32> fields)
- Rust containing toylang containing Rust (R(T(R)))
- 4-level deep nesting
- Vec operations (new, push, len) with struct and primitive elements
- Nested Vec (Vec<Vec<ToyPoint>>)
- Mixed fields (primitive + Vec + toylang struct)
- Generic structs with any type args (primitives, structs, Vec)
- Generic toylang functions (fn wrap<T>)
- Boolean literals
- Multiple let bindings, variable passthrough
- Large structs (6 fields, sret return)
- Drop glue

4 tests remain (toylang-main — separate milestone).

---

## Part 5: Key Files

### Library (`rustc-lang-facade/src/`)

| File | Purpose |
|------|---------|
| `lib.rs` | `LangCallbacks` trait, vtable, `is_from_lang_stubs` |
| `queries/layout.rs` | layout_of override (0-field opaque) |
| `queries/per_instance.rs` | per_instance_mir provider + accessor symbol helper |
| `queries/symbol_name.rs` | symbol_name override |
| `queries/mir_build.rs` | mir_built override (non-generic functions) |
| `queries/drop_glue.rs` | Drop glue override |
| `queries/borrowck.rs` | Borrowck skip |
| `queries/mod.rs` | Query override installation |
| `abi_helpers.rs` | ABI coercion queries |
| `mir_helpers.rs` | MIR body construction |
| `codegen_wrapper.rs` | CodegenBackend wrapper |
| `file_loader.rs` | Stub injection |

### Consumer (`toylangc/src/`)

| File | Purpose |
|------|---------|
| `llvm_gen.rs` | Inkwell LLVM backend (MonoItems walk + codegen) |
| `stub_gen.rs` | Generates __lang_stubs.rs |
| `toylang/ast.rs` | Untyped AST |
| `toylang/typed_ast.rs` | Typed AST (ResolvedType on every expr) |
| `toylang/type_resolve.rs` | Type annotation pass |
| `toylang/parser.rs` | Toylang parser |
| `toylang/registry.rs` | Data structures (ToyStruct, ToyFunction) |
| `toylang/callbacks_impl.rs` | LangCallbacks implementation |
| `oracle.rs` | TyCtxt query helpers |

---

## Part 6: Remaining Work

- **Toylang-main** — consumer-owned entry point (4 tests)
- **Inter-toylang function calls** — toylang calling other toylang functions
- **Arithmetic/control flow** — operators, if/else, loops (AST extensions)
- **Trait implementations** — `impl Trait for ConsumerType` in stubs
- **Build system** — RUSTC_WRAPPER integration
- **Architecture guide for library consumers** — how to build a new language on rustc-lang-facade
