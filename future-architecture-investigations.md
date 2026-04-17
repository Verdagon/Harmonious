# Future Architecture Investigations

Canonical summary of the architecture investigations the project has
run in response to external inquiries about the rustc fork. Everything
here is *potential* future work — this file exists so the next TL
can see the investigation landscape at a glance without having to
re-derive it from branches and reasoning docs.

**Top-line status as of 2026-04-17:** all three investigations complete;
response drafted but not sent; no decision taken on whether to pursue
fork reduction for toylang itself.

---

## Part 1: Why these investigations exist

A reviewer on the Vale project
(`/Volumes/V/ValeRustInterop/investigations/reducing-rustc-fork.md`)
asked whether `rustc-lang-facade`'s 5-patch rustc fork could be reduced
or eliminated. The ask was driven by Vale's deployment story: Vale
ships a precompiled binary, and "install our forked rustc" is
user-install friction they want to avoid. Toylang doesn't share that
distribution concern — the fork is fine for a research project — but
Vale is considering adopting the facade architecture for its next
interop generation, and a zero-fork path would make that story
dramatically cleaner for them.

The reviewer's document proposed specific alternatives (`override_queries`
on `optimized_mir`, a `CodegenBackend` plugin, `#[linkage = "external"]`
in a separate-crate stub model, `rustc_public` for read-side code) and
asked, in essence: why didn't we pick any of these? Were they
considered and rejected?

Honest answer on investigation: several of those alternatives were
never evaluated during the original design; a few others were rejected
for reasons that turned out to be specific to toylang's integration
model rather than universal. The three investigations below were run
to close those gaps with evidence rather than just reasoning.

---

## Part 2: The architectural invariant to preserve

Before getting into specific investigations, the one concept that
threads through all of them: **interleaving with rustc's
monomorphization phase is the load-bearing principle, not the
specific `per_instance_mir` query we use to do it.**

The full argument lives in `docs/reasoning/why-interleaved-monomorphization.md`
with a seven-case taxonomy of when consumer architectures require
interleaving vs when a pre-pass suffices. Short version: when
rustc-compiled code originates concrete `(consumer_item, concrete_args)`
tuples that toylang source can't enumerate (cases 1b / 3 / 4 / 5 / 6 of
the taxonomy), toylang has no way to pre-populate the reachable set —
rustc's collector is the only entity with full source visibility,
including Rust trait impls it resolves on the fly. So the facade
plugs into rustc's collector mid-walk, supplies concrete type tuples,
and lets rustc's own generic/trait machinery walk the transitive
closure.

Any fork-reduction path must preserve this interleaving or it loses
the architectural capability. All three investigations below preserve
it; the design-space analysis evaluates alternatives under the
interleaving constraint.

Vale's planned interop (Vale types participating in Rust trait
systems, Vale closures flowing into Rust generic APIs) sits firmly
in the cases that require interleaving — the investigations explicitly
name Vale's model as a case that requires this mechanism.

---

## Part 3: The three investigations

### 3.1 POC #1 — `override_queries` on `optimized_mir`

**Branch:** `poc/optimized-mir-override`
**Worktree:** `/Users/verdagon/erw-poc-optimized-mir/`
**Commits:** `b425094` → `1ee3800` → `119d287` → `e597fdd`
**Full findings:** `findings.md` on the branch

**Goal:** replace `per_instance_mir` (fork patches 1, 2, 4) with an
`override_queries` hook on rustc's existing `optimized_mir` query.
The consumer returns a generic MIR body for each consumer DefId;
rustc's collector substitutes per-Instance during its walk, same
machinery that handles every generic Rust function.

**Verdict: prototype-verified.** Dep discovery works end-to-end —
the collector substituted per-caller Params in the synthetic body
and queued the same Rust deps `per_instance_mir` produces today.
LLVM IR showed `declare` lines for transitively-reachable Rust items.
Every case that works today (trait-generic callbacks, nested
generics, consumer types flowing into Rust generics, drop glue)
carried over to the override path.

**Key negative finding:** the emission conflict manifested as
predicted. 257 linker errors across 117 failing integration tests +
6 failing standalone tests, split into 56 unique `__toylang_impl_*`
collisions + 19 unique `__toylang_accessor_*` collisions.
`__toylang_internal_*` unaffected. Release + LTO doesn't help. So
`override_queries` alone is a **3-patch reduction, not a zero-fork
solution**. Pair with the plugin (see 3.3) to eliminate patch 3.

