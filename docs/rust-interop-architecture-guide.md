# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** Two-crate workspace: `rustc-lang-facade` (reusable library) and
> `toylangc` (toylang consumer). 9 integration tests passing. Preparing a minimal rustc
> fork to add a `per_instance_mir` query for per-instantiation function codegen.

## Scope

This document covers the full architecture for integrating a custom language with rustc.
The project is a Cargo workspace:
- `rustc-lang-facade` — reusable library implementing the rustc integration layer
- `toylangc` — toylang consumer, growing into a full language with linear types,
  deferred borrows, automatic refcounting, etc.

Pinned to `nightly-2025-01-15` (rustc 1.86.0-nightly, commit `8361aef0d7c`).

---

## Part 1: The Mental Model

### 1.1 What "query provider" means

rustc is a demand-driven computation graph. When rustc needs the layout of
`Vec<YourStruct>`, it calls `tcx.layout_of(Vec<YourStruct>)`, which calls
`tcx.layout_of(YourStruct)`. If your language has a custom provider for `layout_of`,
that call lands in your code. Your provider can call back into `tcx` freely. The query
system memoizes results and detects cycles.

**Your language does not need its own monomorphizer.** Rust's monomorphizer drives
the process. When it encounters `YourStruct` as a generic argument, it queries your
provider. You respond. Rust continues. Your language's logic executes *inside* rustc's
query evaluation.

### 1.2 The relationship to `unsafe`

Your language's safety guarantees are enforced by *your* type checker, not by Rust's
borrow checker. From rustc's perspective, your language's generated MIR is trusted,
the same way `unsafe` blocks are trusted.

### 1.3 The compilation flow

```
┌─────────────────────────────────────────────────────────────────┐
│  Consumer frontend (runs first, entirely outside rustc)          │
│  1. Parse source files                                           │
│  2. Type check (your language's rules)                           │
│  3. Produce generic IR (pre-monomorphization)                    │
│  4. Register everything in a ToylangRegistry                     │
└────────────────────────┬────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  rustc session (your language embedded as query providers)       │
│  5. rustc starts with Config::override_queries installed         │
│  6. rustc parses and type-checks Rust source files               │
│  7. Monomorphization begins (inside codegen_crate)               │
│     - per_instance_mir fires for each consumer function instance │
│     - layout_of fires for each consumer type instantiation       │
│     - mir_shims fires for each consumer drop glue instantiation  │
│  8. Consumer's generate_and_compile callback fires               │
│     - LLVM backend compiles all discovered instances to .o       │
│  9. codegen_mir skips consumer functions (leaves as extern decl) │
│  10. Link: consumer .o + rustc .o → final binary                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Part 2: The Five Mechanisms

### 2.1 Opaque stubs via FileLoader

A custom `FileLoader` injects generated Rust source (`__lang_stubs.rs`) into rustc's
parsing pipeline. The stubs contain:

- **Opaque struct definitions:** `pub struct Counter(());` or
  `pub struct Pair<A, B>(PhantomData<(A, B)>);` — layout_of reports 0 fields, so
  rustc treats these as opaque memory blobs.
- **Wrapper functions:** `pub fn make_counter() -> Counter { unreachable!() }` —
  gives Rust something to typecheck against. The body is never executed.
- **Accessor methods:** `impl Counter { pub fn value(&self) -> &i32 { unreachable!() } }`
  — Rust-side field access through methods. Bodies are never executed.
- **Extern declarations:** `extern "C" { fn __toylang_impl_make_counter(...); }` —
  for the linker to resolve against the consumer .o.

All consumer items (structs, functions, methods) live in `__lang_stubs`. Query overrides
use `is_from_lang_stubs(tcx, def_id)` to match by module path, preventing name collisions
with user-defined items.

### 2.2 layout_of override

**Key:** `Ty<'tcx>` (includes generic args) → fires **per instantiation**.

When rustc needs the size/alignment of a consumer type, our override calls
`monomorphize_type` on the consumer, which returns concrete field types. We compute
C-style layout (field offsets with padding) and return `BackendRepr::Memory { sized: true }`
with 0 fields in `FieldsShape`. The struct is a pure opaque blob to rustc.

### 2.3 per_instance_mir (requires rustc fork)

**Key:** `Instance<'tcx>` (includes concrete generic args) → fires **per instantiation**.

