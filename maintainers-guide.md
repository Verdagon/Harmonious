# Maintainer's Guide

For the next engineer picking up the codebase, whether you're doing a nightly
bump, adding a feature, debugging a regression, or rebuilding the architecture.
The goal of this document is to compress the operational knowledge that lives
in many places into one practical reference.

If you have time to read one other thing first, read **§3, §5, §25.2, and §26**
of `rust-interop-architecture.md`. Everything else can be looked up as needed.

---

## Quick start: the "read first" map

Stop and read these BEFORE touching code:

| If you're about to... | Read first |
|---|---|
| Bump the rustc nightly pin | This doc § "Per-bump procedure"; arch doc §3.4, §3.5; `docs/usage/rebuilding-rustc-fork.md` |
| Add a fork patch | arch doc §3.2; this doc § "When to extend the patches vs the facade" |
| Add a Sky-emitted symbol | arch doc §26.16 SMPLZ; this doc § "Invariants you maintain" |
| Add a query override | arch doc §26.2 GCMLZ; this doc § "Invariants you maintain" |
| Add a new emission shape (drop glue, vtables, async poll, closures) | arch doc §26.16 SMPLZ + §26.17 SBMNBIZ (vacuous but historically informative); this doc § "Where the load-bearing discipline lives" |
| Add a `type_params.is_empty()` branch | arch doc §26.15 NNGZ + §1.5.5; the arch-fence CI test will flag you |
| Investigate a "Sky symbol disappeared" link error | arch doc §25.2 B14 + the playbook in §25.2 |
| Investigate a runtime panic from an inlined unreachable | arch doc §F.13, §F.17 (now design history but the diagnostic pattern still applies) |
| Investigate the partition filter dropping a `CodegenUnit` field | arch doc §25.2 B2 |
| Add a stdlib type fixture or build-script test | arch doc §28.1 test-fences list; this doc § "Test coverage gaps" |

---

## The mental model in five sentences

1. **Rustc is a query system.** Most of its work is structured as cached
   functions; plugins override individual queries via `Config::override_queries`.
2. **Sky/toylang is a "consumer language" plugin** that hooks into rustc deeply
   enough that Sky source can call Rust libraries and Rust source can call Sky
   libraries.
