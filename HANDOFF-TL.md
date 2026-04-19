# TL Handoff — erw

**For the incoming TL on the `erw` (Embed Rust Well) project.**
Current TL role is being handed off; this doc orients you on project
state, open threads, and how to get productive without reading every
doc in the repo.

---

## 1. 30-second summary

- **What erw is:** a framework (`rustc-lang-facade`) for embedding
  custom languages into rustc's compilation pipeline, plus a
  demonstrator consumer language (`toylang`) that proves the
  framework by compiling toylang programs that call into real
  crates.io Rust crates.
- **Current state:** all 8 implementation phases complete + fork-
  reduction roadmap **fully shipped, zero fork patches.** 211 tests
  passing (67 unit + 129 integration + 15 standalone), 0 failed,
  0 ignored, against vanilla `nightly-2025-01-15` installed via
  rustup. Stages 1–4 committed: `ed2e692` (callback job-split),
  `b345162` (cross-crate backend cleanup), `ce437ae` / `bf770ae` /
  `da7ad87` (Stage 3 — `optimized_mir` override, 5 patches → 2),
  `1d862f4` (Stage 4a — partitioner override filters consumer items
  from CGUs), `13d8f12` (Stage 4b — `CODEGEN_SKIP_HOOK` retired,
  1 patch remaining), `51f0c5e` (Stage 4c step 1 —
  `upstream_monomorphizations_for` override scaffolding), `d044560`
  (Stage 4c — `VISIBILITY_OVERRIDE_HOOK` retired via plugin-set
  linkage, 0 patches), plus Stage 4d for toolchain switch + doc
  pass.
- **Active work:** none. Fork-reduction roadmap complete.
  `FileLoader`-injected stub model is preserved as-is (retirement
  was not pursued — Outcome A from the 2026-04-19 TL investigation
  showed zero-fork was reachable without it, so single-crate
  `FileLoader` stays indefinitely for simplicity).
- **Open outbound:** a response draft to the Vale team
  (`response-reducing-rustc-fork.md`, repo root, ~168 lines). Not
  sent; its content is partially superseded by stages 1/2/3 having
  shipped (the POC findings it cites are now shipping architecture).
  Decision to send / rewrite / hold belongs to the incoming TL.

---

## 2. What's in flight

- **No active junior handoffs.** Stages 1–4 all landed cleanly;
  their handoff docs are in `docs/historical/`
  (`handoff-codegen-backend-plugin.md` among them). The
  `FileLoader` retirement / separate-crate-stubs migration was
  scoped and then explicitly dropped during Stage 4c — the TL's
  investigation found that plugin-set linkage in the partitioner
  override retires `VISIBILITY_OVERRIDE_HOOK` without needing any
  of it. `FileLoader` stays indefinitely.
- **Three POC/spike branches** (`poc/optimized-mir-override`,
  `poc/separate-crate-stubs`, `spike/modulellvm-wall`) remain at
  `/Users/verdagon/erw-poc-*` and `/Users/verdagon/erw-spike-*`.
  **Still preserve these** — POC #1's findings shipped in stage 3
  but the worktree is the reproducibility anchor, and POC #2 +
  the spike directly feed stage 4's design (separate-crate
  mechanics and the Medium-path partitioner-override pattern,
  respectively). The stage-4 junior reads them heavily.

---

## 3. Inherited open threads — decisions requiring TL judgment

### 3a. The Vale response

Draft at `response-reducing-rustc-fork.md` (repo root, ~168 lines).

Original Vale inquiry:
`/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`.
The draft answered six questions about fork-reduction feasibility,
reflecting the POC findings at a point when no migration had
landed. **As of stage 3 shipping (commit `bf770ae`), the draft's
POC #1 answer now describes shipping architecture, not prospective
migration.** That weakens the "here's what we're thinking about"
framing substantially; the response reads better as "here's what
we did, and here's what stage 4 will finish" now.

**Decision:**
- **Send as-is:** content is still mostly correct, just dated.
  Minimal effort.
- **Rewrite:** reframe as a post-migration status report. Accurate
  but more work; the incoming TL decides if that matters.
- **Hold:** content isn't time-sensitive; Vale has not followed
  up. Stage 4 landing is a natural prompt for a comprehensive
  update.

Leaning hold until stage 4 ships, then send a consolidated
"here's where we ended up" message. But judgment call.

### 3b. Upstream PR contribution (optional, not critical path)

