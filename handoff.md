# Sky Architecture Review (Rounds 1–3, 2026-06-23) — Implementation Handoff

**Source conversation:** the full three-round review exchange that inspired this handoff is archived at [`./convo-with-rustc.md`](./convo-with-rustc.md). This document is the distillation; the conversation is the rationale at full fidelity if you ever need it.

**Read this section first.** Below the major `=====` separator is prior-session history (mostly shipped work) — useful context but lower priority than what's in this section.

This section captures everything that came out of a three-round design-review exchange on `rust-interop-architecture.md` conducted 2026-06-23, plus the implementation work it commits us to. We've closed the review exchange and are pivoting to implementation. Round 4 with the reviewer happens after we have empirical data + implementation surprises in hand.

---

## Recent landings (full implementation arcs in arch §F.18-§F.22 + §7.8)

| Landing | Commit | Tests | Arch |
|---|---|---|---|
| **Sidecar→cache migration** (sidecar emission retired; `.sky-cache` is sole upstream metadata at `target/<triple>/<profile>/deps/`; cache-missing = hard error per §7.6; 7-axis Merkle digest in header; skyc-generated `build.rs` emits per-axis `cargo:rerun-if-*`; closed-source distribution formally out of scope) | (working tree) | 352/0/1 integration + 73 unit + 4 fence | §7 (new), §7.8 design history, §27.2 retired |
| **Drop is just a function** (mono-time drop pass retired; wrapper-emission via `__toylang_drop<T>(x: *mut T) { drop_in_place(x) }`) | `7f8814d` | 451/0/1 | §F.22 |
| **Phase O drift fences** (B24/B25/B26 detection: `drop/fence_b24_field_drop_order/`, `mangler_version_fence.rs`, `instance_kind_coverage_fence.rs`) | `e282924` | 451/0/1 | §25.2 |
| **A1 audit + A2 rename** (`bench4` → `bench4_artifactual_loopfold_only`; resolve_expr audit found no live silent-miscompile of the IntLit class) | `89e3bb1` | 451/0/1 | — |
| **Two-enum split (Option B)** (`SourceType` parser-shape vs `ResolvedType` resolved-shape; `oracle::resolve_source_type` is the chokepoint) | `9796604` | 446/0/1 | §F.21 |
| **Sunny-karp** (eager type-resolve + typed-body cache on `ToylangState` + bare-T drop closure; oracle accepts Param-bearing queries) | `af42c20` | 487/0/1 | §F.20, §19.5 Layer 2 |

Cache migration shipped 2026-06-29; working tree pending commit.

---

## Next thing to do when picking up

**Cache migration is fully shipped end-to-end (2026-06-29).** Working tree is at "Step 4 complete + post-migration arch doc rewrite landed" — see "Recent landings" above. Outstanding items for the next session:

1. **Commit the working tree** if not already committed. Substantial diff: cache.rs + cache_key.rs (new), sidecar.rs deletion, callbacks_impl.rs / driver.rs / lib.rs surgery, arch §7 rewrite, glossary updates, §27.2 retirement, plus 6 fence test files. Status doc at `scratchpad/cache-migration-status.md`.
2. **Send the Vale reply** at `scratchpad/reply-to-vale-FINAL.md` once the user signs off on it.
3. **Pick the next priority** from the candidates listed below. Multiple shipped landings retired their "Next thing to do" successors; the slate is mostly clean.

### Candidate next priorities (no hard ordering)

These are the unaddressed items from prior sessions; pick whichever fits the user's current focus.

- **Phase G** (cdylib backend distribution, ~5-7 days; arch §29.A.cdylib). Retires wrapper-mode in favor of `libsky_backend.so` plus a default rustc-fork codegen backend that loads it. Dev-velocity win (cdylib rebuild = 30s-2min vs full rustc-fork rebuild = 5-15 min).
- **Phase N** (Sky-side recursion safety, ~2-3 days; arch §29.A.sky-recursion). Memoize + depth counter on `walk_and_stash_internal_callees` aligned with `tcx.recursion_limit()`. Forward-defense; no live bug.
- **Phase K** (content-hash const args, §29.A.content-hash-const-args). Unblocks comptime → Rust-visible const generic arg flow. Vacuously deferrable while toylang has no comptime.
- **Bench numbers for Vale reply** — run `toylangc/tests/scripts/run_cache_vs_sidecar_bench.sh` and paste numbers into `scratchpad/reply-to-vale-FINAL.md` before sending.
- **Existing items #4-#8** from the "Previous Next thing to do snapshots" below remain unaddressed.

### Cache migration: full inventory of what shipped

For the full step-by-step accounting (Step 0 prerequisites → Step 4 deletion + cleanup audit + Vale-reply), see `scratchpad/cache-migration-status.md`. Highlights:

- New code: `toylangc/src/cache.rs` (~600 LOC, 16 unit tests), `toylangc/src/cache_key.rs` (~300 LOC, 6 unit tests).
- New fences: `cache_determinism.rs`, `cache_key_axis_fence.rs`, `cache_trait_impl_fence.rs`, `cleanup_audit.rs`, plus the cold-CI bench script `tests/scripts/run_cache_vs_sidecar_bench.sh`.
- Retired: `toylangc/src/sidecar.rs` (file deleted), `on_sky_lib_loaded` trait method + vtable slot + trampoline + call_on_sky_lib_loaded fn (all gone), `OnSkyLibLoaded` log variant, `cache_sidecar_equivalence.rs` + `cache_shadow_mode.rs` test files (vacuous post-Step-4), `test_s5_sidecar_determinism` (deleted), §27.2's planned cross-version migration framework, closed-source distribution future direction.
- Doc updates: arch §6.7, §7 (rewrite), §8 (reframing throughout), §13.4 (single-class comptime), §22.5, §27.2 (retired), §F.7 (audit correction).

### Cache migration cross-references

- Migration plan: `tmp/claude-plan-2026-06-28-bd1a7f89.md`
- Conversation: `tmp/claude-conversation-2026-06-28-bd1a7f89.md`
- Status doc: `scratchpad/cache-migration-status.md`
- Vale reply (send-ready): `scratchpad/reply-to-vale-FINAL.md`
- Validation: workflow run IDs `wf_04092741-fc1` (round 1, 44/50 confirmed), `wf_4fa0af0d-de8` (round 2, 49/50 confirmed)
- Arch: §7 (cache chapter, rewritten), §7.8 (design history), §27.2 (retired), §F.7 (`Any`-layer audit correction), glossary §30

---

## Previous "Next thing to do" snapshots

**Snapshot from 2026-06-25 (post-sunny-karp):** Two candidates, user-pick — Option A (queued round-4-close items, mostly shipped) and Option B (two-enum split). Option B SHIPPED 2026-06-25 (commit `9796604`). Option A items SHIPPED individually (Phase P commit `760b674`, etc.). The post-sunny-karp pickup pool is exhausted apart from the cache arc above and the mechanical cleanup below.

---

## What this handoff is for

You are picking up the transition from "design exchange concluded" to "Sky's actual architecture implemented in the toylang reference + ready for Sky proper."

The review exchange surfaced many architectural improvements over the doc as written. Most of them are NOT yet in the codebase. Many of them have NOT been validated empirically. Some of them reverse decisions that ARE in the codebase. Your job is to:

1. **Build the empirical baseline first** — toylang has zero drop tests today; you need to fix that before doing any of the migration work.
2. **Run the perf bench** — single most important data point that calibrates whether several major architectural decisions actually deliver the UX they're supposed to.
3. **Land the architectural migrations** in dependency order — they touch each other; do them out of order and you'll churn.
4. **Update the doc** to reflect the decisions — the design doc is currently the pre-review state.
5. **Come back with data** — round 4 with the reviewer is the natural next exchange once you have bench numbers + implementation surprises.

