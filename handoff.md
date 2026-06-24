# Sky Architecture Review (Rounds 1–3, 2026-06-23) — Implementation Handoff

**Source conversation:** the full three-round review exchange that inspired this handoff is archived at [`./convo-with-rustc.md`](./convo-with-rustc.md). This document is the distillation; the conversation is the rationale at full fidelity if you ever need it.

**Read this section first.** Below the major `=====` separator is prior-session history (mostly shipped work) — useful context but lower priority than what's in this section.

This section captures everything that came out of a three-round design-review exchange on `rust-interop-architecture.md` conducted 2026-06-23, plus the implementation work it commits us to. We've closed the review exchange and are pivoting to implementation. Round 4 with the reviewer happens after we have empirical data + implementation surprises in hand.

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

## Status as of writing (2026-06-24)

**Closed:**
- The review exchange itself. Three rounds. Reviewer signed off; expecting round 4 only after we bring back bench numbers + implementation surprises.
- All architectural decisions documented below.
- **Phase A (empirical baseline drop fixtures)** — DONE 2026-06-23. Findings A.1–A.10 documented in the Empirical section below; headline: the previous `mir_shims` override was silently no-op'ing rather than performing useful work, validating Decision 1's premise empirically.
- **Phase E (mir_shims elimination)** — DONE 2026-06-23. `mir_shims` override + `drop_glue.rs` + `mir_helpers.rs` + the `DEFAULT_MIR_SHIMS` plumbing all deleted. Rustc's default DropGlue path fires unchanged; per-type drop semantics flow through a compiler-synthesized `Drop::drop(&local)` AST node inserted at scope end (Phase E.b/E.c/E.d below).
- **Phase E.b/E.c/E.d (scope-end drop emission)** — DONE 2026-06-23. The compiler synthesizes `TypedStmt::ExprStmt(TypedExprKind::StaticCall { ty: "Drop", method: "drop", args: [Ref(local)] })` at scope-end positions of every void-returning function whose body has a let-binding with a Drop-implementing type. The synthesis runs **once** in `insert_scope_end_drops` and is invoked from both the dep-collection site (`type_resolve_body` in `callbacks_impl.rs`) and the codegen site (`codegen_internal_function` in `llvm_gen.rs`). After that pass, every downstream stage — dep walker, mono cascade, codegen, symbol resolution, link — treats drop calls as ordinary trait static calls with no drop-specific code paths. 9 drop fixtures + 333 existing tests pass cold: **342 / 0 / 1 ignored**. See Decision 1 (substantially revised below) for the architectural detail.
- **Phase B (perf bench)** — DONE 2026-06-24 (commits `81b6eb1` + later doc landing). 17 bench fixtures land under `toylangc/tests/integration_projects/perf_bench/` (5 Bench-1 configs + 2 Rust-only baselines + 6 Bench-2 K/LTO combos + 4 Bench-3 drop variants). Runner at `toylangc/tests/scripts/run_perf_bench.sh` builds, runs, and reports a markdown table. Headline numbers (M-series macOS, LLVM 21.1.8): **Bench 1 LTO ratio = 1.50× → handoff "<2× → lock §5.5 confidently" gate fires**; **Sky vs Rust baseline at O3 thin = 0.3% delta (essentially identical)**; **Bench 3 drop chain LTO ratio = 26.5×**. Full results + interpretation in `tmp/perf-bench-summary.md` and `rust-interop-architecture.md` §22.4 (which now includes §22.4.2 reproduction steps and §22.4.3 interpretation assumptions). Two follow-up findings surfaced: **F3** (Sky main + while loop + 1M+ allocations stack-overflows due to toylangc O0 alloca recycling bug) and **B10 residual** (Phase E drop synthesis + Vec<SkyStruct> at opt-level ≥ 1 still trips the LLVM 21 BitcodeWriter bug under ThinLTO cross-CGU import; arch doc §25.2 B10 tightened from "CLOSED" to "CLOSED for primary path; residual trigger…").
- **Phase I (cache_on_disk_if audit)** — DONE 2026-06-24. **Decision 14's prescribed Provider-slot syntax (`providers.queries.layout_of_cache_on_disk_if = ...`) doesn't exist on current nightly.** `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL, not a Provider slot. The audit re-derived cache-safety: every Sky-overridden query is safe by construction (`per_instance_mir` declared `false` in fork patch; `layout_of` + `cross_crate_inlinable` default to `false`; `collect_and_partition_mono_items` is `eval_always`; `symbol_name` is disk-cached but its override is scheduled for removal per Decision 2 and the default mangler invalidates correctly). Annotated all 5 override files with `cache-audit:` marker comments. New CI fence `toylangc/tests/cache_audit.rs` asserts every override carries a marker. New §22.4.1 documents the policy table; new B21 risk entry tracks "per-query disk-cache staleness if rustc evolves the cache API." Decision 14 itself is revised below.

**Not yet started (your work):**
- Phases C (tool attribute), D (predicate migration), F (symbol_name elimination), G (cdylib), H (FFI shape), J (u128 typeids), K (content-hash const args), L (per-view refs), M (async typestate), N (recursion safety), O (drift fences).

**In flight from prior sessions (independent of this exchange):**
- Patch 5 retirement (shipped 2026-06-22 per the historical section below).
- Thread B (share_generics support boundary) — open, lower priority than the new work.

---

## TL;DR

If you only have time to read 200 words, here it is:

The review exchange committed us to **17 architectural changes**, most importantly: eliminate the `mir_shims` query override (drop becomes a normal generic Sky function call), eliminate the `symbol_name` override (live no-op), replace the partition filter predicate with a `#[skyc::emit_consumer_body]` tool attribute, ship Sky's backend as a **cdylib** instead of statically linked, use **`#[repr(C)]` function-pointer struct** instead of `&mut dyn Trait` across the cdylib FFI boundary, retire the slab-pointer-as-u64 surface in favor of **content-hash const args** (u128 with collision detection), introduce **per-view ref types** `SkyRef<T, V>` with `V ∈ {Frozen, Mutable}` for Send/'static honesty at the Rust boundary, narrow `#[may_dangle]` policy to a **syntactic rule** (T appears only behind pointer indirection), and audit + document cache-on-disk-if discipline (Decision 14 — note: 2026-06-24 audit found the prescribed Provider-slot API doesn't exist on current nightly; every Sky-overridden query is cache-safe by construction, documented via `cache-audit:` markers + CI fence at `toylangc/tests/cache_audit.rs`).

Critical caveat (historical): **toylang had zero drop tests**, so the mir_shims elimination had no empirical baseline. Phase A built fixtures FIRST, validated baseline under current model, THEN did the migration with fixtures as the regression suite. Both phases shipped 2026-06-23. Phase B (perf bench) shipped 2026-06-24 with similar discipline — build fixtures, run, capture, interpret, land empirical anchors in the doc. Decision-gate verdict: **lock §5.5**.

**Implementation order, top 5 (refreshed 2026-06-24 after Phases A/E/B/I shipped):**

1. ~~Drop fixtures~~ — DONE (Phase A, 2026-06-23). 9 fixtures + 333 existing tests passing.
2. ~~Perf bench~~ — DONE (Phase B, 2026-06-24). 17 fixtures; Bench 1 ratio 1.50× → lock §5.5; Bench 3 drop chain 26.5×. See §22.4 + `tmp/perf-bench-summary.md`.
3. ~~mir_shims elimination~~ — DONE (Phase E + E.b/c/d, 2026-06-23). AST-rewrite drop synthesis is shipped; the override + `drop_glue.rs` + `mir_helpers.rs` are deleted.
4. ~~cache_on_disk_if audit~~ — DONE (Phase I, 2026-06-24). Audit found prescribed API doesn't exist; all queries cache-safe by construction; CI fence in place.
5. **Tool attribute infrastructure + partition predicate migration (Phase C+D)** — NEXT. ~2-3 days. Wire up `#![register_tool(skyc)]` + `#[skyc::emit_consumer_body]`; migrate `is_consumer_codegen_target` to the two-gate attribute conjunction (Decision 3).

After that: symbol_name elimination (Phase F, ½ day), cdylib build system + patch 4 rev 3 (Phases G+H, ~1 week), u128 typeids + content-hash const args (Phases J+K, ~1.5 weeks), per-view ref types + async typestate (Phases L+M, ~3 weeks).

**Round 4 deliverables** (when you come back to the reviewer):
1. **Perf bench numbers — IN HAND.** Bench 1 ratio 1.50×; Sky vs Rust baseline at 0.3% delta; Bench 3 drop chain 26.5×. Decision-gate verdict: lock §5.5.
2. **Cache_on_disk_if audit results — IN HAND.** Prescribed API doesn't exist; every override cache-safe by construction; CI fence in place.
3. **Drop fixture outcomes — IN HAND.** 9 fixtures pass; the prior `mir_shims` override was empirically broken (never fired; Finding A.6).
4. **mir_shims elimination empirical validation — IN HAND.** AST-rewrite shape ships at 342/0/1 tests; no regressions in the 191 prior integration tests.
5. **Implementation surprises grouped by where they surfaced — IN HAND.** F3 (Sky main + while loop stack overflow due to toylangc O0 alloca recycling), B10 residual (Phase E drop synthesis + Vec<SkyStruct> still trips bug under ThinLTO cross-CGU import); both documented in arch doc §F.18.

The round 4 conversation now has the data anchors the reviewer asked for. The remaining open items are post-bench: Phase C/D/F (tool attribute + predicate migration + symbol_name elimination) is the next chunk of execution work.

---

## The review-exchange story

Three rounds across one focused day.

