# Rust Interop via rustc Query Provider: Architecture Guide

> **Current status:** 211 tests passing (67 unit + 129 integration + 15
> standalone), 0 failed, 0 ignored. **Zero rustc fork patches — built
> against vanilla `nightly-2025-01-15`.** All rustc integration flows
> through `Config::override_queries`, `FileLoader`, and a `CodegenBackend`
> wrapper — no fork, no hook statics.
>
> All 8 implementation phases complete. Fork-reduction roadmap fully
> shipped (stages 1–4: `ed2e692`, `b345162`, `ce437ae`/`bf770ae`/`da7ad87`,
> `1d862f4`/`13d8f12`/`51f0c5e`/`d044560`/`c25aa4b`).
>
> **Per-phase history:** `docs/historical/phase-history.md`.
> **Architectural decisions** (why each choice): `docs/reasoning/architecture-decisions.md`.
> **Long-term risk assessment:** `docs/architecture/risks.md`.
> **Build & test commands:** `docs/usage/testing.md`.

## Overview

Two-crate workspace:

- `rustc-lang-facade` — reusable library for integrating custom languages with rustc.
- `toylangc` — toylang consumer, the demonstrator.

Built against vanilla `nightly-2025-01-15` via rustup — zero rustc fork patches. All rustc integration flows through sanctioned extension points:

- **`Config::override_queries`** installs six query overrides (`optimized_mir`, `symbol_name`, `layout_of`, `mir_shims`, `collect_and_partition_mono_items`, `upstream_monomorphizations_for`).
- **`FileLoader`** injects `__lang_stubs.rs` as a virtual source file.
- **`CodegenBackend` wrapper** (`codegen_wrapper.rs`) sits between cargo's driver and `LlvmCodegenBackend`, injects the consumer's `.o` at `join_codegen`.

Consumer types appear to rustc as opaque stubs with `unreachable!()` bodies. Internal consumer functions are never exposed to rustc — they are discovered via deep monomorphization walk and compiled separately by an Inkwell LLVM backend. A global mutex serializes all consumer code (single-threaded).

`ResolvedType` is the unified type representation (no string-based types). Explicit type args required at all call sites (no inference).

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
│     ├─ optimized_mir override fires once per CONSUMER DefId      │
│     │  → symbolic walk finds ALL transitive Rust deps            │
│     │  → Param-bearing (DefId, GenericArgsRef) pairs returned    │
│     │  → rustc's collector substitutes per caller                │
│     ├─ symbol_name fires per concrete ENTRY-POINT instance       │
│     │  → internal toylang callees stashed in ToylangState        │
│     ├─ layout_of fires for each consumer type instantiation      │
│     ├─ mir_shims fires for each consumer drop glue instance      │
│     └─ collect_and_partition_mono_items override runs:           │
│         → filters consumer items out of rustc's CGU list         │
│         → forces (External, Default) linkage on __lang_stubs     │
│         → stashes consumer CGUs for the plugin's own codegen     │
│  7. rustc codegens ONLY the filtered-down CGU list               │
│     (no consumer items; no codegen-skip hook needed)             │
│  8. generate_and_compile fires (all instances known)             │
│     ├─ Entry-point fns from MonoItems (with Instance for ABI)    │
│     ├─ Internal fns from ToylangState.toylang_instances           │
│     ├─ Two-pass codegen: internal fns, then extern wrappers      │
│     └─ llc compiles to .o, injected at join_codegen              │
│  9. Link: consumer .o + rustc .o → final binary                  │
└──────────────────────────────────────────────────────────────────┘
```

Key insight: internal toylang functions (those only called by other toylang functions) are never exposed to rustc. The deep monomorphization walk in step 6 discovers them and their transitive Rust deps in a single pass. Rustc only sees entry-point functions and Rust deps.

---

## Part 2: The Query Overrides

Six `override_queries` hooks plus a `CodegenBackend` wrapper. All sanctioned rustc extension points. Consumer functions in `__lang_stubs` have `unreachable!()` bodies that pass rustc's normal pipeline; our overrides take over at monomorphization / partitioner / codegen time.

### 2.1 layout_of

**Key:** `Ty<'tcx>` — fires per concrete type instantiation.

Reports 0 fields in `FieldsShape` (opaque memory blob). Size and alignment come from the consumer's `monomorphize_type` callback, which returns concrete field types. The library computes C-style layout from those.