The ModuleLlvm-wall spike identified a ~30-line PR to rustc that
would unlock finer-grained plugin-based codegen interception. The
single highest-probability change (empty-ifying the
`LlvmCodegenBackend(())` tuple field) has ~80–85% fast-landing
probability as a standalone contribution. **The stage-4 Medium
path does NOT require this PR** — the partitioner-override design
sidesteps the `pub(crate)` wall entirely. If a future consumer
(Vale, or anyone) wants finer item-level interception the PR
becomes useful, but for toylang under the Medium path it's
direction-signaling, not load-bearing.

Details + PR draft in `spike/modulellvm-wall`'s `findings.md`.

### 3c. Minor tech-debt items

`docs/architecture/known-tech-debt.md` lists open items. None are
load-bearing. Delegate as warm-up tasks for future juniors.

---

## 4. External stakeholders

- **Vale project.** Currently the only external stakeholder. They
  don't have a commitment in either direction. The inquiry doc at
  `/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`
  has the original framing. Vale's interop story is a greenfield
  consumer of the facade — stage 4's zero-fork architecture is
  directly what they'd adopt. If the response gets sent, that's
  the starting point for an ongoing conversation.

- **Rust team, implicit.** The rustc fork at `~/rust/` on branch
  `per-instance-mir` tracks `nightly-2025-01-15`. Rebasing against
  newer nightlies is ~1–2 days per bump post-stage-3 (down from
  ~2–3 pre-stage-3). Done ad-hoc; no schedule. No active upstream
  coordination required. Branch name is vestigial — the query it
  referenced is gone. Rename deferred until stage 4 eliminates
  the fork entirely.

---

## 5. Reading order to get oriented

~2–3 hours of reading to make decisions. Don't read more than
tier 1–2 before your first sync.

### Tier 1 — required (45–60 min)

1. **`/Users/verdagon/erw/CLAUDE.md`** — project-wide instructions.
   Compiler laws, key-docs map. 10 min.
2. **`/Users/verdagon/erw/docs/meta.md`** — documentation strategy
   (where different kinds of docs live). 5 min.
3. **`/Users/verdagon/erw/docs/architecture/rust-interop-guide.md`**
   — the canonical architecture doc. Front-matter status block +
   Parts 1–4 are the load-bearing sections. Part 10 has phase
   history; Part 11 has the arcana index. 30–45 min to skim
   critical paths.

### Tier 2 — current-state (30–45 min)

4. **This document (HANDOFF-TL.md)** — you're reading it. Use it
   as the checklist for the rest.
5. **`/Users/verdagon/erw/handoff-codegen-backend-plugin.md`** —
   the active stage-4 junior handoff. Gives you the most specific
   picture of what's in flight. 10 min skim, 30 min if you want
   to review the plan.
6. **Recent git log** — `git log --oneline -20` from repo root.
   Commit messages are dense and specific; reading them orients
   you on shipping history. Stages 1–3 commits are especially
   informative (`ed2e692`, `b345162`, `ce437ae`, `bf770ae`,
   `da7ad87`). 10 min.
7. **`/Users/verdagon/erw/future-architecture-investigations.md`**
   — the Vale inquiry + three investigations summary. 15 min.
8. **`/Users/verdagon/erw/docs/historical/quest.md`** — archived
   project diary. Skim only; useful for debugging past decisions.
   5–10 min skim.

### Tier 3 — only if engaging deeply with stage 4 or the Vale thread (2–3 hours)

9. **`/Users/verdagon/erw/docs/reasoning/why-interleaved-monomorphization.md`**
   — seven-case taxonomy explaining why the facade exists.
10. **`/Users/verdagon/erw/docs/reasoning/rustc-fork-design-space.md`**
    — full fork-reduction analysis. §4.2 is the stage-4 spec;
    §5 is the post-stage-3 cost accounting.
11. **`/Users/verdagon/erw/docs/reasoning/dep-discovery-approaches.md`**
    — Approach A vs B comparison; the architectural insight that
    stage 3 depended on. Short (~180 lines) but dense.
12. **`/Users/verdagon/erw/response-reducing-rustc-fork.md`** — the
    outbound draft (see §3a).
13. **Three POC/spike `findings.md` files** on the respective
    branches' worktrees. Read only if you need specific evidence
    for stage-4 design claims or Vale-response citations. For
    stage 4 specifically: `poc/separate-crate-stubs` §4.3D and
    `spike/modulellvm-wall` §4.1 (the Medium path) are
    load-bearing.

### Tier 4 — as-needed reference

14. **`docs/arcana/*.md`** — cross-cutting concerns. Read when
    working on code that references `@ID` markers; don't read
    front-to-back. Part-11 arcana index in the arch guide lists
    them.
