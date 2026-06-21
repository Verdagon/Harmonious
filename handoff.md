# Handoff: Discovery/synthesis machinery + share_generics + inlining coverage

**Who this is for:** the next engineer (probably you, fresh-session) picking up the remaining
investigation threads. Assumes you've read
[`rust-interop-architecture.md`](rust-interop-architecture.md) at least once,
particularly §F.13 (cascade-fires-at-stub-rlib), §F.14.1 (Option 4 partition
filter retirement), §8.9.5 (discovered trait-impl instances), §25.2 (the
risk register, especially B14/B15/B16/B17), and §26.16 (SMPLZ arcanum). If
not, read those first.

**Status:** open threads identified during session 2026-06-19/20. None of these
threads is bug-fixing — they're refinement/investigation/expansion work.
Release-mode (the previous handoff's bug) is fully resolved as of `08f350e`.

**2026-06-21 progress note: §5.5 chain COMPLETE — Steps 1+2+3 shipped; Step 4 dropped as misdiagnosis.**

The full A.1+A.2+patch-5 retirement chain landed empirically. Three steps shipped, one dropped on architectural review:

- **Step 1 (Sky-frontend types_match)**: types_match extended to bridge `RustType ↔ Struct` / `RustType ↔ StructRef` when names + type_args match (cross-Sky-crate Sky types can be classified as RustType at the rustc-oracle lookup path and Struct at the sidecar-registry path; both are the same logical type). Plus walk_and_stash extended to use an effective_registry (LOCAL + UPSTREAMs merged) for cross-crate callee lookup, with `local_registry` discrimination to avoid duplicate emission of upstream-owned non-generic callees. Unblocks DQ-D (`test_multi_sky_generic`). Commit `20c87c1`.

- **Step 2 (narrower-§5.5)**: non-generic Sky bodies emit at owning crate's stub_rlib compile. Documented LTO trade-off (no cross-Sky/Rust inlining at lto=false; requires `lto = "thin"` or `"fat"`). New @SBMNBIZ arcanum captures the AvailableExternally-stub-must-not-be-inlined invariant. 8 matrix fixtures updated to assert tail-jump present. Commit `41f7ae4`.

- **Step 3 (full §5.5 partial)**: cascade-discovered trait-impl method bodies drain at the cascade-firing crate (stub_rlib compile) rather than at user_bin. A.1.X (capture-ship-replay drain) RETIRED. A.2 (`lang_upstream_monomorphizations[_for]`) RETIRED — empirically redundant because Sky's body emits at the same compile session as the call site, so LOCAL_CRATE = __lang_stubs naturally matches without the augmented map. Patch 5's `consumer_lang_active` query STAYS — load-bearing for F2's `cross_crate_inlinable` override (B16) and Option 4's codegen_fn_attrs gating, not the share-generics escape itself. The fork patch 5 in rustc is now empirically a no-op but stays at 5 patches. Commit `b09a90b`.

- **Step 4 (ReifyFnPointer extension)**: ❌ **DROPPED**. The handoff's premise was a category error: ReifyFnPointer casts tell rustc to discover and compile RUST deps Sky transitively calls. Using the same mechanism for Sky-INTERNAL deps would be wrong — rustc has no role in Sky-internal emission (Sky's fill_extra_modules handles that), and non-export Sky-internal items don't have DefIds (arch §9.3, §9.4). walk_and_stash is the correct Sky-side mechanism for Sky-internal discovery; there's no pressure to push it into rustc's world. A.1.Y (walk_and_stash) stays as legitimate Sky-side infrastructure.

**Net chain outcome**:
- A.1.X (capture-ship-replay user_bin drain): RETIRED.
- A.1.Y (walk_and_stash transitive Sky-callee discovery): STAYS — correct architectural place.
- A.2 (synthesize_upstream_monomorphizations + the per-DefId map override): RETIRED. ~70 lines of code dead-but-archived; cleanup deletion is a follow-up.
- Patch 5 (consumer_lang_active facade override): STAYS for sibling-dependency reasons. The rustc fork patch is a no-op under Step 3 — could retire as a separate fork-cleanup arc.
- Fork: stays at 5 patches.
- Verification: 192/0/1 cold + warm across all 4 commits in the chain.

**Test count progression**: 191/0/2 baseline → 192/0/1 final (the +1 is DQ-D's `test_multi_sky_generic` un-ignored and passing).

The chain proved the §5.5 revision's payoff is **real but partial** vs the handoff's projection. A.1.X + A.2 retirement is genuine architectural cleanup. Patch 5 retention is honest. The "rustc-natural model" framing the design-conversation anticipated is now empirically validated for non-generic and discovery paths; comptime-dependent items (Sky proper's future concern, not toylang) are the remaining motivation for §5.5 as originally designed.

### Patch 5 retirement — INVESTIGATED + BLOCKED (2026-06-21)

Attempted retirement: removed Sky's override of `consumer_lang_active` (made the default `false` provider win), kept the cross_crate_inlinable override using an inline `is_sky_active(tcx)` helper instead of `tcx.consumer_lang_active(())`. Tests: 3 case5 fixtures (`test_inline_case5_no_lto`/`_thin_lto`/`_fat_lto`) failed at RUNTIME with `unreachable!()` from the stub body. SBMNBIZ-style failure mode.

**Root cause finding** — invalidates the earlier "Step 3 makes patch 5 a no-op" claim:

Patch 5's gated share-generics escape clause is load-bearing for **rustc's NATURAL `upstream_monomorphizations_for` map**, not just for Sky's A.2-synthesized entries. Rust generic items emitted at `__lang_stubs`'s share-generics-true compile (e.g., `Vec::<i32>::new`, `<MyCounter as Clone>::clone`) get recorded in rustc's natural map via the standard rmeta path. At the user_bin's share-generics-false compile, the gate's main condition (`!share_generics() && inline != Never`) returns `true` → would return `None` → mangler picks LOCAL_CRATE disambig → mismatch with where the body actually lives.

The patch 5 escape clause prevents this short-circuit by consulting `consumer_lang_active(())` and checking the natural map. Under Sky-active compiles, when the natural map has the entry, the escape fires → mangler picks the right upstream disambig.

The Step-3 reasoning ("augmented map is empty so escape is a no-op") was right for SKY items (A.2 retired, no augmented entries). But for RUST items, the natural map has entries, and the escape clause is genuinely load-bearing.

**What's load-bearing:**
1. The fork patch in `Instance::upstream_monomorphization` (the gated escape clause) — prevents share-generics short-circuit for items rustc's natural map has entries for.
2. Sky's facade-side `lang_consumer_lang_active` provider — returns `true` for Sky-active compiles so the escape clause fires.
3. The query declaration + default provider in the fork — infrastructure for (2).

All three pieces stay. Fork stays at 5 patches.

**Sky-side cleanup that DID land (commit forthcoming):**
- New `pub fn is_sky_active(tcx) -> bool` helper in `rustc-lang-facade/src/lib.rs` — consolidates the marker-walk logic.
- `lang_consumer_lang_active` simplified to call `is_sky_active(tcx)` (was duplicating the same logic inline).
- `lang_cross_crate_inlinable` + `lang_extern_cross_crate_inlinable` now call `crate::is_sky_active(tcx)` instead of `tcx.consumer_lang_active(())`. Cheaper (skip query plumbing); cleaner abstraction.
- No fork changes.

**Empirical wins:** 192/0/1 stays. Slightly less coupling between cross_crate_inlinable and the query system. Marker-walk logic exists in one place.

**Future-direction note:** if a future Sky/Sky-proper architecture relaxes the share-generics constraint at user_bin (e.g., force share_generics=true via a heuristic similar to the `__lang_stubs` heuristic), the gated escape clause could potentially retire. That's a separate investigation, not gated on §5.5 chain work.

---

**2026-06-21 progress note: §5.5 Step 2 (narrower-§5.5) SHIPPED with a documented LTO trade-off + new @SBMNBIZ arcanum.**

Step 2 of the A.1+A.2+patch-5 retirement chain landed. Non-generic Sky bodies now emit at their owning crate's stub_rlib compile (via `consumer_fill_modules`'s extended populate path + eager-emit of local trait-impls). A.1.X (capture-ship-replay) retires for non-generic trait impls; A.1.Y (transitive-callee stash), A.2, and patch 5 stay (those address generic-instantiation concerns, which Step 3 will tackle).

**Empirically-surfaced finding (the "thin-local LTO" mechanism):** the inlining matrix's `_no_lto` fixtures previously relied on a mechanism that wasn't documented in the arch doc — rustc's ThinLTO running BETWEEN its own CGUs WITHIN a single rustc invocation, even at `lto = false`. Cargo calls this "thin-local LTO" (per `lto = false`'s docs). Step 2 moves Sky's body to a different rustc invocation, so thin-local LTO can no longer bridge the call. **Decision (locked by user direction):** accept the trade-off; cross-Sky/Rust inlining now requires `lto = "thin"` or `"fat"`. Updated 8 matrix fixtures (case{1a,2,4,6}_no_lto + case4_o{1,2,s,z}) to assert tail-jump present rather than absent, codifying the new behavior. Documented in arch doc §F.16 (new subsection with the 5-levels-of-inlining table) and §26.17 (new @SBMNBIZ arcanum).

**New arcanum: @SBMNBIZ ("Stub Body Must Not Be Inlined").** The Option 4 AvailableExternally pattern creates a potential UB hazard: if LLVM inlines the unreachable!() stub body into a real caller, the caller's continuation becomes unreachable → optimizer removes downstream code → UB. The discipline: at every compile session where rustc emits an AvailableExternally stub body, EITHER Sky's fill_extra_modules emits a real-body shadow (IR linker picks External over AvailableExternally), OR no caller exists in that session's IR (per @F.13's gate). Both conditions hold under Step 2; codified in `docs/arcana/StubBodyMustNotBeInlined-SBMNBIZ.md` and registered in §26's arcana table.

Verification: 191/0/2 cold + warm. 2 ignored = pre-existing flake (`test_inline_case3_inline_never`, LLVM aggression on a single Priority B fixture) + DQ-D blocked fixture (`test_multi_sky_generic`, Sky-frontend type bug — Step 1 of the chain will unblock).

---

**2026-06-21 progress note: Option 4 SHIPPED.** Thread A's actionable
half-day item landed. The `collect_and_partition_mono_items` override
(formerly at `rustc-lang-facade/src/queries/partition.rs`, ~107 lines) is
retired; replaced by a `codegen_fn_attrs` override at
`rustc-lang-facade/src/queries/codegen_fn_attrs.rs` (~95 lines) that sets
`linkage = Some(Linkage::AvailableExternally)` on consumer-defined items.
Same outcome (rustc emits no `.o` symbol for Sky-defined items; consumer's
`fill_extra_modules` body is the sole def at link), smaller surface, more
architecturally consistent with the per-item rather than per-CGU pattern
the rest of the facade uses. Verification: all 191 integration tests + 40
inlining matrix fixtures + 106 unit tests pass cold and warm. Open-question
check passed: the MIR inliner does not pull `unreachable!()` into Sky's
callers within the stub_rlib compile — case 1a/2/4/6 main bodies still
constant-fold (`mov w8, #42`) cross-crate at every LTO mode + -O3. One-time
gotcha hit: rustc's incremental cache `Found unstable fingerprints` after
the migration; wipe `toylangc/target/integration-projects-cache` once.
Updated arch doc: §F.14.1 (new subsection), §25.2 B17 (new entry), §5.3
(update note pointing at §F.14.1), §3.2 patch 5 fix (stale B17 reference
→ B14). A.1 (capture-ship-replay) and A.2 (`synthesize_upstream_monomorphizations`)
remain load-bearing as expected — Option 4 only retires A.3.

**2026-06-21 progress note: §5.5 revision investigation (Rounds 1+2 +
manual DQ-D probe). Outcome: revision DEFERRED; 3 small wins shipped;
1 Round 1 finding RETRACTED.**

A two-round multi-agent investigation explored revising §5.5 (the locked
"all Sky bodies emit at user_bin compile" policy) to instead match rustc's
natural emission model (each Instance emits at the crate where it's first
reachable as a concrete Instance). The motivation: retire A.1+A.2's
discovery-vs-emission-site bookkeeping.

What landed from this investigation:

- **`#![no_builtins]` retirement** — `toylangc/src/build.rs:329` no longer
  emits the attribute on stub rlibs. Empirically verified safe by Round 2
  E2: full matrix passes without it (LLVM IR linker already prefers Sky's
  External-linkage body over the AvailableExternally placeholder
  unambiguously post-Option-4). Arch §F.3 marked RETIRED with archive
  context. Net: one less defensive belt-and-suspenders mechanism.

- **Callback log per-compile tagging** — `callbacks_impl.rs::consumer_fill_modules`
  now prefixes each TOYLANG_LOG_PATH entry with `[compile=rlib]` or
  `[compile=userbin]`. Defensive against any future change that causes
  both compiles to emit duplicate-shaped entries. Test parsers updated
  to strip the prefix via a `strip_compile_tag` helper.