3. **Where rustc's sanctioned overrides aren't enough, we add fork patches**
   — currently four: the `per_instance_mir` trio (Instance-keyed MIR query
   needed because Sky's arbitrary-typed comptime args can't be rustc-substituted)
   and `fill_extra_modules` (the hook that lets Sky's emitted LLVM IR ride
   rustc's LTO pipeline).
4. **Stub rlibs project Sky items into rustc's universe**: rustc parses
   `pub fn foo<T>() { unreachable!() }` declarations, type-checks them,
   discovers their dep graph during monomorphization, but never emits machine
   code for them — Sky's `collect_and_partition_mono_items` filter removes
   them from the CGU list before LLVM codegen, and Sky's `fill_extra_modules`
   hook contributes the real bodies.
5. **Pass-through is sacred**: with no `__SKY_STUBS_MARKER` present, Sky's
   rustc must produce byte-identical output to vanilla nightly.

---

## What you're maintaining (the layered picture)

### Layer 1: the rustc fork (`~/rust/`)

Four patches, all in the rustc tree:

- **Patches 1-3 (per_instance_mir trio):** `compiler/rustc_middle/src/query/mod.rs`
  declares the query; `compiler/rustc_monomorphize/src/collector.rs` calls
  it before falling through to `instance_mir`; `compiler/rustc_mir_transform/src/lib.rs`
  registers a default provider returning `None`. ~21 lines of actual code.
- **Patch 4 (fill_extra_modules hook):** `compiler/rustc_codegen_ssa/src/traits/backend.rs`
  declares the trait methods (`allocate_extra_module`, `fill_extra_modules`)
  plus the `ExtraModuleAllocator<M>` trait; `compiler/rustc_codegen_ssa/src/base.rs`
  calls the hook from `codegen_crate`; `compiler/rustc_codegen_llvm/src/lib.rs`
  adds raw-pointer accessors (`llcx_raw_mut`, `llmod_raw`) and the
  `set_fill_extra_modules_hook` OnceLock plumbing. ~100 lines across 4 files.

The fork branch is `per-instance-mir` in `~/rust/`. The patches live in the
working tree (uncommitted modifications) and get rebased onto each new
nightly target. The branch has had patch 5 (`consumer_lang_active`) in the
past; that retired 2026-06-22 — see arch §F.14.1 / §F.17 if you need to
understand the design history.

### Layer 2: the facade (`rustc-lang-facade/`)

A reusable library that installs query overrides + the fill_extra_modules
hook + provides infrastructure for consumer-language plugins. Key files:

- `src/lib.rs` — the `is_consumer_codegen_target` predicate, `is_sky_active`
  marker walk, `SkyUniverse` lock-free registry, OnceLock plumbing for
  saved upstream providers.
- `src/queries/mod.rs` — installs the query overrides.
- `src/queries/partition.rs` — the `collect_and_partition_mono_items`
  filter (restored 2026-06-22; ~95 LOC).
- `src/queries/per_instance.rs` — provides per_instance_mir bodies for
  consumer items.
- `src/queries/layout.rs`, `drop_glue.rs`, `symbol_name.rs`,
  `cross_crate_inlinable.rs` — the other query overrides.
- `src/extra_modules_hook.rs` — installs patch 4's hook.
- `src/driver.rs` — the `LangDriver` Callbacks impl, including
  `after_expansion → load_upstream_sidecars`.

### Layer 3: the consumer (`toylangc/`)

Toylang is the reference consumer; Sky proper will be the real consumer
when it's built. Toylangc has:

- A frontend (parser, typechecker, comptime, code-gen-queue construction).
- An LLVM emitter (`src/llvm_gen.rs`) that consumes the codegen queue and
  emits Sky bodies via Inkwell.
- Stub_gen (`src/stub_gen.rs`) that generates the Rust stub source for
  each Sky library.
- Build orchestration (`src/build.rs`, `src/main.rs`) that drives cargo.

### Layer 4: the test fences

In `toylangc/tests/`:

- `integration_projects.rs` — ~333 fixture tests.
- `common/inlining_harness.rs` — disassembly + symbol-table helpers
  used by the inlining matrix.
- `integration_projects/inlining/` — ~180 inlining-matrix fixtures
  exercising 7-case taxonomy × LTO modes × opt-levels × codegen-units.
- `passthrough_corpus.rs` — vanilla-vs-fork byte-identity tests
  (currently 1 passing, 3 broken pre-existing).
- `integration_projects/release_mode_smoke/`, `opt_level_3_fat_lto_smoke/`,
  `lto_smoke/` — specific failure-mode canaries.

---

## Per-bump procedure (nightly rustc upgrade)

Every ~6 months you'll bump the rustc nightly pin. Budget: **~1.5-2 weeks
of focused engineering**, scheduled as dedicated work (not interleaved
with features).

### Step-by-step

1. **Decide to bump.** Triggered by ~6 months elapsed OR a forcing function
   (need an upstream feature, security update, etc.). Don't chase the
   latest nightly.

2. **Pick the target.** ~3 months old. This lets ecosystem-adjacent projects
   (cranelift, miri, rust-analyzer) hit drift first.

3. **Snapshot the current state.**
   ```
   cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml \
       --test integration_projects -- --test-threads=1 > tmp/pre-bump-test-snapshot.log 2>&1
   ```
   Record the test count. This is your baseline.

4. **Bump the rustc fork.**
   ```
   cd ~/rust
   git fetch upstream
   git checkout nightly-<target-date>
   # Rebase the per-instance-mir branch onto the new nightly:
   git checkout per-instance-mir
   git rebase nightly-<target-date>
   # Or, if patches are kept as working-tree modifications, manually
   # re-apply them to the new tree.
   ```
   The per_instance_mir trio rebases cleanly most of the time. Patch 4
   (`fill_extra_modules`) is exposed to codegen-coordinator restructures
   — budget a half-day if rustc has restructured `rustc_codegen_ssa::base`
   or `rustc_codegen_ssa::traits::backend`.

5. **Build + install the fork.** Follow `docs/usage/rebuilding-rustc-fork.md`
   EXACTLY. Steps 4 and 5 (`x.py build library --stage 2` + reinstall
   rustc-dev) are MANDATORY — skipping them produces a binary/metadata
   mismatch that crashes rustc internally with `profiler.unwrap()` panic.
   The failure mode is unrelated to the cause and you'll waste hours
   debugging if you skip.

   ```
   cd ~/rust
   python3 x.py dist rustc-dev > tmp/rustc-rebuild.log 2>&1
   cd /tmp && rm -rf rustc-dev-<ver>-aarch64-apple-darwin
   tar xzf ~/rust/build/dist/rustc-dev-<ver>-aarch64-apple-darwin.tar.gz
   cd rustc-dev-<ver>-aarch64-apple-darwin
   bash install.sh --prefix=$HOME/rust/build/host/stage2
   rm -rf $HOME/rust/build/host/stage2/lib/rustlib/rustc-src
   cd ~/rust
   python3 x.py build library --stage 2
   cd /tmp/rustc-dev-<ver>-aarch64-apple-darwin
   bash install.sh --prefix=$HOME/rust/build/host/stage2  # REINSTALL — mandatory
   ```

6. **Sanity-check the toolchain.**
   ```
   /Users/verdagon/.rustup/toolchains/rustc-fork/bin/rustc --version
   echo 'fn main() { println!("hi"); }' > /tmp/hello.rs
   /Users/verdagon/.rustup/toolchains/rustc-fork/bin/rustc /tmp/hello.rs -o /tmp/hello && /tmp/hello
   ```

7. **Verify dylib actually rebuilt.** Check `librustc_driver-*.dylib`
   timestamp in `~/.rustup/toolchains/rustc-fork/lib/`. If it didn't
   change after your dist step, `x.py dist rustc-dev` didn't actually
   compile your edits — you're shipping the previous build. This is a
   silent failure mode; cargo's incremental machinery in x.py is opaque
   about it.

8. **Rebuild the facade + toylangc.**
   ```
   LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
       cargo +rustc-fork build --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --tests
   ```
   Fix any compile errors. The facade is exposed to:
   - `MonoItemPartitions` / `CodegenUnit` struct shape changes
     (partition filter rebuilds CGUs manually).
   - MIR construction APIs (synthetic body building).
   - Query system providers struct restructuring.
   - ABI helper API drift.
   - `Layout` / `LayoutData` shape shifts.

   Make each drift-fix a SEPARATE commit. Don't batch. Future bumps that
   bisect get cleaner.

9. **Run cold tests.**
   ```
   rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache
   find /Users/verdagon/erw/toylangc/tests/integration_projects \
       -name ".toylang-build" -type d -exec rm -rf {} + 2>/dev/null
   LLVM_SYS_211_PREFIX=... cargo +rustc-fork test --manifest-path .../Cargo.toml \
       --test integration_projects -- --test-threads=1 \
       > tmp/cold-test.log 2>&1
   ```
   Expect the same count as your pre-bump snapshot. Diagnose regressions
   per "Recognizing failure modes" below.

10. **Run warm tests.** Same command. Catches incremental-compilation
    edge cases.

11. **Run pass-through corpus.**
    ```
    cargo +rustc-fork test --manifest-path .../Cargo.toml --test passthrough_corpus
    ```
    Confirms pure-Rust crates compile byte-identically.

12. **Update the arch doc's §3.4 empirical bump-cost data** with what
    actually happened this bump.

### Gotchas you might hit

- **The `~/rust/` source tree may show 500+ deleted files in `git status`.**
  This is accumulated `.old.old.old...` backup damage from install.sh's
  rustc-src step. Restore with `git ls-files --deleted | xargs git restore`
  (after confirming there's no real work to lose). Periodically clean up
  the `.old*` backup files too if disk usage matters.

- **`x.py dist rustc-dev` succeeding does NOT mean the dylib rebuilt.**
  See step 7 above.

- **Step 4 of the rebuild docs requires the rust source tree to be
  intact.** If `cargo metadata` fails inside `x.py build library --stage 2`,
  you have source-tree damage. Restore files before continuing.

- **Incremental cache mismatch after a query-system schema change.**
  Symptom: tests fail in seemingly random ways with `Found unstable
  fingerprints`. Mitigation: wipe `target/integration-projects-cache`.

- **Toylangc's wrapper-mode dispatch.** Integration tests of LTO behavior
  MUST invoke through toylangc's wrapper, not directly via cargo.
  Direct-cargo invocations bypass `RUSTC_WORKSPACE_WRAPPER` and Sky's
  hook never installs — the build succeeds but the binary panics at
  `unreachable!()` stubs (arch §F.11 / §C7).

---

## Recognizing failure modes

When a test fails after a bump, the failure shape tells you where to look.

### "Undefined symbols" link error mentioning `__lang_stubs` disambig

Two distinct mechanisms produce nearly-identical error messages. Disambiguate
with `llvm-objdump -t` on the post-LTO `.o`:

| Symbol shape | Mechanism | Where to look |
|---|---|---|
| `g F __TEXT,__text _<symbol>` (global) | The partition filter is intact but rustc's natural cstore-walk isn't populating `upstream_monomorphizations_for`. Check the `LangDriver::config` heuristic that forces `share_generics = true` at stub rlib compiles. Or rustc's `Instance::upstream_monomorphization` may have restructured. | arch §25.2 B14 |
| `l F __TEXT,__text _<symbol>` (local) | LTO `internalize` demoted the symbol. The `@llvm.used` pin is missing or using the weaker `@llvm.compiler.used`. | arch §25.2 B15, §26.16 SMPLZ |
| symbol absent entirely | `GlobalDCE` removed it. Same root as B15 — the pin is missing. | arch §25.2 B13 |

The arch doc's §25.2 carries the full "Sky symbol disappeared somewhere
in the LLVM pipeline" playbook. Use `RUSTFLAGS="-C save-temps"` to preserve
the intermediate `.bc` files; `llvm-dis` and `llvm-objdump -d` show what
LLVM did at each pass.

### Runtime panic with "internal error: entered unreachable code"

The binary built but a Sky function panicked at the stub's `unreachable!()`
body instead of executing Sky's real body. This is the SBMNBIZ scenario
(arch §26.17 — now vacuous post-2026-06-22 but the diagnostic pattern still
applies for regressions):

- **Did the partition filter run?** Verify in
  `rustc-lang-facade/src/queries/mod.rs` that
  `providers.queries.collect_and_partition_mono_items` is being set.
- **Did the filter actually remove items?** Add a debug log to `partition.rs`
  counting items removed per CGU.
- **Did `fill_extra_modules` produce a body?** Check toylangc's emission
  queue under `RUSTFLAGS="-C save-temps"`; look for the Sky CGU in
  `.no-opt.bc` files.
- **If both ran, check the partitioner restructure.** The most likely cause
  is rustc adding a new field to `CodegenUnit` that the rebuild loop in
  `partition.rs` doesn't preserve.

### Tests panic with `profiler.unwrap()` or similar rustc-internal panic

ABI mismatch between rustc-fork's dylib and its installed metadata. Caused
by an incomplete rebuild (steps 4-5 not run, or run on a damaged source
tree). Recover by:

1. Restoring the rust source tree (`git ls-files --deleted | xargs git restore`).
2. Re-running steps 4-5 of the rebuild procedure cleanly.

### "Sky body isn't getting emitted" — runtime fallthrough or wrong output

Per arch §F.13, the cascade that discovers monomorphizations fires at the
**stub rlib compile**, not at user_bin. Check WHICH compile is supposed to
emit the body:

- **Non-generic Sky items** (exports, cascade-discovered trait-impl methods):
  emit at the owning crate's compile via `fill_extra_modules`.
- **Generic items**: emit at the first compile session where a concrete
  instantiation arises (typically the binary's compile).
- **Sky-internal non-export items**: emit at the same session as the export
  that reached them, via `walk_and_stash_internal_callees` Sky-side
  discovery.

If you're debugging a missing body, instrument the cascade at the expected
compile session, not just user_bin.

### Tests fail in seemingly random ways across runs

Incremental cache shape mismatch. Wipe `toylangc/target/integration-projects-cache`
and re-run. This is arch §25.3 C8.

### Pass-through corpus fails (byte divergence between vanilla and forked)

Sky's machinery is leaking into pure-Rust compiles. Threats per arch §25.3.5:

1. Side effects in Sky's startup before the marker check (env reads,
   file touches, panic handler installs).
2. The partition filter mutating CGU state even when no items match
   `is_consumer_codegen_target` (every restructure of `CodegenUnit` adds
   a potential silent drop here).
3. A query override running for pure-Rust crates without short-circuiting
   on `is_sky_active(tcx)`.

Audit Sky's `init` / `provide` / `Callbacks::config` for unconditional
behavior.

---

## Invariants you maintain (discipline, not mechanism)

These are NOT enforced by the type system. Code review catches code; test
fences catch most regressions. But several classes of failure remain
detectable only via specific fixtures.

### @SMPLZ: pin Sky-emitted symbols in `@llvm.used`

Any Sky-emitted symbol whose only callers live in OTHER compile units'
machine code MUST be pinned in the LLVM `@llvm.used` global (not the
weaker `@llvm.compiler.used`). Three LLVM passes will otherwise remove
or rewrite the symbol:

- `GlobalDCE` deletes it (no in-module use → dead).
- LTO `internalize` demotes External → Internal (at `lto = "fat"`).
- The linker dead-strips.

Today this applies to every rustc-mangled extern wrapper. Future emission
paths that produce rustc-visible symbols (drop glue, vtable shims, async
state machine `poll` impls, closure `Fn` impls) need the same pin.

**The mechanism**: `toylangc/src/llvm_gen.rs::pin_in_llvm_used` is called
once at the end of `fill_module`'s emission loop, collecting names from
the `fn_items` set. Future emissions should route their symbols through
`fn_items` so the end-of-loop pin picks them up, OR call `pin_in_llvm_used`
themselves.

**The test fence**: `tests/integration_projects/opt_level_3_fat_lto_smoke/`
catches the canonical SMPLZ failure (External-to-Internal demotion at
`lto = "fat"`).

### @GCMLZ: don't lock a consumer-state mutex inside a rustc query provider

If Sky uses a global mutex for any mutable consumer state, and the mutex
is held during codegen, and a query provider tries to lock it, Sky
deadlocks. The failure is silent — 0% CPU, no diagnostic.

The discipline:

- Predicates (`is_consumer_type`, `is_consumer_fn`) are lock-free reads of
  `SkyUniverse` (RwLock, populated during populate-only phases before
  codegen starts).
- In-query callbacks like `symbol_name` are stateless functions of
  `(tcx, instance)`.
- Codegen contribution via `fill_extra_modules` happens via patch 4's
  hook (long-running mode is OK because it doesn't recurse into queries).

**If you add a new query provider**, it must read from `SkyUniverse`,
NOT from a `Mutex`-protected state.

### @NNGZ: non-generic is the degenerate case of generic

Never branch on `type_params.is_empty()`. A non-generic item is N=0 of
the same path that handles N≥1.

The arch-fence CI test (`tests/architecture_fence.rs`) greps the
frontend source for `type_params.is_empty()` patterns and asserts each
is annotated `arch-fence-allow: <reason>`. Unannotated occurrences
fail the test.

**Forced exceptions** (must be annotated):
1. Rust syntax constraints — `impl<>` is a parse error, so stub_gen
   skips `<>` decoration for N=0.
2. External rustc behavior — when a query's contract differs for
   N=0 vs N≥1 in a way Sky can't influence.
3. Approach A invariants — `debug_assert!(!instance.args.has_param())`
   is "substituted vs unsubstituted," not "N=0 vs N≥1."

### Pass-through invariant

Sky's `rustc` binary, when compiling a crate without `__SKY_STUBS_MARKER`,
must produce byte-identical output to vanilla nightly rustc. This is
arch §25.3.5, "the hardest invariant in the document."

The corpus that verifies this is `tests/passthrough_corpus.rs`. It's
currently small (4 fixtures, 3 broken pre-existing). Expand it when
you have time — Agent 8's list of missing coverage (build.rs interactions,
proc-macro derives, common stdlib types, mixed cargo workspaces) is
all real.

### Other invariants worth knowing

- **@SyMINCZ**: `symbol_name` query is a pure read; codegen is driven by
  `ReifyFnPointer` casts in per_instance_mir bodies.
- **@DPSFDOZ**: `tcx.def_path_str()` ICEs outside diagnostics. Use
  `tcx.def_path(...)` walks or `tcx.crate_name(...)`.
- **@ELASZ**: lifetime slots in GenericArgs are filled with `re_erased`,
  not `'static`.
- **@ACRTFDZ**: LLVM extern declarations use rustc's ABI-coerced types.
- **@TCHAPZ**: `#[track_caller]` Rust fns need a hidden Location arg
  appended at the call site.
- **@RTMEIZ**: every Rust type Sky source uses must be explicitly imported.
- **@UTAIRZ**: unsized types appear only as the inner of a reference.
- **@MBMRVZ**: `fn main()`'s tail expression is void.
- **@IVTDBTZ**: inherent vs trait dispatch is type-kind based.
- **@TVIMDGAZ**: trait method Instances are built from the trait def's
  method DefId + `[Self, ...]`.
- **@ATAFLBZ**: `tcx.all_impls(...)` walks need an `is_from_sky_stubs`
  filter on the self type.

Most of these are documented inline in the code at the sites they apply.
Arch §26 enumerates them with cross-references.

---

## When to extend the fork vs the facade

A new requirement might call for one or the other. Heuristic:

| If the requirement is... | Add to... |
|---|---|
| A new way Sky needs to override rustc behavior at a sanctioned extension point | Facade (new query override) |
| A new way Sky's frontend needs to participate in rustc's pipeline | Facade (new callback, new lib.rs function) |
| Something that requires rustc to do A else B based on Sky-specific state | First try a query override. If no extension point exists, consider a fork patch. |
| Something that requires rustc to add a new query | Fork patch |
| Something that requires rustc to call into Sky code at a new point | Fork patch (like patch 4) |

**Fork patches are forever-ish.** Each patch you add costs ~1-2 days per
nightly bump in the worst case (more if it's in a churn-prone area like
the codegen coordinator). Be sure the alternative isn't viable via query
override first.

**Before adding a fork patch**:

1. Check if `Config::override_queries` can do it.
2. Check if a callback at `after_expansion`, `after_analysis`, or via
   `CodegenBackend`'s wrap-and-delegate can do it.
3. Check if a different layer of Sky's machinery (frontend, codegen,
   testing) can do it.
4. Only if none of the above: add a fork patch.

The bar is high because Sky's stated posture (arch §1.5) is "prefer fork
patches over fragile mechanism plumbing" — meaning when you DO add a
patch, it should be small, structurally local, and use default-no-op
providers that preserve the pass-through invariant.

**Patch 5 retirement teaches a specific lesson**: patches that solve a
problem created by a different design decision should be retired by
reversing the design decision. Patch 5 papered over a hazard that
Option 4 introduced; restoring the partition filter removed the hazard
and let the patch retire. If you ever consider adding a patch to fix
a problem caused by another part of the architecture, look for the
upstream cause first.

---

## When to ask before pivoting

CLAUDE.md says "if we decided on a certain course of action, then you
discover that it won't work, or that it will take too much additional
time/budget, **DO** ask the user first before changing directions."

This applies especially during bumps. If you hit:

- An unexpected rustc-internal restructure that requires re-architecting
  a query override.
- Pre-existing source-tree damage in `~/rust/`.
- A regression you can't immediately explain.
- A test that fails in a way that suggests an invariant has shifted
  rather than just drifted.

Stop, document what you've observed, and ask. Don't pivot or paper over.

---

## Test coverage gaps to be aware of

The current test surface is reasonable but has gaps. The next maintainer
should know:

1. **Stdlib-type coverage is mostly Vec.** We have a few Option/Result
   fixtures (`option_unwrap_basic`, `result_unwrap_basic`) but no
   Box-of-Sky-type, no Result<SkyType, E>, no HashMap<K, SkyType>.

2. **No build.rs / proc-macro fixtures.** Real-world Rust dep graphs
   almost always include build.rs (sys-crate wrappers) or proc-macros
   (serde_derive). We've never validated Sky's machinery survives these
   intact. This is tier-1 priority for any real Sky deployment.

3. **The symbol-uniqueness fence (B17) catches one class of UB.**
   It doesn't catch lifetime-discriminating trait dispatch UB,
   cross-language Drop unsoundness, Pin violations from migratory
   futures, or other UB classes. Many failure modes have no fence.

4. **`-Os` and `-Oz` matrix cells use correctness-only assertions.**
   The LLVM size-conscious inliner heuristic is unpredictable; we
   verify only that the binary runs and produces the expected output.

5. **The passthrough corpus is small.** 4 fixtures (hello, closures_iters,
   generics_heavy, trait_dispatch) verifying vanilla-vs-fork byte-comparable
   runtime behavior. Adding more shapes (cfg-gated items, complex
   `#[derive]` patterns, build.rs-using crates) would meaningfully
   expand confidence in the pass-through invariant.

---

## Where the load-bearing discipline lives

If you add a new emission path (drop glue, vtable shims, async poll impls,
closure Fn impls, comptime types), check that it:

1. **Pins symbols in `@llvm.used`** (SMPLZ).
2. **Doesn't lock consumer-state from a query provider** (GCMLZ).
3. **Doesn't branch on `type_params.is_empty()`** (NNGZ).
4. **Handles N=0 (non-generic) through the same path as N≥1** (NNGZ).
5. **Substitutes args before emission** (Approach A invariant —
   `debug_assert!(!instance.args.has_param())` in per_instance.rs).
6. **Goes through `is_consumer_codegen_target` for any rustc-side
   filtering** (so pass-through stays intact).
7. **Has a fixture** in the integration suite that exercises the
   shape, ideally at multiple opt-levels + LTO modes.

The arch fence CI test catches NNGZ violations mechanically. SMPLZ /
GCMLZ / pass-through are caught only by code review + the relevant
fixtures running.

---

## Traps from prior sessions

These specific gotchas have bitten previous engineers. They're documented
in the arch doc but worth surfacing here too.

### The `__SKY_STUBS_MARKER` parentage check

A naive marker check that just looks for the symbol name fails when a
Sky lib glob-re-exports through another Sky lib. The downstream crate
inherits the marker visibility and gets incorrectly identified as a
Sky stub rlib. Result: the partition filter strips the downstream's
own items (including `fn main`), link fails with undefined `_main`.

Fix: verify `def_id.krate == crate_num` in the marker check. The check
is in `is_from_lang_stubs` (`rustc-lang-facade/src/lib.rs`).

### Wrapper-mode bypass

`cargo build` directly bypasses `RUSTC_WORKSPACE_WRAPPER`, so Sky's
patch 4 hook never installs, Sky's bitcode contribution is empty, and
the binary falls back to the stub rlib's `unreachable!()` bodies. The
build succeeds but the binary panics at runtime.

Always invoke through `toylangc build` for integration tests of LTO
behavior. Or set `RUSTC_WORKSPACE_WRAPPER` explicitly.

### Profile overrides in workspace members are silently ignored

Cargo only honors `[profile.dev]` etc. in the workspace ROOT Cargo.toml.
If you put profile overrides in a member package's manifest, cargo
silently drops them. Symptom: `lto = "thin"` in member toml has no
effect on the generated rustc command.

Reference: `build.rs::write_workspace_toml` in toylangc.

### LLVM 21 BitcodeWriter bug

LLVM 21's bitcode writer drops `FUNCTION` records under specific ABI-
coerced extern signatures. Hit by toylang's Phase 4.5 pre-Approach-B
design. Closed: Approach B (patch 4 rev 2) doesn't serialize bitcode
anymore — Sky emits IR directly into rustc-owned modules. The bug
remains real upstream but is unreachable in our pipeline.

### The `default_collect_and_partition()` direct-call pattern

When the partition override needs the upstream's unfiltered output, it
calls the saved upstream provider directly:

```rust
let upstream = crate::default_collect_and_partition();
let partitions = upstream(tcx, ());
```

Do NOT call `tcx.collect_and_partition_mono_items(())` for this — that
goes through the query cache, which has memoized your override's output.
You'd get the filtered result back, not the unfiltered one.

The saved-provider pattern is in `rustc-lang-facade/src/lib.rs`
(`DEFAULT_COLLECT_AND_PARTITION` OnceLock + `default_collect_and_partition()`
accessor).

### Test totals drift

Various places in the code and docs reference test counts ("210 tests",
"333 tests", etc.). These are point-in-time snapshots. The actual count
grows as new fixtures land. Don't rely on absolute numbers; rely on
"this run produced the same count as the previous run."

### Source-position-in-debuginfo for opaque types

Per arch §10.4.5, rustc's debuginfo walker assumes `source_fields.len() ==
layout.fields.count()`. Sky's old opaque-with-zero-fields shape violated
this and ICE'd when Sky ADTs appeared inside Rust generics. The shipped
wrapper-as-field shape (§10.6) preserves the invariant structurally:
`pub struct Foo(SkyOpaqueType<HASH>)` has one source field, one layout
field, no mismatch.

If you change the opaque-stub shape, verify the walker invariant holds.

---

## Decision history that won't kill you to know

A few one-line summaries of architectural decisions that are easy to
re-litigate if you don't know they've been settled:

- **Approach A (Instance-keyed) over Approach B (DefId-keyed)** —
  forced by Sky's comptime ambitions; rustc can't substitute Sky-typed
  comptime args. See arch §3.1.

- **Option C (full CodegenBackend plugin) over partitioner-override-
  and-mutate** — eliminates B2 by owning emission outright. See
  arch §5.1.

- **Approach B (rustc-owns-lends) for patch 4** over Approach A
  (Sky-owns-transfers) — avoids LLVM context migration and target-attr
  skew. See arch §F.15.

- **Partition filter over Option 4's codegen_fn_attrs** — Option 4 was
  smaller in pure LOC but created a CGU-placement hazard that required
  patch 5 to paper over. Retiring both together (2026-06-22) was net
  simpler. See arch §F.14.1, §F.17.

- **Source-shipped Sky libs (no precompiled .o)** — Sky version
  independence + cross-platform + comptime-driven layouts. See arch
  §5.5, §8.6, §10.8.

- **Per-library stub rlibs (not one combined)** — cargo incremental
  needs per-library caching. See arch §6.1.

- **Sidecar-adjacent, not embedded-in-rlib** — inspection + clean
  missing-file failure mode. See arch §7.1.

- **No `git checkout` to revert in production work** — discipline from
  CLAUDE.md. Use `git diff` + manual apply, or read content and Write.
  (Allowance for emergency: file restoration from backup or via explicit
  user OK for `git restore`-style operations.)

---

## Useful commands

Build + test toylangc:
```
LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
    cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml \
    --test integration_projects -- --test-threads=1
```

Verify a single fixture:
```
LLVM_SYS_211_PREFIX=... cargo +rustc-fork test --manifest-path .../Cargo.toml \
    --test integration_projects -- test_inline_case5_no_lto
```

Run toylangc directly on a fixture (for save-temps debugging):
```
cd /Users/verdagon/erw/toylangc/tests/integration_projects/inlining/case5_no_lto
rm -rf .toylang-build
LLVM_SYS_211_PREFIX=... DYLD_FALLBACK_LIBRARY_PATH=/Users/verdagon/.rustup/toolchains/rustc-fork/lib \
    RUSTFLAGS="-C save-temps" /Users/verdagon/erw/target/debug/toylangc build
```

Inspect a Sky binary's symbol table:
```
/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm/bin/llvm-objdump -t \
    /path/to/binary | grep <symbol-fragment>
```

Disassemble main:
```
/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm/bin/llvm-objdump -d \
    --disassemble-symbols=__<mangled_main_name> /path/to/binary
```

Wipe caches for a cold run:
```
rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache
find /Users/verdagon/erw/toylangc/tests/integration_projects \
    -name ".toylang-build" -type d -exec rm -rf {} +
```

Restore rust source tree damage:
```
cd /Users/verdagon/rust
git ls-files --deleted > /tmp/rust-deleted-files.txt
cat /tmp/rust-deleted-files.txt | xargs git restore
```

---

## Closing notes

You are the next person who will know this codebase deeply. The arch doc
is the authoritative reference for "what" and "why." This guide is for
"how to work with it day-to-day."

The architecture is reasonably solid but it's load-bearing on **discipline
more than mechanism**. Code review catches code; tests catch most
regressions; but several classes of silent UB remain undetectable except
by accidentally writing a fixture that exercises them. Internalize the
invariants. Be paranoid about pass-through. When you hit a wall, stop
and ask before pivoting.

Good luck.

— from the engineer who shipped the 2026-06-22 Option 4 + patch 5
retirement and wrote up the lessons