The total work is ~6-10 weeks for a focused engineer, weighted heavily toward implementation rather than design.

## Audience

You should be comfortable with:
- Rust at the systems-programming level (lifetimes, traits, generics, FFI, unsafe).
- Rustc internals at the rustc_private level: queries, providers, MIR, mono collector, codegen backends. You don't need to be expert but you need to be able to read the source and follow the dispatch chain.
- LLVM at the API level: contexts, modules, function types, linkage, basic IR construction. Inkwell is what we use; familiarity helps.
- Cargo: workspaces, profiles, build scripts, `RUSTC_WORKSPACE_WRAPPER`.

If you don't have rustc-internals background, plan ~1 week of orientation reading: the architecture doc, the existing facade code under `rustc-lang-facade/src/queries/`, and the toylang reference implementation under `toylangc/src/`.

## Prerequisites — read in this order

1. **`rust-interop-architecture.md`** in full. It's ~5800 lines. Three to five hours. Don't skip — this section assumes you've read it.
2. **The decisions log in this handoff section** — captures EVERY architectural commitment from the review exchange, including ones that reverse what's currently in the doc.
3. **`rustc-lang-facade/src/lib.rs`** — the facade entry point. Most of what you'll touch is plumbed here.
4. **`rustc-lang-facade/src/queries/`** — the query overrides. After elimination of mir_shims and symbol_name, you'll be removing files here; understand what's there first.
5. **`toylangc/src/stub_gen.rs`** — the stub source generator. You'll be modifying this substantially.
6. **`toylangc/src/llvm_gen.rs`** — Sky's LLVM emission. You'll touch this for cdylib and for the bench scaffolding.
7. **`toylangc/tests/integration_projects/`** — sample fixtures. You'll be adding ~10 drop-related fixtures here.

If you're picking up cold from someone else, also skim `tmp/claude-conversation-2026-06-23-836f2993.md` — the full review-exchange transcript. It's ~10,800 lines but the actual content is roughly half that (the user pasted some long articles in for context). The decisions in this handoff are the distillation; the transcript is the rationale at full fidelity if you ever need it.

## Status as of writing (refreshed 2026-06-25)

See the Decisions status table in the Decisions log + the Implementation backlog section for the canonical SHIPPED / OPEN status of every decision and phase. Headline test count: **451 / 0 / 1 cold**.

Open work the user wants: Phase G (cdylib + wrapper-mode retirement, engineering velocity) and Phase N (recursion safety). Phases J/K/L/M (Sky language design) explicitly out of scope per user. Thread B (share_generics support boundary) open but lowest priority.

---

## TL;DR

17 architectural decisions logged below; 11 SHIPPED (Decisions 1, 2, 3, 5, 14, 15, 16 + sunny-karp + two-enum split + drop-is-just-a-function migration + Phase O drift fences), 7 OPEN (Decisions 4, 6-12, 13, 17 — most in scope-skip territory per user). Current state: **451 / 0 / 1 tests cold**, 5 commits ahead of origin/main.

**Next session pickup (NEW, queued 2026-06-28):** sidecar→local-sibling-cache migration. Sidecars retire; cache file lives at `target/<triple>/<profile>/deps/lib<crate>-<hash>.sky-cache`. 10 decisions locked, 2 adversarial validation rounds (44/50 + 49/50 confirmed). No prerequisites in open-phase list. Subsumes pre-existing Item 3. ~2-4 weeks. **See "Next thing to do when picking up" near the top of this doc for the full spec.**

State of the World's decision tree shows the full open/closed map; the Implementation backlog has full specs for the few open phases (G/J/K/L/M/N).

---

## State of the World

Bench numbers + reproduction in arch §22.4 + §22.4.2 + §22.4.3. Sky vs Rust delta at thin LTO: 0.3% (Bench 1) / 3% (Bench 3). Bench 3's 25.5× LTO speedup is inherited from Rust's 27.5× cross-crate baseline, not Sky-specific.

**Open design questions** (Decisions 6-12 + 17 — see Decisions log below for full specs):
- 6 (u128 typeids) → Phase J. 7 (content-hash const args) → Phase K, depends on J. 8 (per-view ref types `SkyRef<T, V>`) → Phase L, depends on K. 9 (closed V set) → subset of L. 10 (async typestate) → Phase M, depends on L. 11 (strict_linear + opt-out) → subset of M. 12 (narrowed may_dangle) → subset of L/M. 17 (stub-type contract) → vacuously satisfied today.

**User-priority context:** Phases J/K/L/M (Sky language design) explicitly out of scope. Phase G (cdylib) + Phase N (recursion safety) eventually wanted.

### What's left — at-a-glance decision tree

User's stated priority: **rustc-integration quality, especially perf/inlining**. Round-4-close-driven rustc-integration tasks closed 2026-06-25. Sunny-karp (eager type-resolve + cache + bare-T drop closure) shipped same day.

```
Status →
├── 🆕 Sidecar→local-sibling-cache migration (NEXT, queued 2026-06-28)
│   ├── Sidecars retire; cache at target/<triple>/<profile>/deps/lib<crate>-<hash>.sky-cache
│   ├── 10 decisions locked (see "Next thing to do" near top of doc)
│   ├── 2 adversarial validation rounds: 44/50 + 49/50 findings confirmed
│   ├── Migration: 4 steps, conservative with rollback + explicit cleanup discipline
│   ├── 5 CI fences ship with prototype (axis mutation, determinism, shadow-mode, private-impl, cold-CI timing)
│   ├── Subsumes pre-existing Item 3 (Cargo fingerprint sidecar blindness)
│   ├── Prerequisites: NONE in open-phase list (Phase K soft-dep vacuously satisfied — no comptime in toylang)
│   └── ~2-4 weeks; comparable to sunny-karp + two-enum split combined
│
├── rustc-integration (USER PRIORITY) — DONE earlier sessions
│   ├── ✅ Phase P: deduce_param_attrs soundness override (commit 760b674)
│   ├── ✅ Phase R Site #8: sret-bridge defensive fix (commit 04e98c7)
│   ├── ⚰️ Phase Q: codegen_fn_attrs NEVER_UNWIND override (shipped 736eeb5 then RETIRED f88aa84)
│   ├── ✅ Phase R Sites #1/#5/#6/#10: defensive ABI-coercion fixes (commit 52ee0a3)
│   ├── ✅ Bool accessor silent miscompile FIX (commit 736eeb5)
│   └── ✅ Bench 4 LargeStruct (commit d9248c7) — artifactual; Bench 4b (ae014d0) is the meaningful measurement: 20-run trimmed-mean Sky-Rust parity at 2% delta within noise
│
├── ✅ Sunny-karp (eager type-resolve + typed-body cache + bare-T drop closure) — DONE 2026-06-25
│   ├── ✅ A: oracle accepts Param-bearing queries (6 early-returns deleted; TypeParam → ty::Ty::new_param)
│   ├── ✅ B: typed-body cache on ToylangState.typed_bodies (pivoted from ToyFunction per &self callback shape)
│   ├── ✅ C: substitute_in_typed_body pure walk (chains resolve_struct_fields for StructRef→Struct promotion)
│   ├── ✅ D: mono path substitutes cached typed AST instead of re-resolving
│   ├── ✅ E: drop_synthesized flag + insert_late_scope_end_drops closes bare-TypeParam drop gap
│   ├── ✅ F: collect_rust_deps_recursive + walk_and_stash + codegen_internal_function all read cached body
│   └── Fixtures: drop/fixture10_generic_consume_t_with_drop + fixture11 (negative). 487/0/1.
│
├── ✅ Doc sweep — COMPLETED 2026-06-25
│   ├── §22.4 reframed to lead with Sky-Rust parity (commit 8f85622 + 33aeae4)
│   ├── §25.2 B10 rewrite (commit 33aeae4) — "was Sky's bug, not LLVM's" framing
│   ├── §15.7 dual-path drop narrative paragraph (commit 8f85622)
│   ├── Lifecycle-traits registry (commit 8f85622) — generalizes "Drop" hardcoding
│   ├── §25.3.6 calibration discipline (commit f88aa84) — 4-bugs-with-rationalization-priors pattern
│   ├── §22.4 v2 path-b note (commit f88aa84) — preserves v2 perf-recovery option
│   └── §19.5 memoization status update + §F sunny-karp lesson — sunny-karp post-landing sweep
│
├── Type-system hardening (queued, optional — NEXT SESSION candidate)
│   └── Two-enum split (SourceType vs ResolvedType) — closes the silent-miscompile class
│       that sunny-karp's surprise #3 was one instance of. ~600-800 lines, 11 files,
│       roughly 2x sunny-karp. See "Option B" near the top of this doc for the sketch.
│
├── Operational / drift hygiene (medium priority — NEXT SESSION candidate)
│   ├── Phase N (recursion safety, ~2-3 days)
│   └── Phase O (drift CI fences for B24/B25/B26, ~3-5 days)
│
├── Engineering velocity (lower priority unless blocking)
│   └── Phase G (cdylib + wrapper-mode retirement, ~5-7 days)
│       └── ~10× iteration speed for backend changes; not perf
│
├── Mechanical cleanup (cheap, anytime)
│   ├── Delete RustTypeLookupContext::DeferredTypeParam variant (no constructors after sunny-karp)
│   ├── Delete oracle::contains_type_param (no callers after sunny-karp)
│   └── Delete or revisit UnresolvedRustType::is_deferred() + TypeResolveError::RustTypeDeferred
│       (kept for forward-compat with any future legacy-defer producer; not exercised today)
│
└── Sky language design (USER WANTS TO SKIP)
    └── Phases J/K/L/M — u128 typeids → content-hash → SkyRef → async typestate
```