- **Round 1 D1 finding RETRACTED.** Round 1's destructive probe claimed
  "A.2 is dead weight — 210 tests pass without it." Round 2's V3 + E3
  re-ran with proper cache discipline (full wipe of BOTH
  `target/integration-projects-cache/` AND per-fixture `.toylang-build/`)
  and showed disabling A.2 produces 8 deterministic link failures in
  generic-impl-block fixtures. A.2 IS load-bearing. Round 1's "all pass"
  was a cache-staleness artifact.

What the investigation confirmed about the locked design:

- **A.1 (capture-ship-replay) is structurally irreducible** under the
  locked architecture (E5: disabling breaks 5 fixtures — generic
  transitive consumer→consumer callees can't be discovered by rustc's
  mono walker through `unreachable!()` bodies).
- **A.2 + patch 5 are independently load-bearing** (E3: 8 fixtures need
  A.2, 28 need patch 5, 6 need both). The fork CANNOT shrink from 5 to
  4 patches via this route.
- **D2's claim that toylangc emits Sky bodies at stub_rlib is refuted
  for main** (V1a + V1b + V2a + V2b all independently confirmed: the
  `is_user_bin_compile` gate at `callbacks_impl.rs:1124` cleanly
  short-circuits before any IR emission at rlib compile).

Why the §5.5 revision itself is deferred:

The novel architectural case the revision opens — cross-Sky-library
generic instantiation where lib_a defines `wrap<T>`, lib_b defines a
type `Thing`, and dqd_app calls `wrap<Thing>(make_thing(42))` — was
constructed as a new fixture (`dqd_app/`, `dqd_lib_a/`, `dqd_lib_b/`
under `tests/integration_projects/`) and surfaced a SKY-FRONTEND
type-classification bug, UNRELATED to §5.5's emission policy:

```
function 'main': ArgTypeMismatch {
  func_name: "wrap",
  expected: RustType { name: "Thing", type_args: [] },
  got: Struct { name: "Thing", field_types: [I32] }
}
```