**Key positive surprise:** the POC author's initial prediction that
the callback-boundary issue would block the whole approach turned
out to reshape into a clean design instead. The current Instance-keyed
`monomorphize_fn` callback does two jobs (external-dep registration
+ internal-callee stashing), only one of which a DefId-keyed override
can drive. The natural redesign splits the trait across two queries
that *already* fire at the right keying:

- `collect_generic_rust_deps(def_id)` — called from `optimized_mir`,
  DefId-keyed, returns Param-typed Rust deps.
- `notify_concrete_entry_point(instance)` — called from `symbol_name`,
  Instance-keyed, drives the recursive internal-callee walk.

No new rustc-exposed hook needed. Trait-level reshape, ~2-3 weeks
of consumer-side work.

**Cost estimate impact:** bumped the zero-fork estimate from 2-4
weeks to 4-8 weeks, because the callback-redesign line item wasn't
in the earlier number.

### 3.2 POC #2 — Separate-crate stub model

**Branch:** `poc/separate-crate-stubs`
**Worktree:** `/Users/verdagon/erw-poc-separate-crate-stubs/`
**Commits:** `2463248` → `acc62d2` → `2c004a6`
**Full findings:** `findings.md` on the branch

**Goal:** place `__lang_stubs.rs` in its own rlib instead of injecting
it into the user's test crate via `FileLoader`. This lets
`#![feature(linkage)]` + `#[linkage = "external"]` live entirely in
consumer-generated code, avoiding the "nightly feature flag leaks to
user's crate root" problem that motivated fork patch 5 (the partitioner
visibility hook).

**Verdict: prototype-characterized, with brownfield/greenfield split.**

*Mechanical confirmation:* `#![feature(linkage)]` at a real crate
root works as predicted. Stub rlib compiles cleanly. `nm` inventory
confirmed the wrappers are properly generated. Cargo orchestration
for a two-member workspace is tractable (~100 LoC of build.rs changes,
no wrinkles).

*Three architectural blockers surfaced for toylang-brownfield retrofit:*

- **Risk #1** (predicted): generic `#[inline(never)]` wrappers in an
  rlib with no local caller are never codegenned, because rustc's
  mono collector walks from rlib-local roots. Downstream bin routes
  link to the rlib's (non-existent) mono via
  `Instance::upstream_monomorphization` returning `Some(rlib_cnum)`.
- **Risk #8** (new): plain rustc compiling the stub rlib codegens
  every `unreachable!()` body, because fork patch 3's codegen-skip
  is useless without toylang processing installed for that compile.
  These bodies win at link time.
- **Risk #9** (new): `llvm_gen::generate_with_tcx`'s MonoItems walk
  filters with `def_id.as_local()`. Under single-crate-compile, all
  consumer DefIds are local; under separate-crate, they're in the rlib.
  The filter skips them; no extern wrappers get emitted.

All three are **single-crate-compile-model assumptions** that toylang's
backend makes. A greenfield consumer designing for separate-crate
from day one would not make these assumptions — the filter wouldn't
exist, forwarding bodies would be emitted by design, anchor generation
would be built in.

**Brownfield vs greenfield cost split:**
- Toylang brownfield retrofit: ~1-2 weeks of backend surgery
  (remediations A/B/C in the findings doc). More expensive than
  maintaining patch 5 (~2-3 days per rustc bump). Recommendation:
  stay with the fork for toylang.
- Greenfield consumer with plugin path: approximately zero
  marginal cost. Plugin mode naturally subsumes risks #8 and #9
  (plugin fires per crate-compile, decides what to emit); risk #1
  reduces to ~5 LoC of `upstream_monomorphization` override.

**Key practical output:** the separate-crate piece is *absorbed into
the plugin work* (3.3) for greenfield consumers, not a separate line
item. This tightens the Vale estimate rather than adding to it.

### 3.3 Spike — ModuleLlvm `pub(crate)` wall

**Branch:** `spike/modulellvm-wall`
**Worktree:** `/Users/verdagon/erw-spike-modulellvm-wall/`
**Full findings:** `findings.md` on the branch (882 lines)

