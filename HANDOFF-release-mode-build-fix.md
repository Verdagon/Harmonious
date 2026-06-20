# Handoff: Release-Mode Build Fix (`-O>=2` link errors)

**Status:** In progress. Partial fix shipped (commit `82a9c4d`). The deeper architectural fix that unblocks release-mode builds is the next task.

**Who this is for:** The next engineer picking up Sky/toylang work. Assumes you've read [`rust-interop-architecture.md`](rust-interop-architecture.md) and [`HANDOFF-TL.md`](HANDOFF-TL.md) at least once. If not, read those first — this doc assumes you understand `per_instance_mir`, the seven-case taxonomy, patch 4 / `fill_extra_modules`, the stub rlib model, and §F.13's cascade-fires-at-stub-rlib finding.

---

## TL;DR

The architecture works at debug builds. At release builds (`opt-level >= 2`), fixtures involving cross-language generic trait dispatch (like `case_generic_impl_block`) **fail to link** with undefined-symbol errors. Toylang's test suite passes because it runs at debug. **Sky users running `cargo build --release` on real workloads would hit this immediately.**

Root cause is a chain of three interacting issues:

1. `Options::share_generics()` defaults to false at `-O2/-O3` (in `rustc_session/src/config.rs:1443`).
2. Without share_generics, `Instance::upstream_monomorphization` early-returns `None` (in `rustc_middle/src/ty/instance.rs:211-217`), bypassing Sky's `synthesize_upstream_monomorphizations` augmentation.
3. Sky's emission of trait-impl methods (e.g. `<Wrapper<i32> as Clone>::clone`) doesn't reach the bin's final link at -O3 — distinct from the disambig issue, surfaced when we tried to fix it.