15. **`docs/usage/writing-main.md`** — practical rules for
    writing toylang code. Only if debugging compiler-feature
    tests or helping future juniors.
16. **`docs/architecture/known-tech-debt.md`** — tracked debt.
    Occasional reference.
17. **`docs/historical/handoff-*.md`** — archived junior handoffs
    from stages 1/2/3. Reference if the stage-4 junior wants to
    see how prior stages were scoped and executed.

### Tier 5 — build/tooling (read once)

18. **`/Users/verdagon/.claude/CLAUDE.md`** (if using the same
    shell tooling as the prior TL) — build-redirect conventions
    (redirect to `/tmp/<session>.txt` via `>`, inspect as a
    separate command, vanilla `nightly-2025-01-15` toolchain).
    Applies to
    whoever's running builds.

---

## 6. What lives in the author's head (not in docs)

- **The "don't commit without explicit request" habit.** The prior
  TL waited for the user (project owner) to say "commit" before
  running `git commit`. Treated the working tree as the primary
  shared state; commits happened in intentional batches.

- **The roadmap staging discipline.** Each of the five fork-
  reduction stages is designed to leave the codebase strictly
  better even if the next stage never ships. Stage 1 (callback
  split) was a standalone win regardless of zero-fork pursuit.
  Stage 2 (cross-crate cleanup) was another. Stage 3 (`optimized_mir`
  override + fork reshape) shipped a 5→2 patch reduction that's
  useful on its own. Stage 4 completes the zero-fork story.
  **Don't break this discipline** — if you inherit a mid-stage
  situation, land what's viable as its own improvement before
  continuing.

- **The arcana threshold.** Not every cross-cutting concern becomes
  an arcana. Rule of thumb: if the concern touches ≥3 files OR its
  absence would cause the same bug to be re-introduced, make it an
  arcana with `@ID` references. Otherwise, a code comment suffices.

- **Spike discipline.** Investigations run under `git worktree add`
  on a dedicated branch with a `findings.md` deliverable. The
  findings doc is the output, not the code. Three POC/spike
  branches in the repo are exemplars of the pattern.

- **Writing conventions.** The prior TL's writing is dense and
  specific — no throat-clearing, no narrating internal deliberation,
  no "this is tricky" framings. Commit messages are paragraphs,
  not one-liners; see stages 1/2/3 commits for the house style.
  Adjust to your own style as preferred, but know that future
  readers may be calibrated on the prior style.

- **Pre-registered predictions.** Before running a POC or spike,
  write `predictions-before-running.md` locking in expectations.
  After running, honestly score what was right/wrong. The three
  POC/spike branches all follow this discipline; it made the
  investigations substantially more valuable as empirical evidence.

- **"Don't fix things you didn't break" during focused work.** If
  you notice something broken while working on an unrelated task,
  write it down (tech debt, quest.md note, whatever) and move on.
  Discipline kept past refactors clean.

- **Junior handoff structure.** Stage handoffs (1/2/3/4) all follow
  the same shape: Context, Required reading, Current surface,
  Proposed design, Implementation steps (staged), Critical
  subtleties, Verification, Out of scope, If you get stuck. When
  delegating to a junior, lean on this structure — it's earned
  its keep across three successful migrations.

---

## 7. Repo layout cheatsheet

```
erw/
├── CLAUDE.md                                   # project-wide instructions
├── README.md                                   # public-facing (minimal)
├── HANDOFF-TL.md                               # this doc
├── handoff-codegen-backend-plugin.md           # ★ active stage-4 junior handoff
├── future-architecture-investigations.md       # Vale/POC summary + roadmap status
├── response-reducing-rustc-fork.md             # outbound Vale draft
├── Cargo.toml                                  # workspace
├── docs/
│   ├── meta.md                                 # doc strategy
│   ├── architecture/
│   │   ├── rust-interop-guide.md               # ★ canonical arch doc
│   │   └── known-tech-debt.md
│   ├── reasoning/
│   │   ├── why-interleaved-monomorphization.md # architectural invariant
│   │   ├── rustc-fork-design-space.md          # fork-reduction analysis
│   │   ├── dep-discovery-approaches.md         # Approach A vs B (stage-3 landing)
│   │   └── trait-call-investigation.md
│   ├── usage/
│   │   ├── writing-main.md
│   │   └── rebuilding-rustc-fork.md
│   ├── arcana/                                 # ~12 cross-cutting concerns
│   ├── background/
│   ├── skills/
│   └── historical/
│       ├── quest.md                            # archived project diary
│       ├── handoff-regex.md                    # archived junior handoff
│       ├── handoff-phase7-uuid.md              # archived
│       ├── handoff-cross-crate-backend.md      # stage-2 junior handoff (archived)
│       ├── handoff-optimized-mir-migration.md  # stage-3 junior handoff (archived)
│       └── (various phase plans + session notes)
├── rustc-lang-facade/                          # reusable framework crate
├── toylangc/                                   # demonstrator consumer
└── Cargo.lock
```

