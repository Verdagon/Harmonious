# Phase History

Per-phase writeups for erw's implementation phases (1–8) plus the fork-reduction stages (1–4). All complete as of 2026-04-19 (commit `c25aa4b`, zero-fork landing). Extracted from `docs/architecture/rust-interop-guide.md` at that landing as reference material — the canonical architecture doc now describes current state only; phase-by-phase history lives here.

For a running diary of day-to-day discoveries (as opposed to per-phase summaries), see `docs/historical/quest.md`.

---

## Phase 1 — Explicit trait method calls

`Trait::method(receiver, args)` syntax via `StaticCall`. The oracle resolves the trait DefId via `find_use_imported_trait_def_id`, finds the method in the trait's associated items, and builds args as `[Self, ...]` on the trait definition's DefId (`@TVIMDGAZ`). `Instance::expect_resolve` maps to the concrete impl at monomorphization time.

Fixed a latent `#[track_caller]` ABI bug along the way (`@TCHAPZ`): ~43 Vec methods have a hidden `&Location` pointer param that must be appended at every call site. Previously absent → undefined behavior that happened not to trigger in existing tests.

Tests: `test_trait_static_call_clone_vec`, `test_trait_static_call_result_discarded`, `test_ref_expr_basic`, + regression coverage.

## Phase 2 — Rust free function calls

Use-imported free functions like `stdout()`. Added `find_use_imported_fn_def_id` for `DefKind::Fn` re-exports, `rust_free_fn_return_type` / `rust_free_fn_param_types` for the FnCall path, plus `ArgTypeMismatch` error variant and `types_match()` for semantic `StructRef` vs `Struct` equivalence.

FnCall dispatch restructured to handle both extern declarations and use-imported free functions with real type args. All `instantiate_identity()` call sites got comments explaining why structural inspection is safe there.

Tests: 12 new, covering arg type checking, free function resolution, and existence sentinel (`Option::None` = "not found", `Some(vec![])` = "found, takes no args").

## Phase 3 — Byte string literals

`b"hello\n"` → `&[u8]` fat pointer `{ ptr, i64 }`. The lexer recognizes the `b"` prefix and handles escape sequences. In LLVM codegen, a global constant byte array is allocated and wrapped in a fat pointer struct.

Fixed a latent `ScalarPair` ABI bug: previously ScalarPair was collapsed into `Direct("{ ptr, i64 }")` (one LLVM param), but rustc's ABI wants two separate params (ptr + i64). Added `CoercedParam::Pair(String, String)` variant. No existing code exercised it because no existing code used `&[u8]`.

The FnCall path also gained ABI-correct declarations: previously it built LLVM function decls from toylang-internal types, causing silent data corruption for extern C functions. Both `FnCall` paths (extern-declared and use-imported) now query `coerced_param_types_for_instance`.

Tests: 9 new, covering byte string parsing, type resolution, and ScalarPair ABI.

## Phase 4 — I/O integration, GLOBALS split, ABI coercion

`Write::write_all(&stdout(), b"hello\n")` works end-to-end. Implementation required four distinct fixes:

1. **GLOBALS deadlock fix (`@GCMLZ`).** Split the single `Mutex<FacadeGlobals>` into `CONFIG: OnceLock<FacadeConfig>` (immutable), `DEFAULT_*: OnceLock<fn>` (immutable), and `MUTABLE_STATE: OnceLock<Mutex<FacadeMutableState>>` (mutable only). Query providers reading only config are lock-free, so they can execute during `generate_and_compile` without deadlock.

2. **sret handling in FnCall use-import path.** `stdout()` returns `Stdout` — rustc may return it via sret (indirect) depending on size. Added `coerced_return_type_for_instance` query and handling for all three modes (Direct, Indirect, Void), following the pattern from `get_or_resolve_rust_method`.

