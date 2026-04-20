# Long-Term Risks and Mitigations

> **Audience:** Anyone maintaining erw on a multi-year horizon. Read this when you're about to adopt a new rustc nightly, when something unexpected breaks after a bump, or when you're evaluating whether to invest further in the current architecture vs. refactor around it.

This document catalogs the specific ways erw's architecture could degrade or fail over time, and what to do when each one fires. It's the companion to the architectural optimism in `rust-interop-guide.md` — that doc describes what works today; this one describes where the edges are.

The framing throughout: erw embeds a custom language into rustc's compilation pipeline via sanctioned extension points (`Config::override_queries`, `CodegenBackend` wrapping) plus a real on-disk two-crate cargo workspace (stub rlib + user bin) that rustc compiles as ordinary Rust. Zero rustc fork patches as of stage 4 (commits `ed2e692` → `c25aa4b`); stage 5c.4 retired `FileLoader` and the direct-mode `--toylang-input` path, so the stub rlib is now always a real on-disk crate. The architecture is built on decade-old rustc infrastructure, but rustc internals are explicitly-unstable-but-shape-stable — individual APIs drift, the overall integration model persists.

---

## 1. Risk taxonomy

Three tiers, used consistently throughout:

- **Category A: Shatter-tier.** Low probability (<5% over 5 years); would kill the architecture if they fired. Exit strategy: re-fork, possibly combined with upstream coordination.
- **Category B: Mechanism-level.** Moderate probability (15–50% over 5 years per item); weeks-to-months of facade rework when they fire. Architecture survives.
- **Category C: Operational invariants.** Ongoing hygiene. Failure is self-inflicted (someone introduces the regression); tests catch it fast.

Category A is rare but catastrophic. Category B is the realistic ongoing concern. Category C is discipline.

---

## 2. Category A: Shatter-tier risks

These are the "whole architecture stops working" scenarios. Each is unlikely and would give significant lead time if it started happening.

### A1. `rustc_private` locked down

**What.** The `#![feature(rustc_private)]` feature gate is required for every crate that pulls in `rustc_middle`, `rustc_driver`, `rustc_interface`, `rustc_codegen_ssa`, etc. erw's facade and toylangc both require it. If rustc ever removed `rustc_private` as a usable escape hatch, the facade couldn't compile at all.

**Probability: <5% over 5 years.** Rust-lang has explicitly stated (via multiple RFC discussions and project-goals documents) that `rustc_private` remains the "all bets off" nightly escape hatch even as `rustc_public`/`stable_mir` stabilizes. The stated direction is adding stable surface area alongside `rustc_private`, not removing it. Mature projects (rust-analyzer, miri, cranelift codegen backend, clippy) all depend on `rustc_private` or its close equivalents.

**Canaries.**
- Deprecation warning added to `#[feature(rustc_private)]` in a nightly release.
- RFC proposing removal or hard-gating.
- Public communication from the compiler team signaling sunset.

**Mitigation.** Monitor the compiler-team roadmap and `rustc_public` trajectory. If `rustc_public` stabilizes enough to replace significant portions of our rustc-internal surface, the migration becomes a proactive option. Today `rustc_public` covers ~40-50% of our read-side surface — the other half (query providers, MIR construction, `CodegenBackend` integration, partitioner hooks) has no stable equivalent on any current roadmap.

**Reaction if it fires.** Two possible paths:
- **Fork rustc** — the pre-stage-4 model. The three POC/spike worktrees + `docs/historical/rebuilding-rustc-fork.md` preserve the full fork-rebuild workflow. This is a multi-week retreat but viable; erw shipped with a 5-patch fork for most of its history.
- **Collaborate with rust-lang on unlocking the specific surface we need** — slower but better long-term.

**Long-term solution.** `rustc_public`'s stabilization trajectory is the right bet. If it eventually covers the load-bearing pieces (query providers, partitioner hooks, `CodegenBackend` integration), migrate proactively. See `docs/reasoning/rustc-fork-design-space.md` §4.4 for the current coverage analysis.

### A2. `Config::override_queries` removed

**What.** The primary extension point erw uses. Five of the six query overrides (`optimized_mir`, `symbol_name`, `layout_of`, `mir_shims`, `collect_and_partition_mono_items`, `upstream_monomorphizations_for`) route through this hook. If it were removed, the architecture's query-override layer would collapse.

