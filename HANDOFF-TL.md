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
- **Current state:** all 8 phases of the implementation are complete.
  211 tests passing (67 unit + 129 integration + 15 standalone),
  0 failed, 0 ignored. No open blockers, no active regressions.
- **Open architecture investigations:** three prototype-verified
  investigations map a zero-fork path (for a future consumer that
  needs it, e.g. Vale). No commitment to pursue for toylang itself.
  Summary at `future-architecture-investigations.md`.
- **Open outbound:** a response draft to the Vale team
  (`response-reducing-rustc-fork.md`) reflecting the investigations'
  findings. Not sent. Decision to send / rewrite / hold belongs to
  the new TL.

---

## 2. What's in flight

**Nothing urgent.** The project is in a quiet state. Specifically:

- **No failing tests, no active regressions.** Full suite runs
  in 30-45 seconds.
- **No juniors mid-handoff.** Phase 7's per-crate junior handoffs
  all landed; those docs moved to `docs/historical/` as pattern
  references.
- **Three POC/spike branches** (`poc/optimized-mir-override`,
  `poc/separate-crate-stubs`, `spike/modulellvm-wall`) are preserved
  as architecture-investigation reference material. **Don't
  garbage-collect these.** Each has a `findings.md` cited in the
  reasoning doc and the Vale response draft. Worktrees at
  `/Users/verdagon/erw-poc-*` and `/Users/verdagon/erw-spike-*`.

---

## 3. Inherited open threads — decisions requiring TL judgment

Items that need a decision from whoever holds the TL role. None are
urgent; all can be paused or resolved as you see fit.

### 3a. The Vale response

Draft at `response-reducing-rustc-fork.md` (repo root, ~168 lines).

A Vale team member (see `/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`
for their original inquiry) asked whether our rustc fork could be
reduced or eliminated. The response reflects three investigations'
findings honestly:
- Zero-fork is viable for greenfield consumers (like Vale).
- Toylang-brownfield migration would cost 4-8 weeks; not worth it
  for a research project.
- The map of alternatives is clearly drawn.

**Decision:** send / rewrite / hold.

Sending requires knowing Vale's current architecture-planning
posture (ask the original inquiry author via the channel in their
investigation doc). Rewriting is a judgment call — the draft has
been reviewed by both POC authors. Holding is fine; the content
isn't time-sensitive.

### 3b. Whether to pursue zero-fork for toylang

Current recommendation (per `docs/reasoning/rustc-fork-design-space.md`
§4.2 and Part 5): don't. The 4-8 week migration exceeds the fork's
~2-3 days-per-bump maintenance cost over any reasonable horizon. But
this presumes toylang stays a research project with "clone the repo,
install rustc-fork once, carry on" as an acceptable developer
workflow.

**When this decision changes:** if toylang's distribution model
shifts (e.g., publishing as a crates.io-installable compiler), the
fork becomes user-visible friction and the cost calculus flips.

Relevant docs if you ever revisit: the reasoning doc's §4.1–4.6,
Part 5, and the three POC `findings.md` files.

### 3c. Upstream PR contribution (optional, not critical path)

The ModuleLlvm-wall spike identified a ~30-line PR to rustc that
would unlock finer-grained plugin-based codegen interception. The
single highest-probability change (empty-ifying the
`LlvmCodegenBackend(())` tuple field) has ~80-85% fast-landing
probability as a standalone contribution. Independent of any
zero-fork decision; direction-aligned with rust-lang/rust#45274.

If Vale (or anyone) wants a direction-signaling contribution to
rustc that doesn't depend on the larger architectural discussion,
this is the smallest coherent unit. Details + PR draft in
`spike/modulellvm-wall`'s `findings.md`.

### 3d. Minor tech-debt items

`docs/architecture/known-tech-debt.md` lists open items. None are
load-bearing for current work. Leave them until convenient, or
delegate as warm-up tasks for future juniors.

---

## 4. External stakeholders

- **Vale project.** Currently the only external stakeholder. They
  don't have a commitment in either direction. The inquiry doc at
  `/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`
  has the original framing. If you send the response draft, that's
  the starting point for an ongoing conversation.

