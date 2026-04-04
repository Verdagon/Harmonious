# Handoff: Milestone 2 — Struct Nesting and Type Dependency Discovery

## What this document is

A thorough handoff for the next person implementing Milestone 2: making toylang structs
able to contain Rust types (like `Vec<i32>`) and other toylang structs. This is the
biggest architectural step remaining — it requires solving the "type dep discovery" problem
and touches the stub generator, the LLVM backend, and the library's MIR builder.

## Read these first

1. **`rustc-lang-facade/README.md`** — the library's API, compilation lifecycle, debugging tips
2. **`docs/historical/struct-opacity-and-type-deps.md`** — full analysis of Option A vs B
   with concrete examples, code sketches, and tradeoffs. **The decision is Option B** (opaque
   stubs, lazy discovery). Read this entire doc.
3. **`docs/rust-interop-architecture-guide.md`** — Milestone 2 description and checklist

## Current state

### What works today

9 passing integration tests (`cargo test -p toylangc`):
- Toylang struct with primitive fields (`Counter { value: 42 }`)
- Generic toylang struct with primitive args (`Pair<i32, i32>`)
- Generic toylang with mixed-size args (`Pair<i32, i64>`)
- `Vec<ToyPoint>` — Rust containing toylang type
- Layout override (`size_of`, `align_of`)
- Drop glue (`__toylang_drop_Point` fires correctly)
- **Toylang struct containing another toylang struct** (`ToyOuter { inner: ToyInner }`)
- **Layout of nested toylang structs** (`ToyOuter` with `ToyInner` + `i32` = 8 bytes)
- **Layout of generic toylang wrapping toylang** (`ToyWrapper<ToyPoint>` = 8 bytes)

### What doesn't work (17 ignored tests)

Run `cargo test -p toylangc -- --ignored` to see them all fail. Key ones:

- `test_t_of_r_vec_field` — toylang struct with `Vec<i32>` field (needs phantom local monomorphization)
- `test_tg_bool_i32` — generic struct with bool type arg (LLVM gen issue)
- `test_tg_of_toypoint` — generic struct wrapping toylang type (needs generic accessor monomorphization)
- `test_r_t_r_vec_of_ship` — `Vec<ToyShip>` where ToyShip has `Vec<i32>`
- `test_t_r_t_construct` — toylang struct with `Vec<ToyPoint>` field
- `test_deep_t_r_t_r` — 4 levels of nesting
- `test_mixed_fields` — struct with primitive + rust + toylang fields
- `test_toylang_main_*` — toylang-defined main function (4 tests, separate milestone)

### What the toylang parser currently supports

From `toylangc/src/toylang/parser.rs`:
- Struct definitions with fields: `i32`, `i64`, `f64`, `bool`, type params
- **Toylang struct names as field types** (e.g. `inner: ToyInner`) — DONE
- **Rust generic types as field types** (e.g. `wings: Vec<i32>`) — DONE
- Function bodies: integer literals, variables, struct literals, `Vec::new()`, `.push()`, `.len()`
- **NOT supported:** field access (`p.x`), arithmetic, if/else, loops

### What's been implemented so far

**Parser + registry:** `ToyFieldType` has new variants `ToyStruct(String)` and
`RustGeneric(String, Vec<ToyFieldType>)`. Parser handles both. Struct names are tracked
during parsing so forward references within a file work.

**Opaque stubs:** Struct stubs are now opaque — `pub struct Counter(())` for non-generic,
`pub struct Pair<A, B>(PhantomData<(A, B)>)` for generic. Rust code accesses fields via
generated accessor methods (`c.value()`, `p.first()`).

**Zero-field layout:** `layout_of` override reports 0 fields in `FieldsShape`. Rustc
treats consumer types as opaque memory blobs via `BackendRepr::Memory { sized: true }`.
This prevents rustc's ABI code from indexing into the struct's ADT fields (which are
dummies). The total size and alignment are computed from the consumer's real field types.

**Module-qualified type matching:** `layout_of` and `drop_glue` overrides check both type
name AND that the DefId comes from the `__lang_stubs` module (`is_from_lang_stubs`). This
prevents name collisions with user-defined types that happen to share a name with a toylang
type. (Previously matched by bare name only, which caused crashes.)