This is a new query added via a minimal rustc fork. When the monomorphization collector
encounters a consumer function instance (e.g., `identity::<i32>`), it calls
`per_instance_mir` instead of `instance_mir`. Our provider:

1. Asks the consumer for Rust-side dependencies (types and functions needed)
2. Records the instance for later batch compilation
3. Returns a MIR body that references those dependencies (driving the collector's
   fixpoint loop) with a panic terminator (never executed)

The collector walks this MIR body, discovers the referenced types and functions, adds
them to its work queue, and continues the fixpoint loop. This handles arbitrary
cascading depth naturally.

At codegen time, the `MonoItem::Fn` dispatch skips `codegen_mir` for consumer instances.
The predefine phase already created the LLVM function declaration; without `codegen_mir`
filling in a body, it remains an extern declaration. The `symbol_name` override maps
each consumer instance to the consumer's symbol name (e.g., `__toylang_impl_identity__i32`).
The consumer .o provides the definition. Standard extern linking, zero overhead.

**Why not mir_built?** `mir_built` is keyed by `LocalDefId` — one body per definition,
not per instantiation. For generic functions, rustc takes the single MIR body and
substitutes type parameters internally. We never get called for specific instantiations
like `identity::<i32>` vs `identity::<bool>`. The `per_instance_mir` query fixes this.

### 2.4 mir_shims override (drop glue)

**Key:** `InstanceKind::DropGlue(_, Some(ty))` → fires **per instantiation**.

When rustc drops a consumer type, our override builds a MIR body that calls
`__toylang_drop_TypeName(ptr)`. The consumer provides the destructor implementation.

### 2.5 CodegenBackend wrapper

`LangCodegenBackend` wraps `rustc_codegen_llvm`. During `join_codegen`, it injects
the consumer's .o into `CodegenResults` so it participates in the link step.

---

## Part 3: The rustc Fork

### 3.1 What we're adding

A single new query: `per_instance_mir(Instance<'tcx>) -> Option<&'tcx Body<'tcx>>`.
Plus patches to two call sites so they check this query first.

### 3.2 Patch sites

| File | Change | Purpose |
|------|--------|---------|
| `rustc_middle/src/queries.rs` | Add `per_instance_mir` query definition | Define the new query |
| `rustc_monomorphize/src/collector.rs` | Check `per_instance_mir` before `instance_mir` | Drive dependency discovery in fixpoint loop |
| `rustc_codegen_ssa/src/mono_item.rs` | Skip `codegen_instance` for consumer items | Leave as extern declaration |
| `rustc_mir_transform/src/lib.rs` | Default provider returning `None` | No-op for non-consumer compilations |

Additionally, in the consumer driver (not the fork):

| Override | Purpose |
|----------|---------|
| `per_instance_mir` | Return dependency-referencing MIR body |
| `symbol_name` | Map consumer instances to consumer symbol names |
| `mir_built` | Return trivial `unreachable!()` for consumer functions |

### 3.3 How codegen skipping works

The codegen pipeline has two phases per MonoItem:

1. **Predefine:** Creates LLVM function declaration (`declare_fn` → `LLVMRustGetOrInsertFunction`)
2. **Define:** Calls `codegen_mir` to fill in the body

If we skip step 2 for consumer items, the function stays as an extern declaration.
Callers already reference it by the symbol name from our `symbol_name` override.
The linker resolves it to the consumer .o.

### 3.4 Two-phase consumer callout

1. **During monomorphization (fixpoint loop):** `per_instance_mir` calls the consumer's
   dependency analyzer. Lightweight — no LLVM, no codegen. Returns which Rust types and
   functions this instantiation needs. Must be fast (fires many times).

2. **After monomorphization (during codegen_crate):** `generate_and_compile` fires.
   All instances are known. Consumer's LLVM backend runs in batch — full IR generation,
   optimization, .o emission.

### 3.5 How the fixpoint loop works for cascading dependencies

```
Collector discovers consumer_map::<i32>
  → per_instance_mir fires
  → Consumer says: "I need Vec<ConsumerPair<i32, i32>>"
  → MIR body references Vec<ConsumerPair<i32, i32>>
  → Collector discovers this type
    → layout_of fires (existing override)
    → drop_in_place discovered → mir_shims fires (existing override)
    → accessor methods discovered → per_instance_mir fires again
  → Repeat until fixpoint — no new items
```

### 3.6 Maintenance