- **Rust team, implicit.** The rustc fork at `~/rust/` on branch
  `per-instance-mir` tracks `nightly-2025-01-15`. Rebasing against
  newer nightlies is ~2-3 days per bump. Done ad-hoc; no schedule.
  No active upstream coordination required.

---

## 5. Reading order to get oriented

~2 hours of reading gets you to the point where you can make
decisions. Don't read more than tier 1–2 before your first sync on
the project.

### Tier 1 — required (45-60 min)

1. **`/Users/verdagon/erw/CLAUDE.md`** — project-wide instructions.
   Compiler laws, key-docs map. 10 min.
2. **`/Users/verdagon/erw/docs/meta.md`** — documentation strategy
   (where different kinds of docs live). 5 min.
3. **`/Users/verdagon/erw/docs/architecture/rust-interop-guide.md`**
   — the canonical architecture doc. Front-matter status block +
   Parts 1-4 are the load-bearing sections. Part 10 has phase
   history; Part 11 has the arcana index. 30-45 min to skim
   critical paths.

### Tier 2 — current-state (30-45 min)

4. **This document (HANDOFF-TL.md)** — you're reading it. Use it
   as the checklist for the rest.
5. **`/Users/verdagon/erw/docs/historical/quest.md`** — the archived
   running project diary. Skim only; don't read end-to-end. The
   phase-by-phase "Discoveries and fixes" sections are useful when
   debugging past decisions. 10 min skim.
6. **Recent git log** — `git log --oneline -20` from repo root. The
   commit messages are dense and specific; reading them orients you
   on the actual shipping history. 10 min.
7. **`/Users/verdagon/erw/future-architecture-investigations.md`** —
   the Vale inquiry + three investigations summary. 15 min.

### Tier 3 — only if engaging with the Vale thread (2-3 hours)

8. **`/Users/verdagon/erw/docs/reasoning/why-interleaved-monomorphization.md`**
   — the seven-case taxonomy explaining why the facade exists.
9. **`/Users/verdagon/erw/docs/reasoning/rustc-fork-design-space.md`**
   — full fork-reduction analysis.
10. **`/Users/verdagon/erw/response-reducing-rustc-fork.md`** — the
    outbound draft.