**Goal:** characterize the one remaining unknown from the design-space
analysis — whether the `pub(crate)` boundary in `rustc_codegen_llvm`
blocks a wrapping plugin that wants to selectively delegate to LLVM
for Rust items while intercepting consumer items.

**Verdict: Medium.** The narrow wall (item-level interception inside
LLVM's CGU codegen) is genuinely blocked by `pub(crate)` types
(`CodegenCx`, `Builder`, `ModuleLlvm` constructors). But a coarser
design using **`collect_and_partition_mono_items` override** sidesteps
the wall entirely using only sanctioned `override_queries` hooks.

**The coarser-design shape:** override the partitioner query to
delegate to upstream (via the same `OnceLock<DefaultPartitionerFn>`
pattern the facade uses for `DEFAULT_OPTIMIZED_MIR` / `DEFAULT_*`),
then filter consumer DefIds from the returned CGU list. Rust CGUs go
to `LlvmCodegenBackend::codegen_crate` unchanged; consumer items are
codegenned through the plugin's own path. Outputs combine at
`join_codegen`. Total plugin-partitioner code ~150 LoC; plugin wrapper
extending the existing `codegen_wrapper.rs` pattern ~200 LoC.

**Most surprising finding:** the `LlvmCodegenBackend(())` sealed tuple
field is *leaky*. Calling `LlvmCodegenBackend::new().codegen_crate(...)`
returns `Box<dyn Any>` which downcasts to
`OngoingCodegen<LlvmCodegenBackend>`, whose `pub backend: B` field
exposes a concrete `LlvmCodegenBackend`. Since `LlvmCodegenBackend`
derives `Clone`, external code can extract an owned copy without
`unsafe`. This enables post-codegen LLVM operations (additional CGU
codegen, module introspection) without an upstream PR.

**Upstream PR option (optional, not on critical path):** a ~30-line
PR exposing 3 items (empty-ifying `LlvmCodegenBackend(())`, `pub` on
`ModuleLlvm::new` and `ModuleLlvm::llmod`) would unlock item-level
interception for consumers that want finer control. Landing
probability: item 1 ~80-85% weeks-level; items 2-3 ~25-35% weeks / ~60-70%
within 6 months (they touch LLVM handle-lifetime `unsafe` invariants,
likely drawing reviewer requests for a sanctioned wrapper trait).
PR draft in the spike's `findings.md`.

**Cost estimate impact:** 4-8 week zero-fork estimate held (didn't
grow further), and the mechanism for implementation became concrete.

---

## Part 4: What this means for fork-reduction as a whole

**For Vale** (greenfield, distribution-friction concern): zero-fork is
viable. 4-8 weeks of engineering. Specific path:

1. `override_queries` on `optimized_mir` (3.1) — 1-2 weeks plumbing
2. `LangCallbacks` trait job-split per 3.1's design — 2-3 weeks
3. `CodegenBackend` plugin with partitioner-override per 3.3's coarser
   design + `upstream_monomorphization` override per 3.2's ~5 LoC —
   2-3 weeks
4. Separate-crate stubs per 3.2's greenfield pattern — absorbed into
   step 3 at ~zero marginal cost

**For toylang** (brownfield, fork-maintenance-cost concern): don't
pursue. The 4-8 week migration exceeds the fork's ~2-3 days-per-bump
maintenance cost over any reasonable horizon. The architectural
investment would be for a capability (zero-fork distribution) toylang
doesn't need.

The reasoning doc (§4.1–4.3, Part 5) has the itemized cost-accounting
for both positions.

---

## Part 5: Current status — what's outstanding

### The Vale response

Draft at `response-reducing-rustc-fork.md` at the repo root.
**~168 lines. Not sent. Updated with all three investigations'
findings.**

Decision required from whoever picks up this investigation:
- Send as-is
- Tighten or rewrite
- Hold while Vale's timeline clarifies

Sending requires: knowing the Vale contact (see `/Volumes/V/ValeRustInterop/`
for the original inquiry document) and deciding whether it's the right
time in their own architecture planning cycle. The draft has been
reviewed by both POC authors and the spike author.

### Doc currency

The reasoning doc and architecture guide have been updated to reflect
all three investigations' findings. Specifically:

- `docs/reasoning/why-interleaved-monomorphization.md` — the
  seven-case taxonomy anchoring why the architecture exists.
- `docs/reasoning/rustc-fork-design-space.md` — Parts 2 (query
  choice) / 3 (linkage rejection) / 4.1-4.3 (alternatives catalog)
  / 4.6 (spike references) / 5 (cost accounting).
- `docs/architecture/rust-interop-guide.md` §10.6.4 (accessor
  immunity scope) and §10.6.5 (linkage rejection nuance) updated.

All three docs reference the specific POC branches and commit SHAs
for reproducibility. If future TL work continues the investigation,
those refs should stay in sync.

### Branches kept as reference

All three worktrees stay around indefinitely. No merges planned.
They serve as:

- **Reproducibility anchors.** Each branch has a `findings.md`, a
  `predictions-before-running.md`, and either scaffolding-grade code
  or prototype-level code. The specific commit SHAs are cited in the
  reasoning doc.
- **Context for the next investigation.** If Vale (or anyone else)
  eventually commits to pursuing the zero-fork architecture, the
  branches show exactly what was verified, what was characterized,
  and what remains as implementation work.

If the branches need to be cleaned up (disk space, cognitive load),
that's a judgment call that doesn't affect the reasoning-doc
citations — the `findings.md` content is captured in the branch's
commit history and can be resurrected from git regardless of whether
the worktree exists on disk.

### Open threads that could become investigations

None of these are active work; they're things this investigation
surfaced that could become follow-ups if someone decides to pursue:

- **`LlvmCodegenBackend` tuple-field PR as standalone toehold.**
  ~80-85% fast landing probability. Independent of whether zero-fork
  is pursued. Could be driven by Vale (or anyone) as a direction-
  aligned contribution to rust-lang/rust. Zero dependency on the
  larger architecture work. See spike `findings.md`'s PR draft for
  rationale.
- **`rustc_public` adoption for toylang consumer code.**
  Orthogonal to fork-reduction. ~1-2 weeks of consumer-side churn
  reduction (40-50% of rustc-internal usage migrates to the stable
  surface). Blocked on `rustc_public`'s own stabilization trajectory,
  which is the project-stable-mir team's schedule. Recommendation:
  track but don't pursue until `rustc_public` hits a stable-enough
  surface.
- **Synthetic `impl Trait` generic inference** — earlier docs
  suggested clap was blocked on this; the Phase 7 `clap_test`
  experiment proved the concern was wrong (clap worked first-try
  with explicit type-arg naming via `Command::new<&str>("app")`).
  Not an outstanding investigation; mentioned here because older
  docs may still reference it as a blocker.

---

## Part 6: For the next person picking this up

If you're picking this investigation up cold:

1. **Read `docs/reasoning/why-interleaved-monomorphization.md`** first.
   It's the architectural invariant. The whole investigation makes
   sense only in its context.

2. **Then read `docs/reasoning/rustc-fork-design-space.md`** —
   specifically Parts 1-5. That's the full design-space analysis.
   Each of the three investigations contributes to it directly.

3. **Optionally read the three `findings.md`s on the POC/spike
   branches** if you need specific evidence cited in the reasoning
   doc. You don't need to read them for top-level understanding.

4. **Look at `response-reducing-rustc-fork.md`** to see what's been
   drafted to Vale. That's the "outbound" form of the investigation.
   If you're deciding whether to send / rewrite / hold, this is the
   artifact under review.

5. **Don't re-run any of the three investigations.** They're
   prototype-verified or prototype-characterized as noted; unless
   rustc has meaningfully changed since the investigations (check
   the rustc-fork toolchain version), re-running would just reproduce
   known results.

6. **If Vale engages further**, the response draft is your starting
   point, not the reasoning doc. The reasoning doc is the substrate;
   the response is the conversation. Customize the response to
   whatever Vale's current posture is.

7. **If toylang's position changes** (e.g., the distribution-friction
   concern shifts to apply to toylang too), the reasoning doc's
   Part 5 cost accounting tells you what you'd need to commit to
   for zero-fork. 4-8 weeks, plus ongoing ~1 week per rustc bump
   for MIR-construction churn (irreducible, same as the fork path).

The whole investigation is complete and self-contained. If you want
to leave it alone, everything continues to work as-is (the fork is
maintained; Phase 7 is done; no active regressions). If you want to
engage, the reading order above gets you oriented in ~2 hours.
