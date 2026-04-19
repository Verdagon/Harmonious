# Handoff: CodegenBackend plugin — retire the rustc fork

**Task owner:** a junior engineer joining the erw project. **Previous exposure to stages 1/2/3 not assumed.** The first 3 sections of this doc give you the context you need; beyond that we go into specifics.
**Branch:** work on `main` directly.
**Estimated effort:** 3–5 weeks end-to-end, split across four staged sub-deliverables (§6). Plan for at least two rustc-toolchain rebuilds (8–12 min each) and several dead-end dives into rustc internals.
**Risk level:** high. This is the largest refactor in erw's history. Mitigated by (a) four staged sub-commits, each a standalone improvement; (b) two prior investigations (`poc/separate-crate-stubs` and `spike/modulellvm-wall`) have already verified the mechanism works; (c) rollback at any sub-stage is a revert + toolchain rebuild.

---

## 1. Project crash course (read even if you think you don't need it)

erw is a two-crate workspace:

- **`rustc-lang-facade`** — a reusable library for embedding custom languages into rustc's compilation pipeline via query providers.
- **`toylangc`** — an example consumer compiler for "toylang" (a small language) that uses the facade to demonstrate the architecture end-to-end.

The framework's core trick: toylang code calls into real crates.io Rust crates (`uuid`, `serde_json`, `clap`, etc.), but rustc doesn't know about toylang's type system or codegen. The facade bridges the gap via rustc query overrides — rustc believes toylang types exist but treats them opaquely; rustc compiles the Rust parts and toylang's own LLVM backend compiles the toylang parts; the two `.o` files link together into a working binary.

**Three architectural concepts you must internalize before starting:**

1. **Interleaved monomorphization.** Rustc's mono collector is the only entity that walks both Rust source and (via the facade) consumer source. When rustc code calls a toylang function with concrete Rust type args, we need to tell rustc what the toylang function depends on so rustc can queue those deps for codegen. The facade does this via the `optimized_mir` override (stage 3) — returns a synthetic MIR body full of `ReifyFnPointer` casts that the collector walks. Full taxonomy in `docs/reasoning/why-interleaved-monomorphization.md`; read at least the summary table before touching stage 4.

2. **Opaque stubs.** Consumer types appear to rustc as 0-field opaque structs with `unreachable!()` bodies, injected into the user's crate via a `FileLoader` (today — that's what stage 4 retires in favor of a separate rlib). Rustc parses and type-checks them as normal Rust; at codegen time our overrides kick in.

3. **The two-sided codegen.** Rust items → rustc's LLVM backend. Toylang items → toylang's Inkwell LLVM backend, producing a separate `.o` that gets injected at link time via today's `CodegenBackend` wrapper (`rustc-lang-facade/src/codegen_wrapper.rs`). Today rustc still runs its default partitioner and tries to codegen stubs; fork patches 3 and 5 prevent that. Stage 4's plugin takes over the partitioner and the codegen dispatch, retiring both patches.

### The fork-reduction roadmap (context for your stage)

erw started with a 5-patch rustc fork. Three stages have landed:

- **Stage 1** (commit `ed2e692`): split the `monomorphize_fn` callback into two single-responsibility hooks (`collect_generic_rust_deps` + `notify_concrete_entry_point`). Removed an undocumented ordering dependency and set up stage 3's DefId-keyed override. **This changed the trait's shape on the consumer side; every reference to `monomorphize_fn` in old POC findings is stale. Trust current `main` over old writeups for the trait shape.**

