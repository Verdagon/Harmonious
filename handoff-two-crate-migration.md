# Handoff: Two-crate architecture migration (stage 5)

**Task owner:** a junior engineer joining the erw project. **Previous exposure to stages 1–4 not assumed.** The first few sections of this doc give you the context you need; beyond that we go into specifics.
**Branch:** work on `main` directly.
**Estimated effort:** 3–4 weeks for a junior, including ramp time.
**Risk level:** medium. The scope is well-bounded (three clearly-separated pieces); the tricky architectural piece — cross-crate type resolution in `oracle.rs` — has been scoped, sketched, and tested in prior exploratory work. No toolchain rebuild required; erw is already zero-fork.

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

### 4.3 Direct-mode in-process two-crate (sub-stage 5c)

Each integration test allocates a tempdir, writes stub rlib source + user bin source, and invokes `rustc_driver::RunCompiler` TWICE per test:

```rust
fn compile_toylang_direct_two_crate(toylang_source: &str) -> CompileResult {
    let tmp = tempfile::tempdir()?;

    // Generate stub rlib + user bin sources via facade's helpers.
    let (stub_content, user_main_content) =
        facade::generate_two_crate_sources(toylang_source);

    std::fs::create_dir(tmp.path().join("lang_stubs"))?;
    std::fs::write(tmp.path().join("lang_stubs/lib.rs"), stub_content)?;
    std::fs::write(tmp.path().join("main.rs"), user_main_content)?;

    // Phase 1: compile stub rlib via rustc_driver.
    let stub_rlib_path = tmp.path().join("liblang_stubs.rlib");
    let rlib_args = compose_rustc_args_for_rlib(tmp.path(), &stub_rlib_path);
    rustc_driver::RunCompiler::new(&rlib_args, &mut facade_callbacks()).run();

    // Phase 2: compile user bin with --extern lang_stubs=<rlib>.
    let bin_path = tmp.path().join("test_bin");
    let bin_args = compose_rustc_args_for_bin(tmp.path(), &stub_rlib_path, &bin_path);
    rustc_driver::RunCompiler::new(&bin_args, &mut facade_callbacks()).run();

    // Execute and capture output.
    run_and_capture(&bin_path)
}
```

No cargo spawn — just two in-process `rustc_driver` invocations. Expect ~2× the per-test compilation cost vs today, so the integration-test suite goes from ~30s to ~2–3 min. Acceptable per TL direction.

**Add `tempfile` as a dev-dependency** in `toylangc/Cargo.toml`. Integration tests live in `toylangc/tests/`, so dev-dep scope suffices.

**Facade helper:** add `generate_two_crate_sources(toylang_source) -> (stub_content, user_main_content)` in the facade, wrapping the existing `stub_gen` + wrapper-source-generation logic. Both direct mode and wrapper mode use it.

### 4.4 Retire FileLoader + §6.10 probe + doc pass (sub-stage 5d)

- Delete `rustc-lang-facade/src/file_loader.rs`.
- Remove the `FileLoader` installation from `rustc-lang-facade/src/driver.rs`.
- Delete any `generate_stubs` / wrapper-source helpers that were FileLoader-specific. Retain whatever's still used by `generate_two_crate_sources`.
- Run the §6.10 `#[linkage]` probe (if not already done during 5b).
- Doc pass.

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

### Sub-stage 5c — direct-mode in-process two-crate (~1–1.5 weeks)

1. **Add `tempfile` as a dev-dependency** in `toylangc/Cargo.toml`.
2. **Write a shared test helper** `toylangc/tests/common/mod.rs` (create if it doesn't exist) implementing `compile_toylang_direct_two_crate` per §4.3. Handles tempdir, source writing, two rustc_driver invocations, cleanup-on-drop.
3. **Add facade helper** `generate_two_crate_sources(toylang_source) -> (stub_content, user_main_content)`. Wraps existing `stub_gen` + wrapper-source logic. Keep the old FileLoader-driven helpers alive for now — retire in 5d.
4. **Migrate integration tests in one pass.** Most are a mechanical find-replace: `compile_toylang_direct(source)` → `compile_toylang_direct_two_crate(source)`. A handful of tests may have FileLoader-specific assertions; adapt to two-crate reality.
5. **Runtime expectations.** Post-migration suite takes ~2–3 min (vs today's ~30s). Acceptable per TL direction. If you see 10+ min, something's wrong — probably cargo is spawning somewhere it shouldn't, or tempdir cleanup is non-linear. Check the rustc-driver arg composition.
6. **Grep for remaining `FileLoader` references in code** (outside the file being retired in 5d). Document any remaining call sites in the commit message.
7. **Commit 5c.** Direct mode on two-crate; FileLoader still present but unused by any caller.

### Sub-stage 5d — retire FileLoader, §6.10 probe, docs (~3–5 days)

1. **Run the §6.10 `#[linkage]` probe if not already done.** Try emitting the stub rlib's `src/lib.rs` WITHOUT `#[linkage = "external"]` and WITHOUT `#![feature(linkage)]`. Run the full suite. If 211/211, the attribute/feature are redundant under plugin-set-linkage — drop them. If tests fail with link errors, keep them.
2. **Delete `rustc-lang-facade/src/file_loader.rs`.**
3. **Remove the FileLoader installation in `rustc-lang-facade/src/driver.rs`**.
4. **Delete FileLoader-specific helpers** that are no longer called by `generate_two_crate_sources`. Keep helpers still in use.
5. **Run the full suite** one more time. 211/211 expected.
6. **Doc pass:**
   - `docs/architecture/rust-interop-guide.md` — front-matter status line: mention stage-5 (two-crate architecture). §2.5 partitioner-override description may need a brief mention that stub-rlib items also carry `#[linkage]` belt-and-suspenders (or, per §6.10, don't). §3 Opaque Stubs discussion of `FileLoader` becomes historical — describe the two-crate shape instead.
   - `docs/architecture/risks.md` §3 B2 — update to reflect that Outcome A is now supplemented by the `#[linkage]` source-attribute fast-path (belt-and-suspenders). If §6.10 probe succeeded, note that the attribute was redundant but the source-attribute path was NOT; these are different mechanisms.
   - `docs/architecture/known-tech-debt.md` — if it mentions FileLoader, update.
   - `HANDOFF-TL.md` — §1 summary: note stage-5 landing. §2: FileLoader is gone.
   - `docs/historical/phase-history.md` — add a Stage 5 entry.
   - Move `handoff-two-crate-migration.md` (this doc) to `docs/historical/`.
7. **Commit 5d.** Stage 5 complete.

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

### 6.8 The 129 direct-mode tests are where 5c's risk lives

Most will migrate mechanically. A handful have FileLoader-specific assertions or specific path expectations. Expect to spend ~2–3 days on the mechanical migration and ~1–2 days on edge cases. Don't move to 5d until all 129 pass.

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

Post-5c, the integration-test suite runs ~2–3 min (vs today's ~30s). If 10+ min, something's spawning cargo or doing non-linear work. Check the `rustc_driver` arg composition and tempdir-cleanup path.

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
- **Direct-mode test runtime explodes past 5 min.** Something is spawning cargo or doing redundant work. Check rustc-driver arg composition.
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
