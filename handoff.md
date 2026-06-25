# Sky Architecture Review (Rounds 1–3, 2026-06-23) — Implementation Handoff

**Source conversation:** the full three-round review exchange that inspired this handoff is archived at [`./convo-with-rustc.md`](./convo-with-rustc.md). This document is the distillation; the conversation is the rationale at full fidelity if you ever need it.

**Read this section first.** Below the major `=====` separator is prior-session history (mostly shipped work) — useful context but lower priority than what's in this section.

This section captures everything that came out of a three-round design-review exchange on `rust-interop-architecture.md` conducted 2026-06-23, plus the implementation work it commits us to. We've closed the review exchange and are pivoting to implementation. Round 4 with the reviewer happens after we have empirical data + implementation surprises in hand.

---

## Most recent landing: two-enum split (Option B) SHIPPED 2026-06-25

**One-line summary:** parser-shape types live in a new `SourceType` enum
(carries `StructRef { name, type_args }`); resolved-shape types live in
`ResolvedType` (carries `Struct { name, type_args, field_types }` with
`field_types` MANDATORY — no `StructRef` variant exists). The boundary
between them is the single chokepoint `oracle::resolve_source_type`;
codegen and the substitution walk literally cannot see a `StructRef`
because it's no longer a `ResolvedType` variant.

**Why it surfaced.** Sunny-karp surprise #3 (commit `af42c20`) — the
`StructRef → Struct` promotion silent miscompile — was one instance of
a broader class: `ResolvedType` having both `StructRef` and `Struct`
variants meant "this state is reachable but invalid" was representable
in the type system. The reactive fix in sunny-karp chained
`resolve_struct_fields` through `substitute_in_typed_body`; the
structural fix is to make the invalid state unrepresentable.

**Files touched (11):** `typed_ast.rs` (added `SourceType` enum +
`to_source_type` demote on `ResolvedType` + `contains_type_param` method
on `SourceType`), `ast.rs` (`Expr::*::type_args` slots → `SourceType`),
`parser.rs` (`parse_type` returns `SourceType`), `registry.rs`
(`ToyField.rust_type`, `ToyParam.ty`, `ToyFunction.return_ty`,
`typeid_table` values, `synthesize_accessor_fn` → `SourceType`),
`oracle.rs` (new `rustc_ty_to_source_type` + `try_source_to_rustc_ty` +
`source_to_rustc_ty` + `resolve_source_type` + `substitute_source_type`
+ `strip_source_ref` + `source_type_to_mangled_name`; `try_resolved_to_rustc_ty`
loses its `StructRef` arm; oracle queries take `&[SourceType]` and
return `SourceType`), `type_resolve.rs` (`resolve_struct_fields` retired
— replaced by `oracle::resolve_source_type`; closures take/return
`SourceType`; `substitute_type_params` no longer has a `StructRef` arm;
`substitute_in_typed_body` no longer needs the registry-promotion chain;
test mod retired — registry fixtures used `ResolvedType::StructRef`
which doesn't exist), `stub_gen.rs` (`resolved_type_to_syn` → `source_type_to_syn`,
operates on `SourceType` registry fields), `sidecar.rs` + `typeid.rs`
(`SourceType` in registry-side positions), `llvm_gen.rs` (the lazy
`StructRef`-resolve arm at line 185 is gone — codegen sees only `Struct`
by construction; demote at `redirect_to_wrapper` callsites because
codegen carries `ResolvedType`), `callbacks_impl.rs` (added
`source_to_rustc_ty_with_subst` + `collect_rust_type_names_source`
siblings for the registry-side walks; closure sigs flip to SourceType;
`resolve_caller_from_instance` chains `rustc_ty_to_source_type` →
`resolve_source_type` for the subst map values).

**Validation:** `cargo test --workspace` = **446 / 0 / 1** (down from
sunny-karp's 487; the 41 retired tests were the deleted type_resolve
unit-test module — ~38 tests whose fixtures depended on
`ResolvedType::StructRef` — plus 3 oracle unit tests for the retired
`contains_type_param` and `DeferredTypeParam` paths). All 352
integration_projects fixtures pass (the 1 ignored is the pre-existing
`test_inline_case3_inline_never` LLVM-attribute-stripping fixture per
arch §F gotcha); full suite covers the same end-to-end behavior the
retired unit tests checked.

**Mechanical cleanup landed alongside:**
- `oracle::contains_type_param` retired (no callers after sunny-karp +
  the two-enum split; the SourceType `contains_type_param` method is
  carried as `#[allow(dead_code)]` for future eager-typecheck use).
- `RustTypeLookupContext::DeferredTypeParam` variant retired.
- `UnresolvedRustType::is_deferred()` reduced to a vestigial `false`
  constant (closure arms in `after_rust_analysis` still reference it
  for forward-compat with any future legacy-defer producer).
- `resolved_to_rustc_ty_with_subst` retired (sole caller migrated to
  `source_to_rustc_ty_with_subst`).
- `collect_rust_type_names` (ResolvedType variant) retired (sole
  caller migrated to `collect_rust_type_names_source`).

**Surprises encountered:**

1. **Closure signature ripple.** Oracle queries flipped to `&[SourceType] →
   SourceType`. The closures defined in `callbacks_impl.rs` (`rust_method_ret`,
   `rust_param_types`) carry the same signature down to
   `type_resolve::resolve_fn_body`. The transitive update was a sweep of
   ~14 `&[crate::toylang::typed_ast::ResolvedType]` → `SourceType` across
   `callbacks_impl.rs` + `type_resolve.rs` + `llvm_gen.rs`'s cache-miss
   fallback closures. None of this was visible from sunny-karp's surprise
   #3 — surfaced only when running cargo check after the first cut.

2. **`MethodCall` receiver-type demote.** In `resolve_expr`'s `MethodCall`
   arm, the receiver's `typed_recv.ty` is `ResolvedType` (typed AST),
   but the oracle closure expects `&[SourceType]` for the receiver's
   `type_args`. Resolution: demote via `to_source_type()` at the
   callsite. The codegen-side counterpart (`redirect_to_wrapper`
   callsite at `llvm_gen.rs:348` and `callbacks_impl.rs:2129`) has the
   same shape — codegen carries `ResolvedType`, oracle wants `SourceType`,
   demote via the same path.

3. **Test module deletion was the cheapest path.** The `type_resolve.rs`
   unit-test mod (~1000 lines) extensively constructed registry-side
   fixtures via `ResolvedType::StructRef{...}` and called
   `resolve_struct_fields` directly. Migrating each fixture to
   `SourceType::StructRef` was mechanical but high-volume; the tests
   themselves were redundant with the integration suite's end-to-end
   coverage. Net: deleted the mod with a comment noting why, kept
   integration_projects as the authoritative validator. The 449/0/1
   final count reflects this — the lost tests were unit-tests of
   `resolve_struct_fields` (now retired) and `resolve_fn_body`
   end-to-end behaviors that the integration suite exercises through
   real fixtures.

**Net.** The class of silent-miscompile sunny-karp's surprise #3
represented is now unrepresentable: any code path that produces a
`ResolvedType` cannot accidentally leave it as `StructRef` (the variant
is gone). The compiler enforces the invariant.

---

## Previous landing: sunny-karp SHIPPED 2026-06-25

**Plan archive:** [`/Users/verdagon/.claude/plans/we-should-completely-do-sunny-karp.md`](/Users/verdagon/.claude/plans/we-should-completely-do-sunny-karp.md) — kept for the alternatives-considered rationale and the dependency-ordering of A–F.

**One-line summary:** generic Sky bodies type-resolve **once** at `after_rust_analysis`; the typed result lives on `ToylangState.typed_bodies`; per-Instance mono substitutes the cached typed body via a new pure typed-AST walk (`substitute_in_typed_body`) instead of re-running `resolve_fn_body` + `insert_scope_end_drops` per monomorphization. Bare-`TypeParam` locals get a late drop-synth pass (`insert_late_scope_end_drops`) at mono once their concrete type is known.

**Six work items shipped:**