**monomorphize_type:** Handles `ToyStruct` fields (looks up the struct's `Ty` via
`find_local_struct_ty`) and `RustGeneric` fields (constructs `Ty` via `get_diagnostic_item`
for known types like Vec). Both work for layout computation.

**LLVM backend:** Handles nested struct fields in LLVM IR generation. Uses recursive
`lower_store_struct_lit` for nested `StructLit` expressions (alloca + GEP chain). Generates
accessor functions for non-generic structs (GEP to field offset, return pointer).

**Accessor methods:** Non-generic structs use extern LLVM accessor functions. Generic
structs currently use inline Rust pointer math — **this is tech debt to be fixed** (see
"Next: route generic accessors through monomorphization" below).

## The problem you're solving

When a toylang struct contains a Rust generic type:

```
struct ToyShip {
    wings: Vec<i32>,
}
```

Rustc needs to monomorphize `Vec<i32>` — generate its drop glue, layout, method bodies.
But rustc discovers types to monomorphize by looking at struct field declarations in the
parsed source. Our stubs are what rustc sees. If the stub doesn't mention `Vec<i32>`,
rustc won't monomorphize it, and linking fails with undefined symbols.

## The chosen approach: Option B (opaque stubs, lazy discovery)

**Decision: confirmed and partially implemented.**

### Why opaque

We want toylang to have full control over its types. Rustc should NOT see toylang's
internal field structure. This gives toylang freedom to:
- Reorder fields for cache performance
- Add internal fields (refcounts, vtable pointers) without changing stubs
- Use types that don't exist in Rust's type system (conditional types, etc.)
- Control drop order independently of field declaration order

### How it works

1. **Stubs are opaque:** `pub struct ToyShip(());` — rustc doesn't see fields.
   Layout reports 0 fields; total size/align come from `monomorphize_type`.
2. **`monomorphize_type` provides field types for layout:** when `layout_of(ToyShip)`
   fires, the consumer returns `[Vec<i32>]` as the field types. The library computes layout.
3. **`monomorphize_fn` discovers type deps:** when the consumer monomorphizes a function
   that constructs `ToyShip`, it walks the struct's fields, discovers `Vec<i32>`, and
   returns it as a `rust_dep` alongside the function's function deps.
4. **Library emits phantom refs in MIR:** for function deps, the existing `ReifyFnPointer`
   mechanism. For type deps, a new mechanism: phantom locals declared with the type.

### How Rust accesses toylang struct fields: generated accessor methods

Because stubs are opaque, Rust code **cannot** directly access fields like `ship.wings`.
Instead, toylangc generates accessor methods in `impl` blocks within the stubs.

Accessor methods go through the monomorphization pipeline like any other function:
- Stub declares the method with an `unreachable!()` body
- `mir_built` intercepts it and builds a MIR call stub to the extern accessor symbol
- The LLVM backend generates the accessor (GEP to field offset, return pointer)

**Key design principles:**
- **Toylang stays UFCS.** Toylang itself has no `impl` blocks — all functions are
  top-level. The `impl` blocks in stubs are purely a Rust-facing presentation layer.
- **toylangc controls the Rust API shape.** Each function registered by toylangc can
  specify how Rust should see it:

```rust
enum RustPresentation {
    FreeFunction,                                    // pub fn foo(...)
    Method { on_type: String },                      // impl Foo { pub fn bar(&self, ...) }
    TraitMethod { on_type: String, trait_name: String }, // impl Trait for Foo { fn bar(...) }
}
```

For Milestone 2, only `Method` is needed (for field accessors). `FreeFunction` and
`TraitMethod` are future extensions for user-defined functions and trait conformance.

- **The .o file is always flat extern symbols.** Whether Rust sees `ship.wings()` or
  `get_wings(&ship)`, the linker resolves the same `__toylang_accessor_ToyShip_wings`
  symbol.

### The unverified part

**Does a phantom local trigger monomorphization?**

```
// In the MIR stub:
StorageLive(_6)     // _6: Vec<i32>
StorageDead(_6)
```

Does rustc's monomorphization collector see `Vec<i32>` in `local_decls` and add it to
its worklist? This needs testing. If it doesn't work, there's a fallback:

```
// Fallback: make the type "used" by flowing its size into the extern call
StorageLive(_6)                          // _6: [Vec<i32>; 0]
_7 = std::mem::size_of_val(&_6)         // always 0
_8 = Add(_phantom_sum, _7)              // still 0
StorageDead(_6)
// ... pass _phantom_sum as extra arg to extern call
```

