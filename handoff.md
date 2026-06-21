# Handoff: Discovery/synthesis machinery + share_generics + inlining coverage

**Who this is for:** the next engineer (probably you, fresh-session) picking up four
interrelated investigation threads. Assumes you've read
[`rust-interop-architecture.md`](rust-interop-architecture.md) at least once,
particularly §F.13 (cascade-fires-at-stub-rlib), §8.9.5 (discovered
trait-impl instances), §25.2 (the risk register, especially B14/B15), and
§26.16 (SMPLZ arcanum). If not, read those first.

**Status:** open threads identified during session 2026-06-19/20. None of these
threads is bug-fixing — they're refinement/investigation/expansion work.
Release-mode (the previous handoff's bug) is fully resolved as of `08f350e`.

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
For full A.1+A.2+A.3 retirement, Option 8 (per_instance_mir returns
real MIR via a toylang→MIR emitter) is the long-term endgame — substantial
new component, plan for post-Sky-proper-MVP. See "Thread A — deep
investigation findings" below for the full menu of 8 options.

---

## TL;DR

Three open threads remaining (F1 + F2 + Thread C closed):

1. **Inlining test matrix (Thread C)** — ✅ **FULLY SHIPPED 2026-06-20.**
   Harness (`toylangc/tests/common/inlining_harness.rs`) + 41 fixtures across
   `toylangc/tests/integration_projects/inlining/` + matrix tests in
   `integration_projects.rs`. 40 passing / 1 ignored (LLVM `#[inline(never)]`
   aggression flake) / 0 failing. Surfaced F1 + F2, both now resolved.
   Optional follow-up: extend matrix to cover Sky-side `#[inline]` syntax
   (requires Sky frontend work) and Sky-top Priority B variants (blocked
   on same).
2. **Finding F1 — Sky-export LTO inlining gap. ✅ CLOSED 2026-06-20.**
3. **Finding F2 — case 3 / case 5 disambig bug. ✅ CLOSED 2026-06-20** via
   `cross_crate_inlinable` query override.
4. **Discovery/synthesis/filter machinery (Thread A)** — three Sky-side
   layers (capture-ship-replay, synthesize-upstream-monomorphizations,
   partition filter) coordinate to handle the F.13 cascade-timing
   problem. Investigate whether any can be pruned/eliminated, especially
   in light of the new release-mode fix's machinery. Cost: ~1-2 weeks
   for full investigation; partial pruning is hours.
5. **share_generics handling (Thread B)** — current support is forced-on
   at `__lang_stubs`, hard-error if user disables it there, otherwise
   user choice. Decide if we should support/honor it differently in
   other configurations. Cost: ~1 day investigation + decision.
6. **The "kill partition filter" sub-thread (Thread A.5)** — separable
   from the rest. If the partition filter could be eliminated, both A.1
   (capture-ship-replay) and A.2 (synthesize-upstream-monomorphizations)
   dissolve naturally. Requires either an upstream RFC
   (`#[codegen_backend_provides_body]`) or a Sky-side replacement
   mechanism. Cost: investigation ~3-5 days; implementation likely
   multi-week.

Recommended order (updated 2026-06-20): **A → B**. Threads A and B are
the remaining architectural work; F1 and F2 became top-of-stack because the matrix surfaced them as real architectural
gaps with user-visible consequences. A is hygiene; B is design-space
exploration. C is largely shipped; pick it back up only if you want
Sky-side `#[inline]` support OR Sky-top Priority B coverage.

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
- **(8) per_instance_mir returns the REAL body (path (e) of original
  handoff).** Cleanest long-term: Sky lowers Sky source directly to
  rustc MIR. The infrastructure exists — `mir_helpers.rs` already
  builds real `Call`-based MIR for drop glue. Cost: substantial new
  component (full toylang→MIR emitter); retires patch 4 of the fork
  too. For Sky proper this is the architectural endgame.

### Updated recommended sequence for Thread A

| Path | Retires | Cost |
|---|---|---|
| **Option 4 (AvailableExternally) — DO NOW** | A.3 only | ~half day prototype + verify; ~108 lines retire; F1 promise preserved |
| **Option 8 (real-MIR per_instance_mir) — LATER** | A.1, A.2, A.3, AND patch 4 | Significant — full MIR emitter; long-term post-Sky-proper-MVP |

Option 4 is the actionable win. Implementation sketch:

```rust
// New file: rustc-lang-facade/src/queries/codegen_fn_attrs.rs
pub fn lang_codegen_fn_attrs<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CodegenFnAttrs {
    let mut attrs = default_codegen_fn_attrs()(tcx, def_id);
    if is_consumer_codegen_target(tcx, def_id.to_def_id()) {
        attrs.linkage = Some(Linkage::AvailableExternally);
    }
    attrs
}
```

Then delete `partition.rs`'s filter logic. The matrix should keep passing
(F1's promise preserved); the `cross_crate_inlinable.rs` override (B16)
stays — it's an independent fix.

What the handoff's original Thread A claimed:
- A.1 + A.2 + A.3 all dissolve under path (c). ❌ Wrong — path (c)
  doesn't actually buy LTO inlining for non-generic Sky exports.

What we actually know now:
- A.1 (capture-ship-replay) is genuinely load-bearing as long as the
  cascade fires at stub_rlib compile. Even Option 4 doesn't retire A.1.
- A.2 (synthesize_upstream_monomorphizations) is genuinely load-bearing
  for Sky trait methods. Even Option 4 doesn't retire A.2.
- A.3 (partition filter) CAN be retired via Option 4 today, at the cost
  of one new query override (~80 lines).

So the actually-achievable architectural cleanup is more modest than
the original handoff suggested: Option 4 retires A.3 but A.1 and A.2
stay. ~150 lines of code retire (the partition filter + its tests).
Sky's architecture doc §F.13 stays accurate; §F.14 (why Approach C
doesn't work) becomes purely historical.

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
