# Plan: Implement Option B library split in two phases

## Context

Split the project into a reusable `rustc-lang-facade` library and a `toylangc` consumer. Option B: the library owns the driver lifecycle and exposes a callback trait (`LangCallbacks`) with 4 methods. See `docs/historical/library-split-options.md` for the full design discussion.

## Target API (Option B)

```rust
// The trait the user implements — has generic 'tcx on methods, which is fine
// for concrete types (not object-safe, but we don't use dyn).
pub trait LangCallbacks: Send + Sync + 'static {
    fn generate_stubs(&self) -> String;
    fn monomorphize_type<'tcx>(&self, name: &str, tcx: TyCtxt<'tcx>, ty: Ty<'tcx>) -> MonomorphizeTypeResult<'tcx>;
    fn monomorphize_fn<'tcx>(&self, name: &str, tcx: TyCtxt<'tcx>, def_id: LocalDefId) -> MonomorphizeFnResult<'tcx>;
    fn generate_and_compile<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Option<(PathBuf, Vec<String>)>;
}

// Generic entry point — user passes a concrete instance, never sees dyn/Any/vtable.
pub fn run_compiler<C: LangCallbacks>(callbacks: C, rustc_args: &[String]) -> Result<(), Error>;
```

### The `'tcx` lifetime problem and solution (VERIFIED — compiles)

The `LangCallbacks` trait has generic `<'tcx>` on its methods. This means `dyn LangCallbacks`
is not allowed (Rust object safety rules). But query override providers are plain function
pointers that must read from a global — they can't capture the callbacks instance.

Solution: **manual vtable with HRTB function pointers** (prototyped in `src/lang_trait.rs`):

1. `run_compiler<C>()` knows the concrete type `C`
2. It stores the callbacks as `Box<dyn Any + Send + Sync>` (type-erased)
3. It creates trampoline functions monomorphized for `C`, stored as `for<'tcx> fn(...)` pointers
4. Query overrides call through the vtable — the trampoline downcasts from `dyn Any` to `&C`
   and calls the concrete method (which supports generic `'tcx>`)

The user just does:
```rust
struct MyCallbacks { ... }
impl LangCallbacks for MyCallbacks { ... }
rustc_lang_facade::run_compiler(MyCallbacks::new(), &args).unwrap();
```

Library identifies consumer items by name-based lookup (name sets stored alongside the vtable).

---

## Phase 1: Preparatory refactoring — COMPLETE ✓

All query overrides now go through the `LangCallbacks` vtable. No query override imports
`ToylangRegistry` or toylang AST types. 5/5 tests pass.

### What was done

**Step 1.1: Created `src/lang_trait.rs`** ✓
- `LangCallbacks` trait with 4 methods (generic `'tcx` on methods)
- `MonomorphizeTypeResult<'tcx>` and `MonomorphizeFnResult<'tcx>` result types
- `CallbackVtable` with HRTB `for<'tcx> fn(...)` function pointers
- Trampoline functions that downcast `dyn Any` → `&C` and call concrete methods
- `install_callbacks<C>()` stores callbacks + vtable + name sets in globals
- Helper functions: `is_consumer_type()`, `is_consumer_fn()`, `call_monomorphize_type()`,
  `call_monomorphize_fn()`, `call_generate_and_compile()`

**Step 1.2: Refactored all 4 query overrides** ✓
- `queries/layout.rs` — removed `REGISTRY` OnceLock, calls `call_monomorphize_type()` through vtable
- `queries/borrowck.rs` — removed `REGISTRY` OnceLock, uses `is_consumer_fn()` for detection
- `queries/mir_build.rs` — removed `REGISTRY` OnceLock and all toylang AST code, calls `call_monomorphize_fn()`
- `queries/drop_glue.rs` — removed `REGISTRY` OnceLock, uses `is_consumer_type()` for detection

**Step 1.3: Created `src/toylang/callbacks_impl.rs`** ✓
- `ToylangCallbacks` struct holding `Arc<ToylangRegistry>` + LLVM paths
- `impl LangCallbacks` with all 4 methods
- Moved toylang-specific code here: field type mapping (ToyFieldType → Ty), Vec dep scanning
  (collect_rust_deps, scan_body_vec_ops, etc.), LLVM codegen orchestration

**Step 1.4: Updated `callbacks.rs`** ✓
- No longer holds registry — just stubs string + has_external_codegen flag
- `after_analysis` calls `call_generate_and_compile()` through vtable