**Bench 4 finding (resolved 2026-06-25):** Original Bench 4's "Sky matches inlineable Rust at 0.1% delta" finding was artifactual — disassembly inspection confirmed LLVM completely inlined and constant-folded the inner loop. Bench 4b (with `std::hint::black_box` defeating the fold + post-IntLit-widening-fix) gives the meaningful number: **Sky inner loop is byte-identical to inlineable Rust's; 20-run trimmed-mean comparison shows Sky 6505μs vs Rust 6658μs (Sky ~2% faster, within run-to-run variance).** Path-b conclusion is now robust: Sky's cross-crate boundary cost equals inlineable Rust at thin LTO for shapes LLVM can inline through. The pre-fix 38% gap was entirely the IntLit widening silent miscompile, not the wrapper boundary itself. Phase P's conservative `&[]` override is sufficient for v1.

**⚠️ IMPORTANT — read before next session:** Of 9 verification gaps from this session's defensive correctness work, 5 closed via audit/IR-inspection + 4 remain genuinely blocked behind toylang grammar growth. **Three silent-miscompile bug fixes were verified end-to-end** (bool accessor, IntLit widening, B10 round-4). Bench 4b 20-run verification established Sky-vs-Rust wrapper-boundary parity (path-b emission unnecessary for v1). Phase Q shipped-then-retired same day per reviewer's audit (cargo enforces panic-strategy consistency; mixed case structurally impossible). Of the four continuation follow-ups (A1-A4), A3 + A4 closed; A1 + A2 remain open and grammar-unblocked. See "Session 2026-06-25 verification gaps + suspected issues" below for the full state.

**Recommended ordering for next fresh-context session (refreshed 2026-06-28 post-Vale-sidecar-exchange):**
1. **Mechanical cleanup** (10 min): delete `RustTypeLookupContext::DeferredTypeParam` variant + `oracle::contains_type_param` (no callers after sunny-karp). Frees up dead-code warnings. Optionally retire `UnresolvedRustType::is_deferred()` + `TypeResolveError::RustTypeDeferred`.
2. **Sidecar→local-sibling-cache migration** (~2-4 weeks). The big new arc. See "Next thing to do when picking up" near top of doc for the full spec. Subsumes pre-existing Item 3.
3. **Phase G** (cdylib + wrapper-mode retirement, engineering velocity) OR **Phase N** (recursion safety, operational hygiene) — user pick once cache migration is in.
4. Re-probe Phase R deferred items when toylang grammar growth makes their triggers reachable.

**Historical orderings (kept for reference):**
- 2026-06-25 (post-sunny-karp): mechanical cleanup → two-enum split OR Phase O fences → Phase G. Two-enum split SHIPPED 2026-06-25 (commit `9796604`). Phase O fences SHIPPED 2026-06-25 (commit `e282924`). Drop-is-just-a-function migration SHIPPED 2026-06-25 (commit `7f8814d`). A1 audit, A2 rename, A3 Phase Q retire, doc sweep all closed 2026-06-25.

---

## The review-exchange story

Three rounds across one focused day produced 17 architectural decisions (see Decisions log below). Reviewer wants bench data before further design discussion. Detailed narrative archived in `tmp/claude-conversation-2026-06-23-836f2993.md` (~10,800 lines). Round 4 closed 2026-06-24; round 5 awaits Phase L or Phase G.

---

## Decisions log

For OPEN decisions: full **WHAT** / **WHY** / **ALTERNATIVES CONSIDERED** / **IMPLEMENTATION NOTES** / **GOTCHAS** / **DOC IMPACT** sections preserved below — load-bearing for whoever picks up the open work.

### Status table

| # | Decision | Status | Phase |
|---|----------|--------|-------|
| 1 | Eliminate `mir_shims` override | **SHIPPED** | A + E (arch §F.18, later superseded by §F.22) |
| 2 | Eliminate `symbol_name` override | **SHIPPED** | F (arch §6.2, §26.1) |
| 3 | `#[skyc::emit_consumer_body]` tool attribute | **SHIPPED** | C+D (arch §5.3) |
| 4 | cdylib for Sky's backend | **OPEN** | G (~5-7 days) |
| 5 | `#[repr(C)]` FFI for patch 4 rev 3 | **SHIPPED** | H (arch §3.2 patch 4, §F.15) |
| 6 | u128 typeids + collision detection | **OPEN** | J (~2-3 days) |
| 7 | Content-hash const args | **OPEN** | K (~1 week, depends on J) |
| 8 | Per-view ref types `SkyRef<T, V>` | **OPEN** | L (~2 weeks, depends on K) |
| 9 | Closed V set in Sky stdlib | **OPEN** | Subset of L |
| 10 | Async typestate pattern | **OPEN** | M (~3-5 days, depends on L) |
| 11 | `strict_linear` + `#[rust_droppable]` opt-out | **OPEN** | Subset of M |
| 12 | Narrowed `#[may_dangle]` syntactic rule | **OPEN** | Subset of L/M |
| 13 | Sky-side recursion limit alignment | **OPEN** | N (~2-3 days) |
| 14 | `cache_on_disk_if(false)` audit | **AUDIT SHIPPED** | I (arch §22.4.1) |
| 15 | Drift-observation + B24/B25/B26 | **SHIPPED** | O (arch §25.2) |
| 16 | §1.7 reframe — backend pluralism leads | **DOC-PENDING** | doc-only |
| 17 | Stub-type contract | **OPEN (vacuous today)** | Subset of L |

