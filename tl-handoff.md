# Handoff: erw → Sky Quarter Of Work

Hi, future-you / junior engineer. You're picking up a multi-week project. This
document is your full briefing — read it end to end before you touch any code.
Sections are ordered "context first, action last."

If you only read one thing, read **§7 Where to start**.

---

## 1. The project in one paragraph

`erw` is a Rust-interop prototype currently embodied by two crates:
`rustc-lang-facade` (a reusable library that hooks into rustc via query
overrides) and `toylangc` (a toy consumer language that uses the facade to
prove out interop mechanics). The **architectural target** is **Sky**, a
memory-safe systems language whose compiler will use the same facade pattern.
The master design doc is `rust-interop-architecture.md` (5,148 lines). The
catalog of places where erw currently diverges from Sky is `course-correct.md`
(18 items). This project — driven by the plan at
`/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md` — adds
tests that exercise the five "hard cases" (1b, 3, 4, 5, 6) from the
architecture doc's seven-case taxonomy. As a side effect, four course-correct
items land (`#6`, `#11`, `#15`, `#16`). **Items #11 and #15 are done as of
Session 4** (sidecar load + oracle cross-crate sweep + Workstream A). Items
#6 and #16 land in Phase 3 (multi-crate).

---

## 2. The story so far (sessions before yours)

You should know the recent history because file names and concepts in your
work descend from it.

**Session 1 (course-correct.md authoring).** Read the architecture doc end
to end. Produced `course-correct.md` — 18 places in the prototype that
diverge from Sky's locked decisions.