- **Stage 2** (commit `b345162`): removed a single-crate-compile assumption from the backend (`llvm_gen.rs`'s `as_local()` filter) plus consolidated a cross-crate-safe `__lang_stubs` predicate into `is_from_lang_stubs_safe` in the facade. **This predicate is load-bearing for stage 4 — you'll use it in the partitioner override to split consumer items from Rust CGUs.**

- **Stage 3** (commits `ce437ae` + `bf770ae` + `da7ad87`): migrated from the custom `per_instance_mir` query to an `override_queries` hook on rustc's existing `optimized_mir` query. Reshaped fork patch 3 into a facade-installed `CODEGEN_SKIP_HOOK` (symmetric with the existing `VISIBILITY_OVERRIDE_HOOK`). **Took the fork from 5 patches to 2.** You're removing the last 2 plus `FileLoader`.

**Stage 4 target (this task):** zero fork patches. `FileLoader` eliminated. Consumer stubs live in their own rlib. Toylang builds against vanilla nightly.

---

## 2. Required reading before you code (4–6 hours)

Substantial. Don't skip — stage 4 touches parts of rustc and the facade that a quick skim won't orient you on.

### Tier 1 — project orientation (90 min)

1. `/Users/verdagon/erw/CLAUDE.md` — project-wide instructions. Compiler laws, build conventions.
2. `/Users/verdagon/erw/HANDOFF-TL.md` — project state summary (has been refreshed with current status).
3. `/Users/verdagon/erw/docs/architecture/rust-interop-guide.md` — Parts 1–4 for the pipeline, §2.2 (`optimized_mir` override) and §10.6.4 (`VISIBILITY_OVERRIDE_HOOK`) in full. Skip Part 10's phase history on first read.

### Tier 2 — stage-4 design (2 hours)

4. `/Users/verdagon/erw/docs/reasoning/rustc-fork-design-space.md` **§4.2 in full** — the primary spec for stage 4's plugin architecture. The "Medium path" sidebar is what we're implementing; the "Easy path" upstream-PR sidebar is NOT on the critical path. §5 has the post-stage-3 cost accounting.
5. `/Users/verdagon/erw-spike-modulellvm-wall/findings.md` — the spike that verified the Medium path. Read the Executive Summary in full; skim Exp 4.1 (partitioner override mechanism) and the "Medium-path workaround details" section. The `OngoingCodegen` downcast trick (Exp 2e) is a useful backup option if you get stuck but not the primary path.
6. `/Users/verdagon/erw-poc-separate-crate-stubs/findings.md` — the POC that verified separate-crate stubs work. Read the Executive Summary; Step 3 (`nm` inventory confirming the rlib compiles clean); §4.3.D (plugin mode absorbs risks #8 and #9 — this is the "why FileLoader retirement is easy under plugin mode" argument). **Risks #1/#8/#9 from this doc ARE the subtleties you need to handle.**
7. `/Users/verdagon/erw/rustc-lang-facade/src/codegen_wrapper.rs` — read in full (~135 lines). This is what you'll extend.

### Tier 3 — context for debugging (read as needed)

8. `/Users/verdagon/erw/docs/reasoning/why-interleaved-monomorphization.md` — at minimum the summary table. Full read if you hit a "why does rustc's collector matter here?" question.
9. `/Users/verdagon/erw/docs/reasoning/dep-discovery-approaches.md` — Approach A vs B. Stage 3 background; referenced when debugging Param-flow issues.
10. `/Users/verdagon/erw/docs/usage/rebuilding-rustc-fork.md` — the 5-step toolchain rebuild workflow. You'll run this at least twice.
11. `/Users/verdagon/erw/docs/historical/handoff-optimized-mir-migration.md` — stage-3 junior handoff. Useful if you want to see how the prior refactor was scoped and what subtleties surfaced.
12. `/Users/verdagon/erw/docs/arcana/` — cross-cutting concerns. Reference when you hit `@ID` comments in the code.

### Tier 4 — upstream rustc references

13. `~/rust/compiler/rustc_codegen_cranelift/src/lib.rs` — in-tree example of a standalone `CodegenBackend`. Not quite our shape (cranelift fully replaces LLVM; we want to coexist) but useful for the trait contract.
14. `~/rust/compiler/rustc_codegen_ssa/src/traits/backend.rs` — `CodegenBackend` trait definition. Memorize which methods you delegate vs override.

---

## 3. Current surface (verified file:line references)

### Rustc fork (`~/rust` branch `per-instance-mir`)

After stage 3 the fork has two patches:

- **`CODEGEN_SKIP_HOOK`** (patch 3 post-reshape) — in `~/rust/compiler/rustc_codegen_ssa/src/mono_item.rs`. A `pub static CODEGEN_SKIP_HOOK: OnceLock<for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> bool>` the facade fills at startup. Inside `MonoItemExt::define` (or similar dispatch code), a check `if CODEGEN_SKIP_HOOK.get().is_some_and(|f| f(tcx, instance)) { return; }` skips codegen for consumer items. **You will remove this in sub-stage 4b.**
- **`VISIBILITY_OVERRIDE_HOOK`** (patch 5) — in `~/rust/compiler/rustc_monomorphize/src/partitioning.rs`. A `pub static VISIBILITY_OVERRIDE_HOOK: OnceLock<for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> Option<(Linkage, Visibility)>>` the facade fills at startup. Inside `mono_item_linkage_and_visibility`, a check consulting the hook before rustc's default logic. **You will remove this in sub-stage 4c.**

Both hooks are consumer-agnostic — the fork knows nothing about `__lang_stubs`. Removing them means deleting the `pub static`, the `use` imports, and the call sites inside the respective functions. `pub mod` flips in the enclosing `lib.rs` may no longer be needed after removal — audit per the `-D warnings unreachable_pub` rule per `docs/usage/rebuilding-rustc-fork.md`.

### Facade (`rustc-lang-facade/src/`)

- **`lib.rs`**: `CODEGEN_SKIP_HOOK.set(...)` installation in `install_callbacks`. Also `VISIBILITY_OVERRIDE_HOOK.set(...)`. Both go away as you retire the corresponding fork patches.
- **`lib.rs` — `is_from_lang_stubs_safe`**: the cross-crate-safe predicate from stage 2. You'll use this in the partitioner override to filter consumer items.
- **`lib.rs` — `is_consumer_codegen_target`**: the predicate currently installed in `CODEGEN_SKIP_HOOK` (`is_from_lang_stubs_safe` AND (consumer_fn OR consumer_accessor)). Read it; the plugin's partitioner-side filter will use the same predicate shape.
- **`codegen_wrapper.rs`** (~135 lines): the current `CodegenBackend` impl `LangCodegenBackend`. Wraps `LlvmCodegenBackend`, delegates most methods, intercepts `join_codegen` to inject the consumer's `.o`. **You'll extend this substantially in sub-stage 4a.**
- **`driver.rs`**: where `LangCodegenBackend` gets installed into rustc's driver. For `-Zcodegen-backend=...` wiring in 4c, you may need to add a `#[no_mangle] pub extern "C" fn __rustc_codegen_backend() -> Box<dyn CodegenBackend>` entry point per rustc's plugin loading protocol. Reference `rustc_codegen_cranelift/src/lib.rs` for the exact shape.
- **`file_loader.rs`**: the current stub-injection mechanism. Reads `__lang_stubs.rs` content from the consumer callback, splices it into the user's compilation. **You'll retire this in sub-stage 4c** in favor of a separate stub rlib.
- **`queries/mod.rs`**: where `Config::override_queries` is wired. **You'll add the `collect_and_partition_mono_items` override here in sub-stage 4a.**
- **`queries/optimized_mir.rs`**: the stage-3 dep-discovery override. Unchanged by stage 4.
- **`queries/symbol_name.rs`**: the stage-3 entry-point symbol override. Unchanged by stage 4.
- **`queries/layout.rs` / `queries/drop_glue.rs`**: unchanged.

### Consumer (`toylangc/src/`)

- **`build.rs`**: the `toylangc build` command. Today writes a single-crate Cargo project with user code + `FileLoader`-injected stubs. **You'll extend this in sub-stage 4c** to write a two-member Cargo workspace (stub rlib + user bin). Reference the POC #2 branch's implementation as a starting point.
- **`stub_gen.rs`**: generates `__lang_stubs.rs` content. Today the content gets FileLoader-injected. Tomorrow it writes to the stub rlib's `src/lib.rs`. Content may need small changes for cross-crate visibility (pub re-exports, crate-root `#![feature(linkage)]`).
- **`llvm_gen.rs`**: toylang's Inkwell codegen. Largely unchanged by stage 4; the plugin's partitioner-override takes consumer items off rustc's plate before rustc sees them, but the consumer's codegen output shape is the same.

### The POC #2 worktree has a working separate-crate scaffold

`/Users/verdagon/erw-poc-separate-crate-stubs/` contains a scaffolding-grade implementation of the separate-crate model. **Don't copy-paste it** (it predates stage 3; its trait signatures are stale). But the cargo orchestration logic (workspace layout, path deps, `RUSTC_WORKSPACE_WRAPPER` routing) in its `build.rs` is the reference for your own implementation. Read it before you start sub-stage 4c.

---

## 4. Proposed design

The end state is a four-piece change:

1. Plugin takes over CGU partitioning via `collect_and_partition_mono_items` override. Rust CGUs flow to `LlvmCodegenBackend::codegen_crate` unchanged; consumer items get pulled into the plugin's own codegen path.
2. Plugin takes over codegen dispatch for consumer items. Retires `CODEGEN_SKIP_HOOK` because rustc never sees consumer items at the codegen stage.
3. Plugin sets linkage directly (it IS the partitioner now). Retires `VISIBILITY_OVERRIDE_HOOK`.
4. Stubs move from `FileLoader`-injected module to a separate rlib **in both wrapper mode and direct mode** (wrapper mode via a two-member Cargo workspace; direct mode via in-process two-crate compilation through `rustc_driver` with tempfile-backed sources). Requires adding `upstream_monomorphization` override (~5 LoC) so the user bin's collector routes generic wrapper monos to local codegen. Retires `file_loader.rs` entirely — no mode keeps it alive as test-infrastructure convenience.

### 4.1 Partitioner override mechanism

`rustc_middle::query::mod::collect_and_partition_mono_items` returns `(&'tcx DefIdSet, &'tcx [CodegenUnit<'tcx>])` — the reachable set and the CGU list. Install via `Config::override_queries` same as `optimized_mir`, `symbol_name`, etc.

Pattern (mirrors the stage-3 `DEFAULT_OPTIMIZED_MIR` OnceLock + `default_optimized_mir` accessor):

```rust
// lib.rs
pub static DEFAULT_PARTITIONER: OnceLock<...fn ptr...> = OnceLock::new();

// queries/partition.rs (new file)
pub fn lang_collect_and_partition(
    tcx: TyCtxt<'_>,
    _key: (),
) -> (&'_ DefIdSet, &'_ [CodegenUnit<'_>]) {
    // Delegate to upstream for the walk + default partition.
    let upstream = crate::DEFAULT_PARTITIONER.get().expect(...);
    let (reachable, cgus) = upstream(tcx, ());

    // Filter consumer items out of the CGU list. They go to our own
    // codegen path; rustc only codegens what's left.
    let (rust_cgus, consumer_cgus): (Vec<_>, Vec<_>) = cgus.iter()
        .map(|cgu| split_cgu_by_consumer_predicate(tcx, cgu))
        .unzip();

    // Stash consumer_cgus for our plugin's codegen_crate to consume.
    stash_consumer_cgus(consumer_cgus);

    // Return rust_cgus to rustc. Reachable set unchanged — downstream
    // queries like upstream_monomorphization still need it intact.
    (reachable, tcx.arena.alloc_slice(&rust_cgus))
}
```

The `split_cgu_by_consumer_predicate` function walks each CGU's items, partitioning by `is_consumer_codegen_target` (the existing predicate from stage 3). Consumer items go into a parallel `consumer_cgus` list; rust items stay in the returned `rust_cgus`.

**Why return `reachable` unchanged**: downstream queries (`upstream_monomorphization`, `explicit_linkage`, etc.) inspect the reachable set to make their own decisions. If we remove consumer items from reachable, those queries behave differently for dep-discovery downstream callers. The spike's findings.md §4.1 explicitly walks through this — re-read if unclear.

### 4.2 Plugin takes over consumer codegen

In `codegen_wrapper.rs::codegen_crate`, after `inner.codegen_crate` returns, iterate the stashed `consumer_cgus` and codegen each via toylang's LLVM backend. The current flow already has a `call_generate_and_compile(tcx)` hook that does most of this work; extend it to consume the stashed consumer CGUs instead of discovering items independently via the MonoItems walk in `llvm_gen.rs::generate_with_tcx`.

Once this lands, consumer items are never in rustc's CGUs → rustc never codegens `unreachable!()` bodies for them → `CODEGEN_SKIP_HOOK` has nothing to skip → retire the hook.

### 4.3 Partitioner sets linkage directly

With the plugin doing its own partitioning, it controls the `(Linkage, Visibility)` of every item it places. Consumer items get `(Linkage::External, Visibility::Default)` directly, same as `VISIBILITY_OVERRIDE_HOOK` currently forces. No separate hook needed — retire `VISIBILITY_OVERRIDE_HOOK`.

### 4.4 Two-crate architecture (both compile modes)

Retire `FileLoader` entirely. Both of toylang's compile modes (wrapper mode and direct mode) migrate to a two-crate structure: a stub rlib (facade-generated source) + a user bin that depends on it. This keeps nightly features (`#![feature(linkage)]` if we keep it — see §6.9) confined to the stub rlib's crate root, avoiding any feature-scope propagation into user code. The two crates compile as independent units; feature flags don't cross the boundary.

#### 4.4a Wrapper mode: two-member Cargo workspace

`toylangc build` generates:

```
.toylang-build/
├── Cargo.toml           # workspace
├── lang_stubs_crate/
│   ├── Cargo.toml       # rlib
│   ├── src/lib.rs       # generated by stub_gen
│   └── rust-toolchain.toml
└── user_bin/
    ├── Cargo.toml       # path dep on lang_stubs_crate
    ├── src/main.rs      # user's main + `use __lang_stubs::*`
    └── build.rs
```

The stub rlib and user bin both compile via `RUSTC_WORKSPACE_WRAPPER` pointing at toylangc, so the plugin's overrides fire for both crates. POC #2's `build.rs` on branch `poc/separate-crate-stubs` has the cargo-orchestration patterns (workspace layout, path dep, `CARGO_PRIMARY_PACKAGE` sniffing to distinguish stub-rlib-compile from user-bin-compile).

#### 4.4b Direct mode: in-process two-crate via `rustc_driver`

Integration tests today live in `toylangc/tests/integration_tests.rs` — each provides a toylang source string and the facade compiles it in-process via `rustc_driver::RunCompiler` with `FileLoader` injecting synthetic `__lang_stubs.rs` content. That's the path we're retiring.

New shape: each test allocates a tempdir (via the `tempfile` crate), writes the stub rlib's `src/lib.rs` + a `Cargo.toml` for context, writes the user bin's `src/main.rs` + its `Cargo.toml`, and invokes `rustc_driver::RunCompiler` **twice per test** — once for the stub rlib (produces a `.rlib` in the tempdir), once for the user bin (with `--extern lang_stubs=<rlib path>` pointing at the rlib). No cargo spawn; direct `rustc_driver` invocations, same in-process model as today.

Implementation sketch (in a test helper in `toylangc/tests/common/mod.rs` or similar):

```rust
fn compile_toylang_direct_two_crate(toylang_source: &str) -> CompileResult {
    let tmp = tempfile::tempdir()?;

    // Generate stub content + user wrapper source via facade's existing helpers.
    let (stub_content, user_main_content) = facade::generate_two_crate_sources(toylang_source);

    // Write stub rlib source tree.
    std::fs::create_dir(tmp.path().join("lang_stubs"))?;
    std::fs::write(tmp.path().join("lang_stubs/lib.rs"), stub_content)?;

    // Write user bin source.
    std::fs::write(tmp.path().join("main.rs"), user_main_content)?;

    // Phase 1: compile stub rlib with rustc_driver.
    let stub_rlib_path = tmp.path().join("liblang_stubs.rlib");
    rustc_driver::RunCompiler::new(
        &compose_rustc_args_for_rlib(tmp.path(), &stub_rlib_path),
        &mut facade_callbacks(),
    ).run();

    // Phase 2: compile user bin with --extern lang_stubs=<rlib path>.
    let bin_path = tmp.path().join("test_bin");
    rustc_driver::RunCompiler::new(
        &compose_rustc_args_for_bin(tmp.path(), &stub_rlib_path, &bin_path),
        &mut facade_callbacks(),
    ).run();

    // Execute and capture output.
    run_and_capture(&bin_path)
}
```

Budget: roughly 2× the per-test compilation cost vs today, since each test goes through two rustc invocations instead of one. Current suite ≈ 30s; expect ~2–3 min post-migration. Acceptable per TL direction.

**Why not a cargo-spawn-per-test approach**: cargo's startup overhead (manifest parsing, target-dir resolution, dep-graph building) is 5–15s per invocation even on a warm build cache. 129 tests × 10s ≈ 20 minutes. `rustc_driver` in-process skips all of that — measured POC-grade direct-mode tests take <300ms each.

#### 4.4c Generic-wrapper routing (both modes)

Both modes share one subtlety: generic wrapper functions (like `__toylang_option_unwrap<T>`) are `#[inline(never)]`. Under default Rust semantics, an `#[inline(never)]` generic in an rlib with no local caller never gets codegen'd (rustc's mono collector walks from rlib-local roots). The user bin's `Instance::upstream_monomorphization` returns `Some(rlib_cnum)` for these wrappers, routing the link to the rlib's non-existent mono → link fails.

**Fix**: `override_queries` on `upstream_monomorphization`, returning `None` for consumer DefIds so the user bin mono'd the wrapper locally (where the plugin's backend emits it). ~5 LoC. POC #2 §4.3.D describes the exact shape. This was already scaffolded in sub-stage 4c step 1 (commit `51f0c5e`) so the facade-side code is in place; you just verify it fires correctly in the two-crate build.

