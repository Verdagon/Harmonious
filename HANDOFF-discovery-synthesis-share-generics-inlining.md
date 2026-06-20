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

---

## TL;DR

Four open threads, interrelated. Roughly in order of risk-reduction value:

1. **Inlining test matrix (Thread C)** — we have ONE inlining-verification
   fixture (`test_lto_smoke`); the rest is hope. User wants this "way
   overboard." Expect ~30-50 fixtures + a reusable disassembly-assertion
   harness. Cost: ~10-15 hours. Likely catches real bugs.
2. **Discovery/synthesis/filter machinery (Thread A)** — three Sky-side
   layers (capture-ship-replay, synthesize-upstream-monomorphizations,
   partition filter) coordinate to handle the F.13 cascade-timing
   problem. Investigate whether any can be pruned/eliminated, especially
   in light of the new release-mode fix's machinery. Cost: ~1-2 weeks
   for full investigation; partial pruning is hours.
3. **share_generics handling (Thread B)** — current support is forced-on
   at `__lang_stubs`, hard-error if user disables it there, otherwise
   user choice. Decide if we should support/honor it differently in
   other configurations. Cost: ~1 day investigation + decision.
4. **The "kill partition filter" sub-thread (Thread A.5)** — separable
   from the rest. If the partition filter could be eliminated, both A.1
   (capture-ship-replay) and A.2 (synthesize-upstream-monomorphizations)
   dissolve naturally. Requires either an upstream RFC
   (`#[codegen_backend_provides_body]`) or a Sky-side replacement
   mechanism. Cost: investigation ~3-5 days; implementation likely
   multi-week.

Recommended order: **C, then A in priority order, then B**. C is most
risk-reducing for Sky proper; A is hygiene; B is design-space exploration.

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

### Recommended sequence for Thread A

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

### Current state

| Verified | Mechanism |
|---|---|
| Sky body inlined into Rust caller at -O3 + thin LTO (Case 2) | `test_lto_smoke` disassembles `lto_smoke::main` and asserts no `bl` to Sky symbols + constant-folded result `mov w8, #50` |
| Sky-emitted stubs never MIR-inlined | `architecture_fence.rs` asserts `#[inline(never)]` on stubs in stub_gen output |

That's it. Everything else is hopeful inference.

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

— Previous engineer, via Claude Opus 4.7 (1M context),
  Claude-Session: https://claude.ai/code/session_014jTbwcznQUd4i89tbLMdAa