**Session 2 (Approach A restoration — course-correct item #1).** The biggest
divergence: erw's "stage 3" migration replaced an Instance-keyed forked
`per_instance_mir` query with a DefId-keyed `optimized_mir` override
(Approach A → Approach B), and "stage 4" went further by retiring all rustc
fork patches to zero-fork. Sky **requires** Approach A because Sky's
arbitrary-typed comptime arguments cannot be substituted by rustc's
collector — only Sky's frontend understands Sky-typed values.

This session restored Approach A end-to-end. Three workstreams:
- **W1**: Rebuilt the 3-patch rustc fork (declare query, collector hook,
  default-None provider). `~/rust` checkout bumped from rustc 1.86 (Jan 2025)
  to rustc 1.95.0-dev / commit `d940e568` (matching `nightly-2026-01-20`).
  Build took 14:43 with `download-ci-llvm = true`. Installed as toolchain
  `rustc-fork` at `$HOME/rust/build/host/stage2`.
- **W2**: Facade switched from `optimized_mir.rs` to `per_instance.rs`.
  Trait method `collect_generic_rust_deps` now takes `ty::Instance<'tcx>`
  instead of `LocalDefId`.
- **W3**: Toylang substitutes `instance.args` concretely Sky-side. Retired
  `oracle::ActiveParamMap` thread-local. `debug_assert!(!instance.args.has_param())`
  added in `build_dependency_body`. `ResolvedType::TypeParam` arm in
  `try_resolved_to_rustc_ty` (oracle.rs:292) now panics unconditionally
  ("Sky-side substitution should have replaced it").

After W3: 210/210 tests pass. Plus a positive-evidence probe test
`test_approach_a_callback_log_shape` was added — asserts the
`CollectGenericRustDeps` log entries carry an `args_fingerprint` field, which
is sharp regression protection for the trait-signature shape.

**Session 3 (quarter-of-work Phase 1 Workstream S start).** Wrote this plan.
Started Phase 1. Landed three sub-commits:
- **S.1**: Sidecar format spec doc + `SidecarHeader` types.
- **S.2**: Bincode serialization + BLAKE3 checksum + serde derives + `BTreeMap` promotion.
- **S.3**: Sidecar written at end of rlib compile's `after_rust_analysis`.

Test counts after Session 3: 225/225 (81 unit + 129 integration + 15
standalone). 121 `.sky-meta` sidecar files materialize during the test run.

**Session 4 (this session — finished Workstream S, oracle sweep, Workstream A).**
Three major pieces landed plus one attempted+rolled-back, then re-tried after
removing the blocker:

- **S.4** (sidecar load): Facade walks `tcx.crates(())`, finds Sky-marked rlibs
  via `is_from_lang_stubs`, reads adjacent `.sky-meta` files (with cargo's `lib`
  prefix strip), fires new `LangCallbacks::on_sky_lib_loaded(tcx, crate_name,
  bytes)` trait method (Option B per §7.3 — facade doesn't know consumer payload
  shape). Toylang's impl deserializes via `sidecar::deserialize_sidecar`, stores
  in new `ToylangState.upstream_registries: BTreeMap<String, ToylangRegistry>`.
  New `CallbackLog::OnSkyLibLoaded` variant + `test_s4_sidecar_load_smoke`.
- **S.5** (sidecar determinism CI invariant): `test_s5_sidecar_determinism`
  builds the `arithmetic` fixture twice with isolated per-run `CARGO_TARGET_DIR`s
  and byte-compares the two `.sky-meta` files. Panic on mismatch points at
  common causes (HashMap-leak, bincode-cfg drift, timestamp leak).
- **Workstream A first attempt — REVERTED.** A.1+A.2+A.4 prototype compiled
  but tripped a deeper blocker: `oracle::find_extern_fn_def_id` walks only
  LOCAL HIR items. At user-bin compile every `extern "C" { fn ... }` decl is
  in the upstream `__lang_stubs` rlib, not local. Lookup panics. Reverted A
  edits, documented findings in `workstream-a-scope-notes.md`.
- **Oracle cross-crate sweep** (the blocker fix): completed stage-5a's
  half-done refactor. Added cross-crate fallbacks to `find_extern_fn_def_id`
  + new `find_extern_fn_in_stub_rlib` + `find_stub_fn_in_stub_rlib` +
  `find_wrapper_fn_def_id`. Same pattern stage 5a applied to
  `resolve_rust_path`. New `CallbackLog::OracleCrossCrateProbe` variant +
  isolated `oracle_probe/` fixture + `test_oracle_cross_crate_extern_fn_lookup`
  asserting the fallback fires at user-bin compile.
- **Workstream A re-attempted — LANDED.** With oracle blocker gone, A.1+A.2+A.4
  shipped. Rlib compile produces no toylang `.o` (`llvm_paths = None`). User-bin
  compile is the codegen site. Discovery shifted from upstream-CGU walk (finds
  zero items at user-bin) to **registry-driven**: iterate `registry.functions`,
  push `ToylangInstance` per non-generic body-bearing fn, look up `pub fn` shell
  DefId in `__lang_stubs`, transitively `walk_and_stash_internal_callees` to
  surface generic monomorphizations like `wrap<i32>`. New `stub_def_id` field
  on `ToylangInstance` lets codegen build `Instance::new_raw(def_id, empty_args)`
  for extern wrapper emission. See `workstream-a-scope-notes.md` for the
  detailed completion notes including the two key unlocks.

Test counts after Session 4: **228/228** (81 unit + 132 integration + 15
standalone). Course-correct items #11 and #15 are done. Sky-aligned shape
at the SHAPE level for single-file programs; literal Sky shape (per-library
publishing) lands in Phase 3.

---

## 3. The plan you're executing

Master plan: `/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md`.
Read it. It's the authoritative scope; this handoff document is the
narrative version.

Three phases, ~15 weeks total:

| Phase | Weeks | Scope | Cases unlocked |
|---|---|---|---|
| 1 | 1-7 | Sidecar (S) + binary-compile codegen (A) + typeresolver fix (B) + rust_caller fixtures (D) | 1b, 3, 5 |
| 2 | 8-12 | Toylang `impl rust_trait for toylang_type` language feature (C) | 4 |
| 3 | 13-15 | Multi-toylang-crate workspace + marker-based detection (E) | 6 |

**Phase 1 — substantially complete.** S.1–S.5 done, oracle cross-crate
sweep done, Workstream A (A.1+A.2+A.4) done. Remaining within Phase 1:
A.5 (byte-identical pass-through corpus, CI infrastructure work),
Workstream B (~60 LOC oracle TypeParam tolerance), Workstream D
(rust_caller fixtures + Cases 1b/3/5 tests). Then Phase 2 (toylang
`impl rust_trait for toylang_type`), then Phase 3 (multi-crate — this
is where toylang's shape matches Sky literally rather than as a
rehearsal).

Detailed commit-by-commit schedule is in the master plan's "Sequencing
recommendation" table at the end. Use that as your daily checklist.

---

## 4. Critical context you need

### 4.1 Approach A: why per_instance_mir matters

Read `rust-interop-architecture.md` §3.1 + §19.1 carefully. The
one-paragraph version: rustc's mono collector substitutes generic args
inline as it walks. For toylang's currently-supported generics (rustc-
representable types), Approach B works fine. For Sky's comptime args
(arbitrary Sky-typed values), rustc literally cannot represent them, so
substitution MUST happen Sky-side before MIR construction. Sky's only
viable path is Instance-keyed `per_instance_mir` where the consumer's
provider receives the concrete `Instance` and returns a fully-substituted
body.