### 4.5 Fork deletion

Once sub-stages 4a–4c land and tests pass against the now-patch-free fork, the fork branch is vestigial:

- Delete the fork branch (or keep as `archive/per-instance-mir` for history).
- Update `rust-toolchain.toml` / cargo config to use vanilla nightly-2025-01-15.
- Unlink `rustup toolchain list`'s `rustc-fork`.
- Retire `docs/usage/rebuilding-rustc-fork.md` (move to historical).

---

## 5. Implementation steps

Four sub-stages. Each is a standalone improvement; pause between any two if you need a break or a review checkpoint.

### Sub-stage 4a — partitioner override + plugin-owned consumer codegen (~1–2 weeks)

Goal: rustc stops codegen-ing consumer items; plugin handles them directly. `CODEGEN_SKIP_HOOK` becomes a no-op (safety net still installed) but the skip condition is never actually hit because consumer items never make it into the CGUs rustc processes.

1. **Create `queries/partition.rs`** with `lang_collect_and_partition` (shape in §4.1).
2. **Add `DEFAULT_PARTITIONER: OnceLock<...>` in `lib.rs`** following the existing `DEFAULT_OPTIMIZED_MIR` precedent.
3. **Wire the override** in `queries/mod.rs` alongside the existing 4 overrides. Save the upstream default before overriding.
4. **Add `stash_consumer_cgus` / `take_consumer_cgus` helpers** — a new `OnceLock<Mutex<Vec<CodegenUnit<'tcx>>>>` in `lib.rs` or a field on `FacadeMutableState`. The lifetime bookkeeping here is non-trivial — CGU references into `tcx.arena` must outlive the stash. Review with the TL if you get stuck.
5. **Extend `codegen_wrapper.rs::codegen_crate`** to consume the stashed CGUs. Route them through toylang's Inkwell backend (existing `generate_and_compile` + `generate_with_tcx` path).
6. **Verify `CODEGEN_SKIP_HOOK` never fires** — add temporary `eprintln!` inside the hook's closure. Run the full test suite. The eprintln should never print. If it does, the partitioner override is missing consumer items.
7. **Commit this sub-stage.** `CODEGEN_SKIP_HOOK` is still installed (safety net); `VISIBILITY_OVERRIDE_HOOK` unchanged; `FileLoader` unchanged. Tests 211/211.