Skips types with unresolved type params (`has_param` check) — these are generic definitions, not concrete instantiations.

### 2.2 optimized_mir override

**Key:** `LocalDefId` — fires once per consumer function. Installed via `Config::override_queries`.

Non-consumer DefIds delegate to rustc's saved upstream provider; consumer items (filtered by `is_consumer_codegen_target` — `is_from_lang_stubs_safe` AND either consumer-fn or consumer-accessor) get a synthesized body whose only purpose is to mention each transitive Rust dep via a `ReifyFnPointer` cast. The body terminates with `Unreachable`. Rustc codegen never sees these bodies because the partitioner override (§2.5) filters consumer items out of the CGU list before codegen starts; the consumer's own `.o` supplies the definitions at link time.

The provider calls `collect_generic_rust_deps` which:

1. Installs a Param-name → Param-index map (`oracle::ActiveParamMap`) for the duration of the walk.
2. Type-resolves the consumer body WITHOUT substituting Params — the body stays generic.
3. Walks the typed AST for deps.
4. For toylang callees: recursively walks their bodies using a **local cycle guard** (not persistent state) so transitively-reached deps are collected on every call.
5. For Rust deps (extern fns, Rust methods): returns `(DefId, GenericArgsRef)` pairs whose args may contain `ty::TyKind::Param`. Rustc's mono collector substitutes those Params per caller during its own walk of our synthesized body — the same substitution engine it applies to every generic Rust function.

This callback is side-effect-free with respect to consumer codegen state — internal-callee stashing is the separate job of `notify_concrete_entry_point` (§2.3).

**Why the override works with Params in the output.** Dep discovery's OUTPUT must describe per-Instance deps but its COMPUTATION can be symbolic and run once per DefId — rustc's collector does the per-Instance concretion. `docs/reasoning/dep-discovery-approaches.md` spells out this asymmetry, including why the same trick does NOT apply to internal consumer→consumer codegen (no downstream substitutor exists for toylang LLVM IR, so `notify_concrete_entry_point` stays Instance-keyed).

### 2.3 symbol_name

**Key:** `Instance<'tcx>` — maps consumer instances to consumer symbol names.

Calls `notify_concrete_entry_point` to get the extern symbol. This is also the hook that drives **internal consumer→consumer discovery + stashing** into `ToylangState.toylang_instances`. The `walked_entry_points` set in `ToylangState` dedups across calls so a shared internal callee reached from multiple entry points is walked + stashed exactly once.

Symbol examples:

- Concrete: `__toylang_impl_make_counter`
- Generic: `__toylang_impl_wrap__i32`
- Accessor: `__toylang_accessor_Pair_first__i32__i32`

Type args in symbols use `_LT_` / `_GT_` delimiters for collision safety: `Vec<i32>` → `Vec_LT_i32_GT_`.

### 2.4 mir_shims (drop glue)

**Key:** `InstanceKind::DropGlue(_, Some(ty))` — per concrete type instantiation.

Builds a MIR body calling `__toylang_drop_TypeName(ptr)`. The consumer provides the destructor in its .o file. `set_required_consts` and `set_mentioned_items` must be called (with empty vecs) on shim bodies — `mir_promoted` doesn't run for shims.

### 2.5 collect_and_partition_mono_items (partitioner override)

**Key:** `()` — fires once per compilation, returns the (reachable, CGUs) pair that rustc's codegen backend consumes.

Delegates to rustc's upstream partitioner (saved via `DEFAULT_PARTITIONER` OnceLock), then:

1. **Filters consumer items out of the CGU list.** Items matching `is_consumer_codegen_target` are removed from rustc's CGUs. They never reach `LlvmCodegenBackend::codegen_crate`; the consumer's Inkwell backend handles them via `generate_and_compile`. This is what makes the old `CODEGEN_SKIP_HOOK` fork patch unnecessary.
2. **Forces `(Linkage::External, Visibility::Default)` on `__lang_stubs` items that remain** (the Phase 6 `__toylang_option_unwrap` / `__toylang_result_unwrap` wrappers — real Rust code rustc still compiles). The LLVM backend reads `data.linkage` directly from the CGU's `MonoItemData` without re-derivation, so this post-partition mutation survives to emission. Replaces the old `VISIBILITY_OVERRIDE_HOOK` fork patch.

