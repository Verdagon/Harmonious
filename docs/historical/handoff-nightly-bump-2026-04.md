# Handoff: Bump pinned nightly from `2025-01-15` to `~2026-01-20`

**Task owner:** a junior engineer with solid Rust experience. Prior rustc-internals exposure helpful but not required — you will learn the relevant surfaces by adapting our code to their new shapes, not by reading rustc's implementation cold.

**Branch:** work on `main` directly. Commit in staged sub-phases per §6 so there's always a green checkpoint to roll back to. **Do not squash** until the whole bump is green; the staged commits are load-bearing context for the final PR.

**Risk level:** medium. The likely path is 1.5–2 weeks of signature-drift repair. There is a bounded worst-case (risks.md §B2) where the partitioner's linkage mutation stops surviving — if that fires, **STOP and escalate before doing the fork retreat**. See §10.

**Budget:** expected 1.5–2 weeks. Worst case 3–4 weeks. Don't mix this with feature work; it's a dedicated stream.

**Expected final state:** `rust-toolchain.toml` pinned to `nightly-2026-01-20`, all code sites updated, 210/210 tests passing, one documentation sweep updating the empirical bump cost in `risks.md`, one clean PR.

---

**Landing status (2026-04-20):** Shipped. `rust-toolchain.toml`
flipped, all 7 code sites + 21 doc sites updated, full 210-test
suite green (67 unit + 128 integration + 15 standalone) cold and
warm on the new pin. §B2 (partitioner Outcome A) **held** — no
escalation needed. The empirical bump-cost summary is written up
in `docs/architecture/risks.md` §3 "Nightly-pin strategy" for the
next bump's calibration reference.

Surfaces that drifted (exact breakdown in risks.md §3): §4.1
`RunCompiler` removal, §4.2 `CodegenBackend::codegen_crate`/`link`
signatures, §5.1 MIR construction (Instance, Statement,
BasicBlockData non-exhaustive; PointerCoercion Safety arg;
NullaryOp removal), §5.2 Reg/RegKind privacy, §5.3
`MonoItemPartitions` struct + `Linkage` relocation, plus one
surface not in the original inventory: `rustc_middle::util::Providers`
restructured into `queries`/`extern_queries`/`hooks` sub-structs
(now the largest single repair). Layout-side: AbiAlign rename,
`uninhabited` field added, Hash64 type, `memory_index` →
`in_memory_order` inverse-permutation rename. Consumer (toylangc):
mirror of facade drift + `catch_with_exit_code` closure returns
`()`, `GenericArg::unpack` → `.kind`, `TargetDataLayout::pointer_size`/
`pointer_align` field → method.

Total empirical cost: under one workday of focused drift repair
with clean compiler-error guidance. Handoff's 1.5–2 week budget
built in B2-fires slack that went unused.

---

## 1. Context

### Why this is being done

The current pin is `nightly-2025-01-15` — 15 months old. The project is in maintenance mode (HANDOFF-TL.md confirms no active roadmap), which makes this a pure hygiene task: no competing feature pressure. The alternative is waiting longer, but drift compounds — a 30-month gap is harder than two 15-month gaps. Per `docs/architecture/risks.md` §3 B3, MIR construction drift is "~100% per 6-month bump"; we are ≈2.5 bumps overdue.