### Sub-stage 4b — retire `CODEGEN_SKIP_HOOK` (~3–5 days)

Goal: remove fork patch 3. The facade stops installing the hook; the fork deletes the `pub static` and the `if ... { return; }` check in `mono_item.rs`.

1. **Remove `CODEGEN_SKIP_HOOK.set(...)` from `lib.rs::install_callbacks`.**
2. **Run the full suite.** Expect 211/211 — rustc's default codegen dispatch runs for non-consumer items only (because consumer items aren't in the CGUs).
3. **Edit the rustc fork** at `~/rust/compiler/rustc_codegen_ssa/src/mono_item.rs`:
   - Delete the `pub static CODEGEN_SKIP_HOOK: OnceLock<...>`.
   - Delete the `use` imports that only reference it.
   - Delete the `if CODEGEN_SKIP_HOOK.get().is_some_and(|f| f(tcx, instance)) { return; }` check inside the dispatch.
   - Audit `pub mod` flips in `rustc_codegen_ssa/src/lib.rs` — the `pub mod mono_item;` flip may no longer be needed. Restore to `mod mono_item;` if so (rustc's `-D warnings unreachable_pub` will tell you).
4. **Rebuild the toolchain** per `docs/usage/rebuilding-rustc-fork.md` — 5-step workflow, 8–12 min.
5. **Run the full suite.** Still 211/211.
6. **Commit.** Fork is now 1 patch.

### Sub-stage 4c — two-crate architecture in both modes + retire `VISIBILITY_OVERRIDE_HOOK` (~2–3 weeks)

Goal: `FileLoader` eliminated, stubs live in their own crate in both wrapper and direct modes, fork patch 5 retired. This is the largest sub-stage — split into three clearly-bounded sub-commits. Land each as a standalone unit before starting the next.

#### 4c.1 — wrapper-mode migration (~1 week)

Wrapper mode (`toylangc build`) moves to a two-member Cargo workspace per §4.4a. Direct-mode integration tests still use `FileLoader` at this point; `VISIBILITY_OVERRIDE_HOOK` still installed. Only the 15 standalone tests exercise this path.

1. **Extend `toylangc/src/build.rs`** to emit a two-member workspace. Use POC #2's `build.rs` as a starting point but adapt to current `main`'s trait signatures. ~100 LoC of build-file-emission.
2. **Adjust `stub_gen.rs`** to write full `src/lib.rs` content (with `#![feature(linkage)]` at crate root if §6.9's probe says we still need it, pub re-exports, etc.).
3. **Update `toylangc/src/main.rs`**'s wrapper-mode handling to route the stub rlib compile vs the user bin compile distinctly. Stub rlib: plain rustc with facade overrides installed (plugin does its partitioning work). User bin: same, plus the `upstream_monomorphization` override for generic wrappers.
4. **Verify the `upstream_monomorphization` override (from commit `51f0c5e`) fires correctly** in the two-crate build. Watch for link errors naming `__toylang_option_unwrap` or similar generic wrappers — those indicate the override isn't routing correctly.
5. **Set linkage in the partitioner override** — in `lang_collect_and_partition`, consumer CGUs carry `(Linkage::External, Visibility::Default)`. This is what makes `#[linkage = "external"]` redundant per §6.9 — verify with the probe.
6. **Run the full suite.** Standalone tests (`toylangc/tests/standalone_tests.rs`) all pass via the new wrapper-mode path. Integration tests still pass via `FileLoader` (unchanged in this step).
7. **Commit 4c.1.** `FileLoader` still alive for direct mode; `VISIBILITY_OVERRIDE_HOOK` still installed; 211/211.

#### 4c.2 — direct-mode migration (~1 week)

Direct mode (integration tests) moves to in-process two-crate compilation per §4.4b. `FileLoader` becomes unused after this step. `VISIBILITY_OVERRIDE_HOOK` still installed — retired in 4c.3.

1. **Add `tempfile` as a dev-dependency** in `toylangc/Cargo.toml`.
2. **Write a new test helper** in `toylangc/tests/common/mod.rs` (create if doesn't exist) implementing `compile_toylang_direct_two_crate` per §4.4b's sketch. Handles tempdir allocation, source writing, two `rustc_driver::RunCompiler` invocations, cleanup-on-drop.
3. **Add facade helpers** for source generation: `generate_two_crate_sources(toylang_source) -> (stub_content, user_main_content)`. This wraps the existing `stub_gen` + wrapper-source-generation machinery that `FileLoader` currently calls into. Keep the old `FileLoader`-driven helpers intact for now — we'll delete them in 4c.3.
4. **Migrate the 129 integration tests in one pass** to use the new helper. Most should be a mechanical find-replace — test fixtures call `compile_toylang_direct(source)` today, just swap to `compile_toylang_direct_two_crate(source)`. A handful of tests may have FileLoader-specific assertions (e.g., asserting that `__lang_stubs.rs` resolves to a specific path); those get adapted to match the new two-crate reality.
5. **Expect ~2–3 min test suite runtime** post-migration (vs today's ~30s). Per the CLAUDE.md redirect convention, run with `cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage4.txt` and `grep "test result:" /tmp/stage4.txt`. Runtime is load-bearing to verify here — if you see 10+ minutes, something's wrong (likely cargo spawning somewhere it shouldn't).
6. **Grep for remaining `FileLoader` references in code.** Should be limited to `file_loader.rs` itself + possibly one installation site in `driver.rs`. Document the remaining sites in the commit message.
7. **Commit 4c.2.** `FileLoader` still present but unused by any caller; `VISIBILITY_OVERRIDE_HOOK` still installed; 211/211.

#### 4c.3 — retire `FileLoader`, retire `VISIBILITY_OVERRIDE_HOOK`, retire fork patch 5 (~3–5 days)

1. **Delete `file_loader.rs`.** The facade no longer has anything using it.
2. **Remove the `FileLoader` installation in `driver.rs`**. The `rustc_interface::Config::file_loader` field goes unset; rustc uses `RealFileLoader` by default.
3. **Delete the `stub_gen` + wrapper-source helpers that were `FileLoader`-specific.** Some may have dual purpose (also used by the wrapper-mode build.rs or the direct-mode two-crate helper); retain those, delete the ones that were unique to FileLoader injection.
4. **Remove `VISIBILITY_OVERRIDE_HOOK.set(...)`** from `lib.rs::install_callbacks`.
5. **Edit the rustc fork** at `~/rust/compiler/rustc_monomorphize/src/partitioning.rs`:
   - Delete `pub static VISIBILITY_OVERRIDE_HOOK`.
   - Delete the hook-consulting check in `mono_item_linkage_and_visibility`.
   - Audit `pub mod` flips; revert as appropriate.
6. **Rebuild the toolchain.** 8–12 min.
7. **Run the full suite.** 211/211.
8. **If §6.9's probe succeeded**, this is also where you drop `#[linkage = "external"]` + `#![feature(linkage)]` from `stub_gen.rs`. Verify tests still pass.
9. **Commit 4c.3.** Fork is now 0 patches. `FileLoader` gone. Both modes run through the two-crate architecture.

### Sub-stage 4d — fork deletion + toolchain switch + final cleanup (~2–3 days)

Goal: delete the fork entirely. Toylang builds against vanilla nightly.

1. **Verify the fork is empty** — grep for any remaining diffs in `~/rust` against upstream. There shouldn't be any; the previous sub-stages should have restored everything.
2. **Switch the toolchain link.** `rustup toolchain link rustc-fork` pointing at vanilla nightly-2025-01-15 instead of `~/rust/build/host/stage2`. (Or uninstall `rustc-fork` and rename the default toolchain to match project expectations — judgment call.)
3. **Update `rust-toolchain.toml`** to specify vanilla `nightly-2025-01-15` if the toolchain link is removed.
4. **Test with vanilla nightly.** Run the full suite.
5. **Move `docs/usage/rebuilding-rustc-fork.md` to `docs/historical/`.** It's no longer needed.
6. **Update architectural docs:**
   - `rust-interop-guide.md` front-matter: "Zero-fork architecture; plugin-based codegen dispatch."
   - `CLAUDE.md` background: "Plugin-based codegen via `-Zcodegen-backend=...`" (or however you've wired it).
   - `docs/reasoning/rustc-fork-design-space.md` §4.2: LANDED status header.
   - `future-architecture-investigations.md`: mark spike as LANDED.
   - `HANDOFF-TL.md`: update fork state + roadmap status.
   - `README.md` if it mentions the fork.
7. **Move this handoff doc** (`handoff-codegen-backend-plugin.md`) to `docs/historical/`.
8. **Commit.** This is the landing commit — the moment erw becomes a zero-fork project.

---

## 6. Critical subtleties

### 6.1 The partitioner override returns arena-allocated slices

Both return-tuple elements are `&'tcx ...`. Your override must allocate into `tcx.arena` — not a `Vec::leak` or a thread-local. The POC #1 `queries/optimized_mir.rs` shows the pattern (`tcx.arena.alloc_slice(&...)`); follow that style exactly.

### 6.2 Consumer CGU stashing has a lifetime problem

The `consumer_cgus` list you stash contains `&'tcx CodegenUnit<'tcx>` references. These live as long as `tcx` does (i.e., the whole `codegen_crate` call). Your stash must hold them in a way that doesn't outlive `tcx`. Options:

- **Thread-local scoped to `codegen_crate`**: cleanest, but requires careful lifetime management.
- **`Arc<Mutex<Vec<_>>>` with lifetime laundering via `tcx.lift`**: heavier machinery but more forgiving.
- **Process the CGUs inline in the partitioner override**: if you can do all the consumer-side work inside `lang_collect_and_partition` itself, you dodge the lifetime issue. Tradeoff: mixing partition-time and codegen-time concerns; may cause re-entrancy issues with other queries.

POC #1's `DEFAULT_OPTIMIZED_MIR` pattern sidesteps this (function pointers don't carry lifetimes). Your case has data. Review with the TL before committing to an approach.

### 6.3 `-Zcodegen-backend=...` vs `rustc_driver::Callbacks`

Today's codegen wrapper is installed via `rustc_driver::Callbacks`. Full plugin mode uses `-Zcodegen-backend=<path>` loading a cdylib at runtime. Stage 4 can go either way:

- **Keep `rustc_driver::Callbacks`**: minimal migration, the wrapper is already a `CodegenBackend`. Everything installs from the existing entry points. Preferred for this stage's scope.
- **Switch to `-Zcodegen-backend=valec`**: idiomatic and symmetric with cranelift/gcc, but adds build infrastructure (cdylib target, `__rustc_codegen_backend` entry point, compile-time loading).

**Recommendation: stay with `rustc_driver::Callbacks` for this stage.** The partitioner override works either way. If the TL wants `-Zcodegen-backend=...` migration later, it's a separate follow-up. Don't mix concerns.

### 6.4 The three risks POC #2 surfaced are load-bearing to address

- **Risk #1**: `#[inline(never)]` generic wrappers in an rlib with no local caller aren't codegen'd. Fix: `upstream_monomorphization` override (§4.4 above).
- **Risk #8**: plain rustc codegens `unreachable!()` bodies in the stub rlib. **Under plugin mode this is subsumed** — the plugin IS the codegen for the stub rlib compile too, and it skips the bodies. Verify your plugin is installed for BOTH crate compiles (stub rlib AND user bin).
- **Risk #9**: single-crate-compile assumption in `llvm_gen.rs`. **Already fixed in stage 2** (`as_local()` filter removed, `is_from_lang_stubs_safe` as the cross-crate predicate). Don't reintroduce.

### 6.5 `#![feature(linkage)]` must be at a REAL crate root

The stub rlib's `src/lib.rs` is a real crate root. Putting `#![feature(linkage)]` there works. The current `FileLoader`-injected module has `#![feature(linkage)]` at a module level (not crate root), which rustc rejects with E0658. This is WHY we're moving to separate-crate stubs — POC #2 Step 2 has the exact diagnostic.

### 6.6 The `walked_entry_points` + `ActiveParamMap` from stages 1 and 3 should Just Work

You're not touching the consumer-side dep-discovery machinery. `collect_generic_rust_deps` fires the same way; `notify_concrete_entry_point` fires the same way. If you accidentally break them by re-entering or lock-inverting, the Phase 6 unwrap tests and the `test_diamond_call_pattern` test will tell you loud and fast.

### 6.7 Wrapper mode: Cargo workspace complexity is real

Stub rlib and user bin in a two-member workspace + `RUSTC_WORKSPACE_WRAPPER` routing to toylangc + `CARGO_PRIMARY_PACKAGE` env var sniffing to determine "am I the stub rlib or the user bin?" is fiddly. POC #2's `build.rs` has patterns for this; use them as a starting point but expect to iterate.

### 6.7b Direct mode: `rustc_driver` orchestration complexity is different-but-real

Unlike wrapper mode, direct mode's two-crate story has no cargo — you're invoking `rustc_driver::RunCompiler` twice directly. Watch for:

- **Argument-list composition.** The stub rlib invocation needs `--crate-type rlib --crate-name lang_stubs -o <path>/liblang_stubs.rlib <path>/lib.rs` plus whatever flags match the existing integration-test setup (edition, target spec, etc.). The user bin invocation needs `--extern lang_stubs=<path>/liblang_stubs.rlib <path>/main.rs -o <path>/test_bin`. Copy the current integration-test flag setup as a baseline; add the two-crate specifics.
- **Parallel test safety.** `cargo test` runs tests in parallel by default. Each test's `tempfile::tempdir()` call gets an independent OS-level temp path, so source files don't race. But `rustc_driver`'s process-level state (callbacks, `CONFIG`/`MUTABLE_STATE` OnceLocks) is shared across the whole test process. Today the facade installs callbacks once per process and serializes work via `MUTABLE_STATE` mutex; the two-crate direct-mode harness needs to slot into the same locking story without deadlock. If you find tests hanging, check @GCMLZ for locking rules and verify `MUTABLE_STATE` is released between the stub-rlib compile and user-bin compile of the same test.
- **Callback reinstallation.** `install_callbacks` uses `OnceLock::set` — set-once semantics. If a test tries to reinstall (even with identical content) because a prior test already installed, the second call silently no-ops. That's fine as long as the installed callbacks are consistent across all tests. If you end up with per-test callback variations, you need a different mechanism — flag this to the TL before inventing one.
- **Output capture.** The existing `compile_toylang_direct` helper captures stdout/stderr from the compiled binary's execution. The new helper does the same, plus captures any compile-time errors from `rustc_driver::RunCompiler::run()`. If a stub-rlib-compile fails, the user-bin-compile shouldn't run; handle the error chain cleanly.

POC experience suggests budgeting ~3 days for the direct-mode helper alone — less mechanical than it sounds, because every test's flag/path composition has to be right.

### 6.8 Don't forget doc updates

Stages 1/2/3 each included a doc pass. Stage 4 is architectural enough that skipping the doc pass would leave the repo's docs structurally wrong for weeks. Budget 1/2 day for sub-stage 4d's doc update.

### 6.9 Probe: is `#![feature(linkage)]` even needed under the plugin architecture?

Fork patch 5 (`VISIBILITY_OVERRIDE_HOOK`) forced `(Linkage::External, Visibility::Default)` on `__lang_stubs` items via a partitioner hook. Stage 4c moves that linkage-setting into the plugin's `collect_and_partition_mono_items` override (§4.4 step 6) — **the plugin sets linkage directly on the CGU items it returns**.

That raises a question worth verifying during 4c: if the plugin is already setting `(Linkage::External, Visibility::Default)` on CGU items, is `#[linkage = "external"]` on the source-level wrapper functions still needed?

Likely no. `#[linkage]` is a source-level attribute that eventually lowers into the same `(Linkage, Visibility)` pair the partitioner manipulates — if the plugin overrides the partitioner anyway, the attribute is redundant.

**Probe during 4c step 6.** Try emitting the stub rlib's `src/lib.rs` WITHOUT `#[linkage = "external"]` AND WITHOUT `#![feature(linkage)]`. Run the full suite. Watch especially `test_option_unwrap_basic` and the Phase 6 `_unwrap` tests — they're the most sensitive to internalization-of-generic-wrappers bugs. If tests pass, the attribute is redundant; drop both the attribute emission and the crate-root feature gate. If tests fail (internalization re-emerges), the plugin's partitioner override isn't covering every DefId that needs External linkage — investigate, either widen the override's reach or keep the attribute.

**Why this matters (modest benefit):** retires one nightly feature from the stack, makes the stub rlib compile as plain Rust (no `#![feature(...)]` at all), cleans up the "why are we using `linkage`?" question that future facade consumers would have to re-derive. Doesn't materially reduce maintenance cost — `rustc_private` API drift dominates — so this is pedagogical cleanup, not a priority. But since stage 4's partitioner-set-linkage design makes the probe cheap, it's worth taking if it works.

**If the probe succeeds, update:**
- `toylangc/src/stub_gen.rs` — drop the `#[linkage = "external"]` emission and the `#![feature(linkage)]` crate-root line.
- `docs/architecture/rust-interop-guide.md` §10.6.5 — the "rejected alternatives" discussion of `#[linkage]` becomes historical-interest-only; note in passing.
- Commit message for 4c should mention the retirement explicitly ("plus retires `#![feature(linkage)]` — the plugin's partitioner override subsumes it").

**If the probe fails**, that's useful information too — leave a one-paragraph note in the commit explaining what internalization pattern the attribute was load-bearing for, so nobody re-tries the removal in a future refactor.

---

## 7. Verification

### Build & test

Per `CLAUDE.md` redirect convention:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage4.txt
grep "test result:" /tmp/stage4.txt
```

After each sub-stage: 211/211 passing, 0 failed, 0 ignored.

After sub-stage 4d: same suite passes using vanilla nightly-2025-01-15 (no fork).

Zero warnings:

```bash
cargo +rustc-fork check -p toylangc 2>&1 > /tmp/stage4.txt
grep "warning:" /tmp/stage4.txt
```

### Required greps after 4c.3

```bash
rg "CODEGEN_SKIP_HOOK|VISIBILITY_OVERRIDE_HOOK" rustc-lang-facade/ toylangc/  # zero hits expected
rg "FileLoader" rustc-lang-facade/ toylangc/                                   # zero hits expected
rg "file_loader" rustc-lang-facade/                                            # zero hits expected
cd ~/rust && rg "CODEGEN_SKIP_HOOK|VISIBILITY_OVERRIDE_HOOK"                   # zero hits expected
```

### Runtime check after 4c.2

Integration tests run via in-process two-crate compilation. Expect ~2–3 min for the 129-test integration suite. If you're seeing 10+ minutes, something is wrong — likely cargo spawning somewhere (grep for `Command::new("cargo")` or `std::process::Command` in the new test harness) or the tempdir-cleanup overhead has gone non-linear.

### Required greps after 4d

```bash
cd ~/rust && git diff upstream/master -- compiler/   # (adjust upstream refname)
# expect: no differences — fork is deleted
```

### Sanity: observable behavior identical

Before starting, pick any Phase 7 standalone test (e.g., `uuid_test`). Capture the full test run including produced binary's behavior. After 4d, re-run and diff. Expect byte-identical behavior — the architectural change is invisible to user-level semantics.

### The zero-fork moment

After 4d, `git log` on your main branch should have four stage-4 sub-commits. The rustc-fork directory at `~/rust` should be at vanilla nightly-2025-01-15 with no diffs. The toolchain `rustc-fork` can be uninstalled:

```bash
rustup toolchain uninstall rustc-fork
rustup toolchain install nightly-2025-01-15
```

The full test suite should pass with `cargo +nightly-2025-01-15 test -p toylangc` (modulo `rust-toolchain.toml` pinning).

That's the landing.

---

## 8. Out of scope / follow-ups

**Not in this task** (flag but don't do):

- **Upstream PR for `LlvmCodegenBackend(())` unseal** — the spike's speculative workstream. Optional, independent, per HANDOFF-TL.md §3b.
- **Switch to `-Zcodegen-backend=valec` style** — see §6.3. Separate follow-up; keep `rustc_driver::Callbacks` for this stage.
- **`rustc_public` adoption** — orthogonal churn-reduction work. Track separately per `future-architecture-investigations.md`.
- **Rename the fork branch** — moot after 4d (branch deleted).
- **Revisit test coverage** — tests should all still pass. If stage 4 surfaces gaps in test coverage, file a follow-up rather than widening scope.

**Follow-up tickets to file** after landing:

- Investigate whether `codegen_wrapper.rs` can be renamed / restructured now that it's the primary codegen dispatcher, not just a wrapper.
- Consider whether `toylangc/src/build.rs`'s two-member workspace emission can be generalized into a small library (useful for other future facade consumers).
- Update the Vale response draft (`response-reducing-rustc-fork.md`) to reflect the zero-fork landing.

---

## 9. Rollback and if-you-get-stuck

### Rollback

Each sub-stage commits independently. Rollback is:

- **4a fails**: `git revert` the sub-stage commit. Nothing else to do — the fork patches are still intact, the old code path (with `FileLoader` + `CODEGEN_SKIP_HOOK` + `VISIBILITY_OVERRIDE_HOOK`) still works.
- **4b fails**: revert the facade-side changes. Re-apply the fork patch 3 (you'll have the diff in `git log` of the fork branch, or just re-apply from a pre-4b snapshot of `~/rust`). Toolchain rebuild.
- **4c fails**: larger rollback — revert the build.rs + stub_gen + driver changes. Re-apply fork patch 5. Toolchain rebuild.
- **4d fails**: this is fully behavioral — re-link the fork toolchain, revert any rust-toolchain.toml changes.

Don't panic on any single failure. Each sub-stage is a standalone improvement; reverting one doesn't lose prior sub-stages' wins.

### Come find the TL if:

- **The partitioner override produces different CGUs for different nightly builds.** Rustc's partitioner is version-sensitive; if your filter logic depends on CGU structure, it could break across rustc bumps. The TL has seen this before.
- **Consumer CGU stashing hits lifetime errors you can't resolve.** §6.2 is genuinely hard; don't spin for more than half a day before escalating.
- **After removing `CODEGEN_SKIP_HOOK` (4b), tests fail because rustc is still trying to codegen consumer items.** Means the partitioner override from 4a missed some consumer DefIds. Grep the failing items' DefPaths against `is_from_lang_stubs_safe` to find what's slipping through.
- **The stub rlib compiles cleanly but link fails with "cannot find function __toylang_...".** Classic Risk #1 — `upstream_monomorphization` override isn't firing or is returning the wrong value. POC #2 Step 3's `nm` analysis is the diagnostic.
- **Phase 6 unwrap tests fail after 4c.** `#![feature(linkage)]` + `#[linkage = "external"]` must be at the real stub-crate root. Verify with the stub rlib's emitted `src/lib.rs`.
- **Vanilla nightly compilation (4d) fails in ways the forked toolchain didn't.** Unlikely if 4c is fully green, but if it happens, likely a `cfg` or feature gate that was always-enabled in the fork and isn't in vanilla. Grep `~/rust` for `cfg` differences.

Don't be shy. Stage 4 is the largest refactor erw has done; nobody expects you to breeze through it in a single week.

### A note on the shape of this handoff

Unlike stages 1/2/3, stage 4 doesn't have a single clean "remove this filter" or "rename this function" victory. It's a sequence of architectural moves that, together, eliminate the rustc fork. The sub-stages are intentionally loosely-coupled — each improves the codebase on its own. Clean stopping points if you need to pause or escalate:

- **After 4b**: fork at 1 patch (`VISIBILITY_OVERRIDE_HOOK` only). `FileLoader` still present. Both compile modes unchanged. TL can pick up 4c/4d later with prior context fresh. *(Junior already reached this point in commits `1d862f4` / `13d8f12` / `51f0c5e`.)*
- **After 4c.1**: fork still at 1 patch. Wrapper mode migrated to two-crate architecture; direct mode still uses `FileLoader`. Mixed-architecture state but functional. The 15 standalone tests exercise the new wrapper-mode path; 129 integration tests still use the old direct-mode path.
- **After 4c.2**: fork still at 1 patch. Both modes migrated to two-crate; `FileLoader` is present but unused. Test suite runtime has increased (~2–3 min). Legitimate stopping point if 4c.3 gets hard.
- **After 4c.3**: fork at 0 patches. `FileLoader` gone. Both modes on the target architecture. 4d is just cleanup / doc pass.
- **After 4d**: erw is a zero-fork project.

The goal is zero-fork. The path there is four sub-stages with 4c itself having three sub-commits. Don't race; land each checkpoint solidly before starting the next, and if you exhaust yourself at any of the bulleted stopping points, that's a legitimate landing — the TL can pick up what's left.