The fork patches ~11 lines of rustc across 4 files. Expected maintenance: 0-30 minutes
per nightly bump. Most bumps apply cleanly.

---

## Part 4: Type Layout

### 4.1 Opaque layout (0-field FieldsShape)

Consumer types use `BackendRepr::Memory { sized: true }` with empty `FieldsShape::Arbitrary`.
Rustc sees them as opaque memory blobs. Size and alignment come from the consumer's
`monomorphize_type` callback, which returns concrete field types that the library uses
to compute C-style layout.

### 4.2 Why 0 fields

Reporting per-field offsets in the layout caused rustc's ABI code to index into the
ADT's fields (which are dummy stubs, not real field types) — field count mismatch caused
panics. With 0 fields, the ABI code treats the type as an opaque aggregate. No field
decomposition, no mismatch.

### 4.3 Generic types

`layout_of` is keyed by `Ty<'tcx>`, which includes generic args. So
`layout_of(Pair<i32, i32>)` and `layout_of(Pair<i64, i32>)` are separate invocations.
The consumer's `monomorphize_type` resolves type params from the concrete `Ty<'tcx>`,
returns field types, and the library computes layout. Mutually recursive layouts
(Rust type containing consumer type containing Rust type) work naturally via re-entrant
query calls.

### 4.4 Target portability

All layout computation uses `tcx.data_layout` — no hardcoded sizes. Pointer size,
alignment, and padding rules come from the target specification.

---

## Part 5: Accessor Methods

### 5.1 Why accessors

Consumer structs are opaque to Rust. Rust can't access fields directly. Instead,
the stub generates accessor methods:

```rust
impl Counter {
    pub fn value(&self) -> &i32 { unreachable!() }
}
```

The `unreachable!()` body is intercepted by the query system and replaced with
a call to the consumer's accessor implementation.

### 5.2 Non-generic accessors (current implementation)

For non-generic structs, the stub also declares an extern symbol:
```rust
extern "C" { fn __toylang_accessor_Counter_value(s: *const Counter, _deps: *const ()) -> *const i32; }
```

The LLVM backend generates a simple GEP function for each accessor.

### 5.3 Generic accessors (via per_instance_mir)

For generic structs like `Pair<A, B>`, accessor methods are generic:
```rust
impl<A, B> Pair<A, B> { pub fn first(&self) -> &A { unreachable!() } }
```

When rustc monomorphizes `Pair::<i32, i32>::first`, `per_instance_mir` fires with the
concrete instance. The consumer returns the extern symbol
`__toylang_accessor_Pair_first__i32__i32` and records the accessor request. The LLVM
backend generates a GEP function for that specific instantiation.

This is the same mechanism used for all consumer functions — accessors are not special-cased.

### 5.4 Toylang stays UFCS

Toylang has no `impl` blocks. All functions are top-level. The `impl` blocks in stubs
are purely a Rust-facing presentation layer. A `RustPresentation` enum controls how
each function appears to Rust: `FreeFunction`, `Method { on_type }`,
`TraitMethod { on_type, trait_name }`.

---

## Part 6: Monomorphization Ownership

### 6.1 Division of labor

- **Consumer compiles consumer functions** (to LLVM IR → .o file)
- **Rustc compiles Rust functions** (including generic instantiations like `Vec::push<YourStruct>`)
- **No overlap.** The `MonoItem::Fn` dispatch skips `codegen_mir` for consumer items,
  so rustc never emits code for them.

### 6.2 Triggering Rust monomorphization

The MIR body returned by `per_instance_mir` references Rust types (via `NullaryOp::SizeOf`)
and Rust functions (via `Call` terminators). The collector discovers these and adds them
to its work queue. This replaces the old `ReifyFnPointer` phantom cast mechanism, which
only worked for concrete (non-generic) consumer functions through `mir_built`.

### 6.3 Duplicate monomorphization prevention

The `symbol_name` override ensures consumer functions get consumer symbol names.
The `MonoItem::Fn` dispatch skips `codegen_mir` for consumer items. Rustc never
emits code for consumer functions, so there are no duplicate symbols.

---

## Part 7: Drop Glue

### 7.1 The drop chain

```
drop(Vec<YourStruct>)
  → Vec's Drop impl runs
  → drops each element: drop(YourStruct)
    → your destructor runs (__toylang_drop_YourStruct)
    → drop each field recursively