Decisions 1, 2, 3, 5, 14, 15 SHIPPED — full implementation arcs in arch §F.18-§F.22. Decision 16 (§1.7 backend-pluralism reframe) shipped 2026-06-25. **Open decisions 4, 6-13, 17 have been migrated to arch §29.A (WIP Design Directions from the 2026-06-23 Review Exchange)** — see §29.A.cdylib, §29.A.u128-typeids, §29.A.content-hash-const-args, §29.A.skyref, §29.A.closed-V, §29.A.async-typestate, §29.A.strict-linear, §29.A.may-dangle, §29.A.sky-recursion, §29.A.stub-contract. Each is clearly marked WIP and points to the conversation log for full historical context.


## Empirical work backlog (DO FIRST, before any migration)

### The empirical-vacuum context

**Toylang has ZERO drop tests today.** Verified during round-3 follow-up Q2:
- `drop_glue.rs` mir_shims override exists with a debug eprintln; nothing exercises it (no eprintln output appears in any test run).
- Toylangc never emits `__toylang_drop_*` symbols.
- Stub_gen never emits `impl Drop for ...` blocks.
- No fixture in `tests/integration_projects/` has "drop", "linear", "destruct", "cleanup", or "finalize" in the name.
- The historical `test_point_drop` fixture was deleted in 5c.4; nothing replaced it.

This means: the mir_shims elimination's "current behavior" baseline is UNVERIFIED. Without an empirical baseline, the elimination is unverified against a behavior we never observed.

**Implication: build fixtures FIRST, validate baseline under CURRENT mir_shims model, THEN do the migration with fixtures as the regression suite.**

User confirmed: "we need to prototype all these things before handing anything off to sky. we need excellent test coverage. toylangc will exist even alongside sky well into the future, as a reference implementation that will help us narrow down bugs."

### Fixtures built (Phase A + later) — SHIPPED

The 9 drop fixtures plus 2 sunny-karp bare-T fixtures plus 1 B24 fence ship under `toylangc/tests/integration_projects/drop/`. Pre-shipping specs removed; see git history if needed.

### Other tests/fences to build

**Test A: multiple ReifyFnPointer casts (mentioned-items safety).**
Fixture: Sky function with body that casts to multiple Rust deps (Vec::new, Vec::push, Vec::clear, Vec::drop — 4 distinct casts).
Assertions:
- `llvm-objdump -t` on resulting binary shows symbols for all 4 mono'd Rust deps.
- Binary runs without undefined-symbol errors at link.
- Sky's body in LLVM IR has all 4 casts in the right form.

**Test B: Rust dep ONLY reachable through Sky's body.**
Fixture: a Rust dep that no Rust source references directly; only Sky's per_instance_mir body references it via ReifyFnPointer cast.
Assertions:
- Binary's symbol table includes the dep's mono'd Instance.
- Function works at runtime (if reachable through the Sky path).
Tests end-to-end dep-registration mechanism.

**Cache-correctness fence (edit-and-rebuild).**
1. Build a project with `Widget { x: I32, y: I32 }` (16-byte struct).
2. Edit Sky source: add `z: I32` (now 24 bytes).
3. Rebuild WITHOUT cache wipe.
4. Verify built binary uses NEW layout (24 bytes), not cached old layout (16 bytes).
Catches Decision 14's failure mode.

**Cross-crate symbol-name test (B25 drift detection).**
Build a project that exercises cross-crate calls between Sky-emitted code and rustc-compiled call sites. Build at multiple `-Csymbol-mangling-version=` settings (v0 default + any new defaults). Verify link succeeds at each.

**Drop-instance-kind coverage (B26 drift detection).**
Integration tests exercising drop for every Sky type category (linear, async-migratory, async-default, generic, with `#[may_dangle]`, etc.). Verify Sky's `__sky_drop_X` is observed running (via instrumentation counters or side-effects).

**Architecture-fence CI (NNGZ).**
Per §26.15 (Non-generic is the Normal-case-of-Generic): grep-based CI test walks Sky's frontend source for `type_params.is_empty()` patterns, asserts each is annotated `arch-fence-allow: <reason>`. Unannotated occurrences fail the test.

**Byte-identical pass-through corpus.**
Per §25.3.5: corpus of representative Rust crates (small bin, serde-derive consumer, tokio program, generic-heavy, trait-heavy, sys-crate wrapper). For each: build with vanilla nightly rustc + with Sky's rustc (Sky machinery dormant). Byte-compare output objects. Any divergence is a regression.

### The perf bench (PRIORITIZED FIRST ARTIFACT) — DONE 2026-06-24

Bench fixtures live under `toylangc/tests/integration_projects/perf_bench/`; runner at `toylangc/tests/scripts/run_perf_bench.sh`; reproduction in arch §22.4.2; results in §22.4. Phase B/B+ outcome summaries below.

---

## Implementation backlog

Shipped phases A/B/B+/C/D/E/F/H/I/O/P/Q/R: full implementation arcs + lessons in arch §F.18-§F.22 + perf model in §22.4. Open phases (G, J-N) below.

### Phase G: cdylib build system (3-5 days)

Restructure Sky's toolchain to ship as paired rustc-fork + libsky_backend.so (Decision 4).

Tasks:
- Restructure `rustc-lang-facade` to compile as cdylib.
- Modify rustc-fork to load libsky_backend.so via the `CodegenBackend` plugin mechanism (`-Zcodegen-backend=sky` baked into default config).
- Build runtime version handshake at backend load.
- Update toolchain bundle structure (Sky toolchain ships rustc-fork + libsky_backend.so + skyc + cargo + LLVM shared libs).
- Update build scripts for the cdylib model.

**Output**: Sky's backend loadable as cdylib; fast iteration loop unlocked for further work.

### Phase J: u128 typeids + collision detection (2-3 days)

Migrate typeids from u64 to u128 with universe-level collision detection (Decision 6).

Tasks:
- Change Sky's typeid type to u128.
- Update `SkyOpaqueType<const T: u64>` → `SkyOpaqueType<const T: u128>` in Sky stdlib stub source.
- Update universe table key type and add collision-detection on insertion.
- Update typeid hashing (BLAKE3 truncated to 16 bytes).
- Update all existing typeid plumbing (sidecar serialization, stub_gen, comptime-recipe encoding).
- Build collision-error message with clear diagnostic.

**Output**: typeids are u128 throughout; collision detection in place.

### Phase K: Content-hash const args (1 week, depends on J)

Retire slab-pointer-as-u64; replace with content-hash const args (Decision 7).

Tasks:
- Sky's frontend, at comptime-arg binding, computes BLAKE3 of the (frozen) snapshot content, truncates to u128, synthesizes `ConstKind::Value(u128_bytes)`.
- Stub source signatures for Sky generic fns with comptime args use `const T: u128`.
- per_instance_mir looks up values in Sky's universe via the content hash.
- Sky's typechecker rejects `ptr_eq` on generic-arg references (Approach A from round-2 Q16).
- Update §13.3 design doc and any code comments referencing the slab-pointer trick.

**Output**: comptime args are content-hashes; slab is purely Sky-internal; cross-invocation symbol matching works automatically.

### Phase L: Per-view ref types (2 weeks, depends on K)

Introduce `SkyRef<T, V>` with View markers (Decision 8). Closed V set (Decision 9).

Tasks:
- Sky stdlib defines `Frozen`, `Mutable`, `SkyRef<T, V>` with conditional Send/Sync impls.
- Sky's typechecker tracks group views at every `&` site.
- Sky's stub_gen emits parametric `SkyRef<T, V>` for Sky references. Picks View at each call site based on Sky's typechecker analysis.
- Methods emit as one parametric impl per Sky type (`impl<V> SkyRef<T, V> { ... }`). Specialized impls only when behavior differs per view.
- Owned values: per-type honest Send computation.

**Output**: Send/'static honesty at the Rust boundary; tokio interop fallout deferred to bridge crate work.