`docs/historical/approach-a-reference/` has the historical Approach A
implementation extracted from before stage 3 retirement. Read its README;
it's a structural template for any future change to the substitution
direction.

The `debug_assert!(!instance.args.has_param())` in
`rustc-lang-facade/src/queries/per_instance.rs::build_dependency_body` is
load-bearing — if it fires during a test run, Approach B behavior has
regressed somewhere upstream.

### 4.2 The sidecar architecture (your current workstream)

Read `rust-interop-architecture.md` §7 + §8, then `docs/architecture/sidecar-format.md`
(I wrote it in S.1, it's the implementer-facing reference).

Picture: Sky libraries compile to **rlib + sidecar only** — no Sky `.o`.
The rlib contains Rust stub source (compiled by rustc) with
`unreachable!()` bodies. The sidecar (`.sky-meta`) is a binary blob
carrying the typed AST for every item in the library — exports AND
non-exports. The binary compile reads sidecars from every Sky-marked rlib
it loads, then codegens **every reachable Sky item across all libs** into
one consolidated `.o`.

This is course-correct items #11 + #15 in their locked end states. Phase 1
Workstream A is what actually moves toylang to this emission model.
Workstream S is the foundation it depends on.

### 4.3 The two-symbol architecture (Sessions ago, still relevant)

Toylang emits three layers of symbols for any item:
1. `__toylang_internal_<name>__<mangled_targs>` — toylang↔toylang ABI.
   Used by toylang's own codegen for toylang→toylang calls.
2. `__toylang_impl_<name>__<mangled_targs>` — Rust-ABI-coerced extern
   wrapper. Used when Rust code calls into toylang.
3. The rustc-mangled name (`__lang_stubs::wrap::<i32>`) — what Rust
   source sees. The facade's `symbol_name` override at
   `rustc-lang-facade/src/queries/symbol_name.rs:31-80` reroutes this to
   `__toylang_impl_*`.

Important for your work: **Sessions before you had a wrong mental model**
that called this a "symbol mismatch." It's not. The override at
`queries/symbol_name.rs` already makes Rust call sites resolve to the
right symbol. The REAL blocker is **cross-crate generic instantiation**:
toylang's `.o` is emitted at the rlib compile, before Rust call sites for
`wrap::<LocalThing>` exist at the user_bin compile. Workstream A's job is
to fix this by moving codegen to the binary compile.

### 4.4 The two compiles per build

When you run `toylangc build` on a project, cargo invokes rustc TWICE:
- **rlib compile**: compiles the `__lang_stubs` rlib. Crate name starts
  with `lang_stubs_`. Marker `is_downstream_of_stubs = false`.
- **user_bin compile**: compiles the binary that depends on
  `__lang_stubs`. Crate name is the user's project name. Marker
  `is_downstream_of_stubs = true`.

Each invocation is its own process — independent state, no cross-process
sharing. The callback log file `.toylang-build/callback.log` gets written
by whichever compile fires `generate_and_compile`. Today: rlib compile
writes it (llvm_paths = Some), user_bin compile doesn't (llvm_paths = None,
returns early at `callbacks_impl.rs:584`).

Phase 1 Workstream A INVERTS this. After A, the rlib compile produces NO
`.o` (and stops writing the log) and the user_bin compile produces ALL
`.o` (and writes the log).

The gate variable `is_downstream_of_stubs` appears at
`callbacks_impl.rs:75, 191, 214, 387` and `main.rs:145-195`. Workstream A
renames it `is_user_bin_compile` and inverts its semantics throughout.

---

## 5. Files you'll touch (organized by area)

### 5.1 Facade (`rustc-lang-facade/src/`)

- **`lib.rs`** — trait `LangCallbacks`, vtables, trampolines, `OnceLock`
  defaults, the `is_from_lang_stubs` / `is_consumer_codegen_target` /
  `is_consumer_accessor_safe` predicates. This is the integration boundary
  between facade and consumer (toylang).
  - For Phase 1 S.4: you'll add sidecar-loading machinery here.
  - For Phase 3 E.1: you'll replace `is_from_lang_stubs` with marker-based
    detection.
  - For Phase 2 C.6: you'll extend `is_consumer_accessor_safe` to handle
    trait-impl methods (`tcx.impl_trait_ref`).
