# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 116 integration tests + 4 standalone tests + 60 unit tests passing, 0 ignored.
> Minimal rustc fork with `per_instance_mir` query. Inkwell LLVM backend.
> Deep monomorphization walk — internal toylang functions never exposed to rustc.
> GLOBALS split into immutable `CONFIG` (OnceLock) + mutable `MUTABLE_STATE`
> (Mutex) to avoid a deadlock where query providers triggered during
> `generate_and_compile` tried to re-lock the mutex (see @GCMLZ). Unified
> `ResolvedType` everywhere. Explicit typed literals. Typed error enums.
> Full ABI coverage including ABI-coerced return types for function declarations
> (see @ACRTFDZ). Generic functions with explicit type args. Mutable assignment,
> else if, boolean operators (&&/||), unary negation.
>
> **Phases done:**
> - Phase 1: Explicit trait method calls (`Trait::method(receiver, args)`)
> - Phase 2: Rust free function calls via `use` imports
> - Phase 3: Byte string literals (`b"hello\n"`)
> - Phase 4: I/O integration (`Write::write_all(&stdout(), b"hello\n")`),
>   GLOBALS deadlock fix, ABI return type coercion, broadened TyKind handling
>   for `Option`/`Result`, enum support in `find_reexported_type`
> - Phase 5: `toylang.toml` manifest + `toylangc build` command
>   orchestrating cargo with `RUSTC_WORKSPACE_WRAPPER`; wrapper mode re-reads
>   the manifest from `CARGO_MANIFEST_DIR/..` instead of using an env var
>   side-channel (see @MRRIWMZ)
> - Phase 6 (step 1): `.unwrap()` on `Result`/`Option` via `#[inline(never)]`
>   wrappers in `__lang_stubs` that take the receiver by raw pointer. Wrapper
>   redirect lives in `oracle::redirect_to_wrapper`, hooked into both
>   dep-registration (`callbacks_impl::collect_toylang_fn_deps_inner`) and
>   codegen (`llvm_gen::get_or_resolve_rust_method`) — see @SMINCZ for why
>   both sites are required. Linkage is forced via a 14-line patch in the
>   forked `rustc_monomorphize/src/partitioning.rs` that gives every
>   `__lang_stubs` item `(Linkage::External, Visibility::Default)`. Also
>   fixed a pre-existing latent bug in `push_arg_for_rust_call` where
>   non-pair Direct(scalar) args were passed by pointer instead of value
>   (corrupted every `Vec::push(int)` since toylang's inception, never
>   noticed because no test read the stored value back).
>
> - Phase 6 (step 2): `visibility_override` callback replaces the
>   inline `__lang_stubs` string match in the rustc fork. The fork
>   exposes `rustc_monomorphize::partitioning::VISIBILITY_OVERRIDE_HOOK`
>   (a `OnceLock<fn ptr>`); the facade installs a bridge fn at startup;
>   toylang's impl walks DefPath data safely (per @DPSFDOZ). String
>   `__lang_stubs` no longer appears in the rustc fork.
> - Phase 6 (step 3): two-family callback split. `LangCallbacks` is now
>   `LangCallbacks: LangPredicates`, with state-taking methods on the
>   former and pure methods (including `visibility_override`) on the
>   latter. Predicate trampolines have no `&mut state` parameter, so
>   bridge fns for predicate hooks are structurally lock-free. The
>   "partitioner-time hooks may lock MUTABLE_STATE" exception in @GCMLZ
>   is dissolved — the type system enforces the rule now. See Part 2.6
>   for the family taxonomy.
>
> **Phase 6 complete.** Next milestones: Phase 7 (9 standalone test
> projects linking against rand, regex, uuid, clap, serde, toml, glob,
> indexmap, reqwest) and Phase 8 (test harness).
>
> **After Phase 6:** 9 standalone test projects linking against rand, regex,
> uuid, clap, serde, toml, glob, indexmap, reqwest (Phase 7); test harness
> (Phase 8).

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

### 2.5 visibility_override (CGU partitioner hook)

**Key:** `Instance<'tcx>` — per `MonoItem::Fn` during CGU partitioning.

Returns `Option<(Linkage, Visibility)>`. `None` defers to rustc's
default logic; `Some((linkage, vis))` forces an assignment and prevents
internalization. Used to keep symbols in the consumer's `__lang_stubs`
module visible to the externally-linked consumer .o file.

Unlike the four query-provider hooks above, this one isn't a query
override. The rustc fork exposes a `OnceLock<fn ptr>` static
(`rustc_monomorphize::partitioning::VISIBILITY_OVERRIDE_HOOK`) that the
facade fills at startup with a bridge fn. The fork itself knows nothing
about the consumer. See §10.6.4 for the design.

### 2.6 The two callback families

The five hooks split into two trait families based on whether they
need consumer state:

**`LangPredicates`** (pure; no state, no lock):
- `is_consumer_type`, `is_consumer_fn` — name predicates.
- `generate_stubs` — produces the injected stub source once at startup.
- `visibility_override` — partitioner-time linkage decision.

**`LangCallbacks: LangPredicates`** (stateful; takes `&mut dyn Any`,
locks `MUTABLE_STATE`):
- `create_state` — constructor for the consumer state box.
- `monomorphize_type`, `monomorphize_fn` — produce per-instantiation
  data; mutate state during the deep dependency walk.
- `after_rust_analysis` — validation after rustc's analysis phase.
- `generate_and_compile` — runs the consumer's LLVM backend; holds the
  lock for the entire duration of consumer codegen.

The split is enforced by signature: predicate trampolines have no
`&mut (dyn Any + Send + Sync)` parameter, so a hook in the predicate
family literally cannot acquire the `MUTABLE_STATE` lock — it has no
state to pass to a lock-acquiring helper. New hooks pick a family
based on whether they need state; that choice surfaces the locking
story up-front instead of leaving it to a prose invariant. See
@GCMLZ for the locking history this split replaced.

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
   - Uses `resolved_to_inkwell` for types (toylang ABI, not Rust ABI)

2. **Extern wrapper** (`__toylang_impl_{name}`) — thin wrapper matching Rust ABI:
   - Calls the internal function
   - Adapts return/params to match `fn_abi_of_instance`
   - Uses `parse_coerced_type` from ABI-coerced strings for types (see @ACRTFDZ)