```

### 7.2 Implementation

`mir_shims` override intercepts `InstanceKind::DropGlue(_, Some(ty))` for consumer types.
Builds a MIR body calling `__toylang_drop_TypeName(ptr)`. The consumer provides the
destructor in its .o file.

**Important:** `mir_shims` bodies MUST call `set_required_consts` and `set_mentioned_items`
(both empty). The `mir_promoted` pass doesn't run for shims. Without this, the
monomorphization collector panics.

---

## Part 8: Module-Qualified Matching

All query overrides use `is_from_lang_stubs(tcx, def_id)` to verify the item comes from
the `__lang_stubs` module. This checks `tcx.def_path_str(def_id).starts_with("__lang_stubs::")`.

This prevents name collisions between consumer types and user-defined types with the
same name (e.g., a user-defined `Point` struct vs the consumer's `Point`).

---

## Part 9: Nightly API Churn

### 9.1 What changes

| Component | Stability | Change frequency |
|-----------|-----------|-----------------|
| `Callbacks` trait | Very stable | Rarely |
| `override_queries` mechanism | Very stable | Rarely |
| `Body`, `BasicBlock`, `Statement` | Mostly stable | Occasionally |
| `LayoutData` constructor | Unstable | Frequently |
| `TerminatorKind` fields | Unstable | Occasionally |

### 9.2 Isolate rustc_private usage

All `rustc_private` imports are confined to `rustc-lang-facade/src/`. The consumer
(`toylangc`) uses the library's public API. When a nightly breaks something, only the
library needs fixing.

### 9.3 The rustc fork adds maintenance

The fork patches 4 files. When bumping nightlies:
1. Rebase the fork branch onto the new commit
2. Verify the 4 patch sites still exist (grep for `instance_mir`)
3. Fix any API changes in MIR data structures
4. Rebuild the forked toolchain

---

## Part 10: Milestones

### Milestone 0: Proof of concept ✓

Validated query mechanisms: `layout_of`, `mir_built`, `mir_borrowck`, `mir_shims`,
type oracle. `Vec<Point>` compiles and runs.

### Milestone 1: External codegen ✓

Function bodies compiled to LLVM IR by external backend. `mir_built` produces thin
call stubs. CodegenBackend wrapper injects .o. ABI coercion via `fn_abi_of_instance`.

### Milestone 2: Struct nesting (in progress)

**Status:** 9 tests passing (up from 5).

**Done:**
- Opaque stubs with accessor methods
- Zero-field FieldsShape
- Module-qualified matching (`is_from_lang_stubs`)
- Parser + registry extended for ToyStruct and RustGeneric field types
- monomorphize_type handles nested structs and Rust generic fields
- Recursive LLVM codegen for nested struct construction
- Function wrapper generation in __lang_stubs (in progress)

**Blocked on rustc fork:**
- Generic accessor methods (need per_instance_mir)
- Phantom type dep monomorphization for T(R) tests
- All remaining test groups

**Test status:**
- Passing: counter_construct, pair_construct, vec_point, point_layout, point_drop,
  t_of_t_construct, t_of_t_layout, tg_i32_i64, tg_of_toypoint_layout
- Ignored (17): T(R), R(T(R)), T(R(T)), deep nesting, mixed fields, toylang main

### Milestone 3: rustc fork + per_instance_mir

Implement the minimal rustc fork:
- Add `per_instance_mir` query
- Patch monomorphization collector
- Patch codegen MonoItem dispatch
- Override `symbol_name` for consumer instances
- Convert `mir_built` override to use `per_instance_mir`
- Unify concrete and generic function handling

This unblocks all remaining Milestone 2 tests plus generic accessor methods.

### Milestone 4: LLVM backend expression coverage

Arithmetic, field access, comparisons, control flow (if/else, loops), local
variables, function calls between consumer functions.

### Milestone 5: Trait implementations

Generated `impl Trait for ConsumerType` blocks in stubs. Trait method stubs intercepted
by `per_instance_mir`. Consumer compiles trait method implementations to LLVM.

### Milestone 6: Consumer owns main

Consumer-defined `main` function as program entry point.

### Milestone 7: Build system integration

RUSTC_WRAPPER, Cargo rerun-if-changed, CI pipeline.

### Milestone 8: Codegen optimization

Evaluate codegen-units=16 performance. Investigate LTO if needed.

### Milestone 9: Diagnostics and debugger support

DWARF debug info, error attribution, source spans.

---

## Part 11: Key rustc APIs (Quick Reference)

### Layout queries
```rust
tcx.layout_of(PseudoCanonicalInput { value: ty, typing_env })
tcx.data_layout                       // Target-specific layout info
```

### Type construction
```rust
tcx.types.i32 / tcx.types.bool / tcx.types.unit
Ty::new_adt(tcx, adt_def, args)       // ADT with generic args
Ty::new_mut_ptr(tcx, ty)              // *mut T
Ty::new_fn_def(tcx, def_id, args)     // Function item type
```

### Definition lookup
```rust
tcx.def_kind(def_id)                  // Fn, Struct, Trait, etc.
tcx.item_name(def_id)                 // Name as Symbol
tcx.get_diagnostic_item(sym::Vec)     // DefId of well-known item
tcx.inherent_impls(type_did)          // All inherent impls
tcx.associated_item_def_ids(impl_did) // Items in an impl block
```

### MIR queries
```rust
tcx.mir_built(local_def_id)           // Raw MIR (before optimization)
tcx.instance_mir(instance_kind)       // MIR for a specific instance
tcx.per_instance_mir(instance)        // NEW: per-instantiation override
```

### Symbol names
```rust
tcx.symbol_name(instance)            // Mangled symbol name
```

### Function signatures and ABI
```rust
tcx.fn_sig(def_id).instantiate(tcx, args)  // Monomorphized signature
rustc_lang_facade::abi_helpers::coerced_return_type(tcx, def_id)  // ABI coercion
```

---

## Part 12: Known Unknowns

**Cross-crate type registration:** Consumer types in crate A need to be visible to
crate B without re-running the consumer frontend. Requires `.rmeta` integration.
Workaround: compile in the same session.

**Trait coherence:** Rust's orphan rule may reject trait impls for consumer types.
Whether `override_queries` can bypass this is untested.

**Async/generators:** Significantly more complex MIR. Separate milestone if needed.

**ABI coercion on non-aarch64:** `fn_abi_of_instance` handles target-specific rules,
but `cast_target_to_llvm_str` may need extension for x86_64 register splitting.

**per_instance_mir query caching:** The query is marked `no_hash` to avoid incremental
compilation issues. Need to verify this doesn't cause stale results within a session.

**CGU internalization:** The CGU partitioner may assign Internal linkage to consumer
function declarations, hiding them from the linker. May need `-C codegen-units=1` or
`exported_symbols` override. Needs testing.

---

## Quick Reference Checklist

### Query overrides
- [x] `layout_of` — non-generic and generic types (0-field opaque layout)
- [x] `mir_built` — extern call stubs (concrete functions)
- [x] `mir_borrowck` — selective skip for consumer items
- [x] `mir_shims` — drop glue for consumer types
- [ ] `per_instance_mir` — per-instantiation function stubs (needs fork)
- [ ] `symbol_name` — consumer symbol name mapping (needs fork)

### External LLVM backend
- [x] LLVM IR generation for struct-returning functions
- [x] ABI coercion via `fn_abi_of_instance`
- [x] Mangled symbol resolution via `tcx.symbol_name(instance)`
- [x] CodegenBackend wrapper (.o injection)
- [x] FileLoader (stub source injection)
- [x] Nested struct codegen (recursive alloca + GEP)
- [x] Accessor function codegen (GEP to field, non-generic)
- [ ] Generic accessor codegen (needs per_instance_mir)

### Stub generation
- [x] Opaque struct definitions (PhantomData / unit)
- [x] Wrapper functions with unreachable!() bodies
- [x] Accessor methods with unreachable!() bodies
- [x] Extern "C" declarations for accessor symbols (non-generic)
- [x] Extern "C" declarations for function symbols

### Type system
- [x] Primitive field types (i32, i64, f64, bool)
- [x] Type parameters in generic structs
- [x] ToyStruct fields (toylang containing toylang)
- [x] RustGeneric fields in monomorphize_type (Vec<i32>, etc.)
- [ ] Full T(R) codegen (needs phantom type dep monomorphization)

### Module-qualified matching
- [x] `is_from_lang_stubs` for layout_of, drop_glue
- [x] `is_from_lang_stubs` for mir_built (functions + accessor methods)
- [x] `is_from_lang_stubs` for borrowck

### Test suite
- [x] 9 passing integration tests
- [ ] 17 ignored tests (north star for Milestones 2-6)