11. **Three POC/spike `findings.md` files** (on the respective
    branches' worktrees). Read only if you need specific evidence
    for cited claims.

### Tier 4 — as-needed reference

12. **`docs/arcana/*.md`** — cross-cutting concerns. Read when working
    on code that references `@ID` markers; don't read front-to-back.
    The Part-11 arcana index in the arch guide lists them.
13. **`docs/usage/writing-main.md`** — practical rules for writing
    toylang code. Only if debugging compiler-feature tests or helping
    future juniors.
14. **`docs/architecture/known-tech-debt.md`** — tracked debt.
    Occasional reference.

### Tier 5 — build/tooling (read once)

15. **`/Users/verdagon/.claude/CLAUDE.md`** (if using the same shell
    tooling as the prior TL) — build-redirect conventions (pipe to
    `/tmp` via `tee`, don't chain grep, `rustc-fork` toolchain).
    Applies to whoever's running builds.

---

## 6. What lives in the author's head (not in docs)

Things worth conveying verbally before handoff is complete:

- **The "don't commit without explicit request" habit.** The prior
  TL waited for the user (project owner) to say "commit" before
  running `git commit`. Treated the working tree as the primary
  shared state; commits happened in intentional batches.

- **The arcana threshold.** Not every cross-cutting concern becomes
  an arcana. Rule of thumb: if the concern touches ≥3 files OR its
  absence would cause the same bug to be re-introduced, make it an
  arcana with `@ID` references. Otherwise, a code comment suffices.

- **Spike discipline.** Investigations run under `git worktree add`
  on a dedicated branch with a `findings.md` deliverable. The
  findings doc is the output, not the code. Three POC/spike branches
  in the repo are exemplars of the pattern.

- **Writing conventions.** The prior TL's writing is dense and
  specific — no throat-clearing, no narrating internal deliberation,
  no "this is tricky" framings. Commit messages are paragraphs, not
  one-liners. Matches the project's overall density. Adjust to your
  own style as preferred, but know that future readers may be
  calibrated on the prior style.

- **Pre-registered predictions.** Before running a POC or spike,
  write `predictions-before-running.md` locking in your expectations.
  After running, honestly score what was right/wrong. The three
  POC/spike branches all follow this discipline; it made the
  investigations substantially more valuable as empirical evidence.

- **"Don't fix things you didn't break" during focused work.** If
  you notice something broken while working on an unrelated task,
  write it down (tech debt, quest.md note, whatever) and move on.
  Discipline kept past refactors clean.

---

## 7. Repo layout cheatsheet

Repo root after handoff cleanup:

```
erw/
├── CLAUDE.md                                   # project-wide instructions
├── README.md                                   # public-facing (minimal)
├── HANDOFF-TL.md                               # this doc
├── future-architecture-investigations.md       # Vale/POC summary
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
│       ├── bug-report-regex-*.md               # archived
│       └── (various phase plans + session notes)
├── rustc-lang-facade/                          # reusable framework crate
├── toylangc/                                   # demonstrator consumer
└── Cargo.lock
```

External worktrees (preserved as reference; don't delete):

```
/Users/verdagon/erw-poc-optimized-mir/          # POC #1 branch
/Users/verdagon/erw-poc-separate-crate-stubs/   # POC #2 branch
/Users/verdagon/erw-spike-modulellvm-wall/      # spike branch
```

The rustc fork:

```
~/rust                                          # forked rustc
                                                # branch: per-instance-mir
                                                # linked as rustup toolchain: rustc-fork
```

---

## 8. Commands to know

All assume `rustc-fork` toolchain is linked (check: `rustup toolchain list`).

```bash
# Full test suite (should: 67 + 129 + 15 = 211 passing, 0 failed, 0 ignored)
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw.txt
grep "test result:" /tmp/erw.txt

# Check compiler warnings
cargo +rustc-fork check -p toylangc 2>&1 | tee /tmp/erw.txt
tail -20 /tmp/erw.txt

# Run one test
cargo +rustc-fork test -p toylangc --test integration_tests test_name

# Rebuild the forked toolchain (rarely needed; see docs/usage/rebuilding-rustc-fork.md)
```

Build-redirect convention (per `CLAUDE.md`): pipe to a fixed
`/tmp/<session-name>.txt` via `tee`, inspect as a separate command.
Don't chain `| grep` on the same line.

---

## 9. Sanity-check before declaring handoff complete

Run this checklist before the outgoing TL is done:

- [ ] New TL has read Tier 1 + 2 at minimum.
- [ ] Outgoing TL has walked through sections 3 (open threads) and
      6 (in-head knowledge) with the new TL in a sync.
- [ ] New TL has at least skimmed `future-architecture-investigations.md`.
- [ ] New TL knows about the three preserved worktrees and what
      each contains.
- [ ] If the Vale response is going to be sent, sent before handoff
      (so outgoing TL can handle any follow-up). If being held, new
      TL knows the current posture.
- [ ] Full test suite runs green on the new TL's machine
      (`cargo +rustc-fork test -p toylangc`).

---

## 10. One-paragraph "take" on the project's state

The implementation is complete through Phase 8. The architecture
hypothesis — that a custom language can embed into rustc's
compilation pipeline via query providers + an LLVM backend without
reimplementing trait/generic resolution — is demonstrably true;
toylang compiles 15 real crates.io dependencies (uuid, indexmap,
regex, toml, serde_json, glob, rand, reqwest, clap, plus
`reqwest_get`). The framework works. Recent investigation work
(three POCs + one spike) mapped a zero-fork alternative in detail,
triggered by Vale's interest but not committed to for toylang
itself. The project is in a quiet, maintainable state; handoff
mostly consists of "know where things are" rather than "here's
what's on fire."