**Probability: <5% over 5 years.** `Config::override_queries` predates erw by years. Used by rust-analyzer (for incremental/partial analysis), miri (for its custom execution model), and various testing tools. Removal would require migrating all of them; no such plan exists. The mechanism's public shape has been stable across multiple rustc internal refactors.

**Canaries.**
- Rust-analyzer or miri publicly migrating away from `override_queries`.
- Tracking issue opened proposing replacement.
- `rustc_interface` API deprecation warnings.

**Mitigation.** Same as A1 — monitor the compiler-team roadmap. Nothing proactive to do.

**Reaction if it fires.** Depends on the replacement. If rust-lang provides a new query-override mechanism (likely, given the cohort of dependents), migrate. If they remove it in favor of a compile-plugin API, redesign around that. Weeks-to-months of rework per migrated query. Partially mitigated by the fact that our query overrides are thin adapters — most of the real logic lives in `build_dependency_body`, the oracle, and the Inkwell backend, all of which are mechanism-agnostic.

**Long-term solution.** Track the `rustc_interface` API surface; participate in any RFC discussions about its evolution.

### A3. Query system replaced

**What.** Rustc's internal query system (Salsa-inspired, the backbone of all incremental compilation and lazy computation) is replaced by a fundamentally different architecture.

**Probability: <1% over 5 years.** This is rustc-wide re-architecture territory. Would be a multi-year effort with explicit community coordination; can't happen silently. Not on any current roadmap. Individual queries get restructured all the time (category B), but the overall system is deeply entrenched.

**Canaries.**
- Major compiler-team-blog announcement.
- Multi-year tracking issue for the replacement.
- RFCs discussing post-query compilation models.

**Mitigation.** Nothing proactive; the lead time would be years.

**Reaction if it fires.** Multi-month re-architecture of the facade's integration layer. The *concepts* (interleaved monomorphization, opaque stubs, two-sided codegen) transfer; the specific hooks don't. Worst-case scenario for erw, but with years of notice and likely guidance from rust-lang on the migration story.

**Long-term solution.** None currently needed. If the compiler team signals movement, engage early in the RFC process.

---

## 3. Category B: Mechanism-level risks

These are the realistic ongoing concerns. Each is moderate probability, and each has a bounded (weeks-to-months) rework cost. Budget for one of these hitting every 2–4 years.

### B1. Mono collector behavior drift

**What.** The `optimized_mir` override returns a synthetic MIR body containing `ReifyFnPointer` casts for Rust deps. Rustc's mono collector walks that body, substitutes Params per caller, and queues the Rust items for codegen. This behavior — collector walking our synthesized body and queueing deps via ReifyFnPointer recognition — is how dep discovery works. See `docs/reasoning/dep-discovery-approaches.md` (Approach B) for the mechanism in detail.

**Probability: 30–50% over 5 years.** The mono collector is one of the churn hot spots in rustc. It has been restructured multiple times (e.g., the `#55627` era) and will be again.

**Canaries.**
- Tests that exercise deep dep graphs (`test_diamond_call_pattern`, Phase 7 standalones) start failing with missing-symbol link errors.
- Rustc release notes mention changes to "mono collector" or "reachability."
- `tcx.collect_and_partition_mono_items` signature or semantics change.

**Mitigation.** The synthetic body at `rustc-lang-facade/src/queries/optimized_mir.rs::build_dependency_body` is ~150 LoC. Keep it simple — don't add features to it unless they're required. Fewer lines = smaller drift surface.

**Reaction if it fires.** Typically a 1–3 week repair:
1. Reproduce the failure on a single Phase 7 standalone (isolated, fast feedback).
2. Re-read the relevant rustc internals (`rustc_monomorphize/src/collector.rs`) to understand what changed.
3. Adapt `build_dependency_body` to the new collector behavior. Often this is "the Statement variant we were using has split in two" or "ReifyFnPointer cast kinds now require an extra field."
4. Verify 211/211.

**Long-term solution.** If the collector's extension contract were ever formalized (e.g., a stable "declare dependency" API), migrate to it. No such API exists today.

### B2. Partitioner / `mono_item_linkage_and_visibility` restructure (Outcome A assumption)