- **`queries/per_instance.rs`** — Approach A's `per_instance_mir` provider.
  The `debug_assert!(!instance.args.has_param())` is here. Don't touch
  unless you really mean to.
- **`queries/optimized_mir.rs`** — DOES NOT EXIST. It was deleted in W2 of
  the previous session. Don't bring it back. (If you grep for
  "optimized_mir" you'll find historical references in comments — that's
  fine.)
- **`queries/symbol_name.rs`** — the Rust-mangled-name → `__toylang_impl_*`
  redirect. For Phase 2 C.6 you'll add trait-impl method handling here.
- **`queries/layout.rs`** — `layout_of` override for consumer types. The
  layout-probe tests grep its `[toylang] layout_of intercepted for: ...`
  stderr line. Don't add new eprintlns nearby without thinking about it.
- **`queries/partition.rs`** — CGU filter. Should be largely untouched in
  Phase 1; Phase 3 E.1's marker-based predicate will simplify it.

### 5.2 Toylang (`toylangc/src/`)

- **`sidecar.rs`** — Phase 1 S.1 + S.2 (already written). Read it.
  Contains `SidecarHeader`, `serialize_sidecar`, `deserialize_sidecar`,
  14 unit tests.
- **`toylang/callbacks_impl.rs`** — toylang's implementation of the facade
  trait. This is where most of Phase 1 Workstream A's work lands.
  Specifically: invert `is_downstream_of_stubs`, change `after_rust_analysis`
  to read upstream sidecars + populate registry from union, change
  `generate_and_compile` gating.
- **`toylang/registry.rs`** — `ToylangRegistry` + `ToyStruct` + `ToyFunction`
  etc. Phase 1 S.2 already promoted `HashMap` → `BTreeMap` for determinism;
  any new types you add here must derive `Serialize, Deserialize`.
- **`toylang/parser.rs`** — Phase 2 C.1's main work site (`impl` block
  parsing).
- **`toylang/ast.rs`, `toylang/typed_ast.rs`** — Phase 2 C.2's main work
  site (`ToyImpl` AST node, threading `ResolvedType::TypeParam` and friends
  through).
- **`oracle.rs`** — the rustc-querying helpers. Phase 1 B's site is
  `rust_trait_method_return_type` / `_param_types` at lines 665-768.
  Phase 3 E.4's site is `resolve_rust_path` at lines 117-124.
- **`build.rs`** — the orchestration that generates `.toylang-build/`.
  Phase 1 D.2 will add a `rust_caller.rs` copy step here. Phase 3 E.3 will
  add the multi-crate fan-out.
- **`manifest.rs`** — toylang.toml schema. Phase 1 D.1 adds
  `project.rust_caller`. Phase 3 E.2 adds `[[dependencies]]`.
- **`stub_gen.rs`** — generates the Rust stub source for `__lang_stubs/src/lib.rs`.
  Phase 2 C.4 adds `impl ::std::clone::Clone for #ident` block emission.
  Phase 3 E.1 will add the `pub const __SKY_STUBS_MARKER` (if not already).
- **`main.rs`** — entry point with two modes: orchestration (`toylangc build`)
  and wrapper-mode (rustc-driver). Phase 1 A.1 / A.2 touches
  `run_toylang_compile` here.
- **`llvm_gen.rs`** — large file. Toylang's LLVM IR emission. Phase 2 C.5
  adds trait-impl method codegen.

### 5.3 Reference materials

- **`rust-interop-architecture.md`** (repo root) — Sky's architecture doc,
  5,148 lines. The master reference for "what are we building toward?"
- **`course-correct.md`** (repo root) — the 18-item catalog of erw → Sky
  divergences. Items #6, #11, #15, #16 are the ones this plan touches.
- **`docs/architecture/sidecar-format.md`** — the spec for the file format
  you're implementing.
- **`docs/architecture/rust-interop-guide.md`** — erw's current architecture
  guide. Outdated in some places (still describes Approach B in spots),
  but the cross-cutting invariants section (@SyMINCZ, @ELASZ, @GCMLZ,
  @ACRTFDZ, @TCHAPZ, @RTMEIZ, @UTAIRZ, @MBMRVZ, @IVTDBTZ, @TVIMDGAZ,
  @ETASTZ, @DPSFDOZ) is still load-bearing. Read those — they're traps.