### Phase M: Async typestate refinement (3-5 days, depends on L)

Refactor async state machine emission per Decision 10.

Tasks:
- Sky's stub_gen emits ONE struct per Sky async fn (storage sized for both phases).
- IntoFuture impl on the type handles polling in either phase.
- Sky source typechecker tracks NotStarted vs Running typestate.
- Pin/Unpin declared per migratory/default on the rustc-level type.
- Sky drop function for state machines dispatches on phase discriminant.

**Output**: async typestate pattern in place; Fixtures 6-9 should pass.

### Phase N: Sky-side recursion handling (2-3 days)

Add memoization + depth counter to `walk_and_stash_internal_callees` (Decision 13).

Tasks:
- Add visited-set memoization to the walker.
- Add depth counter aligned with `tcx.recursion_limit()`.
- Add diagnostic-attribution logic (walk back to user-source entry point on overflow).
- Build a fixture: pathologically-deep Sky-internal recursion; assert clean compile-time error (not crash).

**Output**: Sky-side walkers safe from runaway recursion.

### Total estimate for remaining open phases

- Phase G (cdylib + wrapper-mode retirement): 5-7 days.
- Phase J (u128 typeids): 2-3 days.
- Phase K (content-hash const args, depends on J): 1 week.
- Phase L (per-view ref types, depends on K): 2 weeks.
- Phase M (async typestate, depends on L): 3-5 days.
- Phase N (Sky-side recursion): 2-3 days.

User has stated J/K/L/M as out of scope (Sky language design, not rustc integration). Phase G is engineering-velocity only; Phase N is operational hygiene.

---

## Doc update plan (pending arch-doc edits)

All doc-only items shipped 2026-06-25: §1.7 (backend-pluralism reframe), §1.2 + §11.1 (groups are primarily compile-time scope; arena allocation is separate concern), §3.1 (per-Instance dep enumeration is load-bearing reason; comptime is secondary), §6.6.5 (`#[inline(never)]` vs `@llvm.used` comparison table). §26 NNGZ enforcement note already covered in §26.15 + §1.5.5.

Phase-gated updates pending (rewrite when corresponding phase ships):
- §4, §29.6 — Phase G (cdylib model)
- §10, §12 — Phase L (per-view ref types)
- §13.3/§13.4/§13.7-9 — Phases J/K (u128 typeids + content-hash const args)
- §14, §17 — Phase M (async typestate + tokio interop)
- §19 safety-properties subsection — Phase N (recursion safety)

---

## Session 2026-06-25 — verification gaps + suspected issues