**What.** erw's stage-4c landing relies on a specific timing assumption: the plugin's `collect_and_partition_mono_items` override delegates to upstream, then mutates `MonoItemData.linkage` on `__lang_stubs` items before returning. The LLVM backend reads `data.linkage` directly from the returned CGU list and emits at that linkage, without re-querying from source attributes. See `rust-interop-guide.md` §2.2 / §10.6.4 for the shipping architecture, and this file's §1 commit-history note for provenance.

**The assumption stack:**
1. `mono_item_linkage_and_visibility` runs once, during upstream's default partitioner (`rustc_monomorphize/src/partitioning.rs:261-277`).
2. `internalize_symbols` runs BEFORE the query returns; the plugin's override runs AFTER.
3. LLVM backend reads `data.linkage` directly (`rustc_codegen_llvm/src/base.rs:89`), no re-derivation from source attributes.
4. Internalization only affects `(Hidden, can_be_internalized)` items; items with `External` linkage are immune (`partitioning.rs:268-269`).

If rustc ever (a) moves internalization after the query returns, (b) adds a new phase that re-reads source `#[linkage]` attributes, (c) moves the LLVM backend to re-query `mono_item_linkage_and_visibility` instead of reading from CGU data, or (d) restructures the query boundary so the plugin's mutation no longer persists — the architecture's linkage-setting mechanism silently breaks. `__lang_stubs` items start getting internalized. External linking fails.

**Probability: 20–30% over 5 years.** The partitioner has been restructured before (the `rustc_monomorphize` crate was factored out of `rustc_mir` in ~2019; internalization logic has moved multiple times). Not a question of if, but when.

**Canaries.**
- Phase 6 unwrap tests (`test_option_unwrap_basic`, `test_result_unwrap_basic`, `test_unwrap_arithmetic_chain`, and the suite of Vec/HashMap tests that exercise generic `#[inline(never)]` wrappers) start failing with link errors of the form `undefined symbol: __toylang_option_unwrap_i32` or `duplicate symbol: __toylang_*`.
- Rustc release notes mention "partitioner", "internalization", or "CGU linkage."
- The specific functions at `rustc_monomorphize/src/partitioning.rs:261-277` or `:533-609` (internalize_symbols) get meaningfully changed.

**Mitigation.** The canary tests above are comprehensive — Phase 6 wrappers exercise every generic-monomorphization path the architecture depends on. Don't dismiss unwrap-test failures as "probably unrelated." Any such failure after a rustc bump is evidence this assumption just broke.

**Reaction if it fires.**

Three responses in order of preference:

1. **Re-derive the same mutation at a different phase.** If the plugin's CGU mutation no longer survives, the alternative is installing a `mono_item_linkage_and_visibility` replacement via the same `Config::override_queries` mechanism. This gives us the exact same interception point patch 5 had, without a rustc fork — assuming rustc still supports overriding that specific function (or an equivalent).

2. **Re-introduce a small rustc fork hook.** The historical `VISIBILITY_OVERRIDE_HOOK` pattern (see `docs/historical/handoff-codegen-backend-plugin.md` §3 for the exact shape) is always available. A 1-patch fork is a 1–2 week retreat. Worth it if the no-fork mechanism can't be recovered.

3. **Upstream a new stable hook.** If the visibility-override use case is generalizable (likely — cranelift and gcc backends have similar concerns), an RFC for a sanctioned hook in `rustc_codegen_ssa` is the long-term answer. Multi-month timeline but permanent.

**Long-term solution.** (3) is where this eventually wants to land. In the meantime, (1) and (2) are bounded retreats.

### B3. MIR construction API surface drift

**What.** `rustc-lang-facade/src/queries/optimized_mir.rs::build_dependency_body` + `rustc-lang-facade/src/mir_helpers.rs` (~250 LoC combined) construct synthetic MIR bodies using `rustc_middle::mir` directly — `Body::new`, `BasicBlockData`, `Statement`, `Terminator`, `Rvalue::Cast(CastKind::PointerCoercion(...))`, `SourceInfo`, `UnwindAction`, etc. This is the biggest API-drift surface in erw.

**Probability: ~100% over any given 6-month bump** (low-severity drift); **~40% over 5 years** (structural change requiring meaningful rework).

**Canaries.**
- Compile errors at `build_dependency_body` when a rustc bump lands. MIR struct fields add/remove; enum variants split.
- `set_required_consts` / `set_mentioned_items` required-boilerplate may change shape.
- Specific `ReifyFnPointer` or `CoercionSource` API tweaks (both have shifted in the last 2 years).

