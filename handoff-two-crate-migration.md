# Handoff: Two-crate architecture migration (stage 5)

**Task owner:** a junior engineer joining the erw project. **Previous exposure to stages 1–4 not assumed.** The first few sections of this doc give you the context you need; beyond that we go into specifics.
**Branch:** work on `main` directly.
**Estimated effort:** 3–5 weeks for a junior, including ramp time.
**Risk level:** medium. Scope is well-bounded (four clearly-separated sub-phases); the tricky architectural piece — cross-crate type resolution in `oracle.rs` — was landed in 5a. No toolchain rebuild required; erw is already zero-fork.

**Landing status (2026-04-19):**
- **Sub-stage 5a landed** (cross-crate oracle). Commit `99b10df`-ish — see git log.
- **Sub-stage 5b landed** (wrapper-mode two-crate). Commit `b6a2bf6`. 211/211 green.
- **Sub-stage 5c pivoted mid-attempt.** An earlier 5c attempt (in-process `rustc_driver::RunCompiler` twice per test) surfaced a real architectural question — which compile owns consumer codegen when tests use body-less toylang fns backed by bin-local Rust fixtures. TL call: the fixture pattern is test-only and violates "tests should act like production users," so the right move is to migrate tests to the production pattern rather than architect around the test-only one. **5c now scoped as F-wide**: migrate all 129 integration tests to standalone-style projects, retire direct mode and FileLoader entirely. Details in §4.3 and §5 below (rewritten for F-wide; previous in-process `rustc_driver` approach is superseded and intentionally not preserved inline — see git history of this doc if you need the old plan).

---

## 1. Project crash course (read even if you think you don't need it)

erw is a two-crate Rust workspace:

- **`rustc-lang-facade`** — a reusable library for embedding custom languages into rustc's compilation pipeline via query providers.
- **`toylangc`** — an example consumer compiler for "toylang" (a small language) that uses the facade to demonstrate the architecture end-to-end.

The framework's core trick: toylang code calls into real crates.io Rust crates (`uuid`, `serde_json`, `clap`, etc.), but rustc doesn't know about toylang's type system or codegen. The facade bridges the gap via rustc query overrides — rustc believes toylang types exist but treats them opaquely; rustc compiles the Rust parts and toylang's own Inkwell LLVM backend compiles the toylang parts; the two `.o` files link together into a working binary.

**Three architectural concepts you must internalize before starting:**

1. **Interleaved monomorphization.** Rustc's mono collector is the only entity that walks both Rust source and (via the facade) consumer source. When Rust code calls a toylang function with concrete Rust type args, we need to tell rustc what the toylang function depends on so rustc can queue those deps for codegen. The facade does this via the `optimized_mir` override — returns a synthetic MIR body with `ReifyFnPointer` casts that the collector walks. See `docs/reasoning/why-interleaved-monomorphization.md` for the full seven-case taxonomy.

2. **Opaque stubs.** Consumer types appear to rustc as 0-field opaque structs with `unreachable!()` bodies, injected into the user's crate via rustc's `FileLoader` trait (today — that's what stage 5 retires). Rustc parses and type-checks them as normal Rust; at codegen time our overrides kick in.

3. **Zero-fork architecture.** As of stage 4 (commit `c25aa4b`), erw builds against vanilla `nightly-2025-01-15` with no rustc fork patches. All rustc integration flows through `Config::override_queries` + `FileLoader` + a `CodegenBackend` wrapper. Stage 5 preserves zero-fork; we're not touching rustc internals.

### The fork-reduction roadmap (history)

erw started with a 5-patch rustc fork. Four stages shipped fork-reduction work:

- **Stage 1** (commit `ed2e692`): split the `monomorphize_fn` callback into two single-responsibility hooks (`collect_generic_rust_deps` + `notify_concrete_entry_point`). Prerequisite for stage 3.
- **Stage 2** (commit `b345162`): removed a single-crate-compile assumption from the backend. Added `is_from_lang_stubs_safe` helper — **you'll use this heavily in stage 5's cross-crate oracle work**.
- **Stage 3** (commits `ce437ae`/`bf770ae`/`da7ad87`): migrated from a custom `per_instance_mir` fork-query to an `optimized_mir` override. Fork: 5 patches → 2.
- **Stage 4** (commits `1d862f4`/`13d8f12`/`51f0c5e`/`d044560`/`c25aa4b`): CodegenBackend plugin via `collect_and_partition_mono_items` override. Plugin takes over partitioning + sets linkage directly on CGU items. Fork: 2 patches → 0.

Per-phase details: `docs/historical/phase-history.md`. Stage-4 detailed handoff (now historical): `docs/historical/handoff-codegen-backend-plugin.md`.

### What stage 5 is (and isn't)

**Stage 5 target:** migrate both compile modes (wrapper mode and direct mode) from FileLoader-injected stubs to a two-crate architecture (stubs in their own rlib; user code in a separate crate that depends on it).

**Why now, after zero-fork already shipped:**

- **Architectural robustness.** The stage-4 Outcome A approach sets linkage via a post-partition mutation that survives to LLVM emission because LLVM reads `MonoItemData.linkage` directly. This works but depends on an internal timing assumption (`docs/architecture/risks.md` §3 B2). Two-crate with `#[linkage = "external"]` routes through rustc's source-attribute fast-path at `partitioning.rs:755-756` — no timing-sensitive mutation. If rustc ever restructures internalization timing (20–30% probability over 5 years), two-crate is unaffected; Outcome A silently breaks.
- **Vale-fork readiness.** Vale's planned interop story (`why-interleaved-monomorphization.md` Cases 1a/1b/3/4/6) includes Rust-as-top-level scenarios where Rust code imports toylang-shaped libraries. Single-crate FileLoader forecloses these architecturally. Two-crate is the precondition for any future "Rust depends on toylang" scenario and matches what a greenfield consumer like Vale would build from day one. POC #2's §4.3.D already framed this: "For a greenfield consumer like Vale that's already doing the plugin work, separate-crate integration is absorbed at approximately zero marginal cost." Stage 5 brings toylang up to that same shape.

**Stage 5 is NOT:**

- Changing rustc fork patches — there aren't any.
- Changing Outcome A's partitioner-set-linkage mechanism — that stays and works. It just stops being the *sole* mechanism; `#[linkage]` in the stub rlib is an additional belt-and-suspenders.
- Replacing the plugin or `codegen_wrapper.rs` — those stay.
- Adding new nightly features — we're migrating to a two-crate shape where `#![feature(linkage)]` can live at a real crate root (the stub rlib's `src/lib.rs`). If `§6.9 probe` (see §6.10 below) shows the attribute is unnecessary under plugin-set-linkage, it's optional.