The mutation-survives-to-LLVM assumption is the load-bearing piece — see `docs/architecture/risks.md` §3 B2 for the assumption's mechanics and failure modes.

### 2.6 upstream_monomorphizations_for

**Key:** `DefId` — fires during mono collection to decide whether a downstream crate should use an upstream crate's monomorphization.

Returns `None` for consumer DefIds so generic wrappers (like `__toylang_option_unwrap<T>`) are monomorphized locally rather than deferred to an upstream crate. Scaffolded in stage-4c step 1 (commit `51f0c5e`); ~40 LoC. Originally intended for the separate-crate-stubs migration (POC #2 risk #1); the override is load-bearing anyway for consumer generic-wrapper routing even under the single-crate FileLoader model.

### 2.7 The two callback families

The hooks split into two trait families based on whether they need consumer state:

**`LangPredicates`** (pure; no state, no lock):

- `is_consumer_type`, `is_consumer_fn` — name predicates.
- `generate_stubs` — produces the injected stub source once at startup.

**`LangCallbacks: LangPredicates`** (stateful; takes `&mut dyn Any`, locks `MUTABLE_STATE`):

- `create_state` — constructor for the consumer state box.
- `monomorphize_type` — produces per-instantiation type-layout data.
- `collect_generic_rust_deps` — returns the Rust items a consumer function transitively depends on; called from the `optimized_mir` override at DefId granularity, returns Param-bearing args that rustc's collector substitutes per caller.
- `notify_concrete_entry_point` — returns the extern symbol for a concrete consumer entry-point Instance and drives internal-callee stashing; called from `symbol_name`.
- `after_rust_analysis` — validation after rustc's analysis phase.
- `generate_and_compile` — runs the consumer's LLVM backend; holds the lock for the entire duration of consumer codegen.

The split is enforced by signature: predicate trampolines have no `&mut (dyn Any + Send + Sync)` parameter, so a hook in the predicate family literally cannot acquire the `MUTABLE_STATE` lock — it has no state to pass to a lock-acquiring helper. New hooks pick a family based on whether they need state. See `@GCMLZ` for the locking story this split enforces.

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

Query overrides use `is_from_lang_stubs(tcx, def_id)` (diagnostic-gated — safe inside `generate_and_compile`) or `is_from_lang_stubs_safe(tcx, def_id)` (structural `DefPathData` walk — safe anywhere). Both check that the DefId's path contains `__lang_stubs::`. The `_safe` variant is used in the partitioner override and other paths that may run outside `generate_and_compile`; see `@DPSFDOZ`.

### 3.3 Why unreachable!() works

Consumer functions have `unreachable!()` bodies. Rustc compiles them normally through `mir_built` and `borrowck` — no overrides needed. The `optimized_mir` override replaces the bodies at monomorphization time with a synthetic dep-registering body. The partitioner override (§2.5) filters consumer items out of the CGU list, so rustc's LLVM backend never emits code for them. The `unreachable!()` code is never reached or emitted in the final binary.

---

## Part 4: The LLVM Backend

### 4.1 Instance discovery

`generate_with_tcx` discovers toylang function instances from two sources:

1. **MonoItems walk** — finds entry-point functions (Rust calls them) and accessor methods. Entry-point functions have a rustc `Instance` (needed for extern wrapper ABI queries). The walk uses `is_from_lang_stubs` as the sole consumer-item filter; DefIds may be local or cross-crate.

2. **`state.toylang_instances`** — internal functions discovered during the deep monomorphization walk. These have no rustc `Instance` and only get an internal ABI function (no extern wrapper).

Each function instance carries:

- `resolved_func: ToyFunction` — the type-substituted function definition
- `instance: Option<Instance<'tcx>>` — `Some` for entry points, `None` for internal
- `extern_symbol: String` — the mangled symbol name

### 4.2 Two-pass codegen

**Pass 1:** `codegen_internal_function` for ALL items (entry + internal). Simple ABI: primitives direct, structs always sret.

**Pass 2:** `codegen_extern_wrapper` for entry-point items ONLY (those with `Some(instance)`). Thin wrapper matching Rust ABI, delegates to internal function.

Accessor methods are generated inline via `codegen_accessor_inline` (GEP to field offset, return pointer).

### 4.3 Type annotation pass

`type_resolve.rs` produces a `TypedBlock` where every expression carries a `ResolvedType`. This runs before LLVM codegen and handles:

- **IntLit:** Type carried on the AST node from lexer (suffixes: `42i32`, `42i64`, `42usize`; default i32, auto-promote to i64 if value overflows i32).
- **BoolLit:** Always `ResolvedType::Bool`.
- **UnaryNeg:** Desugared to `BinaryOp::Sub(zero, inner)` with zero matching the inner expression's type.
- **Generic type params:** Substituted with concrete types from Instance args (including in body expressions via `substitute_type_params_in_body`).
- **BinaryOp:** Left operand's type propagates to right. Comparison/boolean ops (`==`, `!=`, `<`, `<=`, `>`, `>=`, `&&`, `||`) return `Bool`.
- **Assignment:** RHS type checked against variable's existing type (`AssignTypeMismatch` error).

**No inference machinery.** All type args must be provided explicitly at call sites. Integer literals carry their own type from the parser.

**Error handling:** Returns `Result<T, TypeResolveError>` with 12+ typed error variants. `after_rust_analysis` validates non-generic function bodies and reports all errors before aborting.

### 4.4 Deep dependency discovery

Two walkers sit behind the two callbacks, sharing `walk_typed_body_for_deps` (the typed-AST traversal primitive) and `type_resolve_body`. The split mirrors the trait split.

`collect_rust_deps_recursive` (driven by `collect_generic_rust_deps`):

- **Toylang function calls:** recurse into the callee's body under a **local cycle guard** (a fresh `HashSet<String>` per outer call). Do NOT stash. Do NOT report the toylang callee to rustc.
- **Extern function calls:** Report `(DefId, GenericArgsRef)` to rustc.
- **Rust method calls:** `find_inherent_method` / trait / wrapper dispatch → report to rustc.

`walk_and_stash_internal_callees` (driven by `notify_concrete_entry_point`):

- **Toylang function calls:** stash the callee in `state.toylang_instances` and recurse. Uses `state.walked_entry_points` as persistent dedup so shared internal callees are stashed exactly once per compilation.
- **Everything else:** ignored — Rust deps flow through the other walker.

The two dedup structures are intentionally separate. `walked_entry_points` persists across `notify_concrete_entry_point` calls so internal-callee codegen isn't duplicated. The Rust-deps walker's cycle guard is local so that a shared internal helper reached from two entry points still gets its Rust deps collected both times — rustc's mono collector dedups Rust items independently, so duplicated dep registration is harmless, whereas a missed registration would produce unresolved-symbol link failures.

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
- `StaticCall` / `MethodCall` → looked up in `rust_method_info`, sret or direct, with null appended for `#[track_caller]` hidden param (see `@TCHAPZ`)

Rust types (Vec, etc.) use opaque `[N x i8]` byte arrays — size and alignment queried from `tcx.layout_of`. Toylang structs use real LLVM struct types with GEP-based field access.

### 4.6 Internal/extern ABI split

Each entry-point toylang function generates **two** LLVM functions:

1. **Internal** (`__toylang_internal_{name}`) — simple, predictable ABI:
   - Primitives (i32, i64, f64, bool, usize): returned directly.
   - Void: void return.
   - Structs/Vec: always sret (ptr first param, void return).
   - Uses `resolved_to_inkwell` for types (toylang ABI, not Rust ABI).

2. **Extern wrapper** (`__toylang_impl_{name}`) — thin wrapper matching Rust ABI:
   - Calls the internal function.
   - Adapts return/params to match `fn_abi_of_instance`.
   - Uses `parse_coerced_type` from ABI-coerced strings for types (see `@ACRTFDZ`).

Internal-only toylang functions generate only the internal function (no wrapper). Toylang-to-toylang calls use `__toylang_internal_` symbols directly.

### 4.7 Toylang-owned main

When toylang defines `fn main()`, the stub wrapper is renamed to `__toylang_main` to avoid conflicting with Rust's `main`. The mapping flows through:

- `is_consumer_fn` returns true for both `"main"` and `"__toylang_main"`.
- `collect_generic_rust_deps` / `notify_concrete_entry_point` both map `"__toylang_main"` → `"main"` for registry lookup.
- `compute_fn_symbol` → extern symbol `__toylang_impl_main`.

### 4.8 `use` imports

Toylang supports `use` statements: `use std::alloc::Global`. The parser stores the path in `registry.imports`. The stub generator emits `pub use` in `__lang_stubs.rs`. Three oracle functions find re-exports via `module_children_local`:

- `find_reexported_type` — matches `DefKind::Struct` and `DefKind::Enum`.
- `find_use_imported_trait_def_id` — matches `DefKind::Trait`.
- `find_use_imported_fn_def_id` — matches `DefKind::Fn`.

### 4.9 Trait method calls

Explicit trait qualification using `StaticCall`:

```
use std::io::Write
fn main() {
    let out = stdout();
    Write::write_all(&out, b"hello\n")
}
```

The type resolver distinguishes trait calls from inherent calls by checking `find_use_imported_trait_def_id`. For codegen, `get_or_resolve_rust_method` uses the trait definition's method `DefId` with `[Self, ...]` args (per `@TVIMDGAZ`). `Instance::expect_resolve` maps from the trait-level DefId to the concrete impl at monomorphization time.

### 4.10 ABI details

- **ABI-coerced return types for Rust function declarations (`@ACRTFDZ`):** all three Rust call paths use `parse_coerced_type(coerced_ret)` for the declaration. When the ABI type differs from toylang's type, codegen stores the return value through an alloca (type-punning bitcast via memory). Load-bearing for any Rust function returning a struct that rustc coerces to a scalar — e.g., `stdout()` returning `Stdout`, which rustc coerces to `i64`.
- **Byte string literals:** `b"hello\n"` → `&[u8]` fat pointer `{ ptr, i64 }`. `CoercedParam::Pair` ABI variant splits the struct into two LLVM args.
- **String literals:** `"hello"` → `&str` via `ResolvedType::Ref { Str }`, same fat-pointer shape as `&[u8]` (see `@UTAIRZ`).
- **`#[track_caller]` hidden parameter:** null `ptr` appended at every call site to methods carrying the attribute (see `@TCHAPZ`).

### 4.11 Broadened TyKind handling

`rustc_ty_to_resolved_type` handles primitive int/uint/float widths (i8, u8, u16, u32, u64, f32, etc.) as opaque `RustType`; `TyKind::Str`, `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple` pass through as opaque `RustType` with a stable name. These types are never inspected by toylang — they surface as generic type arguments of Rust types (e.g., `Option<u8>`, `HashMap<String, i32>` internals). `resolved_to_rustc_ty` round-trips primitive names back to `tcx.types.*`.

---

## Part 5: Global State and Threading

### 5.1 Split into immutable config and mutable state (`@GCMLZ`)

Facade state is split into several statics, each with the minimum synchronization necessary:

```
CONFIG:              OnceLock<FacadeConfig>                // immutable
DEFAULT_LAYOUT_OF:   OnceLock<LayoutOfFn>                  // immutable
DEFAULT_MIR_SHIMS:   OnceLock<MirShimsFn>                  // immutable
DEFAULT_SYMBOL_NAME: OnceLock<SymbolNameFn>                // immutable
DEFAULT_OPTIMIZED_MIR: OnceLock<OptimizedMirFn>            // immutable
DEFAULT_PARTITIONER:   OnceLock<PartitionerFn>             // immutable
MUTABLE_STATE:       OnceLock<Mutex<FacadeMutableState>>   // mutable
```

`FacadeConfig` (set once during `install_callbacks`, never changes):

- `callbacks: Box<dyn Any>` — the type-erased `ToylangCallbacks`.
- `vtable: CallbackVtable` — HRTB function pointers for dispatch.

`FacadeMutableState` (locked only by callbacks that need `&mut state`):

- `consumer_state: Box<dyn Any>` — the type-erased `ToylangState`.
- `lang_obj_path: Option<PathBuf>` — compiled .o path for link injection.

Default query providers live in their own `OnceLock` statics, read without locking during query provider fallthroughs.

### 5.2 Locking protocol

| Function | Reads | Locks |
|----------|-------|-------|
| `is_consumer_type` / `is_consumer_fn` | `CONFIG` | none |
| `default_layout_of` / `default_mir_shims` / `default_symbol_name` / `default_optimized_mir` / `default_partitioner` | `DEFAULT_*` | none |
| `call_monomorphize_type` / `call_collect_generic_rust_deps` / `call_notify_concrete_entry_point` | `CONFIG` | `MUTABLE_STATE` |
| `call_after_rust_analysis` / `call_generate_and_compile` | `CONFIG` | `MUTABLE_STATE` |
| `set_lang_obj_path` / `get_lang_obj_path` | — | `MUTABLE_STATE` (brief) |

This ensures single-threaded toylang code execution even when rustc's query providers fire on Rayon worker threads. Query providers reading only config are lock-free; only callbacks that need `&mut consumer_state` serialize.

### 5.3 Reentrancy avoidance

`generate_and_compile` calls `generate_with_tcx` which calls `callbacks.notify_concrete_entry_point_inner()` — this is NOT the trait method (which would re-lock). It's a direct method on `ToylangCallbacks` that takes `&mut ToylangState` as a parameter, bypassing the mutex entirely. Used for accessor symbol lookup during codegen (the consumer already holds the mutable state mutex for the duration of `generate_and_compile`, per `@GCMLZ`).

### 5.4 Consumer state (`ToylangState`)

```rust
pub struct ToylangState {
    pub log: Vec<CallbackLog>,
    pub toylang_instances: Vec<ToylangInstance>,
    pub walked_entry_points: HashSet<String>,
}
```

- `log` — structured record of every callback from rustc (for test assertions).
- `toylang_instances` — functions discovered during the internal-callee walk (consumed by `generate_with_tcx`).
- `walked_entry_points` — persistent dedup set for the internal-callee walk so shared internal callees are stashed exactly once per compilation. **Not** shared with the Rust-deps walker — that walker uses a local cycle guard per call (see §4.4).

### 5.5 Callback log

```rust
pub enum CallbackLog {
    MonomorphizeType { name: String },
    CollectGenericRustDeps { name: String },
    NotifyConcreteEntryPoint { name: String },
    AfterRustAnalysis,
    GenerateAndCompile,
}
```

Tests can set `TOYLANG_LOG_PATH` env var to dump the log to a file, then assert that internal functions do NOT appear in either per-entry-point variant. A helper at the top of `toylangc/tests/integration_tests.rs` — `log_mentions_callback_for(log, name)` — treats the two per-entry-point variants as equivalent from the test's standpoint.

---

## Part 6: Key Files

### Library (`rustc-lang-facade/src/`)

| File | Purpose |
|------|---------|
| `lib.rs` | `LangPredicates` + `LangCallbacks: LangPredicates` traits, split globals (`CONFIG`, `MUTABLE_STATE`, `DEFAULT_*` — see `@GCMLZ`), vtables + trampolines, `is_from_lang_stubs` / `is_from_lang_stubs_safe` / `is_consumer_codegen_target` / `is_consumer_accessor_safe` helpers |
| `queries/layout.rs` | `layout_of` override |
| `queries/optimized_mir.rs` | `optimized_mir` override: synthesizes Param-bearing dep-registering bodies for consumer DefIds, delegates to saved upstream default for everything else |
| `queries/symbol_name.rs` | `symbol_name` override |
| `queries/drop_glue.rs` | Drop glue (`mir_shims`) override |
| `queries/partition.rs` | `collect_and_partition_mono_items` override: filters consumer items out of CGUs, forces `(External, Default)` linkage on remaining `__lang_stubs` items |
| `queries/upstream_monomorphizations.rs` | `upstream_monomorphizations_for` override |
| `queries/mod.rs` | Query override installation |
| `abi_helpers.rs` | ABI coercion helpers: `CoercedReturn`, `CoercedParam` (incl. `Pair` variant for ScalarPair, see `@ACRTFDZ`); hidden `#[track_caller]` param (see `@TCHAPZ`) |
| `mir_helpers.rs` | Drop glue MIR builder |
| `codegen_wrapper.rs` | CodegenBackend wrapper, .o injection at `join_codegen` |
| `driver.rs` | `run_compiler` entry point |
| `file_loader.rs` | Stub injection via rustc's `FileLoader` trait |

### Consumer (`toylangc/src/`)

| File | Purpose |
|------|---------|
| `llvm_gen.rs` | Inkwell LLVM backend: instance discovery, two-pass codegen, Rust method resolution, FnCall use-import path with ABI return coercion (`@ACRTFDZ`) |
| `stub_gen.rs` | Generates `__lang_stubs.rs` content |
| `oracle.rs` | TyCtxt query helpers, type conversion, symbol mangling, `ActiveParamMap` thread-local |
| `main.rs` | CLI entry point — three-mode dispatch (build / wrapper / direct) |
| `build.rs` | `toylangc build` — generates `.toylang-build/` Cargo project, spawns cargo (see `@MRRIWMZ`) |
| `manifest.rs` | `toylang.toml` parser |
| `toylang/ast.rs` | Untyped AST |
| `toylang/typed_ast.rs` | `ResolvedType` enum, `TypedBlock`, `TypedStmt` |
| `toylang/type_resolve.rs` | Type annotation pass, `TypeResolveError` |
| `toylang/parser.rs` | Parser, lexer, operator precedence |
| `toylang/registry.rs` | `ToyStruct`, `ToyFunction`, `ToyParam` |
| `toylang/callbacks_impl.rs` | `LangCallbacks` impl, `ToylangState`, deep monomorphization walk, `CallbackLog` |

---

## Part 7: Known Technical Debt

See `docs/architecture/known-tech-debt.md` for tracked items. Open items are small and non-urgent; load-bearing work is complete.

---

## Part 8: Arcana Index

Cross-cutting concerns documented as arcana (each has `@ID` comments at affected code sites):

- `@TCHAPZ` — Track Caller Hidden ABI Parameter (`docs/arcana/TrackCallerHiddenABIParameter-TCHAPZ.md`)
- `@TVIMDGAZ` — Trait vs Impl Method DefId Generic Args (`docs/arcana/TraitVsImplMethodDefIdGenericArgs-TVIMDGAZ.md`)
- `@GCMLZ` — Generate Compile Mutex Lock (`docs/arcana/GenerateCompileMutexLock-GCMLZ.md`)
- `@ACRTFDZ` — ABI Coerced Return Type In Function Declarations (`docs/arcana/ABICoercedReturnTypeInFunctionDeclarations-ACRTFDZ.md`)
- `@MRRIWMZ` — Manifest Re-read In Wrapper Mode (`docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md`)
- `@SMINCZ` — Symbol Mangling Is Not Codegen (`docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md`)
- `@DPSFDOZ` — DefPathStr Is For Diagnostics Only (`docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md`)
- `@MBMRVZ` — Main Body Must Return Void (`docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md`)
- `@RTMEIZ` — Rust Types Must Be Explicitly Imported (`docs/arcana/RustTypesMustBeExplicitlyImported-RTMEIZ.md`)
- `@UTAIRZ` — Unsized Types Appear Inside Ref (`docs/arcana/UnsizedTypesAppearInsideRef-UTAIRZ.md`)
- `@IVTDBTZ` — Inherent Vs Trait Dispatch By Type (`docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md`)
- `@ELASZ` — Early-bound Lifetime Args Are Synthesized (`docs/arcana/EarlyBoundLifetimeArgsSynthesized-ELASZ.md`)
- `@ETASTZ` — Extra Type Args Are Silently Truncated (`docs/arcana/ExtraTypeArgsSilentlyTruncated-ETASTZ.md`)

---

## See also

- `docs/architecture/risks.md` — long-term risk assessment for the zero-fork architecture. Categorizes what could break, how likely, what the canaries are, and what the exit strategies are. Read when bumping rustc nightly or when something breaks unexpectedly.
- `docs/architecture/known-tech-debt.md` — tracked tech debt items.
- `docs/reasoning/architecture-decisions.md` — why each major architectural choice was made.
- `docs/reasoning/why-interleaved-monomorphization.md` — the architectural invariant (interleaving with rustc's monomorphization) the whole approach depends on.
- `docs/reasoning/rustc-fork-design-space.md` — the design-space analysis that led to zero-fork.
- `docs/reasoning/dep-discovery-approaches.md` — Approach A vs B comparison for per-Instance dep discovery.
- `docs/usage/testing.md` — build & test commands.
- `docs/usage/writing-main.md` — practical rules for writing toylang programs.
- `docs/historical/phase-history.md` — per-phase writeups for all shipped implementation phases and fork-reduction stages.