- **`docs/historical/handoff-optimized-mir-migration.md`** — the
  forward-direction (A→B) handoff. Read it in reverse to understand what
  the W1-W3 work undid.
- **`docs/historical/approach-a-reference/`** — extracted snapshots of
  Approach A's implementation before stage 3 retired it.
- **`docs/historical/rebuilding-rustc-fork.md`** — procedure for rebuilding
  `~/rust`'s fork. If you need to bump nightly, read this.

---

## 6. Discipline and conventions

Read `CLAUDE.md` (both user-global and project) in full. The
non-negotiable rules:

- **No `cd && cargo`.** Use `cargo --manifest-path /absolute/path/Cargo.toml`.
  `cd` is OK only when the user explicitly asks.
- **Don't pivot unilaterally.** If you discover the plan won't work as
  written, STOP and ask the user before changing direction.
- **Don't make temporary debug programs.** If you need a probe, add a test
  case. The probe pattern I used in this session: add an `eprintln!` with
  `[PROBE]` prefix, run, remove. NEVER write a `tmp_debug.rs`.
- **Don't use `git checkout -- file` to revert.** Use `git diff` then apply
  the diff in reverse manually.
- **Always pipe build/test output to a fixed tmp file per session.** This
  session used `./tmp/quarter-of-work.txt`. Don't rotate file names. Don't
  chain `cargo test | tail`. Run the test, then grep the file separately.
- **Use relative paths in `cargo` commands.** Not `/Volumes/V/...` or
  `/Users/verdagon/...`.

The non-obvious rule from this session:
- **`integration-projects-cache` stale-cache gotcha.** When integration
  tests fail in seemingly random ways (callback log empty, expected
  callbacks missing, etc.), wipe the shared cargo cache first:
  `rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache`.
  Then re-run. This is a real source of false negatives and ate me ~30
  minutes in Session 3.

---

## 7. Where to start

Phase 1's Workstream S, oracle cross-crate sweep, and Workstream A all
landed in Session 4. The next-step menu has multiple viable entries
depending on your time budget:

### Option A: Phase 3 (multi-crate) — the architecturally important next move

**Recommended.** This is where toylang's shape LITERALLY matches Sky's
multi-library shape rather than rehearsing it (see architecture doc §6.1).
Course-correct items #6 (`__SKY_STUBS_MARKER`) and #16 (per-Sky-library
stub rlibs) land here.

Estimate: 2–3 weeks of focused work. Sub-tasks per the master plan:

- **E.1** (~1–2 days): Replace `is_from_lang_stubs` (crate-name match)
  with marker-based detection (`__SKY_STUBS_MARKER` at crate root). Land
  first; confirm 228 existing tests still pass before any other change.
- **E.2** (~1 day): Extend `toylang.toml` schema with `[[dependencies]]`
  for cross-toylang-crate deps.
- **E.3** (~2–3 days): Build orchestration — detect transitive Sky deps,
  fan out stub_gen per library, link all stub rlibs.
- **E.4** (~1–2 days): Oracle cross-crate walk generalization — already
  partial (S.4 sidecar load + oracle sweep). Extend to multiple
  upstream Sky crates.
- **E.5** (~1–2 days): Sky-side cross-crate name resolution at typecheck.
- **E.6** (~3–5 days): Multi-crate test fixtures exercising Sky's
  hard-case taxonomy (real lib + real bin + real sidecar load between).

### Option B: Workstream B (oracle TypeParam tolerance) — quick win

~60 LOC. `oracle::rust_trait_method_return_type` and `_param_types` panic
on `TypeParam` Self today. Make them return a defer sentinel instead so
the per-Instance substituted pass can resolve later. Doesn't unblock A
(already done) but is small + clean.

### Option C: Workstream D (rust_caller fixtures) — Case 1b/3/5 tests

~1 week. Adds `project.rust_caller: Option<String>` to `toylang.toml`,
copies a Rust caller source into `user_bin/src/`, exposes `case_1b_*`,
`case_3_*`, `case_5_*` fixtures with per-firing `args_fingerprint`
assertions. Exercises the per_instance_mir Approach A path more sharply
than today's test corpus.

### Option D: A.5 (byte-identical pass-through CI)