Partial fix shipped: Option A (added `#[inline(never)]` to every Sky export's stub fn). This escapes the gate for **Sky-owned** items and mitigates a separate MIR-inliner-leak concern. It does NOT cover Rust generics that take Sky types as args (the `some_rust_lib::duplicate<Wrapper<i32>>` case), and exposed a second issue around clone-emission-at-bin that the partial fix didn't address.

Your job: complete the release-mode fix. Probably 1-2 weeks of focused work.

---

## Background context you need

### 1. How Sky integrates with rustc, in one paragraph

Sky's compiler is a fork of rustc with 4 patches: three for the `per_instance_mir` query, one for the `fill_extra_modules` hook (patch 4 rev 2 / "Approach B"). Sky's facade (`rustc-lang-facade/`) installs query overrides via `Config::override_queries` and contributes Sky-emitted LLVM bodies via patch 4's allocator callback. Sky-defined items appear to rustc as opaque stubs (`pub fn foo() { unreachable!() }`) in skyc-generated stub rlibs, with `__SKY_STUBS_MARKER` activating Sky's machinery per-crate. The toylang prototype at `toylangc/` exercises this end-to-end with a small toy language.

### 2. The cascade timing (§F.13 — load-bearing)

This is THE most important thing to internalize. Read [`rust-interop-architecture.md` §F.13](rust-interop-architecture.md) carefully before doing anything.

Summary: at the **bin compile**, rustc's mono collector at `rustc_monomorphize::collector::collect_used_items` gates on `is_reachable_non_generic(def_id) || instance.upstream_monomorphization(tcx).is_some()` for non-local symbols. For Sky's `__toylang_main` (non-generic, upstream — lives in the bin's own stub rlib from the bin compile's perspective), this gate returns `true` and the collector **never calls `per_instance_mir` for `__toylang_main` at the bin compile**. So the cascade — and discovery of `duplicate<Wrapper<i32>>`, `<Wrapper<i32> as Clone>::clone`, etc. — fires **only at the stub rlib compile**.

Two consequences:

- The stub rlib compile is where Rust generic intermediaries (like `duplicate<Wrapper<i32>>`) get queued and emitted. Inside their bodies, references to consumer trait methods bake in a specific instantiating-crate disambig at THAT compile.
- Cross-compile coordination is needed for the bin to emit Sky's bodies with the matching disambig so the linker resolves correctly. This is what `discovered_trait_impl_instances` capture-ship-replay (§8.9.5) and `synthesize_upstream_monomorphizations` (`rustc-lang-facade/src/queries/upstream_monomorphization.rs`) do.

### 3. The v0 mangler disambig flow

When the stub rlib compile emits `duplicate<Wrapper<i32>>`, the v0 mangler picks an "instantiating-crate disambig" for both the symbol itself AND for inner references (like the call to clone). That disambig comes from `Instance::upstream_monomorphization` consultation. If `upstream_monomorphizations_for` has an entry, the mangler uses that crate's disambig. Otherwise it falls back to `LOCAL_CRATE`.

At debug, `share_generics()` is `true`, so the mangler consults the map. Sky's `synthesize_upstream_monomorphizations` override augments the map with `__lang_stubs` as the instantiating crate for consumer trait-impl methods. Both the stub rlib and bin compiles see the augmentation, pick `__lang_stubs` disambig, symbols match.

At release (-O>=2), `share_generics()` is `false`. `Instance::upstream_monomorphization` early-returns `None` UNLESS the Instance is `#[inline(never)]`. The mangler falls back to `LOCAL_CRATE` — which is **different** at the stub rlib compile (`__lang_stubs`) vs the bin compile (the bin's crate). Symbols mismatch.

### 4. The single-symbol architecture (Phase 4.5 Path B, §6.2, §F.2)

Sky's bitcode emits each rustc-visible body under the **rustc-mangled name** rustc would have given the stub fn. Single symbol. The `#![no_builtins]` on stub rlibs excludes their bitcode from the LTO IR linker pool, leaving Sky's body as the sole definition for cross-language inlining.

This is why disambig coordination matters so much: the *name* matters because Sky needs to emit under the right mangled name. If the mangler picks the wrong disambig, Sky emits a body that no caller references.

---

## The bug, precisely

### Reproducer

Create a fixture `release_mode_smoke` by cloning `case_generic_impl_block`:

```
cp -R toylangc/tests/integration_projects/case_generic_impl_block toylangc/tests/integration_projects/release_mode_smoke
rm -rf toylangc/tests/integration_projects/release_mode_smoke/.toylang-build
```

Edit its `toylang.toml` to add `opt-level = "3"`:

```toml
[project]
name = "release_mode_smoke"
source = "main.toylang"
opt-level = "3"

[rust-dependencies]
test_helpers = { path = "../test_helpers" }
some_rust_lib = { path = "../some_rust_lib" }
```

Add a `#[test] fn test_release_mode_smoke() { run_integration_project("release_mode_smoke"); }` at the end of `toylangc/tests/integration_projects.rs` and run it:

```
LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
  cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml \
  --test integration_projects test_release_mode_smoke -- --nocapture
```

### The error

You will see one of two link errors depending on whether you've also applied the partial Option B fix in the driver:

**Without Option B** (current state after commit `82a9c4d`):

```
Undefined symbols for architecture arm64:
  "__RINvCsHq...13some_rust_lib9duplicateINtCskpHi..._12___lang_stubs7WrapperlEECs9NQO..._18release_mode_smoke"
  referenced from __RNvCskpHi..._12___lang_stubs14___toylang_main
```

Translation: `duplicate<Wrapper<i32>>` is undefined with bin's disambig (`release_mode_smoke`). It exists in the stub rlib with `__lang_stubs` disambig. The bin's mangler picked the wrong disambig because share_generics is off.

**With Option B applied** (force `share_generics = true` in the facade's `config()` hook):

```
Undefined symbols for architecture arm64:
  "__RNvXs_CskpHi..._12___lang_stubsINtB4_7WrapperlENtNtCs5cZ..._4core5clone5Clone5cloneB4_"
  referenced from __RIN...duplicateINt...Wrapper... in lib__lang_stubs-XXX.rlib
```

Translation: `<Wrapper<i32> as Clone>::clone` is undefined. The stub rlib's emission of `duplicate<Wrapper<i32>>` correctly references clone with `__lang_stubs` disambig, but Sky's emission of clone isn't reaching the final binary at -O3.

### Why these symbols are missing

- The first error is the share_generics gate firing for `duplicate` (Sky can't mark Rust source).
- The second error is a different problem: Sky's patch 4 emission of `<Wrapper<i32> as Clone>::clone` isn't producing the symbol in the binary. Could be that the cascade doesn't queue clone at -O3, or that Sky's bitcode contribution doesn't survive rustc's release-mode pipeline, or some other interaction.

The second is the deeper unknown. You'll need to investigate it.

---

## What's been tried

Read the conversation transcript at the bottom of `generics-law-convo-3.md` (or whichever conversation log was current) for full context. Summary of explored options:

### Option A — `#[inline(never)]` on all Sky exports — SHIPPED (commit `82a9c4d`)

**What it does:** `toylangc/src/stub_gen.rs` now emits `#[inline(never)]` on every Sky export stub (accessors, top-level wrapper fns, trait-impl methods). Previously only Phase-6 wrappers (`__toylang_option_unwrap`, etc.) had it.

**Why this helps:** `Instance::upstream_monomorphization` (`rustc_middle/src/ty/instance.rs:211-217`) has an escape clause: if the Instance is `#[inline(never)]`, the share_generics gate doesn't fire. So for Sky-OWNED items (clone, Sky generic exports), the mangler consults `synthesize_upstream_monomorphizations` even at -O3.

**Why it's only partial:** Sky can't add attributes to Rust source. The `duplicate` Rust generic from `some_rust_lib` still hits the gate at -O3.

**Bonus benefit:** Also mitigates Agent 1's MIR inliner leak concern (cross-crate inlining of stub `unreachable!()` bodies into Rust callers).

### Option B — Force `share_generics = true` in driver config — TESTED BUT NOT SHIPPED

**What it does:** Add `config.opts.unstable_opts.share_generics = Some(true);` to `LangDriver::config` in `rustc-lang-facade/src/driver.rs`.

**Why this helps:** Bypasses the share_generics gate entirely. The mangler always consults `upstream_monomorphizations`. Sky's augmentation works. Both stub rlib and bin pick the same disambig.

**Why we didn't ship it:** It surfaced a *different* failure mode — Sky's clone emission at the bin doesn't reach the link. We don't know why yet. This second failure must be investigated and resolved before Option B is useful.

**Trade-off if shipped:** Affects every crate compiled through Sky's `rustc` binary, including pure-Rust pass-through crates. At -O2/-O3 they'd behave as if `-Z share-generics=yes` were set. The pass-through corpus (`tests/passthrough_corpus.rs`) tests runtime behavior, not byte equality, so this wouldn't fail any current tests — but it does weaken the arch doc §4.4 byte-identity invariant further.

### Option C — Rustc patch to bypass the gate when Sky has an entry — NOT TRIED

A fifth fork patch that modifies `Instance::upstream_monomorphization` to also escape the gate when `tcx.upstream_monomorphizations_for(def_id)` has an entry. Sky's augmentation would always be consulted regardless of share_generics or `#[inline(never)]`.

**Why this is appealing:** Cleanest semantic. Doesn't affect pure-Rust crates. Sky-side knowledge ("this Instance has a canonical upstream owner") drives the decision.

**Why we didn't try it:** Real fork patch, ongoing maintenance cost. Defer unless Options A+B don't pan out.

### Option D — Force `share_generics = true` only for Sky-marked crates — INVESTIGATED, BLOCKED

The marker check (`__SKY_STUBS_MARKER`) requires `tcx`, which is only available in `after_expansion`. By then `Session::opts` is mostly frozen. You can't conditionally override `share_generics` based on the marker.

We considered checking `Config::opts.crate_name` for a `lang_stubs_*` pattern at `config()` time, but that's a fragile heuristic.

---

## Current state of the codebase

### What's committed

```
82a9c4d  stub_gen: emit #[inline(never)] on every Sky export   ← partial fix (Option A)
30899d1  tests: explicit fence for the glob-reexport marker trap
d34215d  tests: decoupling fence + minimal pass-through corpus
4854a5a  facade: decouple from Inkwell + ModuleLlvm via LlvmModuleFactory
575eb86  approach b phase 5: docs reflect shipped Approach B; risks closed
8257a35  approach b phase 4: toylang codegen consumes borrowed ModuleLlvm
29bbe59  approach b phase 3: facade migrates to fill-modules callback
89166a5e693  patch 4 (rev 2): ExtraBackendMethods::fill_extra_modules + allocator  (in /Users/verdagon/rust/)
941cb47  approach b phase 1: vendor inkwell + add Module::new_borrowed
ce97773  arch doc: fold patch-4 bytes-as-interface critique + Approach B endgame
```

### Test state

```
267/267 tests passing at debug.
test_lto_smoke passes (verifies cross-language ThinLTO inlining at opt-level=3 + lto=thin).
NO test exercises -O3 without LTO. This is the gap that would catch the bug.
```

### Working tree

Clean modulo a few untracked notes (`generics-law-convo-1.md`, `-2.md`, `-3.md`) which capture the long conversation that produced this handoff. Read them in order if you want full context.

---

## Recommended next steps

### Step 1: Reproduce the second failure mode

Apply Option B locally (don't commit yet) and run the release_mode_smoke fixture. You'll see the "clone undefined" error from the "With Option B applied" example above. **This is your starting point.**

Specifically: edit `rustc-lang-facade/src/driver.rs::LangDriver::config` to add:

```rust
config.opts.unstable_opts.share_generics = Some(true);
```

Then rebuild toylangc + run the fixture.

### Step 2: Investigate where Sky's clone emission goes at -O3

This is the hard part. Some hypotheses to test:

**Hypothesis 1: The cascade doesn't queue clone at -O3.**

At the stub rlib compile, Sky's `per_instance_mir` provider returns `__toylang_main`'s body with ReifyFnPointer casts for `duplicate<Wrapper<i32>>`. Rustc walks duplicate's body, sees `x.clone()`, resolves to `<Wrapper<i32> as Clone>::clone`, queries `per_instance_mir`. Sky's provider returns clone's body. Cascade walks it. Done.

At -O3, is anything in this chain different? Possibly:
- MIR optimization might transform `x.clone()` into something that resolves differently.
- The collector might short-circuit certain walks at higher opt levels (unlikely, but check).

**How to test:** Add eprintln logging to `rustc-lang-facade/src/queries/per_instance.rs` (Sky's `per_instance_mir` provider) to see what Instances are queried at -O3 vs debug. Compare lists.

**Hypothesis 2: Sky's clone IS queued and emitted, but the binary loses it.**

The stub rlib compile cascades, queues clone, Sky's patch 4 emits clone's body. Sky's bitcode for clone enters the stub rlib's CGU stream via `fill_extra_modules`. Under `#![no_builtins]`, the stub rlib's bitcode is excluded from LTO. **But under no-LTO, the bitcode-to-object conversion still happens.** So clone should land in the stub rlib's `.o`.

Check: does `/Users/verdagon/erw/toylangc/target/integration-projects-cache/debug/deps/lib__lang_stubs-XXX.rlib` (or `release/deps/...` if at -O3) contain a defined symbol for `<Wrapper<i32> as Clone>::clone`? Extract the rlib with `ar x` and run `llvm-objdump -t` on the contained `.o` files.

If Sky's clone is in the rlib's `.o`: linker should resolve it. Either it's not being included in the link, or its symbol name doesn't match what the bin's references expect (back to the disambig issue).

If Sky's clone is NOT in the rlib's `.o`: patch 4's emission isn't completing for this Instance at -O3.

**Hypothesis 3: Sky's symbol_name override produces a different name at -O3 vs debug.**

Sky's `symbol_name` query calls `default_symbol_name(tcx, instance).name` to get the rustc-default mangling. The rustc-default mangling depends on `Instance::upstream_monomorphization`. If at -O3 the mangler picks a different disambig, Sky's emission name differs from what callers reference.

**How to test:** Add logging to `rustc-lang-facade/src/queries/symbol_name.rs` to print every symbol name Sky returns. Compare debug vs -O3 runs. Look for items where Sky's name doesn't match what `llvm-objdump -t` shows as referenced from `duplicate`'s body.

### Step 3: Once the root cause is identified

The fix shape depends on what hypothesis 1/2/3 turns out to be.

- If H1 (cascade doesn't queue clone at -O3): Sky needs to force collection somehow, or the cascade walk in Sky's `per_instance_mir` provider needs to be more robust to MIR-optimization-transformed call sites.
- If H2 (Sky's clone in rlib but not in binary): linker / partition / linkage issue. Maybe Sky's emission has the wrong visibility/linkage at -O3.
- If H3 (mangler picks different disambig): Sky's `synthesize_upstream_monomorphizations` augmentation needs to be richer, or Sky's `symbol_name` override needs to consult the augmentation directly rather than relying on rustc's default mangling.

### Step 4: Add the regression fence

Once the fix lands, the `release_mode_smoke` fixture should pass. Add it as a permanent CI test. The exact pattern is described above (set `opt-level = "3"` in `toylang.toml`, add `#[test] fn test_release_mode_smoke()`).

Optionally extend the fence to cover other opt levels:
- `opt_level_2_smoke` — same fixture at `opt-level = "2"`.
- `opt_level_3_no_lto_smoke` — explicitly `opt-level = "3"` with `lto = "off"`.

### Step 5: Update the architecture doc

Add findings to `rust-interop-architecture.md`:
- §F.13 / §8.9.5 region: note the share_generics gate interaction.
- §25.2 risks: B16 or similar for the release-mode build issue, now-CLOSED.
- §6.6.5 / §26.x: codify the `#[inline(never)]` discipline on all stubs (NEW arcanum — call it INVZ or similar for "INliNever Z").

---

## Investigation context from the agent audit

Before this work started, I dispatched 7 parallel agents to audit the proposed `#[codegen_backend_provides_body]` attribute (a different upstream proposal we ultimately didn't pursue). Several of those audits surfaced bugs in the **current** architecture, including this one. The full agent reports are in the conversation log; the relevant ones for THIS handoff:

### Agent 2: share_generics gate

This agent identified the gate behavior precisely:
- `Options::share_generics()` at `rustc_session/src/config.rs:1443`.
- `Instance::upstream_monomorphization` at `rustc_middle/src/ty/instance.rs:201-217`.
- The escape clause requires `#[inline(never)]` OR `#[track_caller]`.
- At -O2/-O3, share_generics is false by default; the gate fires; Sky's augmentation isn't consulted.

Quote: "share_generics defaults FALSE at -O2/-O3 — at release builds, the natural mechanism silently bypasses."

This is the root cause of the disambig issue.

### Agent 1: MIR inliner cross-crate inlinable

This agent identified that:
- `rustc_metadata/src/rmeta/encoder.rs:1828` exports `optimized_mir` into rmeta when `cross_crate_inlinable` is true.
- For generics, `cross_crate_inlinable` defaults true.
- Downstream Rust crates can inline the stub's `unreachable!()` body into Rust callers.

Option A (the `#[inline(never)]` we shipped) defends against both this AND the share_generics gate at once for Sky-owned items.

### Agent 4: trait dispatch + vtables + fn pointers

This agent noted: "MIR inliner is THE sharp edge: must imply `#[rustc_no_mir_inline]` + `#[inline(never)]`."

We shipped `#[inline(never)]`. The rustc_no_mir_inline part is a stronger upstream-internal attribute that would be belt-and-suspenders. Probably not needed.

---

## Files you'll touch

| File | Purpose | Why |
|---|---|---|
| `rustc-lang-facade/src/driver.rs` | `LangDriver::config` — install Option B's force-on share_generics | Coordinates disambig across compiles |
| `rustc-lang-facade/src/queries/per_instance.rs` | Sky's `per_instance_mir` provider | Debug whether clone is queued at -O3 |
| `rustc-lang-facade/src/queries/symbol_name.rs` | Sky's `symbol_name` override | Debug whether Sky's emission name matches references |
| `rustc-lang-facade/src/queries/upstream_monomorphization.rs` | `synthesize_upstream_monomorphizations` impl | Possible site for richer augmentation |
| `toylangc/src/llvm_gen.rs::fill_module` | Sky's emission path | Check if clone reaches Sky's emission queue at -O3 |
| `toylangc/src/toylang/callbacks_impl.rs::consumer_fill_modules` | Patch 4 hook entry | Check if hook fires per Instance at -O3 |
| `toylangc/tests/integration_projects/release_mode_smoke/` | New fixture for the regression fence | After fix, this becomes a permanent CI test |
| `toylangc/tests/integration_projects.rs` | Test entry registration | Add `test_release_mode_smoke` once fixture passes |
| `rust-interop-architecture.md` | Doc updates | §F.13, §8.9.5, §6.6.5, §25.2 |

---

## Gotchas / things to watch out for

### The `RUSTC_WORKSPACE_WRAPPER` discipline (§F.11)

Sky's machinery only activates when rustc is invoked **through** the toylangc/skyc wrapper. If you run `cargo build` directly bypassing the wrapper, Sky's `config()` callback doesn't fire — no overrides, no per_instance_mir, no patch 4. The build "succeeds" but the binary is wrong (calls `unreachable!()`).

**During debugging, always go through the proper toylangc harness.** Either via `cargo test --test integration_projects` or via the `run_integration_project` helper. Bypassing the wrapper will give misleading results.

### The `git checkout` prohibition

`CLAUDE.md` forbids `git checkout` for file reverts. Use `git show HEAD:path > path` instead. This bit us during the investigation when I tried to revert `integration_projects.rs` after an awk-script mishap.

### `cargo +rustc-fork` toolchain quirks

The rustc-fork toolchain is linked into rustup but doesn't have its own `cargo` binary. You'll see `info: cargo is unavailable for the active toolchain` warnings — those are fine. The actual fork is the rustc binary at `/Users/verdagon/.rustup/toolchains/rustc-fork/bin/rustc`, which loads `librustc_driver` from `/Users/verdagon/rust/build/host/stage2/lib/`.

If you need to rebuild the fork (after editing files under `/Users/verdagon/rust/compiler/`), follow `docs/historical/rebuilding-rustc-fork.md`. Plan for 8-12 minutes per rebuild.

### Test cache poisoning

`cargo test --workspace` produces different results from `cargo test --manifest-path ./toylangc/Cargo.toml` because the integration-projects-cache state differs. **Always wipe `toylangc/target/integration-projects-cache` before drawing conclusions about test results.** The architecture-fence + decoupling-fence + passthrough-corpus + marker-parentage-fence tests will pass even when the integration tests are silently using stale cache state.

### LTO smoke test is NOT a release-mode test

`test_lto_smoke` uses `lto = "thin"` + `opt-level = "3"`. The LTO bitcode-extraction pipeline at the LLVM level handles a lot of the disambig issues by inlining Sky's body directly at link time. **It does not exercise the no-LTO release path.** Don't be misled by its passing into thinking release mode works.

### Sky generic exports vs Rust generics

Throughout this investigation, the distinction between "Sky-owned generic" (defined in Sky source, has a stub fn in the stub rlib) and "Rust-owned generic that takes Sky types" (defined in `some_rust_lib`, takes `Wrapper<T>` as arg) matters constantly. Option A only helps the former because Sky can mark its own stubs but not Rust source.

### The pass-through corpus weakness

`tests/passthrough_corpus.rs` is documented as runtime-behavior-identical, NOT byte-identical (we couldn't achieve byte identity given build provenance differences). If you ship Option B (force share_generics), the byte-identity invariant gets weaker. Document this in `rust-interop-architecture.md` §4.4 / §25.3.5 when you do.

---

## What success looks like

When you're done:

1. The reproducer (`release_mode_smoke` fixture at `opt-level = "3"`) builds and runs correctly, outputting `42\n7\n` (the same as `case_generic_impl_block`).
2. The test suite at debug stays at 267/267 (or more, with the new fence).
3. The `release_mode_smoke` fixture is added as a permanent integration test.
4. Optionally, `opt_level_2_smoke` and `lto_off_smoke` variants are also added as fences.
5. The `rust-interop-architecture.md` doc is updated to reflect the fix and the new invariant ("Sky's machinery must work at -O2/-O3 in addition to debug").

### Verification commands

```bash
# Build
LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
  cargo +rustc-fork build --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --release

# Run reproducer
LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
  cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml \
  --test integration_projects test_release_mode_smoke -- --nocapture

# Inspect emitted symbols (if linking fails, see what disambig was picked)
find /Users/verdagon/erw/toylangc/target/integration-projects-cache/debug -name "release_mode_smoke*.rcgu.o" 2>/dev/null | xargs -I{} /Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm/bin/llvm-objdump -t {} 2>/dev/null

# Inspect Sky's IR emission (debug symbol names)
LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
  RUSTFLAGS="--emit=llvm-ir" cargo +rustc-fork build \
  --manifest-path /Users/verdagon/erw/toylangc/tests/integration_projects/release_mode_smoke/.toylang-build/Cargo.toml \
  --release --target-dir /tmp/sky-ir
# (then inspect /tmp/sky-ir/release/deps/*.ll)
```

### Final test run

```bash
rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache && \
  LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm \
  cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml 2>&1 | \
  grep -E "test result|FAILED"
```

Should show all fences + integration_projects with the new fixture passing.

---

## Long-term context — other items uncovered by the agent audit

The investigation that surfaced this bug also surfaced four others. Three are addressed elsewhere or are low priority; one (DWARF) is a substantial separate effort.

### Done

- **MIR inliner cross-crate inlining leak.** Mitigated by the Option A `#[inline(never)]` ship in `82a9c4d`.

### Not currently manifesting (kept for future hardening)

- **Cargo fingerprint blind to sidecars.** Verified empirically not biting today (toylangc regenerates stub source on every invocation, updating mtimes). Could bite if Sky users use cargo directly. Defensive fix: register sidecar paths in `tcx.sess.parse_sess.file_depinfo`. ~1 day.
- **Rustc incremental cache blind to Sky-side state.** Same — verified not biting today. Defensive fix: disable rustc incremental for Sky-marked crates, OR thread Sky-IR fingerprint through dep graph. ~2-3 days for the conservative option.
- **`deduce_param_attrs` derives wrong attrs from stub.** Verified `noreturn cold` attrs are derived for `__toylang_main` from its `unreachable!()` body. Today masked because user_bin only imports as `declare` without propagating attrs. Defensive fix: override `deduced_param_attrs` query in facade. ~half day.

### Substantial separate effort

- **Sky-side DWARF emission is zero.** Today rustc emits a placeholder DISubprogram from the stub source, which provides minimal backtrace info. For real Sky users, you'll need Inkwell `DIBuilder` integration. **~3-6 weeks** of focused work. Plan separately when production users are imminent.

These are catalogued in the agent investigation conversation. If you want full detail, the agent outputs are preserved in the conversation log.

---

## One more thing — the upstream RFC discussion

During the agent audit, I explored two upstream RFCs:

1. **`#[exclude_body_from_lto]` per-function attribute** — would let the bin's stub rlib collapse into the bin's single crate. Substantial simplification (would retire `discovered_trait_impl_instances` capture-ship-replay for the bin's own discoveries) but doesn't subsume library-side coordination needs.

2. **`#[codegen_backend_provides_body]` attribute** — more general "rustc, body comes from elsewhere" signal. The agent audit revealed this would require ~5 query overrides Sky must add (`cross_crate_inlinable`, `should_encode_mir`, `deduced_param_attrs`, `has_ffi_unwind_calls`, plus a Sky-IR fingerprint dep input) plus attr-check rejection rules. Not the "one small RFC" framing I initially gave it.

**Neither is currently being pursued.** The architecture works today's debug case and the partial Option A fix mitigates real risks. The release-mode fix you're working on is the immediate priority. The upstream RFCs are longer-term simplification, not blockers.

If you discover during your work that the share_generics gate is genuinely intractable Sky-side and a rustc patch is needed, Option C above (patch the gate) is the targeted fix. It's a single small fork patch (probably 5-10 LOC) modifying the early-return condition in `Instance::upstream_monomorphization`.

---

## Final note

The toylang prototype is in a clean state — 267 tests passing, four risks closed, two new fence tests, the facade decoupled from Inkwell. The architecture works for the cases it exercises. Your job is the production-readiness pass for release mode.

Take the time to read `rust-interop-architecture.md` Part 8 (the §F appendix lessons). Especially §F.13 — the cascade-fires-at-stub-rlib finding is the single most important architectural fact in the whole system, and it underlies the bug you're fixing.

Good luck.

— Previous engineer, via Claude Opus 4.7