When dqd_app references `Thing` as a type argument to `wrap<Thing>`,
the validator classifies it as `RustType` (because dqd_lib_b's stub
rlib exposes Thing via Rust's `pub use` path). When dqd_app receives
the value from `make_thing(42)`, the validator classifies it as
`Struct` (because dqd_lib_b's sidecar registry knows Thing's full
Sky structure). The `types_match` predicate at
`toylangc/src/toylang/type_resolve.rs:164` bridges `StructRef ↔
Struct` but NOT `RustType ↔ Struct`, so validation fails before
per_instance_mir or any §5.5-related machinery is reached.

The fixture (`test_multi_sky_generic`) is committed with `#[ignore]`
and a detailed comment so future investigators have a ready probe.
The Sky-frontend fix is small (extend `types_match` to bridge
`RustType ↔ Struct` when names + type_args match), but it's outside
the §5.5 scope and not load-bearing for any current user-visible
behavior.

**Recommendation for any future §5.5 revision attempt:** fix the
Sky-frontend type-classification bug first (so DQ-D is even runnable),
then re-run the empirical investigation. The investigation framework
(workflows + structured findings + paired verifiers + worktree
isolation) is reusable — see `/private/tmp/claude-501/.../tasks/wklgsxl9j.output`
for Round 2's full 15 findings and synthesis verdict.

**Investigation-quality lessons** (general, beyond §5.5):
- Always wipe BOTH the shared cache AND per-fixture `.toylang-build/`
  before destructive probes. Round 1's D1 didn't wipe `.toylang-build/`
  and produced a totally wrong conclusion.
- Worktree-isolated agents may be branched from old revisions — pair
  with the agent itself verifying the worktree's commit SHA against
  main before drawing architectural conclusions. Round 2's E4 produced
  contradictory findings vs V1/V2 because it was in a pre-Phase-3
  worktree (DQ-I unresolved).
- For load-bearing claims, pair two independent verifiers writing
  to a shared schema. Round 2's V1a/V1b and V2a/V2b paired verifier
  pattern caught D2's miscategorization cleanly.

**2026-06-20 progress note:** Thread C **fully shipped + F1 + F2 both
RESOLVED + 11 pre-existing test failures retired + Thread A deep
investigation surfaced 8 more options than originally documented.**
What landed:

- **Inlining matrix infrastructure** (Thread C): harness, 41 fixtures, 40
  matrix tests, rustc-demangle dev-dep. Surfaced both F1 and F2 (matrix
  did its job). Files: `toylangc/tests/common/inlining_harness.rs`,
  `toylangc/tests/integration_projects/inlining/`, `toylangc/tests/integration_projects.rs`.
- **F1: Sky-export LTO inlining gap — CLOSED.** Root cause was `#[inline(never)]`
  on Sky-item stub source. Both historical rationales obsolete: patch 5
  closes the share-generics gate; arch §F.1 says `#[inline(never)]` is wrong
  as the LTO race fix (`#![no_builtins]` + Path B handle it). Removed from
  three Sky-item sites in `stub_gen.rs`. Phase-6 stdlib helpers retain
  the attribute for a different reason (§6.6.5). Protection fence shipped:
  `toylangc/tests/architecture_fence.rs::stub_gen_no_inline_never_on_sky_items`
  pins the count of `#[inline(never)]` emission sites at 2 (the Phase-6
  helpers). If anyone re-introduces the attribute on a Sky-item site
  without flipping the v2-precompiled-bodies trigger, the test fires.
- **F2: case 3 / case 5 disambig bug at -O3 — CLOSED.** Root cause was
  NOT a disambig bug but `cross_crate_inlinable` query returning true,
  causing rustc to emit `available_externally` linkage (body for inlining
  only, no `.o` symbol). Sky's emitted call sites can't inline through
  rustc's IR path, so the references dangled at link. Fix: new query override
  at `rustc-lang-facade/src/queries/cross_crate_inlinable.rs` covering both
  `queries.cross_crate_inlinable` (local items) and `extern_queries.cross_crate_inlinable`
  (upstream items from rmeta) — returns `false` when `consumer_lang_active(())`
  is true. Pass-through preserved (override gated by marker detection).
  See arch §25.2 B16 for full rationale.
- **11 pre-existing test failures — CLOSED.** The callback-log family
  (test_diamond_call_pattern, test_generic_deep_walk, etc.) and the
  layout-stderr family (test_point_layout, test_t_of_r_layout, etc.)
  were all the same cold-vs-warm-cache issue. Rustc's on-disk incremental
  cache replays `per_instance_mir` and `layout_of` query results from
  disk on warm cache, skipping Sky's provider invocation — the callback
  log stays empty; the `layout_of intercepted for: <Type>` eprintln
  doesn't fire. Fix: set `CARGO_INCREMENTAL=0` in the three helpers
  (`run_integration_project_check_callbacks`,
  `run_integration_project_check_build_stderr`,
  `collect_generic_rust_deps_firings`). Cold AND warm runs now pass
  reliably.

Matrix final state: 40 passing, 0 failing, 1 ignored (the one ignored is
an LLVM-honors-`#[inline(never)]`-or-not flakiness on a single Priority B
fixture; documented inline). Full integration suite: 191 passing, 0
failing, 1 ignored — clean baseline both cold and warm.

**Thread A deep investigation (2026-06-20)** surfaced 8 architectural
options for retiring the partition filter (A.3) beyond the 3 paths the
original handoff documented. The single most promising is **Option 4 —
override `codegen_fn_attrs` to set `linkage = Some(AvailableExternally)`
on Sky-export stubs.** Rustc emits IR (for inlining) but no `.o` symbol;
Sky's `fill_extra_modules` body wins. No partition filter needed; existing
`#![no_builtins]` blocks LTO inliner from pulling `unreachable!()`; F1's
LTO inlining promise preserved. **~half day to prototype + verify.**
~108 lines of `partition.rs` retire. A.1 + A.2 stay (different concerns).
For full A.1+A.2+A.3 retirement: **no viable path** under Sky's locked
architecture. The historical Option 8 sketched per_instance_mir returning
real MIR via a Sky→rustc-MIR emitter; that approach is **rejected
permanently** because it surrenders Sky's control of LLVM output to
rustc's codegen pipeline. Sky's design (arch §5, §F.15) requires Sky's
LLVM backend to own emission directly via patch 4 — that's a load-bearing
property, not an accidental layering. A.1 and A.2 stay; the next
meaningful step on Thread A is either incremental hygiene (audit A.2 for
redundant synthesis entries; consolidate the three predicates into one
"Sky-owned set") or accepting that the discovery/synthesis machinery is
the permanent cost of keeping LLVM control. See "Thread A — deep
investigation findings" for the full menu and the Option 8 rejection
rationale.

---

## Forward plan: retire A.1 + A.2 + patch 5 entirely

This section sketches the hoped-for endgame for Thread A. The §5.5
investigation surfaced that ALL of A.1 (capture-ship-replay + transitive
consumer-callee stash), A.2 (synthesize_upstream_monomorphizations),
and patch 5 (consumer_lang_active gated share-generics escape) exist
fundamentally because of one architectural decision: §5.5's "all Sky
bodies emit at user_bin." Reverse that decision and the cross-session
bookkeeping dissolves.

### Why §5.5 was locked the way it is — context from the design conversation

§5.5's locked-everything-at-user_bin policy was **explicitly flagged as
a starting-point simplification, with the optimization end-state being
exactly the chain below.** Verbatim from the design conversation
(`design-convo-log.md:16223`, the locked-decision discussion of the
multi-rlib model):

> "The harder question multi-rlib raises is *where compile-time evaluation
> runs.* Cleanest model: comptime always evaluates at the top-level skyc
> invocation (the final binary's compile). Sky libraries ship stubs +
> Sky source for comptime-relevant items; users effectively recompile
> generics at point of use. ... **You can complicate this later
> (precompile-bodies-when-possible as an optimization), but starting
> simple is right.**"

And reinforced at `design-convo-log.md:16944`:

> "If you wanted 'comptime runs once at lib_a's compile and the results
> are baked into lib_a's rlib,' you'd need to enumerate every
> instantiation Sky's downstream consumers will use — which is exactly
> the pre-pass model that the seven-case taxonomy showed doesn't work
> for bidirectional interop. Per_instance_mir at downstream-compile-time
> is the right place for Sky's arbitrary-typed comptime."

Distilled: §5.5's rationale is **about comptime**, not about a
fundamental architectural correctness concern. The argument was that
Sky's arbitrary-typed comptime needs concrete args (which only exist at
downstream-compile time per the 7-case taxonomy), so comptime-dependent
bodies have to emit downstream — and the simplification was to put
*everything* downstream rather than discriminate comptime-dependent
vs not.

**Toylang has no comptime**, so 100% of toylang items are eligible for
the "precompile-bodies-when-possible" optimization. For Sky proper, the
discriminator becomes "does this item's body depend on comptime
evaluation?" — comptime-dependent items keep emitting at downstream
(per the original rationale); non-comptime-dependent items emit at
their owning crate (per the optimization). The chain below applies
directly to toylang and structurally to Sky's non-comptime items.

### Structure of the chain

The reversal can't happen in one step — the dependencies and verifiable-
prerequisites partition into a chain. Each step is independently
shippable and verifiable; each unlocks the next. None past Step 1
is empirically verified; the chain is projected from architectural
analysis, not a working prototype.

### The chain

| Step | Action | Effort | Retires | Gates on |
|---|---|---|---|---|
| 1 | Fix Sky-frontend `types_match` to bridge `RustType ↔ Struct` for cross-Sky-crate Sky types | ~0.5 day | Nothing directly — but unblocks DQ-D so Step 3 becomes empirically testable | None |
| 2 | **SHIPPED 2026-06-21.** Narrower-§5.5 (non-generics only): consumer_fill_modules emits owned non-generic items at every compile session it owns (stub_rlib + user_bin both populate now); eager-emit of local non-generic trait_impls added; internal symbols also pinned in @llvm.used. **Actual cost: ~1 day** (vs ~0.5 estimated) — overshot because the thin-local LTO mechanism (arch §F.16) wasn't priced into the original plan, requiring 8 matrix fixtures to be flipped + the new @SBMNBIZ arcanum + arch doc updates. **Retires:** A.1.X for non-generic trait impls. **Documented trade-off:** cross-Sky/Rust inlining now requires `lto = "thin"` or `"fat"` (no thin-local LTO bridge across rustc invocations). | None — can ship today, independent of Step 1 |
| 3 | Ship full §5.5 (generics included): same discrimination but per-Instance owning-crate lookup; generics emit at first-reachable site | ~3-5 days | A.1's capture-ship-replay entirely (discovery + emission converge); A.2 likely (External-linkage items recorded naturally in rmeta, R1's filter passes); patch 5 likely (share_generics natural map non-empty, gate's short-circuit doesn't bite); fork may shrink to 4 patches | Step 1 (so DQ-D is verifiable) |
| 4 | ❌ **DROPPED 2026-06-21.** The premise was confused: ReifyFnPointer casts tell rustc to discover and compile RUST deps Sky transitively calls. Using the same mechanism for Sky-INTERNAL deps would be a category error — rustc has no role in Sky-internal item emission (Sky's fill_extra_modules handles that), and non-export Sky-internal items don't have DefIds in the first place (arch §9.3, §9.4). walk_and_stash is the correct Sky-side mechanism for Sky-internal transitive callee discovery; there's no pressure to push it into rustc's mono walker. Walk_and_stash STAYS as legitimate Sky-side discovery infrastructure. | n/a |

### What "A.1" actually contains

A.1 has two mechanistically-distinct pieces, conflated under one name in
the original handoff. Each retires under a different step:

- **A.1.X — Cross-session capture-ship-replay**
  (`capture_discovered_trait_impl_instances` + sidecar serialization +
  `on_sky_lib_loaded` deserialization + populate-time drain).
  Captures trait-impl Instances at stub_rlib compile, ships via sidecar,
  drains at user_bin to emit. **Retires under Step 2 for non-generic
  items; retires entirely under Step 3.** Mechanism: when Sky emits at
  the discovering crate (not user_bin), the cross-session bridge is
  redundant — discovery and emission live in the same compile.
- **A.1.Y — Intra-compile transitive consumer-callee stash**
  (`walk_and_stash_internal_callees`). Recursively walks Sky's typed
  AST to discover Sky-internal callees so Sky's `fill_extra_modules`
  knows what bodies to emit. **STAYS — correct architectural place,
  not retirable.** The original handoff's "retires under Step 4" claim
  was a category error: per_instance_mir's synthetic-body ReifyFnPointer
  casts are a one-way arrow Sky→rustc reporting RUST deps Sky
  transitively calls (so rustc compiles them). Sky-internal items
  aren't rustc's concern (Sky's fill_extra_modules handles them) and
  non-export Sky-internal items don't have DefIds in the first place
  (arch §9.3, §9.4) — you literally cannot ReifyFnPointer-cast them.
  A.1.Y is intra-session, intra-Sky discovery for Sky's own emission
  accounting; there's no architectural pressure to push it into rustc's
  mono world. See the "per_instance_mir's one job" principle at the
  top of the §5.5 chain progress note (line 15-ish): *"Sky's
  per_instance_mir at mono time has one job: walk Sky's call graph to
  report back the Rust things Sky transitively calls. Sky-internal
  callees are not its concern."* Future forward-plans should anchor
  on this principle to avoid resurrecting Step-4-shaped misdirected
  proposals.

### Full retirement scenario (CORRECTED 2026-06-21 — Step 4 dropped)

Original projection said Steps 1+2+3+4 would retire A.1, A.2, and patch
5. The empirical outcome at commit 51b7221 is partial in reality —
honest accounting below. The original "fully retires" claims are
preserved struck-through for archive value; the [STATUS] notes show
what actually happened.

- ~~**A.1 fully retires.**~~ **A.1.X retires; A.1.Y stays correctly.**
  A.1.X (cross-session capture-ship-replay) retires under Step 3 — the
  cascade-firing crate drains its own discoveries inline. A.1.Y
  (walk_and_stash_internal_callees) is intra-session intra-Sky
  discovery for Sky's own emission accounting — correct architectural
  place, was misdiagnosed in the original handoff as retirable. See
  the A.1.Y description above and the "per_instance_mir's one job"
  principle for the framing that prevents this misdiagnosis recurring.
- **A.2 fully retires.** [STATUS: ✅ shipped] Under Step 3, Sky's
  body emits at the same compile session as the rustc-side call site →
  LOCAL_CRATE = `__lang_stubs` naturally matches without the augmented
  map. Override is commented out at `queries/mod.rs:91-92` and `105-106`.
- ~~**Patch 5 likely retires.**~~ **Patch 5's facade query
  (`consumer_lang_active`) STAYS — load-bearing for F2/B16's
  `cross_crate_inlinable` override + Option 4's `codegen_fn_attrs`
  gating.** Patch 5 in the rustc fork stays but is a no-op under Step 3
  because the augmented map is empty (A.2 disabled) → the gated escape
  clause never fires. Could potentially retire as a separate
  fork-cleanup arc; consumer_lang_active query itself stays.
- ~~**Fork could shrink from 5 patches to 4.**~~ **Fork stays at 5
  patches.** The gated share-generics escape in `Instance::upstream_monomorphization`
  is the only patch 5 retirement candidate, and it's just dormant
  rather than removable (the consumer_lang_active query that gates it
  is load-bearing for other reasons).
- **Architectural narrative simplifies dramatically.** Today's story is
  "Sky has three load-bearing bookkeeping layers because §5.5 forces
  emission at user_bin." After the chain: "Sky's emission matches
  rustc's natural model — each item emits where it's first reachable.
  No cross-session bookkeeping needed."

### Honest caveats

- **Nothing past Step 1 is empirically verified.** Round 2's E1 was
  blocked from testing DQ-D (worktree-93-commits-behind issue). My
  attempt to run the DQ-D fixture from main surfaced the Sky-frontend
  `RustType ↔ Struct` bug at validation time, which fires before any
  §5.5-related machinery. So the entire chain past Step 1 is
  architectural projection, not empirical proof.
- **A.2 and patch 5 retirement under Step 3 is "likely", not "proven".**
  R1's finding about the rmeta filter was specifically about
  AvailableExternally linkage. Under Step 3, items the owning crate
  emits would carry External linkage, so R1's filter passes — but
  there may be other rustc-internal paths I haven't audited that
  also affect whether the natural map populates correctly. Empirical
  verification with the inlining matrix + integration suite is the
  judgment call, not analysis.
- **The Sky-frontend fix (Step 1) is small but might surface
  cascading issues.** `types_match` is a load-bearing predicate. A
  naive extension to bridge `RustType ↔ Struct` could affect other
  call sites (Rust trait dispatch on Rust-classified types, FFI
  signatures, etc.). The fix shape needs review, not just a five-line
  patch.
- **Steps 2 and 4 are independent of each other and of Step 1.**
  Either can land first; their wins are independent. Step 2 also
  serves as a "narrower revision still works" empirical check that
  builds confidence for Step 3.

### Recommended ordering

1. **Step 2 first** (today, independent). Real bounded win. Validates
   the codegen_fn_attrs is-local discriminator pattern. Cleaner
   narrative even if the rest of the chain stalls.
2. **Step 1 second** (after Step 2). Small bounded fix that unblocks
   DQ-D verification. If `types_match`'s extension is harder than it
   looks, this is where we find out — and the cost of the discovery
   is bounded.
3. **Step 3 third** (after Step 1 verifies DQ-D works). The big
   architectural cleanup. Gate on Step 1's empirical success.
4. ~~**Step 4 anywhere** (independent). ReifyFnPointer extension can
   land independently as opportunistic cleanup.~~ **DROPPED** —
   category error per the corrected A.1.Y framing above. Don't propose
   this kind of step in future plans.

### What success looks like (Thread A)

The architecture doc's §5.5 + §F.13 + §F.14 + §F.14.1 reduces from
~5 subsections explaining cross-session bookkeeping to a single short
section stating "Sky's emission matches rustc's natural model for items
without comptime dependencies; comptime-dependent items emit at the
downstream consumer's compile per §5.5's original comptime-driven
rationale." The risk register loses B17 (closed) and shrinks B14/B16.
The fork drops to 4 patches. The handoff doc's Thread A section retires
entirely or becomes a historical record.

This is **the explicitly-anticipated optimization** from the design
conversation, not a deviation from the locked architecture. §5.5's
locking commentary said "you can complicate this later
(precompile-bodies-when-possible as an optimization), but starting
simple is right" — the chain above IS that complication.

If the chain stalls partway (e.g., Step 1 reveals the Sky-frontend
fix is bigger than expected, or Step 3's empirical verification
surfaces unexpected breakage), the partial wins from Steps 2 + 4
still ship and the chain documents the deferral cleanly for a future
investigator.

---

## TL;DR

Closed (no action needed): F1, F2, Thread C, Option 4 (A.3 retirement).

One open thread remaining:

1. **share_generics handling (Thread B)** — current support is forced-on
   at `__lang_stubs`, hard-error if user disables it there, otherwise
   user choice. Decide if we should support/honor it differently in
   other configurations. Cost: ~1 day investigation + decision. Lowest
   urgency: no current user pain, just design-space hygiene.

A.1 (capture-ship-replay) and A.2 (synthesize_upstream_monomorphizations)
are the permanent cost of keeping Sky's LLVM control. The historically
considered "Option 8" (Sky → rustc-MIR emitter) is **rejected** — Sky's
backend must own LLVM emission directly via patch 4 (arch §5, §F.15); we
do not surrender that to rustc's codegen pipeline. A.1+A.2 stay.

Already shipped this arc:

- **Inlining test matrix (Thread C)** — ✅ **SHIPPED 2026-06-20.** 40
  passing / 1 ignored (LLVM `#[inline(never)]` aggression flake) / 0
  failing across `toylangc/tests/integration_projects/inlining/`. Surfaced
  F1 + F2; remained as the verification fence for Option 4.
- **Finding F1 — Sky-export LTO inlining gap. ✅ CLOSED 2026-06-20.**
- **Finding F2 — case 3 / case 5 disambig bug. ✅ CLOSED 2026-06-20** via
  `cross_crate_inlinable` query override.
- **Option 4 — A.3 partition filter retirement. ✅ CLOSED 2026-06-21** via
  `codegen_fn_attrs` override setting `AvailableExternally` linkage. New
  file at `rustc-lang-facade/src/queries/codegen_fn_attrs.rs`; deleted
  file at `rustc-lang-facade/src/queries/partition.rs`. Arch §F.14.1, §25.2
  B17. The "Option 4 implementation guide" below is preserved as design
  history for reference; the work is done.

Recommended next priority if you have time: **Thread B** (share_generics)
since it's bounded and clean. Optional Thread C follow-up: extend matrix
to cover Sky-side `#[inline]` syntax (requires Sky frontend work) and
Sky-top Priority B variants (blocked on same). For Thread A, the remaining
hygiene work (audit A.2 redundancy; consolidate Sky-owned-set predicates)
is opportunistic — pick it up next time someone touches those files. Do
NOT pursue Sky → rustc-MIR emission paths: that surrenders LLVM control,
which is a locked architectural property.

---

## Prerequisites you must internalize

### Background fact 1: F.13 — the cascade fires at stub rlib, not user-bin

At user_bin compile, rustc's mono collector at
`rustc_monomorphize::collector::collect_used_items` gates on:

```rust
if tcx.is_reachable_non_generic(def_id)
   || instance.upstream_monomorphization(tcx).is_some()
{
    return false;  // skip walking
}
```

For Sky's `__toylang_main` (non-generic, lives in upstream `__lang_stubs`),
`is_reachable_non_generic` returns true → the collector **never calls
`per_instance_mir`** for `__toylang_main` at user_bin time. The cascade
that would discover `duplicate<Wrapper<i32>>` and `<Wrapper<i32> as
Clone>::clone` therefore fires **only at the stub rlib compile.**

Consequences:
- The stub rlib compile must capture discoveries Sky needs.
- The user_bin compile must consume them out-of-band (sidecar).
- This entire scaffolding (capture-ship-replay + synthesize +
  partition filter) exists because of this one rustc-collector
  behavior.

### Background fact 2: the three Sky-side coordination layers

| Layer | File | Purpose |
|---|---|---|
| **A.1 Capture-ship-replay** | `toylangc/src/toylang/callbacks_impl.rs::capture_discovered_trait_impl_instances` (line 1305) + `populate_toylang_instances_from_cgus` (the `for upstream in &upstream_clones` loop, line 519) | Carries SKY-OWNED trait method discoveries from stub rlib to user-bin. Solves: user-bin can't re-run the cascade. |
| **A.2 Synthesize-upstream-monomorphizations** | `rustc-lang-facade/src/queries/upstream_monomorphization.rs::lang_upstream_monomorphizations` | Augments rustc's default-built `upstream_monomorphizations_for` map with Sky trait-impl entries. Solves: rustc's default map is empty for these items (because of A.3). |
| **A.3 Partition filter** | `rustc-lang-facade/src/queries/partition.rs::lang_collect_and_partition_mono_items` | Removes Sky-defined items from rustc's CGU list so the LLVM backend doesn't try to codegen `unreachable!()` bodies. Solves: rustc would otherwise compile the stub source's `unreachable!()` into machine code that competes with Sky's real bodies. |

These are linked: A.3 makes A.2 necessary; A.2 only works because A.1
populated the data. Pulling out one link affects the others.

### Background fact 3: the new release-mode fixes (already shipped)

For context on what changed recently:
- **Patch 5** (in `~/rust/compiler/rustc_middle/src/ty/instance.rs`):
  added `consumer_lang_active(())` query + gated escape clause in
  `Instance::upstream_monomorphization`. Lets the v0 mangler consult
  the augmented map even at -O>=2 (where share_generics defaults
  false).
- **`__lang_stubs` heuristic** in `LangDriver::config`: forces
  share_generics=true for the stub rlib compile, so its cstore
  metadata records cascade-emitted Rust generic intermediaries (like
  `duplicate<Wrapper<i32>>`). Downstream user-bin compiles find these
  via rustc's standard `upstream_monomorphizations_for` lookup, no
  Sky synthesis needed.
- **SMPLZ pinning** via `pin_in_llvm_used` in
  `toylangc/src/llvm_gen.rs`: pins every Sky-emitted rustc-visible
  symbol in `@llvm.used` so LLVM optimize/LTO doesn't strip or
  demote them.

These work TOGETHER. None of them retire A.1, A.2, or A.3 — but they
do change what work each layer does in subtle ways. See Thread A for
details.

---

## Thread A: Discovery/synthesis/filter machinery investigation

### The big-picture question

Can any of A.1, A.2, A.3 be pruned, simplified, or eliminated entirely?

### Sub-question A.1: Is capture-ship-replay still load-bearing?

**What it does today:**
- At stub rlib's `consumer_emit_modules` time (post-cascade): walks the
  unfiltered partition for `MonoItem::Fn(instance)` entries, filters by
  `is_consumer_trait_impl_method`, writes records into
  `registry.discovered_trait_impl_instances` (a vec of
  `DiscoveredTraitImplInstance { self_type_name, trait_name,
  method_name, concrete_args }`).
- Writes the registry into the sidecar via `serialize_sidecar`.
- At user_bin's `on_sky_lib_loaded` time: deserializes, pushes each
  discovery into `SkyUniverse.discoveries`.
- At user_bin's `populate_toylang_instances_from_cgus` time: drains
  every upstream's discoveries into `state.toylang_instances`. Sky's
  `fill_module` emits a body per instance.

**Why it might be load-bearing or not:**

Today it's clearly load-bearing. Sky's emission of clone bodies at
user_bin compile requires knowing which (self_type, trait, args) tuples
exist. The user_bin's collector can't tell us (F.13). The stub rlib
compile knows.

The question is whether a different mechanism could replace it. Two
candidates:

- **Could rustc's default cstore mechanism tell us?** Today no, because
  Sky's partition filter (A.3) prevents the stub rlib's metadata from
  recording these items. If A.3 were eliminated, rustc would record
  them naturally and the user_bin's collector would learn about them
  via the cstore. **A.1 would dissolve.**

- **Could Sky walk its own typed AST and figure it out?** Sky knows
  which traits its types impl. But Sky DOESN'T know which concrete
  instantiations rustc's mono walker reaches — that depends on what
  Rust generic intermediaries pass through (`duplicate<Wrapper<i32>>`
  → triggers `<Wrapper<i32> as Clone>::clone`). Sky can't enumerate
  this without re-running the cascade.

**So:** A.1 is load-bearing as long as A.3 exists. Killing A.3 would
naturally retire A.1.

**Investigation tasks for A.1:**
1. Verify the claim above by tracing through `case6_app` (the cross-
   Sky-crate fixture): which discoveries are captured at case6_lib's
   stub rlib? Which at case6_app's stub rlib? Which at user_bin? Are
   any redundant or unused?
2. Check whether A.1 captures any items that ARE in rustc's default
   `upstream_monomorphizations_for` map (post the `__lang_stubs`
   heuristic). If so, those captures are redundant duplicates of
   information rustc would surface anyway.

### Sub-question A.2: Is synthesize-upstream-monomorphizations still load-bearing?

**What it does today:**

`lang_upstream_monomorphizations` (in
`rustc-lang-facade/src/queries/upstream_monomorphization.rs`) overrides
rustc's whole-map query. It:
1. Calls the saved default provider to get rustc's default map.
2. Calls `synthesize_upstream_monomorphizations` (consumer-provided)
   to get a `Vec<(DefId, GenericArgsRef, CrateNum)>` of synthesized
   entries.
3. Merges them into the default map.

`synthesize_upstream_monomorphizations`'s consumer implementation (in
`toylangc/src/toylang/callbacks_impl.rs::synthesize_upstream_monomorphizations`)
walks `SkyUniverse.discoveries`, looks up each trait-impl method's
DefId via `find_trait_impl_method_def_id`, and builds the
`(def_id, args, __lang_stubs_crate_num)` triple.

**Why it might be load-bearing or not:**

For SKY-OWNED trait methods (`<Wrapper<T> as Clone>::clone`): rustc's
default map is empty for these items because A.3 (partition filter)
removed them from the stub rlib's CGU list before metadata was
recorded. So the synthesized entries are necessary. **Load-bearing.**

For RUST GENERIC INTERMEDIARIES (`duplicate<Wrapper<i32>>`): rustc's
default map NOW has these entries thanks to the `__lang_stubs`
share_generics=on heuristic. So if Sky were synthesizing for them, the
synthesis would be redundant. **Possibly redundant.**

Look at the current implementation:

```rust
fn synthesize_upstream_monomorphizations<'tcx>(...) -> Vec<...> {
    let stash = rustc_lang_facade::sky_universe().discoveries_clone();
    // ... iterates `stash` (StashedDiscovery values) ...
    let Some(def_id) = crate::oracle::find_trait_impl_method_def_id(
        tcx, &d.trait_name, &d.self_type_name, &d.method_name,
    ) else { continue; };
    // ...
}
```

`find_trait_impl_method_def_id` is trait-impl-specific. The function
ONLY synthesizes for trait-impl methods. So it doesn't synthesize for
`duplicate` (a Rust generic). Already tight in this regard.

**Conclusion (tentative, verify):** A.2 is load-bearing for Sky trait
methods. There's no immediate pruning available. Could only be retired
if A.3 (which strips trait methods from CGUs → empty default map for
them) goes away.

**Investigation tasks for A.2:**
1. Confirm the conclusion above by running with logging in
   `lang_upstream_monomorphizations` to see which entries get added.
   At debug, vs at -O3, vs at the existing fixture corpus. Are any of
   the added entries redundant with the default map?
2. Audit whether `synthesize_upstream_monomorphizations` is called at
   every compile or only when the marker is present. Cross-check
   against the pass-through invariant.

### Sub-question A.3: Could the partition filter be eliminated?

**What it does today:**

`lang_collect_and_partition_mono_items` (in
`rustc-lang-facade/src/queries/partition.rs`) overrides rustc's
partition query. It:
1. Calls the saved default provider to get the unfiltered CGU list.
2. For each CGU, walks `MonoItem::Fn(instance)` entries.
3. Removes items where `is_consumer_defined_item(instance)` is true
   (= Sky-defined item, identified via marker check).
4. Returns the filtered CGU list to rustc.

Rustc's LLVM backend then codegens the filtered list. Sky-defined items
don't reach LLVM through rustc's pipeline; they go through Sky's
`fill_module` pipeline instead.

**Why it exists:**

If we didn't filter, rustc would compile the stub source's
`unreachable!()` bodies into actual machine code. Under non-LTO, the
linker would see two definitions of the same symbol (Sky's and the
stub's) and either fail with "duplicate symbol" or pick one
non-deterministically. Under thin/fat LTO, the IR linker would face
the same choice. The `#![no_builtins]` mechanism on stub rlibs
addresses the LTO case (excludes the stub rlib's bitcode from the LTO
pool), but doesn't address the non-LTO machine-code case.

**Could it be eliminated?**

Three theoretical paths:

a) **Upstream `#[codegen_backend_provides_body]` attribute** — would
   tell rustc "this function exists in source but the codegen backend
   provides the body; don't codegen the source's body." The earlier
   agent audit concluded this would require ~5 query overrides +
   companion changes (`cross_crate_inlinable`, `should_encode_mir`,
   `deduced_param_attrs`, `has_ffi_unwind_calls`, plus a Sky-IR
   fingerprint), making it bigger than "one small RFC." Multi-year
   upstream coordination. See arch doc §29.6.

b) **Sky-side replacement: per-function LTO exclusion attribute**
   (`#[exclude_body_from_lto]`). Would let stub bodies coexist with
   Sky's emissions because the stub bodies don't participate in LTO.
   Doesn't help the non-LTO case (stub bodies still emit to .o), but
   the linker would dead-strip them if Sky's body has the same name
   and is in another translation unit. This is essentially the
   `#![no_builtins]` mechanism at per-function granularity. Probably
   the smaller RFC.