Build a small Rust corpus with both vanilla nightly and `rustc-fork`,
compare outputs. CI infrastructure work; ~1 week including corpus
selection.

### Option E: small cleanup pass

Dead-code warnings (`compute_layout_size_align`,
`walk_and_stash_internal_callees` if its callers are inlined), doc
updates to `docs/architecture/rust-interop-guide.md` reflecting the
codegen-at-binary architecture.

### My recommendation

**A → B → D in sequence**, with Phase 2 (impl Trait) deferred to last.
Phase 3 (Option A) is where the real Sky alignment happens and it's the
architecturally biggest unblock. B and D are smaller hardenings that can
interleave with Phase 3 work or fit into shorter sessions.

### Cross-references for the next person

- `workstream-a-scope-notes.md` — Workstream A completion notes,
  including the two key unlocks (oracle sweep, transitive callee walk).
  Worth reading before touching populate_toylang_instances_from_cgus.
- `rust-interop-architecture.md` §§4.5, 6.1, 6.3, 6.5 — the
  marker-based detection model Phase 3 E.1 implements.
- `course-correct.md` items #6 and #16 — the divergences Phase 3 closes.

---

## 9. Operational tips

### 9.1 Running tests

```bash
# Full suite (run with cache wiped):
rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
grep -aE "^test result|FAILED|^running" /Users/verdagon/erw/tmp/quarter-of-work.txt | tail -8

# Just sidecar unit tests:
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --bin toylangc sidecar:: > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1

# Just one integration test:
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --test integration_projects test_diamond_call_pattern > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
```

The session log file `./tmp/quarter-of-work.txt` is fixed — re-use it for
every command in your session per CLAUDE.md's "build & run convention."

### 9.2 Direct toylangc invocation (for probing)

```bash
cargo run --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --quiet -- \
    build /Users/verdagon/erw/toylangc/tests/integration_projects/diamond_call_pattern/toylang.toml \
    > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
```

This bypasses cargo test and runs the binary directly, capturing all
stderr. Useful when you need to see eprintln output that cargo test
swallows.

### 9.3 Rebuilding the rustc fork

If you need to make changes to `~/rust` (the forked rustc), see
`docs/historical/rebuilding-rustc-fork.md`. Five steps; the install
step needs `bash install.sh --prefix=$HOME/rust/build/host/stage2`.
Full rebuild from clean state with CI LLVM = ~15 min.

### 9.4 Sidecar inspection

You don't have a `skyc inspect` tool (deferred per architecture doc
§8.9). If you need to inspect a sidecar's contents during debugging,
write a temporary test in `sidecar.rs::tests` that calls
`deserialize_sidecar` on a known file path and prints the registry.
Don't write a freestanding tool.

---

## 10. Things that will probably bite you

These are real traps. Read them.

1. **Stale incremental cache.** Already covered in §6. Wipe
   `integration-projects-cache` when tests act weird.
2. **Wrapper mode vs build mode.** `toylangc` is BOTH the orchestrator
   AND the rustc wrapper. The same binary runs in both modes. See
   `main.rs::run_wrapper_mode` and `build::build_project`. If you're
   debugging "why isn't my callback firing", first determine WHICH MODE
   the failing invocation is in.
3. **Two rustc invocations per build.** rlib + user_bin. Independent
   processes. State doesn't carry across. The callback log file is shared
   but whoever writes last wins.
4. **`is_downstream_of_stubs` semantics.** TRUE means "this is the
   user_bin compile (not the rlib)". The variable name is awkward.
   Phase 1 A.2 renames it.
5. **Cargo's `.cargo` directory.** Doesn't exist in the workspace; `cargo`
   uses `$CARGO_HOME` (typically `~/.cargo/`) for the registry cache.
   When toylang fixtures depend on path-based test_helpers, watch that
   the `path = "../test_helpers"` works in the generated `.toylang-build/`
   workspace.
6. **`tcx.output_filenames(())` vs `tcx.output_filenames(LOCAL_CRATE.into())`.**
   The query key is `()`, not a CrateNum. Easy to get wrong.
7. **`@-arcana` invariants.** The `docs/architecture/rust-interop-guide.md`
   has a section on cross-cutting invariants (@SyMINCZ, @ELASZ, etc.).
   The @SyMINCZ one specifically — "computing a symbol name doesn't force
   codegen" — has caught me. If you add a new call to `tcx.symbol_name`
   thinking it'll register the Instance for codegen, that's wrong; only
   `ReifyFnPointer` casts in the per_instance_mir body do that.