**Round 1**: reviewer asked 31 questions across 11 categories (fork patches, stub rlib, types, groups, marker traits, comptime, async, cascade timing, distribution, operational discipline, rustc-interaction subtleties). I worked through each one; the user weighed in on the substantive ones. Some were "the doc is correct, here's why"; some surfaced doc errors (Sky's groups described as runtime arenas when they're actually compile-time; §12.2's "honest 'static" claim was overstated); some surfaced promising alternatives we adopted (per-view ref types for Send/Sync honesty; u128 typeids instead of u64; content-addressed naming).

**Round 2**: reviewer pushbacks. Several round-1 answers were too soft; some had real holes. They pushed us to be honest about: backend pluralism being the actual load-bearing reason for owning LLVM bytes (not "MIR can't express it" which is mostly false); the cdylib FFI shape's real failure modes; the `#[may_dangle]` discipline being narrower than I'd framed it; symbol_name override being a live no-op worth killing; the `is_consumer_codegen_target` predicate being unspec'd.

**Round 3** (this is where most of the major decisions crystallized): the user's "drop is just a normal generic function call" insight unlocked the **mir_shims elimination** — the single largest architectural simplification from the entire exchange. We ran an 8-agent investigation to validate; it converged on "yes, the simplification works." Reviewer pushed back on the verification's theoretical nature (toylang doesn't actually test drop), the partition filter predicate's lack of explicit specification, the `#[may_dangle]` policy being too permissive, and four cdylib-related details. We adopted all the pushbacks. Then four follow-up questions from us on the new commitments (may_dangle auto-inference, cdylib FFI shape, SkyRef coherence, Pin discipline through Drop bridge), and the reviewer's brief final note on the cache_on_disk_if audit list.

**Net architectural changes from the exchange:** see the Decisions Log below. Net doc-correction work: see the Doc Update Plan.

**Net empirical gaps surfaced:** toylang has zero drop tests; nothing has ever validated the mir_shims override (or the proposed replacement) end-to-end; the "no inlining without LTO" perf model hasn't been measured.

**Net relationship change with the reviewer:** they're invested in the architecture being right; they want bench data before further design discussion. The relationship is collaborative and substantive; future rounds will be high-value if we bring data.

---

## Decisions log — every architectural commitment from the exchange

For each decision: **WHAT** (the commitment), **WHY** (the load-bearing reason), **ALTERNATIVES CONSIDERED** (and why rejected), **IMPLEMENTATION NOTES** (what code touches), **GOTCHAS** (what to watch out for), and **DOC IMPACT** (which arch-doc sections need updating).

### Decision 1: Eliminate the `mir_shims` query override

**STATUS: SHIPPED 2026-06-23** (Phase E + E.b + E.c + E.d). The
implementation reality differs from the original "bridge through
`__sky_drop_X<T>`" plan in a clarifying way — see **AS IMPLEMENTED**
below for the actual shape.

**WHAT.** Remove the `mir_shims` override entirely. Drop becomes a
normal Rust trait method call (`Drop::drop(&local)`) emitted at scope
end. Sky's stub_gen emits standard `impl Drop` blocks for Sky types
whose Sky source declares `export impl Drop for X`. The compiler
inserts the scope-end calls; everything downstream treats them as
ordinary trait static calls.

**WHY.** Drop is not architecturally special; it's just a function
that the language sometimes auto-calls. The user's insight in round
3 — "destructors are not special; mir_shims overrides an entire
rustc mechanism when we could just bridge through standard Rust
trait dispatch" — collapsed the special-case machinery into the
general mechanism. The mir_shims override was a category mistake:
treating drop as a thing that needs its own emission path when it
actually fits perfectly into the existing per_instance_mir + cascade
discovery path.

**AS IMPLEMENTED (Phase E.d AST-rewrite shape — 2026-06-23).** The
plan above describes a "bridge through `__sky_drop_X<T>` placeholder
fn" mechanism. The shipped implementation is *more principled*: there
is no bridge function, no placeholder, no separate Sky drop fn. The
compiler synthesizes a real `Drop::drop(&local)` AST node at scope
end and lets it flow through the same machinery as any other trait
static call. The pipeline:

1. **`stub_gen` emits `impl Drop for X { fn drop(&mut self) { unreachable!() } }` for every Sky struct with `export impl Drop`** (already required by toylang's `is_export` discipline; the user's source has the impl). Plus an unconditional `pub use core::ops::Drop;` re-export so the synthesis pass below can resolve the trait DefId regardless of whether the user wrote `use std::ops::Drop`. The re-export is gated on absence — if the user's source already imports `Drop`, stub_gen skips it (avoids name collision).
2. **Type resolution runs.** `resolve_fn_body` produces the typed AST as usual.
3. **`insert_scope_end_drops` runs immediately after `resolve_fn_body`.** For void-returning functions whose body has any let-binding whose type needs a drop, the pass appends `TypedStmt::ExprStmt(TypedExprKind::StaticCall { ty: "Drop", method: "drop", args: [Ref(local)] })` to the block's stmts in REVERSE declaration order (LIFO — Rust's drop order). Non-void-returning fns are skipped entirely: the caller's let-binding owns the eventual drop via move semantics.
4. **Predicate.** `local_needs_scope_drop(tcx, ty, registry)` returns true for:
   - Sky struct types (`StructRef` / `Struct`) whose `name` matches an `imp.trait_name == "Drop" && imp.self_type_name == name` entry in the registry's `trait_impls`.
   - Rust types (`RustType`) whose ADT has an explicit `impl Drop`, queried via `tcx.adt_destructor(adt_def.did())`. This filters out `Option`/`Result`/`Stdout`/primitives whose drop semantics flow through auto-generated DropGlue with no trait-method symbol; calling `Drop::drop` on those ICEs rustc's mono collector ("failed to resolve instance").
5. **The synthesized calls run through the existing pipeline unchanged.** `walk_typed_body_for_deps` collects them through its normal `rust_method_deps` arm. `collect_rust_deps_recursive` queues `<T as Drop>::drop` as a Rust dep through the standard trait-method dispatch. The per_instance_mir cascade surfaces them. `is_consumer_trait_impl_method` recognizes Sky's `<Widget as Drop>::drop` exactly the same way it recognizes `<Widget as Clone>::clone`. `fill_extra_modules` emits the body for Sky-owned impls; rustc emits the body for std-owned impls like `<Vec<T> as Drop>::drop`.
6. **`codegen_extern_wrapper` reuses existing function declarations.** Switched from unconditional `module.add_function(extern_symbol)` to `get_function(extern_symbol).unwrap_or_else(add_function)` because `declare_external_fn` may have already declared the symbol when the body referenced it via a previous call site (Fixture 6 surfaced this: `__toylang_internal_main`'s synthesized `Drop::drop` call declared the extern before the cascade-drain emitted the body, and the unconditional add produced an LLVM `.1` symbol-disambiguation collision that left the call site unresolved).

This is what "drop is just a function" actually looks like in code:
ONE Drop-aware site (`insert_scope_end_drops` + the predicate it
uses); everything downstream is generic.

**The 8-agent investigation (round 3, before implementation) confirmed the simplification works.**
- Field auto-drop is a no-op for Sky stub types (they're ZST-only).
- Rustc's collector walks drop generically through the same path it uses for any other call.
- Cross-crate generic drop works under §5.5's rules (mono'd at user_bin).
- Const-generic SkyOpaqueType drop works via plain `pub fn __sky_drop_opaque<const T: u128>` — though under the AS IMPLEMENTED shape this becomes `Drop::drop(&opaque)` resolving to a Sky-emitted `<SkyOpaqueType<T> as Drop>::drop` (same mechanism as Widget; the bridge fn collapses out).
- Linear types + async futures + Pin/Unpin + vtable dispatch all preserved.
- The previous mir_shims override was vestigial — **empirically validated 2026-06-23 (Phase A finding A.6)**: even with the override installed and a stub `impl Drop` block in place, the override's `consumer_struct_name` lookup path never fired for any test fixture. `drop_in_place::<Widget>`'s body was a no-op (just stack save/restore/ret) and `<Widget as Drop>::drop` was absent from the binary entirely. The handoff's "build fixtures first" gate revealed the previous machinery had never worked. mir_shims removal lost zero functionality.

**ALTERNATIVES CONSIDERED (additional considerations from the implementation).**
- **Bridge through `__sky_drop_X<T>(self as *mut _)` (the original plan).** Two-layer (stub-emitted Drop impl → `__sky_drop_X<T>` Sky fn → real body). Architecturally fine but **functionally equivalent to the direct dispatch** for the non-comptime case, and toylang has no comptime today. The bridge's primary value (sharing one drop fn across content-hash-keyed `SkyOpaqueType<T>` variants) is a v2 concern when comptime lands. We picked the simpler direct dispatch for v1.
- **LLVM-IR-layer emission of drop calls (Phase E.b/E.c shape, briefly shipped).** Worked but leaked specialness across four sites: `walk_typed_body_for_drop_types`, `local_needs_scope_drop_for_deps`, the drop dep arms in `collect_rust_deps_recursive`, and `emit_scope_drops` / `emit_drop_in_place` / `emit_sky_struct_drop` in `llvm_gen`. Refactored to AST-rewrite (Phase E.d) for principled adherence — see the "principle audit" subsection below.
- Keep mir_shims (the pre-handoff state). Rejected: extra query override, special-case machinery, less capable per-T than per_instance_mir for generic types, and (empirically) didn't actually work.
- Standard Drop impl with per-T iteration in Rust source (e.g., Vec's drop iterates elements). Rejected: leaks opacity (Rust source sees Sky's internals).
- Runtime dispatch via typeid (universal Drop impl + dispatch in Sky's runtime). Rejected: per-drop overhead; per_instance_mir gives compile-time per-T specialization.

**FILES TOUCHED (Phase E + E.b + E.c + E.d, 2026-06-23).**
- `rustc-lang-facade/src/queries/drop_glue.rs`: DELETED.
- `rustc-lang-facade/src/mir_helpers.rs`: DELETED (orphaned — its only consumer was `build_drop_call_body` for mir_shims).
- `rustc-lang-facade/src/queries/mod.rs`: removed `mir_shims` from providers registration + `install_query_defaults` arg list + module-level doc header. Pointed forward at the AST-rewrite mechanism.
- `rustc-lang-facade/src/lib.rs`: removed `DEFAULT_MIR_SHIMS` OnceLock, `default_mir_shims()` accessor, `install_query_defaults`'s `mir_shims` parameter.
- `toylangc/src/toylang/parser.rs`: extended `parse_impl_method` to accept `&mut self` (Phase A — required by Drop's receiver).
- `toylangc/src/toylang/registry.rs`: `ToyImplMethod` gains `is_self_mut: bool` (serde-default for back-compat).
- `toylangc/src/stub_gen.rs`: emits `fn drop(&mut self)` receiver when flag set; conditionally re-exports `core::ops::Drop` when absent from user imports.
- `toylangc/src/toylang/callbacks_impl.rs`: added `insert_scope_end_drops` (pub) + `local_needs_scope_drop` predicate (queries `tcx.adt_destructor`) + `synth_scope_drop_call` builder. Wired into `type_resolve_body` (dep-collection site).
- `toylangc/src/llvm_gen.rs`: calls `insert_scope_end_drops` from `codegen_internal_function` (codegen site); switched `codegen_extern_wrapper` from unconditional `module.add_function` to `get_function`-or-add to handle the case where the body's call site declared the extern symbol first.
- `toylangc/tests/integration_projects/drop/`: 9 new fixtures (basic chain, two-Vec LIFO, helper-fn drop, empty Vec, Widget without Drop, single Sky local with Drop, multiple Sky locals LIFO, Sky local without Drop impl, Sky local in helper fn). All pass cold.

**GOTCHAS.**
- **Both dep-collection and codegen sites must run the synthesis pass.** Initially only `type_resolve_body` ran it; codegen called `resolve_fn_body` directly, bypassing the synth → drops were queued as deps but never emitted as calls. Fix: call `insert_scope_end_drops` at both sites. The two synth invocations must use identical inputs (same registry, same `returns_void` predicate) or the dep queue and emitted body disagree.
- **`tcx.adt_destructor(adt_def.did())` is load-bearing in the predicate.** Without it, the synth tries to emit `Drop::drop` for `Option`/`Result`/`Stdout` which ICEs the mono collector ("failed to resolve instance for `<Option<i32> as Drop>::drop`"). The query returns `Some(_)` iff the ADT has an explicit `impl Drop`; `None` for types whose drop semantics flow through auto-generated DropGlue.
- **`codegen_extern_wrapper` must reuse existing extern declarations.** The body's call site (`__toylang_internal_main`'s emit-call) declares the symbol via `declare_external_fn` (which dedups). The cascade-drain's `codegen_extern_wrapper` formerly added the symbol unconditionally → LLVM `.1` disambig → bare-name call site unresolved. Fix: `get_function(symbol).unwrap_or_else(add_function)`.
- **`pub use core::ops::Drop;` must be gated on absence.** Source files with explicit `use std::ops::Drop` would otherwise collide.
- **Move semantics: non-void returns skip drops.** The synth pass treats `returns_void = matches!(return_ty, None | Some(Void))`. Functions returning a Vec/Widget/etc. don't get scope-end drops — the caller's binding owns it. Conservative; could be tightened later by tracking which locals the return expression moves.
- **Only the OUTERMOST block scope is walked today.** If toylang grows nested-block scopes (currently `while` has its own scope but we don't track per-block drops), the synth needs to recurse and insert drops at each scope's exit.
- **Sync-only drop**: precommits Sky to NOT supporting `AsyncDrop` (rustc's experimental `InstanceKind::AsyncDropGlue`). Not currently a constraint (Sky is sync-cleanup per §15.3) but a soft commitment.
- **B24 risk** (drop-glue shape stability): rustc's `build_drop_shim` shape is now in Sky's load-bearing dependency surface. See Decision 15.

**PRINCIPLE AUDIT (Phase E.d, post-refactor).** "Drop is not
architecturally special; it's just a function that the language
sometimes auto-calls" — honored at ~95%:
- Specialness lives in ONE site: `insert_scope_end_drops` (~30 lines)
  + `local_needs_scope_drop` predicate (~25 lines) +
  `synth_scope_drop_call` AST builder (~20 lines) + one
  `pub use Drop` line in stub_gen.
- After the synth pass, every downstream stage treats drop calls as
  ordinary trait static calls. No drop-specific path in: dep walker,
  mono cascade, per_instance_mir, codegen, symbol resolution, link.
- Remaining 5% leak: the predicate hardcodes the trait name "Drop"
  (and "drop" method name) at the registry lookup. Could be
  generalized to "synthesize trait-method calls based on a marker
  trait" but Drop is the only such trait Sky needs today.

**EMPIRICAL FINDINGS (Phase A characterization, 2026-06-23).**
| # | Finding |
|---|---------|
| A.1 | Parser rejected `&mut self` — Drop's receiver requires the mutable form. Fixed: `parse_impl_method` plumbs `is_self_mut: bool` to `ToyImplMethod`. |
| A.2 | Toylang requires `;` between statements (fixture-author syntax error, not a toylangc gap). |
| A.3 | `impl Drop` requires `export impl Drop` to surface to the stub rlib (doc lesson). |
| A.4 | stub_gen's `is_export` gate is correct — no change needed. |
| A.5 | `layout_of` intercepts Widget but `mir_shims/DropGlue intercepted` never fires. Override's `consumer_struct_name` lookup was never invoked. |
| A.6 | **Headline**: Even with `impl Drop for Widget` properly emitted in the stub source, `drop_in_place::<Widget>`'s body is a no-op (`sub; str; add; ret`) and `<Widget as Drop>::drop` is absent from the binary. The mir_shims override never fired; rustc's collector + the partition filter eliminated the body before any callable symbol could exist. The previous machinery was empirically broken, not just vestigial. |
| A.7 | `collect_consumer_trait_impl_instances` correctly captures `<Widget as Drop>::drop` from the partition once the toylang source declares `use std::ops::Drop` (`name=drop is_consumer_trait_impl_method=true`). |
| A.8 | `oracle::find_trait_impl_method_def_id` requires the trait to be `use`-imported in toylang source (or `pub use`-re-exported by stub_gen). For Drop, the user must import it OR stub_gen must re-export unconditionally. Phase E.d chose the latter (gated on absence). |
| A.9 | With `use std::ops::Drop` present, the cascade fully wires through: rlib defines `<Widget as Drop>::drop` and `__toylang_internal__Widget__Drop__drop`, the chain `drop_in_place → Drop::drop → __toylang_internal` is intact. |
| A.10 | But Sky's `__toylang_internal_main` didn't emit a drop_in_place / Drop::drop call at scope end. Drop fired for Vec elements (Vec's stdlib drop iterates) but not for bare Sky-struct locals. This is the gap Phase E.b/E.c/E.d filled: synthesize the call at scope end via AST rewrite. |

**DOC IMPACT (covered by this commit, 2026-06-23 doc-update pass).**
- §15.7 of `rust-interop-architecture.md`: rewritten around AST-rewrite synthesis + `Drop::drop` direct dispatch.
- §1.7: notes that drops fit through standard machinery.
- §5.4: providers list reflects mir_shims retirement.
- §F: new appendix entry F.18 — Phase E implementation lessons.

### Decision 2: Eliminate the `symbol_name` query override

**WHAT.** Remove the `symbol_name` override. Sky's `Providers::symbol_name` is unset; rustc's default mangler (v0) fires for all items. Sky's call-site emission computes target names via `default_symbol_name()(tcx, instance)` (already what it does internally).

**WHY.** The override is a live no-op confirmed by direct code audit. The facade's `lang_symbol_name` does shape classification (is_fn/is_accessor/is_trait_impl), builds a callback_name, and calls `consumer_symbol_for_callback_name`. The toylangc implementation (`compute_consumer_symbol`) IGNORES the callback_name parameter and returns rustc's default. The override does:
- Filter to consumer items from `__lang_stubs`.
- Classify shape (work).
- Build callback_name (work).
- Call consumer callback (work).
- Get rustc-default-mangled name back.
- Return it.

All the classification work the override does is ENTIRELY UNUSED at the symbol_name layer. The classification predicates (`is_consumer_fn`, etc.) are needed elsewhere (partition filter, layout) but those sites call the predicates directly.

The toylangc source even has a comment acknowledging the dormancy:
> "The `_name` parameter (callback-name shape from the facade) is now unused for routing but kept in the signature so the trait method stays stable until Tier 3 #12 retires the `consumer_symbol_for_callback_name` callback entirely."

This is Tier 3 #12.

**ALTERNATIVES CONSIDERED.**
- Keep as drift-observation sentinel (the "live no-op overrides as drift sentinels" framing). Rejected for symbol_name specifically because the integration test surface (Thread C inlining matrix, cross-crate link tests) catches mangler drift cleanly without the override. Elimination is justified; B25 (default symbol mangling stability) covers the drift-observation responsibility going forward.

**IMPLEMENTATION NOTES.**
- `rustc-lang-facade/src/queries/symbol_name.rs`: DELETE.
- `rustc-lang-facade/src/queries/mod.rs`: remove `symbol_name` from providers.
- `toylangc/src/toylang/callbacks_impl.rs::compute_consumer_symbol`: delete (orphaned).
- `consumer_symbol_for_callback_name` callback in the consumer trait: remove (Tier 3 #12 done).
- AUDIT: `is_consumer_fn` and `is_consumer_accessor_safe` for live callers post-symbol_name removal. They likely have no remaining users (the override was their primary caller). Delete if orphaned.
- KEEP: `is_consumer_trait_impl_method` — used by `collect_consumer_trait_impl_instances` (§8.9.5 cascade drain). Different concern.

**GOTCHAS.**
- Don't accidentally delete `is_consumer_trait_impl_method` along with the other matchers. It survives because §8.9.5's cascade drain uses it.
- After elimination, `default_symbol_name()(tcx, instance)` is the canonical name computation. Make sure Sky's emission still calls this (it does today, just not through the override).

**DOC IMPACT.**
- §5.4: drop `symbol_name` from override list.
- §6.2: remove symbol_name override description; note that single-symbol naming uses rustc's default mangling natively.
- §26.1 (SyMINCZ): keep the invariant ("symbol_name is a pure read, doesn't drive codegen") but frame as "rustc's default mechanism preserves this invariant" rather than "Sky's override enforces it."
- Query-providers count: -1 from current.

### Decision 3: `#[skyc::emit_consumer_body]` tool attribute as partition predicate

**WHAT.** Replace the current `is_consumer_codegen_target` predicate (a three-way union of name-based + structural matchers) with a two-gate attribute conjunction:

```rust
pub fn is_consumer_codegen_target<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    is_from_lang_stubs(tcx, def_id)
        && tcx.has_attr(def_id, sym::skyc_emit_consumer_body)
}
```

Skyc's stub_gen tags items at emission time. Items in Category B (Sky-emitted bodies, `unreachable!()` placeholders) get the tag; items in Category A (real Rust bodies like Drop impl bridges, Phase-6 wrappers) don't.

Tool-attribute form: `#![register_tool(skyc)]` at the stub source crate root + `#[skyc::emit_consumer_body]` on tagged items.

**WHY.** Post-mir_shims-elimination, the stub rlib has TWO body categories the old predicate can't distinguish:
- Category A (real bodies, rustc compiles, filter LEAVES ALONE): Drop impl bridges, Phase-6 wrappers, type/trait declarations, the marker item.
- Category B (unreachable placeholders, filter REMOVES, Sky emits via fill_extra_modules): exported Sky functions, Sky drop functions, Sky trait-impl methods on Sky types, Sky accessor methods.

The old predicate matched by crate-membership + name/shape heuristics, which:
- Couldn't distinguish bridges from Sky drop fns (both have similar shapes from the predicate's view).
- Required state-dependent reads (`is_consumer_fn` reads Sky's universe).
- Required structural ADT walks per call (expensive).

The attribute-based mechanism is:
- Explicit (human-readable at source level).
- Auditable (`grep` for tagged items).
- Stateless (no universe dependency).
- Cheap (two query lookups, both cached or fast).
- Dep-tracked (attributes flow through rustc's standard dep graph).
- Robust to future emission shapes (doesn't depend on naming conventions).

**ALTERNATIVES CONSIDERED.**
- Body-shape detection (match on `unreachable!()` body): rejected, fragile to nightly MIR-shape changes and to future placeholder evolution.
- Naming convention (function starts with `__sky_*`): rejected, brittle and doesn't extend to trait impls or types.
- Per-crate flag (whole crate's bodies are Sky-emitted): rejected, too coarse — bridges and Sky drop fns coexist in the same crate.
- `#[unsafe(rustc_intrinsic)]`-style decoration: wrong semantic — those are for compiler intrinsics.
- Keep name-based approach with expanded matchers: rejected, adds matchers indefinitely as new emission categories appear.

**IMPLEMENTATION NOTES.**
- Stub source emits `#![register_tool(skyc)]` at crate root (already needs nightly features; cost is free).
- Skyc's stub_gen tags items at emission per the two-category split:
  - Tag: exported Sky functions, Sky drop fns (`__sky_drop_X<T>`), Sky trait-impl methods, Sky accessor methods.
  - DON'T tag: Drop impl bridges, Phase-6 wrappers, type/trait declarations, the marker.
- `rustc-lang-facade/src/lib.rs::is_consumer_codegen_target`: rewrite to two-gate conjunction.
- AUDIT and delete: the three-way matchers (`is_consumer_fn`, `is_consumer_accessor_safe`) become orphaned after Decision 2 + this. Keep `is_consumer_trait_impl_method` (cascade drain still uses it).
- CI fence: walk stub source for tagged items, walk Sky's emission list, verify 1:1 mapping for items the cascade reaches.

**GOTCHAS.**
- The 1:1 invariant: every tagged item ↔ exactly one Sky emission per mono Instance the cascade reaches. Skyc-side bug = link error (duplicate or undefined symbol). Loud failure mode, not silent.
- Don't tag items rustc compiles (bridges, Phase-6 wrappers). The filter would remove them; rustc would emit no `.o` for them; Sky doesn't have a body for them; link error.
- Don't FAIL to tag items Sky emits. Same failure direction.
- Same predicate must be used by both the partition filter AND per_instance_mir override (they MUST agree on what's consumer).

**DOC IMPACT.**
- §5.3: rewrite partition filter section with the explicit predicate definition + the 1:1 invariant + the Category A/B split.
- §6: stub_gen section needs the tagging rules.
- §F.14.1 / §F.17: update the partition filter design history with the post-mir_shims-elimination shape.

### Decision 4: cdylib for Sky's backend (Phase 1)

**WHAT.** Sky's backend ships as a separately-loaded cdylib (`libsky_backend.so` / `.dylib` / `.dll`) instead of being statically linked into the forked rustc binary. The Sky toolchain bundle ships rustc-fork + libsky_backend.so as paired binaries; rustup-style atomic installation enforces pairing. Runtime version handshake at backend load detects mismatches.

**WHY.** Post-elimination of mir_shims and symbol_name (Decisions 1+2), Sky's rustc-touch surface shrunk from ~8 entry points to ~6: `per_instance_mir`, `layout_of`, `cross_crate_inlinable` (both flavors), `collect_and_partition_mono_items`, and `fill_extra_modules` hook. Smaller surface tilts the cdylib calculus favorable.

Empirical case (per reviewer's Q23 + cdylib pushback):
- Dev velocity: rebuilding rustc-fork is 5-15 minutes for any backend change. Rebuilding a cdylib is 30s-2min. Order-of-magnitude faster iteration on backend hacking.
- Cranelift's cdylib operation validates the model. Sky's touch surface is similar in shape (TyCtxt-typed entry points; standard `CodegenBackend` trait).
- Reversible: if cdylib bites operationally in production, switch to static link in a subsequent release. End users see slightly larger rustc binary; engineering surface is unchanged.

User priority context: user prioritizes runtime perf over dev perf (Q21 discussion), BUT dev velocity for Sky's IMPLEMENTERS (which is what cdylib helps) is "a reasonable factor" (Q23) → upgraded to "sufficient factor for Phase 1" given the eliminations shrunk the FFI surface.

**ALTERNATIVES CONSIDERED.**
- Static link (current model from §4.1): rejected for Phase 1 due to dev-velocity cost. May revisit if cdylib operational issues surface.
- Hybrid (forked rustc + thin shim statically linked + Sky frontend as cdylib): rejected, adds complexity without clear benefit.

**IMPLEMENTATION NOTES.**
- Sky toolchain bundle structure:
  - `bin/rustc` (forked rustc binary with fork patches, codegen backend statically registered via `-Zcodegen-backend=sky` default).
  - `lib/libsky_backend.{so,dylib,dll}` (Sky's frontend + Sky's codegen backend; provides the `Providers` setup and `fill_extra_modules` hook).
  - `bin/skyc` (orchestrator; unchanged).
  - `bin/cargo` (vanilla cargo from upstream nightly).
- Rustc-fork's default codegen backend is set to load `libsky_backend.so` via the standard `CodegenBackend` plugin mechanism cranelift uses.
- Build-time version pin: rustc-fork and libsky_backend.so are built from the same source tree at the same commit. Toolchain bundle ships the paired binaries together; version pin is a property of the bundle.
- Runtime version handshake: when `libsky_backend.so` loads, it queries rustc-fork's version string and verifies the pairing. Mismatch → clear error message ("Sky backend version X.Y.Z doesn't match rustc-fork version A.B.C; reinstall the Sky toolchain") and exit cleanly.
- Inkwell/LLVM version match: the bundle ships LLVM shared libs once; both rustc-fork and libsky_backend.so dynamically link them. No runtime LLVM version mismatch possible because the bundle's content enforces it.
- See Decision 5 for the FFI shape.

**GOTCHAS.**
- **Don't use `&mut dyn Trait` across the cdylib boundary** for any of the providers or hooks. Use `#[repr(C)]` function-pointer struct (Decision 5).
- The marker check / `is_sky_active(tcx)` discipline must work the same across the cdylib boundary as statically linked. Sky's `init`/`provide` methods short-circuit on marker absence for byte-identical pass-through (§4.4) regardless of link mode.
- B22/B23 risks: version-pairing drift + FFI ABI drift in TyCtxt-typed arguments. See Decision 15.
- If cdylib bites operationally, the reversion path is changing the build system's link mode. The OVERRIDES themselves (what providers do, what the hook does) are identical either way.

**DOC IMPACT.**
- §4.1: restructure — Sky's compiler is two binaries: rustc-fork + libsky_backend cdylib.
- §4.2: third binary in toolchain (`lib/libsky_backend.*`).
- §4.3: rustup toolchain layout updated.
- §4.4: pass-through invariant requires marker-check in cdylib's init/provide methods.
- §25: add B22 + B23 risks.
- §29.6 (open question — cdylib as future direction): close, no longer open.

### Decision 5: `#[repr(C)]` function-pointer struct for cdylib FFI (Patch 4 rev 3)

**WHAT.** Replace patch 4's current `&mut dyn ExtraModuleAllocator<ModuleLlvm>` callback with a `#[repr(C)]` function-pointer struct:

```rust
#[repr(C)]
pub struct ExtraModuleAllocator<M> {
    pub state: *mut c_void,
    pub allocate: unsafe extern "C" fn(
        state: *mut c_void,
        name_ptr: *const u8,
        name_len: usize,
    ) -> *mut M,
}
```

`repr(C)` + `extern "C" fn` are stable-ABI primitives. Pass cleanly across the cdylib FFI boundary without rustc-internal type-layout dependencies. ~10 more lines of boilerplate at the call sites (state pointer + fn pointer); failure mode goes from "subtle vtable-layout mismatch" to "if it links, it works."

**WHY.** The reviewer's Q2 sharp catch: cranelift's `CodegenBackend` trait dispatch works because rustc owns the vtable, cdylib registers itself (callee-owns-vtable direction). Sky's allocator is the INVERSE — rustc constructs the trait object, cdylib receives and calls methods through it. The vtable was emitted by rustc; the cdylib has to match the rustc-side layout exactly.

Within a single rustc invocation, vtable layouts are stable. Across the FFI to a separately-compiled cdylib, they're stable only if both sides were compiled against an identical `rustc_codegen_ssa` (same source, same rustflags, same feature flags). For Sky's distribution model this is satisfied IN PRACTICE (atomic bundle pairing), but the constraint is INVISIBLE at the type level — a toolchain mismatch produces runtime vtable interpretation error → segfault.

Reviewer explicitly noted: "rustc_driver::Callbacks trait is dyn-passed, but rustc_driver is statically linked into consumers, not cdylib-passed. Cranelift's loaded-cdylib surface deliberately avoids `&mut dyn`. I'd treat the absence of precedent as evidence rather than as opportunity."

**ALTERNATIVES CONSIDERED.**
- Keep `&mut dyn Trait` (current patch 4 rev 2 shape). Rejected: subtle FFI failure mode; no precedent in the rustc-private ecosystem for cdylib-passed trait objects.
- Concrete struct with virtual dispatch internally: variant of the function-pointer struct; equivalent properties. Use whichever shape is more ergonomic at the call sites.

**IMPLEMENTATION NOTES.**
- Modify patch 4 in `~/rust/compiler/rustc_codegen_ssa/src/traits/backend.rs`:
  - Replace `&mut dyn ExtraModuleAllocator<M>` with the function-pointer struct.
  - Update the `fill_extra_modules` hook signature to take the struct by value (cheap copy of two pointers).
- `rustc_codegen_ssa/src/base.rs::codegen_crate`: construct the struct with state = pointer to the modules-vec, allocate = a stable extern "C" fn that mints rustc-owned modules.
- `rustc_codegen_llvm/src/lib.rs::fill_extra_modules`: read the hook (function pointer), invoke through the struct.
- Sky's consumer side (`rustc-lang-facade/src/extra_modules_hook.rs`): the hook receives the struct, calls `(struct.allocate)(struct.state, name_ptr, name_len)` to get a `*mut ModuleLlvm`, wraps in suppressed-Drop Inkwell handles, emits IR.
- Wrap/unwrap boilerplate: ~10 lines on each side of the FFI. Pay the cost once; the failure mode improvement is worth it.

**GOTCHAS.**
- The struct's pointer fields are raw; provenance discipline applies. The `state` pointer must be valid for the duration of the `fill_extra_modules` call.
- Mind the lifetime story: rustc owns the state (a Vec<ModuleCodegen<ModuleLlvm>>); Sky's hook receives a pointer to it; the allocate callback (on rustc's side) appends to the Vec and returns a `*mut ModuleLlvm` borrowing into the Vec's heap. Borrow scope is the duration of the hook call; rustc must not relocate the Vec mid-call.
- If you need to extend the struct later (more callbacks), prefer adding a new struct variant or version-tagged struct rather than mutating the existing one. Stable ABI requires careful evolution.

**DOC IMPACT.**
- §3.2 patch 4: update with the function-pointer struct shape.
- §B.4 (the patch shape appendix): rewrite for the new shape.
- §C.4 (shipping patch 4 shape: rustc-owns-lends): update; the OWNERSHIP model is the same, just the callback transport differs.
- §F.15 (patch 4 design history): add the rev 3 rationale (reviewer's cdylib FFI shape pushback).

### Decision 6: u128 typeids with universe-level collision detection

**WHAT.** Sky's content-addressed type identities are u128 BLAKE3-truncated hashes. Sky's universe table maintains a `HashMap<u128, SkyTypeInfo>` and detects collisions explicitly: on every insertion, compute the BLAKE3-truncated hash, look up; if mapped to identical content → fine; if mapped to DIFFERENT content → build fails with explicit error.

```rust
pub struct SkyOpaqueType<const T: u128>(PhantomData<()>);
```

**WHY.** Round-1 Q10 raised collision risk for u64 typeids (birthday at ~2^32 = 4B types). Round-2 we agreed on 128 or 256 bits. Round-3 reviewer Q10 follow-up: u128 is enough (collision-free up to ~2^64 types in a single program), readable in error messages and `tcx.def_path_debug_str` output. 256-bit (`[u8; 32]`) is overkill; u128 is the better default.

User raised concern: "any chance of birthday collision?" The answer is the universe-level collision detection: practically zero collision risk, hard error if it ever occurs, never silent corruption.

**ALTERNATIVES CONSIDERED.**
- u64 (current). Rejected: birthday collision at 4B types is small but not zero; failure mode (silent type confusion) is catastrophic.
- 256-bit `[u8; 32]`. Rejected: overkill; harder to read in error messages.
- Monotonic IDs. Rejected: requires central registry; breaks distributed compilation.
- Full content as ID (variable-size). Rejected: doesn't fit const generic constraints.

**IMPLEMENTATION NOTES.**
- `rustc-lang-facade/src/lib.rs` (or wherever `SkyTypeId` is defined): change typeid type to `u128`.
- Toylangc / Sky stdlib's `SkyOpaqueType<const T: u128>`: update wrapper declaration.
- Universe table: change keys to u128. Add collision check on insertion:
  ```rust
  fn insert_type(&mut self, content_hash: u128, info: SkyTypeInfo) -> Result<(), CollisionError> {
      match self.types.get(&content_hash) {
          Some(existing) if existing.content_signature() != info.content_signature() => {
              return Err(CollisionError { content_hash, existing: existing.clone(), incoming: info });
          }
          _ => { self.types.insert(content_hash, info); Ok(()) }
      }
  }
  ```
- Hashing: BLAKE3 of the canonical content (source path for source-defined types; canonical recipe for comptime-produced types per §10.8). Truncate to u128 (first 16 bytes).
- Error message on collision: include both types' source paths or recipes so the user can identify what conflicted. Ask the user to file a Sky bug since the probability is astronomically low.

**GOTCHAS.**
- Collision detection runs on insertion, not on lookup. Lookups assume the table is correct.
- The "content signature" for comparison is the canonical content (source path or recipe), not the SkyTypeInfo's full data (which may include compile-derived layout metadata that varies legitimately across compiles).
- Don't share the u128 namespace between type typeids and value content hashes (Decision 7). They're in different DefId namespaces in rustc's view, so they can't collide rustc-side, but Sky's universe table should tag entries by kind anyway to keep the failure modes clean.

**DOC IMPACT.**
- §10.6: wrapper definition changes to `u128`.
- §10.8: typeid format spec — BLAKE3 truncated to u128 with universe-level collision detection.
- §13.8: same.

### Decision 7: Content-hash const args (retire slab-pointer-as-u64)

**WHAT.** Sky's comptime values that flow to rustc as const generic arguments are surfaced as `ConstKind::Value(content_hash_bytes)` — content-addressed u128 hashes (same scheme as Decision 6 typeids, applied to value content). The slab is purely Sky-internal; slab pointers never enter rustc's Instance args.

**WHY.** Round-3 reviewer Q16 catch (the most important architectural correction from round 3): if Sky's per_instance_mir bodies use slab-pointer-as-u64 as the const-arg surface, two Sky source sites that produce content-equal values at different slab offsets generate two distinct rustc Instances with two distinct mono items. Symbol naming via content-hash would then produce TWO MonoItems with the SAME symbol name → comdat dedup (best case) or linker error / non-deterministic pick (worst case).

The clean fix: do the content-hash dedup at the INSTANCE level, not the symbol_name level. Sky's frontend, when binding a comptime value as a generic arg, computes the content hash of the (frozen) snapshot and synthesizes `ConstKind::Value(hash)`. Rustc's collector dedups naturally on `(DefId, args)`. Symbol naming via rustc's default mangler produces the right names. Slab stays Sky-internal as the runtime substrate for mutable comptime evaluation.

This dovetails with Decisions 6 (u128 typeids) and 1 (mir_shims elimination): unified u128 hashing scheme; per_instance_mir handles content-hash-keyed bodies the same way it handles any other Instance.

**ALTERNATIVES CONSIDERED.**
- Slab-pointer-as-u64 + content-addressed symbol naming (the round-2 plan). Rejected per the reviewer's Q16 catch.
- Slab-pointer-as-u64 + Instance-level dedup via per_instance_mir canonicalization. Possible but requires Sky to canonicalize const args inside per_instance_mir, which is invasive. The content-hash-as-const-arg shape achieves the same result naturally.
- Variable-size content as const arg. Rejected: doesn't fit existing const generic constraints.

**IMPLEMENTATION NOTES.**
- `toylangc/src/toylang/frontend` (or wherever comptime-arg binding lives): when binding a comptime value to a generic arg, compute BLAKE3 of the snapshot content, truncate to u128, synthesize `ConstKind::Value(u128_bytes)` for the Instance args.
- Stub source signature for generic Sky fns with comptime args:
  ```rust
  pub fn zork<const T: u128>(...) { unreachable!() }
  ```
  T is the content hash, not the slab offset.
- per_instance_mir: looks up the value in Sky's universe using the content hash as the key. Sky-side value lookup table (`HashMap<u128, SkyValue>`) maps hash → snapshot.
- Slab stays as Sky's runtime substrate. Slab pointers never escape Sky's frontend.
- Snapshot-at-capture semantics (from round-2 Q16 discussion): when a comptime value is bound as a generic arg, take a snapshot. Subsequent mutations to the original source variable don't propagate to the snapshot. The hash is computed from the frozen snapshot.

**GOTCHAS.**
- Don't allow identity observation (`ptr_eq`) on generic-arg references in Sky source (Approach A from round-2 Q16 discussion). Under content-hash naming, two content-equal snapshots at different slab offsets get the SAME hash → SAME symbol → SAME Instance. If user code could observe identity in the generic body, content-equal-but-identity-distinct values would conflate. Sky's typechecker rejects `ptr_eq` calls on generic-arg refs.
- The hash table for value lookup is per-compile-invocation. Cross-invocation, two compiles producing content-equal snapshots independently compute the same hash → cross-crate symbol matching works automatically.
- Symbol_name override removal (Decision 2) means Sky doesn't need to canonicalize symbols for comptime-arg-parameterized Instances. rustc's default mangler handles them correctly because the const args are already content-hashes.

**DOC IMPACT.**
- §13.3: rewrite — retire slab-pointer-as-u64; describe content-hash const args.
- §13.4: clarify slab is purely Sky-internal substrate for active mutable comptime evaluation.
- §13.7: SkyOpaqueType wrapper uses content-hash const args (same as typeids).
- §13.8: unified content-hash mechanism for types and values.
- §13.9: keep "no new fork surface" framing as primary reason; content-addressing is a bonus property (per reviewer's round-3 Q9 correction).

### Decision 8: Per-view ref types `SkyRef<T, V>` for Send/'static honesty

**WHAT.** Sky's stub rlib emits `SkyRef<T, V>` (parametric over view marker) for Sky references at the Rust boundary. View markers `Frozen` and `Mutable` (closed set, see Decision 9). Send/Sync/'static impls vary by view:

```rust
pub struct Frozen;
pub struct Mutable;

pub struct SkyRef<T, V> {
    ptr: *const T,
    _v: PhantomData<V>,
}

unsafe impl<T: Sync> Send for SkyRef<T, Frozen> {}
// no Send impl for SkyRef<T, Mutable>

// Methods polymorphic over view kind:
impl<V> SkyRef<MyType, V> {
    pub fn velocity(&self) -> f64 { unreachable!() }
}

// Frozen-view-only methods:
impl SkyRef<MyType, Frozen> {
    pub fn concurrent_read(&self) -> Data { unreachable!() }
}
```

Sky's frontend picks the View at each `&` site based on the actual group's frozen/mutable status (which Sky's typechecker tracks). One Rust function/impl per Sky function/impl (parametric over view kind); cardinality doesn't double.

**WHY.** Round-1 Q13 surfaced the silent-bug surface of the current "global `unsafe impl Send` for every Sky type" (§12.1). Rust source could pass a non-sendable Sky value to `tokio::spawn`; rustc accepts (because of the global lie); runtime data race.

User clarified the actual Sky model: sendability is group-view-based, not per-type. `&g'Spaceship` is Send when group g is frozen (immutable). Round-1 Option C: emit two Rust types per ref (SkyFrozenRef / SkyMutableRef). Round-2 refinement to Option C': parametric `SkyRef<T, V>` to avoid stub rlib doubling. Single rustc type, view-marker parameter, conditional Send impl.

This is the structurally honest design that matches what Sky's typechecker knows. Eliminates the Case 3b silent-bug surface from Q13.

**ALTERNATIVES CONSIDERED.**
- Global lie (current). Rejected: silent data races.
- Refuse to project any Sky refs to Rust. Rejected: too restrictive.
- Two distinct Rust types per ref (Option C original). Rejected: stub rlib doubling.
- Per-type honest Send for owned values + per-view for refs (Option C' as written). Adopted.

**IMPLEMENTATION NOTES.**
- Sky stdlib defines `Frozen`, `Mutable`, `SkyRef<T, V>` (and the Send/Sync impls).
- Sky's stub_gen, when emitting a function signature that takes `&g'Spaceship`:
  - Determines the group's view at the call site (Sky's typechecker knows this).
  - Emits the parameter as `SkyRef<Spaceship, FrozenView>` or `SkyRef<Spaceship, MutableView>` accordingly.
  - For Sky source that's generic over the view (e.g., `fn process<G>(s: &G Spaceship)`), the Rust signature is parametric: `pub fn process<V>(s: SkyRef<Spaceship, V>)`.
- Sky's stub_gen emits methods as one parametric impl per Sky type (`impl<V> SkyRef<T, V> { ... }`). Specialized impls only when behavior differs per view.
- Sky's frontend's typechecker, at every `&` site, determines which view to project (frozen if Sky has proven the group is frozen at that scope; mutable otherwise).
- For owned values (non-refs): per-type honest Send computation. Sky's typechecker analyzes the type's structure; emits `unsafe impl Send` only when structurally sendable. Opt-in lie via `#[unsafe_send]` Sky annotation for nuanced cases (rare).

**GOTCHAS.**
- The View parameter is a NEW dimension that Sky-source generics didn't have before. Sky's typechecker has to track it everywhere it tracks group views.
- Closed V set (Decision 9) — don't let users define new V kinds; coherence gets fragmented across crates.
- Tokio interop fallout: Sky futures capturing group-borrowed state can't `tokio::spawn` directly under Option C'. They need conversion via a Sky-provided bridge crate (deferred; see Open Questions).
- Owned Sky values: per-type honest Send by default, but `#[unsafe_send]` annotation lets users opt into the lie for nuanced cases. Most Sky types should get honest Send (structural analysis works).

**DOC IMPACT.**
- §12.1: rewrite around per-view ref types + per-type honest Send for owned.
- §12.2: rewrite — the 'static framing was wrong (per round-1 Q14). Same Option C extension: per-lifetime stubs for group-parameterized types.
- §17: Sky-native async primary; tokio via bridge crate.
- §11.2/§11.3: integrate with view-marker projection.

### Decision 9: Closed V set in Sky stdlib

**WHAT.** The set of view markers in Sky stdlib is closed: `Frozen`, `Mutable` (possibly `Exclusive` for &own-style if needed). Users cannot define new view kinds. Custom borrow semantics are achieved by wrapping (newtype + delegation), not by introducing new V.

**WHY.** Round-3 reviewer Q3 follow-up: with open V, coherence fragments across crates. Sky stdlib provides `impl<T: Send> Send for SkyRef<T, Frozen>`; downstream crate could provide `impl<T> Send for SkyRef<T, MyCustomView>` for some custom view. Coherence is satisfied (different V), but the user model "Sky decides what's Send" fragments. Each new V is one more dimension to audit globally.

Closed V makes coherence globally analyzable. Aligns with Sky's broader "wrap, don't fork" pattern (§6.6's first idiom — newtype with cheap delegation).

**ALTERNATIVES CONSIDERED.**
- Open V (user-extensible). Rejected per reviewer's catch.
- Plugin-style V registration (Sky tooling enforces global registry). Possible but adds infrastructure; not justified absent demand.

**IMPLEMENTATION NOTES.**
- Sky stdlib defines `Frozen`, `Mutable` (and possibly `Exclusive`) as marker types.
- Sky's typechecker rejects any attempt to define a new view marker outside stdlib.
- The Send/Sync impls for `SkyRef<T, V>` are pinned to the closed set.

**GOTCHAS.**
- If a future Sky feature genuinely needs a new V (e.g., RC-shared view, transitioning view), it gets added to stdlib by stdlib authors — not by user crates.
- The exact set (Frozen + Mutable, or +Exclusive, or +others) needs concrete design in Phase 5 (groups + linear types). The handoff commits to "closed"; the exact contents are open.

**DOC IMPACT.**
- §12.1 + §12.2 (per-view stubs): note V is closed-set.
- New subsection or §6.6 update: document the closed-V principle and how users get custom borrow semantics (via wrapping).

### Decision 10: Async typestate pattern (one rustc type, source-level witnesses)

**WHAT.** Each Sky async fn produces ONE rustc-visible type whose storage is sized to hold both NotStarted and Running phase state. Sky source operates over this storage via two typestate witnesses (`SkyNotStarted_foo`, `SkyRunning_foo`) that share the underlying storage but expose different methods and have different safety properties enforced by Sky's typechecker. `.start()` is a typestate transition at Sky's source level; doesn't change rustc-level identity. The IntoFuture impl on the rustc-visible type handles polling in either phase via internal discriminant.

Pin/Unpin properties are declared on the rustc-level type based on what the underlying state machine needs (migratory = Unpin; default = !Unpin). The typestate witness doesn't factor into Pin — Pin's contract is "no moves after pinning"; the pinning point is `.await` regardless of typestate.

NotStarted phase IS movable for !Unpin types (Pin's contract is "no moves after pinning"; before pinning, !Unpin values move freely).

**WHY.** Round-1 §14.10 framing presented this as "two distinct rustc-level types" which the reviewer pushed back on in round-2 Q19. The IntoFuture hybrid (which makes Rust callers see one type for `.await` ergonomics) implies the rustc-visible type IS singular; the "two-type split" was Sky-source-level discipline, not rustc-level structural.

Round-3 reviewer confirmed: it's a typestate pattern, not two physical types.

User had reservation about NotStarted movability under !Unpin declaration; cleared up by re-explaining Pin's actual semantics (it's "no moves AFTER pinning," not "no moves ever").

**ALTERNATIVES CONSIDERED.**
- Two distinct rustc-level types. Rejected per reviewer's Q19.
- One type with internal flag and runtime state checks. Rejected — typestate at source level enforces safety properties at compile time.

**IMPLEMENTATION NOTES.**
- Sky's stub_gen emits ONE struct per Sky async fn: `pub struct __sky_async_foo<'a>(SkyOpaqueType<HASH>, PhantomData<&'a ()>);`
- IntoFuture impl on this type handles polling in NotStarted or Running phase via internal discriminant in Sky's universe (or in the storage; choose based on implementation).
- Sky source typechecker tracks the typestate witness — NotStarted vs Running — for source-level safety properties:
  - Can't `.start()` twice (only NotStarted typestate has `.start()` method).
  - Can't access captures after start (only NotStarted typestate exposes capture accessors).
  - Can't drop a Running typestate of a default async fn (linearity rule).
  - Migratory/cancellable propagation rules per typestate.
- Pin/Unpin declarations on the rustc-level type:
  - Migratory: `impl Unpin for __sky_async_foo<'a> {}` (no cross-await self-refs ever).
  - Default: NO Unpin impl (`!Unpin`; may have cross-await self-refs after first poll).

**GOTCHAS.**
- NotStarted-phase movability for !Unpin types: works correctly. Pin's contract is "no moves after pinning"; the pinning point is `.await`. Before `.await`, the user can `move s` freely.
- SkyRunning typestate can't be passed to Rust APIs (it's Sky-source-only). If a user calls `.start()` explicitly and then wants to pass to Rust, they need to either keep it Sky-side or not pre-start.
- Drop for default async in Running phase: Sky's drop function panics (per Decision 11). The Sky source typechecker should also reject Sky-source-level drops, but the rustc-emitted drop_in_place path can still fire (e.g., when Rust source holds the future); the drop function is the runtime safety net.

**DOC IMPACT.**
- §14.10: substantial rewrite — typestate pattern, not two physical types.
- §14.5: clarify migratory's marker bundle on the rustc-level type.
- §14.7: cancellable wrapper's Drop semantics in the typestate model.
- §15.7: phase-dependent drop semantics for state machines (Sky's drop function dispatches on phase).

### Decision 11: Strict_linear default with `#[rust_droppable]` opt-out

**WHAT.** Linear Sky types default to strict — runtime panic when Rust drops them (existing §15.7 design). User can opt-out via a `#[rust_droppable]` annotation; opt-out types drop normally without panic. Q15 Option 4 (compile-time error via mir_shims) is DEFERRED to v2; v1 uses runtime panic.

**WHY.** User explicitly said in round-3: "sky might make their types *by default* linear, so forbidding using them directly in Vec<T> is a lot more friction than i want." Linear-by-default means the compile-time error path (Q15 Option 4) would block standard Rust container usage for most Sky types — too restrictive.

Two-level distinction the user accepted:
- Default (strict_linear): runtime panic if Rust drops. Most Sky types.
- Opt-out (`#[rust_droppable]`): drop is fine, runs Sky's normal cleanup. Types where drop is safe and ergonomic.

The compile-time error path (Option 4) is deferred to v2 as an explicit opt-in `#[ultra_strict]` (or similar) for types where runtime panic is too late.

**ALTERNATIVES CONSIDERED.**
- Compile-time error default (Q15 Option 4). Rejected — too restrictive given linear-by-default Sky types.
- Refuse to export linear types (round-3 reviewer Q15 option). Rejected — too restrictive.
- SkyOwn<T> wrapper as opt-in escape (round-3 reviewer suggestion). Deferred — useful for v2 if Option 4 lands as ultra-strict tier.

**IMPLEMENTATION NOTES.**
- Sky's typechecker tracks linearity per type.
- Sky's stub_gen, for linear types:
  - Default (strict_linear): emit Drop impl bridge calling Sky drop function that panics.
  - With `#[rust_droppable]`: emit Drop impl bridge calling Sky drop function that does normal cleanup.
- The Sky drop function body (via per_instance_mir):
  - For strict_linear: `sky_runtime_panic("Sky linear type X dropped from Rust source")` + `abort()`. No actual cleanup — process is dying.
  - For rust_droppable: normal Sky-side cleanup logic.

**GOTCHAS.**
- Async future Drop is phase-dependent (§14.4, §14.5). The Sky drop function for state machines reads the discriminant; for default-async in Running phase, panics; for migratory, normal cleanup; for NotStarted phase, normal cleanup.
- Cancellable wrapper Drop (§14.7): reads Ready/Pending; for Pending, invokes the user's cleanup handler then drops the inner future.
- The `#[rust_droppable]` annotation is Sky source-level; Sky's stub_gen translates it to the Drop impl body choice.

**DOC IMPACT.**
- §15.7: keep as runtime panic for default; add `#[rust_droppable]` opt-out spec.
- §15: mention Q15 Option 4 (compile-time error tier) as deferred v2 work.

### Decision 12: Narrowed `#[may_dangle]` policy (syntactic rule for synthesized types)

**WHAT.** `#[may_dangle] T` is emitted for a generic Sky type's Drop iff T appears in the lifted type's storage EXCLUSIVELY behind pointer indirection (`&T`, `&mut T`, raw pointers). By-value storage of T (or storage of types containing T by-value) DEFAULTS to STRICT (no may_dangle).

This rule is SYNTACTIC — Sky's frontend reads it directly off the capture-analysis output. No recursive structural-drop analysis. No "is the drop structural" proof obligation.

For SYNTHESIZED types (closures, async state machines): Sky's frontend auto-applies the syntactic rule.

For USER-DEFINED generic Sky types: default strict. User opts in via explicit Sky source annotation if their Drop is structural over T.

For STDLIB CONTAINERS (SkyVec, SkyMap, SkyChannel, etc.): annotated by stdlib authors who guarantee structural drops.

**WHY.** Round-3 reviewer's Q1 follow-up rejected my proposed "structural-over-T captures" rule because:
- Recursive structural-drop analysis is a non-trivial property to maintain as Sky's type system evolves.
- Every new feature (linear types, comptime-produced types, traits with default drop_in_place) has to re-prove "is structural drop preserved through this construct?"
- Skip a case → silently emit may_dangle for a Drop that actually reads T → silent unsoundness.

The syntactic rule (T appears only behind pointer indirection) is what rustc itself uses internally for some of its dropck heuristics. Captures the common case (closures over borrowed data, async state machines holding refs across await points) which is what's needed ergonomically. By-value captures default strict; revisit if a real workload needs them.

User's earlier reasoning ("dangling things is baked into Sky's model anyway via group borrowing's invalidation rules") conflated two different concepts. Sky's group system manages when memory becomes dangling at the region level; `#[may_dangle]` is a per-Drop-impl promise about not dereferencing dangling pointers. Different concepts.

**ALTERNATIVES CONSIDERED.**
- Emit may_dangle by default for generic Sky containers (my round-3 initial position). Rejected — too permissive; soundness obligation passed to skyc's lifting analysis.
- Always emit strict; user opts in for every case. Rejected — closure/async ergonomics suffer (which is what may_dangle is for).
- "Drop is structural over T" recursive analysis. Rejected — fragile to type-system evolution.

**IMPLEMENTATION NOTES.**
- Sky's stub_gen, when emitting Drop for a generic Sky type:
  - Read the type's storage shape from Sky's typechecker.
  - For each type parameter T, check: does T appear in storage exclusively behind `&`/`&mut`/raw pointer indirection?
  - If yes → `unsafe impl<#[may_dangle] T> Drop`.
  - If no → `unsafe impl<T> Drop`.
- For synthesized types (closures, async state machines): Sky's frontend has full info; auto-applies.
- For user-defined types: default strict; user opts in via `#[sky_may_dangle(T)]` (or similar) source annotation. Sky's typechecker validates the opt-in is justified (e.g., warns or rejects if the user's Drop body accesses T-typed values).
- For stdlib containers: stdlib authors explicitly annotate; no auto-inference.

**GOTCHAS.**
- The syntactic rule is conservative. Some closures with by-value captures whose Drop IS structural over T (rare) won't get may_dangle. User has to restructure or accept the borrowck friction.
- Don't try to auto-infer for user-defined Drop bodies; that's where the analysis fragility lives.
- This is one of the most important corrections from the review exchange — getting may_dangle wrong is a soundness bug, not just a UX issue.

**DOC IMPACT.**
- §15 (new chapter or rewrite): full `#[may_dangle]` policy subsection.
- §10: stub-type contract notes the syntactic rule for synthesized types.

### Decision 13: Sky-side recursion limit alignment

**WHAT.** `walk_and_stash_internal_callees` (and any other Sky-side walker that enumerates dep graphs) gets memoization + depth counter aligned with `tcx.recursion_limit()`. Walks that exceed the limit emit a compile-time error attributed to a user-source entry point; don't stack-overflow Sky's walker.

**WHY.** Round-3 reviewer Q3 follow-up: rustc's recursion_limit applies to rustc's mono collector. Sky has its OWN walks (Sky-internal callee enumeration) that don't go through rustc's collector. Without explicit handling, pathologically deep Sky-internal recursion stack-overflows Sky's walker or wedges in a cycle, before rustc's standard recursion-limit error can fire.

**ALTERNATIVES CONSIDERED.**
- No protection (current). Rejected — pathological Sky code crashes Sky's walker.
- Sky-specific limit independent of rustc. Rejected — surprising for users who configure recursion_limit and expect it to govern any walker.
- Iterative-only (no recursion in Sky's walker). Possible refinement; recommended where feasible but recursion + depth counter is simpler to implement first.

**IMPLEMENTATION NOTES.**
- Sky's `walk_and_stash_internal_callees` (location in toylangc; equivalent in Sky's frontend):
  - Add per-walk `visited: FxHashMap<(DefId, GenericArgsRef), DepListHandle>` initialized at walk entry.
  - Add per-walk `depth: usize` counter, checked against `tcx.recursion_limit().value` before each recursive descent.
  - On exceed: emit error with walk's entry-point span (find by walking back through the in-flight stack to outermost user-source DefId); return error sentinel.
  - Iterative traversal where feasible (worklist-based) to limit OS stack growth.
- Memoization is also cycle detection: memo hit during in-flight walk → return placeholder.

**GOTCHAS.**
- Diagnostic attribution: don't emit at a synthetic "Sky walker at line ???" span. Walk back to the outermost user-source frame and emit there.
- Memo + depth counter together are required: memo handles cycles (every cycle revisits a key), depth counter handles non-cyclic-but-deep cases.

**DOC IMPACT.**
- §19.5: expand into "Sky-side recursion and cycle handling" subsection covering all three Sky-side walks (comptime, typechecker, dep enumeration) and their protections.
- §25: add B20 risk entry (Sky-side walker recursion safety).

### Decision 14: `cache_on_disk_if(false)` for Sky-overridden queries

**STATUS: AUDIT SHIPPED 2026-06-24** (Phase I). The audit found this decision's
prescribed Provider-slot syntax cannot be implemented as written — see the
revised understanding immediately below, then the original text for historical
context. The handoff Phase I entry above has the implementation notes.

**REVISED understanding (2026-06-24 audit).** `cache_on_disk_if` is a
query-DECLARATION-time modifier in rustc's macro DSL (`rustc_middle/src/query/mod.rs`),
NOT a Provider slot. The `providers.queries.layout_of_cache_on_disk_if` syntax
below doesn't compile against current nightly — that field doesn't exist on
`Providers`. The macro-generated `cache_on_disk` function emits whatever the
declaration says (defaulting to `false` if the modifier is absent). Sky cannot
override this from outside without fork-patching each query's declaration.

The audit re-derived cache-safety from upstream declarations:

| Query | Upstream policy | Why safe under Sky |
|---|---|---|
| `per_instance_mir` | `false` (Sky fork patch) | Never disk-cached. |
| `layout_of` | (no clause → default `false`) | Never disk-cached. |
| `cross_crate_inlinable` | (no clause → default `false`) | Never disk-cached. |
| `collect_and_partition_mono_items` | (no clause + `eval_always`) | Re-runs every compile. |
| `symbol_name` | `true` upstream | Override scheduled for removal per Decision 2; default mangler invalidates correctly (Instance-keyed). |

Implementation:
- All 5 `rustc-lang-facade/src/queries/*.rs` files annotated with `cache-audit:` marker comments.
- New CI fence `toylangc/tests/cache_audit.rs` asserts every override carries a marker.
- New `rust-interop-architecture.md` §22.4.1 documents the policy table.
- New §25.2 B21 risk entry tracks "per-query disk-cache staleness if rustc evolves the cache API."

**ORIGINAL prescription (preserved for historical context — DO NOT IMPLEMENT AS WRITTEN):**

**WHAT.** Every Sky-overridden query forces `cache_on_disk_if(false)` to prevent rustc's incremental cache from returning stale results when Sky's universe changes between compiles.

Audit list (per reviewer's micro-note):
- `per_instance_mir`: already declared `false` in the fork patch. ✓
- `layout_of`: force false. Sky's universe-dependent layouts → silent-incremental-staleness is the failure mode.
- `cross_crate_inlinable` + `extern_queries.cross_crate_inlinable`: force false. The rmeta-encoding flow makes this load-bearing.
- `collect_and_partition_mono_items`: verify rustc's default (probably already per-compile, not disk-cached); force false defensively if not.

**WHY.** Round-3 reviewer's late-arriving Q4 concern (sharpened in the closing micro-note): post-elimination of mir_shims and symbol_name overrides, MORE queries flow through rustc's natural caching path. Sky's universe state affects, e.g., `layout_of` results for SkyOpaqueType; layout_of is cached on disk by default; Sky's universe changes between compiles (Sky source edit changes a typeid) → cache returns stale layout.

Failure mode is SILENT WRONG layouts in incremental builds; doesn't show up in cold-build CI.

`cross_crate_inlinable` is the sharpest case: Sky forces it to false universe-conditionally, AND that result flows into rmeta encoding for downstream consumers. Stale rmeta-encoded inlinability decisions propagate badly.

**ALTERNATIVES CONSIDERED.**
- Include Sky's universe fingerprint in cache key (preserves cache benefit). Deferred to v2 as perf optimization. v1 prioritizes correctness via the conservative cache disable.
- Hope rustc's default invalidation catches it. Rejected — Sky's universe state isn't part of rustc's cache key.

**IMPLEMENTATION NOTES.**
- For each Sky-overridden query, the override forces cache_on_disk_if(false) via the providers structure:
  ```rust
  providers.queries.layout_of = sky_layout_of_provider;
  providers.queries.layout_of_cache_on_disk_if = |_tcx, _key| false;
  ```
  (Exact API per current nightly; the point is Sky owns both the provider AND the cache policy.)
- Verify `collect_and_partition_mono_items`'s default — probably already per-compile (not disk-cached); if confirmed, no Sky override needed for cache policy. Document the verification result either way.
- CI fence: edit-and-rebuild test — build a project, edit Sky source that changes a typeid, rebuild WITHOUT cache wipe, verify the binary's behavior reflects the edit.

**GOTCHAS.**
- Sky's toolchain version is embedded in rustc's version string; the cache hashes the version string. Cross-Sky-version invalidation works via this mechanism. Don't double up.
- Perf cost is small: cheap queries re-run per-compile; expensive ones like collect_and_partition_mono_items aren't typically disk-cached by default anyway.

**DOC IMPACT.**
- §22.3 (new subsection): "Queries Sky touches and their cache policy." Table per query: default policy, Sky's override, why Sky forces false (or doesn't).
- §25: add B21 risk entry (per-query disk-cache staleness).

### Decision 15: Drift-observation discipline + B24/B25/B26 risk entries

**WHAT.** Cross-cutting discipline: each Sky override eliminated trades surface area for drift-observation. The criterion for eliminating a Sky override:
1. Does this override serve as a synchronization point where Sky would observe rustc behavior drift?
2. If yes: keep as a delegating shim (live no-op but with deliberate drift-observation purpose).
3. If no (override genuinely does no work AND change in rustc's default would manifest cleanly via integration tests): eliminate.

mir_shims elimination (Decision 1): justified — drop integration fixtures catch shape drift.
symbol_name elimination (Decision 2): justified — Thread C inlining matrix + cross-crate link tests catch mangler drift.

New B-class risks added to §25 capturing the drift-observation gaps:

**B24. Drop-glue shape stability.** Post-mir_shims-elimination, rustc's `build_drop_shim` shape is in Sky's load-bearing dependency surface. Risk if rustc changes pre-drop/post-drop steps, Drop::drop ordering, field iteration order, etc. Probability ~5-10% over 5 years (deeply embedded in MIR semantics). Impact: silent UB in destructor chains. Detection: integration fixtures with multi-field types whose drops have observable side effects (Fixtures 1-3, 6-9 from the empirical work backlog). Reaction: re-introduce mir_shims override.

**B25. Default symbol mangling stability.** Post-symbol_name-elimination, rustc's default mangler (v0) is load-bearing. Risk if rustc changes mangler defaults (HAS happened — v0 was introduced 2020). Probability ~20-30% over 5 years. Impact: link errors (mostly clean failure mode, low silent-corruption risk). Detection: cross-crate symbol-name tests + mangler-version sweep. Reaction: re-introduce symbol_name override as delegating shim.

**B26. `drop_in_place` resolution path stability.** Post-mir_shims-elimination, Sky relies on rustc's `DropGlue` InstanceKind being the only relevant drop path. Risk if rustc adds new InstanceKind variants for drop (`AsyncDropGlue` already exists for experimental AsyncDrop). Probability ~15-20% over 5 years. Impact: silent miscompile if new variant bypasses user's Drop impl. Detection: drop-instance-kind-coverage tests. Reaction: override relevant InstanceKind resolution.

**WHY.** Round-3 reviewer's "a new concern: every elimination tightens dependency on rustc's natural behavior." Each removed override removes a synchronization point where Sky could observe drift. The B-class risks should grow new entries.

**IMPLEMENTATION NOTES.**
- Add the discipline as a meta-invariant in §26 (or wherever cross-cutting principles live).
- Add B24/B25/B26 risk entries to §25 with the full risk profile (probability, impact, canary, reaction).
- Build CI fences per each risk's detection requirement (covered in the empirical work backlog).

**GOTCHAS.**
- Don't over-eliminate. If you eliminate an override without confirming integration tests cover the drift, you've removed a sentinel without gaining one. Some overrides should stay as delegating shims (live no-op but intentional).

**DOC IMPACT.**
- §25: B24, B25, B26 entries.
- §26: drift-observation discipline as cross-cutting invariant.

### Decision 16: §1.7 reframe — backend pluralism leads, not "MIR can't express it"

**WHAT.** §1.7's "Sky does not surrender LLVM output control" reasoning leads with **backend pluralism** (GPU/NPU/MLIR/etc. won't go through LLVM, so committing to MIR forecloses non-LLVM backends) as the principled load-bearing reason. Engineering reuse of Vale's Inkwell pipeline is the secondary practical reason. The "MIR can't express it" framing is RETIRED — MIR is genuinely expressive enough; pretending otherwise invites future engineers to disprove it and re-open the decision.

**WHY.** Round-2 reviewer Q2 + round-3 confirmation: "Sky's vocabulary is bigger than MIR" was doing too much work. MIR's `Rvalue`/`TerminatorKind` cover almost any imperative program. Once group/linearity invariants are typechecker-only and comptime values bake to constants, MIR could probably express the body content.

The TRULY load-bearing reasons are:
1. **Backend pluralism.** GPU/NPU/custom targets won't go through LLVM. Sky may want to lower to MLIR for some computations. Committing to MIR-as-target permanently bolts Sky to rustc's LLVM pipeline.
2. **Engineering reuse.** Vale's existing Inkwell pipeline exists; rewriting as a MIR builder is months of work for a property that's already paid for.

User confirmed: "i might want to make sky based on MLIR, which means holding direct references to high-level nodes and optimizations that are defined by others. i dont think MIR can express that. #2 (engineering reuse) isnt much of a reason for us, we can rewrite things, sky/vale are still small. its mainly the backend pluralism."

**IMPLEMENTATION NOTES.**
- This is a doc change, not an architecture change. Update §1.7 to lead with backend pluralism.

**DOC IMPACT.**
- §1.7 (Sky does not surrender LLVM output control): rewrite leading with backend pluralism. Drop the "vocabulary" framing. Acknowledge engineering reuse as secondary.

### Decision 17: Stub-type contract — Sky's drop function must not invalidate field storage that auto-drop will subsequently traverse

**WHAT.** The structural invariant for Sky's stub types under the mir_shims-eliminated model:

> Sky's `__sky_drop_X` function must not invalidate field storage that auto-drop will subsequently traverse. Field destructors run via the standard auto-drop ladder after `Drop::drop` returns; Sky's drop function does Sky-side cleanup that's independent of field storage.

Today's ZST-only stub pattern (`SkyOpaqueType<HASH>` + `PhantomData<P>`) trivially satisfies this (no fields with destructors to traverse). Future stub-gen evolutions may expose non-ZST fields (with or without destructors) as long as the contract holds.

NO `ManuallyDrop` requirement by default. ManuallyDrop is only needed in edge cases where Sky's drop function specifically wants to take ownership of field destruction (rare).

**WHY.** Round-3 discussion explored two false framings before landing here:
- First false framing: "stub types' source-level fields must all be `!needs_drop`." Too narrow — implies stub fields are restricted to ZST or u64-tag-shaped types.
- Second false framing: "non-ZST fields with destructors MUST be wrapped in `ManuallyDrop`." User pushed back: "why do we need to wrap them in `ManuallyDrop`?" Correct answer: we don't, generally.

The actual invariant is about Sky's drop function's BEHAVIOR (don't invalidate field storage), not about the field TYPES.

In practice: Sky's `__sky_drop_X` operates on Sky's UNIVERSE (off-stub), not on the stub allocation's bytes. The stub allocation is rustc-managed; Sky's universe holds the real data. Sky's drop function does universe-side cleanup that doesn't touch field storage. So field destructors via auto-drop work fine.

**IMPLEMENTATION NOTES.**
- Sky's `__sky_drop_X` body discipline: operate on Sky's universe; don't read or write the stub allocation's field storage in ways that would interfere with auto-drop.
- For the typical case (stub fields are ZSTs by convention), this is vacuously satisfied.
- For edge cases where Sky genuinely wants to take ownership of a field's destruction (e.g., specific ordering requirements), use `ManuallyDrop<T>` for that field and explicitly invoke `ManuallyDrop::drop(&mut self.field)` in Sky's drop function.
- See Decision 1 (mir_shims elimination) for the broader drop machinery context.

**GOTCHAS.**
- This is a Sky drop function CORRECTNESS property, not a type-level structural property. CI fences can't easily enforce it at the type level; instead, enforce it at the emitter level (the `project_raw_field` helper discipline from Decision 1's CI fence).
- The earlier "stub fields are `!needs_drop` only" framing was a self-imposed restriction we don't actually need. Allow stub fields to be anything Sky's `layout_of` override can report consistent offsets for.

**DOC IMPACT.**
- §10 / §15: state the invariant as the broader "auto-drop ladder finds only non-interfering fields" property. Today's ZST-only convention is one way to satisfy it; ManuallyDrop wrapping is the more flexible alternative for cases where Sky wants ownership.

---

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

### Fixtures to build BEFORE migration

Build under `toylangc/tests/integration_projects/drop/` (new subdirectory). Each fixture is a full mini-project (Cargo.toml, .toylang source, optionally Rust source). Integration test harness runs end-to-end.

**Fixture 1: basic drop chain.**
```sky
struct MyType { resource: SomeSkyResource }
impl Drop for MyType { fn drop(&mut self) { release(self.resource) } }

fn main() {
    let mut v = SkyVec::<MyType>::new();
    v.push(MyType { resource: acquire() });
    v.push(MyType { resource: acquire() });
    v.push(MyType { resource: acquire() });
    // v drops at scope end
}
```
Assertions:
- Drop chain runs: `Vec::Drop` → Sky-side handling → per-element drop → `MyType::Drop::drop` → release(resource) per element.
- `release()` called exactly 3 times.
- Drop order is deterministic (pin LIFO or FIFO; either is fine, document the choice).
- Each `release` happens before SkyVec storage is freed.
Verification: instrument `release` to log; assert log content matches expected sequence.

**Fixture 2: drop with may_dangle (borrowed data in container).**
```sky
struct Holder<'a> { borrowed: &'a I32 }

fn outer() {
    let value = 42i32;
    let mut v = SkyVec::<Holder<'_>>::new();
    v.push(Holder { borrowed: &value });
    drop(v);  // explicit drop before value goes out of scope
}

fn dangling_case() {
    let mut v = SkyVec::<&'_ i32>::new();
    {
        let value = 42i32;
        v.push(&value);
    }  // value goes out of scope; v still holds dangling ref
    // v drops at scope end — needs #[may_dangle] for SkyVec to compile
}
```
Assertions:
- With `#[may_dangle] T` on SkyVec's Drop (stdlib annotation per Decision 12): `dangling_case()` compiles. Drop runs without dereferencing the dangling reference.
- For a user-defined Sky container without may_dangle: `dangling_case()` is a borrowck error pointing at v's drop site.
Verification: build the fixture both ways and check rustc's output (success / specific error message).

**Fixture 3: linear type in a Rust collection.**
```rust
extern crate sky_lib;
use sky_lib::LinearWidget;

fn store(v: &mut Vec<LinearWidget>, x: LinearWidget) {
    v.push(x);
}

fn main() {
    let mut v = Vec::new();
    store(&mut v, sky_lib::make_linear_widget());
    // v drops at scope end; LinearWidget's drop panics + aborts
}
```
For v1 (runtime panic, per Decision 11):
- Build succeeds.
- Runtime: when Vec drops, `__sky_drop_LinearWidget` runs per element, panics with Sky's documented message.
- Process aborts with non-zero exit.
Assertion: panic message contains "linear type", "must be explicitly consumed" or equivalent, points to LinearWidget by name.
For v2 (compile-time error, deferred — see Open Questions): build fails; error span at v.push(x) in user source; error text explains the constraint.

**Fixture 4: closure with move capture.**
```sky
fn outer() {
    let resource = acquire();
    let closure = move || drop(resource);
    closure();  // closure drops; resource's destructor fires
}
```
Assertions:
- Closure type's stub representation includes the captured resource.
- Drop runs for closure → resource drops → release.
- `release()` called exactly once.

**Fixture 5: closure with ref capture + may_dangle.**
```sky
fn outer<'a>(data: &'a Data) -> impl Fn() + 'a {
    move || println(data.value())
}

fn dangling_case() {
    let closure;
    {
        let data = Data::new(42);
        closure = outer(&data);
        // closure ends here
    }
    // data is dropped; closure (which captures &data) is also out of scope
}
```
Assertions:
- Closure's stub Drop has `#[may_dangle] T` (per Decision 12's syntactic rule applied to ref captures).
- Compiles cleanly.

**Fixture 6: async NotStarted phase drop.**
```sky
async fn fetch(id: I32) -> Data { http_get(id).await }

fn main() {
    let f = fetch(42);  // SkyNotStarted_fetch; captures id = 42
    drop(f);            // dropped before .await
    // captures (just id) drop normally; no abort
}
```
Assertions:
- f is movable (Decision 10).
- Drop runs; no abort; no allocation leak.
- Sky-side cleanup runs (if any captures need it).

**Fixture 7: migratory async mid-execution drop.**
```sky
migratory async fn worker(items: Vec<I32>) -> () {
    for item in items {
        process(item).await;
    }
}

fn main() {
    let f = worker(vec![1, 2, 3]);
    let handle = tokio::spawn(f);
    sleep(100ms);  // let it run for a bit
    handle.abort();  // tokio drops the future mid-execution
    // Migratory: state machine drops live captures, cleans up; no abort
}
```
Assertions:
- Build links cleanly.
- Runtime: spawn works; abort triggers drop; live captures (whichever are alive at current state) drop correctly; process exits cleanly.

**Fixture 8: default async mid-execution drop = panic.**
```sky
async fn process(g: &g'State) -> () { /* uses g across .awaits */ }

fn main() {
    let state = State::new();
    let f = process(&state);
    let handle = tokio::spawn(f);  // This should fail to compile (non-'static)
    // ... assuming it compiles via wrappers ...
    handle.abort();  // would trigger panic+abort
}
```
Test in two variants:
- Variant A: try to spawn directly; expect compile-time rejection (per Option C' Send/'static honesty, Decision 8).
- Variant B: wrap via the (deferred) tokio bridge; abort triggers Sky's drop panic.

**Fixture 9: cancellable wrapper Pending drop with cleanup handler.**
```sky
async fn long_running() -> Result { ... }

fn main() {
    let cleanup_ran = AtomicBool::new(false);
    {
        let f = into_cancellable(long_running(), || {
            cleanup_ran.store(true, ...);
        });
        let handle = tokio::spawn(f);
        sleep(10ms);
        handle.abort();  // dropped while Pending
    }
    assert!(cleanup_ran.load(...));
}
```
Assertions:
- Cleanup handler invoked when wrapper dropped Pending.
- Cleanup NOT invoked when wrapper dropped Ready (separate test variant).

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

**Status: SHIPPED 2026-06-24.** See Phase B entry below for the full outcome + headline numbers. The bench design notes that follow are preserved as the spec the shipped fixtures implement (commands have refreshed for the actual fixture layout — `bench1_dev_o0_nolto`, `bench1_o3_thin`, etc. — and the runner script + reproduction steps are in `rust-interop-architecture.md` §22.4.2).

Per reviewer's order-of-presentation discipline: **the bench numbers go FIRST in round 4.** Lead with this; the rest of the implementation work calibrates by the bench's results.

**Bench 1: pure call-boundary cost (worst case).**
```sky
// Sky lib:
export fn add(a: I32, b: I32) -> I32 { a + b }
```
```rust
// Rust bin:
fn main() {
    let mut sum: i64 = 0;
    for i in 0..100_000_000 {
        sum = sum.wrapping_add(add(i, 1) as i64);
    }
    println!("{}", sum);
}
```
Builds across the matrix:
- Profiles: dev (default opt-level=0 + lto=false), dev-with-O1 (opt-level=1 + lto=false), release (lto=false), release with lto="thin", release with lto="fat".
- Per reviewer's micro-note: add `opt-level=1 + lto=false` as third data point (isolates Sky's boundary cost from O0's general slowness).
Measure:
- Wall-clock runtime per build.
- `.text` size of the produced binary.
- Symbol count of the produced binary (`llvm-objdump -t | wc -l` or similar).
Per reviewer's instrumentation suggestion: `.text` size + symbol count at each LTO level quantifies the inlining-vs-emission tradeoff visibly.

**Bench 2: realistic work-per-call.**
Same Sky function but doing K units of work (`a*b + 3`, or a Vec push, or similar). Vary K from 1 to ~100. Plot ratio (lto=false/lto="thin") as function of K.
Per reviewer's "the realistic ratio is usually 1.5-3x of the synthetic peak" rule-of-thumb: this calibrates whether the synthetic-bench cost translates to real-world UX pain.

**Bench 3: drop chain (validates mir_shims-eliminated drop model under perf scrutiny).**
```sky
export struct Widget { id: I32 }
// Drop impl bridges into __sky_drop_Widget
```
```rust
fn main() {
    let v: Vec<Widget> = (0..10_000_000).map(|i| Widget { id: i }).collect();
    drop(v);  // 10M Widget drops
}
```
Measure: per-drop overhead at lto=false vs lto="thin". Validates that mir_shims elimination doesn't have unexpected drop-perf surprises.

**Possible outcomes (per reviewer's pre-discussion):**
- Bench 1 ratio < 2x: architecture's perf overhead is small even worst-case. Document "LTO recommended but not critical"; lock §5.5 confidently.
- Bench 1 ratio 2-10x: real cost for tight loops; mitigated by LTO. Document "recommend `[profile.dev] lto = 'thin'` for dev iteration"; lock §5.5.
- Bench 1 ratio > 10x: architectural cost is significant. Same documentation recommendation; consider whether to investigate the AvailableExternally-or-equivalent alternative.
- Bench 1 LTO doesn't reduce ratio: architecture has a bug — LTO isn't actually inlining. **DON'T lock §5.5; investigate and fix** before proceeding.

Schedule: ~1 focused day to write benches + run on representative targets (linux-x86_64 + macos-arm64 if available) + fold numbers into §22.4 doc draft.

---

## Implementation backlog (ordered by dependency)

### Phase A: Empirical baseline fixtures under CURRENT model (1-2 weeks) — DONE 2026-06-23

**Status: SHIPPED 2026-06-23.** Findings A.1–A.10 in Decision 1 above.

**Headline finding (A.6):** the previous `mir_shims` model was silently
no-op'ing — even with a properly emitted `impl Drop for Widget` stub,
`drop_in_place::<Widget>`'s body was a no-op and `<Widget as Drop>::drop`
was absent from the binary. The handoff's "build fixtures first" gate
revealed the previous machinery had never actually worked, validating
Decision 1's premise empirically and reframing Phase E's risk profile:
mir_shims elimination loses zero functionality because there was no
functionality to lose.

**Fixture 1 (basic drop chain)** lives at
`toylangc/tests/integration_projects/drop/fixture1_basic_drop_chain/`
under the AS-IMPLEMENTED shape (Phase E.d) and serves as the forward-
going regression test. The other 8 fixtures (Phase E.b/E.c additions —
see Phase E entry below) also live under `drop/` and pass green.

**Empirical work surfaced:**
- Toylang parser rejected `&mut self` — extended `parse_impl_method` to accept it.
- Required `use std::ops::Drop` in source; later relaxed by adding unconditional `pub use core::ops::Drop;` re-export in stub_gen (gated on absence).
- The cascade-discovery path (`collect_consumer_trait_impl_instances`) already captures `<Widget as Drop>::drop` correctly when the partition contains it.
- `oracle::find_trait_impl_method_def_id` correctly resolves to the impl method's DefId once the trait is `pub use`-re-exported.

Fixtures 2/3 (may_dangle, linear-panic) deferred — they need toylang
grammar/runtime extensions that aren't on the Phase E critical path.
The shipped Fixtures 1-9 already cover the load-bearing cases.

### Phase B: Perf bench — DONE 2026-06-24

**Status: SHIPPED 2026-06-24** in commit `81b6eb1` (initial scaffolding + first results) plus a follow-up commit landing the reproduction steps + interpretation assumptions in §22.4.2/§22.4.3.

**Outcome:** 17 bench fixtures (5 Bench-1 LTO/opt variants + 2 Rust-only baselines + 6 Bench-2 K/LTO combos + 4 Bench-3 drop variants); shell runner at `toylangc/tests/scripts/run_perf_bench.sh`; markdown results in `tmp/perf-bench-results.md`; round-4 summary in `tmp/perf-bench-summary.md`; per-fixture main-symbol disassembly archived to `tmp/perf-bench-disasm/`.

**Headline numbers** (M-series macOS, LLVM 21.1.8, 5-run median):
- Bench 1 (100M `add` calls): O3 nolto = 86.7ms; O3 thin = 57.9ms → **LTO ratio 1.50×** (handoff decision band: `<2×` → "lock §5.5 confidently").
- Rust baseline at O3 thin = 58.1ms → **Sky vs Rust delta = 0.3%** (essentially identical).
- Bench 2 K=100 (10M calls): nolto 11.7ms vs thin 7.9ms → ratio 1.48× (matches Bench 1; ratio is a property of the cross-crate boundary, not Sky-specific).
- Bench 3 (10M `<Widget as Drop>::drop`): O3 nolto = 10.0ms; O3 thin = 0.4ms → **LTO ratio 26.5×** (worst-case empty-Drop amplification).

**Decision-gate verdict:** lock §5.5. Recommendation: `[profile.release] lto = "thin"` for any perf-sensitive build.

**Two follow-up findings (documented in arch doc §F.18 + §25.2 B10):**
- **F3:** Sky `main` + while loop + 1M+ allocations stack-overflows (hypothesis: toylangc O0 alloca recycling bug). Worked around by driving Bench 3's loop from a Rust caller.
- **B10 residual:** Phase E drop synthesis + `Vec<SkyStruct>` at opt-level ≥ 1 still trips the LLVM 21 BitcodeWriter bug under ThinLTO cross-CGU import. Same Rust-caller workaround. §25.2 B10 was tightened from "CLOSED" to "CLOSED for primary path; residual trigger…".

**Doc landings in this phase:**
- New §22.4 "Perf model" with bench results, recommendation, and the cache-policy table (§22.4.1).
- New §22.4.2 "Reproducing the benches" with precise commands.
- New §22.4.3 "Interpretation assumptions" covering host sensitivity, `black_box` discipline, thermal/freq variance, B10 Rust-caller workaround, LTO-ratio interpretation guidance.
- §22 renumbered (existing §22.4 → §22.5; §22.5 → §22.6).
- §5.5 trade-off paragraph: empirical anchors (1.5× hot-call slowdown, ~26× drop-heavy slowdown).
- §F.16 inlining-levels table: empirical-speedup column at Levels 4/5.
- §F.18 (Phase E lessons): F3 + B10 residual footnotes.
- New §25.2 B27 risk entry: "bench-detected creeping perf regression between nightly bumps" with canary at Bench 2 K=100 ratio drift > 10% / Bench 3 ratio drift > 20%.

**What stayed deferred (and is in the "Known follow-up" backlog at the end of this doc):**
- B-22 criterion-style statistical framework (~1 day on its own).
- B-11 cross-platform reruns (linux-x86_64 — Sky's actual primary target).
- B-12 / B-13 generic-fn / trait-impl benches (need design).
- I-1 B10 upstream investigation + minimal LLVM repro.
- I-12 toylangc O0 alloca-recycling fix (F3 root cause).

### Phase C: Tool attribute infrastructure (1-2 days)

Wire up the `#![register_tool(skyc)]` + `#[skyc::emit_consumer_body]` tool attribute machinery.

Tasks:
- Stub source's crate root emits `#![register_tool(skyc)]`.
- Define `sym::skyc_emit_consumer_body` for the attribute name.
- Verify rustc's `tcx.has_attr(def_id, sym::skyc_emit_consumer_body)` works correctly on tagged items.

**Output**: attribute machinery in place; partition predicate can be migrated next.

### Phase D: Partition predicate migration (1 day, depends on C)

Replace `is_consumer_codegen_target` with the two-gate attribute conjunction (Decision 3). Update skyc's stub_gen to tag items per the Category A/B split.

Tasks:
- Implement new predicate per Decision 3.
- Update skyc's stub_gen: tag exported Sky functions, Sky drop fns, Sky trait-impl methods, Sky accessors. Do NOT tag bridges, Phase-6 wrappers, type/trait declarations, the marker.
- AUDIT classification matchers (`is_consumer_fn`, `is_consumer_accessor_safe`); delete if orphaned post-symbol_name-elimination.
- Build CI fence: verify 1:1 mapping between tagged items and Sky's emission list.

**Output**: predicate migrated; classification matchers cleaned up; CI fence in place.

### Phase E: mir_shims elimination — DONE 2026-06-23

**Status: SHIPPED 2026-06-23**, in four sub-phases:

**Phase E (the override deletion).** `rustc-lang-facade/src/queries/drop_glue.rs` + `mir_helpers.rs` deleted. `mir_shims` removed from `Providers` registration + `install_query_defaults` + `DEFAULT_MIR_SHIMS` OnceLock + `default_mir_shims()` accessor. Rustc's default DropGlue path fires unchanged thereafter. All 333 existing integration tests passed immediately upon override removal — empirical proof that the override was vestigial (Finding A.6 expansion).

**Phase E.b (Rust-typed locals get scope-end `drop_in_place::<T>`).** Briefly shipped: `CodegenCtx::scope_drops` tracked `(alloca, ResolvedType)` at let bindings; `emit_scope_drops` walked LIFO before each void return. Move-semantics conservative: sret / primitive returns skipped drops (caller's binding owns). Refactored away in E.d below.

**Phase E.c (Sky-struct locals get direct `<T as Drop>::drop` via trait dispatch).** Briefly shipped: `emit_sky_struct_drop` dispatched on type-kind in `emit_scope_drops`. Codegen-extern-wrapper fix landed in this sub-phase too: switched unconditional `module.add_function` to `get_function`-or-add to handle the case where the body's call site declared the extern symbol first, which had been producing LLVM `.1` disambig collisions (Fixture 6 surfaced it). The `get_function`-or-add fix is RETAINED post-E.d. Refactored further in E.d.

**Phase E.d (AST-rewrite refactor for principled adherence).** The shipped shape. Replaced LLVM-IR-layer drop emission with a typed-AST synthesis pass `insert_scope_end_drops` that runs after `resolve_fn_body`. The pass appends synthetic `Drop::drop(&local)` StaticCall nodes at scope-end positions; existing pipeline machinery handles them uniformly. Collapsed 4 specialness sites to 1. Added `tcx.adt_destructor` query as the Rust-type filter so Option / Result / Stdout / primitives (which have no trait-symbol drop) don't ICE the mono collector. Added `pub use core::ops::Drop` to stub_gen, gated on absence. See Decision 1's "AS IMPLEMENTED" subsection for the full shape.

**Files touched (full inventory):** see Decision 1's "FILES TOUCHED" subsection. Net diff: 9 files modified, 2 deleted, ~640 insertions / ~33 deletions; +9 drop fixtures.

**Output:**
- mir_shims gone.
- `insert_scope_end_drops` is the single Drop-aware compiler pass.
- 9 drop fixtures (basic chain, two-Vec LIFO, helper-fn drop, empty Vec, Widget without Drop, single Sky local with Drop, multiple Sky locals LIFO, Sky local without Drop impl, Sky local in helper fn) at `toylangc/tests/integration_projects/drop/`.
- Full integration suite: 342 / 0 / 1 ignored cold.

**What was deferred** (and remains deferred):
- The `__sky_drop_X<T>` bridge function from the original plan. Sky's
  AS-IMPLEMENTED direct-dispatch is functionally equivalent for the
  non-comptime case (toylang has no comptime). When comptime lands
  (Sky v2), the bridge may need re-introduction to share one drop
  fn across content-hash-keyed `SkyOpaqueType<T>` variants.
- `project_raw_field` CI fence — not needed because the synth pass
  never directly accesses field storage; the drop call is just
  `Drop::drop(&local)` and the body lives in fill_extra_modules
  emission that goes through normal codegen.
- The `#[may_dangle]` syntactic-rule wiring (Decision 12) — gated
  on Sky generic stdlib containers existing. Toylang's existing
  uses of `Vec<Widget>` flow through Rust's stdlib `<Vec<T> as Drop>::drop`
  which already has correct `#[may_dangle]` annotations.
- Async-fn drop wiring (Decision 11, §14) — gated on async support
  landing in toylang.

### Phase F: symbol_name elimination (half-day)

Remove the symbol_name override (Decision 2).

Tasks:
- Delete `rustc-lang-facade/src/queries/symbol_name.rs`.
- Remove `symbol_name` from providers.
- Delete `compute_consumer_symbol` and the `consumer_symbol_for_callback_name` callback (Tier 3 #12).
- Verify rustc's default mangler is what Sky's emission now consults.

**Output**: symbol_name override gone; Sky relies on rustc's default mangling.

### Phase G: cdylib build system (3-5 days)

Restructure Sky's toolchain to ship as paired rustc-fork + libsky_backend.so (Decision 4).

Tasks:
- Restructure `rustc-lang-facade` to compile as cdylib.
- Modify rustc-fork to load libsky_backend.so via the `CodegenBackend` plugin mechanism (`-Zcodegen-backend=sky` baked into default config).
- Build runtime version handshake at backend load.
- Update toolchain bundle structure (Sky toolchain ships rustc-fork + libsky_backend.so + skyc + cargo + LLVM shared libs).
- Update build scripts for the cdylib model.

**Output**: Sky's backend loadable as cdylib; fast iteration loop unlocked for further work.

### Phase H: cdylib FFI shape (1-2 days, depends on G)

Refactor patch 4 to use `#[repr(C)]` function-pointer struct instead of `&mut dyn ExtraModuleAllocator` (Decision 5).

Tasks:
- Modify rustc-fork's patch 4: replace `&mut dyn ExtraModuleAllocator<M>` with the function-pointer struct.
- Update `rustc_codegen_ssa::base::codegen_crate` call site.
- Update `rustc_codegen_llvm::lib::fill_extra_modules`.
- Update `rustc-lang-facade/src/extra_modules_hook.rs` consumer-side hook.

**Output**: FFI uses stable-ABI primitives; cdylib pairing failures fail at link, not at runtime.

### Phase I: cache_on_disk_if audit — DONE 2026-06-24

**Status: SHIPPED 2026-06-24** in commit `81b6eb1`.

**Key finding (changes Decision 14's scope substantially):** the prescribed Provider-slot syntax doesn't exist on current nightly. `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL (`rustc_middle/src/query/mod.rs`), not a Provider slot. Searching `providers.queries.layout_of_cache_on_disk_if` against current rustc returns nothing — that field literally does not exist on `Providers`. The "force false via Provider slots" prescription cannot be implemented as written.

**What we did instead (audit + document + fence):**

| Query | Upstream declaration | Why cache-safe under Sky |
|---|---|---|
| `per_instance_mir` | `cache_on_disk_if { false }` (Sky fork patch) | Never disk-cached. |
| `layout_of` | no clause → default `false` | Never disk-cached. |
| `cross_crate_inlinable` | no clause → default `false` | Never disk-cached. |
| `collect_and_partition_mono_items` | `eval_always` | Re-runs every compile; never cached. |
| `symbol_name` | `cache_on_disk_if { true }` | Override scheduled for removal per Decision 2. Default rustc behavior is Instance-keyed → cache invalidates correctly when Sky's universe state changes. |

**Files touched:**
- `rustc-lang-facade/src/queries/layout.rs`, `per_instance.rs`, `symbol_name.rs`, `partition.rs`, `cross_crate_inlinable.rs`: each gains a `cache-audit:` marker comment in its module header documenting the cache-on-disk policy + safety reasoning.
- `toylangc/tests/cache_audit.rs`: NEW. CI fence asserts every override file carries a `cache-audit:` marker. Adding a new override forces a fresh audit at that time.
- `rust-interop-architecture.md` §22.4.1: documents the policy table; cross-links to the fence.
- `rust-interop-architecture.md` §25.2 B21: new risk entry tracking "per-query disk-cache staleness if rustc evolves the cache API."

**What Decision 14 should be revised to say** (this entry's deferred to a clean revision pass; right now Decision 14 below is preserved as historical context with a footnote pointing here): "Audit every Sky-overridden query's `cache_on_disk_if` modifier in the upstream declaration. If the declaration is `false` / `eval_always` / missing-default-false, the query is cache-safe by construction. If the declaration is `true`, either retire the Sky override (preferred) or fork-patch the declaration to gate on a Sky-active marker. The `cache-audit:` marker discipline + CI fence preserve the audit findings at the code level so future overrides force a fresh audit."

**Output achieved:** cache discipline documented; CI fence in place; arch doc reflects reality.

**What stayed deferred:**
- A "cache_correctness_smoke" edit-and-rebuild fixture that mutates Sky source between builds. The audit finding (all queries safe by construction) means this fence is regression insurance, not active need. Designing the fixture properly requires temp-dir file mutation infrastructure that's bigger than today's scope. Reasonable future work; not blocking.
- Re-running this audit if rustc evolves the API (canary: B21 risk entry).

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

### Phase O: Drift-observation CI fences (3-5 days)

Build the detection tests for B24/B25/B26 (Decision 15).

Tasks:
- Cross-crate symbol-name test (B25 detection).
- Drop-instance-kind coverage tests (B26 detection).
- Confirm Fixtures 1-9 cover B24 detection requirements.
- Document fences in §25 risk entries.

**Output**: drift-observation safety net in place.

### Total estimate

~6-10 weeks for a focused engineer:
- Phase A: 1-2 weeks (empirical baseline).
- Phase B: 1 day (perf bench).
- Phase C: 1-2 days (attribute infra).
- Phase D: 1 day (predicate migration).
- Phase E: 1 week (mir_shims elimination, gated).
- Phase F: half-day (symbol_name elimination).
- Phase G: 3-5 days (cdylib build).
- Phase H: 1-2 days (FFI shape).
- Phase I: half-day (cache audit).
- Phase J: 2-3 days (u128 typeids).
- Phase K: 1 week (content-hash const args).
- Phase L: 2 weeks (per-view ref types).
- Phase M: 3-5 days (async typestate).
- Phase N: 2-3 days (Sky-side recursion).
- Phase O: 3-5 days (drift CI fences).

Some can parallelize (cdylib build + FFI shape + cache audit could overlap with content-hash work); some can't (predicate migration must precede mir_shims elimination; both must precede per-view ref types in stub_gen).

---

## Doc update plan

After implementation, the architecture doc needs updates reflecting the decisions. Don't do these BEFORE implementation — the doc updates should be backed by the implementation reality, not by predicted reality.

### High-priority rewrites (substantial changes)

- **§15 (drop semantics):** substantial rewrite around standard Drop impl + Sky drop function model. Add `#[may_dangle]` policy subsection (syntactic rule for synthesized types; user types opt in; stdlib containers annotated). Add Drop-body contract (Sky's drop function must not invalidate field storage). Add the two-category body model (Category A real bodies vs Category B unreachable placeholders + attribute predicate). Drop the mir_shims-override description entirely.
- **§1.7 (Sky does not surrender LLVM output control):** rewrite leading with backend pluralism. Drop the "MIR vocabulary" framing. Acknowledge engineering reuse as secondary practical reason.
- **§10 (type representation):** add three explicit reasons for opacity (transitive cascade, exotic layouts, Sky-is-main-character). Update stub-type contract to the broader "Sky's drop must not invalidate field storage" formulation.
- **§12.1 (Send):** rewrite around per-view ref types `SkyRef<T, V>`. Per-type honest Send for owned values.
- **§12.2 ('static):** correction — the "honest by construction" framing was wrong. Same Option C extension applies.
- **§13.3 / §13.4 (slab + content-hash):** rewrite — retire slab-pointer-as-u64; content-hash const args; slab purely Sky-internal.
- **§14.10 (async two-type split):** rewrite — typestate pattern, not two physical types.
- **§17 (tokio interop):** restructure — Sky-native async primary, tokio via bridge crate (deferred to when bridge crate exists).

### Smaller refinements

- **§1.2 + §11 (groups):** correction — groups are compile-time, not runtime arenas.
- **§3.1 (per-Instance body content):** sharpen — per-Instance dep enumeration is the load-bearing reason, not arbitrary-typed const generics.
- **§3.2 / §B.4 / §C.4 (patch 4):** update with rev 3 shape (function-pointer struct).
- **§4.1 / §4.2 / §4.3 / §4.4 (distribution):** restructure for cdylib model.
- **§5.3 (partition filter):** explicit predicate definition + 1:1 invariant + Category A/B split.
- **§5.4 (LlvmCodegenBackend delegation):** drop symbol_name from override list.
- **§6.2 (single-symbol):** reframe — single-symbol stays for simplicity, not IR-race protection (partition filter handles that independently).
- **§6.6.5 (Phase-6 wrappers):** brief comparison of `#[inline(never)]` vs `@llvm.used` (different layers).
- **§10.6 / §10.8 / §13.7 / §13.8 (typeids):** u128, content-hash mechanism.
- **§13.9 (no synthetic DefIds):** keep "no new fork surface" framing primary; content-addressing is bonus.
- **§14.1 / §14.3 / §14.5 / §14.7 (closures/async):** integrate with the new drop model and typestate pattern.
- **§19 (per_instance_mir):** extend to cover drop functions as just-another-class of Sky-generic-function. Add safety-properties subsection (mentioned-items invariant, recursion safety).
- **§22.3 (new):** queries Sky touches and cache policy table.
- **§22.4 (new):** perf model — LTO-first; bench numbers go here.
- **§25:** add B20-B26 risk entries.
- **§26:** add NNGZ enforcement note (drift-observation discipline + grep CI).
- **§29.6 (cdylib as open question):** close — committed for Phase 1.
- **§F.14.1 / §F.17:** update with post-mir_shims-elimination architecture context.

### Doc-correction discipline

Audit other chapters for empirical-backing gaps (similar to §15's now-acknowledged "no empirical backing" status). Add "Status" notes to chapters that lack toylang verification for their main claims. Specifically check §11 (groups), §12 (Send/Sync/'static), §13 (comptime), §14 (closures/async) — these may have similar gaps.

---

## Round 4 prep (when you come back to the reviewer)

Order matters per reviewer's discipline:

1. **Perf bench numbers FIRST.** Bench 1 + Bench 2 + Bench 3 across the lto matrix + O0/O1 dimension + `.text` size + symbol count. This single data point disambiguates whether several major architectural decisions (forced share_generics, cross_crate_inlinable=false, single-symbol-no-AvailableExternally) deliver acceptable UX. Without this number, round 4 has no anchor.

2. **cache_on_disk_if audit results.** Confirmation that the four queries on the reviewer's audit list are correctly handled.

3. **Drop integration fixture outcomes.** Fixtures 1-9 — did they pass under current model? Did they still pass after mir_shims elimination? What surprises surfaced?

4. **mir_shims elimination empirical validation.** Specifically: did the round-3 8-agent investigation's predictions hold up? What did the fixtures reveal that the reasoning missed?

5. **Implementation surprises grouped by where they surfaced.** cdylib build setup surprises, FFI shape surprises, per-view ref types coherence surprises, content-hash migration surprises, etc.

DON'T:
- Reopen design questions that were settled in rounds 1-3.
- Bring questions that empirical data should answer (cdylib operational details, perf, drop chain correctness).
- Pad with stylistic refinements.

DO:
- Frame surprises as questions if you genuinely need reviewer input.
- Quantify everything (bench numbers, fixture counts, line-of-code changes).
- Note any architectural decision that empirical work suggests we should REVISIT (rare; if it happens, lead with the evidence).

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

### 3. Cargo fingerprint sidecar blindness (~1 day)

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

---

## Sanity-checking questions to ask yourself before starting

Before diving into the NEXT chunk of work (Phase C/D — tool attribute infrastructure + partition predicate migration), confirm you can answer these:

1. What's the actual shape of Sky's Drop dispatch after Phase E.d? (No bridge fn; `insert_scope_end_drops` synthesizes `Drop::drop(&local)` AST nodes; existing pipeline handles them as ordinary trait static calls. The `__sky_drop_X<T>` bridge from the original plan was collapsed out as functionally equivalent for the non-comptime case.)
2. What does the `#[skyc::emit_consumer_body]` attribute mean, semantically? (It tags items that Sky will provide bodies for via `fill_extra_modules`; the partition filter removes them from rustc's CGU list; mandatory 1:1 correspondence with Sky's emission list.)
3. Why is `&mut dyn Trait` wrong across the cdylib FFI boundary but fine across a static-link boundary? (Vtable layout dependency; rustc constructs the vtable, cdylib has to interpret it; vtable layout is `rustc_codegen_ssa`-source-dependent.)
4. Why content-hash const args instead of slab-pointer-as-u64? (Two content-equal Sky values at different slab offsets would produce two distinct rustc Instances → two MonoItems with same symbol → linker conflict. Content-hash const args dedup at the Instance level.)
5. What does the perf bench actually measure, and what's the user-facing takeaway? (Bench 1 = synthetic call boundary cost: 1.50× LTO ratio, but ALSO Sky vs Rust baseline = 0.3% delta. Bench 3 = drop chain amplification: 26.5× LTO ratio because empty Drops chain. Recommendation: `[profile.release] lto = "thin"`. Bench 1 + baseline together prove Sky adds no measurable per-call overhead beyond Rust's cross-crate cost.)
6. What does the cache_audit fence enforce, and why? (It walks the 5 Sky-overridden query files and requires each to carry a `cache-audit:` marker comment. Adding a new override forces a fresh audit because the marker is required to pass the test. The audit found Decision 14's prescribed Provider-slot API doesn't exist; safety is by upstream-declaration construction.)

If you can't answer one, re-read the relevant section above. If you can, you're ready.

---

## What success looks like

**Phase A success — ACHIEVED 2026-06-23:** nine drop integration fixtures pass cold + warm under the new AST-rewrite drop synthesis model. Findings A.1-A.10 documented above; headline A.6 (prior `mir_shims` was empirically broken) validated Decision 1.

**Phase B success — ACHIEVED 2026-06-24:** perf bench numbers documented in `rust-interop-architecture.md` §22.4 (with reproduction steps in §22.4.2 and interpretation assumptions in §22.4.3). Numbers confirm "LTO-first model is acceptable" — Bench 1 LTO ratio 1.50× (in handoff's "<2× → lock §5.5 confidently" band); Sky vs Rust baseline at 0.3% delta (Sky adds no measurable overhead); Bench 3 drop chain 26.5× LTO speedup. Decision-gate verdict: lock §5.5. Two follow-up findings flagged: F3 (toylangc O0 alloca recycling) and B10 residual (ThinLTO cross-CGU import re-triggers the bug); both documented in §F.18 + §25.2 B10.

**Phases C+D success — NOT YET STARTED:** `#[skyc::emit_consumer_body]` attribute machinery in place; partition predicate migrated; orphaned classification matchers deleted; CI fence verifies 1:1 invariant.

**Phase E success — ACHIEVED 2026-06-23:** mir_shims override gone; all 9 drop fixtures pass under the new AST-rewrite model; no regressions in the 333 prior integration tests (now 342 total). The `project_raw_field` CI fence turned out unnecessary because the synth pass never directly accesses field storage.

**Phase F success — NOT YET STARTED:** symbol_name override gone; no test regressions; rustc's default mangler is what Sky's emission consults.

**Phases G+H success — NOT YET STARTED:** Sky's backend ships as cdylib with `#[repr(C)]` function-pointer struct FFI; iteration loop is order-of-magnitude faster.

**Phase I success — ACHIEVED 2026-06-24:** cache_on_disk_if audit complete; key finding is that Decision 14's prescribed Provider-slot API doesn't exist on current nightly; every Sky-overridden query is cache-safe by construction; CI fence at `toylangc/tests/cache_audit.rs` enforces `cache-audit:` markers on every override file. Decision 14 itself is annotated with the revised understanding above. The cache-correctness "edit-and-rebuild" fixture from the original plan stayed deferred (audit found we're safe by construction; the fixture is regression insurance not active need; designing it properly needs temp-dir mutation infrastructure that's bigger than the day's scope).

**Phase J success:** typeids are u128 throughout; collision detection in place with clear error message; existing tests still pass.

**Phase K success:** content-hash const args in use; slab is Sky-internal only; cross-invocation symbol matching works automatically.

**Phase L success:** `SkyRef<T, V>` machinery in place; closed V set documented; per-type honest Send for owned values; existing tests still pass (within the constraints of the new model — some tests may need updating to use the new API surface).

**Phase M success:** async typestate refinement in place; Fixtures 6-9 pass.

**Phase N success:** Sky-side recursion handling in place; pathological-deep-recursion fixture produces clean compile-time error.

**Phase O success:** drift CI fences in place for B24/B25/B26 detection.

**Overall success — round 4 inputs READY:** all four round-4 deliverables now in hand. Bench numbers (Phase B), cache audit results (Phase I), drop fixture outcomes (Phase A — 9 fixtures passing), mir_shims elimination empirical validation (Phase E — 342/0/1 tests). Plus implementation surprises grouped by where they surfaced (F3 toylangc alloca recycling; B10 residual under ThinLTO; cache_on_disk_if Provider-slot API doesn't exist). The next session can either (a) schedule round 4 with the reviewer using these anchors, or (b) continue with the unblocked next chunk of execution work — Phase C+D (tool attribute + predicate migration), Phase F (symbol_name retirement), Phase G+H (cdylib backend + FFI shape).