c) **Sky-side compile-time stub-source rewrite** — at stub_gen time,
   wrap every Sky export in a `cfg(not(sky_provides_body))` guard, and
   have the facade pass `--cfg sky_provides_body` when Sky's machinery
   is active. The stub source then compiles to a different body shape
   (extern decl) when Sky is providing. No fork patch needed; pure
   skyc-side discipline. **This may be the cheapest path.** Worth
   exploring.

**Investigation tasks for A.3:**
1. Build a small prototype of path (c) — modify stub_gen to emit
   `extern "Rust" { pub fn foo() -> T; }` instead of `pub fn foo() ->
   T { unreachable!() }` when a specific cfg is set. Have the facade
   set that cfg. Verify that:
   - Sky's emission can satisfy the extern declaration.
   - The partition filter (A.3) is no longer needed.
   - The augmented map (A.2) for Sky trait methods is still needed
     (because rustc's default map still doesn't record extern decls
     as monos).
   - Whether discoveries (A.1) are still needed.
2. If path (c) works at the toy level, evaluate the implications:
   does it preserve LTO inlining? (extern decls don't carry MIR; LTO
   wouldn't have anything to inline.) Does it affect debuginfo?
3. Compare path (c) against path (b) (upstream RFC for
   `#[exclude_body_from_lto]`) on architectural cleanness.

### Recommended sequence for Thread A (original, pre-deep-investigation)

1. **First: verify the current claims** (~half day). Confirm A.1 and
   A.2 conclusions with empirical logging. Confirm A.3's necessity by
   trying to comment it out and seeing what breaks.
2. **Then: explore path (c) for A.3** (~3-5 days). If it works,
   sketch the migration plan. If it doesn't, document why for
   future readers.
3. **Then: act on findings.** If A.3 can be eliminated, A.1 also goes
   (verify, then delete). A.2 stays only for Sky trait methods. Lots
   of Sky-side code retires. The architecture doc §F.13/§F.14/§8.9.5
   collapses significantly.

### Thread A — deep investigation findings (2026-06-20)

The original 3 paths (a/b/c) above turn out to be incomplete. A deeper
investigation surfaced **8 architectural mechanisms** for retiring the
partition filter — listed in roughly increasing cost:

**Mechanism family — Suppress rustc's stub-body codegen**

- **(1) `#![no_codegen]` sibling attribute (upstream RFC).** Smaller
  RFC than path (a). `#![no_builtins]` already handles the LTO case
  by excluding the rlib bitcode from the LTO pool. A sibling
  `#![no_codegen]` attribute that does for `.o`/ELF what `no_builtins`
  does for LTO — per-crate rather than per-function (Path b was
  per-function).
- **(2) Per-item `#[linkage = "..."]` via stub_gen.** Rejected
  historically because `#![feature(linkage)]` is a crate-root attribute
  that leaks. **The rejection no longer applies to Sky** — Sky's stubs
  are in their own rlib, so the feature flag stays inside skyc-generated
  code. From `docs/reasoning/rustc-fork-design-space.md` Part 3.
- **(3) `#[no_mangle]` non-generic shim layer.** Non-generic `pub fn`
  with `#[no_mangle]` always gets external linkage. Works on stable
  Rust, no fork, no feature gate. Only addresses non-generics — doesn't
  help generics or trait-impl methods.

**Mechanism family — Linkage-based without filtering**

- **(4) Override `codegen_fn_attrs` to set `linkage = Some(AvailableExternally)`
  on Sky-export stubs. ⭐ RECOMMENDED.** Rustc emits IR (for inlining)
  but no `.o` symbol. Sky's `fill_extra_modules` body wins. No partition
  filter needed. The `mono_item.explicit_linkage(tcx)` fast-path at
  `rustc_monomorphize::partitioning.rs:749-751` short-circuits when
  linkage is explicit. Risk (cross-module inliner pulling
  `unreachable!()` into callers) is mitigated by existing `#![no_builtins]`
  for LTO and by available_externally semantics for non-LTO.
  **No new fork patch needed — `codegen_fn_attrs` is overridable.**
- **(5) Same as (4) but with `linkage = Some(WeakAny)`.** Sky's
  strong-linkage body wins at link. Stub's weak body still emits
  ~10-20 bytes per export but is dead at link. No filter needed.
  Crude but always-correct.
- **(6) Post-partition linkage mutation pattern (already proven for
  Phase 6 wrappers).** The erw codebase already mutates
  `MonoItemData.linkage` post-partition for Phase 6 generic wrappers.
  Generalize this pattern to mutate Sky-export stubs to
  `AvailableExternally` or `WeakODR` linkage instead of filtering them
  out. **The mechanism is already in production for a related case.**

**Mechanism family — per_instance_mir variants**

- **(7) per_instance_mir returns a TRAMPOLINE body.** Stub's
  `unreachable!()` body replaced (at codegen time) with `extern "C"`
  call to Sky's real implementation symbol. Rustc emits a thin
  trampoline that tail-calls into Sky's `.o`. Cost: doubles symbol
  count. Works for non-generics; breaks for generics (no `extern "C"`
  generics). Could be hybrid (trampoline for non-generics, current
  path for generics).
- **(8) per_instance_mir returns the REAL body — REJECTED PERMANENTLY.**
  Originally framed as the "long-term endgame" (Sky lowers source to
  rustc MIR; rustc's codegen pipeline emits the bodies; retires
  fill_extra_modules + patch 4). **This direction is rejected outright:
  it surrenders Sky's control of LLVM output to rustc's codegen
  pipeline.** Sky's architecture (§5, §F.15) requires Sky's own LLVM
  backend (Inkwell + patch 4's `fill_extra_modules`) to own every byte
  of Sky-emitted LLVM IR. That control is non-negotiable — it's how Sky
  guarantees its codegen quality, ABI discipline (`@ACRTFDZ`/`@TCHAPZ`),
  pin discipline (`@SMPLZ`), and emission-shape stability across rustc
  bumps. Any future investigator considering this direction: **do not
  pursue.** Open the question with the user before reconsidering; the
  rejection is a locked architectural property, not a deferral.

### Updated recommended sequence for Thread A

| Path | Retires | Status |
|---|---|---|
| **Option 4 (AvailableExternally)** | A.3 only | ✅ SHIPPED 2026-06-21. ~half day end-to-end. ~107 lines retired. F1 promise verified preserved via matrix. |
| **Option 8 (real-MIR per_instance_mir)** | hypothetically A.1+A.2+A.3+patch 4 | ❌ REJECTED PERMANENTLY. Surrenders Sky's LLVM control to rustc's codegen pipeline. Do not pursue. |

What the handoff's original Thread A claimed:
- A.1 + A.2 + A.3 all dissolve under path (c). ❌ Wrong — path (c)
  doesn't actually buy LTO inlining for non-generic Sky exports.

What Option 4 confirmed empirically (2026-06-21 shipping run):
- A.1 (capture-ship-replay) IS genuinely load-bearing — preserved
  unchanged through Option 4. **Permanent under Sky's locked LLVM-control
  architecture.**
- A.2 (synthesize_upstream_monomorphizations) IS genuinely load-bearing
  for Sky trait methods — preserved unchanged. **Permanent for the same
  reason.**
- A.3 (partition filter) was retired via the `codegen_fn_attrs` override.
  ~107 lines of partition.rs deleted; ~95 lines of codegen_fn_attrs.rs
  added. Net cleanup smaller than the handoff predicted because the
  type alias + accessor scaffolding for `default_collect_and_partition`
  was preserved (still used by `toylangc::llvm_gen` and the discovery
  pipeline for the unfiltered-slice walk — that consumer-side path can
  also be retired in a follow-up but wasn't required for Option 4).

So the actually-achievable architectural cleanup landed as documented:
Option 4 retires A.3; A.1 and A.2 stay permanently as the cost of
keeping Sky's LLVM control. Sky's architecture doc §F.14 gained §F.14.1
documenting the retirement; §25.2 gained B17 as the closing entry.

**Remaining Thread A hygiene** (opportunistic, not a thread of its own):
- Audit A.2 for synthesis entries rustc's default map would have
  produced anyway (post-`__lang_stubs` share_generics heuristic). Trim
  redundant entries if any.
- Consolidate the three Sky-owned-set predicates
  (`is_consumer_codegen_target`, `is_consumer_trait_impl_method`,
  `is_consumer_defined_item`) into one unified membership check.
- Update the `default_collect_and_partition` consumer call sites
  (`toylangc::llvm_gen` + `callbacks_impl`) to use
  `tcx.collect_and_partition_mono_items(())` directly now that no
  override is installed; then retire the type alias + OnceLock +
  accessor scaffolding.

Each is ~hours of work; pick up next time someone touches those files.

---

## Option 4 implementation guide (preserved as design history; work SHIPPED 2026-06-21)

> **Status:** Option 4 was implemented and shipped on 2026-06-21 per the
> playbook below. The integration suite passes (191 / 0 / 1) and the
> inlining matrix verifies the LTO promise. This section is preserved
> verbatim for posterity / future investigators reasoning about
> alternative paths or revisiting the choice. If you're picking up where
> this session left off, jump to "Thread B" or "What success looks like"
> below instead.

This subsection is the complete how-to for retiring A.3 via Option 4.
Treat it as a self-contained playbook. Estimated cost: half a day if
the in-process inliner concern (see "Open question" below) doesn't
bite; ~1 day if mitigation is needed.

### What's actually happening today (mechanical detail)

The collision Option 4 is going to defuse:

1. **`stub_gen` emits Rust source** for every Sky-marked crate. For
   `export fn add_one(x: i32) -> i32`, the stub source contains:
   ```rust
   pub fn add_one(x: i32) -> i32 { unreachable!() }
   ```
   See `toylangc/src/stub_gen.rs:333-342` for the wrapper-fn emission
   site. (Pre-F1 this also had `#[inline(never)]`; that was removed
   in commit `c4e8271`.)

2. **Rustc compiles `__lang_stubs`** as ordinary Rust. By default it
   would emit machine code for `add_one` — a `panic("unreachable")`
   blob in a `.rcgu.o`. This is the body that competes with Sky's.

3. **Sky's `fill_extra_modules` hook** (patch 4 of the fork) emits the
   REAL body for `add_one` into a separate LLVM module. Same mangled
   symbol name (per the Path B / single-symbol architecture, §F.2 in
   arch doc), External linkage.

4. **The partition filter (A.3)** intercepts the CGU list between
   rustc's default partitioner and rustc's LLVM backend. It REMOVES
   every Sky-defined item from the CGU list. Rustc's backend then
   has nothing to codegen for those items → no `.o` from rustc →
   no collision with Sky's `.o`.

The filter is at `rustc-lang-facade/src/queries/partition.rs:59-107`.
The whole file is 107 lines including comments. The retirement
target.

### Why Option 4 works

LLVM has a linkage kind called `AvailableExternally`. Semantics: "the
body of this function is present in the module **for inlining
purposes only**. Do NOT emit it as a `.o` symbol. Assume someone else
provides the symbol at link time."

Set `attrs.linkage = Some(Linkage::AvailableExternally)` on every
Sky-defined item's `CodegenFnAttrs`. Two effects:

1. Rustc's partitioner sees `mono_item.explicit_linkage(tcx)` returns
   `Some(AvailableExternally)` and short-circuits at
   `rustc_monomorphize::partitioning.rs:749-751` — the item still
   lands in a CGU, but the linkage decision is locked.

2. Rustc's LLVM backend codegens the body but the resulting LLVM
   function has `available_externally` linkage. LLVM's emit step
   does NOT produce a `.o` symbol for `available_externally`
   functions. The body exists in the IR for cross-module inlining
   (LTO) but produces no machine code.

Sky's `fill_extra_modules` still emits its real body with External
linkage as usual. At link time, Sky's body is the sole definition;
the linker resolves all references to it.

No collision. No partition filter needed.

### The rustc query you'll override

```
query codegen_fn_attrs(def_id: DefId) -> &'tcx CodegenFnAttrs {
    desc { |tcx| "computing codegen attributes of `{}`", tcx.def_path_str(def_id) }
    arena_cache
    cache_on_disk_if { def_id.is_local() }
    separate_provide_extern
    feedable
}
```

Source: `~/rust/compiler/rustc_middle/src/query/mod.rs` (search for
"query codegen_fn_attrs").

Important properties:
- **`separate_provide_extern`**: there are TWO providers — one for
  local items (`queries.codegen_fn_attrs`, takes `LocalDefId`) and one
  for upstream items read from rmeta (`extern_queries.codegen_fn_attrs`,
  takes `DefId`). Sky needs to override BOTH, just like the F2 fix
  in `cross_crate_inlinable.rs` does. See that file for the pattern.
- **`arena_cache`**: rustc wraps the provider to arena-allocate the
  returned `CodegenFnAttrs` and serve it back as `&'tcx CodegenFnAttrs`.
  Sky's provider returns by value; rustc handles the arena.
- **`feedable`**: there's also a `tcx.feed_codegen_fn_attrs(...)` API
  for direct injection. Not needed for Sky's use case — the query
  override path is simpler.

The default provider is at
`~/rust/compiler/rustc_codegen_ssa/src/codegen_attrs.rs:fn codegen_fn_attrs`.
It takes `LocalDefId` and returns `CodegenFnAttrs` by value.

### Step-by-step implementation

**Step 1 — Read these before touching code:**

- `rustc-lang-facade/src/queries/cross_crate_inlinable.rs` — the F2
  fix is the exact structural model for this override. Read the whole
  file (it's ~90 lines). The new `codegen_fn_attrs.rs` will mirror its
  shape almost identically.
- `rustc-lang-facade/src/queries/mod.rs` lines 56-89 — see how
  cross_crate_inlinable is wired into both `queries` and
  `extern_queries`. New override follows the same pattern.
- `rustc-lang-facade/src/lib.rs:1115-1145` — the
  `install_query_defaults` function that saves the default providers.
  Add two new parameters for codegen_fn_attrs (local + extern).
- `rustc-lang-facade/src/queries/partition.rs` (107 lines, doomed) —
  understand what you're deleting before you delete it.
- `rustc-lang-facade/src/lib.rs:738-750` — the `is_consumer_codegen_target`
  function. This is your filter predicate.

**Step 2 — Create the override file.**

New file: `rustc-lang-facade/src/queries/codegen_fn_attrs.rs`. Mirror
the cross_crate_inlinable.rs shape:

```rust
//! `codegen_fn_attrs` query override — retires the A.3 partition filter
//! by marking Sky-defined items with `AvailableExternally` linkage.
//!
//! ## The problem
//!
//! Sky's stub_gen emits `pub fn add_one(x: i32) -> i32 { unreachable!() }`
//! for every Sky export. Rustc's default codegen would compile that
//! body to machine code (a `panic("unreachable")` blob in `.rcgu.o`).
//! Sky's `fill_extra_modules` hook (patch 4) emits the REAL body for
//! the same symbol. Two `.o` files, same mangled symbol — link
//! collision.
//!
//! Historical fix: the A.3 partition filter (in `queries/partition.rs`)
//! removed Sky-defined items from rustc's CGU list before codegen, so
//! rustc emitted no machine code for them. ~107 lines of "Sky censors
//! rustc's pipeline."
//!
//! ## The fix (this file)
//!
//! Override `codegen_fn_attrs` to set
//! `linkage = Some(Linkage::AvailableExternally)` on every Sky-defined
//! item. LLVM emits the body for inlining purposes but produces no
//! `.o` symbol. Sky's `fill_extra_modules` body becomes the sole `.o`
//! definition; the linker resolves cleanly. No filter needed.
//!
//! The partitioner short-circuits at
//! `rustc_monomorphize::partitioning.rs:749-751` when
//! `mono_item.explicit_linkage(tcx)` returns Some, so this override
//! is sufficient — no need to also mutate `MonoItemData.linkage`
//! post-partition.
//!
//! ## Why this preserves pass-through
//!
//! Gated on `is_consumer_codegen_target` which only fires for items
//! in marker-bearing crates. Pure-Rust crates compiled via Sky's rustc
//! binary delegate to the default provider — byte-identical to vanilla.
//!
//! ## Why this preserves F1's LTO inlining promise
//!
//! `available_externally` linkage means the body IS in the IR, just
//! not in the `.o`. LTO's IR linker can still inline the body across
//! crate boundaries — that's the whole point of the linkage kind.
//! F1's matrix (29+ thin/fat LTO assertions) should still pass.

use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrs;
use rustc_middle::ty::TyCtxt;
use rustc_hir::attrs::Linkage;

pub type CodegenFnAttrsFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> CodegenFnAttrs;
pub type ExternCodegenFnAttrsFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> &'static CodegenFnAttrs;
// ^ verify the extern signature against rustc source — `&'static` may need
// to be `&'tcx` or similar; mirror what extern_queries.codegen_fn_attrs
// actually wants in the nightly Sky is pinned to.

pub fn lang_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CodegenFnAttrs {
    let mut attrs = crate::default_codegen_fn_attrs()(tcx, def_id);
    if crate::is_consumer_codegen_target(tcx, def_id.to_def_id()) {
        attrs.linkage = Some(Linkage::AvailableExternally);
    }
    attrs
}

pub fn lang_extern_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
) -> &'tcx CodegenFnAttrs {
    let default = crate::default_extern_codegen_fn_attrs()(tcx, def_id);
    if !crate::is_consumer_codegen_target(tcx, def_id) {
        return default;
    }
    // Clone, mutate, re-arena. Sky's items reached via rmeta also
    // need AvailableExternally so their downstream consumers don't
    // emit collision `.o`s either.
    let mut owned = (*default).clone();
    owned.linkage = Some(Linkage::AvailableExternally);
    tcx.arena.alloc(owned)
}
```

**Step 3 — Wire it into `queries/mod.rs`.**

Mirror cross_crate_inlinable. Add `pub mod codegen_fn_attrs;` near
the other module declarations. In `lang_override_queries`:

```rust
crate::install_query_defaults(
    providers.queries.layout_of,
    providers.queries.mir_shims,
    providers.queries.symbol_name,
    providers.queries.collect_and_partition_mono_items,  // ← keep this; the default partitioner still runs
    providers.queries.upstream_monomorphizations_for,
    providers.queries.upstream_monomorphizations,
    providers.queries.cross_crate_inlinable,
    providers.extern_queries.cross_crate_inlinable,
    providers.queries.codegen_fn_attrs,           // NEW
    providers.extern_queries.codegen_fn_attrs,    // NEW
);

// ... existing overrides ...

// REMOVE THIS LINE (A.3 retirement):
// providers.queries.collect_and_partition_mono_items = partition::lang_collect_and_partition_mono_items;

// ADD THESE (Option 4):
providers.queries.codegen_fn_attrs =
    codegen_fn_attrs::lang_codegen_fn_attrs;
providers.extern_queries.codegen_fn_attrs =
    codegen_fn_attrs::lang_extern_codegen_fn_attrs;
```

**Step 4 — Extend `install_query_defaults` in `lib.rs`.**

Add two more parameters and two more `OnceLock`s. Pattern is identical
to how cross_crate_inlinable's default got saved (~10 lines added in
that commit). Also add `default_codegen_fn_attrs()` and
`default_extern_codegen_fn_attrs()` accessors.

**Step 5 — Remove `queries/partition.rs`.**

Delete the file. Search for references:
```
grep -rn "queries::partition\|queries/partition\|lang_collect_and_partition_mono_items" \
  rustc-lang-facade/ toylangc/
```

Update each site. The `default_collect_and_partition()` accessor at
`rustc-lang-facade/src/lib.rs` is called from
`toylangc/src/llvm_gen.rs::generate_with_tcx` to get the unfiltered
CGU slice for Case 1b discovery. After Option 4, the CGU slice is
unfiltered by default (no override) — the call site can either keep
calling `default_collect_and_partition()` directly or just call
`tcx.collect_and_partition_mono_items(())`. Verify which is correct
by checking what `toylangc::llvm_gen` actually does with the slice.

**Step 6 — Remove the partition module from `queries/mod.rs`.**

Delete `pub mod partition;` and the `partition::` references. Also
update the module doc-comment that mentions partition as one of the
overrides (currently lines 1-27).

**Step 7 — Build and test.**

```
export LLVM_SYS_211_PREFIX=/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm
cargo +rustc-fork build --manifest-path ./toylangc/Cargo.toml > ./tmp/option4.txt 2>&1
echo "build exit=$?"
```

If build is clean, run the suite:
```
cargo +rustc-fork test --manifest-path ./toylangc/Cargo.toml > ./tmp/option4.txt 2>&1
echo "test exit=$?"
grep "test result" ./tmp/option4.txt
```

Expected: 191 passing, 0 failing, 1 ignored (same as the current
baseline). Especially watch the inlining matrix (40 fixtures, all
under `inlining/case*` names) — F1's promise lives there.

### Open question to validate

This is the one thing that could go wrong. The available_externally
body is present in the IR for inlining. Within the stub_rlib's own
compile, Sky's emitted `__lang_stubs::duplicate<Wrapper<i32>>` body
references `__lang_stubs::add_one`. Could LLVM (or rustc's MIR
inliner) inline the `unreachable!()` body into Sky's callers at
codegen time?

**LTO case (cross-module):** `#![no_builtins]` on stub rlibs (emitted
by stub_gen — see `toylangc/src/build.rs:329`) excludes their bitcode
from the LTO pool entirely. The available_externally body never
reaches the LTO IR linker. Sky's emitted body (in a separately-attached
LLVM module via patch 4) IS in the LTO pool. So LTO inlines Sky's
body, not the stub's. Safe.

