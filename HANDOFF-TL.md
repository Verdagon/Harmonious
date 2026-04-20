# TL Handoff — erw

**For whoever picks up the `erw` (Embed Rust Well) project next.**
The prior TL is moving to a different project; this doc orients the
next maintainer on project state, open threads, and how to get
productive without reading every doc in the repo.

The project's active-development roadmap is **complete**. This is a
maintenance-mode handoff, not a mid-project one.

---

## 1. 30-second summary

- **What erw is:** a framework (`rustc-lang-facade`) for embedding
  custom languages into rustc's compilation pipeline, plus a
  demonstrator consumer language (`toylang`) that proves the
  framework by compiling toylang programs that call into real
  crates.io Rust crates.

- **Current state:** all 8 implementation phases complete +
  fork-reduction roadmap fully shipped (stages 1–5). **Zero rustc
  fork patches.** FileLoader retired. Direct mode retired. Two-crate
  architecture (stub rlib + user bin) is the canonical shape.
  Post-stage-5 B6/B7 cleanup also shipped; `risks.md` Category B is
  empty of active items. **210 tests passing** (67 unit + 128
  integration_projects + 15 standalone), 0 failed, 0 ignored,
  cold **and** warm, against vanilla `nightly-2026-01-20` installed
  via rustup.

- **Active work:** none. No junior handoffs in flight. No unresolved
  architectural risks. No mid-stage refactors.

- **Open threads** (all non-urgent, see §3): optional upstream PR
  opportunity, minor tech-debt items. (The Vale response was sent
  2026-04-20; no reply yet — logged in §3a for the record.)

- **The rustc fork working tree at `~/rust` is empty** (no diff
  against upstream). Its branch name `per-instance-mir` is vestigial.
  The `rustc-fork` rustup toolchain link is vestigial. Both are safe
  to delete.

Key shipping commits:

- Stages 1–4 (fork reduction): `ed2e692` (callback split), `b345162`
  (cross-crate backend), `ce437ae`/`bf770ae`/`da7ad87` (stage-3
  `optimized_mir` override, 5→2 patches), `1d862f4` / `13d8f12` /
  `51f0c5e` / `d044560` / `c25aa4b` (stage-4 `CodegenBackend` plugin
  + partitioner override, 2→0 patches).
- Stage 5 (two-crate architecture + FileLoader/direct-mode
  retirement): `6bda10c` / `b6a2bf6` / `91cad25` / `05fed63` /
  `a2f06ea` / `6d65831` / `1ae7fd4` / `b3e276d` / `68e5783`.
- Post-stage-5 cleanup: `7bac631` (B7 bool-bug fix + test
  re-enabled), `74fe3a2` (`is_from_lang_stubs_safe` alias cleanup),
  `3cfb983` + `2eea9b8` (B6 architectural fix — registry-driven
  codegen, stateless `monomorphize_type`, harness stopgap retired).

---

## 2. What's in flight

**Nothing.** The project is genuinely at a resting point.

- **No active junior handoffs.** All five stage handoffs landed and
  their docs moved to `docs/historical/`.
- **Three POC/spike worktrees preserved** at
  `/Users/verdagon/erw-poc-*` and `/Users/verdagon/erw-spike-*` as
  reproducibility anchors for past investigations. No action needed;
  preserve them for anyone later tracing why specific design choices
  were made.
- **No regressions, no failing tests, no drift concerns.** `risks.md`
  Category A items are shatter-tier (very low probability); Category
  B is empty; Category C is ongoing hygiene only.

---

## 3. Inherited open threads

All non-urgent; none block normal maintenance.

### 3a. The Vale response — sent

Sent 2026-04-20 (rewrite-and-send path from the earlier three-way
decision). The rewritten version reframes the content as a post-
landing status report: "here's what we ended up shipping, including
stage 5 making the architecture a natural fit for a greenfield
consumer like Vale," rather than the mid-roadmap "we're considering
X" framing the original draft carried.

Record-of-sent kept at `response-reducing-rustc-fork.md` (repo root).
Original Vale inquiry at
`/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`.

No reply yet (logged for whoever picks this up). If Vale engages
further, the sent response is your starting point — customize the
follow-up to whatever Vale's current posture is rather than re-
arguing from the reasoning docs.

### 3b. Upstream PR contribution (optional)