3. **ABI-coerced return types (`@ACRTFDZ`).** The FnCall path declared LLVM functions with `resolved_to_inkwell` (toylang's `[8 x i8]` for Stdout), but rustc returns `i64` (Direct scalar). LLVM treats aggregate vs scalar returns differently → garbage return values → segfault when later dereferenced. Fixed by using `parse_coerced_type(coerced_ret)` in the declaration, plus store-through-pointer reinterpretation when ABI type differs from toylang type.

4. **Broadened TyKind handling.** `rustc_ty_to_resolved_type` now handles previously-unsupported types (`u8`/`u16`/etc. as opaque `RustType`, `Str`, `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple`). `find_reexported_type` matches `DefKind::Enum` (fixes `Option`/`Result`). `resolved_to_rustc_ty` maps primitive type names back to `tcx.types.*` for round-tripping.

Tests: 6 new — `test_stdout_call`, `test_stdout_write_all`, `test_stdout_multiple_writes`, `test_write_all_result_bound`, `test_vec_pop_returns_option`, `test_rust_fn_returning_option_u8`.

## Phase 5 — toylang.toml and build orchestration

`toylangc build` reads `toylang.toml` and produces a working binary that can depend on arbitrary Rust crates — no hand-written `main.rs`, no linker flags, no knowledge of rustc plumbing on the user's side.

```
toylang.toml + main.toylang
         ↓
  toylangc build                         ←  build mode (manifest read #1)
         ↓
  generates .toylang-build/
    ├─ Cargo.toml                        (from [rust-dependencies])
    ├─ src/main.rs                       (auto-generated shim)
    └─ rust-toolchain.toml               (pins rustc toolchain)
         ↓
  cargo build
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

1. **Build mode** (`argv[1] == "build"`): parses manifest, generates `.toylang-build/`, spawns cargo.
2. **Wrapper mode** (`argv[1]` is a path ending in `rustc`): cargo's `RUSTC_WORKSPACE_WRAPPER` protocol. If `CARGO_PRIMARY_PACKAGE` is set, re-reads `toylang.toml` one directory up from `CARGO_MANIFEST_DIR` and compiles through the existing toylang flow; otherwise passes through to plain rustc via `rustc_driver::RunCompiler::new(args, &mut NoopCallbacks)`.
3. **Direct mode** (`--toylang-input <path>`): existing behavior, unchanged — used by integration tests.

This follows the Clippy/Miri pattern. Dependencies compile with real rustc; only the primary crate goes through toylangc. See `@MRRIWMZ` for why the manifest is re-read in wrapper mode instead of carrying the source path via a `TOYLANG_INPUT` env var.

The generated `Cargo.toml` includes an empty `[workspace]` table to mark itself as its own workspace root, preventing cargo from walking up into a parent workspace if the user's project happens to sit inside one (e.g., checked-in test projects under `toylangc/tests/standalone/*/`).

**Why `[rust-dependencies]` not `[dependencies]`?** Leaves room for `[toylang-dependencies]` when toylang has its own package ecosystem.

**Why a separate manifest instead of Cargo.toml?** Toylang controls the UX. Cargo is a tool in toylang's toolbox, not the other way around. If toylang moves away from rustc someday, the user-facing contract doesn't change.

See `docs/historical/plan-phase5-toylang-toml-build.md` for the full implementation history.

## Phase 6 — Wrappers for inline stdlib methods

All three steps done. Step 1: `#[inline(never)]` wrappers in `__lang_stubs` + rustc-fork partitioner patch. Step 2: `visibility_override` callback replaces the inline `__lang_stubs` string match in the fork. Step 3: two-family trait split (`LangPredicates` + `LangCallbacks: LangPredicates`) dissolves the "partitioner-time lock" exception. Tech debt #6 (FnCall CoercedParam dispatch) and the `toy_*` → `lang_*` rename also landed alongside.

### The problem

`Option::unwrap` and `Result::unwrap` are `#[inline(always)]`. Rustc never emits a callable symbol for them — they exist only as inlined IR at every Rust call site. Toylang compiles separately via Inkwell into `.o` files that rustc later links against; those `.o` files reference Rust by mangled symbol name. For inline-only methods, the symbol toylang declares (`extern "C" fn ..._unwrap(...)`) doesn't exist anywhere, and the linker fails with `undefined symbol`.

The same blocker applies to ~100+ other inline stdlib functions and to `#[track_caller]` functions (whose hidden ABI parameter can't be supplied from external IR — see `@TCHAPZ`).

Two prior attempts hit different failure modes. Their writeups are in `docs/historical/phase6-attempt1-mono-not-generated.md` and `docs/historical/phase6-attempt2-linkage-visibility.md`. The full design plan (now superseded by what's implemented) is at `docs/historical/plan-phase6-unwrap-wrappers-and-partitioner.md`.

### Solution shape

Generate a non-inline wrapper inside `__lang_stubs` for each blocked method. The wrapper:

```rust
#[inline(never)]
pub unsafe fn __toylang_option_unwrap<T>(o: *mut core::option::Option<T>) -> T {
    core::ptr::read(o).unwrap()
}
```

Three load-bearing details:

1. **`#[inline(never)]` is mandatory.** Without it, rustc may inline the wrapper itself, putting us back at "no callable symbol." This is enforced as LLVM `noinline`, not a hint.
2. **Receiver is `*mut`, not `T` by value.** This sidesteps ABI complications: for any T, the wrapper's first param is just a pointer (Direct(ptr)), which matches toylang's existing MethodCall convention. A by-value wrapper would force toylang to mirror rustc's PassMode for `Option<T>` (Pair, Direct, or Indirect depending on T) at every call site.
3. **`ptr::read` consumes the value.** Toylang doesn't track moves and doesn't run drop glue, so this is sound for the simple T's we use today (i32, u8). For wrappers around methods that consume self of types with destructors, this design needs revisiting.

Toylang dispatches `o.unwrap()` to the wrapper via `oracle::redirect_to_wrapper`. Called from BOTH `callbacks_impl::collect_toylang_fn_deps_inner` (the dep-registration site that drives codegen) AND `llvm_gen::get_or_resolve_rust_method` (the symbol-string consumer). Both sites must produce the same Instance so the symbol the wrapper's body gets mangled with matches the symbol the LLVM IR declares. See `@SMINCZ`.

### Forcing external linkage on the wrapper

Even with the wrapper instantiated and codegen'd, the second attempt discovered that rustc's CGU partitioner internalized the symbol. For an executable crate, generic `#[inline(never)]` items default to `Visibility::Hidden + can_be_internalized = true`. `internalize_symbols` flipped the wrapper to `Linkage::Internal`. Internal-linkage symbols are invisible to externally-linked `.o` files; the linker fails again.

The Phase 6 fix used a rustc fork patch (`VISIBILITY_OVERRIDE_HOOK`). Under the current zero-fork architecture (stage 4c), the plugin's `collect_and_partition_mono_items` override sets `(Linkage::External, Visibility::Default)` directly on `__lang_stubs` items in the returned CGU list — the LLVM backend reads that linkage without re-derivation. See `rust-interop-guide.md` §2.2 / §10.6.4 and `docs/reasoning/rustc-fork-design-space.md` §4.1 for the complete mechanism.

### Side fix: `push_arg_for_rust_call` ABI dispatch

Phase 6's `test_vec_pop_unwrap` test exposed a pre-existing latent bug in `llvm_gen.rs::push_arg_for_rust_call`: every Rust method/trait-static arg that wasn't a ScalarPair was passed by pointer, regardless of whether rustc's ABI declared it as `PassMode::Direct(scalar)`. For `Vec::push(&mut self, value: i32)`, the LLVM declaration is `(ptr, i32, ptr)` but the call site passed `(ptr, ptr, ptr)`. LLVM's opaque-pointer mode accepts this silently; on AArch64 the pointer's low 32 bits land in `w1` and Vec::push stores them as the user's i32. The Vec ends up holding a stack-pointer fragment instead of `99`. Forty-plus existing `v.push(int)` test calls all suffered this corruption, but none of them ever read the stored value back (only `.len()`, `.capacity()`, or clone).

Fix: `push_arg_for_rust_call` dispatches per-arg on `&CoercedParam`. 4 call sites (StaticCall sret/non-sret, MethodCall sret/non-sret) clone `info.coerced_params`, then pass `&coerced_params[1 + i]` per arg.

### Tests added

| Test | What it verifies |
|------|-----------------|
| `test_option_unwrap_basic` | Option<i32>::unwrap from a shim, basic round-trip |
| `test_result_unwrap_basic` | Result<i32, i32>::unwrap, two-arg generic wrapper |
| `test_option_unwrap_result_discarded` | unwrap as ExprStmt (return value discarded) |
| `test_unwrap_arithmetic_chain` | `o.unwrap() + 2i32` — result-typed expression |
| `test_unwrap_two_options_separately` | Two unwrap call sites — wrapper symbol caching |
| `test_vec_pop_unwrap` | Vec::pop().unwrap() — exercises both the wrapper AND the Vec::push ABI fix |

## Phase 7 — Standalone test projects (9/9 + 1 follow-up)

Standalone test projects under `toylangc/tests/standalone/<crate>_test/`, each with a `toylang.toml` and `main.toylang`. No Rust files, no glue. Each project proves toylang can link against and call into a specific Rust crate from crates.io via `toylangc build`.

**Done (9 crates + 1 follow-up):**

- **`uuid_test`** — smoke test bridging Phase 5 (cargo resolves deps) to Phase 7 (toylang calls into deps). `Uuid::new_v4();` + `Write::write_all(&stdout(), b"uuid ok\n");`. Surfaced three latent issues that landed as `@MBMRVZ`, `@RTMEIZ`, and the `[workspace]` note.
- **`indexmap_test`** — 3-arg explicit generics. `IndexMap::new<i32, i32, RandomState>();`. First-try pass. The pre-execution risk that `new()` lives on an S-fixed impl block (not the open `impl<K, V, S>`) dissolved — supplying `RandomState` explicitly matched rustc's elided default.
- **`regex_test`** — `let re = Regex::new("\\d+").unwrap();`. Surfaced **two** latent compiler gaps: (1) dispatch classifier misrouted `RustStruct::method(args)` with non-empty args to the trait path; (2) inherent StaticCall codegen hardcoded `build_call(func, &[])`, silently discarding every arg. Both fixed — see `@IVTDBTZ`. First Phase 7 test to stress four features in composition: Phase 5 build, `@UTAIRZ` `&str` ABI, Phase 6 `.unwrap()` wrapper (first non-stdlib `Result<T, E>`), and Phase 4 I/O.
- **`toml_test`** — `let val = from_str<Value>("").unwrap();`. First integration test of a use-imported **generic free function with an explicit type arg**. Composed six features in one 12-line program.
- **`serde_json_test`** — `let val = from_str<Value>("null").unwrap();`. Surfaced and fixed **`@ELASZ`** — the latent gap where ten `GenericArgs`-building sites hand-rolled args from user type args only, dropping lifetime slots. `serde_json::from_str<'a, T: Deserialize<'a>>` has an early-bound lifetime that ICEd rustc. Fix: shared helper `oracle::build_generic_args_for_item` using `ty::GenericArgs::for_item` with `tcx.lifetimes.re_erased` for lifetime slots. Unblocks any future Rust API with early-bound lifetimes.
- **`glob_test`** — `let result = glob("*.rs");`. First-try pass. First Phase 7 test to bind a `Result` without calling `.unwrap()` on it (intentionally — first-pass scope discipline).
- **`rand_test`** — `let rng = thread_rng();`. First-try pass. First Phase 7 test to bind a non-Copy, non-Result Rust type held by value, with Drop-glue codegen running naturally at end-of-`main`. Pinned to `rand = "0.8"` (0.9 renamed `thread_rng` → `rng`).
- **`reqwest_test`** — `let client = Client::new();`. First-try pass. **First end-to-end exercise of Phase 5's detailed-dep path with a feature flag** — `reqwest = { version = "0.11", features = ["blocking"] }`. First standalone test with a deep-transitive-dep crate (~100 deps pulling in tokio, hyper).
- **`clap_test`** — `let cmd = Command::new<&str>("app");`. First-try pass. The prior "blocked on `impl Into<Str>` synthetic generic" framing was reasoning-to-conclusion without empirical verification — the minimal probe took 4 seconds and disproved the premise. Rust's `impl Trait` in argument position desugars to a synthetic type parameter that toylang's `build_generic_args_for_item` (the `@ELASZ` helper) already consumed uniformly. Meta-lesson: empirical probes beat reasoning-to-conclusion.
- **`reqwest_get_test`** (follow-up) — `let result = get<&str>("");`. First-try pass. Retired the deferred "novel `&T`-type-arg shape" risk from `reqwest_test`. Uses an empty-string URL so `IntoUrl::into_url` / `Url::parse("")` fails synchronously before any network activity; Result bound but not unwrapped. Sixth consecutive first-try Phase 7–style test.

Derive macros are syntactic sugar for trait impls. The underlying APIs are always available imperatively. All standalone tests follow the same 10–20 line pattern printing `"<crate> ok\n"`.

## Phase 8 — Test harness dedup

`toylangc/tests/standalone_tests.rs` builds each standalone project via `toylangc build` and asserts expected output. Collapsed nine near-identical `test_standalone_*` function bodies behind a single `run_standalone_test(project_name: &str, expected: &str)` helper. Each test is now a one-line call preceded by its explanatory comment block; the ~40-line boilerplate per test (build-dir cleanup, `run_build` invocation, binary path join, `Command::new(&bin)` with `DYLD_LIBRARY_PATH`/`LD_LIBRARY_PATH` inheritance, stdout containment check) lives in the helper.

Net 596 → 334 lines (-44%). The helper enforces the project-dir-name = `[project].name` = binary-name convention. Explanatory comments on each test are preserved — they document **why** each test exists (which compiler gap or Rust API shape it probes), which is load-bearing context for future maintainers. Second-order effect: adding a future standalone test costs one line + comment + two files.

---

## Fork-reduction stages (1–4) + architecture-readiness stage 5

After Phase 8, the fork-reduction roadmap. Five stages planned, four shipped stages 1–4 reduced the fork from 5 patches to 0. Stage 5 originally scoped alongside 4 was deferred; it shipped later as the two-crate architecture migration (see below the stage-4 section).

### Stage 1 — `LangCallbacks` job-split (commit `ed2e692`)

Split the former `monomorphize_fn` callback into two single-purpose hooks: `collect_generic_rust_deps` (called from `per_instance_mir`, returns Rust deps) and `notify_concrete_entry_point` (called from `symbol_name`, returns extern symbol + drives internal-callee stashing). Eliminated an undocumented ordering dependency between the two query providers. Toylang-side walker split: `collect_rust_deps_recursive` with local cycle guard + `walk_and_stash_internal_callees` with persistent `walked_entry_points` dedup. Prerequisite for stage 3.

### Stage 2 — Cross-crate backend cleanup (commit `b345162`)

Removed a single-crate-compile assumption from the backend's MonoItems walk (the `def_id.as_local()` filter at `llvm_gen.rs:1897`, dead under current architecture but load-bearing-wrong under any future cross-crate model). Consolidated the cross-crate-safe `__lang_stubs` check into a new facade helper `is_from_lang_stubs_safe` using `tcx.def_path(def_id).data` structural walk. Prerequisite for stage 4's partitioner-override work.

### Stage 3 — `optimized_mir` override (commits `ce437ae` / `bf770ae` / `da7ad87`)

Retired the custom `per_instance_mir` query in favor of a `Config::override_queries` override on rustc's existing `optimized_mir` query. Rust-dep discovery runs once per consumer DefId with Param-bearing outputs — rustc's mono collector substitutes per caller. Fork dropped patches 1/2/4 (query definition, collector fallback, default provider); patch 3 (codegen skip) was reshaped into a facade-installed `CODEGEN_SKIP_HOOK` sibling of the existing `VISIBILITY_OVERRIDE_HOOK`. Net: 5 patches → 2, both consumer-agnostic `OnceLock<fn ptr>` statics. Consumer-side oracle gained a thread-local Param-name→index map (`oracle::ActiveParamMap`). See `docs/reasoning/dep-discovery-approaches.md` and `docs/historical/handoff-optimized-mir-migration.md`.

### Stage 4 — CodegenBackend plugin + zero-fork landing

Four sub-commits:

- **4a** (`1d862f4`): `collect_and_partition_mono_items` override filters consumer items out of rustc's CGU list; plugin takes them through its own codegen path. `CODEGEN_SKIP_HOOK` becomes a no-op safety net (still installed).
- **4b** (`13d8f12`): `CODEGEN_SKIP_HOOK` retired (facade + fork). Fork at 1 patch.
- **4c step 1** (`51f0c5e`): `upstream_monomorphizations_for` override wired (~40 LoC scaffolding, originally for separate-crate migration; kept because the override is also load-bearing for consumer generic-wrapper routing).
- **4c** (`d044560`): **Outcome A landing.** Plugin's partitioner override mutates `MonoItemData.linkage` directly — LLVM reads `data.linkage` without re-derivation, so `(Linkage::External, Visibility::Default)` on `__lang_stubs` items survives to emission. `VISIBILITY_OVERRIDE_HOOK` retired (facade + fork). Fork at 0 patches.
- **4d** (`c25aa4b`): toolchain switch (vanilla `nightly-2025-01-15`) + full doc pass + handoff move to historical.

The original stage-4 roadmap scoped separate-crate stubs + `FileLoader` retirement as part of the work. A TL investigation mid-stage-4 (see `docs/historical/handoff-codegen-backend-plugin.md` §6.9) established that Outcome A reaches zero-fork without the separate-crate migration. Stage 5 was deferred and eventually shipped as a separate roadmap (see below); `FileLoader` preserved through stages 5a/5b as the single-crate stub-injection mechanism.

### Stage 5 — Two-crate architecture (stubs become a real rlib)

Vale-fork-readiness work. Migrates both compile modes from FileLoader-injected stubs to a two-crate architecture (stubs in their own rlib; user code in a separate crate that depends on it). Preserves zero-fork; the work is about what the integration *shape* looks like for someone else to build on, not about what rustc requires.

- **5a** (`6bda10c`): cross-crate oracle. Unified the eight `find_*` name-resolution helpers in `toylangc/src/oracle.rs` behind a single `resolve_rust_path(tcx, path, kind_filter)` walker that tries the local crate root first, then falls back to walking the extern `__lang_stubs` rlib's `module_children`. Behavior-preserving under single-crate FileLoader (five of the helpers collapse to one-liner adapters); becomes load-bearing under 5b's two-crate shape.
- **5b** (`b6a2bf6`): wrapper-mode two-crate. `toylangc build` emits a two-member Cargo workspace at `<project>/.toylang-build/` with `lang_stubs_crate/` (rlib, contains stub struct defs + `pub use` re-exports + extern decls; src from `stub_gen::generate`) and `user_bin/` (bin, path-depends on the stub rlib, `fn main() { __toylang_main(); }` shim). `CARGO_PRIMARY_PACKAGE` + `CARGO_PKG_NAME` distinguish the two compiles at the wrapper; `is_downstream_of_stubs` gates the user-bin compile from running the codegen side of `generate_and_compile`. `upstream_monomorphizations_for` (scaffolded in 4c as ~40 LoC) becomes load-bearing for the first time, routing generic consumer wrappers (`__toylang_option_unwrap<T>`) locally in the user-bin compile.
- **5c.1** (`91cad25` + `05fed63`): integration-test orchestration. Cargo `[[package]]` unique-per-project naming in `write_stub_crate` (cargo dedupes by `(name, version, source)`, so every project needs a distinct package name under a shared `CARGO_TARGET_DIR`). Shared `CARGO_TARGET_DIR` lets `test_helpers` + crates.io deps compile once across the suite. `CARGO_INCREMENTAL=0` scoped to the test harness (risks.md §B6: rustc's incremental cache can short-circuit side-effecting query providers). 3 canary projects landed.
- **5c.2** (`a2f06ea` + `6d65831`): 93-test migration + `test_helpers` expansion. Established the ABI conventions for test_helpers (primitives via `#[no_mangle] pub extern "C"`, fat pointers + `Option`/`Result` via the same plus `#[allow(improper_ctypes_definitions)]`). `build.rs` propagates `features = [...]` from `toylang.toml` to the stub rlib's `src/lib.rs` as well as the user bin's `src/main.rs`.
- **5c.3** (`1ae7fd4`): stub_gen unit-struct fix (`pub struct Foo;` instead of `pub struct Foo(());` — aligns source field count with `layout_of(0-field)`, silences a `build_struct_type_di_node` "index out of bounds" ICE that fires when opaque consumer types appear inside `Vec<Foo, Global>` debuginfo). Two new harness helpers: `run_integration_project_expects_error` (substring match against toylangc stderr) and `run_integration_project_check_callbacks` (reads `TOYLANG_LOG_PATH` output). +22 test migrations.
- **5c.4** (`b3e276d` + stage-5 landing commit): layout probe tests via `run_integration_project_check_build_stderr` harness; `lang_layout_of` log augmented with `size=N align=M`. Stage-5 landing: retire `FileLoader` + `file_loader.rs` + `generate_stubs` trait method, retire direct mode (`--toylang-input` argv handling + `run_direct_mode` + `extract_registry`). Delete `toylangc/tests/integration_tests.rs` (all 129 direct-mode tests — 127 have integration_projects counterparts; 2 accepted coverage loss: pre-existing bool codegen bug + Rust-side `drop_in_place`+runtime.o test with no wrapper-mode equivalent). `is_from_lang_stubs` / `is_from_lang_stubs_safe` collapse to a single `tcx.crate_name == "__lang_stubs"` check. §6.10 `#[linkage]` probe vacuously satisfied: the attribute was never emitted by stub_gen; partitioner-set-linkage has been the sole mechanism all along.

Final test count: 209 (67 unit + 127 integration_projects + 15 standalone). Two-crate architecture complete; FileLoader retired; direct mode retired; zero-fork preserved.

---

## Deferred work

### Duck-typed method resolution

Instead of requiring explicit trait qualification `Trait::method(...)`, the compiler could search all trait impls automatically when `receiver.method(args)` doesn't find an inherent method. Using `tcx.all_traits()` and `tcx.for_each_relevant_impl()`, it would iterate all traits to find one with a matching method that has an impl for the receiver's concrete type. This would let users write `out.write_all(bytes)` instead of `Write::write_all(&out, bytes)` — cleaner syntax at the cost of searching potentially thousands of traits. Deferred because explicit qualification is simpler and avoids ambiguity when multiple traits define the same method name.

---

## See also

- `docs/architecture/rust-interop-guide.md` — current architecture (as shipped through all the phases above).
- `docs/reasoning/architecture-decisions.md` — why each design choice was made, with alternatives.
- `docs/historical/quest.md` — running project diary (day-to-day, more granular than per-phase).
- `docs/reasoning/rustc-fork-design-space.md` — the design space that led to the fork-reduction stages landing.
- `docs/historical/handoff-*.md` — per-stage junior handoff docs (preserved for reference).