**Mitigation.** Keep `build_dependency_body` minimal. It only has to emit ReifyFnPointer casts + SizeOf — don't grow it. `mir_helpers.rs` similarly stays as tight as possible. Fewer lines = less to adapt.

**Reaction if it fires.** Every rustc bump: expect ~1–3 days of fixing `mir_helpers.rs` for signature drift. Budget ~1 week per 6-month nightly bump. This was the dominant ongoing cost pre-stage-4 and remains the dominant ongoing cost post-stage-4 — zero-fork didn't change it.

**Long-term solution.** `rustc_public` does not cover MIR construction (read-only). No stabilization path exists. Accept the ongoing churn as a structural cost.

### B4. ABI helpers drift

**What.** `rustc-lang-facade/src/abi_helpers.rs` implements `CoercedReturn`, `CoercedParam`, and the `#[track_caller]` hidden parameter handling (see `@TCHAPZ`). These operate on `rustc_target::callconv::FnAbi`, which drifts with platform/target support changes in rustc.

**Probability: 15–25% over 5 years** for a meaningful rework.

**Canaries.**
- Compile errors in `abi_helpers.rs` after a bump. `PassMode` variants add or split (happened in the `#112757` era). `Conv` enum adds variants for new calling conventions.
- Tests that exercise specific ABI shapes (byte strings → `ScalarPair`; Vec returns → Indirect; `&stdout()` → scalar Pointer) start failing with argument-mismatch panics.
- Platform-specific ABI work in rustc (e.g., aarch64 SVE support) lands.

**Mitigation.** `abi_helpers.rs` is already tight (~300 LoC). Don't widen it speculatively for platforms we don't test on.

**Reaction if it fires.** 1–2 weeks of repair. Typically localized — a single `PassMode` variant change requires updating a handful of match arms and the coerced-type parser.

**Long-term solution.** Same as B3 — no stable replacement. Accept ongoing cost.

### B5. CGU lifetime erasure fragility

**What.** Stage 4a's `collect_and_partition_mono_items` override stashes consumer CGUs for the consumer's own codegen walk. `CodegenUnit<'tcx>` references carry a lifetime tied to `tcx.arena`, and stashing them across query boundaries requires lifetime laundering. The current implementation uses an `ErasedInstance` trick (commit `1d862f4`) — not `unsafe`, but custom.

**Probability: 15–20% over 5 years** for a breakage requiring redesign of the stash.

**Canaries.**
- Rustc bump produces lifetime errors in `rustc-lang-facade/src/queries/partition.rs` on recompile.
- The `CodegenUnit` struct changes its arena-relationship semantics.

**Mitigation.** The stash is small and localized. Keep it that way — don't stash additional data through the same mechanism. Don't let other modules depend on the erased representation.

**Reaction if it fires.** 1–2 weeks of redesign. Alternatives if `ErasedInstance` stops working: (a) process consumer items inline during the partition override instead of stashing (has re-entrancy concerns with other queries), (b) use `Arc<Mutex<...>>` with explicit `tcx.lift`, (c) narrow the stash to just `(DefId, GenericArgsRef)` tuples that don't carry lifetimes directly, re-fetching the CGU item state from rustc.

**Long-term solution.** None needed today. Monitor `CodegenUnit<'tcx>`'s semantic shape per bump.

### B6. Consumer `.o` emission is a side-effect of rustc query firing (incremental-cache interaction)

**What.** Toylang's consumer `.o` is produced as a side effect of facade callbacks that fire during rustc's query execution — specifically `notify_concrete_entry_point` (driven by `symbol_name`) populates `state.toylang_instances`, which `generate_with_tcx` consumes to emit the `.o`. Under rustc's incremental cache with a shared `CARGO_TARGET_DIR` and unchanged per-project sources, the query provider cache-hits; the side-effecting walk doesn't fire; `state.toylang_instances` stays empty; the emitted `.o` contains only items discovered via the direct CGU walk (accessors). The rlib gets rebuilt bundling an incomplete `.o`; downstream link fails with `__toylang_impl_main undefined` or similar.

**Probability:** **fires reliably** under rustc's incremental cache + shared target dir + unchanged sources. Does NOT bite the typical `toylangc build` wrapper-mode user flow (users edit `main.toylang` between builds, which invalidates the cache correctly and triggers query re-fire). The failure mode is specific to test harnesses with shared target dirs across many unchanged-source test projects.