The ModuleLlvm-wall spike identified a ~30-line PR to rustc —
empty-ifying the `LlvmCodegenBackend(())` tuple field + exposing
`ModuleLlvm::new` / `ModuleLlvm::llmod` — that would unlock
finer-grained item-level codegen interception for any future
consumer of the facade. Highest-probability single change (the
tuple-field unseal) has ~80–85% fast-landing probability.

Erw does NOT need this PR (the partitioner-override design landed
without it). A future consumer doing item-level interception might
benefit. Direction-aligned with rust-lang/rust#45274; low risk;
standalone. Details + PR draft in
`/Users/verdagon/erw-spike-modulellvm-wall/findings.md`.

### 3c. Minor tech-debt items

`docs/architecture/known-tech-debt.md` — three open entries (#5
generic function body validation, #28 silent truncation of
non-default parent-type args, #29 callback-trace `unexpected`
parameter no-op under wrapper-mode semantics). All non-load-bearing;
good warm-up tasks for any future contributor.

### 3d. `~/rust` working tree and `rustc-fork` rustup link

Both are installed and unused. Safe to `rustup toolchain uninstall
rustc-fork && rm -rf ~/rust` at your discretion. Non-urgent; zero
cost to keep them around as historical-reference state, but also
zero downside to reclaiming the disk.

---

## 4. External stakeholders

- **Vale project.** The only external stakeholder who ever engaged
  directly. Their original inquiry
  (`/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`)
  prompted the entire fork-reduction roadmap. Vale's greenfield
  interop shape (Cases 1a/1b/3/4/6 in
  `why-interleaved-monomorphization.md`) is precisely what stage 5's
  two-crate architecture anticipates. If Vale adopts erw as a
  starting point for their next interop generation, the architecture
  is ready.

- **Rust team, implicit.** Erw now builds against vanilla rustc
  nightly (`nightly-2026-01-20`). No active coordination required.
  No upstream dependencies other than the nightly pin. When bumping
  nightly, budget ~1 week for API drift per `risks.md` Category B3
  (MIR construction surface).

---

## 5. Reading order to get oriented

~2 hours of reading gets a new maintainer to the point where they
can make decisions. Maintenance-mode reading is lighter than
mid-project would be.

### Tier 1 — required (45–60 min)

1. **`CLAUDE.md`** — project-wide instructions. Compiler laws,
   build conventions. 10 min.
2. **`docs/meta.md`** — documentation strategy. 5 min.
3. **`docs/architecture/rust-interop-guide.md`** — canonical
   architecture doc. Front-matter status block + Parts 1–5 are
   load-bearing. Parts 6–8 are file index + tech-debt pointer +
   arcana index. 30–45 min to skim critical paths.
4. **This document.** 5–10 min.

### Tier 2 — current-state + long-term risk posture (45–60 min)

5. **`docs/architecture/risks.md`** — what could break over the
   long term, what the canaries are, what exit strategies exist.
   Critical reading before bumping rustc nightly or when diagnosing
   unexpected breakage. 20–30 min.
6. **`docs/reasoning/architecture-decisions.md`** — why each major
   design choice. Short, dense. 10–15 min.
7. **Recent git log** — `git log --oneline -20` from repo root.
   Commit messages are dense prose. 10 min.
8. **`future-architecture-investigations.md`** — Vale inquiry
   history + the three POC/spike summaries. 15 min.

### Tier 3 — engage only when working on specific areas (as-needed)

9. **`docs/reasoning/why-interleaved-monomorphization.md`** — the
   seven-case taxonomy. Read when evaluating whether a new use case
   requires the interleaving architecture.
10. **`docs/reasoning/rustc-fork-design-space.md`** — full
    fork-reduction analysis. Historical reference now that all
    stages shipped.
11. **`docs/reasoning/dep-discovery-approaches.md`** — Approach A
    vs B comparison. The asymmetry insight behind stage 3.
12. **`docs/historical/phase-history.md`** — per-phase writeups
    (phases 1–8 + fork-reduction stages 1–5 + post-stage-5
    cleanup). Historical reference.
13. **`response-reducing-rustc-fork.md`** — record of the Vale
    response sent 2026-04-20 (see §3a).
14. **`docs/historical/handoff-*.md`** — archived junior handoffs
    from each stage. Reference if you need to see how past work
    was scoped + executed.

### Tier 4 — as-needed reference

15. **`docs/arcana/*.md`** — cross-cutting concerns. Read when
    working on code that references `@ID` markers; don't read
    front-to-back. The arcana index in `rust-interop-guide.md`
    Part 8 lists them.