Because `_6` is read by `size_of_val` and the result flows into the call, the collector
must treat `Vec<i32>` as "used."

**DO NOT use `mentioned_items` for this.** See `docs/historical/design-monomorphization-triggers.md`
— mentioned items go into `state.mentioned` (not guaranteed codegen), and the semantics
are explicitly documented as unstable.

## What remains to implement

### Next: Generate function stubs in __lang_stubs

**This is the immediate next step.** Currently, function stubs are declared by the user
in their Rust code:

```rust
// Current (user writes this):
fn make_counter() -> Counter { unreachable!() }
```

This should be generated in `__lang_stubs` instead:

```rust
// Generated in __lang_stubs:
pub fn make_counter() -> Counter { unreachable!() }
```

Benefits:
- User's Rust code is simpler (just calls the function, no stub needed)
- `mir_built` can add `is_from_lang_stubs` check, preventing name collisions
- Consistent: all consumer items live in `__lang_stubs`
- `borrowck` override can also use `is_from_lang_stubs`

### Next: Route generic accessors through monomorphization pipeline

**Current state (tech debt):** Generic struct accessors use inline Rust pointer math in
the stub (computing C-layout offsets at runtime with `size_of`/`align_of`). This bypasses
the monomorphization pipeline and duplicates layout logic.

**Target state:** All accessor methods (generic and non-generic) are generated with
`unreachable!()` bodies. `mir_built` intercepts them, `monomorphize_fn` returns the extern
symbol, and the LLVM backend generates the GEP-based accessor for each instantiation.

**Key design issue:** `mir_built` identifies functions by item name. An accessor method
named `first` on `Pair` just has item name `first`. To distinguish it from a regular
function, check `tcx.associated_item(def_id)` to see if it's a method, then examine the
parent impl's self type to get the struct name.

### Next: Phantom local monomorphization (for T(R) tests)

Test whether an unused local with a Rust generic type triggers monomorphization.
Needed for `test_t_of_r_vec_field` and all tests involving Rust types as struct fields.

### Remaining steps

1. **Generate function stubs in __lang_stubs** — stub_gen emits wrapper functions, tests
   remove redundant declarations, add `is_from_lang_stubs` to mir_built/borrowck
2. **Route all accessors through monomorphization** — remove inline pointer math, use
   `unreachable!()` bodies, handle in monomorphize_fn + LLVM backend
3. **Phantom local monomorphization** — verify mechanism, implement type dep discovery
   in `monomorphize_fn`, emit phantom type locals in `build_extern_call_body`
4. **Make T(R) tests pass** — `test_t_of_r_vec_field`, `test_t_of_r_layout`
5. **Make remaining tests pass** — deep nesting, mixed fields, etc.
6. **Fix test_tg_bool_i32** — bool literal codegen in LLVM gen

## Key files

### Library (`rustc-lang-facade/src/`)

| File | What it does | What changes for remaining work |
|------|-------------|------------------------------|
| `lib.rs` | `LangCallbacks` trait, vtable, `is_from_lang_stubs` | May need accessor-aware function matching |
| `mir_helpers.rs` | MIR body construction | Add phantom type local emission |
| `queries/layout.rs` | layout_of override (0-field FieldsShape) | No changes needed |
| `queries/mir_build.rs` | mir_built override | Add `is_from_lang_stubs` check, detect accessor methods |
| `queries/drop_glue.rs` | Drop glue override (uses `is_from_lang_stubs`) | No changes needed |
| `queries/borrowck.rs` | Borrowck skip | Add `is_from_lang_stubs` check |
| `codegen_wrapper.rs` | .o injection | No changes |
| `driver.rs` | Driver lifecycle | No changes |

### Consumer (`toylangc/src/`)

| File | What it does | What changes for remaining work |
|------|-------------|------------------------------|
| `toylang/parser.rs` | Toylang parser (supports struct/generic field types) | No changes needed |
| `toylang/registry.rs` | Data structures (has ToyStruct, RustGeneric variants) | No changes needed |
| `toylang/callbacks_impl.rs` | `impl LangCallbacks` | Handle accessor monomorphize_fn, type dep walking |
| `stub_gen.rs` | Opaque stubs + accessor methods + extern decls | Generate function stubs, switch accessor methods to unreachable!() |
| `llvm_gen.rs` | LLVM IR generation (handles nested structs) | Generate accessor functions for generic instantiations |
| `oracle.rs` | TyCtxt query helpers | No changes needed |