**Canaries.** Symbols like `__toylang_impl_main` go undefined on a second build with unchanged source. Diagnosed by inspecting the rlib's archive (`ar -t <rlib>`) after repeated builds — the archive bundles an undersized `.o` that's missing most expected consumer symbols.

**Mitigation (shipping wrapper mode).** The preconditions don't fire under typical user flow. Monitor; no active concern.

**Mitigation (test harness).** Stage-5c.1 landed `CARGO_INCREMENTAL=0` scoped to the integration-test harness. Package-level cache (`test_helpers`, crates.io deps) still benefits from sharing; only rustc's per-crate fingerprint is forfeit. Per-test marginal cost stays ~0.3–0.4s warm — comfortable inside the runtime budget.

**Architectural fix (deferred).** Two options:

1. **Drive consumer codegen from the registry directly** rather than relying on `notify_concrete_entry_point`'s side effects. Removes the "query must fire for codegen to be correct" dependency; `state.toylang_instances` gets built deterministically each compile. More invasive; cleanest architecturally.
2. **Route the `.o` through cargo-tracked paths via a `build.rs` in the stub rlib** that invokes `toylangc` as a subprocess and emits `cargo::rustc-link-arg=` + `cargo::rerun-if-changed=` directives. Cargo treats the `.o` as a tracked build artifact; cache invalidation becomes cargo's responsibility. Scoped fix; requires `toylangc` to be invokable as a subprocess from build scripts.

**References.** Diagnosis in commit `91cad25`. Harness stopgap in the same commit's `integration_projects.rs`.

### B7. Bool extern-arg return-type leak in toylang codegen

**What.** Pre-existing latent bug in toylang's codegen path for functions containing extern "C" calls returning void. Toylang emits `define i8 @__toylang_internal_do_print()` with a `ret void` terminator — an LLVM type mismatch (declared return type vs terminator shape don't agree). Hash-order-dependent: the project name `extern_fn_call` reproduces the bug; identical toylang source under a different project name (`probe_extern_void`) does not. Traced to how bool return types leak through `__toylang_internal_*` wrapper generation when the wrapped extern returns void.

**Probability:** fires on any toylang program that wraps a void-returning extern "C" Rust fn in a context where the internal ABI derivation picks up a stale bool. Stage-5c migration exposed it via 1 of 57 extern-fixture tests.

**Canaries.** LLVM IR verifier errors at build time: "function return type does not match ret terminator" or similar. Internal fn signature shows `i8` return but body has `ret void`.

**Mitigation.** None applied. The affected test is parked in `integration_projects.rs` with an inline TODO; blocks 1 of 57 migrated tests but doesn't block stage 5c overall.

**Reaction if investigated.** Trace through `llvm_gen.rs::codegen_internal_function` — specifically the return-type derivation for internal-ABI wrappers around extern-C-void calls. Likely a bad default in the ABI-coerced return mapping. Probably a few hours' fix once the exact code path is isolated. Non-architectural; a straightforward codegen-correctness bug fix.

**References.** Diagnosis in commit `a2f06ea`. Test inline-documented in `toylangc/tests/integration_projects.rs`.

---

## 4. Category C: Operational invariants

These are rules you follow to keep the architecture working. Failures here are self-inflicted — the tests usually catch it immediately, but knowing the invariants prevents the regression.

### C1. `@DPSFDOZ` — don't use `def_path_str` outside `generate_and_compile`

**Rule.** `tcx.def_path_str` is diagnostic-only; it ICEs during normal compilation outside diagnostic contexts. See `docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md`.

**What to do instead.** Use `is_from_lang_stubs_safe` (walks `tcx.def_path(def_id).data` structurally) — added in stage 2, the canonical cross-phase-safe version.

**How this gets violated in practice.** Someone adds a new query provider or partitioner hook, writes an inline `tcx.def_path_str(def_id).starts_with("__lang_stubs::")` check because it's shorter than the DefPathData walk, and it silently works in tests that happen to run inside `generate_and_compile` but ICEs in some other integration path. The arcana doc calls this out; the helper exists to make the safe form trivially reusable.

**Detection.** Panic messages containing `'trimmed_def_paths' called, diagnostics were expected but none were emitted`.