This session shipped Phase P, Phase Q (then retired same day per reviewer's audit), Phase R (Sites #1/#5/#6/#8/#10), Bench 4, the bool accessor fix, and the IntLit widening fix. Of the original 9 verification gaps tracked: 5 closed via audit/inspection, 4 remain genuinely blocked behind toylang grammar growth (those 4 are the Phase R defensive fixes whose triggers require i8/i16, trait impls on slices, or f32/SIMD). The follow-ups discovered during the continuation (A1-A4): A3 + A4 closed; A1 + A2 remain unblocked-open.

### Verification gaps (defensive fixes lacking trigger-condition tests)

1. ~~Phase P override verification~~ — **resolved via IR inspection 2026-06-25.** The existing `bench4_artifactual_loopfold_only` fixture (renamed 2026-06-25 from `bench4_largestruct_byval_thin` per A2) exercises the override end-to-end (Rust caller invokes Sky exports `make_large(i64) -> LargeStruct` and `first_field(LargeStruct) -> i64`, both with indirect-passed params). IR inspection confirms the override fires: Sky's tagged-item declarations show `captures(address)` (default for `&T`/sret) NOT `captures(none)` (which `deduce_param_attrs` would have added), and NO `readonly` attr. Verification details captured in `rustc-lang-facade/src/queries/deduce_param_attrs.rs` header. A true soundness fence (Sky body mutating the indirect param) still requires toylang grammar to grow `&mut T` non-receiver args + field mutation — that follow-up remains open but the override IS verified to suppress the deduced attrs at the LLVM IR layer. **Closes verification gap #1.**

2. ~~Phase Q perf bench~~ — **resolved via interaction audit 2026-06-25 + reviewer's retirement endorsement.** The `codegen_fn_attrs` override was consumed by `rustc_middle::ty::layout::fn_can_unwind` (layout.rs:1233-1257). Same function early-returns false for ALL non-foreign items when `tcx.sess.panic_strategy().unwinds()` is false. Sky's enforced panic=abort posture (§16.1) makes that the case → Phase Q's `NEVER_UNWIND` flag was REDUNDANT under uniform panic=abort. Reviewer's round-4-followup additionally observed that cargo enforces panic-strategy consistency at build-graph resolution, so even the mixed-strategy edge case Phase Q would have helped was structurally impossible within Sky's tooling. **Phase Q retired** 2026-06-25 (commit `f88aa84`); no bench needed because there's no override to bench against. Provenance comment chain pinned in `rustc-lang-facade/src/queries/mod.rs` with DO NOT FLATTEN annotation. **Closes verification gap #2.**

3. **Phase R Site #8 (sret-bridge alloca/load size mismatch, commit `04e98c7`) — no regression probe for the actual trigger.** When my initial bool-struct fixture surfaced the bool accessor bug instead (which I fixed separately), I removed the misleading fixture and committed the defensive fix unverified. Site #8's actual trigger requires struct shapes toylang can't currently express (LLVM-size differs from rustc's ABI-coerced direct return size — typically 3/5/6/7-byte structs requiring `i8`/`i16` field types). **Follow-up:** when toylang grammar grows `i8`/`i16`, write a probe fixture with a 3-byte struct returned by-value, verify build + correct runtime at -O3 + thin LTO.

4. **Phase R Site #1 (`Pair` arm assumes struct source, commit `52ee0a3`) — defensive memory-reinterpret path untested.** Triggered by passing a Sky source value whose toylang type resolves to a non-struct LLVM type into a position where rustc's ABI coerces to `Pair` (e.g., `&Struct` where rustc considers the ref a fat pointer). Currently unreachable in toylang grammar (thin refs are always `Direct`). **Follow-up:** when toylang grammar grows trait impls on slices / unsized types that would surface Pair-receiver coercion, write a probe.

5. **Phase R Sites #5/#6 (receiver Pair dispatch, commit `52ee0a3`) — fix is plausible but unverified.** Dispatches receiver through `push_arg_for_rust_call` when `coerced_params[0]` is `Pair`. For `&[T]` receivers, Sky's `resolved_to_inkwell` returns `{ptr, i64}` struct, so the dispatch should work correctly (`push_arg_for_rust_call::Pair` would extract both scalars). **Edge case I noticed:** for `Vec<T>` receivers (Sky treats Vec by-reference even when declared by-value), if rustc ever coerced a Vec-shaped self to Pair (unlikely), my dispatch would lower the Vec value as struct + extract wrong fields. Currently unreachable; document for the next session. **Follow-up:** if toylang grammar adds trait impls on slices, add a probe with `<[T]>::len(self: &[T])`-shaped call.

6. **Phase R Site #10 (`parse_coerced_type` float/vector arms, commit `52ee0a3`) — fix is straightforward but no toylang surface to trigger.** Added `half`, `float`, `fp128`, and `<N x T>` vector parsing. Toylang grammar only has `f64`. **Follow-up:** when toylang grammar grows `f32`/`f16`/SIMD, write smoke fixtures exercising the new arms.

### Suspected issues (potential bugs I noticed but didn't verify)

7. ~~Bench 4 artifactual~~ — **resolved 2026-06-25 (commits `ae014d0` + post-fix verification).** Confirmed: disassembly inspection showed LLVM completely inlined and constant-folded the original Bench 4's inner loop; the "0.1% delta" was loop-overhead noise. Bench 4b (commit `ae014d0`) adds `std::hint::black_box` between maker + reader to defeat folding. **Critical surprise during IR inspection:** the disassembly revealed Sky was emitting `str wzr` (32-bit zero stores) for `i64` field initializers in struct literals — a silent miscompile from a parser/type-resolver bug where unsuffixed integer literals defaulted to `i32` and weren't widened against the expected field type. Fixed in the same commit (`type_resolve::resolve_expr`'s `IntLit` arm widens i32 → i64/usize against expected_ty; narrowing still errors). Regression fixture at `test_drop_intlit_widening_struct_field`. **Post-fix verification (20-run sample):** Sky inner loop is BYTE-IDENTICAL to inlineable Rust's inner loop (10 instructions, same operations, same registers). Trimmed-mean comparison: Sky 6505μs vs Rust 6658μs over 20 runs each — Sky ~2% faster, within run-to-run variance. The earlier "10% slower" finding from a 5-run sample was small-sample noise. **Path-b conclusion is robust:** Sky's cross-crate boundary cost equals inlineable Rust at thin LTO for shapes LLVM can inline through; the 38% pre-IntLit-fix gap was entirely the silent miscompile, not the wrapper boundary. **Closes verification gap #7 AND found a real silent-miscompile bug AND established Sky-vs-Rust parity at the wrapper boundary for byval-indirect-passed structs.**

8. ~~`static_size_bytes` alignment heuristic~~ — **audited 2026-06-25.** Heuristic `field_align = min(field_size, pointer_bytes)` matches LLVM's actual rules EXACTLY for every type Sky's `resolved_to_inkwell` produces today (`bool`/`i1`, `i32`, `i64`/`usize`/`ptr`, `f64`, structs of those). Known divergences from LLVM for types Sky doesn't emit yet: `i128` fields (LLVM aligns to 16; heuristic clamps to pointer_bytes), array fields in structs (LLVM uses element-alignment), and vector types (heuristic panics). Audit + caveats documented in `static_size_bytes`' doc-comment in `llvm_gen.rs`. **Follow-up:** when toylang grammar grows `i128`, fixed-size byte arrays, or SIMD, re-audit against `TargetData::abi_alignment_of_type`. **Closes verification gap #8 within current Sky scope.**

9. ~~Query override interaction audit~~ — **audited 2026-06-25.** Traced consumers for each Sky override. Findings: (a) Phase Q's `codegen_fn_attrs` (then-active) was consumed by `fn_can_unwind` (layout.rs:1233) — the ONLY downstream consumer for Sky-tagged items. The audit was load-bearing: it surfaced that `fn_can_unwind`'s panic_strategy early-return made Phase Q effectively no-op under uniform panic=abort, leading directly to Phase Q's retirement (see #2 above). (b) `cross_crate_inlinable`, `deduced_param_attrs`, `layout_of`, `mir_inliner_callees`, `mir_callgraph_cyclic` are independent — no cross-feed for Sky-tagged items. Post-Phase-Q-retirement: only 4 distinct active overrides remain, and the interaction surface is now zero (no downstream consumer of any Sky override flows into another query). **Closes verification gap #9.**

### What was actually fully verified this session

- ✅ **B10 round-4 fix** (yesterday, commit `3041ec8`) — 5 regression probes prove correctness.
- ✅ **Bool accessor fix** (commit `736eeb5`) — `test_drop_bool_accessor_via_rust_caller` regression fixture proves correctness.
- ✅ **IntLit widening fix** (commit `ae014d0`, found 2026-06-25 continuation) — `test_drop_intlit_widening_struct_field` regression fixture proves correctness; pre-fix Sky was emitting `store i32 0` to i64 fields silently.
- ✅ **Phase P override** (commit `760b674`) — IR inspection of `bench4_artifactual_loopfold_only` (renamed 2026-06-25 from `bench4_largestruct_byval_thin`) confirms attrs suppressed.
- ✅ **Bench 4b Sky-vs-Rust parity** (post IntLit fix, 20-run sample) — inner loops byte-identical; trimmed-mean Sky 6505μs vs Rust 6658μs.
- ✅ **All commits compile cleanly + pass 350 integration_projects + standalone tests** — no regressions introduced.

Three real silent-miscompile bug fixes this session (yesterday's bool accessor, today's IntLit widening, yesterday's bool i1 accessor) — these would have shipped to users. Defensive correctness fixes (Phase P, Q, R Sites #1/#5/#6/#8/#10) remain unverified at their actual trigger conditions but the override+fix shapes are sound.

### Follow-ups discovered during the continuation session

These are open items found by the verification audit work itself; not blocked by grammar growth:

- **A1. Audit other `resolve_expr` arms for missing `expected_ty` coercion.** The IntLit fix (commit `ae014d0`) added widen-against-expected_ty only to `Expr::IntLit`. Other arms (`BinaryOp`, `FnCall`, `MethodCall`, etc.) might have similar bugs where a sub-expression's actual type doesn't get coerced against the surrounding context's expected type. Concrete example to check: `let x: i64 = 1 + 2` where both literals default to i32 and the BinaryOp result is i32 stored into i64 — would produce `store i32` to i64 slot. **Audit:** search all arms of `resolve_expr` for ignored `expected_ty` and probe each. ~1 hour audit + targeted fixtures.

- **A2. Bench 4 (original, non-`4b` variant) is now misleading.** Original Bench 4 measures loop overhead (LLVM folds the inner loop entirely). Bench 4b is the meaningful measurement. Three options: (a) delete Bench 4 + keep Bench 4b as the canonical measurement; (b) keep both and rename Bench 4 → `bench4_artifactual_loopfold_only` to make the limitation explicit; (c) keep both with cross-references in headers. Recommend (b) so the historical context survives but readers can't accidentally cite the wrong number. ~half hour rename + doc update.

- ~~A3. Phase Q retirement decision~~ — **RESOLVED 2026-06-25 (commit `f88aa84`)**. Reviewer's round-4-followup confirmed retirement is the right call: cargo enforces panic-strategy consistency at build-graph resolution, making the mixed case structurally impossible within Sky's tooling. Phase Q files deleted + cache_audit entry removed + design history preserved in `queries/mod.rs` header. Closes A3 cleanly.

- **A4. The 20-run Bench 4b sample suggests the test_widgets `#[inline]` helpers may not actually be inlining.** Looking at the trimmed means (Sky 6505 vs Rust 6658), Rust was slightly SLOWER than Sky. With byte-identical inner loops, that suggests something outside the loop differs slightly. Cache layout? Function alignment? Not pursuing because the gap is within noise, but worth noting as a curiosity for whoever next looks at perf benches.

---

## Open / deferred (do NOT do now)

These are explicitly NOT in scope for the current implementation arc:

- **Linear-type compile-time error diagnostic spans (v2).** Option 4 path. When implemented, needs real diagnostic-span walk (walk usage_map up to user-source frame). Non-trivial work; defer.
- **SkyOwn<T> wrapper.** Only relevant if v2 lands ultra-strict-linear tier (Decision 11's deferred extension). Skip for v1.
- **AvailableExternally-or-equivalent for non-LTO inlining.** Only revisit if Bench 1 reveals 10x+ slowdown the user isn't willing to accept. Otherwise the "LTO-first perf model" stands.
- **Universe-fingerprint-in-cache-key.** v2 perf optimization that preserves cache benefit while ensuring correctness. v1 uses conservative cache disable (Decision 14).
- **Drop body contract for non-ZST stub fields.** Allowed in principle (Decision 17); not needed for v1's stub_gen pattern. Revisit if Sky's stub-gen evolves.
- **Comptime calling Rust functions.** v2 capability. Sky's design KEEPS THE OPTION OPEN but doesn't implement for v1. Design constraints (per round-2 user decision): sky-side comptime evaluator architecture should NOT preclude future "call Rust function via Miri" extension.
- **Tokio bridge crate.** Sky-stdlib-provided bridge that hides Option C' Send/'static conversions for tokio interop. Real work; defer to when Sky's stdlib + async infrastructure exists.
- **PinnedDrop-style mechanism.** Reviewer explicitly said "do not invent a PinnedDrop-style mechanism for Sky. The pattern you have is correct and well-trodden." Don't.

---

## Pre-existing work not covered by the review exchange

Items from prior sessions (mostly the toylang implementation arc) that the 2026-06-23 review exchange didn't touch. These are forward-looking work; do them alongside (or after) the review-exchange implementation backlog. Each is independent of the others.

### 1. Thread B — share_generics support boundary (~1 day investigation + decision)

The `share_generics` compile-time flag controls whether rustc encodes generic mono Instances into the upstream rlib's rmeta for downstream sharing (true) or has each downstream consumer re-mono locally (false). Cargo's default is `share_generics=true` at debug, `false` at release.

Current Sky-side state (pre-review-exchange):
- Sky's `LangDriver::config` heuristic FORCES `share_generics=true` at every `__lang_stubs`-named crate compile, regardless of user profile.
- Sky HARD-ERRORS if user explicitly sets `share_generics=false` on `__lang_stubs`.
- For non-`__lang_stubs` Sky lib crates (e.g., `case6_lib`): NO Sky-side override; respects whatever the user/cargo sets.
- For pure-Rust crates: NO Sky-side involvement; vanilla rustc behavior.

Open design questions (the original Thread B framing — partially obsoleted by patch 5 retirement 2026-06-22, but the support-matrix question stands):
1. Is the hard-error at `__lang_stubs` + share_generics=false too aggressive? Alternatives: warning + auto-override, error with opt-in escape flag, current hard-error.
2. Should the heuristic ALSO force share_generics=on at non-`__lang_stubs` Sky lib crates (e.g., `case6_lib`)? Current behavior empirically works for case6 fixtures, but it's not clear WHY (might be coincidence based on cascade timing).
3. Should pure-Rust crates compiled via Sky's rustc respect user's explicit share_generics? Current answer: yes (no override fires). Verify with a test.
4. Should we warn if user passes `RUSTFLAGS="-Z share-generics=yes"` on a mixed Sky+pure-Rust project? Forcing share_generics on pure-Rust deps weakens pass-through byte-identity. Nice-to-have, not blocking.
5. Should we test `RUSTFLAGS=-Z share-generics=no` on a user_bin specifically? Should work but verify.

Investigation tasks:
- Build the support matrix as actual fixtures, one per cell. Realistic minimum: 9 fixtures covering `{stub_rlib: default-forced, explicit-on, explicit-off-error} × {user_bin: default, explicit-on, explicit-off} × {pure-Rust dep: default, explicit-on, explicit-off}` (a meaningful subset).
- Decide each cell's behavior. Update `LangDriver::config`'s diagnostic to reflect the supported set.
- Update arch doc §3.2 (or wherever share_generics is currently described — patch 5 retirement may have moved this content) with the support matrix.

Priority: lowest urgency in this list. No current user pain; design-space hygiene work. Defer to after the review-exchange backlog if time-constrained.

### 2. Test gaps from prior recommendations (~1-2 days total)

Several test fixtures were recommended in prior sessions but never landed. They cover edge dimensions the existing test suite doesn't:

**`debug_assertions_off_smoke`** — build at `-C debug-assertions=off` at -O3. Catches MIR-shape changes around `unreachable!` and overflow checks that only surface when debug assertions are disabled.

**`opt_level_z_smoke`** — `-Oz` (size optimization) as a profile distinct from existing matrix coverage. -Oz uses a different optimization preset than -O3 / -O2; some bugs only surface there.

**`lto_off_smoke`** — explicit `lto = "off"` (vs current implicit `lto = false` default). Distinct cargo behavior; validates the toml-parsing path.

**Pass-through invariant assertion: verify pure-Rust crates compiled via Sky's rustc don't get `@llvm.used` global injected.** The SMPLZ pin (`pin_in_llvm_used` in `toylangc/src/llvm_gen.rs`) should be MARKER-GATED — only fire for Sky-marked crates. If it fires for pure-Rust crates, that's a real pass-through violation (the output differs from vanilla rustc's output). Test: build a pure-Rust corpus crate via Sky's rustc; verify the binary has NO `@llvm.used` Sky-related symbols.

**panic=abort enforcement: `LangDriver::config` should error if user sets `panic = "unwind"`.** Arch §16.1 requires `panic = "abort"` exclusively. Today we silently inherit whatever the user has. If they accidentally set `panic = "unwind"`, Sky's no-landing-pads emission collides with the runtime expectation → UB at first panic. Diagnostic should be hard-error at config time with clear message.

Each ~30 minutes to a few hours; net ~1-2 days. None blocking; all independently improve test coverage.

### 3. Cargo fingerprint sidecar blindness (~1 day) — **SUBSUMED 2026-06-28 by the sidecar→local-sibling-cache migration**

**Status: subsumed.** The cache migration's `CACHE_KEY_AXES` + `build.rs` `cargo:rerun-if-changed=` / `rerun-if-env-changed=` mechanism (driven by a single-source-of-truth constant) is structurally what this item's mitigation describes, expanded to cover all 6 cache-key inputs not just sidecar paths. When the cache arc lands, mark this item closed.

Historical text preserved below for archive purposes:

---

SEPARATE from Decision 14's rustc per-query cache discipline. Cargo's fingerprinting is a different layer — cargo decides whether to re-invoke rustc at all based on whether inputs changed. Cargo's standard fingerprint includes source files, dep versions, profile settings. **Cargo does NOT know about Sky's sidecar (`.sky-meta`) files** as part of its fingerprint inputs.

Failure mode: user edits a Sky lib's `.sky` source that changes the lib's sidecar content but NOT its rmeta (e.g., changes a non-export item's body). Cargo sees no change in rustc-visible inputs → skips re-invoking rustc for downstream crates → downstream uses stale Sky-emitted bodies (which were generated from the previous sidecar's universe).

Mitigation: cargo's `links` mechanism or `rerun-if-changed` (via build.rs) can be wired to fingerprint the sidecar. Skyc-generated `build.rs` should emit `cargo:rerun-if-changed=<sidecar path>` for every Sky lib dep.

Verification: a fixture that edits a Sky lib's non-export body, rebuilds without cache wipe, verifies downstream actually re-mono'd the affected Sky generics.

Priority: medium. Not a current user-pain point (Sky toolchain workflow regenerates sidecars on every build), but a real correctness gap in incremental scenarios.

### 4. `deduce_param_attrs` from stub MIR (~half day)

Rustc's `deduce_param_attrs` query analyzes function MIR bodies to infer LLVM parameter attributes (`noalias`, `readonly`, `readnone`, `nocapture`, etc.). These attributes enable LLVM's alias analysis and optimization passes.

For Sky's stub items in the Category B set (post-Decision 3 attribute predicate), the stub body is `unreachable!()`. `deduce_param_attrs` analyzes this empty body and infers... nothing useful. Sky's call sites in Rust source (which use the stub's mangled name) lose param-attr-based optimization.

The real Sky-emitted body (via `fill_extra_modules`) HAS the actual semantics, but it's emitted as LLVM IR directly — `deduce_param_attrs` doesn't run on it.

Fix options:
- Override `deduce_param_attrs` for Sky stubs to return the attrs Sky knows are correct (Sky's typechecker has the info).
- Emit the attrs directly on Sky's LLVM IR functions at codegen time (bypass rustc's deduction entirely for Sky-emitted bodies).
- Accept the perf cost.

Perf impact: probably small for most code (alias analysis at the call-site level is limited without these attrs); could be material for hot paths through Sky APIs. Worth measuring after the main perf bench lands.

Priority: low for correctness, medium for perf-quality. Half-day to implement either fix option once measured.

### 5. Sky-side DWARF emission (~3-6 weeks, significant separate effort)

Sky emits LLVM IR via Inkwell. For debugger usefulness, Sky bodies need DWARF debug info that maps back to Sky source files (line numbers, variable scopes, type info). Today's toylangc emission has minimal-to-no DWARF for Sky bodies.

Architectural commitment (§6.7, §23.3): published Sky libraries ship their `.sky` source alongside artifacts SPECIFICALLY so debug symbols can reference them. The reference is only useful if Sky's emission actually generates DWARF pointing at the source.

Scope:
- Inkwell DWARF API (`DIBuilder`, `DICompileUnit`, `DIFile`, `DIScope`, `DISubprogram`, `DILocalVariable`, `DIType`, etc.).
- Sky source span tracking through Sky's typing pass → Temputs → fill_extra_modules emission.
- Type info for Sky-defined types (mapped to DWARF type entries).
- Variable scopes for Sky source locals.
- Line table for Sky source lines.
- Optimization-friendly debug info (Sky inlines aggressively in some paths; debug info needs to track inlined frames).

Major effort (~3-6 weeks). Not blocking the architecture rollout but significantly improves Sky's UX. Track as known work for Sky proper; toylang may continue without it (minimal debugger usefulness is acceptable for a reference implementation).

Priority: low for v1 architecture; high for Sky proper's user experience.

### 6. Effective-registry merging for cross-Sky-crate dep enumeration (~1 day, low priority)

Course-correct.md item #10 (PARTIAL). Today every Sky-active compile re-derives its dep registry from sidecars + local Temputs. For larger Sky projects with several Sky libs in the build graph, this is O(n²)-ish in the worst case (each lib's compile walks all upstream sidecars).

Optimization: cache a merged Sky universe across libs in a single compile session. Facade-level coordination so each compile pays sidecar-load cost once, not per-Sky-lib.

Not yet investigated rigorously. Probably ~1 day to design + implement once Sky proper or toylangc drives it.

Status under post-review-exchange architecture: lower priority than it was. Compile times haven't been a user pain point and the review-exchange decisions didn't surface this as a bottleneck. Keep on the list but not top-of-stack.

### 7. Wrapper-mode anti-pattern note for Sky proper

Course-correct.md item #13 (REMAINING). Toylangc still uses `RUSTC_WORKSPACE_WRAPPER` wrapper mode (`@MRRIWMZ` arcanum from arch doc). At cargo invocation, toylangc's binary intercepts as the rustc wrapper, parses argv, re-reads `toylang.toml`, and dispatches to either a real rustc compile or its own driver.

The arch doc explicitly calls this arcanum out as "one of two erw arcana with no Sky analog" because Sky's `rustc` is the forked rustc (statically linked OR loaded as cdylib per Decision 4) invoked directly by cargo via `rust-toolchain.toml`.

**Action item for Sky proper's implementation:** do NOT replicate toylang's wrapper-mode dispatch pattern. Sky's distribution model (skyc orchestrator + forked rustc + libsky_backend.so as separate binaries per Decision 4) has no place for the wrapper-mode split. Sky's `bin/rustc` is invoked directly by cargo; no RUSTC_WORKSPACE_WRAPPER interception.

Toylang itself can continue using wrapper-mode (operationally works; architecturally wrong-shape but functionally correct). Retiring toylang's wrapper-mode would be ~2-3 days of work in `toylangc/src/main.rs`; not justified absent a forcing function. The forcing function would be Sky proper's actual implementation work, which forces the split.

Priority: zero for now (defer until Sky proper's toolchain phase begins). Listed here so the Sky proper engineer doesn't accidentally copy the pattern.

### 8. Gotchas — debugging wisdom from prior sessions

General knowledge accumulated through toylang's implementation. Not specific work items, but useful when debugging fixture failures or LLVM-pipeline mysteries.

**Inlining behavior across rustc versions.** LLVM's inliner thresholds and pass scheduling change between rustc versions. Tests that assert specific inlining outcomes can break on nightly bumps even when nothing semantic changed. Mitigation: assert "inlined OR optimized differently in a way that still produces no `bl` to the boundary symbol" rather than "constant-folded to exact value X." Use forgiving patterns.

**Symbol-name pattern matching is fragile.** Sky's symbols use v0 mangling with disambig codes that depend on the crate's compilation. Patterns like `.*sky_lib.*` should match against the symbol's DEMANGLED form (via `rustc-demangle` or LLVM's symbolizer), not the raw mangled name. The harness should do the demangling.

**Some inlining only happens at link time.** Cross-CGU and cross-crate inlining requires LTO. Tests asserting these must explicitly set `lto = "thin"` or `lto = "fat"`. Without LTO, the linker can't inline across translation units even at -O3.

**`#[inline(always)]` is a hint, not a guarantee.** Even `#[inline(always)]` doesn't force inlining in all cases — LLVM refuses if the inliner's heuristics decide it'd produce bad code (recursive calls, very large body, sanitizer interaction). Tests asserting "inline(always) callee was inlined" should be aware of this and use forgiving patterns.

**Sky's `#[inline(never)]` discipline on Phase-6 stubs.** Per §6.6.5, Phase-6 stdlib wrappers carry `#[inline(never)]`. Tests asserting "Phase-6 wrapper is not inlined" should expect the discipline to work; tests asserting "Phase-6 wrapper IS inlined" will fail by design. (Note: this was removed from Sky-item stubs during the F1 investigation — see arch §F.1. Only Phase-6 stdlib wrappers retain it.)

**Save-temps for debugging fixture failures.** When an inlining or codegen test fails unexpectedly, the recipe from arch §25.2 B15's playbook applies: `RUSTFLAGS="-C save-temps"` preserves intermediate bitcode through the LLVM pipeline. `llvm-dis` and `llvm-objdump -d` let you see exactly what LLVM did or didn't inline. Combine with `cargo build -v` to see the exact rustc invocations.

**Worktree-isolated agents may be branched from old revisions.** When using parallel agents (workflow infrastructure) to investigate, pair the agent's reported findings with explicit git SHA verification. Agents in isolated worktrees may report behavior from an old branch state if the parent shell's branch has moved since the worktree was created.

**Wipe BOTH the shared cache AND per-fixture `.toylang-build/`** before destructive probes. The shared cache (`toylangc/target/integration-projects-cache`) and per-fixture build directories are separate; wiping only one can leave the other stale and produce confusing results.

---

## Reference: cross-doc pointers

- `rust-interop-architecture.md` — the architecture design doc. Current state is pre-review; will need updates per the doc update plan above.
- `toylangc/` — the reference implementation. Most of your code changes land here.
- `rustc-lang-facade/` — the facade crate. Provider overrides, hook installation. Substantial code changes here too.
- `~/rust/` — the forked rustc tree. Patch 4 changes land here (Phase H).
- `tmp/claude-conversation-2026-06-23-836f2993.md` — full review-exchange transcript. ~10,800 lines including pasted articles; the substantive content is roughly half that. Decisions log above is the distillation.
- `tmp/patch5-empirical-2026-06-21/VERDICT.md` — the empirical contrast probe that validated §F.17 / drove the patch-5 retirement (pre-review, but useful context for understanding the codebase's empirical-validation methodology).