External worktrees (preserved as reference; don't delete):

```
/Users/verdagon/erw-poc-optimized-mir/          # POC #1 (shipped in stage 3)
/Users/verdagon/erw-poc-separate-crate-stubs/   # POC #2 (feeds stage 4)
/Users/verdagon/erw-spike-modulellvm-wall/      # spike (feeds stage 4)
```

The rustc fork:

```
~/rust                                          # zero-patch working tree
                                                # branch: per-instance-mir
                                                #   (name vestigial; the query
                                                #   was removed in stage 3 and
                                                #   the two remaining hooks
                                                #   were retired in stage 4)
                                                # `git diff upstream/master`:
                                                #   empty
                                                # rustup toolchain `rustc-fork`
                                                #   link still points at
                                                #   ~/rust/build/host/stage2
                                                #   but is no longer used by
                                                #   the project — tests run
                                                #   against vanilla
                                                #   nightly-2025-01-15 via
                                                #   rustup. Safe to uninstall:
                                                #   `rustup toolchain uninstall rustc-fork`
                                                #   The `~/rust` working tree
                                                #   and build outputs can be
                                                #   deleted to reclaim disk.
```

---

## 8. Commands to know

All assume `nightly-2025-01-15` toolchain is installed via rustup
(`rustup toolchain install nightly-2025-01-15`).

```bash
# Full test suite (expect: 67 + 129 + 15 = 211 passing, 0 failed, 0 ignored)
cargo +nightly-2025-01-15 test -p toylangc 2>&1 > /tmp/erw.txt
grep "test result:" /tmp/erw.txt

# Check warnings
cargo +nightly-2025-01-15 check -p toylangc 2>&1 > /tmp/erw.txt
tail -20 /tmp/erw.txt

# Run one test
cargo +nightly-2025-01-15 test -p toylangc --test integration_tests test_name
```

**Build-redirect convention (updated in CLAUDE.md):** use `>` to
redirect fully to a fixed `/tmp/<session>.txt` file, then inspect
as a separate command. Don't chain `| grep` or `| tee` — if the
build takes 30+ seconds, you lose the ability to re-analyze a
different part of the output without re-running.

---

## 9. Sanity-check before declaring handoff complete

- [ ] New TL has read Tier 1 + 2 at minimum.
- [ ] Outgoing TL has walked through §3 (open threads) and §6
      (in-head knowledge) with the new TL in a sync.
- [ ] New TL has skimmed `future-architecture-investigations.md`
      AND `handoff-codegen-backend-plugin.md`.
- [ ] New TL knows about the three preserved worktrees and how
      they feed stage 4.
- [ ] If the Vale response is sent, sent before handoff (so
      outgoing TL can handle any follow-up). If held, new TL
      knows the current posture.
- [ ] Full test suite runs green on the new TL's machine
      (`cargo +nightly-2025-01-15 test -p toylangc` → 211/211).

---

## 10. One-paragraph "take" on the project's state

Phases 1–8 are done and the architecture hypothesis (a custom
language can embed into rustc's compilation pipeline via query
providers + an LLVM backend without reimplementing trait/generic
resolution) is demonstrably true — toylang compiles 15 real
crates.io dependencies. The fork-reduction roadmap, originally
prompted by Vale's deployment-friction inquiry, is fully shipped:
all four stages landed (callback job-split, cross-crate backend
cleanup, `optimized_mir` override + partial hook reshape, and
finally retiring both remaining hooks via plugin-set linkage in
the `collect_and_partition_mono_items` override). Toylang now
builds against vanilla `nightly-2025-01-15` via rustup — zero
fork patches. The `~/rust` forked working tree is empty and
deletable; the `rustc-fork` rustup link is vestigial and safe to
uninstall. `FileLoader`-injected single-crate stubs stay in
place indefinitely (the 2026-04-19 Outcome-A investigation
showed it wasn't load-bearing for zero-fork). Handoff is "the
roadmap is complete — here's what's done and how it's all
wired."