16. **`docs/usage/writing-main.md`** — practical rules for writing
    toylang programs.
17. **`docs/usage/testing.md`** — build & test commands.
18. **`docs/architecture/known-tech-debt.md`** — tracked debt
    (items #5, #28, #29 open).
19. **`docs/historical/quest.md`** — archived project diary. Skim
    only; useful for debugging past decisions.

### Tier 5 — build/tooling (read once)

20. **`/Users/verdagon/.claude/CLAUDE.md`** (if using the same
    shell tooling as the prior TL) — build-redirect conventions.

---

## 6. What lives in the author's head (not in docs)

- **The "don't commit without explicit request" habit.** The prior
  TL treated the working tree as the primary shared state; commits
  happened in intentional batches after explicit owner approval.
  Especially important when a junior is landing mid-scope work.

- **The roadmap staging discipline.** Every fork-reduction stage
  was designed to leave the codebase strictly better even if the
  next stage never shipped. Stopping at 5/5 was a planned end
  state, not an accident. If a future stage-6 emerges, follow the
  same pattern: each sub-stage a standalone win, clean stopping
  points, junior-tractable handoffs.

- **The arcana threshold.** Not every cross-cutting concern
  becomes an arcana. Rule of thumb: if the concern touches ≥3
  files OR its absence would cause the same bug to be
  re-introduced, make it an arcana with `@ID` references.
  Otherwise, a code comment suffices.

- **Spike discipline.** Investigations run under `git worktree
  add` on a dedicated branch with a `findings.md` deliverable.
  The findings doc is the output, not the code. The three
  preserved POC/spike branches are exemplars of the pattern.

- **Writing conventions.** The prior TL's writing is dense and
  specific — no throat-clearing, no narrating internal
  deliberation, no "this is tricky" framings. Commit messages are
  paragraphs, not one-liners. Future readers may be calibrated on
  this style; adjust to your own as preferred.

- **Pre-registered predictions.** Before running any POC or spike,
  write `predictions-before-running.md` locking in expectations.
  After running, honestly score what was right/wrong. This
  discipline made the three investigations substantially more
  valuable as empirical evidence.

- **"Don't fix things you didn't break" during focused work.** If
  you notice something broken while working on an unrelated task,
  write it down (tech debt, quest.md note) and move on. Kept past
  refactors clean.

- **Junior handoff structure.** All stage handoffs (1–5) followed
  the same shape: Context, Required reading, Current surface,
  Proposed design, Implementation steps (staged sub-phases),
  Critical subtleties, Verification, Out of scope, If you get
  stuck. Earned its keep across five successful migrations.

- **Trust escalations.** Junior engineers across multiple stages
  escalated at the right moments (stage-4c's cross-crate oracle
  gap, stage-5c's cargo cache invalidation, stage-5c's test-fixture
  architectural question). Each escalation produced a better
  answer than force-patching through would have. Trust the
  instinct; give juniors license to stop at clean checkpoints.

---

## 7. Repo layout cheatsheet

```
erw/
├── CLAUDE.md                                   # project-wide instructions
├── README.md                                   # public-facing (minimal)
├── HANDOFF-TL.md                               # this doc
├── future-architecture-investigations.md       # Vale/POC summary
├── response-reducing-rustc-fork.md             # Vale response (sent 2026-04-20)
├── Cargo.toml                                  # workspace
├── docs/
│   ├── meta.md                                 # doc strategy
│   ├── architecture/
│   │   ├── rust-interop-guide.md               # ★ canonical arch doc
│   │   ├── risks.md                            # ★ long-term risk catalog
│   │   └── known-tech-debt.md                  # tracked debt items
│   ├── reasoning/
│   │   ├── architecture-decisions.md           # why each choice was made
│   │   ├── why-interleaved-monomorphization.md # architectural invariant
│   │   ├── rustc-fork-design-space.md          # fork-reduction analysis
│   │   ├── dep-discovery-approaches.md         # Approach A vs B
│   │   └── trait-call-investigation.md
│   ├── usage/
│   │   ├── writing-main.md                     # toylang-user rules
│   │   └── testing.md                          # build + test commands
│   ├── arcana/                                 # ~13 cross-cutting concerns
│   ├── background/
│   ├── skills/
│   └── historical/
│       ├── quest.md                            # archived project diary
│       ├── phase-history.md                    # per-phase writeups
│       ├── handoff-*.md                        # archived junior handoffs
│       │                                       #   (5 stages)
│       └── (various plan + session notes)
├── rustc-lang-facade/                          # reusable framework crate
├── toylangc/                                   # demonstrator consumer
└── Cargo.lock
```

External worktrees (preserved as reference; don't delete):

```
/Users/verdagon/erw-poc-optimized-mir/          # POC #1 (shipped in stage 3)
/Users/verdagon/erw-poc-separate-crate-stubs/   # POC #2 (fed stage 4)
/Users/verdagon/erw-spike-modulellvm-wall/      # spike (fed stage 4)
```

The rustc fork — **now deletable**:

```
~/rust                                          # empty working tree (no diff vs upstream)
                                                # branch name `per-instance-mir` is
                                                #   vestigial; the query it named was
                                                #   removed in stage 3 and the two
                                                #   remaining hooks were retired in
                                                #   stage 4
                                                # `git diff upstream/master` → empty
                                                # Safe to `rm -rf ~/rust`
                                                #
rustup toolchain rustc-fork                     # points at ~/rust/build/host/stage2,
                                                #   unused by erw (tests run against
                                                #   vanilla nightly-2026-01-20 via rustup)
                                                # Safe to `rustup toolchain uninstall rustc-fork`
```

---

## 8. Commands to know

All assume `nightly-2026-01-20` toolchain is installed via rustup
(`rustup toolchain install nightly-2026-01-20`).

```bash
# Full test suite (expect: 67 + 128 + 15 = 210 passing, 0 failed, 0 ignored)
cargo +nightly-2026-01-20 test -p toylangc 2>&1 > /tmp/erw.txt
grep "test result:" /tmp/erw.txt

# Check warnings
cargo +nightly-2026-01-20 check -p toylangc 2>&1 > /tmp/erw.txt
tail -20 /tmp/erw.txt

# Run a specific integration test
cargo +nightly-2026-01-20 test -p toylangc --test integration_projects test_name

# Run standalone tests (full crates.io interop suite)
cargo +nightly-2026-01-20 test -p toylangc --test standalone_tests
```

**Build-redirect convention** (per `CLAUDE.md`): use `>` to redirect
fully to a fixed `/tmp/<session>.txt` file, then inspect as a
separate command. Don't chain `| grep` or `| tee` — if the build
takes 30+ seconds, you lose the ability to re-analyze a different
part of the output without re-running.

---

## 9. Sanity-check before declaring handoff complete

- [ ] New maintainer has read Tier 1 + 2 at minimum.
- [ ] New maintainer has skimmed `docs/architecture/risks.md` —
      knows what the canaries are for the Category A/B risks.
- [ ] Outgoing TL has walked through §3 (open threads) + §6
      (in-head knowledge) in a sync (if any).
- [x] Vale response sent 2026-04-20 (see §3a). New maintainer
      knows to use it as the starting point if Vale replies.
- [ ] Full test suite runs green on the new maintainer's machine
      (`cargo +nightly-2026-01-20 test -p toylangc` → 210/210).
- [ ] New maintainer knows about the three preserved POC/spike
      worktrees and why they're kept.
- [ ] New maintainer knows `~/rust` and `rustc-fork` toolchain are
      vestigial and safe to delete.

---

## 10. One-paragraph "take" on the project's state

Erw shipped its full architectural arc. The hypothesis (a custom
language can embed into rustc's compilation pipeline via query
providers + a separately-emitted LLVM backend, without
reimplementing rustc's trait/generic resolution machinery) is
demonstrably true — toylang compiles 15 real crates.io dependencies
against vanilla nightly rustc with zero fork patches. The
fork-reduction roadmap originally prompted by Vale's
deployment-friction inquiry is fully shipped: all five stages
landed (callback job-split; cross-crate backend cleanup;
`optimized_mir` override; `CodegenBackend` plugin via partitioner
override; two-crate architecture with FileLoader + direct-mode
retirement). Post-stage-5 cleanup closed the two Category-B risks
surfaced during migration. The architecture is now a clean
reference for any future consumer — Vale-fork-ready by design, not
by accident. Handoff is genuinely "project in maintenance mode" —
the canonical architecture doc describes current reality, the
risk doc catalogs what could break over time, the tech-debt doc
lists minor items, and nothing urgent is in flight.