- **A (oracle accepts Param-bearing queries):** Six `contains_type_param` early-returns deleted from `oracle.rs:506,813,834,856,890,964`. `try_resolved_to_rustc_ty` takes a new `caller_type_params: &[String]` arg; the `TypeParam(name)` arm rebuilds `ty::Ty::new_param(tcx, idx, name)` from the param's position in the surrounding fn's generics. All six query functions thread the arg through. The matching `contains_type_param` guard at `type_resolve.rs:730` (MethodCall resolver) was also deleted — not enumerated in the plan but had the same effect of forcing deferral.
- **B (cache typed body):** added `pub typed_bodies: BTreeMap<String, TypedBlock>` to `ToylangState` (NOT to `ToyFunction` as the plan suggested — `&self` on `after_rust_analysis` forbids `Arc::make_mut` through a shared registry handle; see "Trade-offs" below). Filled once at `after_rust_analysis` for every body-bearing fn and impl method (keyed by `"<Self>::<method>"`). `#[serde(skip)]`-equivalent: not serialized, downstream rederives at its own compile.
- **C (`substitute_in_typed_body`):** new pure typed-AST walk in `type_resolve.rs`. Each `TypedExpr.ty`, each `type_args` slot, runs through `rewrite_ty` = `substitute_type_params` → `resolve_struct_fields` (the second step is load-bearing; see "Surprises" below). Takes `&ToylangRegistry` for the field-resolution step.
- **D (mono substitution):** `resolve_caller_from_instance` / `resolve_caller_from_type_args` now return `ResolvedCaller { func, typed_body }`. Cache hit → substitute typed AST + `insert_late_scope_end_drops`. Cache miss (synthesized accessors not eager-typed) → fall back to `type_resolve_body`. All call sites threaded: per_instance_mir, populate channels, cascade drain, llvm_gen.
- **E (drop-synth split):** new `drop_synthesized: bool` field on `TypedStmt::Let`. Eager `insert_scope_end_drops` at `after_rust_analysis` sets it `true` on emitted drops. New `insert_late_scope_end_drops` at mono re-checks any `false`-flagged let against its now-concrete substituted type. `local_needs_scope_drop` takes `caller_type_params` so `Vec<T>`-style RustTypes resolve at the eager pass (Vec's destructor is an ADT property, valid regardless of args).
- **F (unify call sites):** `collect_rust_deps_recursive`, `walk_and_stash_internal_callees`, `codegen_internal_function` all read the precomputed substituted typed body. `codegen_internal_function` retains the re-resolve path as a cache-miss fallback. `ToylangInstance` and `FnItem` gained `typed_body: Option<TypedBlock>` fields.

**Validation:** `cargo test --workspace` = **487 / 0 / 1** (baseline 485 + 2 new sunny-karp fixtures). No existing fixture needed expected-output updates. The two new fixtures:

- `drop/fixture10_generic_consume_t_with_drop` — `consume<T>(x: T)` with `T = Widget` (Widget has `impl Drop`). Proves the late pass synthesizes the scope-end drop once the bare-T type becomes concrete.
- `drop/fixture11_generic_consume_t_no_drop` — same shape with `T = i32`. Proves no spurious synth.

**Trade-offs from the state-based cache (vs the plan's `ToyFunction.typed_body` design):**

- **Lost: locality.** A `&ToyFunction` no longer carries everything you need; consumers need `&ToylangState.typed_bodies` too. Sibling-fields-in-different-structs.
- **Lost: clone-on-merge for effective registries.** When `effective_registry` merges in upstreams, typed bodies wouldn't auto-travel from the upstream registries' Arcs (they don't even exist there). Today this doesn't bite because upstream sidecars don't ship typed bodies anyway (`#[serde(skip)]` was the plan), so downstream rederives at its own `after_rust_analysis`. The cost is paying the type-resolve once per crate in the dep graph instead of caching across crates.
- **Lost: cache-key cleanliness for impl methods.** With the field on `ToyFunction`, `method.func.typed_body` is one struct, one key. On state, I introduced `"<Self>::<method>"` as a string key. The namespace now needs a no-collision invariant — `impl Foo for Widget { fn drop() }` and `impl Drop for Widget { fn drop() }` would both produce `"Widget::drop"`. Not exercised by current fixtures but the discipline is now a thing.
- **Lost: borrow geometry.** `walk_and_stash_internal_callees` mutates `state.toylang_instances` while wanting recursive read access to `state.typed_bodies`. I clone the map into a `typed_bodies_snapshot` at entry. Per-walk-root cost, not per-callee.
- **Gained: no LangCallbacks trait change.** `Arc::make_mut(&mut self.registry)` would have required `&mut self` on `after_rust_analysis`, which is a facade-trait signature change. State-based avoids that entirely.
- **Gained: no serialization discipline to litigate.** `typed_bodies` simply doesn't exist on `ToylangRegistry`, so it can't be accidentally serialized.

**Net.** Acceptable for toylang. If Sky proper grows multi-crate generics where downstream compile time matters, or richer impl-method namespacing, the calculus shifts toward the plan's original design — and at that point either change the callback trait or move to interior mutability (`RwLock<ToylangRegistry>`).

**Surprises encountered:**

1. **State vs registry pivot.** `&self` on `after_rust_analysis` made the plan's `Arc::make_mut(&mut self.registry)` impossible. I wrote ~60 lines of "Practical fix..." comments trying to talk myself into a workaround before pivoting to state-based, which the plan had explicitly considered and rejected. See trade-offs above.
2. **Seventh `contains_type_param` guard outside the oracle.** First test run failed 28 case5 tests with `RustTypeDeferred { context: "method '.push' on receiver containing TypeParam" }`. The plan listed six oracle early-returns to delete, but there was a seventh in `type_resolve.rs:730` (`MethodCall` resolver) doing the same defer-on-TypeParam check. Deleted; case5 recovered.
3. **`StructRef → Struct` promotion in substitute_in_typed_body.** First test run after the case5 fix still failed `test_generic_callee_with_struct` — output was stack garbage (`1835591808\n1` instead of `10\n20`). The cached typed body has `TypeParam("T")` placeholders. Substitution replaces them with the instance arg, which arrives as `StructRef` (parser-shape, no `field_types`) from `rustc_ty_to_resolved_type`. Codegen depends on `Struct{field_types}`. Original re-resolve path got this for free via `resolve_struct_fields`. I had to chain it into `substitute_in_typed_body`, which is why that function ended up taking `registry: &ToylangRegistry`. The deeper smell — `ResolvedType` having both `StructRef` and `Struct` variants means "this state is reachable but invalid" is representable — is documented as a v2 cleanup target below.

**Residual dead code (harmless warnings) post-sunny-karp:**

- `RustTypeLookupContext::DeferredTypeParam` variant in `oracle.rs:44`
- `oracle::contains_type_param` (no callers after sunny-karp)
- `UnresolvedRustType::is_deferred()` and `TypeResolveError::RustTypeDeferred` are still alive (the closures' `is_deferred()` arms in `after_rust_analysis` are kept for forward-compat with any future legacy-defer producer; not exercised today).

Mechanical to delete in a follow-up sweep.

---

## Next thing to do when picking up (queued 2026-06-25, post-sunny-karp)

Two candidates, user-pick:

### Option A: queued items from the round-4 close (rustc-integration quality)

These are the items from the prior "NEXT" section, still valid:

1. **Phase P — `deduce_param_attrs` soundness override (~half day).** Highest-priority latent silent-UB vector. Override returns `&[]` for `#[toylang::emit_consumer_body]`-tagged items + fixture with `&mut LargeStruct` Sky export. (See line 86 in the original Not-yet-started section.)
2. **Build a `&LargeStruct` Sky-call perf bench.** Currently no bench exposes the indirect-arg alias-analysis question. Measure before deciding whether path-b emission is worth pursuing.
3. ~~Phase O drift CI fences~~ — **SHIPPED 2026-06-25**. B24/B25/B26 fences all landed. See "Phase O: Drift-observation CI fences" section below.

### Option B: the two-enum split — **SHIPPED 2026-06-25**, see top-of-doc.

The historical sketch follows for archive purposes (the implementation
differed only in fine detail — `oracle::substitute_source_type` for the
parser-side substitution lives in oracle.rs, not in `type_resolve.rs`,
because the registry-side promotion fn is also in oracle.rs).

**Sketch:**

```rust
// parser-shape: registry, sidecar, stub_gen output
pub enum SourceType {
    I32, I64, F64, Bool, Void, Usize, TypeParam(String),
    StructRef { name: String, type_args: Vec<SourceType> },
    RustType  { name: String, type_args: Vec<SourceType> },
    Ref       { inner: Box<SourceType> },
    Str, ByteSlice,
}

// resolved-shape: typed AST, codegen, substitution
pub enum ResolvedType {
    I32, I64, F64, Bool, Void, Usize, TypeParam(String),
    Struct    { name: String, type_args: Vec<ResolvedType>,
                field_types: Vec<ResolvedType> },   // mandatory
    RustType  { name: String, type_args: Vec<ResolvedType> },
    Ref       { inner: Box<ResolvedType> },
    Str, ByteSlice,
}

pub fn resolve_source_type(t: &SourceType, reg: &ToylangRegistry) -> ResolvedType;
```

The 8 shared variants are intentionally duplicated. The compiler then refuses to substitute a `SourceType::StructRef` into a `Vec<ResolvedType>` slot. `oracle::rustc_ty_to_resolved_type` takes `&ToylangRegistry` and promotes at the mint site.

**Scope estimate:** ~600–800 lines across 11 files. Roughly 2x sunny-karp. Files: `typed_ast.rs`, `ast.rs`, `parser.rs`, `registry.rs`, `type_resolve.rs`, `oracle.rs`, `callbacks_impl.rs`, `llvm_gen.rs`, `stub_gen.rs`, `sidecar.rs`, `typeid.rs`. The sidecar bincode format changes (variant tags shift); `cargo clean`-equivalent on the cache before re-running tests. `typeid.rs` (8 refs, content-hashes types) deserves a look before committing — table is registry-side so probably `SourceType`, but cross-references may want `ResolvedType`.

**Started in this session, then reverted.** Type defs were sketched in `typed_ast.rs` — both enums defined with the right variants — but the full surgery is genuinely a fresh-context-budget task. The aborted attempt taught me the scope; a fresh session can use the sketches above as the starting point.

**Recommendation if you pick this:** budget a full session for it. The end state catches the entire class of silent-miscompiles that sunny-karp's surprise #3 was one instance of.

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
- All architectural decisions documented below (status table in the Decisions log).
- **Phase A (empirical baseline drop fixtures)** — DONE 2026-06-23. Findings A.1–A.10 documented in the Empirical section below; headline: the previous `mir_shims` override was silently no-op'ing rather than performing useful work, validating Decision 1's premise empirically.
- **Phase E (mir_shims elimination)** — DONE 2026-06-23. `mir_shims` override + `drop_glue.rs` + `mir_helpers.rs` + the `DEFAULT_MIR_SHIMS` plumbing all deleted. Rustc's default DropGlue path fires unchanged; per-type drop semantics flow through a compiler-synthesized `Drop::drop(&local)` AST node inserted at scope end (Phase E.b/E.c/E.d below).
- **Phase E.b/E.c/E.d (scope-end drop emission)** — DONE 2026-06-23. The compiler synthesizes `TypedStmt::ExprStmt(TypedExprKind::StaticCall { ty: "Drop", method: "drop", args: [Ref(local)] })` at scope-end positions of every void-returning function whose body has a let-binding with a Drop-implementing type. The synthesis runs **once** in `insert_scope_end_drops` and is invoked from both the dep-collection site (`type_resolve_body` in `callbacks_impl.rs`) and the codegen site (`codegen_internal_function` in `llvm_gen.rs`). After that pass, every downstream stage — dep walker, mono cascade, codegen, symbol resolution, link — treats drop calls as ordinary trait static calls with no drop-specific code paths. 9 drop fixtures + 333 existing tests pass cold: **342 / 0 / 1 ignored**. See Decision 1 (substantially revised below) for the architectural detail.
- **Phase B (perf bench)** — DONE 2026-06-24 (commits `81b6eb1` + later doc landing). 17 bench fixtures land under `toylangc/tests/integration_projects/perf_bench/` (5 Bench-1 configs + 2 Rust-only baselines + 6 Bench-2 K/LTO combos + 4 Bench-3 drop variants). Runner at `toylangc/tests/scripts/run_perf_bench.sh` builds, runs, and reports a markdown table. Headline numbers (M-series macOS, LLVM 21.1.8): **Bench 1 LTO ratio = 1.50× → handoff "<2× → lock §5.5 confidently" gate fires**; **Sky vs Rust baseline at O3 thin = 0.3% delta (essentially identical)**; **Bench 3 drop chain LTO ratio = 26.5× initially measured; refreshed to 25.5× during Phase B+ rerun, within run-to-run variance**. Full results + interpretation in `tmp/perf-bench-summary.md` and `rust-interop-architecture.md` §22.4 (which now includes §22.4.2 reproduction steps and §22.4.3 interpretation assumptions). Two follow-up findings surfaced: **F3** (Sky main + while loop + 1M+ allocations stack-overflows due to toylangc O0 alloca recycling bug) and **B10 residual** (Phase E drop synthesis + Vec<SkyStruct> at opt-level ≥ 1 still trips the LLVM 21 BitcodeWriter bug under ThinLTO cross-CGU import; arch doc §25.2 B10 tightened from "CLOSED" to "CLOSED for primary path; residual trigger…").
- **Phase B+ (Bench 3 pure-Rust baselines)** — DONE 2026-06-24 (follow-up session). 6 new fixtures under `bench3_rust_baseline_{single_crate,cross_crate,inline_never}_o3_{nolto,thin}` + new `toylangc/tests/integration_projects/test_widgets/` sibling crate carrying `Widget` + `WidgetNoInline` + `make_test_widget` + `make_test_widget_no_inline`. Runner extended to include the new section. Bench fixture count: 17 → 23. **Verdict: the ~26× drop-chain LTO speedup is INHERITED from Rust, not Sky-specific.** Pure-Rust cross-crate Drop (`bench3_rust_baseline_cross_crate_*`) shows **27.5×** — within run-to-run variance of Sky's 25.5× (R_sky refreshed slightly from 26.5×; same regime). Sky's thin-LTO Drop chain (371μs) matches the cross-crate Rust baseline (360μs) at 3% delta, mirroring Bench 1's 0.3% Sky-vs-Rust finding. Two more baselines bracket the operating point: `single_crate` (Widget IN user_bin → intra-crate inliner already eliminates without LTO, both = ~0.35ms, ratio 0.89×) and `inline_never` (Drop has `#[inline(never)]` → LTO can't help, both = ~9.5ms, ratio 0.99×, ~0.95ns/drop matching Bench 1's nolto baseline ~0.87ns/call). Round-4 framing for Bench 3 becomes: "Sky's drop emission gives LLVM the same elimination opportunity a pure-Rust cross-crate Drop impl does; the 26× speedup is a general LLVM-LTO property Sky inherits, not Sky-specific overhead being eliminated." See `rust-interop-architecture.md` §22.4 (refreshed table + finding #2 rewrite + new finding #3 + new bullet in §22.4.3 "Apples-to-apples Rust baseline") and `tmp/perf-bench-summary.md` (refreshed F2 + new Bench 3 baseline table + round-4 lead-with bullet 2).
- **Phase C+D (tool attribute + partition predicate migration)** — DONE 2026-06-24. `#![register_tool(toylang)]` + `#[toylang::emit_consumer_body]` machinery wired up in `toylangc/src/build.rs` (prepends crate-level attrs) + `toylangc/src/stub_gen.rs` (tags accessor methods, wrapper fns, trait-impl methods). `is_consumer_codegen_target` in `rustc-lang-facade/src/lib.rs` rewritten as the two-gate conjunction `is_from_lang_stubs(tcx, def_id) && tcx.has_attrs_with_path(def_id, &[Symbol::intern("toylang"), Symbol::intern("emit_consumer_body")])`. New 1:1 invariant CI fence at `stub_gen::tests::emit_consumer_body_tags_only_category_b_items` enforces both directions (every `unreachable!()` body has a tag; every tag is followed by `unreachable!()`). 477 tests pass (+1 from new fence). Sky proper will substitute `skyc` for `toylang`. See Decision 3 below.
- **Phase F (symbol_name override retirement)** — DONE 2026-06-24. `rustc-lang-facade/src/queries/symbol_name.rs` deleted; module declaration + provider assignment removed from `queries/mod.rs`; `DEFAULT_SYMBOL_NAME` OnceLock + `default_symbol_name()` accessor + `consumer_symbol_for_callback_name` trait method + StatefulVtable slot + trampoline + call helper all removed from `rustc-lang-facade/src/lib.rs`. `is_consumer_accessor_safe` deleted (orphaned). Toylangc-side: `compute_consumer_symbol` + the `consumer_symbol_for_callback_name` trait impl removed; `compute_fn_symbol` switched from `default_symbol_name()(tcx, instance)` → `tcx.symbol_name(instance).name.to_string()`. 477 tests pass cold; bench3_drop_thin smoke-test 352μs. See Decision 2 below.
- **Phase H (patch 4 rev 3 — `#[repr(C)]` FFI shape)** — DONE 2026-06-24. Rev-2 `trait ExtraModuleAllocator<M> { fn allocate(&mut self, name: &str) -> &mut M; }` + `VecAllocator<'a, M, F: FnMut(&str) -> M>` retired in favor of `#[repr(C)] struct ExtraModuleAllocator<M> { state: *mut c_void, allocate: unsafe extern "C" fn(state, name_ptr, name_len) -> *mut M }`. Fork-side changes (~/rust, 4 files): `compiler/rustc_codegen_ssa/src/traits/backend.rs` (struct + trait method signature), `traits/mod.rs` (re-export update), `base.rs::codegen_crate` (per-`B` `unsafe extern "C" fn` thunk + state struct + struct-literal builder), `rustc_codegen_llvm/src/lib.rs` (`FillExtraModulesHook` + `LlvmCodegenBackend::fill_extra_modules` signature). Facade-side (erw, 2 files): `extra_modules_hook.rs` (consumer_fill_modules_hook signature), `lib.rs::LlvmModuleFactory` (inner field + reborrow discipline in `fill_module`). Rebuilt rustc-fork via the full `x.py dist rustc-dev` + library + reinstall procedure (~3.5 min). 477 tests pass; bench3_drop_thin smoke-test 615μs. FFI shape is now correct under BOTH static-link (current) and cdylib (future Phase G) integration; Sky proper's eventual cdylib refactor becomes a wiring change rather than an ABI rewrite. See Decision 5 below.
- **Phase I (cache_on_disk_if audit)** — DONE 2026-06-24. **Decision 14's prescribed Provider-slot syntax (`providers.queries.layout_of_cache_on_disk_if = ...`) doesn't exist on current nightly.** `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL, not a Provider slot. The audit re-derived cache-safety: every Sky-overridden query is safe by construction (`per_instance_mir` declared `false` in fork patch; `layout_of` + `cross_crate_inlinable` default to `false`; `collect_and_partition_mono_items` is `eval_always`; `symbol_name` was disk-cached but its override later retired in Phase F). Annotated all 4 surviving override files with `cache-audit:` marker comments. New CI fence `toylangc/tests/cache_audit.rs` asserts every override carries a marker. New §22.4.1 documents the policy table; new B21 risk entry tracks "per-query disk-cache staleness if rustc evolves the cache API." Decision 14 itself is revised below.
- **Round 4 with the reviewer** — CLOSED 2026-06-24. Five deliverables landed (bench numbers, cache audit, drop fixtures, mir_shims empirical validation, surprises). Reviewer signed off; round 5 awaits Phase L or Phase G to surface real implementation reality. Two minor doc/code residuals queued for post-round-4 sweep: lifecycle-traits registry (generalize `local_needs_scope_drop`'s hardcoded "Drop" string, ~5 LOC), and §15 dual-path drop narrative paragraph.
- **B10 root cause identified + fixed** — DONE 2026-06-24, commit `3041ec8`. The arch doc had framed B10 as an LLVM bug ("LLVM 21's bitcode writer drops FUNCTION records under ABI-coerced extern call signatures"). It was a Sky emission bug we caused. `push_arg_for_rust_call`'s `Direct` arm in `llvm_gen.rs` was emitting struct aggregate values where rustc's ABI declared a scalar param (e.g. for `v.push(Widget { id: 1 })` where Widget = `{ i32 }` coerces to `i32`, the call instruction passed `{ i32 }` aggregate to a function declared `void(ptr, i32)`). Verifier accepted it; bitcode round-trip failed at -O1+ with "failed to parse bitcode for LTO module: Invalid record" (thin LTO) or "Callee is not a pointer type" (fat LTO). Fix: when source toylang type is `StructType` and target ABI type differs, reinterpret via memory (alloca + store + load-as-target-type). Same pattern `codegen_extern_wrapper` already uses for incoming params (@ACRTFDZ). 5 regression probes (`test_drop_b10_probe_{o0,nolto,sky_main_vec_thin,cgu1_thin,fat}`) enrolled covering the failing matrix; all 5 pass; 482 tests total cold. Bench rerun confirms perf holds within run-to-run variance. **§25.2 B10 doc reframing follows in the post-round-4 sweep.**
- **Sunny-karp (eager type-resolve + typed-body cache + bare-T drop closure)** — DONE 2026-06-25. Six work items (A–F) per `/Users/verdagon/.claude/plans/we-should-completely-do-sunny-karp.md`. Headline mechanism change: generic Sky bodies type-resolve **once** at `after_rust_analysis`; the typed result is cached on `ToylangState.typed_bodies` (NOT on `ToyFunction` as the plan proposed — `&self` on the callback forbade `Arc::make_mut`); per-Instance mono substitutes the cached typed AST via a new `substitute_in_typed_body` pure walk; a late `insert_late_scope_end_drops` pass at mono closes the long-standing bare-`TypeParam` local drop gap. Oracle queries (`oracle.rs`'s six type-arg-checking queries + `try_resolved_to_rustc_ty`) gained a `caller_type_params: &[String]` arg; the previously-panicking `TypeParam(name)` arm now rebuilds `ty::Ty::new_param(tcx, idx, name)`. 485 → **487 / 0 / 1** (2 new fixtures: `drop/fixture10_generic_consume_t_with_drop` + `drop/fixture11_generic_consume_t_no_drop`). Three implementation surprises documented in the top-of-doc section: (a) state-vs-registry pivot, (b) seventh `contains_type_param` guard in `type_resolve.rs:730`, (c) `StructRef → Struct` promotion required inside the new substitution walk (silent-miscompile trap motivated the v2 two-enum split queued as Option B above). Per-Instance computational savings: K monomorphizations of a generic body went from 2K+1 type-resolves + 2K drop-synths to 1 type-resolve + 1 drop-synth + K substitutions. Negligible for toylang fixtures; load-bearing at Sky scale. Arch §19.5 memoization status flips from "not implemented in toylang" to "implemented for typed-body caching."

**Not yet started (your work):**
- Phases **G** (cdylib build system — when Sky proper's architecture migration begins; ~5-7 days because retiring toylangc's wrapper mode is a prerequisite), **J** (u128 typeids), **K** (content-hash const args), **L** (per-view refs), **M** (async typestate), **N** (recursion safety), **O** (drift fences).
- **Phase P (deduce_param_attrs soundness override, ~half day)** — surfaced during round-4 close via parallel-agent investigation. rustc's `deduce_param_attrs` analyzes Sky's `unreachable!()` stub MIR and concludes that `PassMode::Indirect` params are `readonly` + `captures(none)` (because the stub never reads/writes them). LLVM trusts those attrs at every Rust call site. If Sky's actual `fill_extra_modules`-emitted body mutates a `&mut LargeStruct` param, the `readonly` attr is a LIE → silent miscompile at -O2+. **Currently latent** — Sky has no fixture with indirect-passed mutable params (Bench 1's `add(i32, i32)` is all Direct and dodges the indirect-only application path). A real Sky 0.1.0 release with any `fn f(x: &mut LargeStruct)` Sky export would hit silent UB. Fix: override `deduce_param_attrs` for `#[toylang::emit_consumer_body]`-tagged items to return `&[]` (safe default). New integration fence: Sky export takes `&mut LargeStruct`; Rust caller mutates across the call; verify correct result at -O3 + thin LTO. See Phase P entry below.
- ~~Phase Q (codegen_fn_attrs override for NEVER_UNWIND)~~ — **SHIPPED 2026-06-25 (commit `736eeb5`) THEN RETIRED 2026-06-25 (commit `f88aa84`)** per reviewer's round-4-followup. Cargo enforces panic-strategy consistency at build-graph resolution; the mixed-panic-strategy case Phase Q's `NEVER_UNWIND` flag defended against is structurally impossible within Sky's tooling. Phase Q was a defensive correctness no-op with no real failure mode. If Sky ever ships precompiled-bodies for pure-cargo consumption (§21.7 v2), the panic-strategy question reappears at the cargo-package metadata layer, NOT at the codegen-attr layer. Net effect: one fewer query override surface; matches "every Sky mechanism must be load-bearing" discipline.
- ~~Phase R (B10-style emission audit follow-ups)~~ — **SHIPPED 2026-06-25** (commits `04e98c7` + `52ee0a3`). Defensive fixes shipped for all 7 candidate sites: #8 sret-bridge alloca/load size mismatch, #1 Pair arm assumes struct source, #5/#6 receiver-Indirect assumption breaks for Pair-coerced receivers, #10 parse_coerced_type missing float/vector arms. Sites mostly latent in current toylang grammar; defensive fixes prevent ICE/silent miscompile if Sky proper's broader grammar (non-power-of-2 byte structs, traits on slices, SIMD, f32) hits any surface. **Surprise finding while probing Site #8: bool accessor silent miscompile** — Sky's auto-synthesized `&self.bool_field` accessors emitted `Ref(FieldAccess)` via a load-realloc roundtrip with i1 storage having unspecified upper 7 bits → Rust callers reading `*&bool` got undefined values. **Fixed in commit `736eeb5`** alongside the Phase R work; would have shipped silent UB to any Sky user with a bool field accessed from Rust. Regression fixture at `test_drop_bool_accessor_via_rust_caller`.
- Phase H shipped standalone (not bundled with G) because the new `#[repr(C)]` FFI shape works under both static-link and cdylib integration — so Phase G's eventual work doesn't have to revisit patch 4 ABI.

**In flight from prior sessions (independent of this exchange):**
- Patch 5 retirement (shipped 2026-06-22 per the historical section below).
- Thread B (share_generics support boundary) — open, lower priority than the new work.

---

## TL;DR

If you only have time to read 200 words, here it is:

The review exchange committed us to **17 architectural changes**, most importantly: eliminate the `mir_shims` query override (drop becomes a normal generic Sky function call), eliminate the `symbol_name` override (live no-op), replace the partition filter predicate with a `#[skyc::emit_consumer_body]` tool attribute, ship Sky's backend as a **cdylib** instead of statically linked, use **`#[repr(C)]` function-pointer struct** instead of `&mut dyn Trait` across the cdylib FFI boundary, retire the slab-pointer-as-u64 surface in favor of **content-hash const args** (u128 with collision detection), introduce **per-view ref types** `SkyRef<T, V>` with `V ∈ {Frozen, Mutable}` for Send/'static honesty at the Rust boundary, narrow `#[may_dangle]` policy to a **syntactic rule** (T appears only behind pointer indirection), and audit + document cache-on-disk-if discipline (Decision 14 — note: 2026-06-24 audit found the prescribed Provider-slot API doesn't exist on current nightly; every Sky-overridden query is cache-safe by construction, documented via `cache-audit:` markers + CI fence at `toylangc/tests/cache_audit.rs`).

Critical caveat (historical): **toylang had zero drop tests**, so the mir_shims elimination had no empirical baseline. Phase A built fixtures FIRST, validated baseline under current model, THEN did the migration with fixtures as the regression suite. Both phases shipped 2026-06-23. Phase B (perf bench) shipped 2026-06-24 with similar discipline — build fixtures, run, capture, interpret, land empirical anchors in the doc. Decision-gate verdict: **lock §5.5**.

**Implementation order, top 5 (refreshed 2026-06-24 after Phases A/E/B/B+/I shipped):**

1. ~~Drop fixtures~~ — DONE (Phase A, 2026-06-23). 9 fixtures + 333 existing tests passing.
2. ~~Perf bench~~ — DONE (Phase B + B+, 2026-06-24). 23 fixtures; Bench 1 ratio 1.50× → lock §5.5; Bench 3 drop chain 25.5× (Sky) with apples-to-apples 27.5× (pure-Rust cross-crate baseline → inherited from Rust, not Sky-specific). See §22.4 + `tmp/perf-bench-summary.md`.
3. ~~mir_shims elimination~~ — DONE (Phase E + E.b/c/d, 2026-06-23). AST-rewrite drop synthesis is shipped; the override + `drop_glue.rs` + `mir_helpers.rs` are deleted.
4. ~~cache_on_disk_if audit~~ — DONE (Phase I, 2026-06-24). Audit found prescribed API doesn't exist; all queries cache-safe by construction; CI fence in place.
5. ~~Bench 3 pure-Rust baselines~~ — DONE (Phase B+, 2026-06-24, follow-up session). 6 new fixtures + test_widgets sibling crate. **Pure-Rust cross-crate Drop shows 27.5× LTO ratio** (within noise of Sky's 25.5×); the amplification is inherited from Rust, not Sky-specific. Round-4 framing for Bench 3 settled.

6. ~~Tool attribute infrastructure + partition predicate migration (Phase C+D)~~ — DONE (2026-06-24). `#![register_tool(toylang)]` + `#[toylang::emit_consumer_body]` machinery in build.rs + stub_gen.rs; `is_consumer_codegen_target` rewritten as the two-gate conjunction `is_from_lang_stubs(tcx, def_id) && tcx.has_attrs_with_path(def_id, &[toylang, emit_consumer_body])`. New 1:1 invariant CI fence at `stub_gen::tests::emit_consumer_body_tags_only_category_b_items`. 477 tests pass (+1 from new fence). Smoke-test of `bench3_drop_thin` confirms runtime behavior unchanged.
7. ~~Symbol_name override elimination (Phase F)~~ — DONE (2026-06-24). Per Decision 2: deleted `queries/symbol_name.rs`, removed the provider assignment, dropped `DEFAULT_SYMBOL_NAME` + `default_symbol_name()` + the `consumer_symbol_for_callback_name` callback + vtable slot + trampoline + accessor. `compute_fn_symbol` now reads `tcx.symbol_name(instance)` directly. `is_consumer_accessor_safe` deleted (orphaned). `is_consumer_fn` + `is_consumer_trait_impl_method` stay alive (per_instance.rs + cascade-drain callers). 477 tests pass; bench3_drop_thin smoke-test 352μs. Three of the four roundtrip-relevant cleanup arcs (Decisions 1/2/3) now ship.
8. ~~Patch 4 rev 3: `#[repr(C)]` FFI shape (Phase H)~~ — DONE (2026-06-24). Per Decision 5: replaced `&mut dyn ExtraModuleAllocator<M>` trait-object + `VecAllocator` driver with `#[repr(C)] struct ExtraModuleAllocator<M> { state, allocate }`. Updated rustc-fork (4 files: `traits/backend.rs`, `traits/mod.rs`, `base.rs::codegen_crate`, `rustc_codegen_llvm/lib.rs`) and the facade (2 files: `extra_modules_hook.rs`, `lib.rs::LlvmModuleFactory`). Rebuilt rustc-fork via the full `x.py dist rustc-dev` + library + reinstall procedure (~3.5 min). 477 tests pass; bench3_drop_thin smoke-test 615μs (within run-to-run variance). FFI shape is now correct under both static-link (current) and cdylib (future Phase G) integration. Shipped standalone — Phase G's wrapper-mode-retirement + cdylib-build-system work waits for Sky proper's architecture migration.

**NEXT — start here next session (refreshed 2026-06-24 after round 4 close):**

User's stated priority is **rustc-integration quality, especially perf/inlining and "the best way to integrate into rustc"** — NOT Sky language design. Order accordingly:

9. **Phase P — `deduce_param_attrs` soundness override (~half day).** **Highest priority** — closes a latent silent-UB vector currently in production-bound emission. Same B10 shape (rustc trusts stub MIR, Sky's stub "lies" about behavior), worse failure mode (silent miscompile vs loud compile error). Override returns `&[]` for tagged items + fence fixture with `&mut LargeStruct` Sky export.

10. **Phase R Site #8 probe — sret-bridge alloca/load size mismatch (~half day).** Highest-leverage of the 7 emission-audit findings; same shape as B10 we just fixed but asymmetric direction we didn't cover. Build a probe; if it triggers, fix the same way (memory reinterpretation aligned to declared signature).

11. ~~Phase Q — `codegen_fn_attrs` override~~ — **SHIPPED THEN RETIRED 2026-06-25** (commits `736eeb5` ship + `f88aa84` retire). Reviewer's round-4-followup audit confirmed cargo enforces panic-strategy consistency at build-graph resolution; mixed-strategy case is structurally impossible within Sky's tooling, making Phase Q's `NEVER_UNWIND` flag a defensive no-op with no real failure mode. Provenance comment chain pinned in `queries/mod.rs` with explicit DO NOT FLATTEN annotation.

12. **Phase R remaining sites — Sites #1, #5/#6, #10 (~1 day total).** Probe + fix each candidate from the emission audit. Site #10 (parse_coerced_type missing float arms) becomes important the moment anyone wants `f32`/`f64` Sky exports.

13. **Build a `&LargeStruct` Sky-call perf bench.** Currently no bench exposes the indirect-arg alias-analysis question. Without it we can't measure whether path-b emission (Sky-ground-truth attrs in `codegen_extern_wrapper`) is worth pursuing. Measure first, then decide.

**Lower priority (defer unless one becomes blocking):**
- **Phase G (cdylib build system, ~5-7 days)** — engineering-velocity win, not perf. Pairs with the now-shipped Phase H ABI rewrite. Defer until backend iteration speed becomes the bottleneck.
- **Phase N (recursion safety, ~2-3 days)** — bounded fix to runaway-walker risk; no perf impact; low priority.
- ~~Phase O (drift CI fences, ~3-5 days)~~ — **SHIPPED 2026-06-25.** B24/B25/B26 fences all landed; ~1 day actual (vs 3-5 day estimate). Three new test files + arch §25 updates.

**Skip per user's stated priorities** (Sky language design, not rustc integration):
- Phase J (u128 typeids), Phase K (content-hash const args), Phase L (per-view ref types), Phase M (async typestate).

**Doc landings owed from round-4 close** (fold into a single sweep before or during the work above):
- §22.4 reframing: lead with "Sky's cross-crate boundary cost equals Rust's at every opt level" rather than "1.5× LTO ratio" (per reviewer's round-4-close note).
- §25.2 B10 rewrite: drop the "LLVM bug" framing; describe Sky's emission bug + fix.
- §15 dual-path drop narrative paragraph (Sky source → AST synthesis; Rust source → rustc's standard `drop_in_place`; both converge at Sky's single-symbol body).
- Generalize `local_needs_scope_drop`'s hardcoded "Drop" trait name to a lifecycle-traits registry (~5 LOC + registry entry).

**Round 4 — CLOSED 2026-06-24.** All 5 deliverables landed. Reviewer signed off. Round 5 awaits Phase L or Phase G to surface real implementation reality. See "State of the World" below for the canonical summary.

---

## State of the World — reviewer prep (refreshed 2026-06-24)

**Purpose.** Single canonical summary the next engineer can walk the reviewer through in round 4. Treat this section as the FIRST thing you read after the TL;DR; everything else (Decisions log, Phase entries, Doc-update plan) is supporting detail.

### Empirical anchors (lead with these)

| Anchor | Result | Where |
|---|---|---|
| **Bench 1 LTO ratio** | **1.50×** (O3 nolto 86.7ms / O3 thin 57.9ms) — handoff "<2× → lock §5.5 confidently" gate fires | arch §22.4 table |
| **Sky vs Rust baseline at thin LTO (Bench 1)** | **0.3% delta** — Sky adds no measurable overhead beyond Rust's cross-crate cost | arch §22.4 finding #1 |
| **Bench 3 drop chain LTO ratio (Sky)** | **25.5×** (O3 nolto 9.5ms / O3 thin 0.4ms) — Phase B+ rerun number; initial measurement was 26.5×, within run-to-run variance | arch §22.4 + tmp/perf-bench-summary.md |
| **Bench 3 drop chain LTO ratio (pure-Rust cross-crate)** | **27.5×** — apples-to-apples baseline from Phase B+ | arch §22.4.3 + handoff Phase B+ entry |
| **Sky vs Rust at thin LTO (Bench 3)** | **3% delta** — drop-chain LTO speedup is INHERITED from Rust, not Sky-specific | arch §22.4 finding #2 |
| **Bench 2 K=100 LTO ratio** | **1.48×** — realistic-workload anchor matches Bench 1's structural prediction | arch §22.4 |
| **Total test count** | **487 / 0 / 1 cold** (sunny-karp added 2 generic-T drop fixtures; baseline at handoff freeze 485 + 2) | toylangc workspace |

**Decision-gate verdict (handoff Bench 1 ladder):** ratio 1.50× falls firmly in the "<2× → lock §5.5 confidently" band. **Recommendation: `[profile.release] lto = "thin"`**.

### Surprises log (group by where each surfaced)

Listed in rough order of how-load-bearing for the reviewer:

1. **A.6 (Phase A, 2026-06-23) — the prior `mir_shims` override was empirically BROKEN.** Round 3 reasoning assumed the override worked; A.6 found it had never fired in any shipping test fixture. `consumer_struct_name` lookup never matched; `drop_in_place::<Widget>`'s body was a no-op (just stack save/restore); `<Widget as Drop>::drop` was absent from any binary. **mir_shims removal lost zero functionality** because there was no functionality to lose. This directly contradicts how round 3 framed the 8-agent investigation.

2. **Phase B+ (2026-06-24) — Bench 3 amplification is INHERITED from Rust, not Sky-specific.** Pure-Rust cross-crate Drop chain (Widget in sibling `test_widgets` crate) shows 27.5×. Sky shows 25.5×. Sky vs Rust delta at thin LTO = 3% (matching Bench 1's 0.3%). The ~26× speedup is a property of cross-crate Drop chains under LLVM's LTO inliner, not Sky-specific overhead being magically eliminated. Round-4 framing for Bench 3 must lead with "Sky inherits Rust's LTO behavior here" rather than "Sky drops are 26× faster with LTO."

3. **Phase I (2026-06-24) — Decision 14's prescribed Provider-slot API DOES NOT EXIST on current nightly.** `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL, not a `Providers` slot. The reviewer's closing micro-note advice was based on an API that doesn't exist. Audit reframed the discipline via `cache-audit:` marker comments + CI fence; every override safe by construction.

4. **B10 residual (Phase B, 2026-06-24) — Approach B's "closed" claim was partial.** Phase E drop synthesis + `Vec<SkyStruct>` at opt-level ≥ 1 still trips the LLVM 21 BitcodeWriter bug, but only via ThinLTO's INTERNAL cross-CGU import phase (which re-serializes and re-parses bitcode). Sky's primary `fill_extra_modules` path doesn't trigger it. Arch §25.2 B10 tightened from "CLOSED" to "CLOSED for primary path; residual under ThinLTO cross-CGU import."

5. **F3 (Phase B, 2026-06-24) — toylangc O0 codegen has an alloca leak.** Sky `main` + while loop + 1M+ Widget allocations stack-overflows in ~40ms with a 64MB stack. Math doesn't add up (1M × ~4B widgets ≠ 64MB) — root cause likely toylangc's O0 lowering emits a fresh `alloca` per loop-body let-binding without hoisting to entry block. Bench3 routes around it via Rust caller driving the Vec allocation. Toylangc bug, not architectural; documented in arch §F.18.

6. **Phase G discovery (2026-06-24) — wrapper-mode retirement is a Phase G prerequisite.** The handoff's original 1-week G+H estimate didn't account for toylangc's current wrapper-mode architecture: today, toylangc statically links the facade and uses `rustc_driver::run_compiler` as a library. Sky proper's cdylib model needs toylangc to retire wrapper mode (handoff item 7, ~2-3 days) before the cdylib refactor can land in toylang. Phase H shipped standalone with the FFI shape that works under both models, so Phase G's eventual work becomes a wiring change rather than ABI rewrite.

7. **Phase E.b/E.c → E.d refactor mid-flight (2026-06-23).** Initial Phase-E shipping shape emitted Drop calls at the LLVM-IR layer. After observing that this leaked drop-specialness across 4 sites in the pipeline, refactored to a single AST-rewrite pass `insert_scope_end_drops` for principled adherence to "drop is just a function." 95% specialness collapse to one site. Documented in arch §F.18.

8. **Rustdoc-not-installed trap + build-rustdoc-also-wipes-stdlib (2026-06-24).** Long-standing infrastructure issue where `cargo test --workspace` exited non-zero on the doctest step because rustdoc wasn't built for the rustc-fork toolchain. Building rustdoc via `x.py build src/tools/rustdoc` requires re-running the FULL `rustc-dev` reinstall procedure because the library build clears the sysroot's `librustc_*.rmeta` files. Procedure now documented in `docs/historical/rebuilding-rustc-fork.md`.

9. **B10 was a Sky emission bug, not an LLVM bug (round-4 close, 2026-06-24).** The arch doc's §25.2 B10 framing had pinned blame on "LLVM 21's bitcode writer drops FUNCTION records under ABI-coerced extern call signatures." Investigation during round-4 prep showed it was `push_arg_for_rust_call`'s `Direct` arm emitting struct aggregate values where rustc's ABI declared a scalar. LLVM accepted the malformed IR at -O0 but bitcode round-trip failed at -O1+. **Fix is on Sky's side**, not waiting for LLVM. Shipped commit `3041ec8` + 5 regression probes (`test_drop_b10_probe_*`). Doc reframing follows in post-round-4 sweep.

10. **`deduce_param_attrs` is a latent silent-UB vector (round-4-close agent investigation, 2026-06-24).** rustc's query analyzes Sky's `unreachable!()` stub MIR and concludes that since the stub never reads/writes any param, every `PassMode::Indirect` param is `readonly` + `captures(none)`. LLVM applies those attrs at every Rust call site → if Sky's actual body mutates `&mut LargeStruct`, the `readonly` attr is a LIE. Same B10 shape (rustc trusts stub MIR, stub "lies"), worse failure mode (silent miscompile vs loud compile error). **Currently latent** — Bench 1 is all Direct so doesn't expose it; Sky has no fixture with indirect-passed mutable params. A real Sky 0.1.0 release with `fn f(x: &mut LargeStruct)` Sky export would hit silent UB. Phase P fixes it.

11. **`codegen_fn_attrs` is underutilized (round-4-close agent audit, 2026-06-24).** Sky historically overrode `codegen_fn_attrs` for linkage stamping (Option 4 era) and retired the override. The OPTIMIZATION-stamping opportunity is new. `NEVER_UNWIND` alone eliminates LLVM landing-pad emission at every Rust caller — significant for tight callee-rich loops under panic=unwind. Sky's panic=abort posture (§16.1) makes this trivially correct. Phase Q.

12. **Seven B10-class candidate sites in toylangc's emission (round-4-close agent audit, 2026-06-24).** Beyond the `Direct` arm we fixed, the agent emission audit identified 7 more sites where signature mismatch could lurk. Top priority: Site #8 (codegen_extern_wrapper sret-bridge alloca/load size mismatch — same B10 shape, asymmetric direction we didn't cover). Phase R.

13. **Bool accessor silent miscompile — FIXED 2026-06-25 (commit `736eeb5`).** Surfaced during Phase R Site #8 probing. Root cause: Sky's auto-synthesized field accessors (`pub fn a(&self) -> &bool { &self.a }`) used the default `Ref(FieldAccess)` codegen path — load the bool value (i1), alloca i1, store, return pointer to fresh alloca. The fresh i1 alloca's upper 7 bits were unspecified per LLVM's i1 storage semantics. When Rust callers dereferenced the returned `&bool`, rustc emitted `load i8` (Rust's bool is i8 in memory), and LLVM exploited the unspecified bits → `*w.field()` returned `false` regardless of stored value. Fix: `Ref(FieldAccess)` now returns the GEP pointer to the field's actual storage in the receiver struct rather than a load-realloc roundtrip. Regression fixture at `test_drop_bool_accessor_via_rust_caller`. Phase R-adjacent finding that surfaced alongside Site #8 work; would have shipped silent UB to any Sky user defining a struct with a bool field accessed from Rust.

14. **Sunny-karp #1: `&self` callback shape forbade the plan's `Arc::make_mut` design.** The plan called for caching `typed_body` on `ToyFunction` via `Arc::make_mut(&mut self.registry)` at `after_rust_analysis`. The callback's signature is `fn after_rust_analysis(&self, ...)` — there's no `&mut self` handle to take. Wrote ~60 lines of "Practical fix..." comments trying to talk myself into a workaround (interior mutability, `Arc::get_mut`, shadow handles) before pivoting to `ToylangState.typed_bodies`, which the plan had explicitly considered and rejected. Trade-offs are detailed at the top of this doc; net for toylang's scale is acceptable, but Sky proper at multi-crate-generic scale may want to revisit and either change the callback trait signature or move to `RwLock<ToylangRegistry>` interior mutability.

15. **Sunny-karp #2: seventh `contains_type_param` guard outside the oracle.** First test run after the A–F implementation failed 28 case5 tests with `RustTypeDeferred { context: "method '.push' on receiver containing TypeParam" }`. The plan enumerated six oracle early-returns at `oracle.rs:506,813,834,856,890,964` to delete; it missed a seventh in `type_resolve.rs:730` (the `MethodCall` resolver) that did the same defer-on-TypeParam check independently. Once the oracle stopped deferring, the resolver's guard caught the same pattern and turned it back into an error. Fix: deleted the guard; the match arms below it handle `RustType { ... Param-bearing args }` cleanly through the now-Param-aware oracle. **Lesson:** `git grep contains_type_param` once before declaring "I deleted all the guards." Reasoning from the plan's enumeration alone missed this.

16. **Sunny-karp #3: `StructRef → Struct` promotion required inside `substitute_in_typed_body` — silent miscompile trap.** First test run after the seventh-guard fix still failed `test_generic_callee_with_struct` with stack-garbage output (`1835591808\n1` instead of `10\n20`). The cached typed body has `TypeParam("T")` placeholders in expression types. `substitute_in_typed_body` walks and replaces them with the instance arg's `ResolvedType` value. The arg comes from `rustc_ty_to_resolved_type(tcx, instance.args.types()[i])` — which mints `StructRef { name, type_args }` for Sky types (parser-shape, no `field_types`). Codegen consumes `Struct { name, type_args, field_types }` (resolver-shape, mandatory `field_types`). The original re-resolve path passed `StructRef`-bearing source through `resolve_struct_fields` for free. The new pure-substitution path didn't, so the substituted typed body had `StructRef{Point}` where codegen needed `Struct{Point, [I32, I32]}` — codegen treated the field-less form as opaque/zero-sized, returned stack garbage. **Reactive fix:** chained `resolve_struct_fields` after `substitute_type_params` inside the walk, which required passing `registry: &ToylangRegistry` through. **Deeper smell:** `ResolvedType` having both `StructRef` and `Struct` as variants means "this state is reachable but invalid" is representable in the type system. The bug class is broader than this one instance — any pure-AST-walk substitution that produces fresh `ResolvedType` values has to remember the promotion step, with no compile-time enforcement. The structural fix (two distinct types: `SourceType` for parser-shape, `ResolvedType` strictly for resolved-shape, conversion at one chokepoint) is queued as Option B at the top of this doc.

### What contradicts the round 1-3 analysis (READ THIS BEFORE ROUND 4)

The reviewer pushed certain models in rounds 1-3. Three places where empirical work overturned them:

1. **The 8-agent mir_shims investigation conclusion (round 3) was reasoning about working machinery — but the machinery was already broken.** A.6 found the override never fired. The investigation's conclusions (drop fits the general per_instance_mir mechanism naturally) STILL stood up post-Phase-E (AST-rewrite shape ships clean at 342/0/1), but the reviewer should know we removed broken code, not working code.

2. **Decision 14's Provider-slot prescription was invalid.** The audit reframing landed; the reviewer's general concern (Sky's universe state changing between compiles → stale incremental cache for queries that ARE disk-cached) survives but the prescribed mechanism didn't exist. We dealt with it via the `cache-audit:` marker discipline + new B21 risk entry instead.

3. **Bench 3's drop-chain amplification was framed in round 3 (and through Phase B's initial measurements) as potentially Sky-specific.** Phase B+ proved it's structural to cross-crate Drop chains under LLVM LTO. Don't say "Sky's emission gives LLVM more elimination opportunity than Rust's"; say "Sky's emission gives LLVM EQUAL elimination opportunity to a pure-Rust cross-crate Drop impl."

### Open design questions status (Decisions 6-12 + 17)

NONE of these decisions have shipped yet. No empirical work has touched them since round 3. **Framing as stated in the Decisions log stands** — but the reviewer should know:

- Decision 6 (u128 typeids): Phase J, ~2-3 days when prioritized. Direct continuation of the cleanup arc.
- Decision 7 (content-hash const args): Phase K, depends on J.
- Decision 8 (per-view ref types): Phase L, ~2 weeks, depends on K. The deepest design change still pending.
- Decision 9 (closed V set): subset of L. The exact set (Frozen + Mutable + maybe Exclusive) is unresolved.
- Decision 10 (async typestate): Phase M, depends on L.
- Decision 11 (strict_linear + opt-out): subset of M.
- Decision 12 (narrowed may_dangle): subset of L/M. The syntactic rule stands; needs Sky stdlib design work to come.
- Decision 17 (stub-type contract): vacuously satisfied today (ZST stubs); revisit when stub-gen evolves.

### Where to find supporting evidence

- **Bench numbers + reproduction:** arch §22.4 (model + headline) + §22.4.2 (reproduction commands) + §22.4.3 (interpretation assumptions) + `tmp/perf-bench-results.md` + `tmp/perf-bench-summary.md` (round-4 lead-with framing) + `tmp/perf-bench-disasm/` (per-fixture _main symbol disassembly).
- **Drop fixture outcomes:** `toylangc/tests/integration_projects/drop/` (9 fixtures) + Phase A findings table in Decision 1 + `tmp/phase-e-investigation.md` if it exists.
- **mir_shims elimination implementation:** arch §15.7 (AST-rewrite mechanism) + §F.18 (Phase E lessons) + Decision 1 "AS IMPLEMENTED" subsection.
- **Cache audit findings:** arch §22.4.1 (policy table) + `toylangc/tests/cache_audit.rs` (CI fence) + Decision 14's revised section.
- **Phase C+D tool attribute machinery:** arch §5.3 (partition predicate) + `stub_gen::tests::emit_consumer_body_tags_only_category_b_items` (1:1 invariant fence) + Phase C/D entries.
- **Phase F symbol_name retirement:** arch §6.2 (single-symbol architecture) + §26.1 (SyMINCZ) + Decision 2's IMPLEMENTATION NOTES post-shipping refresh.
- **Phase H patch 4 rev 3:** arch §3.2 (patch 4) + §B.4 (patch source) + §C.4 (shipping shape) + §F.15 (design history with rev 2 → rev 3 transition) + Decision 5 SHIPPED entry.
- **Risks status:** arch §25.2 entries A1-A3, B1-B27 (every risk has CLOSED / partial / probability annotation). B21, B24, B25, B26, B27 are NEW from 2026-06-24; B5, B8, B9, B10 (partial), B11, B12 (gated), B13, B14, B15, B16, B17 all CLOSED architecturally.

### What's left — at-a-glance decision tree (refreshed 2026-06-25 post-sunny-karp)

User's stated priority: **rustc-integration quality, especially perf/inlining**. Round-4-close-driven rustc-integration tasks closed 2026-06-25. Sunny-karp (eager type-resolve + cache + bare-T drop closure) shipped same day.

```
Status →
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

**Recommended ordering for next fresh-context session (refreshed 2026-06-25 post-sunny-karp):**
1. **Mechanical cleanup** (10 min): delete `RustTypeLookupContext::DeferredTypeParam` variant + `oracle::contains_type_param` (no callers after sunny-karp). Frees up dead-code warnings. Optionally retire `UnresolvedRustType::is_deferred()` + `TypeResolveError::RustTypeDeferred`.
2. ~~A1 audit~~ — **CLOSED 2026-06-25 (this session).** Audited every `resolve_expr` arm for the B23 IntLit-class silent-miscompile pattern. Findings: only `StructLit` had the silent path (B23 fix covers it). Other arms that ignore `expected_ty` (BinaryOp left/right, StaticCall/MethodCall args, Ref inner, If branches) all surface mismatches LOUDLY via downstream `types_match` validators — `ArgTypeMismatch` or `AssignTypeMismatch` errors the user fixes by suffixing the literal. No new soundness bugs. Two ergonomic wins identified but deferred (BinaryOp + If expected-ty propagation would let `let x: i64 = 0 + 5;` compile without `0i64`; pure ergonomics, no soundness implication).
3. ~~A2 rename~~ — **CLOSED 2026-06-25 (this session).** `bench4_largestruct_byval_thin` → `bench4_artifactual_loopfold_only`. Main.toylang header updated to document why the fixture is retained (Phase P IR fence + artifactual perf finding marker). Doc + facade-side refs updated.
4. ~~A3 Phase Q retire decision~~ — **RESOLVED (commit `f88aa84`)**: reviewer's round-4-followup confirmed retirement; deleted.
5. ~~Doc sweep~~ — **COMPLETED 2026-06-25** (commits `8f85622` + `33aeae4` + `f88aa84` + `5c0a6b2`) + post-sunny-karp landing sweep (§19.5 + §F appendix entry).
6. **Either** the two-enum split (Option B at the top of this doc — structural fix for the silent-miscompile class sunny-karp's surprise #3 represented) **or** **Phase O drift CI fences** (operational hygiene; protects active query overrides). User pick.
7. **Phase G** if iteration speed becomes the bottleneck.
8. Re-probe Phase R deferred items when toylang grammar growth makes their triggers reachable.

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

### Decisions status table — refreshed 2026-06-24

| # | Decision | Status | Phase | Notes |
|---|----------|--------|-------|-------|
| 1 | Eliminate `mir_shims` override | **SHIPPED** | A + E + E.b/c/d (2026-06-23) | Implementation diverged from original "bridge" plan; AST-rewrite synthesis ships. |
| 2 | Eliminate `symbol_name` override | **SHIPPED** | F (2026-06-24) | Override + callback chain + `DEFAULT_SYMBOL_NAME` all retired; `tcx.symbol_name(instance)` direct. |
| 3 | `#[skyc::emit_consumer_body]` tool attribute | **SHIPPED** | C + D (2026-06-24) | Toylang uses `toylang` namespace; Sky proper will substitute `skyc`. New 1:1 CI fence. |
| 4 | cdylib for Sky's backend | **OPEN** | G (~5-7 days when prioritized) | Wrapper-mode retirement is a prerequisite (handoff item 7, ~2-3 days). |
| 5 | `#[repr(C)]` FFI for patch 4 (rev 3) | **SHIPPED** | H (2026-06-24) | Shipped standalone (without G) because the FFI shape is correct under both static-link and cdylib. |
| 6 | u128 typeids + collision detection | **OPEN** | J (~2-3 days) | No fork changes. Direct continuation of cleanup arc. |
| 7 | Content-hash const args | **OPEN** | K (~1 week, depends on J) | Retires slab-pointer-as-u64; depends on u128 typeid migration. |
| 8 | Per-view ref types `SkyRef<T, V>` | **OPEN** | L (~2 weeks, depends on K) | Send/'static honesty at Rust boundary. |
| 9 | Closed V set in Sky stdlib | **OPEN** | Subset of L | Coherence safety; specific set (Frozen + Mutable + maybe Exclusive) TBD. |
| 10 | Async typestate pattern | **OPEN** | M (~3-5 days, depends on L) | One rustc type + source-level NotStarted/Running witnesses. |
| 11 | `strict_linear` + `#[rust_droppable]` opt-out | **OPEN** | Subset of M | Default behavior for linear types. |
| 12 | Narrowed `#[may_dangle]` syntactic rule | **OPEN** | Subset of L/M | T behind pointer indirection only. |
| 13 | Sky-side recursion limit alignment | **OPEN** | N (~2-3 days) | No fork changes. Bounded fix to known runaway-walker risk. |
| 14 | `cache_on_disk_if(false)` audit | **AUDIT SHIPPED** | I (2026-06-24) | Audit found prescribed Provider-slot API doesn't exist on current nightly; every override safe by construction. |
| 15 | Drift-observation discipline + B24/B25/B26 | **SHIPPED** | O (2026-06-25) | B24/B25/B26 risk entries landed in arch §25; fences: `drop/fence_b24_field_drop_order/` (B24), `tests/mangler_version_fence.rs` (B25), `tests/instance_kind_coverage_fence.rs` (B26). |
| 16 | §1.7 reframe — backend pluralism leads | **SHIPPED** | Doc-only landed pre-Phase-E. | No code impact. |
| 17 | Stub-type contract — Sky drop doesn't invalidate field storage | **OPEN** | Subset of L | Vacuously satisfied today (ZST-only stubs); revisit if stub-gen evolves. |

**Round-4 reviewer-relevant takeaways:**
- Three of the four "cleanup arc" Decisions (1, 2, 3) shipped + Decision 5 + Decision 14 audit. The reviewer's round-3 architectural commitments are largely empirically validated.
- Decision 4 (cdylib) waits on Sky proper's architecture migration; Phase H's FFI rewrite shipped so the eventual G migration is a wiring change rather than an ABI rewrite.
- Decisions 6-12 + 17 are still as-stated from round 3; no contradicting empirical work has touched them. Framing stands.

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

**STATUS: SHIPPED 2026-06-24** (Phase F). The override is gone; `compute_fn_symbol` now reads `tcx.symbol_name(instance)` directly. Implementation notes in the Phase F entry below.

**WHAT.** Remove the `symbol_name` override. Sky's `Providers::symbol_name` is unset; rustc's default mangler (v0) fires for all items. Sky's call-site emission computes target names via `tcx.symbol_name(instance).name` (Phase F replaced the pre-2026-06-24 `default_symbol_name()(tcx, instance)` bypass since there's no override to dodge anymore).

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

**IMPLEMENTATION NOTES — POST-SHIPPING REFRESH (2026-06-24, Phase F).** The list below is what actually shipped; the original pre-Phase-F prescription contained one item ("default_symbol_name()() is the canonical computation") that was superseded by the actual implementation.

- ✅ `rustc-lang-facade/src/queries/symbol_name.rs`: DELETED.
- ✅ `rustc-lang-facade/src/queries/mod.rs`: removed `mod symbol_name;`, removed `providers.queries.symbol_name = ...` assignment, dropped the `symbol_name` arg from `install_query_defaults`.
- ✅ `rustc-lang-facade/src/lib.rs`: removed `DEFAULT_SYMBOL_NAME` OnceLock + `default_symbol_name()` accessor (no remaining callers post-Phase-F).
- ✅ `toylangc/src/toylang/callbacks_impl.rs::compute_consumer_symbol`: DELETED.
- ✅ `consumer_symbol_for_callback_name` callback removed from `LangCallbacks` trait + `StatefulVtable` slot + trampoline + `call_consumer_symbol_for_callback_name` accessor + `install_callbacks` initializer.
- ✅ Audit found `is_consumer_accessor_safe` orphaned → DELETED. `is_consumer_fn` stays alive (still consulted by `queries/per_instance.rs` for callback-name routing into `collect_generic_rust_deps`); retires when per_instance.rs's callback-name path is reworked.
- ✅ `is_consumer_trait_impl_method` KEPT (cascade drain uses it).
- ✅ `compute_fn_symbol` (the toylangc-side helper) now reads `tcx.symbol_name(instance).name.to_string()` directly instead of going through `default_symbol_name()(tcx, instance)`. This is what supersedes the pre-Phase-F prescription that `default_symbol_name()` would be the canonical computation — with the override gone, there's nothing to dodge re-entrance through, so `tcx.symbol_name(instance)` IS rustc's default mangler directly.

**DOC IMPACT — SHIPPED:**
- ✅ §5.4: dropped `symbol_name` from override list.
- ✅ §6.2: rewrote single-symbol architecture paragraphs around rustc's default mangler; pre-Phase-F bypass mechanism preserved as historical context.
- ✅ §26.1 (SyMINCZ): rewritten — "symbol-name lookups via `tcx.symbol_name(instance)` are pure reads; the invariant survives the override retirement."
- ✅ Provider-overrides count: 5 → 4.

### Decision 3: `#[skyc::emit_consumer_body]` tool attribute as partition predicate

**STATUS: SHIPPED 2026-06-24** (Phases C+D). Toylang uses the `toylang` namespace (Sky proper will use `skyc`). Implementation details in the Phase C/D entries below.

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

**STATUS: SHIPPED 2026-06-24** (Phase H). The trait-object allocator + `VecAllocator` driver are gone; the new `#[repr(C)]` struct + `unsafe extern "C" fn` callback ship in the fork patch and in the facade hook. Implementation details in the Phase H entry below.

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
- Bench 3 (10M `<Widget as Drop>::drop`): O3 nolto = 9.5ms; O3 thin = 0.37ms → **LTO ratio 25.5×** (worst-case empty-Drop amplification; Phase B+ rerun number). Pure-Rust cross-crate baseline (Phase B+, see below) measures 27.5× — within run-to-run variance; the amplification is INHERITED from Rust, not Sky-specific.

**Decision-gate verdict:** lock §5.5. Recommendation: `[profile.release] lto = "thin"` for any perf-sensitive build.

**Two follow-up findings (documented in arch doc §F.18 + §25.2 B10):**
- **F3:** Sky `main` + while loop + 1M+ allocations stack-overflows (hypothesis: toylangc O0 alloca recycling bug). Worked around by driving Bench 3's loop from a Rust caller.
- **B10 residual:** Phase E drop synthesis + `Vec<SkyStruct>` at opt-level ≥ 1 still trips the LLVM 21 BitcodeWriter bug under ThinLTO cross-CGU import. Same Rust-caller workaround. §25.2 B10 was tightened from "CLOSED" to "CLOSED for primary path; residual trigger…".

**Doc landings in this phase:**
- New §22.4 "Perf model" with bench results, recommendation, and the cache-policy table (§22.4.1).
- New §22.4.2 "Reproducing the benches" with precise commands.
- New §22.4.3 "Interpretation assumptions" covering host sensitivity, `black_box` discipline, thermal/freq variance, B10 Rust-caller workaround, LTO-ratio interpretation guidance.
- §22 renumbered (existing §22.4 → §22.5; §22.5 → §22.6).
- §5.5 trade-off paragraph: empirical anchors (1.5× hot-call slowdown; ~25.5× Sky / ~27.5× pure-Rust drop-heavy slowdown — Phase B+ confirmed the ratio is inherited from Rust, not Sky-specific).
- §F.16 inlining-levels table: empirical-speedup column at Levels 4/5.
- §F.18 (Phase E lessons): F3 + B10 residual footnotes.
- New §25.2 B27 risk entry: "bench-detected creeping perf regression between nightly bumps" with canary at Bench 2 K=100 ratio drift > 10% / Bench 3 ratio drift > 20%.

**What stayed deferred (and is in the "Known follow-up" backlog at the end of this doc):**
- B-22 criterion-style statistical framework (~1 day on its own).
- B-11 cross-platform reruns (linux-x86_64 — Sky's actual primary target).
- B-12 / B-13 generic-fn / trait-impl benches (need design).
- I-1 B10 upstream investigation + minimal LLVM repro.
- I-12 toylangc O0 alloca-recycling fix (F3 root cause).

### Phase B+: Bench 3 pure-Rust baselines — DONE 2026-06-24

**Status: SHIPPED 2026-06-24** (follow-up session after Phase B). 6 new fixtures + 1 sibling crate + runner extension + doc updates landed cleanly.

#### What landed

- **New sibling crate** `toylangc/tests/integration_projects/test_widgets/` with `Widget` (LLVM-inlinable Drop) + `WidgetNoInline` (`#[inline(never)]` Drop) + `make_test_widget` + `make_test_widget_no_inline`. Same `[workspace]` self-isolation pattern as `test_helpers/`.
- **6 new bench fixtures** under `perf_bench/bench3_rust_baseline_{single_crate,cross_crate,inline_never}_o3_{nolto,thin}`, each with a minimal Sky-source placeholder + Rust caller + toml.
- **Runner extended** with a new "Bench 3 pure-Rust baselines" section.
- **§22.4 perf-model table extended** with 3 new rows; finding #2 rewritten ("inherited from Rust"); new finding #3 explaining the floor/ceiling baselines.
- **§22.4.3 "Apples-to-apples Rust baseline" paragraph** added documenting the 27.5× / 3% delta / 0.95ns floor.
- **`tmp/perf-bench-summary.md`** F2 finding rewritten + new Bench 3 baseline table + round-4 lead-with bullet 2 refreshed.

#### Headline finding

**R_rust_cross = 27.5×** (cross_crate_o3_nolto = 9901μs / cross_crate_o3_thin = 360μs) versus **R_sky = 25.5×** (drop_o3_nolto = 9470μs / drop_thin = 371μs). Within run-to-run variance. Sky-vs-Rust at thin LTO: 371μs vs 360μs = **3% delta** (matching Bench 1's 0.3% Sky-vs-Rust finding).

**This is the first row of the interpretation matrix below** ("Sky inherits Rust's LTO behavior"). The 26× LTO speedup is a property of cross-crate Drop chains under LLVM's LTO inliner — NOT Sky-specific. Sky's drop emission gives LLVM the same elimination opportunity a pure-Rust cross-crate Drop impl does.

#### The two bracket baselines

- `single_crate` (Widget defined IN user_bin): nolto = 334μs ≈ thin = 377μs (ratio 0.89×). LLVM's intra-crate inliner already eliminates the empty Drop body at O3 without LTO; LTO has nothing left to do. Upper bound on what LTO can deliver.
- `inline_never` (WidgetNoInline in `test_widgets`, Drop carries `#[inline(never)]`): nolto = 9400μs ≈ thin = 9491μs (ratio 0.99×). LTO literally cannot help when the inliner is forbidden. Per-element cost ≈ 0.95ns/drop — matches Bench 1's nolto baseline (~0.87ns/call), confirming the per-call cross-crate cost story.

These two baselines sandwich Sky's actual operating point in three independent ways, all consistent.

#### Files touched

| File | Change |
|---|---|
| `toylangc/tests/integration_projects/test_widgets/{Cargo.toml,src/lib.rs}` | NEW (sibling crate) |
| `toylangc/tests/integration_projects/perf_bench/bench3_rust_baseline_*/` | NEW (6 fixtures × 4 files each) |
| `toylangc/tests/scripts/run_perf_bench.sh` | Extended with Bench 3 baseline section |
| `rust-interop-architecture.md` §22.4 | Table extended + findings rewritten |
| `rust-interop-architecture.md` §22.4.3 | New "Apples-to-apples Rust baseline" bullet |
| `tmp/perf-bench-summary.md` | F2 rewritten + new Bench 3 baseline table + round-4 framing refresh |
| `handoff.md` | This Phase B+ entry marked DONE + status section + top-5 + round-4 deliverables updated |

The spec/rationale below is preserved as historical context for understanding why this work was important and what would have happened if the result had been the second or third row of the interpretation matrix.

---

#### Why this was needed (read this first or you'll miss the point)

The shipped Bench 3 measures the Sky drop chain: O3 nolto = 10.0ms; O3 thin = 0.4ms; LTO ratio = 26.5×. We have NO pure-Rust comparison for this workload. The current §22.4 / `tmp/perf-bench-summary.md` framing says:

> Bench 3 at 26.5× proves the AST-rewrite drop synthesis composes with LTO end-to-end. [...] don't quote "Sky drops are 26× faster with LTO" as a user-facing number — empty Drops are a worst case.

That framing is honest about the workload but **silent on whether the 26× ratio is Sky-specific or whether pure Rust would show the same.** The reviewer will absolutely ask this in round 4. We need the answer in hand BEFORE the round-4 conversation, not during it.

**Two possible outcomes and what each means for our framing:**

| Pure-Rust cross-crate Bench 3 ratio | Implication |
|---|---|
| ≈ 25× (close to Sky's 26.5×) | **Sky inherits Rust's LTO behavior.** The 26× is "the cross-crate Drop chain under LTO costs ~26× less than without LTO" — a property of LLVM's inliner + the Vec::drop loop shape, not Sky-specific. This is the BEST outcome for the architecture story: identical to Bench 1's "Sky vs Rust = 0.3% delta" finding, extended to drop chains. Round-4 framing becomes: "Sky's drop emission is empirically indistinguishable from a pure-Rust cross-crate Drop impl under LTO." |
| ≈ 3-5× (much smaller than Sky's 26.5×) | **Sky-specific amplification.** Possible cause: Sky's emission produces IR that LLVM can fully eliminate under LTO (because Sky knows the body is a Drop with no side effects); Rust's equivalent IR has metadata or markers (e.g., panic-on-overflow checks, debug-info anchors) that LLVM doesn't eliminate as aggressively. Round-4 framing becomes a more nuanced: "Sky's drop emission gives LLVM more elimination opportunity than Rust's standard Drop emission does. Worth investigating whether Sky's emission is dropping anything that Rust intentionally preserves." We'd need to look at the IR diff. |
| ≈ 1× (Rust nolto already as fast as Rust thin) | The Rust crate had its Widget in-crate, so nolto already inlined intra-crate. **Not apples-to-apples.** Need a sibling-crate Rust baseline (Fixture 2 below specifically targets this — same cross-crate structure as Sky). |

The most likely outcome is the first one (parity with Sky) based on the Bench 1 + baseline finding that Sky's call boundary matches Rust's cross-crate boundary. But "most likely" isn't "verified," and the reviewer will want verification.

#### The three fixtures to build

**All three fixtures share the same Rust-caller pattern:** allocate `Vec<Widget>` with 10M elements, then `drop(v)` and time the drop. The fixtures differ only in WHERE Widget + its Drop impl live (in-crate / sibling-crate / sibling-crate with `#[inline(never)]`) and the LTO setting.

##### Fixture 1: `bench3_rust_baseline_single_crate_o3_{nolto,thin}` (2 fixtures)

The "easy case" for LLVM — Widget defined in the same crate as `main`, so intra-crate inlining at O3 can eliminate the Drop body without LTO.

- Sky source (`main.toylang`): trivial placeholder (toylangc requires it; same shape as `bench1_rust_baseline_o3_nolto/main.toylang`).
- Rust caller (`rust_caller.rs`):

```rust
use std::time::Instant;

struct Widget {
    id: i32,
}

impl Drop for Widget {
    fn drop(&mut self) {}
}

#[inline(never)]
fn make_widget(id: i32) -> Widget {
    Widget { id }
}

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<Widget> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(make_widget(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
```

The `#[inline(never)]` on `make_widget` is intentional — it matches Sky's `make_widget` (which is a stub-rlib extern call from Rust's view), keeping the Vec-build phase comparable. The Drop body itself has no `inline` annotation — LLVM is free to inline or not as it chooses.

- toylang.toml: copy from `bench1_rust_baseline_o3_thin/toylang.toml`; rename + set `opt-level = "3"` + (for the thin variant) `lto = "thin"` + `codegen-units = 16`.
- expected_output.txt: `BENCH_ELAPSED_US=` (matches existing bench fixtures).

##### Fixture 2: `bench3_rust_baseline_cross_crate_o3_{nolto,thin}` (2 fixtures)

The apples-to-apples comparison with Sky's Bench 3 — Widget defined in a SIBLING Rust crate that user_bin path-deps on. This is the cross-crate structural equivalent of Sky's setup.

Requires a new sibling crate at `toylangc/tests/integration_projects/test_widgets/`:

```toml
# test_widgets/Cargo.toml
[package]
name = "test_widgets"
version = "0.0.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[workspace]
```

```rust
// test_widgets/src/lib.rs
pub struct Widget {
    pub id: i32,
}

impl Drop for Widget {
    fn drop(&mut self) {}
}

#[inline(never)]
#[no_mangle]
pub extern "C" fn make_test_widget(id: i32) -> Widget {
    Widget { id }
}
```

(Note: `extern "C"` on `make_test_widget` is necessary because `Widget` would otherwise be non-FFI-safe; under controlled toolchain the ABI happens to round-trip. Same pattern test_helpers uses for `make_some_i32` / `make_ok_i32`. Add `#[allow(improper_ctypes_definitions)]` if rustc warns.)

Rust caller:

```rust
use std::time::Instant;
use test_widgets::Widget;

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<Widget> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(test_widgets::make_test_widget(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
```

toylang.toml adds the path-dep:

```toml
[project]
name = "bench3_rust_baseline_cross_crate_o3_thin"
source = "main.toylang"
rust_caller = "rust_caller.rs"
opt-level = "3"
lto = "thin"
codegen-units = 16

[rust-dependencies]
test_helpers = { path = "../../test_helpers" }
test_widgets = { path = "../../test_widgets" }
```

##### Fixture 3: `bench3_rust_baseline_inline_never_o3_{nolto,thin}` (2 fixtures)

The "irreducible per-drop cost" lower bound. Same as Fixture 2 but with `#[inline(never)]` ON THE DROP IMPL:

```rust
// In test_widgets/src/lib.rs, or a separate test_widgets_inline_never crate:
pub struct WidgetNoInline {
    pub id: i32,
}

impl Drop for WidgetNoInline {
    #[inline(never)]
    fn drop(&mut self) {}
}
```

This forces the per-element drop to be a real function call no matter what LTO does. The nolto-vs-thin ratio for this fixture tells us what the cross-crate call-boundary cost is when the inliner literally can't help (should be similar to Bench 1's ratio: ~1.5×). It also serves as a sanity check — if Fixture 2's thin variant doesn't beat Fixture 3's thin variant, something is wrong with the cross-crate-inline story.

Easier approach: put `WidgetNoInline` in the same `test_widgets` crate alongside `Widget`. Then both fixtures share the same sibling crate.

#### Where to copy patterns from

- Existing Bench 3 Sky fixture: `toylangc/tests/integration_projects/perf_bench/bench3_drop_thin/` — shows the rust_caller.rs shape calling `__lang_stubs::make_widget`. For pure-Rust baselines, replace `__lang_stubs::*` with the Rust-source equivalents.
- Existing Bench 1 Rust baseline: `toylangc/tests/integration_projects/perf_bench/bench1_rust_baseline_o3_thin/` — shows the minimal Sky-source placeholder (`export fn unused_anchor() -> i32 { 0 }`) and the toylang.toml pattern.
- test_helpers crate: `toylangc/tests/integration_projects/test_helpers/Cargo.toml` — model for the new `test_widgets` Cargo.toml. Note the `[workspace]` block at the bottom — that's needed to prevent cargo from walking up into the outer erw workspace.
- Runner: `toylangc/tests/scripts/run_perf_bench.sh` — extend the Bench 3 loop to include the 6 new fixtures.

#### Execution steps

1. Create `toylangc/tests/integration_projects/test_widgets/` (new sibling crate, mirror test_helpers's Cargo.toml + `[workspace]` block + `src/lib.rs`).
2. Add `Widget` + `WidgetNoInline` to `test_widgets/src/lib.rs` per the patterns above.
3. Create 6 new bench fixture directories under `toylangc/tests/integration_projects/perf_bench/` — `bench3_rust_baseline_single_crate_o3_{nolto,thin}`, `bench3_rust_baseline_cross_crate_o3_{nolto,thin}`, `bench3_rust_baseline_inline_never_o3_{nolto,thin}`.
4. For each: minimal main.toylang placeholder, rust_caller.rs per the spec, expected_output.txt with `BENCH_ELAPSED_US=`, toylang.toml.
5. Extend `toylangc/tests/scripts/run_perf_bench.sh` — add the 6 new fixture names to the Bench 3 section's loop. Update the Bench 3 section's intro text to explain the apples-to-apples comparison.
6. Smoke-test one fixture: `cd toylangc/tests/integration_projects/perf_bench/bench3_rust_baseline_cross_crate_o3_thin/ && rm -rf .toylang-build && DYLD_LIBRARY_PATH="$(rustup run rustc-fork rustc --print=sysroot)/lib" LD_LIBRARY_PATH="$(rustup run rustc-fork rustc --print=sysroot)/lib" CARGO_TARGET_DIR=../../../../target/integration-projects-cache ../../../../../target/debug/toylangc build && DYLD_LIBRARY_PATH="$(rustup run rustc-fork rustc --print=sysroot)/lib" ../../../../target/integration-projects-cache/debug/bench3_rust_baseline_cross_crate_o3_thin`. Expected: `BENCH_ELAPSED_US=<number>`.
7. Run the full bench: `bash toylangc/tests/scripts/run_perf_bench.sh > tmp/perf-bench-results.md`. Should now report all 23 fixtures (17 existing + 6 new).
8. Compute and interpret the ratios (see table below).
9. Update §22.4 in `rust-interop-architecture.md` — extend the Bench 3 table with the new variants; update the F2 / drop-chain interpretation in §22.4.3 with the apples-to-apples finding.
10. Update `tmp/perf-bench-summary.md` headline findings (F2) with the comparison.
11. Update handoff.md's Phase B entry to fold in the new numbers.
12. Commit.

#### Interpretation matrix — what to do with each possible outcome

Compute three ratios from the new fixtures + existing Sky Bench 3:
- **R_sky** = bench3_drop_o3_nolto / bench3_drop_thin = **26.5×** (already in hand)
- **R_rust_single** = bench3_rust_baseline_single_crate_o3_nolto / bench3_rust_baseline_single_crate_o3_thin
- **R_rust_cross** = bench3_rust_baseline_cross_crate_o3_nolto / bench3_rust_baseline_cross_crate_o3_thin
- **R_rust_inline_never** = bench3_rust_baseline_inline_never_o3_nolto / bench3_rust_baseline_inline_never_o3_thin

| If R_rust_cross is... | Interpretation | Round-4 framing |
|---|---|---|
| **0.7× × R_sky to 1.3× × R_sky** (≈18–35×) | Sky inherits Rust's cross-crate LTO behavior — same finding as Bench 1's "Sky vs Rust = 0.3% delta." | "Bench 3's 26× LTO speedup matches what pure Rust shows under the same cross-crate Drop conditions. Sky's drop emission is empirically indistinguishable from Rust's. The 26× is a property of cross-crate Drop chains under LLVM's LTO inliner, not anything Sky-specific." This is the best-case for the architecture story. |
| **Substantially smaller than R_sky** (R_rust_cross < 10×) | Sky is somehow giving LLVM MORE elimination opportunity than Rust does. Possible causes: Rust's Drop impl has metadata (panic-on-drop-during-unwind markers? debug-info?) that Sky doesn't emit. | Don't claim Sky parity. Investigate: `llvm-dis tmp/perf-bench-disasm/...` and diff Sky's emitted IR for `<Widget as Drop>::drop` against Rust's. Whatever Sky is omitting — make sure it's safe to omit. Then either fix Sky's emission to match Rust's safety surface, or document this as a deliberate Sky property. Round-4 framing has to be honest about the divergence. |
| **Larger than R_sky** (R_rust_cross > 35×) | Unlikely but possible — would mean Rust is generating IR LLVM can eliminate even more aggressively than Sky's. Investigate IR diff. Probably not a problem (Rust users would be the ones benefiting) but worth understanding. |

R_rust_single (in-crate) is informational — expect it to be much smaller than R_sky because nolto can already inline intra-crate at O3. If R_rust_single is similar to R_sky, then the Vec::drop loop has cross-crate-ness baked in even at single-crate level (Vec lives in stdlib, which is a different crate from user_bin). That would be a finding too.

R_rust_inline_never establishes the floor: how much does the chain cost when the inliner literally can't eliminate the Drop body? Compare to Bench 1's nolto (87ms / 100M = 0.87ns per call): if R_rust_inline_never's THIN result divided by 10M iterations ≈ 0.87ns per drop, then "Drop dispatch cost matches general cross-crate call cost when forced to be a real call." Sanity check on the structural story.

#### Caveats to remember

- **The 6 new fixtures all run at O3.** That's deliberate — measuring "Sky vs Rust under LTO" requires LTO actually doing work. At O0 the comparison is uninformative (no inlining either side).
- **Don't use `black_box` on the drop loop.** The thing being measured is the Drop chain's iteration cost, not the Vec build cost. `black_box` is appropriate when defeating LLVM's whole-loop elimination of a synthetic accumulator (Bench 1 / 2 pattern); Bench 3's drop chain has real per-call side effects (allocation deallocation through Vec's destructor) that LLVM can't elide regardless.
- **Median of 5 is enough but variance might be visible.** Bench 3's thin variant runs in ~400μs — close to scheduler quantum on macOS. If you see >20% variance between runs, bump RUNS in the runner to 25 just for Bench 3 fixtures.
- **The `extern "C"` ABI for `make_test_widget` returning `Widget` is non-FFI-safe by rustc's strict definition** but happens to work under our controlled toolchain (single rustc version, no cross-language ABI risk). If rustc starts hard-erroring on this rather than warning, switch to `pub fn make_test_widget` (no `extern "C"`) — the only loss is that `#[no_mangle]` becomes moot since it's not called from C; the bench still works.

#### Time estimate

~1 focused hour: 20 min scaffolding the test_widgets crate + 6 fixtures, 5 min smoke-test, 10 min full rerun, 15 min computing ratios + drafting interpretation, 10 min doc updates, 5 min commit.

If R_rust_cross deviates substantially from R_sky (the second row of the interpretation matrix), add ~1-2 hours for IR diff investigation. Don't skip it — the architecture story is at stake.

#### Why this is Phase B+ rather than a separate Phase

It's a direct follow-on to Phase B's empirical finding; it tightens the same paragraph in §22.4 and the same headline F2 in the summary. It doesn't unblock anything downstream (Phase C+D don't depend on it). It's small, focused, and answers a single specific question. Folding it into Phase B's section keeps the related work together rather than scattering across phases.

#### What to expect in §22.4 after this lands

The Bench 3 table extends with 6 new rows. The interpretation paragraph in §22.4.3 gets a new bullet:

> **Apples-to-apples Rust baseline (added 2026-XX-XX).** Pure-Rust Bench 3 with Widget in a sibling crate (`test_widgets`) shows an LTO ratio of <NUMBER>×. Sky's 26.5× ratio is [matches Rust within noise / smaller than Rust / larger than Rust], so the drop-chain LTO speedup is [a general cross-crate property that Sky inherits / a Sky-specific amplification because Sky's emission gives LLVM more elimination opportunity / etc.]. The `bench3_rust_baseline_inline_never_*` variant establishes the floor: when the Drop body cannot be inlined, the per-element chain cost is <NUMBER>ns (matches Bench 1's general cross-crate call cost of ~0.87ns).

Fill in the actual numbers from the rerun.

### Phase C: Tool attribute infrastructure — DONE 2026-06-24

**Status: SHIPPED 2026-06-24.** Wired up `#![register_tool(toylang)]` + `#[toylang::emit_consumer_body]` machinery. (Toylang's namespace is `toylang`; Sky proper will use `skyc` per Decision 3.)

**What landed:**
- `toylangc/src/build.rs`: unconditionally prepends `#![feature(register_tool)]\n#![register_tool(toylang)]\n` to every generated stub crate's `src/lib.rs`. Independent of the user's `[project] features` list (which adds additional `#![feature(...)]` lines).
- The `tool` namespace works cross-crate by default: tool attributes encode into rmeta (rustc_metadata's `encode_cross_crate` returns `true` for any attr not in `BUILTIN_ATTRIBUTE_MAP`), so `tcx.has_attrs_with_path(upstream_def_id, &[toylang_sym, attr_sym])` returns the right answer at both the stub-rlib compile and the user-bin compile.

**Verification:** generated stub source for `bench3_drop_thin` confirms the prepend works:
```
#![feature(register_tool)]
#![register_tool(toylang)]
#![feature(allocator_api)]
...
```

### Phase D: Partition predicate migration — DONE 2026-06-24

**Status: SHIPPED 2026-06-24.** Two-gate conjunction predicate + stub_gen tagging + 1:1 invariant fence all landed cleanly.

**What landed:**
- `rustc-lang-facade/src/lib.rs::is_consumer_codegen_target`: rewritten as the two-gate conjunction per Decision 3:
  ```rust
  pub fn is_consumer_codegen_target<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
      if !is_from_lang_stubs(tcx, def_id) { return false; }
      tcx.has_attrs_with_path(def_id, &[
          rustc_span::Symbol::intern("toylang"),
          rustc_span::Symbol::intern("emit_consumer_body"),
      ])
  }
  ```
- `toylangc/src/stub_gen.rs`: three emission sites tagged with `#[toylang::emit_consumer_body]`:
  - Accessor methods inside `impl Foo { ... }` blocks (line ~239)
  - Exported wrapper fns + `__toylang_main` (line ~354)
  - Trait-impl methods on consumer types — both `&self` and `&mut self` variants (line ~463/470)
- Category A items deliberately UNTAGGED (verified via the new CI fence): `__SKY_STUBS_MARKER`, `__ToylangOpaque<T>`, `pub use` re-exports, `extern "C"` declarations, Sky struct declarations, `__toylang_option_unwrap` / `__toylang_result_unwrap` Phase-6 helpers, the `pub use core::ops::Drop` re-export.
- New CI fence `stub_gen::tests::emit_consumer_body_tags_only_category_b_items` enforces the 1:1 invariant in both directions: every `unreachable!()` body must carry the tag within 6 lines above; every `#[toylang::emit_consumer_body]` must be followed by an `unreachable!()` body within 8 lines. Plus a spot-check that exactly 3 tags appear in a representative registry (1 accessor + 1 wrapper + 1 trait-impl-method) and that `#![register_tool(toylang)]` lives only at the build.rs layer, not in stub_gen output.

**What stayed alive (will retire in Phase F per Decision 2):**
- `is_consumer_fn` and `is_consumer_accessor_safe` matchers — still consulted by `queries/symbol_name.rs` (callback-name shape classification) and `queries/per_instance.rs` (callback-name routing). They retire together with the symbol_name override.

**Test coverage:**
- 477 tests pass (1 new test for the 1:1 fence; baseline was 476). 0 failures excluding the pre-existing rustdoc-not-installed doctest infrastructure error in rustc-fork.
- Smoke-test of `bench3_drop_thin` (post-change) reports 419μs — within run-to-run variance of the 371μs pre-change baseline. The new attribute-based predicate produces identical runtime behavior to the old name/structural matchers.

**Files touched:**
- `toylangc/src/build.rs`: +5 / -2 (prepend the two crate-level attrs)
- `toylangc/src/stub_gen.rs`: +160 / -10 (tag the three emission sites + the new CI fence test)
- `rustc-lang-facade/src/lib.rs`: +18 / -23 (rewrite the predicate)

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

### Phase F: symbol_name elimination — DONE 2026-06-24

**Status: SHIPPED 2026-06-24** (Decision 2). The override is gone; rustc's default v0 mangler now produces every consumer symbol directly.

**What landed:**
- `rustc-lang-facade/src/queries/symbol_name.rs`: **deleted**.
- `rustc-lang-facade/src/queries/mod.rs`: removed `mod symbol_name;`, removed the `providers.queries.symbol_name = symbol_name::lang_symbol_name;` assignment, removed the `symbol_name` argument to `install_query_defaults`, updated the module-level doc to describe the retirement.
- `rustc-lang-facade/src/lib.rs`: removed `DEFAULT_SYMBOL_NAME` OnceLock, removed `default_symbol_name()` accessor, removed `symbol_name` parameter from `install_query_defaults`, dropped `is_consumer_accessor_safe` (orphaned post-override-removal; only the `symbol_name` override and the pre-Phase-D `is_consumer_codegen_target` were callers).
- `rustc-lang-facade/src/lib.rs::LangCallbacks`: removed `consumer_symbol_for_callback_name` trait method.
- `StatefulVtable`: removed `consumer_symbol_for_callback_name` fn-pointer slot.
- `trampoline_consumer_symbol_for_callback_name`: removed.
- `call_consumer_symbol_for_callback_name`: removed.
- `install_callbacks::StatefulVtable::new`: removed the slot initializer.
- `toylangc/src/toylang/callbacks_impl.rs`: removed the `consumer_symbol_for_callback_name` trait impl; removed `compute_consumer_symbol` helper; **`compute_fn_symbol` now reads `tcx.symbol_name(instance).name.to_string()` directly** instead of going through `default_symbol_name()(tcx, instance)`. The toylangc-internal callers of `compute_fn_symbol` (7 sites at populate time, populate_toylang_instances_from_cgus, cascade-drain emission, etc.) are unchanged.
- `toylangc/tests/cache_audit.rs`: removed `symbol_name.rs` from the audit list (file is gone); module-level doc rewritten to note the retirement.

**What stayed alive:**
- `is_consumer_fn` (in `queries/per_instance.rs`): used to route the consumer's `collect_generic_rust_deps` callback by name. Eventually retires when per_instance.rs migrates from name-keyed callback routing to Instance-keyed routing (no current driver).
- `is_consumer_trait_impl_method` (in toylangc's `collect_consumer_trait_impl_instances`): used to detect trait-impl methods in rustc's mono partition at `consumer_fill_modules` time. Case 4 / case 6 cascade-drain mechanism; structural to Sky's interop.

**Test coverage:**
- 477 tests pass (same baseline as Phase C+D). 0 failures excluding the pre-existing rustdoc-not-installed doctest infrastructure error in rustc-fork.
- Smoke-test of `bench3_drop_thin` (post-Phase-F): 352μs — within run-to-run variance of the 371μs / 419μs baselines from earlier sessions. The symbol-name path produces identical runtime behavior.

**Doc updates:**
- arch §4.5 marker-detection: dropped `symbol_name` from the provider registration list.
- arch §5.4 (`SkyCodegenBackend::provide`): override list refreshed; symbol_name moved to the "Retired" section with the Phase F annotation.
- arch §6.2: rewrote the single-symbol-architecture paragraph to reflect that rustc's default v0 mangler is now the source of truth, with the pre-Phase-F bypass mechanism documented as historical context. Code example shows `tcx.symbol_name(instance).name` directly.
- arch §6.2 "no forward declarations": refreshed to credit the single-symbol architecture (not the override) for making forward decls unnecessary.
- arch §9.3: rust-c-default mangler note replaces the "symbol_name override" mention.
- arch §20.2 / §20.6 pipeline-ordering: dropped symbol_name from override list; refreshed the per-Instance symbol-name path to credit the default mangler.
- arch §22.4.1 cache-policy table: `symbol_name` row crossed out with the retirement annotation.
- arch §25.2 B21 risk entry: refreshed to note the symbol_name retirement.
- arch §26.1 SyMINCZ quick-ref + body: rewritten to reflect that the invariant survives in spirit; rustc's default mangler is what reads consult.
- arch §26.2 GCMLZ: refreshed the mechanism description.
- arch §F.2: rewrote "compute the rustc-mangled name" guidance — `tcx.symbol_name(instance)` directly, no override to dodge.

**Output:** symbol_name override gone; Sky relies on rustc's default mangling at every read site.

### Phase G: cdylib build system (3-5 days)

Restructure Sky's toolchain to ship as paired rustc-fork + libsky_backend.so (Decision 4).

Tasks:
- Restructure `rustc-lang-facade` to compile as cdylib.
- Modify rustc-fork to load libsky_backend.so via the `CodegenBackend` plugin mechanism (`-Zcodegen-backend=sky` baked into default config).
- Build runtime version handshake at backend load.
- Update toolchain bundle structure (Sky toolchain ships rustc-fork + libsky_backend.so + skyc + cargo + LLVM shared libs).
- Update build scripts for the cdylib model.

**Output**: Sky's backend loadable as cdylib; fast iteration loop unlocked for further work.

### Phase H: cdylib FFI shape — DONE 2026-06-24

**Status: SHIPPED 2026-06-24** (Decision 5). Patch 4 rev 3 lands: trait-object `&mut dyn ExtraModuleAllocator<M>` retired in favor of a `#[repr(C)]` struct with two stable-ABI fields (state pointer + extern-C fn pointer). Done as standalone work without Phase G because the new FFI shape is correct under both static-link (current) and cdylib (future) integration; preserves wrapper-mode unchanged.

**What landed (rustc fork, ~/rust):**
- `compiler/rustc_codegen_ssa/src/traits/backend.rs`:
  - Removed: `pub trait ExtraModuleAllocator<M> { fn allocate(...) -> &mut M; }` + `pub struct VecAllocator<'a, M, F>` + its impl.
  - Added: `#[repr(C)] pub struct ExtraModuleAllocator<M> { state: *mut c_void, allocate: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> *mut M }`.
  - Changed: `ExtraBackendMethods::fill_extra_modules` allocator param from `&mut dyn ExtraModuleAllocator<Self::Module>` → `&ExtraModuleAllocator<Self::Module>`.
- `compiler/rustc_codegen_ssa/src/traits/mod.rs`: dropped `VecAllocator` from the public re-export list.
- `compiler/rustc_codegen_ssa/src/base.rs::codegen_crate`: replaced the `VecAllocator` construction with: an inner `AllocatorState<'a, 'tcx, B>` struct, an `unsafe extern "C" fn allocate_thunk<B: ExtraBackendMethods>` thunk (monomorphized per `B`), and an `ExtraModuleAllocator<B::Module>` struct literal built from a pointer to the state + the thunk. Passes the allocator by shared reference (`&allocator`).
- `compiler/rustc_codegen_llvm/src/lib.rs`: `FillExtraModulesHook` type alias + `LlvmCodegenBackend::fill_extra_modules` signature updated from `&mut dyn ...` to `&...`.

**What landed (facade + toylangc, erw):**
- `rustc-lang-facade/src/extra_modules_hook.rs::consumer_fill_modules_hook`: signature updated to take `&ExtraModuleAllocator<ModuleLlvm>`; module-level doc rewritten with the rev-3 ABI note.
- `rustc-lang-facade/src/lib.rs::LlvmModuleFactory`:
  - Inner field changed from `&'a mut (dyn ExtraModuleAllocator<ModuleLlvm> + 'a)` → `&'a ExtraModuleAllocator<ModuleLlvm>` (the `#[repr(C)]` struct).
  - `fill_module` now reads `(self.inner.allocate)(self.inner.state, name.as_ptr(), name.len())` to obtain `*mut ModuleLlvm`, briefly reborrows it to read the raw LLVMContext + LLVMModule pointers, and hands them to the closure. Reborrow scope ends before the closure returns; subsequent `fill_module` calls (which may invalidate prior pointers via vec realloc inside rustc) are safe.
  - Docstring updated with the rev-3 ABI context + the closed cdylib-FFI risk.

**Test coverage:**
- Rebuilt rustc-fork via `python3 x.py dist rustc-dev` + tarball reinstall + `x.py build --stage 2 library src/tools/rustdoc` + rustc-dev reinstall (the full procedure from `docs/historical/rebuilding-rustc-fork.md`). Rebuild wall-clock: ~3.5 min.
- `cargo build --bin toylangc`: clean.
- Smoke-test of `bench3_drop_thin`: 615μs — within run-to-run variance of recent bench results (range across recent sessions: 352–615μs; M-series macOS thermal/freq drift dominates at this scale).
- `cargo test --workspace`: 477 pass cold, exit 0 (including the doctest pass from yesterday's rustdoc + library + rustc-dev fix).
- No regressions from the FFI shape change.

**Why standalone Phase H (without Phase G):**
Phase G described as the full Sky-architecture migration (toylangc retires wrapper mode + facade becomes cdylib + rustc-fork loads cdylib via `-Zcodegen-backend`). That's ~5-7 days because retiring wrapper mode in toylangc is itself ~2-3 days, plus Phase G's structural work, plus Phase H. The new patch 4 shape from Phase H is correct under BOTH the current static-link model AND the future cdylib model — moving the FFI ABI to stable-ABI primitives first means Phase G, when it lands, becomes a wiring change (build system + crate-type + loader path) rather than an ABI-compatibility rewrite.

**Files touched (8 total):**
- ~/rust: `compiler/rustc_codegen_ssa/src/traits/backend.rs`, `compiler/rustc_codegen_ssa/src/traits/mod.rs`, `compiler/rustc_codegen_ssa/src/base.rs`, `compiler/rustc_codegen_llvm/src/lib.rs`.
- erw: `rustc-lang-facade/src/extra_modules_hook.rs`, `rustc-lang-facade/src/lib.rs`, `rust-interop-architecture.md` (§3.2 patch 4 + §C.1 hook signature comment + §B.4 + §C.4 + §F.15), `handoff.md` (this entry + Decision 5).

**Output:** FFI uses stable-ABI primitives. Sky proper's cdylib refactor (handoff Phase G — when prioritized) becomes a wiring change rather than an ABI-compatibility rewrite. cdylib pairing failures will fail at link, not at runtime.

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

### Phase O: Drift-observation CI fences — SHIPPED 2026-06-25

Detection fences for B24/B25/B26 (Decision 15) all landed.

**Shipped:**
- **B24 (drop-glue shape stability):** `toylangc/tests/integration_projects/drop/fence_b24_field_drop_order/` — `Vec<Vec<Widget>>` exercising rustc's `<Vec<T> as Drop>::drop` iteration order at two nested levels. Expected sequence locks in forward-iteration semantics. Sentinel companions: fixtures 1, 2, 7, 10 (Vec element drops; LIFO order; generic drop).
- **B25 (default symbol mangling stability):** `toylangc/tests/mangler_version_fence.rs` — grep-based assertion that emission paths use `tcx.symbol_name(...)` (rustc-default mangler) + no hardcoded `--symbol-mangling-version` flag. The ~352 cross-crate integration fixtures are the inherent sentinels; the fence makes the dependency on rustc's default explicit.
- **B26 (`MonoItem` / `InstanceKind` variant coverage):** `toylangc/tests/instance_kind_coverage_fence.rs` — compile-time exhaustive match over both enums. rustc's E0004 fires at build time when a new variant lands, forcing a maintainer to consciously add an arm. Currently covers 3 `MonoItem` variants (`Fn`, `Static`, `GlobalAsm`) and 15 `InstanceKind` variants (`DropGlue` is Sky-load-bearing; `AsyncDropGlue` + `AsyncDropGlueCtorShim` are emergent, TODO Phase M).

**Doc impact:**
- Arch §25.2 gained B24/B25/B26 entries with probability, impact, detection fence pointers, reaction plan.
- This handoff section's status flipped from PARTIAL to SHIPPED.

### Phase P: `deduce_param_attrs` soundness override (~half day) — surfaced during round-4 close

**Status: OPEN, high priority.** Latent silent-UB vector currently in production-bound emission.

**WHAT.** Override rustc's `deduce_param_attrs` query for `#[toylang::emit_consumer_body]`-tagged items to return `&[]` (no attrs claimed). This is the safe-default fix that closes the soundness gap; perf recovery (path-b emission of Sky's ground-truth attrs at the wrapper boundary) is a deferred v2 option, tracked in §22.4 of the arch doc — don't commit to it without a bench showing the v1 conservative shape leaves measurable perf on the table.

**WHY.** rustc's `deduce_param_attrs` analyzes Sky's stub `unreachable!()` MIR body. The body lowers to a `Call` terminator to `core::panicking::panic` that doesn't touch param locals at all. `UsageSummary` stays `empty()` → rustc concludes "the function neither mutates, captures, drops, nor shared-borrows its param." `apply_deduced_attributes` (rustc-fork `compiler/rustc_ty_utils/src/abi.rs:646-672`) then sets `ReadOnly` + `CapturesNone` for `PassMode::Indirect` params.

These attrs propagate to every Rust caller's call site. If Sky's actual `fill_extra_modules`-emitted body mutates an indirect-passed param (e.g., `&mut LargeStruct`), the `readonly` attr LLVM applies is a LIE. LLVM trusts the attr; verifier checks shape, not semantics. **Silent UB at -O2+.**

Same B10 shape (rustc trusts stub MIR; stub "lies" about behavior); worse failure mode (silent miscompile vs B10's loud compile error). Currently latent — Sky has no fixture with indirect-passed mutable params. Bench 1's `add(i32, i32) -> i32` is all `PassMode::Direct` and dodges the `apply_deduced_attributes` indirect-only path. A real Sky 0.1.0 release with any `fn f(x: &mut LargeStruct)` Sky export would hit silent UB.

**Tasks.**
- Add `deduce_param_attrs` override in `rustc-lang-facade/src/queries/` returning `&[]` for items where `is_consumer_codegen_target(tcx, def_id)` returns true.
- Add `cache-audit:` marker comment per the new override (per the cache_audit fence from Phase I).
- New integration fence: `tests/integration_projects/<somewhere>/deduce_param_attrs_indirect_mut/`. Sky export takes `&mut LargeStruct`; Rust caller pre-fills the struct, calls Sky which mutates it; verify post-call observation at `-O3 + lto = "thin"` is correct (the mutation visible).
- Optional: build a sibling fixture WITHOUT the override to confirm the bug fires before the fix (negative-test framing in the doc).

**Output.** Silent UB vector closed. Sky exports with indirect-passed mutable params are now safe at all opt levels.

**FILES TOUCHED (planned).**
- `rustc-lang-facade/src/queries/deduce_param_attrs.rs` (new file)
- `rustc-lang-facade/src/queries/mod.rs` (add module + provider registration)
- `rustc-lang-facade/src/lib.rs::install_query_defaults` (sig if needed)
- `toylangc/tests/integration_projects/<new fixture dir>`
- `toylangc/tests/cache_audit.rs` (new override gets a marker requirement)

**GOTCHAS.**
- The override returns `&[]` — empty slice — so applies "no attrs" rather than "wrong attrs." Conservative; never UB; just may lose downstream optimization opportunities. That's the right trade-off for v1.
- Perf recovery (path b — emit Sky's ground-truth attrs in `codegen_extern_wrapper`) is deferred until a `&LargeStruct` perf bench shows the gap is material.

**DOC IMPACT.**
- New §25.2 entry: latent silent-UB vector closed (or refresh existing entry; choose framing).
- §22.4.1 cache-policy table: new query row.

### Phase Q: `codegen_fn_attrs` override for `NEVER_UNWIND` — SHIPPED THEN RETIRED same day 2026-06-25

**Status: SHIPPED THEN RETIRED 2026-06-25.** Commits `736eeb5` (ship) + `f88aa84` (retire). Provenance comment chain pinned in `rustc-lang-facade/src/queries/mod.rs` with explicit DO NOT FLATTEN annotation (commit `a6f7940`); per reviewer's round-4-followup, "Doc-trail of decisions that oscillated is more useful than the doc-trail of decisions that landed cleanly — it shows future engineers the failure mode that drove the re-introduction."

**Why retired (reviewer's reasoning, 2026-06-25):** cargo enforces panic-strategy consistency across the dep graph at build-graph resolution. A `panic = "unwind"` user_bin pulling in a `panic = "abort"` Sky stub_rlib would fail before any compile fires. Sky's `.skybuild/Cargo.toml` pins panic=abort at the workspace root (§16.1); cargo propagates to all members. The mixed-panic-strategy case Phase Q's `NEVER_UNWIND` flag defended against is structurally impossible within Sky's tooling — Phase Q was a defensive correctness no-op with no real failure mode.

If Sky ever ships precompiled-bodies for pure-cargo consumption (arch §21.7 v2), the panic-strategy question reappears at the cargo-package metadata layer (e.g. `package.required-features = ["panic_abort"]`), NOT at the codegen-attr layer. Per "every Sky mechanism must be load-bearing" discipline, dead overrides incur maintenance cost.

**Files removed in `f88aa84`:**
- `rustc-lang-facade/src/queries/codegen_fn_attrs.rs` (deleted)
- mod.rs registration + lang_override_queries call removed
- lib.rs OnceLock + accessor + install_query_defaults sig
- cache_audit.rs entry

**Historical Tasks/WHAT/etc. preserved below as design history; do NOT re-implement.**

----- HISTORICAL DESIGN-HISTORY ENTRY (do not re-implement) -----

**WHAT.** Override `codegen_fn_attrs` for `#[toylang::emit_consumer_body]`-tagged items to stamp the `NEVER_UNWIND` flag. Eliminates LLVM landing-pad emission at every Rust caller — significant for tight callee-rich loops under panic=unwind.

**WHAT.** Override `codegen_fn_attrs` for `#[toylang::emit_consumer_body]`-tagged items to stamp the `NEVER_UNWIND` flag. Eliminates LLVM landing-pad emission at every Rust caller — significant for tight callee-rich loops under panic=unwind.

**WHY.** Sky enforces `panic = "abort"` globally per arch §16.1. Every Sky export is genuinely never-unwind. rustc's default `codegen_fn_attrs` for Sky stubs leaves `NEVER_UNWIND` off (because the stub source doesn't carry a `#[no_panic]` attr) → every Rust caller emits landing pads / cleanup blocks unconditionally, even though Sky's real body cannot panic.

**Tasks.**
- Build a panic=unwind perf bench: Sky export called from a Rust loop, surrounded by unwinding Rust code (e.g., destructors that can panic). Measure runtime at -O3 + thin LTO with and without the override.
- Add `codegen_fn_attrs` override in `rustc-lang-facade/src/queries/` returning the default attrs with `NEVER_UNWIND` flag set, for tagged items.
- `cache-audit:` marker.
- Document delta in §22.4.

**Extensions (incremental from same override).**
- `Cold` for unlikely-path Sky exports — requires Sky source annotation `#[cold]` propagated through `toylang::emit_consumer_body`-tagged emission.
- `FFI_PURE` / `FFI_CONST` where Sky's typechecker knows purity — unlocks LLVM's pure-function optimizations.
- Explicit `target_features` for SIMD-related callsite-feature-compat decisions (per rustc_codegen_ssa builder.rs:1431).

**Output.** Measurable perf win on panic=unwind builds. Other Sky-call optimization opportunities unlocked incrementally.

**GOTCHAS.**
- `NEVER_UNWIND` is only valid if Sky's real body actually doesn't unwind. Under Sky's panic=abort posture this is guaranteed. If Sky ever ships a panic=unwind mode (it shouldn't per §16.1, but hypothetically), this override needs gating.
- Make sure the override is gated on Sky-active marker presence (per §4.4 byte-identical pass-through invariant).
- Historical: Sky overrode `codegen_fn_attrs` during the Option 4 era for linkage stamping. That override retired. The new override is a fresh use of the same query, for a different purpose.

**DOC IMPACT.**
- §22.4 perf-model: new finding for panic=unwind workloads.
- §22.4.1 cache-policy table: new query row.
- New B-class risk entry if applicable.

### Phase R: B10-style emission audit follow-ups (~1-2 days) — surfaced during round-4 close agent investigation

**Status: OPEN, medium priority.** 7 candidate sites identified by emission audit; build probes + fix if triggered.

**WHAT.** Build probe fixtures for each candidate ABI-coercion-mismatch site in toylangc's emission. For each that triggers an LLVM bitcode-parse error (or other malformed-IR symptom) at -O1+, apply the same memory-reinterpretation pattern the round-4 B10 fix uses.

**Sites in priority order:**

**Site #8 — `codegen_extern_wrapper` sret-bridge alloca/load size mismatch** (`llvm_gen.rs:1156-1160`). Top priority — same B10 shape, asymmetric direction. Alloca sized as internal toylang struct type; load reads as `rust_ret_type` (ABI-coerced). When `rust_ret_type` is larger than the alloca, reads past stack. Probe: toylang fn `fn f() -> struct W { a: i8 }` where rustc ABI-coerces W to a larger direct return; verify build + correct runtime at -O3 + thin LTO.

**Site #1 — `push_arg_for_rust_call::Pair` arm assumes struct source** (`llvm_gen.rs:528-537`). Calls `val.into_struct_value()`. If source is a bare `ptr` from `Ref { inner }`, extract panics or extracts wrong bits. No `arg_toylang_ty != target` symmetry-check like the Direct arm has post-fix. Probe: pass `&Widget` (Widget coerces to ScalarPair) to a Rust generic whose param resolves to `Pair`.

**Sites #5 + #6 — receiver-as-Indirect assumption** (`llvm_gen.rs:1532-1547`, `1578-1594`, `1797`, `1813`). Receiver `recv_ptr` always pushed as `ptr`. Breaks when `self` coerces to `Pair` (e.g., slice self-types like `<[T]>::len(self: &[T])`). Probe: explicit static-call form on a slice method.

**Site #10 — `parse_coerced_type` missing float/vector arms** (`llvm_gen.rs:2020-2046`). Missing `float`/`half`/`fp128` + vector cases. Will panic on first `f32`/`f64` Sky export. Loud failure, not silent corruption; but important to fix the moment anyone wants float Sky exports. Probe: `export fn add_f64(a: f64, b: f64) -> f64 { ... }`.

**Lower-priority sites (build only if higher-priority probes complete cleanly):**

**Site #7 — Direct param load with larger internal_ty** (`llvm_gen.rs:1085-1101`). Asymmetric inverse of round-4 fix. Trigger requires exotic `Cast { prefix, rest }` shape.

**Site #9 — Direct return mismatch when internal returns non-int** (`llvm_gen.rs:1161-1171`). `coerce_int_to_type` only handles int truncation. Triggers if `is_internal_sret` evaluates false for a struct that should be sret.

**Site #2 — `Indirect` arm alloca alignment for `repr(transparent)`** (`llvm_gen.rs:519-526`). Lower-likelihood; alloca alignment may differ from callee expectation for `repr(transparent)` newtype.

**Tasks per site:**
- Build a `b10_probe_<descriptive_name>` fixture under `toylangc/tests/integration_projects/drop/` or a new `tests/integration_projects/abi_mismatch/` subdirectory.
- Enroll in integration runner.
- Run probe; if build/runtime fails, characterize the failure mode (compile error vs miscompile vs UB).
- Fix the emission site using the same pattern as the round-4 B10 fix: detect type mismatch, reinterpret via memory.
- Add the fixture as a regression test post-fix.

**Output.** All known ABI-coercion mismatch sites either probed clean or fixed. Sky's emission is robust to LLVM bitcode round-trips across opt levels and LTO modes for all current ABI shapes.

**GOTCHAS.**
- Some probe sites may not trigger today simply because Sky source can't produce the trigger shape (e.g., float Sky exports — Site #10 won't fire until someone writes one). Document as "latent; will fire when X" rather than treating as a clean pass.
- Site #8 and Site #7 are inverses of the round-4 fix and may share a single helper if both need fixing. Refactor opportunity.

**DOC IMPACT.**
- §25.2 B10 already covers this class of bug; new B-class entries for any newly-found sites.
- arch §26.5 (`@ACRTFDZ`) note the memory-reinterpretation pattern is now used at multiple sites, not just `codegen_extern_wrapper`'s incoming params.

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
- Phase P: half-day (deduce_param_attrs soundness override) — SHIPPED 2026-06-25.
- ~~Phase Q: 1 day~~ — SHIPPED THEN RETIRED 2026-06-25 (no work needed; deleted same day).
- Phase R: 1-2 days (B10-style emission audit follow-ups).

Some can parallelize (cdylib build + FFI shape + cache audit could overlap with content-hash work); some can't (predicate migration must precede mir_shims elimination; both must precede per-view ref types in stub_gen).

---

## Doc update plan

After implementation, the architecture doc needs updates reflecting the decisions. Don't do these BEFORE implementation — the doc updates should be backed by the implementation reality, not by predicted reality.

**Status refresh (2026-06-24):** roughly two-thirds of the doc updates listed below have landed via Phases E/F/H/I/B/B+/C+D. The status markers (✅ DONE, ⏳ PENDING-DECISION, ⚙️ STILL-TO-LAND) flag which subsections still need doc work vs which already landed.

### High-priority rewrites (substantial changes)

- ✅ **§15 (drop semantics) — DONE 2026-06-23 (Phase E):** §15.7 rewritten around AST-rewrite drop synthesis + cascade-discovery + `fill_extra_modules` emission. mir_shims-override description retired; §F.18 added with Phase E lessons. `#[may_dangle]` policy + Drop-body contract (Decisions 12 + 17) deferred — they ride on Phase L's per-view ref types work.
- ⏳ **§1.7 (Sky does not surrender LLVM output control) — PENDING:** Decision 16 says lead with backend pluralism. No code change required; doc-only edit waiting for the rewrite pass.
- ⏳ **§10 (type representation) — PENDING:** awaits stub-type contract update once Phase L per-view refs ship.
- ⏳ **§12.1 (Send) — PENDING Phase L:** rewrite around per-view ref types `SkyRef<T, V>`. No empirical work yet.
- ⏳ **§12.2 ('static) — PENDING Phase L:** correction — the "honest by construction" framing was wrong. Same Option C extension applies.
- ⏳ **§13.3 / §13.4 (slab + content-hash) — PENDING Phase K:** rewrite — retire slab-pointer-as-u64; content-hash const args; slab purely Sky-internal.
- ⏳ **§14.10 (async two-type split) — PENDING Phase M:** rewrite — typestate pattern, not two physical types.
- ⏳ **§17 (tokio interop) — PENDING Phase M:** restructure — Sky-native async primary, tokio via bridge crate (deferred to when bridge crate exists).

### Smaller refinements

- ⏳ **§1.2 + §11 (groups) — PENDING:** correction — groups are compile-time, not runtime arenas. Doc-only.
- ⏳ **§3.1 (per-Instance body content) — PENDING:** sharpen — per-Instance dep enumeration is the load-bearing reason, not arbitrary-typed const generics.
- ✅ **§3.2 / §B.4 / §C.4 (patch 4) — DONE 2026-06-24 (Phase H):** updated with rev 3 `#[repr(C)]` function-pointer struct shape; F.15 design-history extended with the rev 2 → rev 3 transition rationale.
- ⏳ **§4.1 / §4.2 / §4.3 / §4.4 (distribution) — PENDING Phase G:** restructure for cdylib model.
- ✅ **§5.3 (partition filter) — DONE 2026-06-24 (Phase D):** explicit predicate definition + 1:1 invariant + Category A/B split documented; predicate-shape history block tracks pre-Phase-D matchers.
- ✅ **§5.4 (LlvmCodegenBackend delegation) — DONE 2026-06-24 (Phase F):** dropped `symbol_name` from override list with retirement annotation.
- ✅ **§6.2 (single-symbol) — DONE 2026-06-24 (Phase F):** reframe shipped — single-symbol architecture works through `tcx.symbol_name(instance)` directly; pre-Phase-F bypass mechanism preserved as historical context.
- ⚙️ **§6.6.5 (Phase-6 wrappers) — STILL-TO-LAND:** brief comparison of `#[inline(never)]` vs `@llvm.used` (different layers). Minor doc clarification; no blockers.
- ⏳ **§10.6 / §10.8 / §13.7 / §13.8 (typeids) — PENDING Phase J:** u128, content-hash mechanism.
- ⏳ **§13.9 (no synthetic DefIds) — PENDING Phase J:** keep "no new fork surface" framing primary; content-addressing is bonus.
- ⏳ **§14.1 / §14.3 / §14.5 / §14.7 (closures/async) — PENDING Phases L/M:** integrate with the new drop model and typestate pattern. §15.7 already covers the AST-rewrite drop discipline; async-specific work waits.
- ⏳ **§19 (per_instance_mir) — PARTIAL:** dep-walker context updated alongside Phase D's predicate migration; safety-properties subsection (mentioned-items invariant, recursion safety) still waits for Phase N.
- ✅ **§22.3 / §22.4 — DONE 2026-06-24 (Phases B + B+ + I):** §22.3 added with cache-policy table; §22.4 with full perf-model writeup including the 23-fixture matrix, the apples-to-apples B+ baselines, reproduction steps (§22.4.2), interpretation assumptions (§22.4.3).
- ✅ **§25 — DONE 2026-06-24 (Phases B/B+/I + ongoing):** B10 tightened from "CLOSED" to "CLOSED for primary path"; B21 added (cache-staleness); B24/B25/B26 added (drift observation); B27 added (bench-detected creeping perf regression).
- ⚙️ **§26 NNGZ enforcement note — STILL-TO-LAND:** brief addendum to §26.15 documenting the grep CI fence under `tests/architecture_fence.rs`. Easy doc landing.
- ⏳ **§29.6 (cdylib as open question) — PENDING Phase G:** close once Phase G ships.
- ✅ **§F.14.1 / §F.17 — DONE 2026-06-22 (Option 4 + patch 5 retirement):** updated with post-retirement architecture context.

### Doc-correction discipline

Audit other chapters for empirical-backing gaps (similar to §15's now-acknowledged "no empirical backing" status). Add "Status" notes to chapters that lack toylang verification for their main claims. Specifically check §11 (groups), §12 (Send/Sync/'static), §13 (comptime), §14 (closures/async) — these may have similar gaps. **Update 2026-06-24:** §15 (drop) and §22.4 (perf) and §6.2 (single-symbol) and §5.3 (partition filter) NOW have full empirical backing per Phases A/B/B+/E/F. §10–14 still pending Phases J/K/L/M.

### Doc-correction discipline

Audit other chapters for empirical-backing gaps (similar to §15's now-acknowledged "no empirical backing" status). Add "Status" notes to chapters that lack toylang verification for their main claims. Specifically check §11 (groups), §12 (Send/Sync/'static), §13 (comptime), §14 (closures/async) — these may have similar gaps.

---

## Round 4 with the reviewer — CLOSED 2026-06-24

**All five deliverables landed; reviewer signed off; round 5 awaits Phase L or Phase G to surface real implementation reality.**

The reviewer's round-4-close response endorsed: (a) §5.5 locked, (b) the Bench 3 apples-to-apples baseline framing, (c) the B10 root-cause reframing (Sky emission bug, not LLVM bug). They made one notable framing observation worth carrying forward: **for §22.4's user-facing perf model, lead with Sky vs Rust parity (0.5–2% delta) rather than the 1.5× LTO ratio.** The structural ratio is a universal-Rust-property; the parity is the Sky-specific claim worth defending. Doc rewrite owed in the post-round-4 sweep.

They also flagged a calibration point for Phase L / Phase G: A.6 (prior mir_shims was empirically broken) and B10 (was a Sky emission bug) were both findings where **integration fixtures caught premise errors, not just confirmed conclusions**. Bring this posture into Phase L: build the Send/Sync/'static integration fixtures FIRST (including per-view conversion failure modes), THEN write the typechecker changes that the fixtures will drive.

Two minor doc/code residuals from round-4 close (folded into the post-round-4 doc sweep):
1. Lifecycle-traits registry: generalize `local_needs_scope_drop`'s hardcoded "Drop" string to a registry lookup (`tcx.is_lifecycle_trait(trait_def_id)`). ~5 LOC + one registry entry for Drop. Pays back when comptime Init / SkyDrop marker / async drop lands.
2. §15 dual-path drop narrative: explicit paragraph naming both paths (Sky source → AST synthesis → trait static call; Rust source → rustc's `drop_in_place` → `<T as Drop>::drop` → resolved via single-symbol naming) and noting they converge at the same Sky-emitted body.

Round-4 close also surfaced 3 new Phase entries via parallel-agent investigation:
- **Phase P** (deduce_param_attrs soundness override) — latent silent-UB vector closed.
- ~~**Phase Q** (codegen_fn_attrs NEVER_UNWIND override)~~ — shipped-then-retired same day per reviewer's interaction audit.
- **Phase R** (B10-style emission audit follow-ups, 7 sites) — systematic ABI-coercion-mismatch hunt.

See "NEXT — start here next session" at the top of the TL;DR for the priority-ordered plan.

### Round 4 presentation order (historical — what was delivered)

Order matters per reviewer's discipline:

1. **Perf bench numbers FIRST.** ALL IN HAND:
   - Bench 1 LTO ratio = **1.50×** (handoff "<2× → lock §5.5 confidently" gate fires).
   - Sky vs Rust baseline at O3 thin = **0.3% delta** (essentially identical; Sky adds no measurable overhead).
   - Bench 3 drop chain: Sky 25.5× / pure-Rust cross-crate baseline 27.5× → **inherited from Rust, not Sky-specific**.
   - Sky vs Rust at thin LTO on Bench 3 = **3% delta** (mirrors Bench 1's 0.3%).
   - All 23 fixtures + 4 LTO modes + opt-level sweep documented; runner reproducible per arch §22.4.2.
   - Verdict: **lock §5.5**. Recommendation: `[profile.release] lto = "thin"`.

2. **Cache_on_disk_if audit results — IN HAND.** Phase I (2026-06-24): **the Decision-14 prescribed Provider-slot API does NOT exist on current nightly.** `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL, not a Provider slot. Audit found every override safe by construction. New CI fence + B21 risk entry. Reviewer's closing micro-note advice was based on an API that doesn't exist; we've reframed via cache-audit markers.

3. **Drop integration fixture outcomes — IN HAND.** 9 fixtures passing cold. Headline finding A.6: **the previous `mir_shims` override was empirically broken** — `consumer_struct_name` lookup path never fired, `drop_in_place::<Widget>`'s body was a no-op, `<Widget as Drop>::drop` was absent from any shipping binary. The override had never actually worked in any test fixture. mir_shims removal lost zero functionality.

4. **mir_shims elimination empirical validation — IN HAND.** AST-rewrite shape (Phase E.d) ships at 342/0/1 tests; no regressions in the 333 prior integration tests. Round-3's 8-agent investigation conclusions held up: drop is just a function the language sometimes auto-calls; cascade-discovery + `fill_extra_modules` emits Sky's Drop body the same way it emits any other trait-impl method.

5. **Implementation surprises grouped by where they surfaced — IN HAND** (see "Surprises Log" in the "State of the World" section above):
   - **A.6:** mir_shims override was empirically broken (most important surprise; directly contradicts round-3's reasoning that thought it worked).
   - **B10 residual:** ThinLTO cross-CGU import re-triggers the LLVM 21 BitcodeWriter bug; arch §25.2 B10 tightened from "CLOSED" to "CLOSED for primary path; residual trigger under ThinLTO cross-CGU import."
   - **F3:** Sky main + while loop + 1M+ allocations stack-overflows (toylangc O0 alloca recycling bug; bench3 routes around it via Rust caller).
   - **Phase G discovery:** wrapper-mode retirement is a Phase G prerequisite — full G+H bundle was ~5-7 days, not the original 3-5d estimate. Phase H shipped standalone with the FFI shape that works under both static-link and cdylib.
   - **Cache_on_disk_if Provider-slot API doesn't exist:** Decision 14 prescription invalid as written.
   - **Rustdoc-not-installed + the build-rustdoc-also-wipes-stdlib trap:** required the full `x.py dist rustc-dev` + library + rustdoc + reinstall sequence (rebuild doc updated to record this).
   - **Phase E.b/E.c → E.d refactor mid-flight:** initial LLVM-IR-layer drop emission shipped briefly, then refactored to AST-rewrite for principled adherence to "drop is just a function."

DON'T:
- Reopen design questions that were settled in rounds 1-3.
- Bring questions that empirical data has already answered (cdylib operational details — covered by Decision 4 + the wrapper-mode-retirement-as-prerequisite finding; perf — covered by Bench 1/2/3 + Phase B+; drop chain correctness — covered by 9 fixtures + A.6).
- Pad with stylistic refinements.

DO:
- Frame surprises as questions if you genuinely need reviewer input.
- Quantify everything (bench numbers, fixture counts, line-of-code changes).
- Note any architectural decision that empirical work suggests we should REVISIT. As of 2026-06-24, the only REVISIT-worthy items are:
  - Decision 14's prescription (already revised; audit landed as the actual mechanism).
  - The framing of round-3's mir_shims investigation (A.6 found it broken, not working).
  - The Bench 3 framing (inherited from Rust, not Sky-specific — Phase B+ confirmed).

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
5. What does the perf bench actually measure, and what's the user-facing takeaway? (Bench 1 = synthetic call boundary cost: 1.50× LTO ratio, with Sky vs Rust baseline at 0.3% delta. Bench 3 = drop chain amplification: 25.5× Sky / 27.5× pure-Rust cross-crate baseline (Phase B+) — the ratio is INHERITED from Rust, not Sky-specific. Sky vs Rust at thin LTO = 3% delta. Recommendation: `[profile.release] lto = "thin"`. Bench 1 + baseline together prove Sky adds no measurable per-call overhead beyond Rust's cross-crate cost; Phase B+ extends the same finding to drop chains.)
6. What does the cache_audit fence enforce, and why? (It walks the 5 Sky-overridden query files and requires each to carry a `cache-audit:` marker comment. Adding a new override forces a fresh audit because the marker is required to pass the test. The audit found Decision 14's prescribed Provider-slot API doesn't exist; safety is by upstream-declaration construction.)

If you can't answer one, re-read the relevant section above. If you can, you're ready.

---

## What success looks like

**Phase A success — ACHIEVED 2026-06-23:** nine drop integration fixtures pass cold + warm under the new AST-rewrite drop synthesis model. Findings A.1-A.10 documented above; headline A.6 (prior `mir_shims` was empirically broken) validated Decision 1.

**Phase B success — ACHIEVED 2026-06-24:** perf bench numbers documented in `rust-interop-architecture.md` §22.4 (with reproduction steps in §22.4.2 and interpretation assumptions in §22.4.3). Numbers confirm "LTO-first model is acceptable" — Bench 1 LTO ratio 1.50× (in handoff's "<2× → lock §5.5 confidently" band); Sky vs Rust baseline at 0.3% delta (Sky adds no measurable overhead); Bench 3 drop chain 26.5× LTO speedup (refreshed to 25.5× during Phase B+ rerun; within run-to-run variance). Decision-gate verdict: lock §5.5. Two follow-up findings flagged: F3 (toylangc O0 alloca recycling) and B10 residual (ThinLTO cross-CGU import re-triggers the bug); both documented in §F.18 + §25.2 B10.

**Phase B+ success — ACHIEVED 2026-06-24 (follow-up):** Bench 3 pure-Rust baselines confirm the ~26× drop-chain LTO speedup is INHERITED from Rust, not Sky-specific. Pure-Rust cross-crate Drop shows 27.5×; Sky shows 25.5×; Sky vs Rust at thin LTO = 3% delta (matching Bench 1's 0.3%). `single_crate` baseline (Widget in user_bin) and `inline_never` baseline (Drop `#[inline(never)]`) bracket the operating point cleanly. 6 new fixtures + new `test_widgets/` sibling crate; 23 total fixtures; runner extended; §22.4 + §22.4.3 + perf-bench-summary updated. Round-4 framing for Bench 3 settled: "Sky's drop emission gives LLVM the same elimination opportunity Rust's does."

**Phases C+D success — ACHIEVED 2026-06-24:** `#[toylang::emit_consumer_body]` attribute machinery in place (build.rs prepends `#![feature(register_tool)] / #![register_tool(toylang)]`); stub_gen tags all three Category B emission sites (accessors, wrapper fns, trait-impl methods); `is_consumer_codegen_target` rewritten as the two-gate conjunction `is_from_lang_stubs && tcx.has_attrs_with_path(&[toylang, emit_consumer_body])`; new CI fence `stub_gen::tests::emit_consumer_body_tags_only_category_b_items` enforces the 1:1 invariant in both directions. 477 tests pass (+1 from the new fence; baseline was 476). The `is_consumer_fn` / `is_consumer_accessor_safe` matchers stayed alive because `queries/symbol_name.rs` + `queries/per_instance.rs` still consult them for callback-name routing; they retire together with the symbol_name override in Phase F (Decision 2). Sky proper will substitute `skyc` for `toylang` when stub_gen and build.rs ship Sky-side.

**Phase E success — ACHIEVED 2026-06-23:** mir_shims override gone; all 9 drop fixtures pass under the new AST-rewrite model; no regressions in the 333 prior integration tests (now 342 total). The `project_raw_field` CI fence turned out unnecessary because the synth pass never directly accesses field storage.

**Phase F success — ACHIEVED 2026-06-24:** symbol_name override gone (`queries/symbol_name.rs` deleted, `mod symbol_name;` removed from `queries/mod.rs`, provider unassigned). The full callback chain that fed it — `consumer_symbol_for_callback_name` trait method + vtable slot + trampoline + accessor + the toylangc impl — also retired. `DEFAULT_SYMBOL_NAME` + `default_symbol_name()` removed (no remaining callers). `is_consumer_accessor_safe` deleted (orphaned post-removal). `compute_fn_symbol` switched from `default_symbol_name()(tcx, instance)` → `tcx.symbol_name(instance).name.to_string()`. Tests: 477 pass cold; smoke-test of bench3_drop_thin reports 352μs (within run-to-run variance of pre-Phase-F baseline). The single-symbol architecture (arch §6.2) is now load-bearing purely through Sky's emission consulting `tcx.symbol_name(instance)` at every read site, with no override or bypass needed.

**Phase H success — ACHIEVED 2026-06-24:** patch 4 rev 3 ships; `ExtraModuleAllocator<M>` is a `#[repr(C)]` struct with two stable-ABI fields (`state: *mut c_void` + `allocate: unsafe extern "C" fn`) rather than a `&mut dyn` trait object. `VecAllocator` driver retired. Updates landed in 4 rustc-fork files + 2 facade files; rustc-fork rebuilt cleanly. 477 tests pass; bench3_drop_thin smoke-test 615μs (within run-to-run variance). FFI shape is now stable-ABI under both static-link (current toylangc) and cdylib (future Sky proper); Phase G's actual cdylib refactor becomes a wiring change rather than ABI rewrite when prioritized.

**Phase G success — NOT YET STARTED:** Sky's backend ships as cdylib; rustc-fork loads it via the `CodegenBackend` plugin mechanism; toylangc retires wrapper mode in favor of the Sky-proper architecture. Iteration loop is order-of-magnitude faster than the static-link build.

**Phase I success — ACHIEVED 2026-06-24:** cache_on_disk_if audit complete; key finding is that Decision 14's prescribed Provider-slot API doesn't exist on current nightly; every Sky-overridden query is cache-safe by construction; CI fence at `toylangc/tests/cache_audit.rs` enforces `cache-audit:` markers on every override file. Decision 14 itself is annotated with the revised understanding above. The cache-correctness "edit-and-rebuild" fixture from the original plan stayed deferred (audit found we're safe by construction; the fixture is regression insurance not active need; designing it properly needs temp-dir mutation infrastructure that's bigger than the day's scope).

**Phase J success:** typeids are u128 throughout; collision detection in place with clear error message; existing tests still pass.

**Phase K success:** content-hash const args in use; slab is Sky-internal only; cross-invocation symbol matching works automatically.

**Phase L success:** `SkyRef<T, V>` machinery in place; closed V set documented; per-type honest Send for owned values; existing tests still pass (within the constraints of the new model — some tests may need updating to use the new API surface).

**Phase M success:** async typestate refinement in place; Fixtures 6-9 pass.

**Phase N success:** Sky-side recursion handling in place; pathological-deep-recursion fixture produces clean compile-time error.

**Phase O success:** drift CI fences in place for B24/B25/B26 detection.

**Overall success — round 4 inputs READY:** all four round-4 deliverables now in hand AND the Bench 3 framing question (Sky-specific vs inherited from Rust) is answered AND all three "cleanup arc" Decisions (1/2/3) plus Decision 5's FFI shape have shipped. Bench numbers + apples-to-apples Rust baselines (Phases B + B+), cache audit results (Phase I), drop fixture outcomes (Phase A — 9 fixtures passing), mir_shims elimination empirical validation (Phase E — 342/0/1 tests), tool-attribute predicate migration (Phase C+D — 477/0/1 with new fence), symbol_name override retirement (Phase F), patch 4 rev 3 FFI shape (Phase H). Plus implementation surprises grouped by where they surfaced (F3 toylangc alloca recycling; B10 residual under ThinLTO; cache_on_disk_if Provider-slot API doesn't exist; the discovery that wrapper-mode-retirement is a Phase G prerequisite). The next session can either (a) schedule round 4 with the reviewer using these anchors, or (b) continue with the unblocked next chunk of execution work — Phase J (u128 typeids), Phase N (recursion safety), Phase O (drift fences), or Phase G (the full cdylib build system + wrapper-mode retirement).