---

## 2. Required reading before you code (4–6 hours)

### Tier 1 — project orientation (90 min)

1. `/Users/verdagon/erw/CLAUDE.md` — project-wide instructions. Compiler laws, build conventions.
2. `/Users/verdagon/erw/HANDOFF-TL.md` — project state summary.
3. `/Users/verdagon/erw/docs/architecture/rust-interop-guide.md` — the canonical architecture doc. Parts 1–5 (compilation flow, query providers, opaque stubs, LLVM backend, global state) are load-bearing.
4. `/Users/verdagon/erw/docs/architecture/risks.md` — risk taxonomy. Especially §3 B2 (the Outcome A timing assumption) — understanding this is the *motivation* for stage 5.

### Tier 2 — stage-5 design (2 hours)

5. `/Users/verdagon/erw-poc-separate-crate-stubs/findings.md` — the POC that verified separate-crate stubs work. Read the Executive Summary, Step 3 (`nm` inventory confirming the stub rlib compiles clean with `#![feature(linkage)]`), §4.3.D (plugin-mode subsumes POC risks #8/#9 — this is WHY the migration absorbs cleanly under the current plugin architecture). Note that the POC predates stages 1/2/3/4, so trait signatures and shipping architecture have drifted — trust current `main` over POC code details.
6. `/Users/verdagon/erw/docs/reasoning/why-interleaved-monomorphization.md` — Cases 1a/1b/3 discussion (seven-case taxonomy). Stage 5 preserves these cases' architectural feasibility for future consumers.
7. `/Users/verdagon/erw/docs/historical/handoff-codegen-backend-plugin.md` §4.4 and §5 sub-stage 4c — the prior stage-4 junior's planning notes. Useful because §4.4b specifically sketched the direct-mode in-process two-crate approach you'll implement in sub-stage 5c. The prior junior attempted sub-stage 4c.1 and rolled back at a cross-crate oracle blocker — that blocker is sub-stage 5a's focus.
8. `/Users/verdagon/erw/docs/reasoning/dep-discovery-approaches.md` — Approach A vs B. Stage 3 background; useful for understanding why `collect_generic_rust_deps` is shaped the way it is (matters when you touch `oracle.rs`).

### Tier 3 — context for debugging (read as needed)

9. `/Users/verdagon/erw/docs/reasoning/rustc-fork-design-space.md` — the design-space analysis that led to stages 3 and 4.
10. `/Users/verdagon/erw/docs/reasoning/architecture-decisions.md` — why each major architectural choice was made.
11. `/Users/verdagon/erw/docs/arcana/` — cross-cutting concerns. Reference when you hit `@ID` comments in the code. Especially `@DPSFDOZ` (don't use `def_path_str` outside `generate_and_compile`) and `@GCMLZ` (mutex locking discipline).
12. `/Users/verdagon/erw/docs/usage/testing.md` — build & test commands, build-redirect convention.

---

## 3. Current surface (verified file:line references)

### Cross-crate oracle blocker (5a's focus)

The prior stage-4 junior hit this and escalated rather than pointwise-patch it. The oracle's type-resolution family scans LOCAL items only. Under two-crate, the user bin compile needs to resolve Rust types like `Stdout` through the extern `__lang_stubs` rlib's re-exports (`pub use std::io::Stdout`), which local-only scans miss.

Roughly 8 sites in `toylangc/src/oracle.rs` (~780 lines) need to route through a cross-crate-aware helper:

- `find_local_struct_def_id`
- `find_rust_type_def_id`
- `find_reexported_type`
- `find_use_imported_trait_def_id`
- `find_use_imported_fn_def_id`
- `rust_free_fn_return_type` / `rust_free_fn_param_types`
- `resolved_to_rustc_ty` / `resolved_to_rustc_ty_with_subst`
- (Grep for `module_children_local` and `tcx.hir_crate_items().definitions()` to find all call sites.)

The failure mode during the prior stage-4c attempt: the 10 failing standalone tests all panicked with `StructRef 'Stdout' should be resolved to Struct before codegen` — exactly the "local-only scan misses cross-crate re-export" pattern.

### FileLoader (to be retired in 5d)

- `rustc-lang-facade/src/file_loader.rs` — current stub-injection mechanism. Intercepts rustc's reads of `__lang_stubs.rs` and returns content from the consumer's `generate_stubs()` callback. Clean implementation of rustc's `FileLoader` trait; stable, sanctioned mechanism — we're retiring it not because it's broken but because two-crate subsumes it.
- `rustc-lang-facade/src/driver.rs` — where `LangFileLoader` gets installed into rustc's `Config::file_loader`.

### Build orchestration (5b's focus)

- `toylangc/src/build.rs` — the `toylangc build` command. Today writes a single-crate Cargo project with user code + a `src/main.rs` shim. Under 5b it writes a two-member workspace (stub rlib + user bin, per POC #2's pattern).
- `toylangc/src/stub_gen.rs` — generates `__lang_stubs.rs` content. Today the output gets FileLoader-injected. Under 5b, the wrapper-mode path writes it to the stub rlib's `src/lib.rs`.

### Direct-mode test harness (5c's focus)

- `toylangc/tests/integration_tests.rs` — 129 integration tests that invoke the compilation pipeline directly via in-process `rustc_driver::RunCompiler` with a single toylang source string. Under 5c, each test allocates a tempdir (via `tempfile`), writes stub rlib + user bin sources to disk, invokes `rustc_driver::RunCompiler` twice per test (once for the rlib, once for the bin with `--extern`).
- Most test fixtures currently call a helper like `compile_toylang_direct(source)` (exact name may vary — grep to find). Stage 5c adds `compile_toylang_direct_two_crate(source)` and migrates call sites.

### What doesn't change in stage 5

- **Partitioner override (`rustc-lang-facade/src/queries/partition.rs`)** — the Outcome A mechanism from stage 4c stays. It still filters consumer items out of CGUs and forces `(External, Default)` linkage on surviving `__lang_stubs` items. Under the two-crate architecture, the stub rlib's `__lang_stubs` items can ALSO carry `#[linkage = "external"]` source attributes as belt-and-suspenders; the partitioner override remains operational.
- **`upstream_monomorphizations_for` override** at `rustc-lang-facade/src/queries/upstream_monomorphizations.rs` (commit `51f0c5e`, ~40 LoC) — already wired. Routes generic wrappers (like `__toylang_option_unwrap<T>`) locally in the user bin compile rather than deferring to the rlib's non-existent monomorphization. **This will fire for real under stage 5 for the first time** — under the current FileLoader-single-crate architecture there is no upstream rlib so the override is inert.
- **The six query overrides** (`optimized_mir`, `symbol_name`, `layout_of`, `mir_shims`, `collect_and_partition_mono_items`, `upstream_monomorphizations_for`) all stay. Their bodies may need touch-ups (especially anything that assumes `FileLoader` or single-crate) but the architectural shape is unchanged.

---

## 4. Proposed design

### 4.1 Cross-crate oracle (sub-stage 5a)

Add a unified helper in `oracle.rs`:

```rust
fn resolve_rust_path(tcx: TyCtxt<'_>, path: &[&str]) -> Option<DefId> {
    // Try local root first (preserves current behavior for Rust items
    // that are defined locally — stdlib types re-exported via the stub
    // rlib's `pub use` ARE effectively local to the stub rlib's compile
    // when we're compiling that crate).
    if let Some(def_id) = walk_module_children_local(tcx, path) {
        return Some(def_id);
    }
    // Then walk the extern __lang_stubs rlib (user-bin-compile context —
    // consumer types live in the rlib, not locally).
    let stubs_cnum = tcx.crates(()).iter().copied().find(|&c| {
        tcx.crate_name(c).as_str() == "__lang_stubs"
    })?;
    walk_module_children_from(tcx, stubs_cnum.as_def_id(), path)
}
```

The two helpers `walk_module_children_local` and `walk_module_children_from` share body structure — walk a module's `module_children` (local via `tcx.module_children_local`, extern via `tcx.module_children`), match by name, filter by `DefKind`. Factor out the common walker.

Migrate each of the ~8 existing `find_*` functions to delegate to `resolve_rust_path` (possibly passing a `DefKind` filter as a parameter). Preserve their existing signatures so callers don't have to change.

**Test as you go.** After migrating each `find_*`, run the full suite. The migration should be behavior-preserving under the current FileLoader-single-crate architecture because cross-crate isn't exercised yet. If tests regress, the helper's local-walk implementation drifted from the function it replaced.

### 4.2 Wrapper-mode two-crate (sub-stage 5b)

Generate a two-member Cargo workspace in `.toylang-build/`:

```
.toylang-build/
├── Cargo.toml                          # workspace root
├── rust-toolchain.toml                 # applies to both members
├── lang_stubs_crate/
│   ├── Cargo.toml                      # rlib crate type
│   └── src/lib.rs                      # generated from stub_gen
└── user_bin/
    ├── Cargo.toml                      # path dep on lang_stubs_crate
    └── src/main.rs                     # user entry shim
```

The stub rlib's `src/lib.rs` content:

```rust
#![feature(linkage)]   // only if §6.10 probe says we need it

// Opaque type definitions
pub struct Counter(());
pub struct Pair<A, B>(std::marker::PhantomData<(A, B)>);

// pub use re-exports for Rust types the consumer references
pub use std::io::Stdout;
pub use uuid::Uuid;
// ...

// Wrapper functions with #[linkage] if needed (§6.10)
#[inline(never)]
#[linkage = "external"]   // again, only if §6.10 says so
pub unsafe fn __toylang_option_unwrap<T>(o: *mut core::option::Option<T>) -> T {
    core::ptr::read(o).unwrap()
}

// Signature-only stubs for consumer functions
pub fn make_counter() -> Counter { unreachable!() }
pub fn wrap<T>(x: T) -> Wrapper<T> { unreachable!() }
```

The user bin's `src/main.rs` content:

```rust
use __lang_stubs::*;

fn main() {
    // user's toylang-compiled entry point is injected via the facade's
    // existing consumer-codegen path; this shim just links against it
}
```

POC #2's `build.rs` on branch `poc/separate-crate-stubs` has the cargo orchestration patterns (workspace layout, path dep, `RUSTC_WORKSPACE_WRAPPER` routing, `CARGO_PRIMARY_PACKAGE` env var to distinguish stub-rlib-compile from user-bin-compile). Use them as a starting point — but expect to update for current trait signatures.

**Crucially:** the plugin's `RUSTC_WORKSPACE_WRAPPER` fires for BOTH crate compiles under two-crate. The plugin needs to behave correctly in both. For the stub rlib compile, `collect_generic_rust_deps` / `notify_concrete_entry_point` fire on consumer items (they're local); for the user bin compile, `upstream_monomorphizations_for` routes generic wrappers back to the bin compile's local codegen. The facade's `callbacks_impl.rs::notify_concrete_entry_point_inner` will be called in BOTH compiles for the same consumer entry-point Instance — under the stub rlib compile it should do the internal-callee walk; under the user bin compile it should skip the walk (already done). Gate the walk behind an `is_downstream_of_stubs` predicate (follow the `after_rust_analysis` precedent).

### 4.3 Migrate integration tests to standalone-style (sub-stage 5c, F-wide)

Each of the 129 integration tests becomes a self-contained toylang project that runs via `toylangc build` — the same path the 15 existing standalone tests use. This makes test invocations match real toylang-user workflow: toylang source + `toylang.toml` + real Rust crate deps, compiled via cargo, binary run with output captured.

Layout per test:

```
toylangc/tests/integration_projects/<test_name>/
├── main.toylang
├── toylang.toml             # [project] + [rust-dependencies]
└── expected_output.txt      # or inline assertion in the harness
```

Tests that currently declare body-less `fn println_int(x: i32);` in toylang source + provide a matching `pub fn println_int` in an inline Rust fixture get migrated to depend on a shared `test_helpers` crate:

```
toylangc/tests/test_helpers/
├── Cargo.toml
└── src/lib.rs               # pub fn println_int, pub fn read_int, etc.
```

Test's `toylang.toml`:

```toml
[project]
name = "my_test"

[rust-dependencies]
test_helpers = { path = "../../test_helpers" }
```

**Error-asserting tests** (parser / type-resolver error tests) today use `assert_matches!(result, Err(TypeResolveError::FooBar { ... }))`. Under wrapper mode, errors surface as `toylangc build` stderr — the harness captures that and matches against expected substring or regex. A new helper like `run_integration_test_expects_error(name, error_pattern)` sits alongside the existing `run_standalone_test` in the test runner. **Granularity regression is accepted:** moving from structural-match to string-match loses some precision, but production users see error strings, not error enum variants — tests now match what users would see.

**Cargo orchestration (runtime-critical).** Without care, each of 129 tests gets its own `.toylang-build/target/`, and cargo rebuilds `test_helpers` (and any shared artifacts) fresh for every test. That's naive-path runtime of 30–60 minutes. The target runtime is 5–15 minutes, achieved by sharing state aggressively:

- **Single shared `CARGO_TARGET_DIR`.** The test harness exports `CARGO_TARGET_DIR=<shared path>` (e.g., `$CARGO_MANIFEST_DIR/tests/integration_target/` or a deterministic `/tmp/` path) before invoking `toylangc build`. All 129 test invocations write to the same cargo target directory. `test_helpers` compiles once across the suite; subsequent tests hit the cache. Cargo's own target-dir locking (`.cargo-lock`) handles concurrent-build coordination cleanly — expect some serialization during the compile phase, none during test-run phase.
- **`test_helpers` as a stable reusable artifact.** All test `toylang.toml` files declare it via the same relative path: `{ path = "../../test_helpers" }` (or an absolute path via cargo's `[patch]` if the relative is awkward under the build-dir layout). Shared path = shared cache entry under shared target dir.
- **Parallel test execution.** `cargo test` parallelizes by default to CPU core count. The shared target dir is safe under concurrent cargo invocations — cargo serializes compilation but runs multiple separate tests' binaries in parallel afterward. 8–16 cores typically give 3–5× speedup over sequential.
- **Avoid crates.io deps in integration-test projects.** Unlike standalone tests (which test real crates.io interop), integration tests should depend only on `test_helpers` and stdlib. No `uuid` / `reqwest` / etc. in integration test tomls — those are what make standalone tests minutes per test. Keep integration tests pure toylang + in-workspace helpers.

With these in place: realistic full-suite runtime 5–15 min, ~5 min warm cache after the first run. Validate this during 5c.1 — if the canary tests run cold at 5+ seconds each, investigate orchestration before committing to 129 migrations.

**Retirement fallout:**

- Direct mode (`toylangc --toylang-input <path>`) has no remaining callers after migration. Retire entirely (delete from `toylangc/src/main.rs`).
- `FileLoader` has no callers after direct mode retires (wrapper mode doesn't use it under two-crate). Retire entirely (delete `rustc-lang-facade/src/file_loader.rs`).
- `generate_stubs` trait callback and FileLoader-specific helpers follow.
- The facade's callback trait may reshape — retain whatever's used by wrapper-mode stub source generation, delete FileLoader-specific pieces.

### 4.4 §6.10 probe: is `#[linkage = "external"]` still needed?

Stage 4's partitioner override sets `(External, Default)` linkage directly on `__lang_stubs` items. Under two-crate, the stub rlib's `src/lib.rs` can ALSO carry `#[linkage = "external"]` (under `#![feature(linkage)]` at crate root) — belt-and-suspenders. These are different mechanisms: the partitioner override is post-partition mutation; `#[linkage]` is a source-attribute fast-path that runs before internalization.

Probe during 5c: emit the stub rlib WITHOUT `#[linkage = "external"]` AND WITHOUT `#![feature(linkage)]`. Run the suite. If 211/211, the attribute/feature are redundant — drop them, retire one more nightly feature. If tests fail, keep them and document what pattern was load-bearing.

---

## 5. Implementation steps

Four sub-stages, each a clean commit point. Pause between any two if you need a review checkpoint.

### Sub-stage 5a — cross-crate oracle (~1–1.5 weeks)

1. **Add `resolve_rust_path` helper** in `toylangc/src/oracle.rs`, with the local + extern-crate walk shape (§4.1).
2. **Migrate the ~8 `find_*` functions** to route through the helper. Each `find_*` becomes a thin adapter; the underlying walk logic lives once in the helper.
3. **Run the full suite between each migration.** Expect 211/211 — the change is behavior-preserving under current single-crate FileLoader. If a migration regresses, the helper's local-walk behavior drifted from the function it replaced.
4. **Commit 5a.** No architectural change visible yet; the code is just refactored to be cross-crate-capable.

### Sub-stage 5b — wrapper-mode two-crate (~1 week)

1. **Extend `toylangc/src/build.rs`** to emit a two-member workspace (§4.2). Use POC #2's `build.rs` as a starting point but adapt for current trait signatures. ~100 LoC of build-file emission.
2. **Adjust `stub_gen.rs`** to write full `src/lib.rs` content for the stub rlib. Include `#![feature(linkage)]` at crate root UNLESS the §6.10 probe says it's unneeded (see §6.10).
3. **Update `toylangc/src/main.rs`** wrapper-mode handling to distinguish the stub-rlib compile from the user-bin compile. Use `CARGO_PRIMARY_PACKAGE` env var for detection.
4. **Add `is_downstream_of_stubs` predicate** in the facade (or equivalent). Gate `notify_concrete_entry_point_inner`'s internal-callee walk — the walk runs in the stub rlib compile; in the user bin compile the walk is a no-op (internal callees are already stashed from the rlib-compile side, consumed by the rlib's `generate_and_compile` output).
5. **Verify `upstream_monomorphizations_for` fires correctly** for generic wrappers. Phase 6 unwrap tests (`test_option_unwrap_basic` etc.) are the canary. If they fail with link errors naming `__toylang_option_unwrap`-ish symbols, the override isn't routing properly.
6. **Run the full suite.** Standalone tests (`toylangc/tests/standalone_tests.rs`) all pass via the new wrapper-mode path. Integration tests still pass via the old FileLoader path (unchanged in this sub-stage).
7. **Commit 5b.** Wrapper mode on two-crate; direct mode still uses FileLoader.

### Sub-stage 5c — migrate integration tests to standalone-style (F-wide, ~3 weeks total across 4 phases)

Split into 4 internal phases, each a clean commit point.

#### 5c.1 — `test_helpers` crate + cargo orchestration + canary migration (~5–7 days)

1. **Create `test_helpers` workspace member** at `toylangc/tests/test_helpers/` (or equivalent). Cargo.toml declares it as a library crate. `src/lib.rs` exposes the functions current inline fixtures provide — `println_int`, `read_int`, etc. Grep `toylangc/tests/integration_tests.rs` for `pub fn ` inside test fixtures to find the full set.
2. **Set up shared cargo target dir.** The test harness (`tests/integration_tests.rs`'s helper, parallel to `run_standalone_test`) exports `CARGO_TARGET_DIR=<shared path>` before invoking `toylangc build`. Pick a path that's deterministic, per-session, and outside the repo (e.g., `/tmp/erw-integration-target/` or env-var-pointed). Verify `test_helpers` compiles once across multiple test invocations by inspecting `<target>/debug/deps/libtest_helpers-*.rlib` existence between runs.
3. **Pick 3–5 canary tests** that currently use extern-fn fixtures. Migrate each to `toylangc/tests/integration_projects/<test_name>/{main.toylang, toylang.toml, expected_output.txt}`. The `toylang.toml` for each declares `[rust-dependencies] test_helpers = { path = "../../test_helpers" }` (adjust relative path to match the actual directory depth).
4. **Write the harness helper** `run_integration_test(name, expected)` in `tests/integration_tests.rs`, mirroring `run_standalone_test` in `tests/standalone_tests.rs`. Calls `toylangc build` on the project directory (with `CARGO_TARGET_DIR` env set), runs the produced binary, asserts stdout contains `expected`.
5. **Validate runtime.** Run the 3–5 canaries. Warm-cache per-test time should be ~3–8s. If individual tests take 30+ seconds each, the shared target dir isn't being picked up — check the env-var export path in the harness, and verify `test_helpers`'s rlib is genuinely reused (not recompiled) between test invocations via `ls -la <target>/debug/deps/libtest_helpers*` mtime checks.
6. **Commit 5c.1.** `test_helpers` exists, harness helper + shared target dir orchestration added, canary tests migrated, orchestration validated. Other 124+ tests still use the inline-fixture direct-mode path.

#### 5c.2 — migrate extern-fixture tests (~1 week)

1. **~57 tests currently use the body-less-fn-plus-Rust-fixture pattern.** Each migrates mechanically:
   - Extract toylang source → `main.toylang`.
   - Extract Rust fixture body → either moves into `test_helpers/src/lib.rs` (if the fn name is shared across tests) or becomes a test-specific tiny Rust helper crate (if the body is test-specific).
   - Extract expected stdout → `expected_output.txt`.
   - Replace inline test body with `run_integration_test("<test_name>", "<expected>");`.
2. **Run full suite after each batch.** Migrate in batches of 5–10 tests, not all 57 at once — easier to bisect if something regresses.
3. **Commit 5c.2.** 57 extern-fixture tests migrated. ~72 remaining tests still use direct mode + inline source strings.

#### 5c.3 — migrate remaining tests (~1 week)

1. **~72 remaining integration tests** — mostly parser and type-resolver error tests plus a few codegen tests that don't use fixtures.
2. **Add error-assertion harness variant** `run_integration_test_expects_error(name, error_pattern)` that captures `toylangc build` stderr and matches against `error_pattern` (substring or regex — pick substring unless a test genuinely needs regex).
3. **Migrate each test.** Most are mechanical. Watch for:
   - Tests exercising specific facade callbacks or internal state that don't surface through `toylangc build` output. **Flag and escalate per-test** — may need rewriting, deletion, or preservation as a unit test on a different harness.
   - Error-assertion granularity loss — today's `Err(TypeResolveError::FooBar { .. })` becomes `stderr.contains("foo bar")`. Accept the regression; production users see strings too.
4. **Run full suite after each batch.**
5. **Commit 5c.3.** All 129 integration tests migrated. Direct mode code in `toylangc/src/main.rs` + `FileLoader` still present as dead code (nobody calls them).

#### 5c.4 — retire direct mode, retire FileLoader, §6.10 probe, doc pass (~3–5 days)

1. **Delete direct-mode handling** from `toylangc/src/main.rs` (`--toylang-input` argv parsing and the direct-mode dispatch).
2. **Delete `rustc-lang-facade/src/file_loader.rs`** and its installation in `driver.rs`.
3. **Delete FileLoader-specific trait callbacks** — specifically `generate_stubs` if/when it's no longer called outside wrapper-mode stub-source generation. Reshape the trait to match what's actually used now.
4. **Run the §6.10 `#[linkage]` probe.** Try emitting the stub rlib without `#[linkage = "external"]` and without `#![feature(linkage)]`. If 211/211 still passes, drop both and retire one more nightly feature.
5. **Run the full suite** one more time. 211/211 expected. Expected suite runtime: 5–15 min with cargo orchestration in place (was ~30s pre-migration; TL-confirmed acceptable).
6. **Doc pass:**
   - `docs/architecture/rust-interop-guide.md` — front-matter status: stage-5 complete, two-crate architecture, FileLoader retired, direct mode retired. §3 Opaque Stubs: describe the two-crate shape rather than FileLoader injection. §2.5 partitioner override: belt-and-suspenders with `#[linkage]` if the probe said keep it; otherwise note that partitioner override is the sole mechanism.
   - `docs/architecture/risks.md` §3 B2: reflect the `#[linkage]` source-attribute fast-path supplement if it survived the probe.
   - `docs/architecture/known-tech-debt.md`: update any FileLoader/direct-mode refs.
   - `HANDOFF-TL.md` §1/§2: note stage-5 landing; both direct mode and FileLoader gone.
   - `docs/historical/phase-history.md`: add Stage 5 entry.
   - Move `handoff-two-crate-migration.md` (this doc) to `docs/historical/`.
7. **Commit 5c.4.** Stage 5 complete.

---

## 6. Critical subtleties

### 6.1 `resolve_rust_path` must be cross-phase-safe

The helper walks `tcx.module_children`, which is safe anywhere (unlike `def_path_str` — see `@DPSFDOZ`). Don't introduce `def_path_str` calls in the helper for "debug logging convenience." If you want debug output, use `tcx.def_path(def_id).data` or `tcx.item_name(def_id)`.

### 6.2 `is_downstream_of_stubs` gating

Under two-crate, `notify_concrete_entry_point_inner` fires in both compiles for the same consumer entry point. Its internal-callee walk (`walk_and_stash_internal_callees`) should ONLY run in the stub-rlib compile — that's where the consumer's codegen will happen for non-generic items (generics route back to the user bin via `upstream_monomorphizations_for`).

Follow the `after_rust_analysis` precedent: read `CARGO_PRIMARY_PACKAGE` / crate-name env var at the callback site; gate the walk. Don't gate via a new facade method — the detection is the consumer's concern.

**Canary:** if you get "function already defined" link errors with consumer internal fn names, the walk ran in both compiles and produced duplicate codegen. If you get "undefined symbol" link errors, the walk ran in the wrong compile or not at all.

### 6.3 `upstream_monomorphizations_for` fires for real under stage 5

Commit `51f0c5e` scaffolded the override ~40 LoC; under the current FileLoader-single-crate architecture it's inert (no upstream rlib exists for it to affect). Under stage 5 it becomes load-bearing — it's what makes generic wrappers work across the two-crate boundary.

Verify by running Phase 6 unwrap tests first thing after 5b lands. `test_option_unwrap_basic`, `test_result_unwrap_basic`, `test_vec_pop_unwrap` — these all exercise generic `#[inline(never)]` wrappers. If any of them fail with link errors, the override isn't routing correctly.

### 6.4 Outcome A partitioner-set-linkage is NOT retired

The stage-4 Outcome A mechanism (partitioner override mutating `MonoItemData.linkage` to force `(External, Default)` on `__lang_stubs` items) stays. Under two-crate, `#[linkage = "external"]` on the wrapper functions is an ADDITIONAL mechanism — belt-and-suspenders, not replacement.

The reason both are desirable: `#[linkage]` takes rustc's source-attribute fast-path at `partitioning.rs:755-756` BEFORE internalization runs. The partitioner override's post-mutation is the backstop for any `__lang_stubs` items that don't carry `#[linkage]`. Together they make the linkage correctness robust to either mechanism independently failing.

§6.10's probe specifically tests whether the `#[linkage]` attribute is redundant given the partitioner override. Even if §6.10 says drop the attribute, the partitioner override stays — that's the Outcome A mechanism that's shipping in production today.

### 6.5 `ActiveParamMap` thread-local preservation

Stage 3's `oracle::ActiveParamMap` scope-guarded thread-local provides Param-name → Param-index lookup during the dep-discovery walk's type resolution. When you migrate oracle functions to `resolve_rust_path`, don't accidentally break the scope guard semantics — the guard is installed at the `collect_generic_rust_deps` callback entry and dropped at exit. Reference `toylangc/src/oracle.rs`'s existing `ActiveParamMap` implementation for the invariant. Probably fine to leave alone if you're just touching the local-vs-extern resolution piece.

### 6.6 `@DPSFDOZ` hazard in stub rlib compile

The stub rlib compile runs entirely inside `generate_and_compile` (the consumer's backend produces its `.o`) so `def_path_str` is technically safe there. But the partitioner override fires OUTSIDE `generate_and_compile` in both compiles. Don't introduce new `def_path_str` calls in code reachable from the partitioner override. Use `is_from_lang_stubs_safe` (the `tcx.def_path(...).data` walk from stage 2) exclusively.

### 6.7 `@GCMLZ` hazard in the two-compile model

Under two-crate, the facade's `MUTABLE_STATE` is a per-process mutex. Both compiles share the same process (when running tests in-process, or when the toylangc wrapper handles both via cargo). If some new code path introduces a lock acquire outside `generate_and_compile`, deadlock returns. Audit any new synchronization you add. Read `@GCMLZ` arcana.

### 6.8 Test-runtime orchestration is load-bearing

Naive F-wide migration hits 30–60 min full-suite runtime because each of 129 tests runs its own isolated cargo build. The target is 5–15 min, achieved via the cargo orchestration in §4.3:

- Shared `CARGO_TARGET_DIR` (env var exported by the harness).
- `test_helpers` as a workspace-path dep shared across all test tomls.
- No crates.io deps inside integration-test projects (keep them pure — `test_helpers` + stdlib only).
- Rely on cargo's default parallel test execution.

**Validate orchestration empirically at 5c.1 canary stage.** If individual canary tests take 30+ seconds cold, something's off — `CARGO_TARGET_DIR` isn't being honored, or `test_helpers` is re-resolving per test. Fix before scaling to 129 migrations. An orchestration bug multiplied across 129 tests is where "30–60 min suite" materializes.

### 6.9 Don't destabilize stage 4 for stage 5

If you find yourself wanting to change the partitioner override, or the `CodegenBackend` wrapper, or the query-override installation — stop. Stage 5 should be additive/substitutive for FileLoader and the oracle; it should NOT be changing the stage-4 plumbing. If you think it needs changing, escalate to the TL.

### 6.10 Probe: is `#[linkage = "external"]` even needed under plugin-set-linkage?

The stage-4 partitioner override forces `(External, Default)` linkage directly on `__lang_stubs` items. If you write the stub rlib's `src/lib.rs` WITHOUT `#[linkage = "external"]` and WITHOUT `#![feature(linkage)]`, the partitioner override still sees those items (they're in `__lang_stubs`) and still forces External linkage. The source attribute becomes redundant.

**Probe during 5b or 5d:** emit the stub rlib without either. Run the full suite. If 211/211, the attribute/feature are redundant — drop them. Result: one fewer nightly feature in the stack, plain-Rust stub crate.

**If the probe fails,** something about `#[linkage]` is load-bearing in a way the partitioner override doesn't cover. Keep the attributes; leave a brief note in the commit explaining what pattern it's load-bearing for.

---

## 7. Verification

### Build & test (per CLAUDE.md redirect convention)

```bash
cargo +nightly-2025-01-15 test -p toylangc 2>&1 > /tmp/stage5.txt
grep "test result:" /tmp/stage5.txt
```

After each sub-stage: 211/211 passing, 0 failed, 0 ignored.

### Runtime check after 5c

Post-5c, the integration-test suite runs 5–15 min with cargo orchestration in place (shared `CARGO_TARGET_DIR`, `test_helpers` as a shared workspace dep, parallel test execution). Warm-cache subsequent runs closer to 5 min. If sustained runtime is 30+ min, orchestration isn't working — `test_helpers` is recompiling per test, or `CARGO_TARGET_DIR` isn't being honored. Debug via `ls -la <shared_target>/debug/deps/libtest_helpers*` between runs (rlib mtime should only change on first compile after source changes, not per test invocation).

### Required greps after 5d

```bash
rg "FileLoader" rustc-lang-facade/ toylangc/          # zero hits expected
rg "file_loader" rustc-lang-facade/                    # zero hits expected
rg "mod __lang_stubs;" toylangc/tests/ toylangc/src/   # should only appear in facade-generated shim content, not in test fixtures
```

### Sanity: observable behavior preserved

The whole migration should be invisible at the user level. Pick any Phase 7 standalone test (e.g., `uuid_test`) and run it before and after. Binary output should be identical ("uuid ok" to stdout). If binary behavior differs, something beyond architectural reshape changed.

---

## 8. Out of scope / follow-ups

**Not in this task** (flag but don't do):

- Removing the partitioner override (stage-4 Outcome A). Belt-and-suspenders mechanism stays.
- Switching to `-Zcodegen-backend=valec`-style codegen backend plugin loading. The current `rustc_driver::Callbacks`-installed wrapper is fine.
- `rustc_public` migration for toylangc's consumer-side code. Orthogonal, tracked separately.
- Additional Cases 1a/1b/3 infrastructure. Stage 5 preserves the capability; actual exercise is future work (possibly Vale).
- Fork-branch cleanup. Erw is zero-fork; `~/rust` working tree and `rustc-fork` toolchain link are preserved for reference but unused.

**Follow-up tickets to file** after landing:

- "Consider retiring `codegen_wrapper.rs` in favor of a more opinionated plugin structure" — stage-5 might surface opportunities here but the refactor is separate.
- "Evaluate Vale's greenfield adoption path" — once stage 5 lands, the erw architecture is what Vale would consume. Consider whether any consumer-agnostic generalizations are worth doing before they fork (e.g., less `__lang_stubs`-name-specific hardcoding).

---

## 9. Rollback and if-you-get-stuck

### Rollback

Each sub-stage commits independently. Rollback is proportional to how far you got:

- **5a fails:** revert the oracle changes. Everything else unchanged. FileLoader-single-crate architecture continues working as-is.
- **5b fails:** revert the `build.rs` + `stub_gen.rs` + wrapper-mode handling changes. Keep 5a's oracle changes (they're behavior-preserving under single-crate). Wrapper mode still uses FileLoader; direct mode unchanged.
- **5c fails:** revert the direct-mode test harness migration. Keep 5a + 5b. Wrapper mode on two-crate; direct mode still on FileLoader. Legitimate partial-landing state.
- **5d fails:** this is purely housekeeping; unlikely to fail mechanically. If docs feel wrong, commit everything else and revisit docs separately.

At every stage the tree is green before the next commit. Don't skip the full test run between steps.

### Come find the TL if:

- **The cross-crate oracle rewrite hits an unexpected rustc internal.** `tcx.module_children` on an extern crate might have quirks; if a DefId comes back that's different from what `module_children_local` would have produced for the same-shape item, something's subtle. Don't spin; escalate.
- **Phase 6 unwrap tests fail after 5b.** Means `upstream_monomorphizations_for` isn't routing generic wrappers correctly. This is the canary. Grep for the override's body in `queries/upstream_monomorphizations.rs` and trace.
- **Integration-test suite runtime explodes past 30 min.** Cargo orchestration isn't sharing state correctly. Check `CARGO_TARGET_DIR` export in the harness, and verify `test_helpers`'s rlib is cached (not rebuilt per test) via `ls -la <target>/debug/deps/libtest_helpers*` mtime inspection.
- **`#[linkage]` probe (§6.10) produces surprising results** — e.g., tests fail with `#[linkage]` removed but pass without it AND without `#![feature(linkage)]`. That's internally inconsistent; probably means some interaction I didn't anticipate. Escalate.
- **Any test fails unexpectedly after a behavior-preserving change** (5a specifically). The migration is supposed to be invisible under single-crate; any failure means something subtle drifted. Investigate, don't paper over.

Don't be shy. Stage 5 is scoped tighter than stage 4 but has its own architectural subtleties — the prior stage-4 junior escalated the cross-crate oracle specifically, and that was the right call.

### A note on the shape of this handoff

Stage 5 is the "make erw architecturally Vale-fork-ready" work. The functional gain (Outcome A → source-attribute fast-path for linkage) is real but moderate; the strategic gain (architecture-complete for Rust-as-top-level cases + clean fork-ready shape) is larger. Treat the work as "improving the architectural foundation" rather than "fixing a problem" — erw works today and will continue to; stage 5 is about what it looks like for someone else to build on.

The sub-stages are intentionally loosely-coupled — each improves the codebase on its own. Clean stopping points if you need to pause:

- **After 5a:** oracle is cross-crate-capable but nothing exercises it. Refactor-only landing.
- **After 5b:** wrapper mode on two-crate; standalone tests exercise the full path. Mixed-architecture state (integration tests still on FileLoader) but functional.
- **After 5c:** both modes on two-crate; FileLoader is dead code. Legitimate stopping point if 5d feels tedious.
- **After 5d:** stage 5 complete.

The goal is two-crate + FileLoader retired. The path there is four sub-stages. Don't race; land each checkpoint solidly before starting the next.