### Tests (`toylangc/tests/`)

| File | What it does |
|------|-------------|
| `integration_tests.rs` | All tests — 9 passing, 17 ignored. Tests use accessor methods. |

## Architecture constraints to know

### Why globals are necessary

Rustc's query overrides are plain function pointers — not closures. They can't capture
state. So the consumer's callbacks are stored in a global `Box<dyn Any>` with a manual
vtable of HRTB function pointers. This is necessary because `dyn LangCallbacks` isn't
allowed (generic `'tcx` methods break object safety). The vtable + trampoline approach
works around this. See `rustc-lang-facade/src/lib.rs` and
`docs/historical/library-split-implementation.md`.

**Implication for tests:** each test process can only call `run_compiler` once (the globals
are `OnceLock`). Integration tests work because each `#[test]` spawns toylangc as a
subprocess.

### Why ReifyFnPointer and not mentioned_items

The monomorphization collector has two sets: "used" (guaranteed codegen) and "mentioned"
(not guaranteed). ReifyFnPointer casts produce "used" items. `mentioned_items` produces
"mentioned" items with no stability guarantee. See
`docs/historical/design-monomorphization-triggers.md` for the full comparison.

### Why codegen happens after monomorphization

`generate_and_compile` is called from `LangCodegenBackend::codegen_crate`, AFTER
`inner.codegen_crate()` returns (which runs monomorphization as its first step). This
ensures the consumer has seen all `monomorphize_fn` callbacks and can compile with full
knowledge of what deps exist. `after_rust_analysis` runs earlier (before monomorphization)
and is for type checking only.

### Why `-C codegen-units=16`

The consumer's .o calls Rust generic instantiations by mangled symbol name. Rustc's
partitioner may give those instantiations internal linkage if all references are within
one CGU. Multiple CGUs force external linkage for cross-CGU references, making symbols
visible to the linker. See `docs/historical/design-codegen-integration.md`.

### The layout_of ADT-only filter

`layout_of` is called for EVERY type — `*mut Point`, `&Point`, `Option<Point>`, etc.
The override MUST filter to `TyKind::Adt` only. Intercepting derived types corrupts
their layouts and causes ICEs. This was a hard-won lesson from the early PoC.

### The layout_of module check (`is_from_lang_stubs`)

`layout_of` and `drop_glue` now verify that the type's DefId comes from the `__lang_stubs`
module, not just that the name matches. Without this, user-defined types sharing a name
with a toylang type get their layouts corrupted. This was discovered when switching to
0-field layouts — a user-defined `Point` struct's codegen crashed because the layout
said 0 fields while the struct had real fields rustc needed to access.

### Zero-field FieldsShape

The layout override reports `FieldsShape::Arbitrary` with 0 field offsets. Rustc treats
the type as a pure memory blob (via `BackendRepr::Memory { sized: true }`). The original
approach of reporting per-field offsets caused ABI computation crashes because rustc tried
to look up field types from the ADT definition (which only has dummy fields in the stub).
With 0 fields, rustc never tries to index into the ADT's fields.

### mir_shims vs mir_built: set_required_consts

- `mir_built` bodies: do NOT call `set_required_consts` / `set_mentioned_items`.
  The `mir_promoted` pass sets them automatically.
- `mir_shims` bodies: MUST call both (with empty vecs). `mir_promoted` doesn't run for
  shims. Getting this backwards causes "have already been set" panics.

## Running tests

```bash
# Build
cargo build

# Run passing tests
cargo test -p toylangc

# Run ALL tests including expected-fail ones
cargo test -p toylangc -- --ignored

# Run a specific test
cargo test -p toylangc --test integration_tests test_t_of_r_vec_field -- --ignored
```

## Suggested implementation order

1. Generate function stubs in `__lang_stubs` + update tests + add `is_from_lang_stubs` everywhere
2. Route all accessor methods through monomorphization (remove inline pointer math)
3. Verify phantom local monomorphization
4. Implement type dep discovery in `monomorphize_fn`
5. Emit phantom type locals in `build_extern_call_body`
6. Make T(R) tests pass (`test_t_of_r_vec_field`, `test_t_of_r_layout`)
7. Make remaining tests pass one by one, working up to deep nesting