**Step 1.5: Updated `main.rs`** ✓
- Creates `ToylangCallbacks`, calls `install_callbacks()` with name sets
- Removed `hardcoded_point()` fallback (empty registry when no `--toylang-input`)

**Cleanup:**
- Deleted `tests/mir_test.rs` (legacy hardcoded test, superseded by other tests)
- Added `tests/drop_point.toylang` (minimal struct def for drop test)
- `drop_test` now uses `--toylang-input tests/drop_point.toylang` instead of hardcoded registry

### Surprises encountered

1. **`monomorphize_type` needed `Ty<'tcx>`, not `LocalDefId`:** The layout_of query receives
   the concrete type (e.g. `Pair<i32, i32>`) with generic args already substituted. Passing
   `LocalDefId` gave the generic definition, causing `TypeParam` fields to fail resolution.
   Fixed by changing the signature to take `Ty<'tcx>` so the consumer can extract concrete
   generic args directly.

2. **DefId tracking deferred:** Originally planned to track DefIds from the stub file for
   robust consumer item detection. Discovered there's no rustc callback hook between "parsing
   done" and "queries start firing" — only `config()` and `after_analysis()` are available.
   Kept name-based detection (storing name sets alongside the vtable) which is sufficient
   since the consumer controls stub naming.

3. **`mir_test.rs` incompatible with new architecture:** The `get_x` function used a hardcoded
   `build_const_i32_body` fallback — a direct MIR body, not an extern call stub. The new
   `monomorphize_fn` always returns an extern symbol + rust deps. Rather than adding complexity
   to support this legacy path, deleted the test since it was superseded by the 5 other tests.

---

## Phase 2: Split into library + consumer crates — COMPLETE ✓

All files moved, workspace compiles with zero warnings, 5/5 tests pass.

### What was done

**Step 2.1–2.5:** Created workspace with two crates, moved files, updated imports.

**Step 2.6:** Deleted old `src/` directory, cleaned up all warnings:
- Removed dead `merge_objects` function from codegen_wrapper
- Removed `hardcoded_point()` and `is_toylang_type()` from registry (legacy PoC code)
- Fixed unused imports (`Ty`, `TyKind`, `sym`, `LangCallbacks`)
- Prefixed unused function params with `_`
- Added `#[allow(dead_code)]` for struct fields used only as HashMap keys

### Surprises

1. **`RunCompiler::run()` returns `()` not `Result`** — removed `.unwrap()` from `run_compiler`
2. **`C: 'static` bound needed** — `install_callbacks` stores in `Box<dyn Any>` which requires `'static`.
   In practice always satisfied since consumer callbacks hold owned data (`Arc`, `PathBuf`).
3. **`extern crate` declarations** — must be in `lib.rs` for the library crate, not in individual
   submodules (Rust 2021 edition with `rustc_private` feature).

### Final workspace structure

```
erw/
  Cargo.toml              # workspace root
  rustc-lang-facade/      # library crate
    Cargo.toml
    src/
      lib.rs              # pub trait LangCallbacks, MonomorphizeTypeResult, etc.
      queries/            # layout.rs, borrowck.rs, mir_build.rs, drop_glue.rs, mod.rs
      mir_helpers.rs
      abi_helpers.rs
      oracle.rs
      codegen_wrapper.rs
      file_loader.rs
      driver.rs           # run_compiler() entry point (was callbacks.rs)
  toylangc/               # consumer crate
    Cargo.toml
    src/
      main.rs
      toylang/            # parser, ast, registry
      callbacks_impl.rs   # impl LangCallbacks for ToylangCallbacks
      stub_gen.rs
      llvm_gen.rs
    tests/
      *.rs, *.toylang
```

### Step 2.2: Move files to library crate

Move these files (already generic after Phase 1) into `rustc-lang-facade/src/`:
- `src/lang_trait.rs` → `lib.rs` (trait + result types + vtable machinery)
- `src/queries/*` → `queries/`
- `src/mir_helpers.rs` → `mir_helpers.rs`
- `src/abi_helpers.rs` → `abi_helpers.rs`
- `src/oracle.rs` → `oracle.rs` (remove `dump_toylang_oracle`)
- `src/codegen_wrapper.rs` → `codegen_wrapper.rs`
- `src/file_loader.rs` → `file_loader.rs`
- `src/callbacks.rs` → `driver.rs` (rename `ToyCallbacks` to `LangDriver`, expose `run_compiler()`)

### Step 2.3: Create `run_compiler` entry point

In `rustc-lang-facade/src/driver.rs`:

```rust
pub fn run_compiler<C: LangCallbacks>(
    callbacks: C,
    rustc_args: &[String],
) -> Result<(), Error> {
    let stubs = callbacks.generate_stubs();
    install_callbacks(callbacks);  // stores in Box<dyn Any> + vtable
    let mut driver = LangDriver::new(stubs);
    rustc_driver::RunCompiler::new(rustc_args, &mut driver).run()
}
```

`LangDriver` implements `rustc_driver::Callbacks` internally — the consumer never sees it.
Query overrides call through the `VTABLE` global, which trampolines to the concrete `C`.

### Step 2.4: Move consumer files

Keep these in `toylangc/src/`:
- `main.rs` — simplified to parse + build callbacks + call `run_compiler`
- `toylang/` — parser, ast, registry (unchanged)
- `callbacks_impl.rs` — `impl LangCallbacks`
- `stub_gen.rs` — toylang-specific stub generation
- `llvm_gen.rs` — toylang-specific LLVM IR generation

### Step 2.5: Wire up Cargo workspace

```toml
# erw/Cargo.toml
[workspace]
members = ["rustc-lang-facade", "toylangc"]

# rustc-lang-facade/Cargo.toml
[lib]
name = "rustc_lang_facade"
# extern crates: rustc_driver, rustc_middle, rustc_hir, etc.

# toylangc/Cargo.toml
[[bin]]
name = "toylangc"
[dependencies]
rustc-lang-facade = { path = "../rustc-lang-facade" }
# also needs rustc crates for Ty, TyCtxt, DefId used in LangCallbacks methods
```

### Step 2.6: Update test runner

Tests currently invoke `./target/debug/toylangc`. The binary path changes to `./target/debug/toylangc` (from the toylangc crate). Update any test scripts or docs.

---

## Files summary

### Moves to library (`rustc-lang-facade`)
| Current path | New path | Changes needed |
|---|---|---|
| `src/lang_trait.rs` | `lib.rs` | Trait + result types + vtable |
| `src/queries/layout.rs` | `queries/layout.rs` | Already uses vtable after Phase 1 |
| `src/queries/borrowck.rs` | `queries/borrowck.rs` | Already uses name detection after Phase 1 |
| `src/queries/mir_build.rs` | `queries/mir_build.rs` | Already uses vtable after Phase 1 |
| `src/queries/drop_glue.rs` | `queries/drop_glue.rs` | Already uses name detection after Phase 1 |
| `src/queries/mod.rs` | `queries/mod.rs` | Unchanged |
| `src/mir_helpers.rs` | `mir_helpers.rs` | Already generic |
| `src/abi_helpers.rs` | `abi_helpers.rs` | Already generic |
| `src/oracle.rs` | `oracle.rs` | Remove `dump_toylang_oracle` |
| `src/codegen_wrapper.rs` | `codegen_wrapper.rs` | Already generic |
| `src/file_loader.rs` | `file_loader.rs` | Already generic |
| `src/callbacks.rs` | `driver.rs` | Rename, wrap as `run_compiler()` |

### Stays in consumer (`toylangc`)
| Current path | New path | Notes |
|---|---|---|
| `src/main.rs` | `main.rs` | Simplified |
| `src/toylang/*` | `toylang/*` | Unchanged |
| `src/toylang/callbacks_impl.rs` | `callbacks_impl.rs` | impl LangCallbacks |
| `src/stub_gen.rs` | `stub_gen.rs` | Unchanged |
| `src/llvm_gen.rs` | `llvm_gen.rs` | Unchanged |

---

## Post-split API polish — COMPLETE ✓

- Added `type_names()` and `fn_names()` to `LangCallbacks` trait (6 methods total)
- `run_compiler` now takes just `(callbacks, rustc_args)` — 2 args instead of 5
- Codegen backend wrapper always installed (no-op if `generate_and_compile` returns `None`)
- Consumer `main.rs` reduced to: `run_compiler(toylang_callbacks, &args)`

---

## Verification — ALL COMPLETE ✓

Both phases done, plus API polish. Zero warnings. 5/5 tests pass via `./target/debug/toylangc`:
- host_test (Vec<Point>) — `toylangc/tests/point.toylang`
- counter_test (Counter struct + params) — `toylangc/tests/counter.toylang`
- pair_test (generic Pair<i32,i32> + ABI coercion) — `toylangc/tests/pair.toylang`
- layout_test (size/align verification) — `toylangc/tests/point.toylang`
- drop_test (drop glue) — `toylangc/tests/drop_point.toylang`