8. **Trait-method dispatch keys on trait DefId, not impl DefId.** Per
   @TVIMDGAZ. When you build an Instance for `<MyType as Trait>::method`,
   you use the trait def's method DefId with `[Self=MyType, …]` args, then
   `Instance::expect_resolve` maps to the impl method at runtime.
9. **`instantiate_identity()` requires a comment.** Per the project's
   CLAUDE.md compiler law. `EarlyBinder::instantiate_identity()` is only
   valid for structural inspection. Every call site needs a comment
   explaining why we're not substituting.
10. **The bincode crate version.** This project uses bincode v2, not v1.
    The APIs differ. Use `bincode::serde::encode_to_vec` and
    `bincode::serde::decode_from_slice`, NOT `bincode::serialize` /
    `bincode::deserialize` (those are v1).

---

## 11. Useful git references

```bash
# Last commit before Approach B migration (clean Approach A state):
git show ce437ae

# The A→B cutover (read in reverse to undo):
git show bf770ae

# Previous handoff doc (the A→B forward direction):
docs/historical/handoff-optimized-mir-migration.md
```

The fork lives at `~/rust` on branch `per-instance-mir`. Three patches
currently applied (uncommitted in the working tree — the project
convention is "patches as working tree state"). See the diff with
`cd ~/rust && git diff --stat`.

---

## 12. Status snapshot (where you start)

**Tests passing**: 228/228 (81 unit + 132 integration + 15 standalone)
when run with `integration-projects-cache` wiped.

**Sidecars produced**: yes, ~120 files materialize during a full test run.
The format is bincode + BLAKE3 truncated checksum with a 64-byte fixed
header. S.4's facade-side loader reads them at user-bin compile time;
S.5's determinism test byte-compares two builds.

**Fork state**: `~/rust` on `per-instance-mir` branch, 3 patches applied:
query declaration, collector hook, default-None provider. Rebuilt for
nightly-2026-01-20 / rustc 1.95.0-dev / commit `d940e568`. Installed as
toolchain `rustc-fork`.

**Toolchain pin**: `rust-toolchain.toml` channel = `"rustc-fork"`. Four
sites stay in sync (the toolchain file + `TOYLANG_NIGHTLY` in main.rs +
two test files).

**Codegen architecture**: post-Workstream A — rlib compile produces
rlib + sidecar only (no toylang `.o`). User-bin compile is the
codegen site, driven by registry-driven discovery + transitive callee
walk (NOT the upstream CGU walk, which finds zero stub items at user-bin
time — see `workstream-a-scope-notes.md` for the why).