Internal-only toylang functions generate only the internal function (no wrapper).
Toylang-to-toylang calls use `__toylang_internal_` symbols directly.

### 4.6.1 ABI-coerced return types for Rust function declarations (@ACRTFDZ)

When toylang calls a Rust function (MethodCall, StaticCall, or use-imported
FnCall), the LLVM function declaration must use rustc's ABI-coerced return type,
NOT `resolved_to_inkwell` (toylang's type representation). For an 8-byte struct
like `Stdout`, rustc returns `i64` (Direct scalar) in register `x0` on aarch64,
but `resolved_to_inkwell` produces `[8 x i8]` (LLVM aggregate). LLVM reads
aggregates from memory, scalars from registers — the mismatch produces garbage.

Fix: all three Rust call paths use `parse_coerced_type(coerced_ret)` for the
declaration. When the ABI type differs from toylang's type, the FnCall path
stores the returned value into an alloca and returns `ExprResult::Ptr` so
downstream code loads it as the toylang type (a type-punning bitcast via
memory). The extern wrapper does the same for `internal_sret`-to-Rust-direct
adaptation, and the param coercion path uses store-through-pointer for
`Rust i64 → internal { i32, i32 }` reassembly.

This is the reason `stdout()` segfaulted before Phase 4 even after the sret
fix: the declaration said `[8 x i8]` but the function returned `i64`, so the
Stdout alloca contained garbage that `Write::write_all` then dereferenced.

### 4.7 Toylang-owned main

When toylang defines `fn main()`, the stub wrapper is renamed to `__toylang_main`
to avoid conflicting with Rust's `main`. The mapping flows through:
- `is_consumer_fn` returns true for both `"main"` and `"__toylang_main"`
- `monomorphize_fn` maps `"__toylang_main"` → `"main"` for registry lookup
- `compute_fn_symbol` → extern symbol `__toylang_impl_main`

### 4.8 `use` imports

Toylang supports `use` statements: `use std::alloc::Global`. The parser stores
the path in `registry.imports`. The stub generator emits `pub use` in
`__lang_stubs.rs`. Three oracle functions find re-exports via
`module_children_local`:

- `find_reexported_type` — matches `DefKind::Struct` and `DefKind::Enum`
  (the latter added in Phase 4 so `Option` and `Result` work). Called by
  `find_rust_type_def_id`.
- `find_use_imported_trait_def_id` — matches `DefKind::Trait`. Used by trait
  method resolution for `Trait::method(receiver, args)` syntax (Phase 1).
- `find_use_imported_fn_def_id` — matches `DefKind::Fn`. Used by FnCall
  use-import path for free function calls like `stdout()` (Phase 2).

### 4.9 Trait method calls (Phase 1)

Toylang calls trait methods via explicit trait qualification using the existing
`StaticCall` AST node:

```
use std::io::Write
fn main() {
    let out = stdout();
    Write::write_all(&out, b"hello\n")
}
```

The type resolver distinguishes trait calls from inherent calls by checking if
the name resolves to `find_use_imported_trait_def_id`. For trait calls, it uses
a `__trait::` prefix convention when calling back into the oracle
(`rust_method_ret("__trait::Write", "write_all", [&Stdout])`), and the oracle
returns via `rust_trait_method_return_type`.

For codegen, `get_or_resolve_rust_method` uses the trait definition's method
`DefId` with `[Self, ...]` args (per @TVIMDGAZ). `Instance::expect_resolve` maps
from the trait-level DefId to the concrete impl at monomorphization time, so
default trait methods like `Write::write_all` work automatically.

### 4.10 Rust free function calls (Phase 2)

Use-imported free functions like `stdout()` go through the FnCall use-import
path. The distinction from regular FnCall: if `registry.functions` has no
entry, we look up the function via `find_use_imported_fn_def_id` and call the
real Rust function with rustc's ABI. If the function has no body but IS in the
registry (extern declaration), same path.

The FnCall path queries `coerced_return_type_for_instance` and
`coerced_param_types_for_instance` to build the LLVM declaration. It handles
sret (returns `ExprResult::Ptr` with an alloca), ScalarPair args (splits a fat
pointer into two args), and ABI return type coercion via store-through-pointer
reinterpretation (@ACRTFDZ).

### 4.11 Byte string literals (Phase 3)

`b"hello\n"` produces a `&[u8]` (fat pointer: ptr + len). The lexer recognizes
the `b"` prefix, supports escape sequences (`\n`, `\t`, `\\`, `\0`, `\"`), and
produces `Token::ByteStringLit(Vec<u8>)`. The type is
`ResolvedType::Ref { inner: ByteSlice }`.

Codegen emits a global constant byte array and constructs a fat pointer struct
`{ ptr, i64 }`. When passed to a Rust function that takes `&[u8]`, the
`CoercedParam::Pair("ptr", "i64")` ABI requires splitting the struct into two
LLVM args. `push_arg_for_rust_call` handles this for MethodCall/StaticCall;
the FnCall path inlines the same ScalarPair splitting logic.

The `CoercedParam::Pair` variant was added to `abi_helpers.rs` in Phase 3 —
previously ScalarPair was incorrectly collapsed into a single
`Direct("{ ptr, i64 }")` param, producing a latent ABI bug that didn't trigger
until byte strings existed.

### 4.12 Broadened TyKind handling (Phase 4)

`rustc_ty_to_resolved_type` formerly panicked on anything outside a narrow set
(i32/i64, usize, f64, bool, unit, Ref, Adt, `[u8]` slice). Phase 4 broadened
it to handle:

- Unsupported int/uint/float widths (i8, u8, u16, u32, u64, f32, etc.) → opaque
  `RustType` with the primitive's name
- `TyKind::Str`, `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple` → opaque
  `RustType` with a stable name

These types pass through as opaque values — toylang never inspects them. They
surface as generic type arguments of Rust types (e.g., `Option<u8>`,
`HashMap<String, i32>` internals).

`resolved_to_rustc_ty` gained a reverse mapping in the `RustType` arm: names
`u8`/`u16`/`u32`/`u64`/`i8`/`i16`/`f32` map back to `tcx.types.*` for
round-tripping. Other opaque names still panic if passed here (acceptable —
these shouldn't reach codegen).

---

## Part 5: Global State and Threading

### 5.1 Split into immutable config and mutable state (@GCMLZ)

Facade state is split into four statics, each with the minimum synchronization
necessary:

```
CONFIG:              OnceLock<FacadeConfig>                // immutable
DEFAULT_LAYOUT_OF:   OnceLock<LayoutOfFn>                  // immutable
DEFAULT_MIR_SHIMS:   OnceLock<MirShimsFn>                  // immutable
DEFAULT_SYMBOL_NAME: OnceLock<SymbolNameFn>                // immutable
MUTABLE_STATE:       OnceLock<Mutex<FacadeMutableState>>   // mutable
```

`FacadeConfig` (set once during `install_callbacks`, never changes):
- `callbacks: Box<dyn Any>` — the type-erased `ToylangCallbacks`
- `vtable: CallbackVtable` — HRTB function pointers for dispatch

`FacadeMutableState` (locked only by callbacks that need `&mut state`):
- `consumer_state: Box<dyn Any>` — the type-erased `ToylangState`
- `lang_obj_path: Option<PathBuf>` — compiled .o path for link injection

Default query providers live in their own `OnceLock` statics, read without
locking during query provider fallthroughs.

### 5.2 Why the split (@GCMLZ)

The previous design had a single `Mutex<FacadeGlobals>` guarding everything.
`call_generate_and_compile` held that mutex for the entire consumer codegen. During
codegen, the consumer makes tcx queries like `tcx.symbol_name(stdout)`. These
trigger our query providers (`lang_symbol_name`), which fell through to
`default_symbol_name()` — which tried to re-lock the same non-reentrant mutex →
**deadlock**.

Existing tests (Vec, Clone, etc.) avoided this by luck: their symbol names were
cached from `inner.codegen_crate` before `generate_and_compile` began. `stdout`
was the first uncached case, and it deadlocked silently (0% CPU, hang forever).

The fix: immutable config moves to lock-free `OnceLock` statics. Query providers
reading config/defaults never touch `MUTABLE_STATE`, so they can freely execute
during `generate_and_compile`.

Residual risk (documented but not currently hit): if a query provider calls
`call_monomorphize_fn` for an uncached consumer item during
`generate_and_compile`, it would try to lock `MUTABLE_STATE` and deadlock.
This is prevented in practice because all consumer items are cached during
`inner.codegen_crate`.

### 5.3 Locking protocol

| Function | Reads | Locks |
|----------|-------|-------|
| `is_consumer_type` / `is_consumer_fn` | `CONFIG` | none |
| `default_layout_of` / `default_mir_shims` / `default_symbol_name` | `DEFAULT_*` | none |
| `call_monomorphize_type` / `call_monomorphize_fn` | `CONFIG` | `MUTABLE_STATE` |
| `call_after_rust_analysis` / `call_generate_and_compile` | `CONFIG` | `MUTABLE_STATE` |
| `set_lang_obj_path` / `get_lang_obj_path` | — | `MUTABLE_STATE` (brief) |

This ensures single-threaded toylang code execution even when rustc's query
providers fire on Rayon worker threads. Query providers reading only config
are lock-free; only callbacks that need `&mut consumer_state` serialize.

### 5.4 Reentrancy avoidance

`generate_and_compile` calls `generate_with_tcx` which calls
`callbacks.monomorphize_fn_inner()` — this is NOT the trait method (which would
re-lock). It's a direct method on `ToylangCallbacks` that takes `&mut ToylangState`
as a parameter, bypassing the mutex entirely.

### 5.5 Consumer state (`ToylangState`)

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

### 5.6 Callback log

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

## Part 6: What Works (60 unit + 116 integration + 4 standalone = 180 tests)

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
- Reference expressions (`&expr`)

### ABI
- Internal/extern ABI split
- Struct sret, param coercion, bool i1↔i8, ScalarPair
- `PassMode::Indirect` (byval) handled as Indirect
- ABI-coerced return types for Rust function declarations (@ACRTFDZ)
- `#[track_caller]` hidden Location parameter appended at all 4 call sites (@TCHAPZ)
- Trait method resolution via `[Self, ...]` args on trait DefId (@TVIMDGAZ)

### Rust interop
- Any Rust type (Vec, Option, Result, etc.) with explicit type args
- Any inherent method (signatures queried from rustc)
- Trait method calls via `Trait::method(receiver, args)` explicit qualification
- Rust free function calls via `use` imports (`use std::io::stdout` → `stdout()`)
- `use` imports for structs, enums, traits, free functions
- Byte string literals (`b"hello\n"` → `&[u8]`)
- I/O without glue: `Write::write_all(&stdout(), b"hello\n")`
- Drop glue

### Type broadening (Phase 4)
- `Option<T>` and `Result<T, E>` — enum lookup in `find_reexported_type`
- Primitive types as generic args (u8, u16, u32, u64, i8, i16, f32)
  pass through as opaque `RustType` values and round-trip back to `tcx.types.*`
- `Str`, `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple` as opaque passthrough

### Deep monomorphization
- Internal toylang functions not exposed to rustc
- Deep chains (a→b→c→Rust), diamond patterns, shared callees
- Generic deep walks with type substitution
- Two entry points sharing internal functions
- `visited_symbols` prevents redundant walks across calls

### Validation
- Parser: duplicate names, reserved `__toylang_` prefix, all error variants tested
- Type resolver: 13 error variants (including `ArgTypeMismatch` from Phase 2),
  all tested
- `after_rust_analysis`: 5 validation checks

### Build orchestration (Phase 5)
- `toylangc build` reads `toylang.toml` and produces a working binary with
  arbitrary crates.io dependencies
- Three-mode dispatch in `main.rs`: build / wrapper / direct (see §10.5)
- Wrapper mode re-reads manifest from `CARGO_MANIFEST_DIR/..` instead of
  using a `TOYLANG_INPUT` env var (see @MRRIWMZ)
- Dependency crates compile via `rustc_driver::RunCompiler` with
  `NoopCallbacks`; only the primary crate (`CARGO_PRIMARY_PACKAGE=1`) goes
  through toylang processing
- 4 standalone tests: minimal project, project with Rust dep, invalid
  manifest, missing source

---

## Part 7: Key Files

### Library (`rustc-lang-facade/src/`)

| File | Purpose |
|------|---------|
| `lib.rs` | `LangPredicates` + `LangCallbacks: LangPredicates` traits, split globals (`CONFIG`, `MUTABLE_STATE`, `DEFAULT_*` — see @GCMLZ), `PredicateVtable` + `StatefulVtable` + their trampolines, `facade_visibility_override` bridge fn (lock-free, see @GCMLZ), `is_from_lang_stubs` |
| `queries/layout.rs` | layout_of override (reads CONFIG/DEFAULT_LAYOUT_OF without lock per @GCMLZ) |
| `queries/per_instance.rs` | per_instance_mir provider |
| `queries/symbol_name.rs` | symbol_name override (reads CONFIG/DEFAULT_SYMBOL_NAME without lock per @GCMLZ) |
| `queries/drop_glue.rs` | Drop glue (mir_shims) override (reads CONFIG/DEFAULT_MIR_SHIMS without lock per @GCMLZ) |
| `queries/mod.rs` | Query override installation (4 providers) |
| `abi_helpers.rs` | ABI coercion helpers: `CoercedReturn`, `CoercedParam` (includes `Pair` variant for ScalarPair, see @ACRTFDZ); hidden `#[track_caller]` param (see @TCHAPZ) |
| `mir_helpers.rs` | Drop glue MIR builder |
| `codegen_wrapper.rs` | CodegenBackend wrapper, .o injection |
| `driver.rs` | `run_compiler` entry point |
| `file_loader.rs` | Stub injection via FileLoader |

### Consumer (`toylangc/src/`)

| File | Purpose |
|------|---------|
| `llvm_gen.rs` | Inkwell LLVM backend: instance discovery, two-pass codegen, Rust method resolution, FnCall use-import path with ABI return coercion (@ACRTFDZ) |
| `stub_gen.rs` | Generates `__lang_stubs.rs` |
| `oracle.rs` | TyCtxt query helpers (struct + enum lookup, trait + free function lookup), type conversion with broadened TyKind handling, symbol mangling |
| `main.rs` | CLI entry point — three-mode dispatch (build / wrapper / direct), `NoopCallbacks` pass-through for dep crates |
| `build.rs` | `toylangc build` — generates `.toylang-build/` Cargo project, spawns `cargo +rustc-fork build` (Phase 5, read site 1 of @MRRIWMZ) |
| `manifest.rs` | `toylang.toml` parser — `Manifest`/`Project`/`DepSpec` structs, `parse()` via `toml::from_str` |
| `toylang/ast.rs` | Untyped AST (Expr, Stmt incl. Assign, Block, BinOp incl. And/Or, UnaryNeg, Ref, ByteStringLit) |
| `toylang/typed_ast.rs` | `ResolvedType` enum (incl. ByteSlice, Ref), `TypedBlock`, `TypedStmt` incl. Assign |
| `toylang/type_resolve.rs` | Type annotation pass, `TypeResolveError` (13 variants incl. ArgTypeMismatch) |
| `toylang/parser.rs` | Parser — `ParseError` variants, byte string lexing, precedence: `\|\|` < `&&` < comparison < additive < multiplicative |
| `toylang/registry.rs` | `ToyStruct`, `ToyFunction`, `ToyParam` |
| `toylang/callbacks_impl.rs` | `LangCallbacks` impl, `ToylangState`, deep monomorphization walk, `CallbackLog`, trait + free fn dep collection |

---

## Part 8: Architecture Decisions

### Why deep monomorphization walk

Previously, `collect_toylang_fn_deps` reported toylang callees to rustc, causing
rustc to process internal functions through `per_instance_mir` / `symbol_name`.
The deep walk eliminates this: `collect_toylang_fn_deps_inner` recursively walks
toylang callees, only returning Rust deps. Internal functions are stashed in
`ToylangState.toylang_instances` for direct codegen. Each function body is walked
exactly once via `visited_symbols`.

### Why split globals (immutable OnceLock + mutable Mutex)

Rustc's query providers fire on Rayon worker threads. The original design used
a single `Mutex<FacadeGlobals>` to serialize all consumer code. This worked
until Phase 4's `stdout()` test, which triggered a deadlock:
`call_generate_and_compile` held the mutex while consumer codegen ran; codegen
called `tcx.symbol_name(stdout)`; our `lang_symbol_name` provider tried to
call `default_symbol_name()` which tried to re-lock the same mutex.

The fix splits state by mutability: immutable config (callbacks, vtable,
default providers) goes in `OnceLock` statics (no locking needed for reads);
mutable state (consumer_state, lang_obj_path) stays behind a Mutex. Query
providers reading only config are lock-free, so they can execute during
`generate_and_compile` without deadlock. See @GCMLZ.

The Mutex on `consumer_state` still serializes all callbacks that need `&mut`
access, preserving the single-threaded execution guarantee for toylang code.

### Why ABI-coerced return types for Rust function declarations

When declaring an LLVM function that will be called as a Rust function, the
return type must match rustc's ABI coercion, not toylang's representation.
For an 8-byte struct like `Stdout`, rustc returns `i64` (Direct scalar in
register `x0`), but toylang's `resolved_to_inkwell` produces `[8 x i8]` (LLVM
aggregate in memory). LLVM uses different code paths for the two — declaring
the wrong one produces garbage return values.

Phase 4 fixed this in all three Rust call paths (MethodCall, StaticCall,
FnCall use-import) by using `parse_coerced_type(coerced_ret)` for the
declaration. When the ABI type differs from the toylang type, codegen stores
the return value through an alloca to reinterpret the bits (type-punning
bitcast via memory). See @ACRTFDZ.

Internal toylang-to-toylang calls still use `resolved_to_inkwell` because
their ABI is fully owned by toylang — no rustc coercion applies.

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

## Part 10: Phases Completed and Planned

### 10.1 Done: Phase 1 — Explicit trait method calls

`Trait::method(receiver, args)` syntax via `StaticCall`. The oracle resolves
the trait DefId via `find_use_imported_trait_def_id`, finds the method in the
trait's associated items, and builds args as `[Self, ...]` on the trait
definition's DefId (@TVIMDGAZ). `Instance::expect_resolve` maps to the
concrete impl at monomorphization time.

Fixed a latent `#[track_caller]` ABI bug along the way (@TCHAPZ): ~43 Vec
methods have a hidden `&Location` pointer param that must be appended at every
call site. Previously absent → undefined behavior that happened not to trigger
in existing tests.

Tests: `test_trait_static_call_clone_vec`, `test_trait_static_call_result_discarded`,
`test_ref_expr_basic`, + regression coverage.

### 10.2 Done: Phase 2 — Rust free function calls

Use-imported free functions like `stdout()`. Added `find_use_imported_fn_def_id`
for `DefKind::Fn` re-exports, `rust_free_fn_return_type` /
`rust_free_fn_param_types` for the FnCall path, plus `ArgTypeMismatch` error
variant and `types_match()` for semantic `StructRef` vs `Struct` equivalence.

FnCall dispatch restructured to handle both extern declarations and
use-imported free functions with real type args. All `instantiate_identity()`
call sites got comments explaining why structural inspection is safe there.

Tests: 12 new, covering arg type checking, free function resolution, and
existence sentinel (`Option::None` = "not found", `Some(vec![])` = "found,
takes no args").

### 10.3 Done: Phase 3 — Byte string literals

`b"hello\n"` → `&[u8]` fat pointer `{ ptr, i64 }`. The lexer recognizes the
`b"` prefix and handles escape sequences. In LLVM codegen, a global constant
byte array is allocated and wrapped in a fat pointer struct.

Fixed a latent `ScalarPair` ABI bug: previously ScalarPair was collapsed into
`Direct("{ ptr, i64 }")` (one LLVM param), but rustc's ABI wants two separate
params (ptr + i64). Added `CoercedParam::Pair(String, String)` variant. No
existing code exercised it because no existing code used `&[u8]`.

The FnCall path also gained ABI-correct declarations: previously it built LLVM
function decls from toylang-internal types, causing silent data corruption for
extern C functions. Both `FnCall` paths (extern-declared and use-imported) now
query `coerced_param_types_for_instance`.

Tests: 9 new, covering byte string parsing, type resolution, and ScalarPair ABI.

### 10.4 Done: Phase 4 — I/O integration, GLOBALS split, ABI coercion

`Write::write_all(&stdout(), b"hello\n")` works end-to-end. Implementation
required four distinct fixes:

1. **GLOBALS deadlock fix (@GCMLZ).** Split the single `Mutex<FacadeGlobals>`
   into `CONFIG: OnceLock<FacadeConfig>` (immutable), `DEFAULT_*: OnceLock<fn>`
   (immutable), and `MUTABLE_STATE: OnceLock<Mutex<FacadeMutableState>>`
   (mutable only). Query providers reading only config are lock-free, so they
   can execute during `generate_and_compile` without deadlock.

2. **sret handling in FnCall use-import path.** `stdout()` returns `Stdout` —
   rustc may return it via sret (indirect) depending on size. Added
   `coerced_return_type_for_instance` query and handling for all three modes
   (Direct, Indirect, Void), following the pattern from
   `get_or_resolve_rust_method`.

3. **ABI-coerced return types (@ACRTFDZ).** The FnCall path declared LLVM
   functions with `resolved_to_inkwell` (toylang's `[8 x i8]` for Stdout), but
   rustc returns `i64` (Direct scalar). LLVM treats aggregate vs scalar returns
   differently → garbage return values → segfault when later dereferenced.
   Fixed by using `parse_coerced_type(coerced_ret)` in the declaration, plus
   store-through-pointer reinterpretation when ABI type differs from toylang
   type.

4. **Broadened TyKind handling.** `rustc_ty_to_resolved_type` now handles
   previously-unsupported types (`u8`/`u16`/etc. as opaque `RustType`, `Str`,
   `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple`). `find_reexported_type`
   matches `DefKind::Enum` (fixes `Option`/`Result`). `resolved_to_rustc_ty`
   maps primitive type names back to `tcx.types.*` for round-tripping.

Tests: 6 new — `test_stdout_call`, `test_stdout_write_all`,
`test_stdout_multiple_writes`, `test_write_all_result_bound`,
`test_vec_pop_returns_option`, `test_rust_fn_returning_option_u8`.

### 10.5 Done: Phase 5 — toylang.toml and build orchestration

`toylangc build` reads `toylang.toml` and produces a working binary that can
depend on arbitrary Rust crates — no hand-written `main.rs`, no linker flags,
no knowledge of rustc plumbing on the user's side.

```
toylang.toml + main.toylang
         ↓
  toylangc build                         ←  build mode (manifest read #1)
         ↓
  generates .toylang-build/
    ├─ Cargo.toml                        (from [rust-dependencies])
    ├─ src/main.rs                       (auto-generated shim)
    └─ rust-toolchain.toml               (pins rustc-fork)
         ↓
  cargo +rustc-fork build
    with RUSTC_WORKSPACE_WRAPPER=<self>
    and  DYLD_LIBRARY_PATH / LD_LIBRARY_PATH set
         ↓
  Dependency crates compile via rustc_driver::RunCompiler
    with NoopCallbacks (no toylang processing)
  Primary crate compiles through toylangc wrapper mode
    gated by CARGO_PRIMARY_PACKAGE=1
    re-reads ../toylang.toml to locate .toylang source  ← manifest read #2
         ↓
  .toylang-build/target/debug/<binary>
```

`toylangc` operates in three modes:
1. **Build mode** (`argv[1] == "build"`): parses manifest, generates
   `.toylang-build/`, spawns cargo.
2. **Wrapper mode** (`argv[1]` is a path ending in `rustc`): cargo's
   `RUSTC_WORKSPACE_WRAPPER` protocol. If `CARGO_PRIMARY_PACKAGE` is set,
   re-reads `toylang.toml` one directory up from `CARGO_MANIFEST_DIR` and
   compiles through the existing toylang flow; otherwise passes through to
   plain rustc via `rustc_driver::RunCompiler::new(args, &mut NoopCallbacks)`.
3. **Direct mode** (`--toylang-input <path>`): existing behavior, unchanged —
   used by integration tests.

This follows the Clippy/Miri pattern. Dependencies compile with real rustc;
only the primary crate goes through toylangc. See @MRRIWMZ for why the
manifest is re-read in wrapper mode instead of carrying the source path via
a `TOYLANG_INPUT` env var.

The generated `Cargo.toml` includes an empty `[workspace]` table to mark
itself as its own workspace root, preventing cargo from walking up into a
parent workspace if the user's project happens to sit inside one (e.g.,
checked-in test projects under `toylangc/tests/standalone/*/`).

**Why `[rust-dependencies]` not `[dependencies]`?** Leaves room for
`[toylang-dependencies]` when toylang has its own package ecosystem.

**Why a separate manifest instead of Cargo.toml?** Toylang controls the UX.
Cargo is a tool in toylang's toolbox, not the other way around. If toylang
moves away from rustc someday, the user-facing contract doesn't change.

See `docs/historical/plan-phase5-toylang-toml-build.md` for the full
implementation history.

### 10.6 Done: Phase 6 — Wrappers for inline stdlib methods

**Status**: All three steps done. 116 integration + 60 unit + 4
standalone = 180 tests passing, 0 ignored. Step 1: `#[inline(never)]`
wrappers in `__lang_stubs` + rustc-fork partitioner patch. Step 2:
`visibility_override` callback replaces the inline `__lang_stubs` string
match in the fork. Step 3: two-family trait split (`LangPredicates` +
`LangCallbacks: LangPredicates`) dissolves the "partitioner-time lock"
exception; also delivered via this refactor rather than the naming-only
approach originally planned. Tech debt #6 (FnCall CoercedParam dispatch)
and the `toy_*` → `lang_*` rename also landed alongside.

#### 10.6.1 The problem

`Option::unwrap` and `Result::unwrap` are `#[inline(always)]`. Rustc never
emits a callable symbol for them — they exist only as inlined IR at every
Rust call site. Toylang compiles separately via Inkwell into `.o` files
that rustc later links against; those `.o` files reference Rust by mangled
symbol name. For inline-only methods, the symbol toylang declares
(`extern "C" fn ..._unwrap(...)`) doesn't exist anywhere, and the linker
fails with `undefined symbol`.

The same blocker applies to ~100+ other inline stdlib functions and to
`#[track_caller]` functions (whose hidden ABI parameter can't be supplied
from external IR — see @TCHAPZ).

Two prior attempts hit different failure modes. Their writeups are in
`docs/historical/phase6-attempt1-mono-not-generated.md` and
`docs/historical/phase6-attempt2-linkage-visibility.md`. The full design
plan (now superseded by what's implemented) is at
`docs/historical/plan-phase6-unwrap-wrappers-and-partitioner.md`.

#### 10.6.2 Solution shape

Generate a non-inline wrapper inside `__lang_stubs` for each blocked
method. The wrapper:

```rust
#[inline(never)]
pub unsafe fn __toylang_option_unwrap<T>(o: *mut core::option::Option<T>) -> T {
    core::ptr::read(o).unwrap()
}
```

Three load-bearing details:

1. **`#[inline(never)]` is mandatory.** Without it, rustc may inline the
   wrapper itself, putting us back at "no callable symbol." This is enforced
   as LLVM `noinline`, not a hint.
2. **Receiver is `*mut`, not `T` by value.** This sidesteps ABI
   complications: for any T, the wrapper's first param is just a pointer
   (Direct(ptr)), which matches toylang's existing MethodCall convention
   of passing `recv_ptr` as the first call arg. A by-value wrapper would
   force toylang to mirror rustc's PassMode for `Option<T>` (Pair, Direct,
   or Indirect depending on T) at every call site.
3. **`ptr::read` consumes the value.** Toylang doesn't track moves and
   doesn't run drop glue, so this is sound for the simple T's we use today
   (i32, u8). For wrappers around methods that consume self of types with
   destructors, this design needs revisiting.

Toylang dispatches `o.unwrap()` to the wrapper via a redirect helper:

```rust
// oracle::redirect_to_wrapper
pub fn redirect_to_wrapper<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[ResolvedType],
) -> Option<(DefId, ty::GenericArgsRef<'tcx>)>
```

Called from BOTH `callbacks_impl::collect_toylang_fn_deps_inner` (the
dep-registration site that drives codegen) AND
`llvm_gen::get_or_resolve_rust_method` (the symbol-string consumer). Both
sites must produce the same Instance so the symbol the wrapper's body gets
mangled with matches the symbol the LLVM IR declares.

#### 10.6.3 Why both call sites are required (@SMINCZ)

`tcx.symbol_name(instance)` and `Instance::expect_resolve(...)` are pure
read queries. They return a v0-mangled string and a typed handle; they
do NOT cause rustc to emit code for that Instance. The first attempt
treated these as if they did, and produced clean compiles + broken links.

Codegen is driven by rustc's mono collector walking ReifyFnPointer casts
inside MIR bodies (`rustc_monomorphize/src/collector.rs:709-717`). The
facade's `per_instance_mir` synthesizes a MIR body for each toylang
function whose only purpose is to mention each Rust dep as a
ReifyFnPointer (`rustc-lang-facade/src/queries/per_instance.rs:106-173`).
Anything pushed into `rust_deps` becomes a ReifyFnPointer; the mono
collector promotes it to `used_items` and rustc emits the symbol.

So: dep registration in `collect_toylang_fn_deps_inner` is the codegen
trigger. The `tcx.symbol_name` call in `llvm_gen` is a downstream
consumer that only works if the matching dep was already registered.
Skipping the dep-registration call but keeping the codegen call is the
canonical Phase 6 trap. This is documented as @SMINCZ in
`docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md`.

#### 10.6.4 Forcing external linkage on the wrapper

Even with the wrapper instantiated and codegen'd, the second attempt
discovered that rustc's CGU partitioner internalized the symbol
(`-Zprint-mono-items=lazy` showed `[Internal]`). For an executable crate
(`local_crate_exports_generics() == false`), generic `#[inline(never)]`
items default to `Visibility::Hidden + can_be_internalized = true`. Since
the wrapper's only user — the synthesized MIR body of `__toylang_main` —
landed in the same CGU as the wrapper itself, `internalize_symbols`
flipped the wrapper to `Linkage::Internal`. Internal-linkage symbols are
invisible to externally-linked `.o` files; the linker fails again.

The fix has two halves: a small hook in the rustc fork, and a
consumer-side callback on `LangCallbacks` that the hook calls into.

**Rustc fork** (`rustc_monomorphize/src/partitioning.rs`) — exposes a
`pub static VISIBILITY_OVERRIDE_HOOK: OnceLock<fn ptr>` (signature
`for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> Option<(Linkage, Visibility)>`).
`mono_item_linkage_and_visibility` calls the hook (if registered) right
after the `explicit_linkage` fast-path; if it returns `Some`, sets
`*can_be_internalized = false` and returns the override. Knows nothing
about `__lang_stubs` or any other consumer-specific name.

**Facade** (`rustc-lang-facade/src/lib.rs`) — adds `visibility_override`
to the `LangPredicates` trait (with default impl returning `None`).
Predicate trampolines do not take `&mut dyn Any state`, so the bridge
fn `facade_visibility_override` is structurally lock-free — it dispatches
through `PredicateVtable` and never touches `MUTABLE_STATE`. See @GCMLZ
for the trait-family split that enforces this.

**Toylang** (`toylangc/src/toylang/callbacks_impl.rs`) — implements
`visibility_override` by walking `tcx.def_path(instance.def_id()).data`
looking for `DefPathData::TypeNs("__lang_stubs")`. Returns
`Some((External, Default))` for matches, `None` otherwise.

Why `Visibility::Default` is sufficient on its own: the internalization
candidate set at `partitioning.rs:254` is built only from items that have
both `Visibility::Hidden` AND `can_be_internalized = true`. Returning
`Default` fails the first conjunct, so the wrapper never enters the
candidate set; no later pass can downgrade it. The
`*can_be_internalized = false` is defense-in-depth for documentation.

The DefPath walk uses `tcx.def_path(def_id).data`, NOT `tcx.def_path_str`.
`def_path_str` is implemented in terms of `trimmed_def_paths` and ICEs
during normal (non-diagnostic) compilation. This is documented as @DPSFDOZ
in `docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md` — the existing
facade `is_from_lang_stubs` uses `def_path_str` and is safe only because
its callers happen to live inside `generate_and_compile`. The partitioner
runs outside `generate_and_compile`, so toylang's `visibility_override`
inlines the safe walk instead of calling `is_from_lang_stubs`.

The check applies uniformly to generic and non-generic items. Per the
project invariant "non-generic is the degenerate case of generic," there
is no `is_generic` branch. Future non-generic items in `__lang_stubs`
(accessor wrappers, static tables, anything) will get the same treatment.

**Accessor wrappers are structurally immune** without needing a separate
`visibility_override` case. The `__toylang_accessor_*` methods generated
by `stub_gen.rs` live in impls on consumer types inside `__lang_stubs`,
so they're already covered by the blanket DefPath check above. Belt and
suspenders, though: `lang_symbol_name` (`queries/symbol_name.rs`)
intercepts accessor instances via `is_consumer_accessor_pub` and rewrites
their symbols to toylang-mangled names (`__toylang_accessor_<struct>_<field>`)
before the partitioner ever sees the original rustc-mangled name. By the
time partitioning runs, accessor callers reference an external-looking
toylang symbol. The DefPath check in `visibility_override` is the backstop
if symbol-name redirection is ever bypassed. No action needed; documented
here so future devs don't re-derive it.

Currently only `MonoItem::Fn` is forwarded to the hook. `MonoItem::Static`
and `MonoItem::GlobalAsm` are skipped (toylang doesn't emit either into
`__lang_stubs`). Widen the hook signature to take `&MonoItem<'tcx>` if a
future consumer needs them.

#### 10.6.5 Why this approach beat the alternatives

Considered and rejected:

- **Make ReifyShim WeakODR (rustc patch).** Breaks on macOS (no COMDAT
  support) and doesn't solve `#[track_caller]`.
- **`#[linkage = "external"]` on the wrapper.** Requires
  `#![feature(linkage)]` at the crate root, which propagates a nightly
  feature flag into user-controlled territory. Unacceptable for a general
  consumer compiler.
- **Per-instantiation `#[no_mangle]` non-generic shims.** Works in vanilla
  Rust but requires knowing every `(wrapper, type_args)` tuple before
  `generate_stubs()` fires, plus risks `#[no_mangle]` collisions across
  workspace crates.
- **`#[used]` and synthetic fn-pointer statics.** The first doesn't drive
  monomorphization; the second ICE'd inside `per_instance_mir` (the hook
  didn't expect a synthetic static referencing a wrapper). Both attempted
  in the first prior attempt.

The chosen approach (wrapper functions + partitioner patch) extends the
existing fork-as-bridge architecture by exactly one mechanism (visibility
override). The patch is consistent with the four existing query-provider
hooks for layering. Step 2 will replace the inline `__lang_stubs` string
match with a `lang_visibility_override` facade callback so the rustc fork
is consumer-agnostic again.

#### 10.6.6 Side fix: `push_arg_for_rust_call` ABI dispatch

The new `test_vec_pop_unwrap` test exposed a pre-existing latent bug in
`llvm_gen.rs::push_arg_for_rust_call`: every Rust method/trait-static arg
that wasn't a ScalarPair was passed by pointer, regardless of whether
rustc's ABI declared it as `PassMode::Direct(scalar)`. For
`Vec::push(&mut self, value: i32)`, the LLVM declaration is `(ptr, i32, ptr)`
but the call site passed `(ptr, ptr, ptr)`. LLVM's opaque-pointer mode
accepts this silently; on AArch64 the pointer's low 32 bits land in `w1`
and Vec::push stores them as the user's i32. The Vec ends up holding a
stack-pointer fragment instead of `99`. Forty-plus existing
`v.push(int)` test calls all suffered this corruption, but none of them
ever read the stored value back (only `.len()`, `.capacity()`, or clone).

Fix: `push_arg_for_rust_call` now dispatches per-arg on
`&CoercedParam` from `info.coerced_params` (already cached in
`RustMethodInfo` since Phase 3, but unused at call sites until now):

- `Direct(llvm_ty_str)` → lower → into_value → `coerce_int_to_type` →
  push as value.
- `Pair(_, _)` → existing extract-and-split.
- `Indirect` → existing into_ptr + push as ptr (now explicit, not the
  unconditional fallback).
- `Ignore` → lower for side effects, push nothing.

The 4 call sites (StaticCall sret/non-sret, MethodCall sret/non-sret)
clone `info.coerced_params`, then pass `&coerced_params[1 + i]` per
arg (offset 1 because `coerced_params[0]` is `self`). Each adds a
`debug_assert_eq!` matching `call_args.len()` against
`func.get_type().count_param_types()` — mirrors the assertion FnCall has
at line 1212.

The FnCall path (lines 1100-1215) still routes pair detection on
toylang's `is_scalar_pair_type(&a.ty)` rather than `coerced_params`.
That's a parallel weaker oracle — works today because `&[u8]` is the
only ScalarPair both sides know about. Migrating FnCall to the same
per-variant dispatch (and deleting `is_scalar_pair_type` entirely) is
known-tech-debt #6.

#### 10.6.7 Tests added

| Test | What it verifies |
|------|-----------------|
| `test_option_unwrap_basic` | Option<i32>::unwrap from a shim, basic round-trip |
| `test_result_unwrap_basic` | Result<i32, i32>::unwrap, two-arg generic wrapper |
| `test_option_unwrap_result_discarded` | unwrap as ExprStmt (return value discarded) |
| `test_unwrap_arithmetic_chain` | `o.unwrap() + 2i32` — result-typed expression |
| `test_unwrap_two_options_separately` | Two unwrap call sites — wrapper symbol caching |
| `test_vec_pop_unwrap` | Vec::pop().unwrap() — exercises both the wrapper AND the Vec::push ABI fix |

#### 10.6.8 Files involved

- `toylangc/src/oracle.rs` — `WRAPPERS` table, `wrapper_fn_name`,
  `find_wrapper_fn_def_id`, `redirect_to_wrapper` helper.
- `toylangc/src/toylang/callbacks_impl.rs::collect_toylang_fn_deps_inner` —
  redirect injected before standard inherent-method dep building. This is
  the codegen-driving site (per @SMINCZ).
- `toylangc/src/llvm_gen.rs::get_or_resolve_rust_method` — same redirect
  for extern declaration. Read-only with respect to codegen (per @SMINCZ).
- `toylangc/src/llvm_gen.rs::push_arg_for_rust_call` — per-arg dispatch on
  CoercedParam.
- `toylangc/src/stub_gen.rs` — emits `__toylang_option_unwrap<T>` and
  `__toylang_result_unwrap<T, E: Debug>` with `#[inline(never)]`.
- `rustc-lang-facade/src/abi_helpers.rs` — `CoercedParam` derives
  `Clone, Debug` (so `coerced_params` can be cloned out of cached info).
- Forked rustc: `rustc_monomorphize/src/partitioning.rs::mono_item_linkage_and_visibility`
  — the visibility override for `__lang_stubs` items.

### 10.7 Planned: Phase 7 — Standalone test projects

Standalone test projects under `tests/standalone/`, each with a `toylang.toml`
and `main.toylang`. No Rust files, no glue. Each project proves toylang can
link against and call a specific Rust crate.

Every target crate has an imperative API that avoids derive macros:

| Crate | Imperative API | Example |
|-------|---------------|---------|
| rand | Free fn + trait method | `thread_rng().gen::<i32>()` |
| uuid | Static method | `Uuid::new_v4()` |
| indexmap | Constructor + methods | `IndexMap::new()`, `.insert()`, `.len()` |
| regex | Method calls | `Regex::new(pat).unwrap().is_match(text)` |
| clap | Builder pattern | `Command::new("app").arg(Arg::new("input"))` |
| glob | Free function | `glob::glob("*.txt").unwrap()` |
| reqwest | Free function | `reqwest::blocking::get(url).unwrap()` |
| toml | `toml::Value` | `toml::from_str::<Value>(text).unwrap()` |
| serde_json | `serde_json::Value` | `serde_json::from_str::<Value>(json).unwrap()` |

Derive macros are syntactic sugar for trait impls. The underlying APIs are
always available imperatively.

### 10.8 Planned: Phase 8 — Test harness

`toylangc/tests/standalone_tests.rs` builds each standalone project via
`toylangc build` and asserts expected output. One test function per project.

### 10.9 Deferred: duck-typed method resolution

Instead of requiring explicit trait qualification `Trait::method(...)`, the
compiler could search all trait impls automatically when `receiver.method(args)`
doesn't find an inherent method. Using `tcx.all_traits()` and
`tcx.for_each_relevant_impl()`, it would iterate all traits to find one with a
matching method that has an impl for the receiver's concrete type. This would
let users write `out.write_all(bytes)` instead of `Write::write_all(&out, bytes)` —
cleaner syntax at the cost of searching potentially thousands of traits.
Deferred because explicit qualification is simpler and avoids ambiguity when
multiple traits define the same method name.

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
# All integration tests (110 passed, 0 ignored):
cargo +rustc-fork test -p toylangc --test integration_tests

# Unit tests (54 passed):
cargo +rustc-fork test -p toylangc --bin toylangc

# Everything the CI cares about:
cargo +rustc-fork test -p toylangc --test integration_tests --bin toylangc

# Check for warnings:
cargo +rustc-fork check -p toylangc
```

### Arcana index

Cross-cutting concerns documented as arcana (each has `@ID` comments at
affected code sites):

- `@TCHAPZ` — Track Caller Hidden ABI Parameter
  (`docs/arcana/TrackCallerHiddenABIParameter-TCHAPZ.md`)
- `@TVIMDGAZ` — Trait vs Impl Method DefId Generic Args
  (`docs/arcana/TraitVsImplMethodDefIdGenericArgs-TVIMDGAZ.md`)
- `@GCMLZ` — Generate Compile Mutex Lock
  (`docs/arcana/GenerateCompileMutexLock-GCMLZ.md`)
- `@ACRTFDZ` — ABI Coerced Return Type In Function Declarations
  (`docs/arcana/ABICoercedReturnTypeInFunctionDeclarations-ACRTFDZ.md`)
- `@MRRIWMZ` — Manifest Re-read In Wrapper Mode
  (`docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md`)
- `@SMINCZ` — Symbol Mangling Is Not Codegen
  (`docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md`)
- `@DPSFDOZ` — DefPathStr Is For Diagnostics Only
  (`docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md`)
- `@MBMRVZ` — Main Body Must Return Void
  (`docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md`)
- `@RTMEIZ` — Rust Types Must Be Explicitly Imported
  (`docs/arcana/RustTypesMustBeExplicitlyImported-RTMEIZ.md`)