### C2. `@GCMLZ` — don't introduce new locking sites outside `generate_and_compile`

**Rule.** `MUTABLE_STATE` is a Mutex held for the duration of `generate_and_compile`. Query providers fire on Rayon worker threads; if a new provider tries to lock `MUTABLE_STATE` while `generate_and_compile` is running, deadlock returns. See `docs/arcana/GenerateCompileMutexLock-GCMLZ.md`.

**What to do instead.** New query providers should read only from `CONFIG` and `DEFAULT_*` OnceLocks (no lock needed). If a provider genuinely needs `MUTABLE_STATE`, it must be installable only during the `generate_and_compile` phase where the lock is already held by the calling thread (in which case re-entrance avoidance matters — route through the `_inner` helpers that bypass the mutex).

**How this gets violated.** Someone adds a feature that needs to mutate consumer state during query-provider execution and locks `MUTABLE_STATE` naively. Deadlock.

**Detection.** Tests hanging with 0% CPU. Phase 4's `stdout()` case was the historical precedent (see the `@GCMLZ` arcana).

### C3. New query providers must preserve the plugin's partitioner invariant

**Rule.** The partitioner override (`collect_and_partition_mono_items`) filters consumer items out of the CGU list and forces `(External, Default)` linkage on remaining `__lang_stubs` items (the Phase 6 wrappers). Any new query provider that depends on CGU-list composition or linkage assumptions must be aware of this filter.

**How this gets violated.** Someone adds a new query that reads CGU contents and is surprised consumer items are missing, or adds visibility logic that interacts with the post-partition linkage mutation.

**Detection.** New-feature tests fail in ways that suggest "consumer item missing from where expected." Re-check the partitioner filter's interaction.

---

## 5. Mitigating factors

Background facts that reduce the overall risk profile:

### Co-travelers

erw is not alone. Multiple mature projects occupy the same neighborhood of "deep rustc integration via nightly extension points":

- **rust-analyzer** — uses `FileLoader` + query interception extensively; survives rustc internals drift continuously.
- **miri** — uses `Config::override_queries` to install interpretation; rustc-internal API churn is routine maintenance.
- **clippy** — deepest HIR/lint integration of any; ongoing maintenance.
- **cranelift codegen backend** — replaces `rustc_codegen_ssa`; pins nightly, adapts per bump.
- **rust-gpu** — also a codegen backend replacement.

Your risk correlates with this cohort. If rustc ever threatens any single one of them, it's an early signal for erw. Monitor their issue trackers for "this just broke on nightly" events — you'll see the drift wave before it hits erw.

### `rustc_public` trajectory

The stable-MIR effort (now `rustc_public`) covers ~40–50% of our read-side rustc surface. If it eventually stabilizes, that portion of erw becomes stable-Rust-compatible. The load-bearing pieces (query providers, MIR construction, `CodegenBackend`, partitioner hooks) have no equivalent on the stabilization roadmap, but even partial migration would reduce drift surface meaningfully. See `docs/reasoning/rustc-fork-design-space.md` §4.4 for the current coverage breakdown.

### Nightly-pin strategy

Currently pinned to `nightly-2025-01-15`. Bumping is a conscious event, not a silent drift — you control when to pay the API-drift cost. Recommended strategy:

- **Don't chase latest nightly.** Bump to a ~3-month-old nightly when you do. Gives the ecosystem time to stabilize around any rustc internals changes; avoids riding the bleeding edge.
- **Bump in dedicated sessions.** Batch the drift repair work into its own commit/PR cycle; don't mix with feature work. Makes it easier to bisect when something breaks.
- **Re-run the full 211-test suite** after every bump. No subset — the full suite catches the most regressions.

### 211 tests as canary

The test suite is your primary regression detector. 67 unit + 129 integration + 15 standalone. Coverage:

- **Phase 6 tests** catch Category B2 (Outcome A assumption breakage).
- **Phase 7 standalones** catch Category B1 (mono collector drift) and B3 (MIR construction API drift).
- **ABI tests** (Vec, ScalarPair, byte strings, `&str`) catch Category B4 (ABI helpers drift).
- **Integration tests** with diamond / two-entry-point-shared-internal shapes catch Category C regressions.

Trust the tests. First failure after a rustc bump is your diagnostic opportunity — investigate root cause, don't paper over with quick fixes. The nightly pin makes this a deliberate investigation, not a panic.