**Non-LTO case (in-process inliner at stub_rlib compile):** rustc's
MIR inliner runs at -O1+. It might pull the stub's `unreachable!()`
body INTO Sky-defined callers within the same crate. If it does,
those callers crash at runtime when reached.

**How to verify quickly:** after Step 7's tests pass, manually check
case 4 / case 6 binaries at -O3:
```
/Users/verdagon/rust/build/aarch64-apple-darwin/ci-llvm/bin/llvm-objdump \
  -d --no-show-raw-insn \
  toylangc/target/integration-projects-cache/debug/case4_thin_lto \
  | awk '/<.*case4_thin_lto.*4main>:/,/^$/' | head -20
```

What you want to see: main constant-folds Sky's body (e.g. `mov w8,
#42`) — same as today. If instead you see an `unreachable` trap or a
`udf` instruction inside main's body, the MIR inliner pulled the
stub's body. Mitigation:

**If the MIR inliner bites:** re-introduce targeted `#[inline(never)]`
on the stub source's body. The F1 fence
(`toylangc/tests/architecture_fence.rs::stub_gen_no_inline_never_on_sky_items`)
will fire — that's correct; bump
`F1_EXPECTED_INLINE_NEVER_SITES` from `2` to whatever the new count
is, and update the comment to point at "MIR inliner mitigation for
Option 4." Verify the matrix's LTO assertions still pass (F1's
original concern was the LLVM-side inliner; `#[inline(never)]` doesn't
block LTO inlining of Sky's separately-emitted body because Sky's body
isn't the one the attribute is on).