Per HANDOFF-TL.md §5 ("Don't chase latest nightly. Bump to a ~3-month-old nightly"), the target is **`nightly-2026-01-20`**. Any nightly within a week of that is acceptable if the canonical one has a known regression you hit during §6.2 below (check [releases.rs](https://releases.rs/) for the date's release notes if you suspect this).

### What prompted it

No external event. The project's TL decided it was time for a hygiene sweep. Recent prior work:

- 2026-04-17: the Vale response was sent (see `response-reducing-rustc-fork.md` and HANDOFF-TL.md §3a). That was the last user-visible thread.
- 2026-04-18 to 2026-04-20: a documentation-currency pass refreshed stale comments across `rustc-lang-facade/src/` and `toylangc/src/` to reflect the post-stage-5 / post-B6 state. You will see that refresh when reading files; it's all current.
- 2026-04-20: a scoping pass (the basis of §3 and §4 of this handoff) identified the drift surfaces and known API changes.

### Intended outcome

- `rust-toolchain.toml` pinned to the new nightly.
- Every hardcoded `nightly-2025-01-15` string updated (7 code sites, ~21 doc sites — see §3).
- Known API drift adapted at exact file:line locations (§4).
- Unknown API drift adapted as compiler errors surface it (§5).
- Full test suite green: **67 unit + 128 integration_projects + 15 standalone = 210**.
- Zero warnings from `cargo check`.
- `risks.md` §3 B3 updated with empirical cost of this bump (so the next bump has a data point).

**Non-goals:** do NOT add features, refactor code beyond what the bump forces, migrate to `rustc_public`, or touch anything unrelated. A bump is a bump.

---

## 2. Required reading before you code (3–4 hours)

Read in order. Stop when oriented; you can always come back.

1. `/Users/verdagon/erw/CLAUDE.md` — project-wide instructions. Compiler laws (especially "non-generic is the degenerate case of generic" and the `instantiate_identity` rule). Build conventions (pipe to `/tmp/<session>.txt`). **10 minutes.**

2. `/Users/verdagon/.claude/CLAUDE.md` if you're using the same shell tooling — the build-redirect conventions are mandatory. Pipe every `cargo run`/`cargo test`/`cargo check`/`cargo build` to a fixed `/tmp/<session>.txt` file, then inspect as a separate command. Don't chain `| grep`. **5 minutes.**

3. `/Users/verdagon/erw/HANDOFF-TL.md` — overall project orientation. §1 summary, §3 open threads (so you know what's NOT your problem), §5 reading order. **10 minutes.**

4. `/Users/verdagon/erw/docs/architecture/rust-interop-guide.md` — canonical architecture doc. You don't need to memorize it; you need to know where the query overrides live (Part 2) and which files they're in (Part 6). Read Parts 1–2 carefully, skim 3–5, skim the Part 8 arcana index. **30–45 minutes.**

5. **`/Users/verdagon/erw/docs/architecture/risks.md` — read this one carefully. §3 B2 (partitioner Outcome A), §3 B3 (MIR construction drift), §3 B4 (ABI helpers), §3 B5 (CGU lifetime). These are the four surfaces you'll be repairing, with estimated probability, canaries, and reaction strategy.** This is the single most important pre-bump document. **25 minutes.**

6. `/Users/verdagon/erw/docs/arcana/` — the `@ID` references you'll encounter in code comments. You don't need to read them front-to-back; when you see an `@ID` reference in a file you're editing, open the matching arcana. Each is 1–2 pages. **Read on-demand.**

7. `/Users/verdagon/erw/docs/historical/handoff-optimized-mir-migration.md` — a prior junior handoff. Read only §1 (Context) and §6 (staging discipline) — you're not doing that work, but the writing style models what this handoff is trying to match, and the staging discipline is applicable. **15 minutes.**

### Optional, as-needed

- `/Users/verdagon/erw/docs/reasoning/dep-discovery-approaches.md` — read if §5.1 (MIR construction) lands you somewhere confusing.
- `/Users/verdagon/erw/docs/arcana/GenerateCompileMutexLock-GCMLZ.md` — read if you're tempted to add a lock somewhere.
- `/Users/verdagon/erw/docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md` — read if you see `def_path_str` anywhere in a PR diff.

---

## 3. Pin occurrences to update (33 total, in 3 classes)

This was inventoried on 2026-04-20. If you see a `nightly-2025-01-15` that isn't in the list below, flag it — either the inventory drifted or you misread.

### 3.1 Canonical anchor (update FIRST, before anything else)

| File | Line | What |
|---|---|---|
| `rust-toolchain.toml` | 2 | `channel = "nightly-2025-01-15"` |

Changing this one line + running `cargo check` is how you learn what's broken. Do not update the code-level strings yet — they're cosmetic and won't block `cargo check`.

### 3.2 Hardcoded toolchain strings in code (7 sites across 4 files)

| File | Line | Context |
|---|---|---|
| `toylangc/src/main.rs` | 199 | `rustc --print sysroot` in `find_sysroot_tool` |
| `toylangc/src/main.rs` | 208 | `rustc -vV` in the same function (host triple) |
| `toylangc/src/build.rs` | 288 | String written into generated `.toylang-build/rust-toolchain.toml` |
| `toylangc/src/build.rs` | 295 | `rustc +<pin> --print sysroot` in `sysroot_lib` |
| `toylangc/src/build.rs` | 317 | `cargo +<pin> build` when spawning cargo |
| `toylangc/tests/standalone_tests.rs` | 14 | `rustup run <pin> rustc --print=sysroot` |
| `toylangc/tests/integration_projects.rs` | 49 | `rustup run <pin> rustc --print=sysroot` |

**Recommendation during this bump:** factor to a single constant. Somewhere appropriate (probably `toylangc/src/main.rs`, `pub const TOYLANG_NIGHTLY: &str = "nightly-2026-01-20";`) and reference it from all sites. This makes the NEXT bump trivial — one constant, one line, done. The refactor is small (~20 lines of change), bounded, clearly motivated by the bump.

### 3.3 Doc references (8 active files, ~21 sites)

Straightforward find-replace, but verify the surrounding prose is still accurate (e.g., risks.md §280 should be rewritten with empirical bump cost after you're done).

| File | Sites | Notes |
|---|---|---|
| `CLAUDE.md` | 3 | Instructions + test commands |
| `README.md` | 4 | Public-facing, be careful with wording |
| `HANDOFF-TL.md` | 10 | Includes test count line — re-verify 210 after bump |
| `docs/usage/testing.md` | 11 | Largest concentration |
| `docs/architecture/rust-interop-guide.md` | 2 | Status header + Overview |
| `docs/architecture/risks.md` | 1 | **Rewrite §280 with empirical bump cost data** (see §7.4) |
| `docs/reasoning/rustc-fork-design-space.md` | 1 | |
| `docs/reasoning/why-interleaved-monomorphization.md` | 1 | |
| `future-architecture-investigations.md` | 1 | |
| `rustc-lang-facade/src/queries/layout.rs` | 32 (comment) | Rewrite comment after verifying the `layout_of` key type on the new pin |
| `toylangc/tests/standalone_tests.rs` | 10 (comment) | Usually OK as-is |

### 3.4 Historical docs — DO NOT EDIT

`docs/historical/*.md` files are immutable records. They correctly reference `nightly-2025-01-15` because that's what was true at the time of each handoff. Per `docs/meta.md`, historical docs are not retconned.

The files not to touch:
- `docs/historical/toylang-rustc-driver-guide.md`
- `docs/historical/implemented.md`
- `docs/historical/old-rust-interop-architecture-guide.md`
- `docs/historical/handoff-codegen-backend-plugin.md`
- `docs/historical/handoff-optimized-mir-migration.md`
- `docs/historical/handoff-two-crate-migration.md`
- `docs/historical/phase-history.md`
- `docs/historical/problem-abi-coercion.md`

---

## 4. Known API drift — exact changes required

These three items are confirmed by PR dates. Addressing them is mechanical. Do not improvise beyond the described changes.

### 4.1 `RunCompiler::new(...).run()` → `run_compiler(...)` free function

**PR:** rust-lang/rust#135880 "Get rid of RunCompiler" — merged 2025-01-24 (9 days after our pin).

**What changed:** `rustc_driver::RunCompiler` was deleted entirely. Its former role is now a free function `rustc_driver::run_compiler(args, &mut callbacks)`. The setter methods that used to live on `RunCompiler` (`set_file_loader`, `set_make_codegen_backend`, `set_using_internal_features`) were removed — those fields now live on `Config` inside `Callbacks::config`. Our code doesn't use any of those setters, so the only change is the call-site shape.

**Sites:**

1. `rustc-lang-facade/src/driver.rs:52`
   - Current: `rustc_driver::RunCompiler::new(rustc_args, &mut driver).run();`
   - Becomes: `rustc_driver::run_compiler(rustc_args, &mut driver);`

2. `toylangc/src/main.rs:194` (inside `run_plain_rustc`)
   - Current: `rustc_driver::RunCompiler::new(args, &mut cb).run();`
   - Becomes: `rustc_driver::run_compiler(args, &mut cb);`

**Cost:** ~15 minutes. Two lines, one import consideration (verify the function is in scope — if not, `use rustc_driver::run_compiler;` — but our imports already have `rustc_driver` crate-level).

### 4.2 `CodegenBackend::codegen_crate` signature narrowed; `link` gains metadata

**PR:** rust-lang/rust#141769 "Move metadata object generation for dylibs to the linker code" — merged 2025-06-16.

**What changed:** `codegen_crate` no longer receives `metadata: EncodedMetadata` or `need_metadata_module: bool`. The metadata now flows into `link` as a new parameter. This is a trait signature change, so our `impl CodegenBackend for LangCodegenBackend` block must match.

**Sites:** all in `rustc-lang-facade/src/codegen_wrapper.rs`.

1. `codegen_wrapper.rs:72-77` — the `codegen_crate` method signature:
   ```rust
   // Current:
   fn codegen_crate<'tcx>(
       &self,
       tcx: rustc_middle::ty::TyCtxt<'tcx>,
       metadata: rustc_metadata::EncodedMetadata,
       need_metadata_module: bool,
   ) -> Box<dyn Any>

   // Becomes:
   fn codegen_crate<'tcx>(
       &self,
       tcx: rustc_middle::ty::TyCtxt<'tcx>,
   ) -> Box<dyn Any>
   ```

2. `codegen_wrapper.rs:99` — the forwarding call:
   ```rust
   // Current:
   let result = self.inner.codegen_crate(tcx, metadata, need_metadata_module);

   // Becomes:
   let result = self.inner.codegen_crate(tcx);
   ```

3. `codegen_wrapper.rs:141-148` — the `link` method gains the metadata parameter:
   ```rust
   // Current:
   fn link(
       &self,
       sess: &Session,
       codegen_results: CodegenResults,
       outputs: &OutputFilenames,
   ) {
       self.inner.link(sess, codegen_results, outputs);
   }

   // Becomes:
   fn link(
       &self,
       sess: &Session,
       codegen_results: CodegenResults,
       metadata: rustc_metadata::EncodedMetadata,
       outputs: &OutputFilenames,
   ) {
       self.inner.link(sess, codegen_results, metadata, outputs);
   }
   ```

**Cost:** ~30 minutes. All three changes are local to one file. Test by running `cargo check -p rustc-lang-facade`.

### 4.3 `Callbacks` trait gained methods (no change needed)

**What:** `rustc_driver::Callbacks` gained `after_crate_root_parsing` and `after_expansion`, both with default implementations. Our `impl rustc_driver::Callbacks for LangDriver` at `driver.rs:57` only overrides `config` and `after_analysis`. The defaults cover the new methods.

**Sites:** none. Mentioned for completeness so you don't panic when you see the larger trait surface on rustdoc.

### 4.4 `ParamEnv → TypingEnv` (no change needed)

**PR:** rust-lang/rust#132460 — merged 2024-11-19 (before our pin). Our code already uses `TypingEnv::fully_monomorphized()`. No action.

### 4.5 `Abi → BackendRepr` rename (no change needed)

**PR:** rust-lang/rust#132385 — merged 2024-11-01 (before our pin). Our code already uses `BackendRepr`. No action.

---

## 5. Unknown API drift — surfaces that may need repair

These four surfaces have historically drifted per nightly. You won't know what broke until `cargo check` tells you. The strategy for each is "read the current rustc source, adapt the signature, don't redesign."

### 5.1 MIR construction (risks.md §B3 — highest drift probability)

**Files:**
- `rustc-lang-facade/src/queries/optimized_mir.rs` — `build_dependency_body` (~110 LoC)
- `rustc-lang-facade/src/mir_helpers.rs` — `build_drop_call_body` (~100 LoC)

**What to watch for:** struct field additions/removals on `Body`, `BasicBlockData`, `Statement`, `Terminator`, `LocalDecl`, `SourceScopeData`, `ConstOperand`. Enum variant changes on `Rvalue`, `CastKind`, `StatementKind`, `TerminatorKind`, `PointerCoercion`, `UnwindAction`, `CallSource`, `NullOp`. Constructor-signature drift on `Body::new`.

**How to repair:** open rustc's source at `<sysroot>/lib/rustlib/src/rust/compiler/rustc_middle/src/mir/mod.rs` (and adjacent files). Find the struct/enum the compile error names. Match its current shape. Adjust our constructor call. Re-run `cargo check`.

**Tool:** `rustc +nightly-2026-01-20 --print sysroot` gives you the toolchain root; rustc's source lives at `<sysroot>/lib/rustlib/src/rust/compiler/`. Requires `rust-src` component installed via `rustup component add --toolchain nightly-2026-01-20 rust-src`.

**Validation:** once `cargo check` is clean, run `cargo +nightly-2026-01-20 run -- build` against a simple integration project with `RUSTFLAGS="-Zvalidate-mir"` set. This catches MIR shape violations that slipped past the compiler.

**Expected cost:** 3–7 days. Tedious but tractable. The code is ~210 LoC total; you'll probably touch 5–20 lines of it.

### 5.2 ABI helpers (risks.md §B4)

**File:** `rustc-lang-facade/src/abi_helpers.rs` (~210 LoC)

**What to watch for:** `PassMode` variants (these have split in past bumps). `BackendRepr`, `Primitive`, `Reg`, `RegKind`, `CastTarget` shape changes. Match-arm exhaustiveness errors are the usual symptom.

**How to repair:** if a new `PassMode` variant appears, read rustc's source to understand what it represents, then extend our match with a correct mapping (usually follows the pattern of adjacent variants). If an existing variant split, our match arm probably needs to split too.

**Cross-file impact:** `toylangc/src/llvm_gen.rs` consumes the `CoercedReturn` / `CoercedParam` enums from `abi_helpers.rs`. If you extend those enums, llvm_gen's matches need updating. Don't change the enum shape without checking both sides.

**Expected cost:** 2–5 days. Usually localized.

### 5.3 Partitioner / MonoItemData / Outcome A (risks.md §B2) — **escalation gate**

**Files:**
- `rustc-lang-facade/src/queries/partition.rs` — `lang_collect_and_partition_mono_items` (~90 LoC)
- `rustc-lang-facade/src/cgu_stash.rs` — CGU lifetime erasure (~80 LoC)

**What to watch for:** `MonoItemData` field changes, `CodegenUnit<'tcx>` constructor drift, the `internalize_symbols` call timing relative to our override's return.

**Canary:** the Phase-6 generic-wrapper tests. Specifically:
- `test_option_unwrap_basic`
- `test_result_unwrap_basic`
- `test_unwrap_arithmetic_chain`
- Every Vec/HashMap test that exercises generic `#[inline(never)]` wrappers

If those tests pass, Outcome A is still holding. If they fail with **link errors of the form `undefined symbol: __toylang_option_unwrap_i32` or `duplicate symbol: __toylang_*`**, the assumption just broke.

**ESCALATION GATE:** If B2 fires (Phase-6 link errors after the bump), **STOP. Do not proceed with a fork retreat. Contact the TL.** The reaction strategy in risks.md §B2 lists three options; picking among them is a TL decision, not a junior one. It matters because the fork retreat has long-term-maintenance implications (a rustc fork to maintain) that a junior shouldn't unilaterally commit the project to.

What to send when escalating:
1. The exact test names that failed
2. The failing linker output (copy-paste from `/tmp/erw-bump.txt`)
3. `git log --oneline -5` from main
4. What rustc PR(s) touched `rustc_monomorphize::partitioning` between `2025-01-15` and the target nightly (search commits; the TL will guide you through this if needed)

**Expected cost if B2 holds:** 0–1 day. Tests pass, move on.
**Expected cost if B2 fires:** +1–2 weeks after TL decision.

### 5.4 Query provider function-pointer signatures (low drift)

**Files:** the `pub type <QueryName>Fn = for<'tcx> fn(...)` lines in `rustc-lang-facade/src/queries/*.rs`.

**What to watch for:** if rustc changes a query's key or return type, our typedefs must match. Most common: `layout_of`'s key type (currently `PseudoCanonicalInput<'tcx, Ty<'tcx>>`) has drifted before.

**How to repair:** update the typedef to the current signature. Re-run `cargo check`.

**Expected cost:** 0–1 day.

---

## 6. Implementation steps (staged sub-phases)

Each phase is a commit point. If a phase is green (compiles + tests pass), commit and move on. If stuck, roll back to the last green commit and re-plan.

### 6.1 Baseline snapshot

Before touching anything:

```bash
cargo +nightly-2025-01-15 test -p toylangc 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Should show three lines totaling 210 passing. If your baseline isn't 210/210, stop and figure out why before you bump — otherwise you can't tell what the bump broke.

Note the exact test counts in a commit message or scratch note. Post-bump regression hunt depends on having this reference.

### 6.2 Install the new toolchain

```bash
rustup toolchain install nightly-2026-01-20 --component rustc-dev --component rust-src --component llvm-tools-preview
```

Verify:

```bash
cargo +nightly-2026-01-20 --version
rustc +nightly-2026-01-20 --print sysroot
```

If the date is a weekend or has known issues, try ±3 days. Check `https://releases.rs/` for the specific date's release-notes page.

### 6.3 Flip the canonical anchor

Edit `rust-toolchain.toml` line 2 to `channel = "nightly-2026-01-20"`.

Now `cargo check` (no `+pin`) will use the new toolchain by default.

```bash
cargo check 2>&1 > /tmp/erw-bump.txt
tail -40 /tmp/erw-bump.txt
```

You will get compile errors. That's the point. Proceed to 6.4.

### 6.4 Adapt the facade to known drift (§4)

Fix §4.1 (RunCompiler → run_compiler) and §4.2 (codegen_crate signature). These are mechanical; should take about an hour.

```bash
cargo check -p rustc-lang-facade 2>&1 > /tmp/erw-bump.txt
tail -40 /tmp/erw-bump.txt
```

If clean, commit:

```bash
git add -u
git commit -m "Adapt to RunCompiler removal (#135880) and codegen_crate signature change (#141769)"
```

Don't worry about a prettier commit message yet; you'll rewrite these in the final PR description.

### 6.5 Adapt the facade to unknown drift (§5)

This is the longest phase. Iterate:

```bash
cargo check -p rustc-lang-facade 2>&1 > /tmp/erw-bump.txt
tail -80 /tmp/erw-bump.txt
```

For each error, identify which §5 surface it belongs to (MIR construction, ABI helpers, partitioner, query types). Open the referenced file, read the current rustc source for context, adapt. Repeat until `cargo check -p rustc-lang-facade` is clean.

**Strongly recommended:** commit after each self-contained repair. "Adapt `build_dependency_body` to `CastKind::PointerCoercion` shape change." One arcana-sized change per commit makes the bump auditable and the history useful for the next bump.

### 6.6 Adapt the consumer

```bash
cargo check -p toylangc 2>&1 > /tmp/erw-bump.txt
tail -80 /tmp/erw-bump.txt
```

Usually much smaller than 6.5. If the facade compiles, the consumer often only has downstream match-arm updates (e.g., new `PassMode` variant in `llvm_gen.rs`) or typedef-import fixes.

### 6.7 Update hardcoded pin strings (§3.2)

Now update the 7 code sites in `toylangc/src/main.rs` (2), `toylangc/src/build.rs` (3), `toylangc/tests/standalone_tests.rs` (1), `toylangc/tests/integration_projects.rs` (1). Consider the `const TOYLANG_NIGHTLY` refactor mentioned in §3.2 — it's small and clearly in-scope for a bump.

```bash
cargo check --tests 2>&1 > /tmp/erw-bump.txt
tail -20 /tmp/erw-bump.txt
```

### 6.8 Run unit tests

```bash
cargo test -p toylangc --bins 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Target: 67/67. Unit tests don't exercise rustc-integration so they should basically always pass once the code compiles.

### 6.9 Run integration tests

```bash
cargo test -p toylangc --test integration_projects 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Target: 128/128.

**This is where B2 would fire** (risks.md §B2 canary). If any of `test_option_unwrap_basic`, `test_result_unwrap_basic`, `test_unwrap_arithmetic_chain`, or Vec/HashMap tests fail with link errors → **STOP AND ESCALATE** (see §5.3).

If integration tests fail for reasons OTHER than B2 (e.g., a specific toylang construct's codegen is wrong), iterate: bisect the failure by running individual tests, narrow down which change caused it, fix it. These are usually ABI-helper drift surfacing at runtime (§5.2) that didn't show as a compile error.

### 6.10 Run standalone tests

```bash
cargo test -p toylangc --test standalone_tests 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Target: 15/15.

These are Phase 7 smoke tests against real crates.io deps. If they fail, it's usually B3 (MIR construction drift for deep dep graphs) surfacing. Same approach: bisect, adapt, commit.

### 6.11 Full suite verification

```bash
cargo test -p toylangc 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Should show three "test result:" lines summing to **210 passed, 0 failed, 0 ignored**. If not, don't proceed.

### 6.12 Warning check

```bash
cargo check --tests 2>&1 > /tmp/erw-bump.txt
grep -E "warning|error" /tmp/erw-bump.txt
```

The project targets zero warnings. If new deprecations appeared with the new nightly (common), either fix them or silence via `#[allow(deprecated)]` with a `// TODO: nightly bump` comment. Don't leave a silent warning.

### 6.13 Doc sweep (§3.3)

Replace every `nightly-2025-01-15` in the 8 active doc files with the new pin. Verify the surrounding prose is still accurate (e.g., `README.md`'s wording).

**Special case: `docs/architecture/risks.md` §3.** Under "Nightly-pin strategy" (line ~280), update the empirical cost guidance with what you actually experienced this bump:

- How many days did you spend?
- Which surfaces drifted the most?
- Did B2 fire? If so, how did you resolve it?

This data point is load-bearing for the NEXT bump; don't skip it.

### 6.14 HANDOFF-TL.md touch-up

Update:
- §1 test count (should still be 210 unless the bump added or retired any)
- §4 "Rust team, implicit" paragraph — pin reference
- §8 "Commands to know" — all `+pin` references
- §9 sanity checklist — `+pin` reference

### 6.15 Cleanup this handoff doc

Move this file (`HANDOFF-nightly-bump.md`) to `docs/historical/handoff-nightly-bump-2026-04.md` (or similar date-stamped name). Add a short header at the top: "Shipped — merged on <date>, nightly bumped from 2025-01-15 to 2026-01-20." Follow the convention in `docs/historical/handoff-*.md` — look at `docs/historical/handoff-two-crate-migration.md` for the shape.

### 6.16 Final PR

One PR, descriptive title, body summarizing:
1. What bumped (2025-01-15 → 2026-01-20)
2. Known drift addressed (PR #135880, PR #141769)
3. Unknown drift encountered and adapted (list by surface)
4. Test suite: 210/210
5. Total time spent (for the risks.md data point)

Link to this handoff doc (now at its historical location).

---

## 7. Critical subtleties

These are things that will bite you if you don't watch for them.

### 7.1 `cargo +nightly-2025-01-15` vs bare `cargo`

Once you've changed `rust-toolchain.toml`, bare `cargo` uses the new toolchain. But every BUILD CONVENTION DOC and every user habit says `cargo +nightly-2025-01-15`. If you run `cargo +nightly-2025-01-15 test` by habit while mid-bump, you'll be testing against the OLD toolchain. Either use bare `cargo` during the bump or use the new pin explicitly.

### 7.2 Shared `CARGO_TARGET_DIR` contamination

Integration tests share a target dir (see `toylangc/tests/integration_projects.rs`'s `shared_cargo_target_dir()`). If you run tests against both old and new toolchain at different points, the shared cache gets confused. If test failures seem inconsistent between runs, wipe the cache:

```bash
rm -rf toylangc/target/integration-projects-cache
```

Also wipe any `.toylang-build/` dirs from previous test runs:

```bash
find toylangc/tests/integration_projects -name ".toylang-build" -type d -exec rm -rf {} +
```

### 7.3 `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` drift

On macOS you need `DYLD_LIBRARY_PATH` set to the new nightly's sysroot `lib/`. On Linux it's `LD_LIBRARY_PATH`. Test harnesses set this automatically via `sysroot_lib()`, but if you run binaries by hand they'll segfault on library-loading without it. If you see "Library not loaded: librustc_driver" or equivalent, this is why.

### 7.4 B6's `populate_toylang_instances_from_cgus` is architecturally load-bearing

See `docs/architecture/risks.md` §B6 RESOLVED. The post-stage-5 B6 fix made consumer codegen depend on a deterministic up-front walk of the partitioner's CGU list, not on query-provider side effects. If the partitioner's output shape changes (see §5.3), this walk may need adapting. Do not assume "the CGU walk always works" — re-verify after bumping by checking that `test_toylang_main_calls_toylang_fn` and the deep-chain tests still produce their expected output.

### 7.5 Don't introduce `def_path_str` anywhere new

Per arcana `@DPSFDOZ`, `tcx.def_path_str` ICEs outside diagnostic contexts. If the compiler suggests `def_path_str` as an alternative to some method that got renamed, do NOT take the suggestion. Use `is_from_lang_stubs` or `tcx.def_path(def_id).data` walks instead.

### 7.6 Don't introduce a mutex lock in query-provider code

Per arcana `@GCMLZ`, any lock on `MUTABLE_STATE` acquired during `generate_and_compile` will deadlock. If you find yourself adding a `Mutex::lock` in any query provider callback while adapting to drift, stop and re-read `@GCMLZ`. There's almost certainly a lock-free alternative (read from `CONFIG`/`DEFAULT_*` OnceLocks).

### 7.7 `instantiate_identity` requires a comment

Per `CLAUDE.md`'s compiler law: every `EarlyBinder::instantiate_identity()` call MUST have a comment explaining why we're not substituting real values. If drift adaptation requires you to add a new call, add the comment. If you delete one, delete the comment.

### 7.8 Historical docs and generated `.toylang-build/` are off-limits

Don't edit `docs/historical/*.md`. Don't edit anything under `toylangc/tests/integration_projects/*/.toylang-build/` — those directories are regenerated by the test harness on every run.

### 7.9 Budget recalibration is allowed

If you hit day 5 of 5.1 and the MIR repair isn't done, that's normal. Don't rush. Commit what you have, update your progress note, keep going. The 2-week expected budget has slack for exactly this.

### 7.10 Compiler laws from CLAUDE.md still apply

- "Non-generic is the degenerate case of generic" — don't add a special-case branch for the non-generic path when adapting to drift.
- "Prefer treating `self` as just another parameter" — if drift removes some self-specific API, reassemble the full parameter list rather than adding ad-hoc handling.

---

## 8. Verification

Full suite must pass:

```bash
cargo test -p toylangc 2>&1 > /tmp/erw-bump.txt
grep "test result:" /tmp/erw-bump.txt
```

Three lines, totaling 210 passed, 0 failed, 0 ignored, cold and warm.

Also:

```bash
cargo check --tests 2>&1 > /tmp/erw-bump.txt
grep -E "warning|error" /tmp/erw-bump.txt
```

Should be empty or only contain allow-listed warnings with `// TODO: nightly bump` comments (see §6.12).

And spot-check that the pin actually changed:

```bash
grep -r "nightly-2025-01-15" rustc-lang-facade/src toylangc/src toylangc/tests
```

Should return only historical-doc results if any (shouldn't match the listed dirs). If it matches any non-historical files, you missed a site.

---

## 9. Out of scope

Do not do any of the following during this bump. If you're tempted, write a note and move on.

- **Don't refactor beyond what drift forces.** A bump is a bump. If you see something that "would be cleaner" nearby, leave it.
- **Don't migrate to `rustc_public` / `stable_mir`.** That's a separate investigation tracked in `future-architecture-investigations.md`.
- **Don't add features.** Tempting during a rewrite; don't.
- **Don't close out the three open tech-debt items** (`known-tech-debt.md` #5, #28, #29). Those are separate tasks.
- **Don't touch the Vale response** (`response-reducing-rustc-fork.md`). It's sent; leave it alone.
- **Don't touch historical docs.**
- **Don't delete `~/rust`** even if it's still around. Separate judgment call per HANDOFF-TL.md §3d.
- **Don't bump past a ~3-month-old nightly.** The bleeding-edge hazard is real.
- **Don't introduce a rustc fork** without TL approval (see §5.3 and §10).

---

## 10. If you get stuck

### 10.1 Escalation gates — contact the TL immediately

- **§5.3 fires (partitioner Outcome A breakage).** Phase-6 link errors after the bump. Do NOT retreat to a fork on your own initiative. Send the diagnostic packet described in §5.3. This is a project-shape decision, not a junior decision.

- **A query/callback you touch needs a new lock on `MUTABLE_STATE`.** Per `@GCMLZ`, this should not happen. If drift adaptation seems to require it, escalate. There's likely a lock-free approach.

- **Drift repair takes more than 3 weeks elapsed time.** At that point, the project's risk posture may warrant rolling back to the old nightly and reassessing strategy. Don't keep pushing indefinitely.

- **You cannot determine whether a behavior change is intentional drift or a bug we're supposed to paper over.** Ask.

### 10.2 Self-recovery tactics — try these first

- **Read the failing rustc source directly.** `<sysroot>/lib/rustlib/src/rust/compiler/` has the full tree. Grep for the struct/enum name in the error; the source is usually self-documenting.

- **Check rust-lang/rust's PR history.** For any API that changed, there's a PR with rationale. Grep `rust-lang/rust` issues/PRs for the old name to find the renaming/restructuring PR.

- **Check what rust-analyzer or miri did.** Both are `rustc_private` consumers. If they adapted to the same drift, their commit history for the affected nightly window is a ready-made template.

- **Roll back to a green commit.** You have staged commits from §6 for exactly this purpose. If a repair went sideways, roll back to the last green state and retry with the lesson.

- **Wipe and retry.** Cargo-cache weirdness is real. If a test fails inconsistently between runs, wipe the target dir (§7.2) and retry before concluding it's a code problem.

### 10.3 Research prompts that tend to help

When diagnosing MIR construction drift, useful search terms:
- `MIR <rustc version> <struct_name> field`
- `rust-lang/rust <PR search terms>`
- `rustc-dev-guide <concept>`

When diagnosing ABI drift:
- `rustc <PassMode/BackendRepr/etc> 2025`
- search rust-lang/rust issues for `abi_helpers`-equivalent crates' bug reports

### 10.4 What NOT to do

- Don't push to `main` without green tests.
- Don't use `--no-verify` on any git command.
- Don't comment out failing tests to make the suite green.
- Don't bypass the MIR validator by removing `-Zvalidate-mir`.
- Don't silence a warning you don't understand — investigate first.
- Don't decide unilaterally to change the target nightly if the current one has a regression; ask before picking a new date.

---

## 11. Rollback plan

If mid-bump you decide to abort (feature pressure from the TL, unrecoverable drift, B2 fires with a bad outcome), rolling back is clean because of the staged commits:

```bash
# Find the pre-bump commit (the baseline commit from §6.1, before you touched rust-toolchain.toml)
git log --oneline -20

# Reset to it (DESTROYS uncommitted changes — confirm with the TL first if any in-flight work)
git reset --hard <pre-bump-sha>

# Reinstall the old toolchain if you uninstalled it
rustup toolchain install nightly-2025-01-15

# Verify you're back to baseline
cargo +nightly-2025-01-15 test -p toylangc 2>&1 > /tmp/erw-rollback.txt
grep "test result:" /tmp/erw-rollback.txt  # should show 210 passing
```

Write a note in `docs/historical/` about what you tried and where it got stuck — the next attempt will benefit from the reconnaissance.

---

## 12. One-paragraph recap

You're bumping the toolchain from `nightly-2025-01-15` (15 months old) to `nightly-2026-01-20` (3 months old). The known drift is small and mechanical (§4). The unknown drift is bounded to four surfaces (§5), budgeted at 1.5–2 weeks. There is one escalation gate: if Phase-6 unwrap tests fail after the bump (risks.md §B2), stop and ask the TL before doing a fork retreat. Work in staged commits, verify 210/210 at the end, update `risks.md` with empirical cost, move this handoff to `docs/historical/`. Don't refactor beyond what drift forces; don't add features; don't touch the Vale response or historical docs. If you get stuck for more than a day without progress, ask — the TL would rather unblock you than have you grind.

Good luck.