---

## 6. Exit strategies

If something in Category A or an especially-bad Category B lands, erw has escape routes:

### Return to a small fork

The `~/rust` worktree on branch `per-instance-mir` is preserved even post-stage-4 — empty (`git diff --stat` returns nothing) but structurally set up. `docs/historical/rebuilding-rustc-fork.md` documents the 5-step toolchain rebuild workflow. If a Category-B2-tier breakage forces reintroducing `VISIBILITY_OVERRIDE_HOOK` (or some equivalent hook), that's a 1–2 week retreat: add the patch, rebuild, switch toolchain.

Historically erw shipped with a 5-patch fork for most of its development. Forking is not an architectural defeat; it's a recovery mechanism. Zero-fork is an ideal state, not a constraint.

### Upstream contribution

If a Category-B breakage generalizes (i.e., other consumers of `rustc_codegen_ssa` would benefit from a sanctioned hook), an RFC for a stable extension point is the long-term answer. The ModuleLlvm-wall spike's `findings.md` sketches one such PR direction (~30-line change to expose `LlvmCodegenBackend` constructors). Other hook-shaped proposals would follow similar process.

Upstream contribution takes months but is permanent. Worth pursuing when the use case clearly generalizes.

### Accept the fork cost

If neither workaround is viable, accept the small fork and treat it as maintained infrastructure. A 1-patch fork maintained at ~1–2 days per nightly bump is genuinely not a project-threatening cost — it's what erw lived with for most of its history. The pre-stage-4 math (`docs/reasoning/rustc-fork-design-space.md` §5) still works as a baseline.

---

## 7. Summary table

| Risk | Category | Probability | Impact | Canary |
|------|----------|-------------|--------|--------|
| `rustc_private` locked down | A1 | <5% / 5yr | Architecture ends | Compiler team roadmap announcement |
| `override_queries` removed | A2 | <5% / 5yr | Query layer ends | rust-analyzer/miri migrating away |
| Query system replaced | A3 | <1% / 5yr | Full re-architecture | Multi-year RFC |
| Mono collector drift | B1 | 30–50% / 5yr | 1–3 weeks repair | Phase 7 link errors |
| Partitioner / Outcome A breakage | B2 | 20–30% / 5yr | 1–4 weeks repair or small re-fork | Phase 6 unwrap tests fail |
| MIR construction drift | B3 | 100% / each bump | ~1 week / bump | `mir_helpers.rs` compile errors |
| ABI helpers drift | B4 | 15–25% / 5yr | 1–2 weeks repair | ABI-shape tests fail |
| CGU lifetime erasure | B5 | 15–20% / 5yr | 1–2 weeks redesign | partition.rs lifetime errors |
| `.o` emission side-effect / incremental cache | B6 | Fires reliably under specific conditions | Test harness: harness-scoped stopgap ✓ / Shipping: doesn't trigger typical flows | Undefined `__toylang_impl_main` on second build |
| Bool extern-arg return leak | B7 | Fires on narrow code shape | 1 of 57 tests parked; non-blocking | LLVM verifier: "return type does not match ret" |
| `@DPSFDOZ` violation | C1 | Regression-risk | Self-inflicted | `trimmed_def_paths` ICE |
| `@GCMLZ` violation | C2 | Regression-risk | Self-inflicted | Test hangs (0% CPU) |
| Partitioner invariant violation | C3 | Regression-risk | Self-inflicted | Unexpected missing items |

---

## See also

- `docs/architecture/rust-interop-guide.md` — the shipping architecture in detail.
- `docs/reasoning/rustc-fork-design-space.md` — the design-space analysis that led to zero-fork. §5 has the cost accounting.
- `docs/reasoning/why-interleaved-monomorphization.md` — the architectural invariant (interleaving with rustc's monomorphization) that the whole approach depends on. If this invariant ever became infeasible to satisfy, that would be a Category A risk beyond the ones listed here.
- `docs/reasoning/dep-discovery-approaches.md` — the mechanism that Category B1 could break.
- `docs/arcana/` — the cross-cutting invariants that Category C protects.
- `docs/historical/rebuilding-rustc-fork.md` — the exit strategy for Category A or severe Category B.
- `HANDOFF-TL.md` §6 — what lives in the outgoing TL's head, including writing conventions and locking discipline that support Category C.