The two concerns are independent:
- F1 was: "stub source has #[inline(never)] → LLVM doesn't inline
  Sky's body cross-crate at LTO." Cause: the noinline attribute
  propagated to call-site decisions. F1 removed the attribute to fix
  this.
- Option 4 risk: "stub source has available_externally body → rustc
  MIR inliner pulls stub's unreachable into Sky's callers in same
  crate." Cause: the body is visible in IR.

`#[inline(never)]` blocks the MIR inliner (rustc-internal, per-call-site).
F1's concern was an LLVM-internal, per-call-site decision driven by
function-attribute metadata. Re-introducing `#[inline(never)]` to fix
Option 4's MIR-inliner risk MAY also re-introduce F1's LLVM-inliner
risk — verify by running the matrix.

**If both can't be reconciled:** abort Option 4 and document why.
Path is still open via Option 6 (post-partition linkage mutation)
which doesn't trigger this exact concern because it doesn't emit a
body at all into the rustc-controlled CGU — it just changes the
data record's linkage field.

### Verification checklist

Before declaring Option 4 done:

- [ ] `queries/partition.rs` is deleted
- [ ] `queries/codegen_fn_attrs.rs` is created
- [ ] Both `queries.codegen_fn_attrs` and `extern_queries.codegen_fn_attrs`
      are overridden
- [ ] `install_query_defaults` saves the new defaults
- [ ] `is_consumer_codegen_target` is the gating predicate (same one
      that partition.rs used)
- [ ] All 191 tests still pass (run twice for warm+cold cache, since
      we just fixed that flake)
- [ ] Inlining matrix LTO assertions all pass
- [ ] Case 4 / case 6 disasm shows constant-folded values in main
      (not `udf`/`unreachable`)