**Uncommitted changes in `/Users/verdagon/erw`** (everything from
Sessions 3 + 4 — un-staged, un-committed — you'll commit them):

S.1 + S.2 + S.3 (Session 3):
- `docs/architecture/sidecar-format.md` (new)
- `toylangc/Cargo.toml` (added bincode + blake3)
- `toylangc/src/main.rs` (added `mod sidecar;`)
- `toylangc/src/sidecar.rs` (new)
- `toylangc/src/toylang/ast.rs` (serde derives)
- `toylangc/src/toylang/typed_ast.rs` (serde derives)
- `toylangc/src/toylang/registry.rs` (serde derives + BTreeMap)
- `toylangc/src/toylang/parser.rs` (BTreeMap + dropped HashMap import)
- `toylangc/src/toylang/type_resolve.rs` (test code BTreeMap)
- `toylangc/src/toylang/callbacks_impl.rs` (S.3 sidecar write at end of
  after_rust_analysis)

S.4 + S.5 (Session 4 — sidecar load + determinism):
- `rustc-lang-facade/src/lib.rs` (`on_sky_lib_loaded` trait method,
  vtable slot, trampoline, call helper)
- `rustc-lang-facade/src/driver.rs` (`load_upstream_sidecars`, fires
  from `after_analysis` before `call_after_rust_analysis`)
- `toylangc/src/toylang/callbacks_impl.rs` (S.4 trait impl,
  `OnSkyLibLoaded` log variant, `upstream_registries` state field,
  log-write reorder to append mode)
- `toylangc/tests/integration_projects.rs` (`test_s4_sidecar_load_smoke`,
  `test_s5_sidecar_determinism`)

Oracle cross-crate sweep (Session 4):
- `toylangc/src/oracle.rs` (cross-crate fallback in
  `find_extern_fn_def_id`, new `find_extern_fn_in_stub_rlib`,
  `find_stub_fn_in_stub_rlib`, `find_wrapper_fn_def_id` fallback)
- `toylangc/src/toylang/callbacks_impl.rs` (`OracleCrossCrateProbe` log
  variant + probe in user-bin `after_rust_analysis`)
- `toylangc/tests/integration_projects.rs`
  (`test_oracle_cross_crate_extern_fn_lookup`)
- `toylangc/tests/integration_projects/oracle_probe/` (new minimal
  fixture with `toylang.toml` + `main.toylang`)

Workstream A (Session 4 — codegen site moved to user-bin):
- `toylangc/src/main.rs` (`is_user_bin_compile` rename + flipped
  `llvm_paths` allocation)
- `toylangc/src/toylang/callbacks_impl.rs` (`is_user_bin_compile` field,
  `ToylangInstance.stub_def_id` field, `populate_*` registry-driven
  rewrite, inverted validation gate)
- `toylangc/src/llvm_gen.rs` (Instance construction from `stub_def_id`)

Doc updates (Session 4):
- `workstream-a-scope-notes.md` (rewritten as completion notes —
  previous draft documented blockers that are now fixed)
- `tl-handoff.md` (this file, updated through Session 4)

Suggested commit grouping: 6 commits matching the workstream boundaries
(S.1, S.2, S.3, S.4 + S.5 + oracle sweep, Workstream A, doc updates).
Or 4 if you prefer fewer (collapse S.1–S.3 + sidecar-related into one;
oracle sweep + Workstream A into another).

**The plan file**: `/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md`.
Already approved.

---

## 13. When to escalate

Ping the user (don't pivot unilaterally) if:
- Phase 3 (multi-crate) turns out to be > 5 weeks (plan budgets ~2–3).
- The rustc fork needs more patches beyond the existing 3.
- You discover a fundamental architectural problem (e.g., bincode can't
  handle a recursive type cleanly; cargo's target-dir layout makes
  sidecar paths unreliable; marker-based detection doesn't survive a
  rustc API change).
- You hit a test failure you can't explain after wiping the cache and
  trying twice.
- Phase 2's toylang `impl` parser turns out > 2 weeks (we budgeted ~1).
- You're tempted to revert Workstream A. The cross-crate codegen path
  it landed is delicate; revert would be a major architectural
  regression. Talk to the user first.

For routine "this took longer than I estimated" — just keep going. The
plan is approved.

**Specific lesson from Session 4:** the handoff's original sizing for
Workstream A (~2–3 weeks) didn't account for the oracle cross-crate
sweep being half-done. Once that was discovered + finished, A landed in
~2 hours. If a future workstream feels MUCH harder than estimated,
look for half-done stage refactors blocking the obvious path. The
codebase has several of these (course-correct.md catalogues some).

---

## 14. Closing notes

You're inheriting a working baseline at a major checkpoint. The mechanism
is alive: Approach A fires per-Instance, the rustc fork is built and
pinned, the sidecar format is specified and types ship, S.4's facade-side
loader reads upstream sidecars at user-bin compile, the oracle helpers
work cross-crate, and Workstream A's codegen-at-binary architecture is
live. The test suite passes green (228/228) when run fresh.

Architecturally the prototype is now SHAPE-aligned with Sky for the
single-file-program case. The "rehearsal" comment in earlier handoffs
still applies — toylang projects use a 2-cargo-crate split that mirrors
Sky's lib+bin distinction. Phase 3 is where that rehearsal gets replaced
with the LITERAL Sky shape: independent toylang libraries published with
their own sidecars, consumed by independent toylang binaries.

The biggest remaining architectural piece is Phase 3 (multi-crate). It's
the natural next move and lands course-correct items #6 + #16. Estimate
is 2–3 weeks of focused work. Land E.1 (marker-based detection) first
and confirm 228 tests still pass — that's the gating change.

Read the architecture doc. Read course-correct.md. Read
`workstream-a-scope-notes.md`. Then start with §7 of this document.

Good luck. The whole point is to make tests for Cases 1b, 3, 4, 5, 6
exist so future drift back toward Approach B fails loudly. Phase 3's
multi-crate fixtures are what makes those tests possible — until you
have a REAL Sky library (not a rehearsal) you can't exercise the
sharper interop cases. That goal is load-bearing.

— previous engineer