- [ ] `test_release_mode_smoke` passes (the B14 fence — patch 5
      shouldn't be affected but worth verifying)
- [ ] Arch doc §F.14 marked "purely historical; A.3 retired via
      Option 4 commit XXX"
- [ ] Arch doc §25.2 gains B17 closing entry for partition filter
      retirement
- [ ] Handoff updated to mark Option 4 SHIPPED + retire this whole
      implementation guide section

### What stays (don't accidentally delete)

These are commonly confused with A.3 but are separate:

- **A.1 capture-ship-replay** (`toylangc/src/toylang/callbacks_impl.rs`
  around line 1305, the `capture_discovered_trait_impl_instances` flow
  + the sidecar populate drain). STAYS. The cascade still fires at
  stub_rlib compile; Sky still needs to ship trait-impl Instances.
- **A.2 synthesize_upstream_monomorphizations** (`rustc-lang-facade/src/queries/upstream_monomorphization.rs`).
  STAYS. The augmented map is what makes the v0 mangler pick
  `__lang_stubs` disambig for Sky trait methods.
- **`cross_crate_inlinable` override** (`queries/cross_crate_inlinable.rs`,
  shipped in F2 / B16). STAYS. This is an independent fix for a
  different mechanism (rustc's normal cross_crate_inlinable produces
  available_externally without Sky asking — F2 forces those to real
  symbols because Sky's CALLERS reference them. Option 4 produces
  available_externally for Sky's CALLEES because Sky doesn't want
  rustc to emit them. Different direction, different override; both
  needed.)
- **Patch 4 (`extra_modules` hook)** in the rustc fork. STAYS. Sky's
  body emission still goes through it; Option 4 just stops rustc from
  emitting competing bodies.
- **`#![no_builtins]`** on stub rlibs (in `toylangc/src/build.rs:329`).
  STAYS. It's load-bearing for the LTO-pool exclusion.

### Files you'll touch

| Path | Action |
|---|---|
| `rustc-lang-facade/src/queries/codegen_fn_attrs.rs` | CREATE (~90 lines, mirror cross_crate_inlinable.rs) |
| `rustc-lang-facade/src/queries/mod.rs` | Add module decl + 2 override wirings; REMOVE partition module decl + override wiring |
| `rustc-lang-facade/src/lib.rs` | Add 2 OnceLocks + extend install_query_defaults signature + 2 accessor fns; consider whether `default_collect_and_partition()` accessor stays (depends on whether toylangc still calls it) |
| `rustc-lang-facade/src/queries/partition.rs` | DELETE |
| `toylangc/src/llvm_gen.rs` (search for `default_collect_and_partition`) | Verify the CGU walk for Case 1b discovery still works — either no change (if `tcx.collect_and_partition_mono_items` is fine without the override) or update to use the accessor |
| `rust-interop-architecture.md` §F.13/§F.14/§5.3 | Update to reflect A.3 retirement |
| `rust-interop-architecture.md` §25.2 | Add B17 "A.3 partition filter retired via Option 4" |
| `handoff.md` | Mark Option 4 SHIPPED; retire this whole "Option 4 implementation guide" subsection |

### One last gotcha

The `cross_crate_inlinable` override (F2 / B16) interacts with
`codegen_fn_attrs` in subtle ways — both decide linkage. The interaction
to watch: `attrs.linkage = Some(AvailableExternally)` is set BEFORE
rustc's downstream code consults `cross_crate_inlinable`. If rustc
checks cross_crate_inlinable on items with explicit linkage already
set, the result might be ignored (because the linkage is locked).
That's probably fine — Sky's items would get AvailableExternally from
this override regardless of what cross_crate_inlinable says about
them. But if you see weird linkage discrepancies, this is the
interaction to grep for. Search rustc source for sites that read
both attrs (`grep -rn "cross_crate_inlinable\|explicit_linkage"
~/rust/compiler/rustc_monomorphize/src/`).

---

## Thread B: share_generics support boundary

### Current support matrix (re-stated)

| Stub rlib | User-bin | Status | How |
|---|---|---|---|
| on (forced) | on | works | Vanilla rustc handles cstore lookup |
| on (forced) | off | works | Patch 5 escape clause consults augmented map |
| off (explicit) | any | hard-error | `LangDriver::config` exits with diagnostic |
| (default at __lang_stubs) | any | forced to on | Heuristic in `LangDriver::config` |

We DON'T currently distinguish:
- User explicitly setting share_generics=true on `__lang_stubs` (works,
  same as default forced-on).
- User explicitly setting share_generics=false on user_bin (works at
  -O>=2 because patch 5 escape fires; works at debug because gate
  doesn't fire when share_generics is on... wait, in this scenario
  share_generics is off at user_bin, so gate FIRES at debug too, then
  the escape clause kicks in — same path as -O3).
- User setting share_generics=true via `RUSTFLAGS` propagated to all
  crates including pure-Rust deps. Affects pass-through byte-identity
  for those crates (vanilla rustc defaults false at -O>=2).

### Open questions for Thread B

1. **Is the hard-error at `__lang_stubs` + share_generics=false too
   aggressive?** The current behavior: `eprintln!` + `exit(1)`. The
   user might have legitimate reasons (e.g., wanting to compare
   behaviors, debugging). Alternatives:
   - Warning + override (current rejection stays but we force it on
     anyway with a warning).
   - Error with an opt-in override flag (`-Z
     allow-sky-stubs-no-share-generics=yes` or similar).
   - Current behavior (hard error).
   What's the right discipline?

2. **Should we also force share_generics=on at `case6_lib` and other
   Sky lib crates?** The heuristic currently matches only crate name
   `__lang_stubs` (the bin's own stub rlib). For multi-Sky-crate
   projects, each Sky lib has its OWN stub rlib (e.g., `case6_lib`'s
   stub rlib is named `case6_lib`). The release-mode fix works for
   case6 fixtures empirically, so something's right — but is it
   because case6_lib doesn't emit Rust generic intermediaries (the
   user_bin's cascade reaches Wrapper through duplicate at user_bin
   side, not at case6_lib's stub rlib side)? Or are we missing
   something?

   **Verify:** trace through `test_case6_app_o3` to see which
   share_generics setting case6_lib's compile gets. Does the lack
   of forcing break anything?

3. **Should pure-Rust crates compiled via Sky's rustc respect user's
   explicit share_generics?** Today: yes. Pure-Rust crate has no
   marker, so `consumer_lang_active=false`, the escape clause doesn't
   fire, and rustc behavior is vanilla. The byte-identity invariant
   is preserved. This seems correct. Just verify with a test.

4. **Should we error/warn if the user passes
   `RUSTFLAGS="-Z share-generics=yes"` on a project that's mixed
   Sky + pure Rust?** Forcing share_generics on pure-Rust crates
   weakens pass-through. Could detect at config time and emit a
   warning. Not blocking; nice-to-have.

5. **What about `RUSTFLAGS=-Z share-generics=no` on a user_bin?**
   The bin doesn't have the marker (the upstream stub rlib does).
   With this set: gate doesn't fire because share_generics is
   honored (off). Patch 5 escape DOES fire (consumer_lang_active is
   true because the loaded `__lang_stubs` has the marker). Should
   work. Verify with a fixture.

### Investigation tasks for Thread B

1. Build the support matrix as actual fixtures, one per cell. Cover:
   - { stub rlib share_generics = forced default, explicit on,
     explicit off (error case) }
   - × { user_bin share_generics = default, explicit on, explicit
     off }
   - × { pure-Rust dep share_generics = default, explicit on,
     explicit off }
   
   Realistic minimum: 9 fixtures covering the meaningful subset.

2. Decide and document each cell's behavior. Update
   `LangDriver::config`'s diagnostic to reflect supported set.

3. Update arch doc §3.2 patch 5 with the support matrix.

---

## Thread C: Inlining test matrix (the big one)

### 2026-06-20 update: partially shipped

What landed (uncommitted on the working tree):

| Component | Path | Notes |
|---|---|---|
| Harness | `toylangc/tests/common/inlining_harness.rs` | `DisasmContext`, `disassemble_binary`, `assert_no_call_to_symbols_matching`, `assert_call_to_symbol_matching`. Demangles symbols via `rustc-demangle`. Extracts mnemonic via `line.split_once(':')` then first whitespace token — robust to address-prefix in objdump format. |
| Dev-dep | `toylangc/Cargo.toml` | Added `rustc-demangle = "0.1"` to `[dev-dependencies]`. |
| Fixtures | `toylangc/tests/integration_projects/inlining/` | 40 subdirectories. Sky source + optional `rust_caller.rs` + `toylang.toml` (with `opt-level`/`lto`/`codegen-units`/`features` knobs) + `expected_output.txt`. Plus shared `case6_lib_inl/` Sky lib used by case6 fixtures. |
| Test entries | `toylangc/tests/integration_projects.rs` (end-of-file) | 40 `#[test] fn test_inline_*`. Helpers: `run_inlining_project`, `assert_no_sky_branch_in_main`, `assert_no_some_rust_lib_branch_in_main`, `assert_sky_branch_present_in_main`, `assert_no_wrapper_branch`, `assert_wrapper_branch_present`. |
| Search markers | (in test source) | `SKY_EXPORT_LTO_INLINING_FINDING` (F1), "sibling of B14" (F2), `PRIORITY_B_DISAMBIG_IGNORE`. |

Test tally (Priority A + B + C + D, 40 total + the ported `test_lto_smoke`):

| Phase | Pass | Ignored | Why ignored |
|---|---|---|---|
| Priority A — 7 cases × 3 LTO modes at -O3 (21) | 7 | 14 | Sky-export LTO inlining gap (F1) and case3/case5 disambig bug (F2) |
| Priority B — 4 Rust-top cases × 3 `#[inline]` variants on Rust wrapper at -O3+thin LTO (12) | 6 | 6 | case3/case5 inherit F2 |
| Priority C — case 4 opt-level sweep (5) | 5 | 0 | — |
| Priority D — case 4 codegen-units {1, 16} at -O3+thin LTO (2) | 2 | 0 | — |
| `test_lto_smoke` (ported) | 1 | 0 | Reduced to smoke-only; assertion was vacuous (see F1) |
| **Total** | **21** | **20** | |

Two findings surfaced (carried to top-of-stack in the TL;DR):

#### F1 — Sky-export LTO inlining gap

At thin/fat LTO + -O3, cases 1a / 2 / 4 / 6 — every case where the user-bin
boundary is a Sky-EXPORTED non-generic fn — do NOT inline cross-crate.
Disassembly evidence: the bin's `main` is a single tail-jump
`b __lang_stubs::__toylang_main` (or `bl __lang_stubs::add_one`); Sky's body
exists and constant-folds internally (e.g. `lto_smoke` produces `mov w8, #50`
inside Sky's `__toylang_main`), but the call doesn't get inlined into the
shim.

Root cause: `toylangc/src/stub_gen.rs:221` emits `#[inline(never)]`
unconditionally on every Sky export. LLVM honors this during the ThinLTO/FatLTO
IR linker pass. The `#[inline(never)]` was added for two reasons (per the
in-source comment): (1) defeats `requires_caller_location_or_inline_never`
at -O2/-O3 — the share_generics gate the release-mode fix targets; (2)
prevents rustc's MIR inliner from leaking the `unreachable!()` stub body
into callers. Both reasons are load-bearing, so relaxing the discipline
isn't a free choice.

Importantly: Sky GENERICS with a Rust-side type arg (case 1b) DO inline at
every opt level — because Sky's `per_instance_mir` emits the substituted
body INTO the user_bin's CGU, where LLVM sees it locally. The gap is
specific to Sky exports.

The original `test_lto_smoke` assertion was **vacuously passing**: its
`trimmed.contains("bl\t")` check (now ported to the harness) didn't match
the `b\t` tail-jump emitted at LTO. Multiple sessions thought the inlining
promise was empirically verified; it wasn't.

Resolution candidates (in roughly increasing ambition):
- **(a)** Document the tail-jump cost as the cost of safety. One tail-jump
  per Sky-export call is real but small; the discipline that produces it
  closes a class of MIR-inliner / share_generics bugs. Update the perf
  promise from "interop is free" to "interop costs one tail-jump per
  Sky-export entry."
- **(b)** Add a per-export opt-in attribute on Sky source (e.g.
  `export #[inline] fn add_one(...)`). stub_gen reads it and OMITS
  `#[inline(never)]` for that export. Requires Sky parser extension + a
  decision about whether the share_generics gate / MIR inliner leak still
  apply per-export. Smallest user-facing change with real fix value.
- **(c)** Replace `#[inline(never)]` with `#[cold]` or a custom attribute
  that defeats only the MIR inliner without LTO's IR inliner. Requires
  understanding rustc's inliner-stage hierarchy better than this handoff
  has investigated.

Investigation entry points: `toylangc/src/stub_gen.rs` (the emission of
`#[inline(never)]` and its rationale comments); `toylangc/tests/integration_projects.rs`
(search `SKY_EXPORT_LTO_INLINING_FINDING` for the 6 fixtures that lock
this in once it's resolved); `rust-interop-architecture.md` §F.13 / §8.9.5
(the share_generics gate + MIR inliner story `#[inline(never)]` defeats).

#### F2 — case 3 / case 5 disambig bug at -O3

`Rust → Sky generic → Rust impl` (case 3, derived `Clone` on a user_bin-local
struct) and `Rust → Sky → Rust generic intermediary` (case 5, `Vec`) both
fail to LINK at -O3, every LTO mode. Linker errors:

```
case 3 fat LTO:
  Undefined symbols: __RNvXCs..._13case3_fat_ltoNtB2_9MyCounter
                       NtNtCs..._4core5clone5Clone5clone
case 5 no LTO:
  Undefined symbols: __RNvMNtCs..._5alloc3vecINtB2_3VecJE3new
                       ...12case5_no_lto
                     __RNvMsF_NtCs..._5alloc3vecINtB5_3VecJE4push
                       ...12case5_no_lto
```

Root cause hypothesis (verify): Sky's `per_instance_mir` emits
`__lang_stubs::clone_it<MyCounter>` (for case 3) into the user_bin's CGU.
That body references `<MyCounter as Clone>::clone`. At -O3 with the user_bin
at default `share_generics=false`, the user_bin's mono collector doesn't
emit `<MyCounter as Clone>::clone` with the disambig the Sky-emitted call
site expects — same gate that B14 targeted, but in the OPPOSITE direction
(B14 was Sky impl reached through Rust generic; F2 is Rust impl reached
through Sky generic).

Same shape for case 5: `Vec::<i64>::new` / `::push` from `alloc::vec` are
referenced from Sky's `store_in_vec<i64>` body emitted at user_bin, but
not emitted with the disambig the call expects.

Resolution candidates:
- **(a)** Extend `synthesize_upstream_monomorphizations`
  (`rustc-lang-facade/src/queries/upstream_monomorphization.rs`) to add
  entries for `<consumer_T as RustTrait>::method` instances that Sky's
  generic bodies reference. Sky knows the (def_id, args) tuple when its
  `per_instance_mir` returns the body — capture and synthesize. Mirror of
  the existing trait-impl synthesis but for Rust impls reached through
  Sky bodies.
- **(b)** Add a sixth fork patch that makes user_bin's mono collector
  re-visit body items referenced by Sky-emitted bodies (so the
  user_bin's normal mono emits the impl method with its own disambig).
  Probably larger surface than (a).
- **(c)** Force `share_generics=true` at user_bin too (via the
  `LangDriver::config` heuristic) when Sky's machinery is active. Symmetry
  with the `__lang_stubs` heuristic. Breaks pass-through invariant more
  broadly than the existing heuristic.

Investigation entry points: `toylangc/tests/integration_projects.rs`
(search "sibling of B14" — 9 fixtures locked in this finding);
`rust-interop-architecture.md` §25.2 B14 (the existing release-mode fix
for the opposite direction);
`rustc-lang-facade/src/queries/upstream_monomorphization.rs` (the synthesis
site for path (a)).

### What's still on the table for Thread C

If/when F1 and F2 are resolved, the natural follow-ups for Thread C:
- **Sky-side `#[inline]` syntax support.** Currently `stub_gen` emits
  `#[inline(never)]` on every export with no override. Adding a Sky
  parser-level attribute (e.g. `export #[inline] fn ...`) would unlock
  Priority B coverage for Sky-top cases (2, 4, 6) — 9 additional fixtures.
- **Cross-Sky-CGU inlining.** Deferred until Sky partitioning exists.
- **Cross-rustc-version drift CI fence.** When a nightly bump changes
  LLVM's inliner thresholds, the matrix may regress. Worth a CI job that
  reports differences rather than failing hard.

### Original Thread C plan (pre-2026-06-20) — preserved for context

### Current state (pre-shipping)

| Verified | Mechanism |
|---|---|
| Sky body inlined into Rust caller at -O3 + thin LTO (Case 2) | `test_lto_smoke` disassembles `lto_smoke::main` and asserts no `bl` to Sky symbols + constant-folded result `mov w8, #50` |
| Sky-emitted stubs never MIR-inlined | `architecture_fence.rs` asserts `#[inline(never)]` on stubs in stub_gen output |

That's it. Everything else is hopeful inference.

> **Update (2026-06-20):** the first row was vacuously passing — see F1
> above. The original `test_lto_smoke` `bl\t` substring check didn't match
> the `b\t` tail-jump LLVM actually emitted. We've been operating on a
> false belief.

### What the user wants

Quote from session: "we need much more tests for all combinations of
things that affect inlining. i think we need to go way overboard and
have an excessive amount of tests for this."

### The matrix

Axes that affect inlining:
- **7-case taxonomy** (case 1a, 1b, 2, 3, 4, 5, 6 from §2). 7 values.
- **Direction** (Sky→Rust inlining vs Rust→Sky inlining). 2 values.
- **opt-level** (-O0 baseline, -O1, -O2, -O3, -Os, -Oz). 6 values.
- **LTO mode** (no LTO, thin, fat). 3 values.
- **`#[inline]` annotation on the boundary callee** (none, #[inline],
  #[inline(always)], #[inline(never)]). 4 values.
- **codegen-units** (default ≥16, =1). 2 values.

Full cross-product: 7 × 2 × 6 × 3 × 4 × 2 = 2016 cells. Not all
meaningful. User said "way overboard" so aim for ~50-100 fixtures
covering the meaningful subset.

### Recommended subset (~40-50 fixtures)

Priority A (catches the most likely real bugs):
- 7 cases × 3 LTO modes (no, thin, fat) at -O3 = 21 fixtures.
- Each one asserts via disassembly that the expected inlining happens.

Priority B (annotation interaction):
- For each of 7 cases at -O3 + thin LTO, add 3 variants: callee with
  #[inline], #[inline(always)], #[inline(never)]. = 21 more fixtures.
- Assert the inlining behavior matches the annotation's intent.

Priority C (opt-level sensitivity):
- For Case 4 only (the canonical hard case), add -O0, -O1, -O2, -Os,
  -Oz variants. = 5 more fixtures. -O0 should NOT inline; -O1+ should.

Priority D (codegen-units):
- For Case 4 at -O3, add codegen-units=1 and codegen-units=16 (default
  is something between; explicit values verify behavior). = 2 fixtures.

Subtotal: 49 fixtures. Add 1-2 for sanity (does Sky's main get inlined
into the bin shim's main? cross-Sky-CGU when Sky has multiple CGUs —
deferred until partitioning exists).

### The harness

Current `test_lto_smoke` has the disassembly check inline. Lift it
into a reusable helper:

```rust
fn assert_no_call_to_symbols_in_binary(
    binary_path: &Path,
    function_name: &str,
    forbidden_symbol_pattern: &str,
) {
    // Run llvm-objdump -d <binary> to get disassembly.
    // Find the function by name.
    // Walk its instructions looking for `bl` / `call` instructions.
    // For each, resolve the target symbol name via the binary's
    // relocation table or symbol table.
    // Assert no target matches `forbidden_symbol_pattern`.
}

fn assert_call_to_symbol_in_binary(
    binary_path: &Path,
    function_name: &str,
    required_symbol_pattern: &str,
) {
    // Inverse: assert that at least one `bl`/`call` target matches.
}

fn assert_constant_in_function(
    binary_path: &Path,
    function_name: &str,
    expected_constant: u64,
) {
    // Assert the function loads the given immediate (e.g.
    // `mov wN, #50`). Used to verify constant-folding from inlining.
}
```

Each fixture's test then becomes:

```rust
#[test]
fn test_case_X_inlining_at_Y() {
    run_integration_project_no_run_check("case_X_inlining_at_Y");
    let bin = shared_cargo_target_dir()
        .join("debug")
        .join("case_X_inlining_at_Y");
    assert_no_call_to_symbols_in_binary(
        &bin,
        "case_X_inlining_at_Y::main",
        ".*sky_lib_function_name.*",
    );
}
```

### Why this is high-value

Inlining is the user-visible perf promise of Sky's interop. If Sky's
first benchmarks show "Sky→Rust calls are 5× slower than expected,"
that's because some inlining direction silently failed. The matrix
catches this BEFORE Sky's first benchmark.

The fat-LTO bug we found via the recent matrix expansion is the
template for what to expect: most cells will pass, 1-3 will reveal
real surprises. Each one found pre-Sky-production is hours-of-
investigation saved later.

### Investigation tasks for Thread C

1. **Build the harness** (~1-2 hours). The disassembly-walking
   functions above. Test with the existing `test_lto_smoke` to verify
   it produces the same result.

2. **Build the 21 Priority A fixtures** (~3-4 hours). Each is a
   small Sky + Rust source pair + a test entry. Use existing case
   fixtures as templates.

3. **Run them all, report which fail.** This is the actual discovery
   step. Some failures will be real bugs (like the fat-LTO one);
   others will be expected-behavior-not-yet-matching-assertions
   (need to update the assertions).

4. **Iterate.** Fix the real bugs; add Priority B/C/D fixtures.
   Final count probably ~50-60 fixtures + harness + ~3-5 newly-
   surfaced bugs.

5. **Long-term: add CI fence**. The matrix should be part of every
   release-mode test run.

---

## All-the-other-tests recap (from prior recommendations)

These are things I recommended over the course of the session that
also didn't land. Roll them into the next session if you want
complete coverage:

- `debug_assertions_off_smoke` — `-C debug-assertions=off` at -O3.
  Catches MIR-shape changes around `unreachable!` and overflow checks.
- `opt_level_z_smoke` — `-Oz` (size optimization). Uses a totally
  different optimization preset.
- `lto_off_smoke` — explicit `lto = "off"` (vs current implicit
  default). Validates the toml-parsing path.
- Pass-through invariant assertion — verify pure-Rust crates compiled
  via Sky's rustc don't get `@llvm.used` global injected (the SMPLZ
  pin should be marker-gated).
- panic=abort enforcement — assert that `LangDriver::config` errors
  if `panic = "unwind"` is set. Today we silently inherit whatever
  the user has; arch §16.1 says we require abort.

Plus the 4 from the agent audit that aren't biting today but should
get fixed for production-readiness:
- Cargo fingerprint sidecar blindness (~1 day)
- Rustc incremental cache Sky-side blindness (~2-3 days conservative)
- `deduce_param_attrs` from stub MIR (~half day)
- Sky-side DWARF emission (~3-6 weeks, separate effort)

---

## Recommended sequence

If you can do it all:

1. **Thread C first** (~10-15 hours): build harness + 30-50 fixtures.
   Highest risk-reduction value. Will find bugs you don't yet know
   about. Sets up infrastructure that the other threads can leverage.

2. **Thread A.1+A.2 verification** (~1 day): empirically confirm
   they're still load-bearing. Cheap, mostly just adds confidence to
   §F.13 / §8.9.5 doc claims.

3. **Thread B** (~1 day): build the support matrix as fixtures, decide
   each cell's behavior, document.

4. **Thread A.3 investigation** (~1 week): the partition-filter
   elimination. If path (c) works, this unlocks A.1 and A.2 pruning;
   significant arch simplification.

5. **Other tests recap** (~3-5 days): the various small fixtures and
   the audit-surfaced bugs. Spread across whichever session has time.

If you can only do one: do Thread C. It's the only one that catches
bugs we don't yet know exist.

---

## Files you'll touch

### For Thread A

| File | Purpose |
|---|---|
| `rustc-lang-facade/src/queries/upstream_monomorphization.rs` | A.2's whole-map override |
| `rustc-lang-facade/src/queries/partition.rs` | A.3's CGU filter |
| `toylangc/src/toylang/callbacks_impl.rs` (around line 1305) | A.1's capture + the populate drain |
| `toylangc/src/stub_gen.rs` | A.3 path (c): cfg-guarded stub source rewrite |
| `rustc-lang-facade/src/driver.rs` | Set `--cfg sky_provides_body` when machinery active |

### For Thread B

| File | Purpose |
|---|---|
| `rustc-lang-facade/src/driver.rs::LangDriver::config` | The heuristic + diagnostic |
| `toylangc/tests/integration_projects/` | New fixtures per the support matrix |
| `rust-interop-architecture.md` §3.2 patch 5 | Document the support matrix |

### For Thread C

| File | Purpose |
|---|---|
| `toylangc/tests/inlining_harness.rs` (new) | Reusable disassembly-assertion functions |
| `toylangc/tests/inlining_matrix.rs` (new) | The test functions for each cell |
| `toylangc/tests/integration_projects/inline_*` (new, ~40-50) | Fixtures |
| `toylangc/tests/integration_projects.rs` | Add `mod inlining_matrix;` or `#[test]` entries |

---

## Gotchas

### Inlining behavior across rustc versions

LLVM's inliner thresholds and pass scheduling change between rustc
versions. Tests that assert specific inlining outcomes can break on
nightly bumps even when nothing semantic changed. Mitigation: assert
"inlined OR optimized differently in a way that still produces no `bl`
to the boundary symbol" rather than "constant-folded to exact value X."
The `test_lto_smoke` assertion is too tight in this regard; we got
lucky that it hasn't broken yet.

### Symbol-name pattern matching is fragile

Sky's symbols use v0 mangling with disambig codes that depend on the
crate's compilation. Patterns like `.*sky_lib.*` should match against
the symbol's *demangled* form (via `rustc-demangle` or LLVM's symbolizer)
rather than the raw mangled name. The harness should do the demangling.

### Some inlining only happens at link time

Cross-CGU and cross-crate inlining requires LTO. Tests asserting these
must explicitly set `lto = "thin"` or `lto = "fat"`. Without LTO, the
linker can't inline across translation units even at -O3.

### `#[inline(always)]` is a hint, not a guarantee

Even `#[inline(always)]` doesn't force inlining in all cases — LLVM
refuses if the inliner's heuristics decide it'd produce bad code
(recursive calls, very large body, sanitizer interaction). Tests
asserting "inline(always) callee was inlined" should be aware of
this and use forgiving patterns.

### Sky's `#[inline(never)]` discipline on stubs

`stub_gen` emits `#[inline(never)]` on every Sky export's stub. Tests
asserting "this is NOT inlined" on Sky stubs should expect the
discipline to work; tests asserting "this IS inlined" on Sky stubs
will fail by design.

### Save-temps for debugging fixture failures

When an inlining test fails unexpectedly, the recipe from §25.2 B15's
playbook applies: `RUSTFLAGS="-C save-temps"` preserves intermediate
bitcode. `llvm-dis` and `llvm-objdump -d` let you see exactly what
LLVM did or didn't inline.

---

## Long-term context (Sky proper)

Sky's actual implementation (Phase 1-9 in arch §28) will rebuild much
of toylang's infrastructure with Sky's own types. The investigations
here matter because:

- **Thread A's findings shape Sky's frontend.** If A.3 can be
  eliminated, Sky's frontend doesn't need to maintain the partition
  filter / capture-ship-replay / synthesize machinery. ~500 lines of
  facade code retires. Less to port.

- **Thread B's findings shape Sky's user-facing build flags.** Sky's
  `skyc` orchestrator will need to respect (or override) various
  share-generics user intents.

- **Thread C's harness is reusable.** Once built, the same harness
  validates Sky's actual inlining behavior at every nightly bump,
  every rustc-fork rebuild, and every Sky compiler change. Worth
  the infra investment.

---

## What success looks like

When you finish all three threads:

- **Thread A:** the architecture doc has clear conclusions on
  whether/how to retire A.1, A.2, A.3. Either: "Investigation
  confirmed they're necessary, documented why" (no code change), or:
  "Path (c) works; here's the migration plan." Either outcome is
  forward progress because the question is answered.

- **Thread B:** the support matrix is documented + fenced by
  fixtures. Each cell either works as expected or is documented as
  unsupported with a clear diagnostic.

- **Thread C:** ~50-60 inlining fixtures all passing OR explicitly
  failing-with-known-cause + tracked as risks. The harness is
  reusable for future expansion. Sky's first production benchmark
  has high confidence that the inlining promise holds.

Test count likely goes from 277 to ~340-360. Architecture doc gains
~3-5 new subsections (§F.x additions for each thread's findings,
plus possibly new arcana entries if surprising patterns emerge).

---

## Long-tail items inherited from `course-correct.md`

`course-correct.md` (kept at repo root for the 20+ code-comment
references like `course-correct.md item #N`) tracked 18 numbered
"wrong-track patterns" that erw needed to flip to align with Sky's
architecture. **16 of 18 are DONE.** Folding the two remaining items
here so they're visible alongside the active threads:

### Item #10 (PARTIAL) — `collect_generic_rust_deps` Instance-keyed body

**Current state.** `collect_generic_rust_deps(LocalDefId) → Vec<(DefId, args-with-Params)>` was migrated to Instance-keyed input + Sky-side substitution per Approach A (commit-time documented at course-correct.md). The primary single-crate path landed. The cross-crate **effective-registry merging** (so a user_bin's compile can resolve Sky deps reaching across multiple Sky libs without each lib re-discovering independently) was deferred — labeled "E.5-style threading" in the course-correct snapshot.

**Why it matters.** Today every Sky-active compile re-derives its dep registry from sidecars + local Temputs. For larger Sky projects with several Sky libs in the build graph, this is O(n²)-ish in the worst case. Effective-registry merging at the facade level would cache a merged universe across libs in a single compile.

**Cost.** Not yet investigated rigorously. Probably ~1 day to design + implement, dependent on whether Sky proper or toylang is the driver.

**Status under post-F1/F2 architecture.** Lower priority than it was — Sky's compile times haven't been a user pain point and the matrix work and F2 fix didn't surface this as a bottleneck. Keep on the list but not top-of-stack.

### Item #13 (REMAINING) — wrapper-mode retirement

**Current state.** toylangc still uses `RUSTC_WORKSPACE_WRAPPER` wrapper mode (`@MRRIWMZ` arcanum). At cargo invocation, toylangc's binary intercepts as the rustc wrapper, parses argv, re-reads `toylang.toml`, and dispatches to either a real rustc compile or its own driver. The arch doc explicitly calls this arcanum out as "one of two erw arcana with no Sky analog" because Sky's `rustc` is the forked rustc statically linked with the backend (§4.1) and is invoked directly by cargo via `rust-toolchain.toml`.

**Why it persists.** The wrapper-mode dispatch is convenient for toylang because it lets the same binary play both "skyc orchestrator" and "rustc with consumer machinery" roles. Sky proper's distribution model splits these into two separate binaries (`skyc` + `rustc` — see arch §4.2). Until Sky proper's toolchain distribution is real, toylang has no place for the split.

**Cost.** Significant restructuring of `toylangc/src/main.rs` (lines 39-55, 82-155 per the course-correct entry) — split the wrapper-mode dispatch and the orchestrator's argv handling. Probably ~2-3 days. Best done bundled with Sky's actual toolchain shipping work, since the split has to match Sky's distribution shape.

**Status.** Deferred until Sky proper's toolchain phase begins. Operationally this is fine because the wrapper mode works; it's architecturally wrong-shape but functionally correct.

### Other "what's NOT on the wrong track" items

For completeness, the course-correct doc also documented patterns that were *direction-correct* but needed content changes — all subsequently landed:
- `queries/layout.rs` opaque-with-size shape
- `queries/drop_glue.rs` InstanceKind::DropGlue → synthetic body
- `queries/upstream_monomorphizations_for.rs` force-local-mono
- `abi_helpers.rs`, `mir_helpers.rs` inherited wholesale per §26.5–26.6

If a future session is auditing the facade for cleanup opportunities, these four files were declared "right shape, content may evolve" — historically the boundary of what was actively re-architected.

---

— Previous engineer, via Claude Opus 4.7 (1M context),
  Claude-Session: https://claude.ai/code/session_014jTbwcznQUd4i89tbLMdAa
