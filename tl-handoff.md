# Handoff: erw → Sky Quarter Of Work

Hi, future-you / junior engineer. You're picking up a multi-week project. This
document is your full briefing — read it end to end before you touch any code.
Sections are ordered "context first, action last."

If you only read one thing, read **§7 Where to start**.

---

## 1. The project in one paragraph

`erw` is a Rust-interop prototype currently embodied by two crates:
`rustc-lang-facade` (a reusable library that hooks into rustc via query
overrides) and `toylangc` (a toy consumer language that uses the facade to
prove out interop mechanics). The **architectural target** is **Sky**, a
memory-safe systems language whose compiler will use the same facade pattern.
The master design doc is `rust-interop-architecture.md` (5,148 lines). The
catalog of places where erw currently diverges from Sky is `course-correct.md`
(18 items). This project — driven by the plan at
`/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md` — adds
tests that exercise the "hard cases" (1b, 3, 4, 5, 6) from the
architecture doc's seven-case taxonomy. **As of Session 8, all seven cases are tested** (1a, 1b, 2, 3, 4, 5, 6).
Phase 2 C landed `impl rust_trait for toylang_type` end-to-end.
**Eleven course-correct items are done** (#1, #2, #4, #5, #6, #11,
#14, #15, #16, #17, #18) and #10 is partial.

**Session 11 layered the generic-vs-non-generic uniformity sweep on top**: four phases (A rename, B drop typecheck skip, C entry-point walk replacing the registry walk, F grep-based CI fence) closed all sites that branched on `type_params.is_empty()` in discovery + typecheck paths. Phase E (struct-shape ICE) was investigated — reproduced live on rustc 1.95.0-dev; concrete two-path recommendation in `phase-e-investigation.md`. **253/253 tests passing.**

---

## 2. The story so far (sessions before yours)

You should know the recent history because file names and concepts in your
work descend from it.

**Session 1 (course-correct.md authoring).** Read the architecture doc end
to end. Produced `course-correct.md` — 18 places in the prototype that
diverge from Sky's locked decisions.

**Session 2 (Approach A restoration — course-correct item #1).** The biggest
divergence: erw's "stage 3" migration replaced an Instance-keyed forked
`per_instance_mir` query with a DefId-keyed `optimized_mir` override
(Approach A → Approach B), and "stage 4" went further by retiring all rustc
fork patches to zero-fork. Sky **requires** Approach A because Sky's
arbitrary-typed comptime arguments cannot be substituted by rustc's
collector — only Sky's frontend understands Sky-typed values.

This session restored Approach A end-to-end. Three workstreams:
- **W1**: Rebuilt the 3-patch rustc fork (declare query, collector hook,
  default-None provider). `~/rust` checkout bumped from rustc 1.86 (Jan 2025)
  to rustc 1.95.0-dev / commit `d940e568` (matching `nightly-2026-01-20`).
  Build took 14:43 with `download-ci-llvm = true`. Installed as toolchain
  `rustc-fork` at `$HOME/rust/build/host/stage2`.
- **W2**: Facade switched from `optimized_mir.rs` to `per_instance.rs`.
  Trait method `collect_generic_rust_deps` now takes `ty::Instance<'tcx>`
  instead of `LocalDefId`.
- **W3**: Toylang substitutes `instance.args` concretely Sky-side. Retired
  `oracle::ActiveParamMap` thread-local. `debug_assert!(!instance.args.has_param())`
  added in `build_dependency_body`. `ResolvedType::TypeParam` arm in
  `try_resolved_to_rustc_ty` (oracle.rs:292) now panics unconditionally
  ("Sky-side substitution should have replaced it").

After W3: 210/210 tests pass. Plus a positive-evidence probe test
`test_approach_a_callback_log_shape` was added — asserts the
`CollectGenericRustDeps` log entries carry an `args_fingerprint` field, which
is sharp regression protection for the trait-signature shape.

**Session 3 (quarter-of-work Phase 1 Workstream S start).** Wrote this plan.
Started Phase 1. Landed three sub-commits:
- **S.1**: Sidecar format spec doc + `SidecarHeader` types.
- **S.2**: Bincode serialization + BLAKE3 checksum + serde derives + `BTreeMap` promotion.
- **S.3**: Sidecar written at end of rlib compile's `after_rust_analysis`.

Test counts after Session 3: 225/225 (81 unit + 129 integration + 15
standalone). 121 `.sky-meta` sidecar files materialize during the test run.

**Session 4 (this session — finished Workstream S, oracle sweep, Workstream A).**
Three major pieces landed plus one attempted+rolled-back, then re-tried after
removing the blocker:

- **S.4** (sidecar load): Facade walks `tcx.crates(())`, finds Sky-marked rlibs
  via `is_from_lang_stubs`, reads adjacent `.sky-meta` files (with cargo's `lib`
  prefix strip), fires new `LangCallbacks::on_sky_lib_loaded(tcx, crate_name,
  bytes)` trait method (Option B per §7.3 — facade doesn't know consumer payload
  shape). Toylang's impl deserializes via `sidecar::deserialize_sidecar`, stores
  in new `ToylangState.upstream_registries: BTreeMap<String, ToylangRegistry>`.
  New `CallbackLog::OnSkyLibLoaded` variant + `test_s4_sidecar_load_smoke`.
- **S.5** (sidecar determinism CI invariant): `test_s5_sidecar_determinism`
  builds the `arithmetic` fixture twice with isolated per-run `CARGO_TARGET_DIR`s
  and byte-compares the two `.sky-meta` files. Panic on mismatch points at
  common causes (HashMap-leak, bincode-cfg drift, timestamp leak).
- **Workstream A first attempt — REVERTED.** A.1+A.2+A.4 prototype compiled
  but tripped a deeper blocker: `oracle::find_extern_fn_def_id` walks only
  LOCAL HIR items. At user-bin compile every `extern "C" { fn ... }` decl is
  in the upstream `__lang_stubs` rlib, not local. Lookup panics. Reverted A
  edits, documented findings in `workstream-a-scope-notes.md`.
- **Oracle cross-crate sweep** (the blocker fix): completed stage-5a's
  half-done refactor. Added cross-crate fallbacks to `find_extern_fn_def_id`
  + new `find_extern_fn_in_stub_rlib` + `find_stub_fn_in_stub_rlib` +
  `find_wrapper_fn_def_id`. Same pattern stage 5a applied to
  `resolve_rust_path`. New `CallbackLog::OracleCrossCrateProbe` variant +
  isolated `oracle_probe/` fixture + `test_oracle_cross_crate_extern_fn_lookup`
  asserting the fallback fires at user-bin compile.
- **Workstream A re-attempted — LANDED.** With oracle blocker gone, A.1+A.2+A.4
  shipped. Rlib compile produces no toylang `.o` (`llvm_paths = None`). User-bin
  compile is the codegen site. Discovery shifted from upstream-CGU walk (finds
  zero items at user-bin) to **registry-driven**: iterate `registry.functions`,
  push `ToylangInstance` per non-generic body-bearing fn, look up `pub fn` shell
  DefId in `__lang_stubs`, transitively `walk_and_stash_internal_callees` to
  surface generic monomorphizations like `wrap<i32>`. New `stub_def_id` field
  on `ToylangInstance` lets codegen build `Instance::new_raw(def_id, empty_args)`
  for extern wrapper emission. See `workstream-a-scope-notes.md` for the
  detailed completion notes including the two key unlocks.

Test counts after Session 4: **228/228** (81 unit + 132 integration + 15
standalone). Course-correct items #11 and #15 are done. Sky-aligned shape
at the SHAPE level for single-file programs; literal Sky shape (per-library
publishing) lands in Phase 3.

**Session 5 (Phase 3 multi-crate + Phase 1 D + cleanups + A.5).** Four
distinct pieces of work, all landed and committed:

- **Phase 3 (E.1–E.6) — multi-toylang-crate end-to-end.** E.1 (marker-based
  `__SKY_STUBS_MARKER` detection — course-correct #6); E.2 (`[toylang-dependencies]`
  manifest schema); E.3 (build orchestration fan-out per Sky lib); E.4
  (oracle multi-crate iteration); E.5 (typecheck cross-crate name
  resolution via effective_registry merge); E.6 (codegen-side upstream
  iteration + new `test_case6_basic_multi_crate` — the first test
  exhibiting architecture §5.5 literally: library compile produces rlib +
  sidecar only; binary compile codegens every reachable Sky item across
  all libs from the AST in the sidecar). Hard-won: a 0%-CPU hang at the
  user-bin compile diagnosed via `sample` as @GCMLZ re-entrance —
  `lang_symbol_name` calls `call_notify_concrete_entry_point` from
  inside `generate_and_compile`, which already holds MUTABLE_STATE;
  std::sync::Mutex isn't reentrant. Fixed with a thread-local
  fat-pointer state-bypass in the facade (`lib.rs`). Course-correct
  items #6 and #16 done.

- **Phase 1 D — rust_caller manifest field + Cases 1a/1b/3/5.**
  `[project.rust_caller]` optional path to a Rust source file that
  supplies the binary's `fn main`. `write_main_shim` prepends the
  standard extern-crate preamble and appends the rust_caller's content,
  replacing the default `__toylang_main` shim. Four new integration
  tests: case1a (Rust calls non-generic Sky), case1b (Rust calls
  generic Sky with rustc-known T — FIRST test exercising Approach A with
  non-empty `instance.args`; required extending the CGU walk in
  `generate_with_tcx` to synthesize `FnItem` from registry + instance.args
  for upstream generics), case5 (Rust → Sky → different Rust lib via
  Vec stdlib), case3 (Rust → Sky → trait dispatch back to Rust top's
  Clone impl; required a small fix in `codegen_extern_wrapper`'s
  `rust_ret_type` arm for direct-coerced RustType returns).

- **Course-correct cleanups #14 + #2.** #14: retire CARGO_PRIMARY_PACKAGE
  env-var gate at the top of `run_wrapper_mode`; the manifest-vicinity
  lookup below already does the right thing. #2: retire the
  `(Linkage::External, Visibility::Default)` post-partition mutation in
  `partition.rs` — Workstream A's binary-codegen model means the
  Phase-6 wrappers and the toylang code that calls them live in the
  same final binary at link time, so default Hidden linkage works. The
  §B2 timing risk dissolves.

- **A.5 byte-identical pass-through invariant** (architecture §4.4).
  New `test_a5_byte_identical_pass_through` standalone test compiles a
  small Rust corpus (add, struct, generic, trait_impl) with both
  `+nightly-2026-01-20` (vanilla) and `+rustc-fork`, emits LLVM IR,
  normalizes the disambiguator hash + module ID + LLVM-version
  metadata, asserts byte-equality. Hard CI guard against
  Sky's-machinery-leaks-into-pure-Rust-compiles regressions. Skips
  gracefully when vanilla toolchain isn't installed.

Test counts after Session 5: **243/243 passing** (90 unit + 137
integration + 16 standalone).

**Session 6 (Tier 1 sweep — #4, B, #5).** All three Tier 1 items landed
in one session as mechanical refactors:

- **#4 codegen-wrapper emission channel** (commit `6c19e53`). Wrapped
  rustc's own ongoing-codegen `Box<dyn Any>` in
  `LangOngoingCodegen { inner, lang_obj_path }`; `codegen_crate`
  returns the wrapper, `join_codegen` downcasts and extracts both.
  Retired `FacadeMutableState.lang_obj_path` +
  `set_lang_obj_path` / `get_lang_obj_path`. The inline-Inkwell
  rewrite the architecture eventually wants is deferred (Sky's full
  codegen still goes through the consumer's `generate_and_compile`
  callback); the cross-phase channel itself is gone.

- **Workstream B oracle TypeParam tolerance** (commit `01d98fd`).
  `oracle::rust_trait_method_return_type` /
  `rust_trait_method_param_types` now detect TypeParam in Self or
  any type arg and return a structured "deferred" error instead of
  panicking via `try_resolved_to_rustc_ty`. New
  `RustTypeLookupContext::DeferredTypeParam` +
  `UnresolvedRustType::is_deferred`; the `TypeResolveError` enum
  gained a `RustTypeDeferred` variant; the Check 5 typecheck loop
  silently skips deferred entries. ~80 LOC + 3 unit tests for the
  new `contains_type_param` helper.

- **#5 hook point: after_expansion not after_analysis**
  (commit `e81cf6d`). One-line driver.rs swap. Toylang's oracle
  queries (fn_sig / adt_def / module_children) are all available at
  expansion-time. The handoff's 3–5-day estimate budgeted for a
  worst-case split (Sky-side parse at after_expansion, rustc cross-
  check at after_analysis); in practice it was a hook-point swap
  with a comment refresh.

Test counts after Session 6: **246/246 passing** (93 unit + 137
integration + 16 standalone).

**Session 7 (Tier 1 mechanical cleanups — #17, #18, #3 audit).** Two
mechanical items closed; #3 deferred after an audit revealed it's
deeper than the docs estimated.

- **#18 build.rs comment refresh** (commit `1c27b09`). The stale
  rationale ("toylang's emitted `.o` (bundled into the rlib) calls
  into rust_dependencies symbols at the OBJECT-FILE level") died with
  Workstream A. Rewrote it to explain the actual current load-bearing
  reason: Phase 1 D's `rust_caller.rs` lives inside user_bin and names
  rust_dependencies directly (`use serde::...`), so cargo must pass
  `--extern serde=...` to user_bin's rustc — which requires the
  direct re-listing. Without it, the compile fails at "unresolved
  import" before linking.

- **#17 stub_gen `is_generic` special-casing** (commit `4f5cc8a`).
  Two of the four `!is_generic` branches were purely cosmetic
  (the impl-block header `impl Foo` vs `impl<T> Foo<T>`, the wrapper-
  fn header `pub fn foo(…)` vs `pub fn foo<T>(…)`) — unified via new
  `generics_for_impl_block` + `fn_generics_clause` helpers that return
  empty token streams for the zero-param case. Two divergences stay
  and are now explicitly documented as gated by external constraints:
  struct shape (rustc debuginfo-walker ICE on opaque non-generic
  ADTs with any source field) and extern decls (`extern "C"` doesn't
  permit generics; Sky retires this whole mechanism when #4's inline-
  codegen rewrite lands).

- **#3 `cgu_stash.rs` retirement — AUDITED, NOT LANDED.** The audit
  found `llvm_gen.rs:1938` still consumes `upstream_cgus(tcx)` for
  two paths that don't go through Workstream A's registry-driven
  discovery: (1) accessor-method discovery via `opt_associated_item`,
  and (2) Case-1b generic consumer fns instantiated from Rust call
  sites (`__lang_stubs::wrap::<i32>(42)` from a `rust_caller.rs`,
  where the registry walk intentionally skips generics because it has
  no caller args). Retiring the stash requires moving both discovery
  paths to the `after_expansion` queue the architecture wants —
  that's bundled with #7 (sidecar-loaded universe) and #9 (symbol_name
  discovery retirement), not a mechanical cleanup. #3 stays paired
  with that deeper rebuild.

Test counts after Session 7: **246/246 passing** (no test count
change — both refactors are byte-equivalent to the old emission).

**Session 8 (Phase 2 C — Case 4 end-to-end).** The toylang language
feature `impl rust_trait for toylang_type` landed across seven
sub-steps, closing the seven-case interop taxonomy.

- **C.1 parser** (commit `6e9e7a8`): top-level `impl <Path> for
  <Ident> { fn … }` recognised; `&self` elevated to an explicit
  `self: &Struct` parameter (CLAUDE.md "prefer self as another
  parameter"); 3 new parser unit tests.

- **C.2 registry** (same commit): `ToyImpl { trait_name,
  self_type_name, methods: Vec<ToyImplMethod> }`,
  `ToylangRegistry.trait_impls: Vec<ToyImpl>` (deterministic by
  source order).

- **C.3 type resolver + C.4 stub_gen** (commit `5b1babd`):
  Check 6 type-resolves each method body via the existing
  `resolve_fn_body` path (no method-specific code path needed);
  stub_gen emits one `impl <Trait> for <SelfType> { fn name(&self,
  …) -> Ret { unreachable!() } }` block per `ToyImpl`. 1 new
  stub_gen unit test.

- **C.5 llvm_gen + C.6 symbol_name + C.7 fixture** (commit
  `b56cf4c`):
  - New facade helper `is_consumer_trait_impl_method` walks
    `opt_associated_item` + `impl_opt_trait_ref` to discriminate
    trait-impl methods on consumer types.
  - `is_consumer_accessor_safe` now excludes them (early return when
    `impl_opt_trait_ref` is Some) so accessor and trait-impl callback
    names don't collide.
  - symbol_name routes trait-impl methods to a new callback shape
    `__impl_method__<Self>__<Trait>__<m>` → consumer mangles to
    `__toylang_impl__<Self>__<Trait>__<m>`.
  - New oracle helper `find_trait_impl_method_def_id` walks
    `tcx.all_impls(trait_def_id)` to find the impl method's DefId
    for Instance construction.
  - `populate_toylang_instances_from_cgus` iterates
    `registry.trait_impls`, pushes `ToylangInstance` per method so
    the existing codegen pass emits both internal and Rust-ABI
    extern wrapper.
  - Auto-deref bug fix in type_resolve + llvm_gen: `Ref { Struct }`
    receiver auto-derefs for `self.field` access. The codegen path
    loads the receiver pointer before GEPing the field — earlier the
    GEP indexed into the receiver's stack slot itself and printed
    garbage (1835707572 instead of 42 the first time the fixture
    ran).
  - `case4_sky_impl_rust_trait/` fixture: Widget + impl Clone + Sky
    accessors; rust_caller calls `Clone::clone(&w)` via trait
    dispatch, prints id via Sky `id_of`.

Test counts after Session 8: **251/251 passing** (97 unit + 138
integration + 16 standalone).

**Session 9 (honest fixture audit + sharpening).** Re-reading the case
fixtures against the architecture doc's worked examples revealed that
several of them are *partial* tests of the architectural case rather
than the sharp version. The seven-case taxonomy fixtures were meant
to make drift toward Approach B "fail loudly"; a weak fixture for the
hardest case doesn't do that.

Audit findings (pre-Session-9 state):

| Case | Architectural shape | Pre-9 fixture | Sharp? |
|---|---|---|---|
| 1a | Rust → Sky non-generic | non-generic call | ✅ |
| 1b | Rust → Sky generic w/ **Rust-defined** T | `identity::<i32>(42)` — stdlib type | ⚠️ exercises Approach-A mechanism (non-empty `instance.args`) but with a stdlib type, not a user-struct (the architectural distinguishing case). case3 below covers the user-struct path. |
| 2 | Sky → Rust generic | existing fixtures (`Vec::new<i32, Global>()`) | ✅ |
| 3 | Rust → Sky generic → trait dispatch back to Rust | `clone_it::<MyCounter>` with MyCounter Clone-derived in rust_caller | ✅ — the genuinely sharp test |
| 4 | **Sky top** → Rust **generic** intermediary → Sky impl of Rust trait | **Rust** top → direct `Clone::clone(&w)`, no Rust generic middle, Sky top inverted to Rust top | ❌ wrong shape — closer to case1a+trait-dispatch than to architectural Case 4 |
| 5 | Rust → Sky **generic** middle → different Rust | Sky middle is non-generic `count_three()` | ⚠️ structurally case-1a-layered-over-case-2; the "1b-layered-over-2" hardness isn't exercised |
| 6 | Sky → Rust **generic** middle → different Sky | both Sky pieces non-generic, no Rust middle | ⚠️ tests "Sky lib depends on Sky lib" but not the architectural difficulty (rustc walking a Rust generic body and dispatching to the other Sky's trait impl) |

What's actually exercised at the mechanism level pre-Session-9:

- ✅ Approach A with non-empty `instance.args` — case1b (i32), case3 (MyCounter).
- ✅ Sky → Rust trait dispatch with substituted Self — case3.
- ✅ Sky stub rlib impl block compiles + rustc dispatches to it — case4 (pre-9).
- ❌ **Rustc walks an extern Rust generic body, sees a trait method
  call, dispatches to a Sky-defined impl.** Load-bearing for Case 4
  *and* Case 6 architecturally; **no pre-9 fixture exercised it.**

Session 9's sharpening work:

- New `some_rust_lib/` test crate ships a true Rust generic
  intermediary (`pub fn duplicate<T: Clone>(x: &T) -> T { x.clone() }`)
  with no `extern "C"` decoration — the architecturally important
  shape that `test_helpers` cannot carry because its surface is
  C-ABI-only.
- case4 rewritten with the correct shape: Sky top, Rust generic middle
  (`duplicate::<Widget>(&w)`), Sky impl of `Clone for Widget`. Now
  exercises rustc walking `duplicate<Widget>`'s body, queueing
  `<Widget as Clone>::clone`, and firing Sky's emission path.
- case5 rewritten with a generic Sky middle (`store_in_vec<T>(x: T) ->
  usize` — 1b layered over 2).
- case6 rewritten with a Rust generic intermediary between the two
  Sky crates — Sky-app calls Rust `duplicate::<Pair>` which dispatches
  to Sky-lib's `Clone for Pair`.

Test counts after Session 9: **251/251 passing** (97 unit + 138
integration + 16 standalone) — the sharpening is byte-equivalent at
the test-count level because the existing case4/5/6 tests were
already counted; what changed is *what they actually test*.

Honest follow-ups deferred from Session 9 (small, real, documented):

1. **8-byte two-field struct return type via Rust ABI through Sky's
   extern wrapper.** When case6 first ran with `Pair { first: i32,
   second: i32 }` (8 bytes), the extern wrapper for
   `__toylang_impl__Pair__Clone__clone` came back with signature
   `define { i8, i8 } @… (ptr)` — only 2 bytes of return data.
   abi_helpers' coerced-return computation treats the 8-byte
   opaque-with-size layout as if it were 2 bytes somewhere along the
   pipeline. case6 was simplified to a single-i32-field `Box` to
   unblock; the two-field-struct gap stands. Not specific to Phase
   2 C — touches the generic small-struct return path that should
   also affect non-trait-method consumer fns. Investigate next.

2. **case1b user-struct variant.** case1b still uses `identity::<i32>`
   (stdlib type). A sharper variant would mirror case3's MyCounter
   pattern. case3 already covers the user-struct-as-T path mechanism,
   so case1b's weakness is mild; add a `case1b_user_struct` variant
   if/when defending against drift in this specific direction.

**Session 10 (export keyword — Sky §9 architectural property
enforced).** Until Session 9 toylang emitted `pub fn name(...) {
unreachable!() }` for every body-bearing fn in the stub rlib, so
rustc DID have DefIds for non-export items even though it never
walked their bodies. Session 10 closes that gap.

- New `export` keyword in front of fn / impl decls; parser sets
  `is_export: bool` on `ToyFunction` and `ToyImpl`. `main` is
  implicitly export (the Rust shim names `__toylang_main`). Structs
  are deliberately NOT gated — §10 needs opaque-with-size ADT
  presence for layout composition.
- stub_gen skips the `pub fn` shell for non-export body-bearing fns
  and the entire `impl` block for non-export impls. Check 2 in
  `after_rust_analysis` (stub-presence guard) restricted to export
  items.
- New `stub_gen::tests::non_export_body_bearing_fn_gets_no_stub_shell`
  is the CI fence: builds a registry with one export + one
  non-export fn, runs `generate`, asserts the non-export name never
  appears as a `fn` declaration. Mirror of A.5's spirit — a CI-
  enforceable fence against drift away from Sky §9's commitment.
- Fixtures migrated: case1a/1b/3/5 added `export` on the Sky fn the
  rust_caller calls; case4 marked `export impl Clone for Widget`;
  case6_lib exported make_box/box_value/impl Clone; two layout-probe
  fixtures (tg_of_*_layout) marked `export fn make_wrapper` because
  they assert the layout query fires at rlib-compile time, which
  needs the rustc-visible signature.

Test counts after Session 10: **252/252 passing** (98 unit + 138
integration + 16 standalone). All seven taxonomy cases continue to
pass under the stricter semantics — proves the discovery + codegen
path was already Sky-aligned at the mechanism level; what changed is
whether rustc can name non-export items at all (it can't, which is
the locked architectural claim).

**Session 10 post-audit: the generic-vs-non-generic uniformity
claim.** A natural follow-up question: are generic and non-generic
toylang fns handled exactly the same way? Honest answer:
**mostly, but not entirely.** Auditing the codebase, four sites
still branch on `type_params.is_empty()`:

| # | Site | Reason | Forced? |
|---|---|---|---|
| 1 | `populate_toylang_instances_from_cgus` registry walk | Skips generic fns at the top level — "no concrete args to substitute at the top level." Transitive generic discovery via `walk_and_stash_internal_callees` still works. | Implementation pragma |
| 2 | Check 5 eager typecheck (`after_rust_analysis`) | Skips generic fn bodies because resolving them needs concrete T. Generic bodies get typechecked lazily inside `codegen_internal_function`. | Implementation pragma |
| 3 | stub_gen `extern "C" { pub fn ... }` block | Rust's `extern "C"` syntactically doesn't permit generics. The non-generic case emits the extern decl; generic case omits it. | External (Rust syntax) |
| 4 | stub_gen `pub struct` shape | Rustc's debuginfo walker ICEs on opaque non-generic ADTs with any source field. Non-generic emits `pub struct Foo;`; generic uses `pub struct Foo<T>(PhantomData<T>)`. | External (rustc ICE) |

What's already uniform:

- **Approach A substitution mechanism**: `resolve_caller_from_instance`
  has a fast-path return for empty type_params but is otherwise
  identical — `instance.args` is just empty for non-generics.
- **Body codegen** once the body is resolved: `codegen_internal_function`
  operates on a resolved body with concrete types; doesn't care about
  genericity.
- **Symbol mangling**: generics get type-arg suffixes
  (`__toylang_internal_wrap__i32`); non-generics get no suffix —
  same code path, the suffix loop just iterates zero items.
- **Impl-block + wrapper-fn headers in stub_gen** (Session 7's #17
  work): unified via `generics_for_impl_block` /
  `fn_generics_clause` helpers that return empty token streams for
  zero params.

The honest framing of sites 1 + 2: they branch on `type_params.is_empty()`
as a *degenerate-case shortcut* for the semantically-correct
condition "skip items whose args are still abstract." For non-generics
the condition is never true; for generics at the top level it's
always true. Same outcome, but the code does the cheap check
instead of expressing the intent.

Sites 3 + 4 are forced by external constraints and stay until those
constraints lift — site 3 via course-correct #4's inline-codegen
rewrite, site 4 via a rustc bug fix or a different opaque-stub
mechanism.

### Session 11 — uniformity sweep landed

Per the focused plan at `tmp/claude-plan-2026-06-15-ccc8939f.md`, Phases A → B → C → F landed in one session. Sites 1 and 2 are gone; sites 3 and 4 remain (forced, as expected). Plus Phase E investigation answered the struct-shape ICE question with reproduction + recommendation.

- **Phase A** (commit `8faca57`): added `ToyFunction::has_abstract_args()` helper to `registry.rs`. Four call sites in `callbacks_impl.rs` renamed. Cosmetic, no behavior change.
- **Phase B** (commit `5a1e7d0`): generalized `DeferredTypeParam` from `{trait_name, method}` to a single `query: String` description. Added `contains_type_param` guards at the top of four ungated oracle entry points (`rust_free_fn_return_type`, `rust_free_fn_param_types`, `rust_method_return_type`, `rust_method_param_types`). Added a receiver-TypeParam short-circuit in `type_resolve.rs`'s `Expr::MethodCall` arm so `fn foo<T>(x: T) { x.clone() }` defers cleanly. Dropped the `has_abstract_args()` skip at Check 5 + Check 6 — generic toylang bodies now type-resolve at `after_rust_analysis` with deferred Rust queries silently skipped via `TypeResolveError::RustTypeDeferred`. Sharper diagnostics: Sky-side typecheck errors in generic bodies surface earlier.
- **Phase C** (uncommitted): replaced the registry walk in `populate_toylang_instances_from_cgus` with the §20.4 entry-point walk. Roots = local main (implicitly `is_export` per `parser.rs:382`) + local export free fns + local `is_export` impl-block methods + same for upstream registries loaded by S.4. Each root pushes a `ToylangInstance` and calls `walk_and_stash_internal_callees` for transitive non-export discovery. Generic exports stay non-roots (no concrete args at root); they reach codegen via Channel A (the CGU walk in `llvm_gen.rs:1995`), driven by caller-site Instances. **Net behavior change**: dead non-export non-generic local code is no longer emitted (aligned with §9.3). No fixture relied on it. The 5 `test_generic_*` fixtures that were expected to fail still do — the plan flagged this as unchanged.
- **Phase F** (uncommitted): new file `toylangc/tests/architecture_fence.rs` grep-scans `callbacks_impl.rs` + `type_resolve.rs` for unmarked `type_params.is_empty()` branches. The four remaining sites (`monomorphize_type`'s ty-subst fast path, the two `resolve_caller_from_*` substituted-args fast paths, `type_resolve.rs`'s FnCall arity check) carry inline `// arch-fence-allow: <reason>` markers so the test passes. Validated by stash-and-rerun — removing the markers makes the test fail loudly with line numbers. Same role A.5 plays for byte-identical pass-through and `non_export_body_bearing_fn_gets_no_stub_shell` plays for §9.
- **Phase E** (investigation, uncommitted scope note in `phase-e-investigation.md`): reproduced the documented ICE under current rustc by temporarily switching to `pub struct Foo(())` for non-generic types. Crash is `rustc_abi/src/lib.rs:1676:66` — `FieldsShape::offset(i)` index-out-of-bounds, called from `build_struct_type_di_node`'s closure that enumerates source `FieldDef`s, called from `build_generic_type_param_di_nodes` (because we're inside `Vec<ToyShip>`). The root cause: Sky's `layout_of` override returns `FieldsShape::Arbitrary { offsets: [] }` (len 0) but the source tuple-struct has 1 source field; the walker iterates with `.enumerate()` and panics. Two paths recommended: (1) a ~5-10 LOC upstream `rustc_codegen_llvm` patch clamping the loop to `min(source.len(), layout.fields.count())` — general defensive fix benefiting any plugin layout-override (cranelift, miri, future Sky); ~1 day to write + weeks of PR review; (2) migrate every Sky struct to `pub struct Widget(SkyOpaqueData<TYPEID>)` per §10.6 — substantial 5-10 day refactor touching stub_gen + layout + oracle + mir_shims + symbol_name. Recommendation: do (1) first; defer (2) until §13's comptime-type machinery is built anyway.

Test counts after Session 11: **253/253 passing** (98 unit + 1 fence + 138 integration + 16 standalone). Phases A and B are on `main`; Phase C edits + Phase F new test file + Phase E scope note are uncommitted at user request pending review.

## How the uniformity sweep was structured (now mostly landed)

| Phase | Site(s) | Approach | Estimate |
|---|---|---|---|
| **A** | 1, 2 (cosmetic) | Rename the predicate to `has_abstract_args()` so the call sites document the architectural intent rather than the shortcut. Body unchanged. Establishes the vocabulary for B. | ~2 hours |
| **B** | 2 (semantic) | Drop the type-params skip in Check 5. Call `resolve_fn_body` on generic bodies; reuse Workstream B's `RustTypeDeferred` error variant to silently skip queries the body can't answer until substitution. Net effect: generic toylang fns get Sky-side type errors earlier (at after_rust_analysis instead of at codegen). | ~1 day |
| **C** | 1 (architectural) | Rebuild discovery around Sky's after_expansion entry-point walk (§20.4). Start from main + exports + impl methods, transitively enumerate every call site's substituted Instance, push to the queue. Generic and non-generic items both arrive via the same channel for the Sky→Sky path. Rust→Sky generic instantiations (Case 1b/4/6) still flow through rustc's CGU walk — that's a separate, architecturally-justified discovery channel. | ~3–5 days |
| **D** | 3 (forced — coupled with #4) | When course-correct #4's deeper rewrite lands (Sky's `codegen_crate` walks the queue and emits via Inkwell inline), the extern-decl pattern dies entirely. stub_gen no longer needs to emit `extern "C" { pub fn ... }` blocks at all. Site 3 vanishes by becoming dead code. | bundled with #4 |
| **E** | 4 (forced — external) | Two options: (1) file a rustc bug + patch the debuginfo walker upstream so opaque non-generic ADTs with source fields don't ICE — then unify struct shape on `pub struct Foo(PhantomData<()>)`. (2) Investigate whether a different opaque-stub pattern dodges the ICE (the architecture mentions `SkyOpaqueType<const T: u64>` as the universal wrapper; a non-generic could be `SkyOpaqueType<typeid>` with empty type args). | ~1 day investigation, then either a rustc patch (~1 week) or the wrapper-migration (~3–5 days) |

**Recommended sequencing:** A → B → C in one Phase-A/B/C focused
session (~1 week total). D follows whenever #4 lands. E is a
research thread that can run independently in the background.

**Architectural-property fence:** the moment Phase C lands, add a
test that mirrors Session 10's commit pattern: walk
`registry.functions` + `registry.trait_impls`, assert there is NO
branch on `type_params.is_empty()` left in the discovery + typecheck
paths (grep-based in CI). Same role A.5 plays for the byte-
identical invariant and `non_export_body_bearing_fn_gets_no_stub_shell`
plays for §9.

---

## 3. The plan you're executing

Master plan: `/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md`.
Read it. It's the authoritative scope; this handoff document is the
narrative version.

Three phases, ~15 weeks total:

| Phase | Weeks | Scope | Cases unlocked |
|---|---|---|---|
| 1 | 1-7 | Sidecar (S) + binary-compile codegen (A) + typeresolver fix (B) + rust_caller fixtures (D) | 1b, 3, 5 |
| 2 | 8-12 | Toylang `impl rust_trait for toylang_type` language feature (C) | 4 |
| 3 | 13-15 | Multi-toylang-crate workspace + marker-based detection (E) | 6 |

**All three phases complete.** Phase 1: S.1–S.5 done, oracle cross-
crate sweep done, Workstream A done, Workstream B done (oracle
TypeParam tolerance, Session 6), Workstream D (rust_caller fixtures
for Cases 1a/1b/3/5) done, A.5 done. Phase 2: C.1–C.7 done (Case 4
via `impl rust_trait for toylang_type`, Session 8). Phase 3: E.1–E.6
done. The seven-case taxonomy is fully tested.

Detailed commit-by-commit schedule is in the master plan's "Sequencing
recommendation" table at the end.

---

## 4. Critical context you need

### 4.1 Approach A: why per_instance_mir matters

Read `rust-interop-architecture.md` §3.1 + §19.1 carefully. The
one-paragraph version: rustc's mono collector substitutes generic args
inline as it walks. For toylang's currently-supported generics (rustc-
representable types), Approach B works fine. For Sky's comptime args
(arbitrary Sky-typed values), rustc literally cannot represent them, so
substitution MUST happen Sky-side before MIR construction. Sky's only
viable path is Instance-keyed `per_instance_mir` where the consumer's
provider receives the concrete `Instance` and returns a fully-substituted
body.

`docs/historical/approach-a-reference/` has the historical Approach A
implementation extracted from before stage 3 retirement. Read its README;
it's a structural template for any future change to the substitution
direction.

The `debug_assert!(!instance.args.has_param())` in
`rustc-lang-facade/src/queries/per_instance.rs::build_dependency_body` is
load-bearing — if it fires during a test run, Approach B behavior has
regressed somewhere upstream.

### 4.2 The sidecar architecture (your current workstream)

Read `rust-interop-architecture.md` §7 + §8, then `docs/architecture/sidecar-format.md`
(I wrote it in S.1, it's the implementer-facing reference).

Picture: Sky libraries compile to **rlib + sidecar only** — no Sky `.o`.
The rlib contains Rust stub source (compiled by rustc) with
`unreachable!()` bodies. The sidecar (`.sky-meta`) is a binary blob
carrying the typed AST for every item in the library — exports AND
non-exports. The binary compile reads sidecars from every Sky-marked rlib
it loads, then codegens **every reachable Sky item across all libs** into
one consolidated `.o`.

This is course-correct items #11 + #15 in their locked end states. Phase 1
Workstream A is what actually moves toylang to this emission model.
Workstream S is the foundation it depends on.

### 4.3 The two-symbol architecture (Sessions ago, still relevant)

Toylang emits three layers of symbols for any item:
1. `__toylang_internal_<name>__<mangled_targs>` — toylang↔toylang ABI.
   Used by toylang's own codegen for toylang→toylang calls.
2. `__toylang_impl_<name>__<mangled_targs>` — Rust-ABI-coerced extern
   wrapper. Used when Rust code calls into toylang.
3. The rustc-mangled name (`__lang_stubs::wrap::<i32>`) — what Rust
   source sees. The facade's `symbol_name` override at
   `rustc-lang-facade/src/queries/symbol_name.rs:31-80` reroutes this to
   `__toylang_impl_*`.

Important for your work: **Sessions before you had a wrong mental model**
that called this a "symbol mismatch." It's not. The override at
`queries/symbol_name.rs` already makes Rust call sites resolve to the
right symbol. The REAL blocker is **cross-crate generic instantiation**:
toylang's `.o` is emitted at the rlib compile, before Rust call sites for
`wrap::<LocalThing>` exist at the user_bin compile. Workstream A's job is
to fix this by moving codegen to the binary compile.

### 4.4 The two compiles per build

When you run `toylangc build` on a project, cargo invokes rustc TWICE:
- **rlib compile**: compiles the `__lang_stubs` rlib. Crate name starts
  with `lang_stubs_`. Marker `is_downstream_of_stubs = false`.
- **user_bin compile**: compiles the binary that depends on
  `__lang_stubs`. Crate name is the user's project name. Marker
  `is_downstream_of_stubs = true`.

Each invocation is its own process — independent state, no cross-process
sharing. The callback log file `.toylang-build/callback.log` gets written
by whichever compile fires `generate_and_compile`. Today: rlib compile
writes it (llvm_paths = Some), user_bin compile doesn't (llvm_paths = None,
returns early at `callbacks_impl.rs:584`).

Phase 1 Workstream A INVERTS this. After A, the rlib compile produces NO
`.o` (and stops writing the log) and the user_bin compile produces ALL
`.o` (and writes the log).

The gate variable `is_downstream_of_stubs` appears at
`callbacks_impl.rs:75, 191, 214, 387` and `main.rs:145-195`. Workstream A
renames it `is_user_bin_compile` and inverts its semantics throughout.

---

## 5. Files you'll touch (organized by area)

### 5.1 Facade (`rustc-lang-facade/src/`)

- **`lib.rs`** — trait `LangCallbacks`, vtables, trampolines, `OnceLock`
  defaults, the `is_from_lang_stubs` / `is_consumer_codegen_target` /
  `is_consumer_accessor_safe` predicates. This is the integration boundary
  between facade and consumer (toylang).
  - For Phase 1 S.4: you'll add sidecar-loading machinery here.
  - For Phase 3 E.1: you'll replace `is_from_lang_stubs` with marker-based
    detection.
  - For Phase 2 C.6: you'll extend `is_consumer_accessor_safe` to handle
    trait-impl methods (`tcx.impl_trait_ref`).
- **`queries/per_instance.rs`** — Approach A's `per_instance_mir` provider.
  The `debug_assert!(!instance.args.has_param())` is here. Don't touch
  unless you really mean to.
- **`queries/optimized_mir.rs`** — DOES NOT EXIST. It was deleted in W2 of
  the previous session. Don't bring it back. (If you grep for
  "optimized_mir" you'll find historical references in comments — that's
  fine.)
- **`queries/symbol_name.rs`** — the Rust-mangled-name → `__toylang_impl_*`
  redirect. For Phase 2 C.6 you'll add trait-impl method handling here.
- **`queries/layout.rs`** — `layout_of` override for consumer types. The
  layout-probe tests grep its `[toylang] layout_of intercepted for: ...`
  stderr line. Don't add new eprintlns nearby without thinking about it.
- **`queries/partition.rs`** — CGU filter. Should be largely untouched in
  Phase 1; Phase 3 E.1's marker-based predicate will simplify it.

### 5.2 Toylang (`toylangc/src/`)

- **`sidecar.rs`** — Phase 1 S.1 + S.2 (already written). Read it.
  Contains `SidecarHeader`, `serialize_sidecar`, `deserialize_sidecar`,
  14 unit tests.
- **`toylang/callbacks_impl.rs`** — toylang's implementation of the facade
  trait. This is where most of Phase 1 Workstream A's work lands.
  Specifically: invert `is_downstream_of_stubs`, change `after_rust_analysis`
  to read upstream sidecars + populate registry from union, change
  `generate_and_compile` gating.
- **`toylang/registry.rs`** — `ToylangRegistry` + `ToyStruct` + `ToyFunction`
  etc. Phase 1 S.2 already promoted `HashMap` → `BTreeMap` for determinism;
  any new types you add here must derive `Serialize, Deserialize`.
- **`toylang/parser.rs`** — Phase 2 C.1's main work site (`impl` block
  parsing).
- **`toylang/ast.rs`, `toylang/typed_ast.rs`** — Phase 2 C.2's main work
  site (`ToyImpl` AST node, threading `ResolvedType::TypeParam` and friends
  through).
- **`oracle.rs`** — the rustc-querying helpers. Phase 1 B's site is
  `rust_trait_method_return_type` / `_param_types` at lines 665-768.
  Phase 3 E.4's site is `resolve_rust_path` at lines 117-124.
- **`build.rs`** — the orchestration that generates `.toylang-build/`.
  Phase 1 D.2 will add a `rust_caller.rs` copy step here. Phase 3 E.3 will
  add the multi-crate fan-out.
- **`manifest.rs`** — toylang.toml schema. Phase 1 D.1 adds
  `project.rust_caller`. Phase 3 E.2 adds `[[dependencies]]`.
- **`stub_gen.rs`** — generates the Rust stub source for `__lang_stubs/src/lib.rs`.
  Phase 2 C.4 adds `impl ::std::clone::Clone for #ident` block emission.
  Phase 3 E.1 will add the `pub const __SKY_STUBS_MARKER` (if not already).
- **`main.rs`** — entry point with two modes: orchestration (`toylangc build`)
  and wrapper-mode (rustc-driver). Phase 1 A.1 / A.2 touches
  `run_toylang_compile` here.
- **`llvm_gen.rs`** — large file. Toylang's LLVM IR emission. Phase 2 C.5
  adds trait-impl method codegen.

### 5.3 Reference materials

- **`rust-interop-architecture.md`** (repo root) — Sky's architecture doc,
  5,148 lines. The master reference for "what are we building toward?"
- **`course-correct.md`** (repo root) — the 18-item catalog of erw → Sky
  divergences. Items #6, #11, #15, #16 are the ones this plan touches.
- **`docs/architecture/sidecar-format.md`** — the spec for the file format
  you're implementing.
- **`docs/architecture/rust-interop-guide.md`** — erw's current architecture
  guide. Outdated in some places (still describes Approach B in spots),
  but the cross-cutting invariants section (@SyMINCZ, @ELASZ, @GCMLZ,
  @ACRTFDZ, @TCHAPZ, @RTMEIZ, @UTAIRZ, @MBMRVZ, @IVTDBTZ, @TVIMDGAZ,
  @ETASTZ, @DPSFDOZ) is still load-bearing. Read those — they're traps.
- **`docs/historical/handoff-optimized-mir-migration.md`** — the
  forward-direction (A→B) handoff. Read it in reverse to understand what
  the W1-W3 work undid.
- **`docs/historical/approach-a-reference/`** — extracted snapshots of
  Approach A's implementation before stage 3 retired it.
- **`docs/historical/rebuilding-rustc-fork.md`** — procedure for rebuilding
  `~/rust`'s fork. If you need to bump nightly, read this.

---

## 6. Discipline and conventions

Read `CLAUDE.md` (both user-global and project) in full. The
non-negotiable rules:

- **No `cd && cargo`.** Use `cargo --manifest-path /absolute/path/Cargo.toml`.
  `cd` is OK only when the user explicitly asks.
- **Don't pivot unilaterally.** If you discover the plan won't work as
  written, STOP and ask the user before changing direction.
- **Don't make temporary debug programs.** If you need a probe, add a test
  case. The probe pattern I used in this session: add an `eprintln!` with
  `[PROBE]` prefix, run, remove. NEVER write a `tmp_debug.rs`.
- **Don't use `git checkout -- file` to revert.** Use `git diff` then apply
  the diff in reverse manually.
- **Always pipe build/test output to a fixed tmp file per session.** This
  session used `./tmp/quarter-of-work.txt`. Don't rotate file names. Don't
  chain `cargo test | tail`. Run the test, then grep the file separately.
- **Use relative paths in `cargo` commands.** Not `/Volumes/V/...` or
  `/Users/verdagon/...`.

The non-obvious rule from this session:
- **`integration-projects-cache` stale-cache gotcha.** When integration
  tests fail in seemingly random ways (callback log empty, expected
  callbacks missing, etc.), wipe the shared cargo cache first:
  `rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache`.
  Then re-run. This is a real source of false negatives and ate me ~30
  minutes in Session 3.

---

## 7. Where to start

Phase 1 (Workstream S/A/B/D + A.5), Phase 2 C (Case 4 language
feature), Phase 3 (multi-crate E.1–E.6), **all seven taxonomy cases**
(1a/1b/2/3/4/5/6), and **eleven of eighteen course-correct items**
(#1, #2, #4, #5, #6, #11, #14, #15, #16, #17, #18) are all done.

Session 8 closed Case 4 via the Phase 2 C feature. The seven-case
taxonomy is now fully tested — there is no longer a "main remaining
piece" with concrete scope; what's left is the deep facade-rebuild
trio (#7/#8/#12 + #9 + #3, bundled) plus the wrapper-mode retirement
(#13).

### Tier 2 — DONE (Session 8)

Phase 2 C (toylang `impl rust_trait for toylang_type`) landed in
seven sub-steps across three commits (see §2 Session 8). Case 4 is
tested via `case4_sky_impl_rust_trait/`. Nothing remains in Tier 2.

The original wording follows for posterity (this is the architecturally
interesting pattern that's now exercised):

> Phase 2 C — toylang `impl rust_trait for toylang_type`. Unlocks
> Case 4 (Sky type implementing a Rust trait, consumed via a Rust
> generic intermediary — "Sky exposes a trait impl that satisfies a
> Rust generic's bound"). Real toylang language-feature work: parser,
> AST, type-resolver, stub_gen impl-block emission, llvm_gen for
> trait-impl method codegen, symbol_name override extension for impl
> DefIds.
>
> This is the most architecturally interesting remaining piece because
> it's the pattern Sky must support: a Sky-defined type implementing
> a Rust trait, where the Rust generic that bounds the trait gets
> instantiated by either Rust or Sky code. The Phase 2 C work touches
> real language design (how does toylang write `impl Clone for Widget`?
> toylang's existing syntax is `impl rust.std.clone.Clone for Widget {
> fn clone(&self) -> Widget { ... } }` per architecture §6.2's worked
> example).

### Tier 3 — larger architectural shifts (multi-week)

These rebuild facade-level assumptions that today's tests don't
exercise sharply. Each is its own multi-week sub-project.

- **#7 — replace `LangPredicates` with sidecar-loaded universe.**
  Today: facade calls `is_consumer_type(name)` / `is_consumer_fn(name)`
  via vtable. Sky wants content-addressed typeids in the sidecar. ~2
  weeks; touches many call sites.

- **#8 — `layout_of` walks Sky-side.** Today: facade calls
  `monomorphize_type` callback for field types and lets rustc compose.
  Sky wants `layout_of` to walk Sky's universe recursively itself.
  ~1–2 weeks.

- **#9 — retire `symbol_name` side-effect channel.** Today: rustc's
  `symbol_name` query firing on a consumer Instance triggers
  `notify_concrete_entry_point` which stashes the Instance for
  internal-callee discovery (`@SyMINCZ` trap-fence). Sky's discovery
  moves to the `after_expansion` queue (§20.4); `symbol_name`
  becomes a pure read. Bundled with #3 (the CGU stash retirement —
  see §2's Session 7 audit). ~1–2 weeks.

- **#12 — retire `MUTABLE_STATE` + two-vtable split.** Today: facade
  holds a Mutex around consumer state; the @GCMLZ bypass uses a
  thread-local fat pointer (Phase 3 E.6). Once #4's deeper inline-
  codegen rewrite and #9's symbol_name retirement land, the locking
  story fundamentally simplifies. ~1–2 weeks.

- **#3 — retire `cgu_stash.rs`.** Session 7 audit (§2) showed this
  is bundled with #7 + #9: the consumer's accessor-method discovery
  and Case-1b generic-from-Rust discovery both still rely on rustc's
  CGU walk; moving both to the after_expansion queue is the same
  architectural shift those items name. Order-of-operations: land
  the sidecar-loaded universe (#7) so the queue exists, retire the
  symbol_name discovery channel (#9), then delete the stash (#3).
  ~comes free with the above.

- **#13 — retire wrapper-mode `@MRRIWMZ`.** Largest. Today: toylangc
  IS the rustc-via-`RUSTC_WORKSPACE_WRAPPER` wrapper, re-parsing the
  toylang.toml in the child process. Sky wants the forked rustc binary
  statically linked with the codegen backend. This is the "ship a Sky
  toolchain" piece. Architecture §4.1–§4.5. ~4–6 weeks; touches
  install, distribution, and the entire startup model.

### My recommendation

**Tier 1, in #4 → Workstream B → #5 order.** Each is a discrete win.
Then Tier 2 (Phase 2 C) is the biggest forward-progress piece — it's
where the language gets a feature that exercises a fundamentally new
Sky interop pattern. Tier 3 is a quarter-of-work each; don't start
without budgeting properly.

**Update after Session 11**: Tier 1 + Tier 2 done; the uniformity sweep
(Phases A/B/C/F) closed every site that branched on
`type_params.is_empty()` in discovery + typecheck paths. **Phase E Path 1
also landed**: the rustc debuginfo clamp was written and verified as fork
patch 4 (`e67de69ef35` on `per-instance-mir`). Verified by reproducing
the previously-ICE'ing `pub struct Foo(())` shape on the patched rustc —
253/253 tests pass. PR draft at `phase-e-rustc-pr-draft.md` ready for
upstream submission.

Remaining options:

- **Phase E completion** (~1 hour): with the clamp now in place locally,
  remove the struct-shape `is_generic` branch from stub_gen and update
  the architecture_fence to allow the now-unified shape. This adds a
  hard local dependency on the fork patch — anyone building against
  vanilla nightly crashes. Trade-off: closes the asymmetry; widens the
  fork-vs-vanilla gap. Could be deferred until the upstream PR lands.
- **Phase E Path 2 — `SkyOpaqueType<typeid>` migration** (5-10 days):
  the §10.6 locked Sky design. Substantial — every Sky struct's stub
  shape becomes a wrapper, and every Sky→rustc type-identity site
  needs to decode the const-generic typeid. High blast radius.
  Recommendation: defer until §13's comptime-type machinery lands and
  this work amortizes.
- **Tier 3 facade-level shifts** (#7, #8, #12, #13): each is its own
  multi-week sub-project. Don't start without explicit agreement.

### Cross-references for the next person

- `workstream-a-scope-notes.md` — Workstream A completion notes,
  including the two key unlocks (oracle sweep, transitive callee walk).
- `phase3-e6-scope-notes.md` — Phase 3 E.6 completion notes, including
  the @GCMLZ re-entrance root cause and the thread-local bypass fix
  pattern.
- `rust-interop-architecture.md` §§4.5, 6.1, 6.3, 6.5 — marker-based
  detection (E.1) reference. §5.3 — codegen backend method sketches
  (#4 reference). §20.3 — pipeline ordering (#5 reference). §6.2 +
  Appendix A.3 — `impl rust_trait for sky_type` worked example
  (Phase 2 C reference).
- `course-correct.md` — top-of-file status table shows what's done.

---

## 9. Operational tips

### 9.1 Running tests

```bash
# Full suite (run with cache wiped):
rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
grep -aE "^test result|FAILED|^running" /Users/verdagon/erw/tmp/quarter-of-work.txt | tail -8

# Just sidecar unit tests:
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --bin toylangc sidecar:: > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1

# Just one integration test:
cargo test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --test integration_projects test_diamond_call_pattern > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
```

The session log file `./tmp/quarter-of-work.txt` is fixed — re-use it for
every command in your session per CLAUDE.md's "build & run convention."

### 9.2 Direct toylangc invocation (for probing)

```bash
cargo run --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --quiet -- \
    build /Users/verdagon/erw/toylangc/tests/integration_projects/diamond_call_pattern/toylang.toml \
    > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
```

This bypasses cargo test and runs the binary directly, capturing all
stderr. Useful when you need to see eprintln output that cargo test
swallows.

### 9.3 Rebuilding the rustc fork

If you need to make changes to `~/rust` (the forked rustc), see
`docs/historical/rebuilding-rustc-fork.md`. Five steps; the install
step needs `bash install.sh --prefix=$HOME/rust/build/host/stage2`.
Full rebuild from clean state with CI LLVM = ~15 min.

### 9.4 Sidecar inspection

You don't have a `skyc inspect` tool (deferred per architecture doc
§8.9). If you need to inspect a sidecar's contents during debugging,
write a temporary test in `sidecar.rs::tests` that calls
`deserialize_sidecar` on a known file path and prints the registry.
Don't write a freestanding tool.

---

## 10. Things that will probably bite you

These are real traps. Read them.

1. **Stale incremental cache.** Already covered in §6. Wipe
   `integration-projects-cache` when tests act weird.
2. **Wrapper mode vs build mode.** `toylangc` is BOTH the orchestrator
   AND the rustc wrapper. The same binary runs in both modes. See
   `main.rs::run_wrapper_mode` and `build::build_project`. If you're
   debugging "why isn't my callback firing", first determine WHICH MODE
   the failing invocation is in.
3. **Two rustc invocations per build.** rlib + user_bin. Independent
   processes. State doesn't carry across. The callback log file is shared
   but whoever writes last wins.
4. **`is_downstream_of_stubs` semantics.** TRUE means "this is the
   user_bin compile (not the rlib)". The variable name is awkward.
   Phase 1 A.2 renames it.
5. **Cargo's `.cargo` directory.** Doesn't exist in the workspace; `cargo`
   uses `$CARGO_HOME` (typically `~/.cargo/`) for the registry cache.
   When toylang fixtures depend on path-based test_helpers, watch that
   the `path = "../test_helpers"` works in the generated `.toylang-build/`
   workspace.
6. **`tcx.output_filenames(())` vs `tcx.output_filenames(LOCAL_CRATE.into())`.**
   The query key is `()`, not a CrateNum. Easy to get wrong.
7. **`@-arcana` invariants.** The `docs/architecture/rust-interop-guide.md`
   has a section on cross-cutting invariants (@SyMINCZ, @ELASZ, etc.).
   The @SyMINCZ one specifically — "computing a symbol name doesn't force
   codegen" — has caught me. If you add a new call to `tcx.symbol_name`
   thinking it'll register the Instance for codegen, that's wrong; only
   `ReifyFnPointer` casts in the per_instance_mir body do that.
8. **Trait-method dispatch keys on trait DefId, not impl DefId.** Per
   @TVIMDGAZ. When you build an Instance for `<MyType as Trait>::method`,
   you use the trait def's method DefId with `[Self=MyType, …]` args, then
   `Instance::expect_resolve` maps to the impl method at runtime.
9. **`instantiate_identity()` requires a comment.** Per the project's
   CLAUDE.md compiler law. `EarlyBinder::instantiate_identity()` is only
   valid for structural inspection. Every call site needs a comment
   explaining why we're not substituting.
10. **The bincode crate version.** This project uses bincode v2, not v1.
    The APIs differ. Use `bincode::serde::encode_to_vec` and
    `bincode::serde::decode_from_slice`, NOT `bincode::serialize` /
    `bincode::deserialize` (those are v1).

---

## 11. Useful git references

```bash
# Last commit before Approach B migration (clean Approach A state):
git show ce437ae

# The A→B cutover (read in reverse to undo):
git show bf770ae

# Previous handoff doc (the A→B forward direction):
docs/historical/handoff-optimized-mir-migration.md
```

The fork lives at `~/rust` on branch `per-instance-mir`. Three patches
currently applied (uncommitted in the working tree — the project
convention is "patches as working tree state"). See the diff with
`cd ~/rust && git diff --stat`.

---

## 12. Status snapshot (where you start)

**Tests passing**: **253/253** (98 unit + 1 fence + 138 integration + 16
standalone) when run with `integration-projects-cache` wiped.

**Seven-case taxonomy coverage**: 1a ✅, 1b ✅, 2 ✅, 3 ✅, 4 ✅,
5 ✅, 6 ✅. All seven cases tested.

**Course-correct.md items done**: #1, #2, #4, #5, #6, #11, #14, #15,
#16, #17, #18 (11/18). #10 partial. #3 audited and deferred (bundled
with #7/#9, not a mechanical cleanup).

**Generic/non-generic uniformity sweep**: Phases A/B/C/F landed (Session
11). The four `type_params.is_empty()` discovery+typecheck sites the
plan flagged as implementation pragmas are gone; the four remaining
sites (substituted-args fast paths + FnCall arity check) carry
`arch-fence-allow` markers and are fenced by
`tests/architecture_fence.rs`. The two externally-constrained sites
(struct shape + extern decls in `stub_gen.rs`) stay until rustc patch /
inline-codegen rewrite lands respectively. Phase E investigation in
`phase-e-investigation.md`.

**Sidecars produced**: yes, ~120 files materialize during a full test run.
The format is bincode + BLAKE3 truncated checksum with a 64-byte fixed
header. S.4's facade-side loader reads them at user-bin compile time;
S.5's determinism test byte-compares two builds.

**Byte-identical pass-through**: guarded by `test_a5_byte_identical_pass_through`
(standalone test). Compiles a 4-entry Rust corpus with both vanilla
nightly + rustc-fork, normalizes the disambiguator-derived bits, asserts
LLVM IR byte-equality. Skips gracefully if vanilla isn't installed.

**Fork state**: `~/rust` on `per-instance-mir` branch, **4 patches**
applied: query declaration, collector hook, default-None provider, and
debuginfo struct+union field clamp (Session 11's Phase E Path 1,
commit `e67de69ef35`). Rebuilt for nightly-2026-01-20 / rustc 1.95.0-dev
/ commit `d940e568`. Installed as toolchain `rustc-fork`.

**Toolchain pin**: `rust-toolchain.toml` channel = `"rustc-fork"`. Four
sites stay in sync (the toolchain file + `TOYLANG_NIGHTLY` in main.rs +
two test files).

**Codegen architecture**: post-Workstream A — rlib compile produces
rlib + sidecar only (no toylang `.o`). User-bin compile is the
codegen site, driven by registry-driven discovery + transitive callee
walk (NOT the upstream CGU walk, which finds zero stub items at user-bin
time — see `workstream-a-scope-notes.md` for the why).

**Working tree is clean** as of Session 8. Sessions 2–8's work is on
`main` across fourteen commits:

| Commit | What |
|---|---|
| `671f002` | Approach A restoration + Sidecar (S.1–S.5) + Workstream A + Phase 3 multi-crate (E.1–E.6) |
| `1a72a64` | Phase 1 D: rust_caller manifest field + Cases 1a/1b/3/5 fixtures + tests |
| `7278f4a` | Course-correct #14 (CARGO_PRIMARY_PACKAGE) + #2 (B2 linkage mutation) retirement |
| `88b56d2` | A.5: byte-identical pass-through invariant CI test (§4.4) |
| `dc52833` | Session-5 doc refresh (course-correct table, tl-handoff §7 tiered options) |
| `6c19e53` | Course-correct #4 (codegen-wrapper emission channel) |
| `01d98fd` | Workstream B (oracle TypeParam tolerance in trait queries) |
| `e81cf6d` | Course-correct #5 (after_expansion hook point) |
| `7c23f63` | Session-6 doc refresh (course-correct status + tl-handoff Session 6) |
| `1c27b09` | Course-correct #18 (build.rs rust_deps re-listing comment) |
| `4f5cc8a` | Course-correct #17 (cosmetic is_generic branches in stub_gen) |
| `7a203b0` | Session-7 doc refresh (Tier 1 closure + #3 audit) |
| `6e9e7a8` | Phase 2 C.1 + C.2 (parser + ToyImpl registry) |
| `5b1babd` | Phase 2 C.3 + C.4 (typecheck + stub-rlib emission) |
| `b56cf4c` | Phase 2 C.5 + C.6 + C.7 (Case 4 end-to-end; 7/7 cases tested) |
| `22a1390` | Session-8 doc refresh |
| `d65ef81` | Session 9 sharpening (case4/5/6 now architecturally correct) |
| `a7683fc` | Session-9 doc refresh |
| `1b738e6` | Session 10: `export` keyword + non-export items invisible to rustc |
| `0d728f9` | Session-10 doc refresh |
| `0b40d98` | Honest accounting: generic-vs-non-generic uniformity audit + fix plan |
| `8faca57` | Phase A: rename `type_params.is_empty()` to `has_abstract_args()` |
| `5a1e7d0` | Phase B: generic bodies type-resolve eagerly, defer on TypeParam |
| `d87638d` | Phase C + F: entry-point walk + architectural-property fence |
| `a43569c` | Phase E investigation: rustc debuginfo ICE reproduces; recommend upstream patch |
| `4c19bec` | Doc refresh: Session 11 uniformity sweep + Phase E investigation |
| `8a9adc8` | Phase E patch landed in fork; verified clamp eliminates the ICE |

Plus fork commit `e67de69ef35` (in `~/rust` on `per-instance-mir`):
debuginfo: clamp struct + union field walk to layout's field count.

Use `git log 411c2f5..HEAD` to walk forward from the pre-Session-2
baseline.

**The plan file**: `/Users/verdagon/.claude/plans/now-please-plan-out-dynamic-island.md`.
Already approved.

---

## 13. When to escalate

Ping the user (don't pivot unilaterally) if:

- The rustc fork needs more patches beyond the existing 3.
- You hit a test failure you can't explain after wiping the cache and
  trying twice.
- Phase 2 C's toylang `impl` parser turns out > 8 weeks (the plan
  budgets 3–5).
- You're tempted to revert Workstream A, the @GCMLZ thread-local
  bypass, or any of the Phase 3 multi-crate plumbing. These are
  delicate; any revert is a major architectural regression. Talk to
  the user first.
- A Tier 3 item (#7, #8, #12, #13) is being started without an
  explicit "yes, we're committing to a multi-week piece" agreement.

For routine "this took longer than I estimated" — just keep going.

**Lessons from prior sessions worth re-reading:**

- **Session 4 — the half-done refactor pattern.** Workstream A's
  original ~2–3 week sizing didn't account for the oracle cross-crate
  sweep being half-done from stage 5a. Once finished, A landed in ~2
  hours. If a future workstream feels MUCH harder than estimated,
  look for half-done stage refactors blocking the obvious path.

- **Session 5 — diagnose before reverting.** A 0%-CPU hang at the
  user-bin compile was initially attributed to a panic + @GCMLZ unwind
  interaction. That was wrong. `sample <pid>` on the hung process
  showed the real cause: std::sync::Mutex same-thread re-entrance at
  MUTABLE_STATE during `lang_symbol_name → call_notify_concrete_entry_point`
  from inside `generate_and_compile`. Fixed in ~30 minutes once the
  stack trace was in hand. Lesson: when a process hangs at 0% CPU
  with no error, run `sample` BEFORE reverting and writing scope
  notes that speculate about the cause.

---

## 14. Closing notes

You're inheriting a working baseline at a **major checkpoint**, with the
architecturally interesting interop machinery proven end-to-end. The
mechanism is alive: Approach A fires per-Instance with concrete args
(Case 1b exercises this directly), the rustc fork is built and pinned
at `~/rust` (3 patches), the sidecar format is specified and types
ship + roundtrip + deserialize at upstream-rlib-load, the oracle
helpers work cross-crate, Workstream A's codegen-at-binary architecture
runs every Sky body at the user-bin compile from the AST in the
sidecar, the multi-crate plumbing works (case6 builds), the seven-case
taxonomy has fixtures for six of seven cases (1a/1b/2/3/5/6), and the
§4.4 byte-identical pass-through invariant is guarded by CI (A.5),
the §9 export commitment is guarded by `non_export_body_bearing_fn_gets_no_stub_shell`,
and the CLAUDE.md compiler-law's generic/non-generic uniformity is
guarded by `architecture_fence.rs`.
**253/253 tests pass.**

Architecturally the prototype is now **LITERAL** Sky shape for
multi-toylang-crate projects, no longer just rehearsal. Single-file
toylang programs still use the 2-cargo-crate split (lang_stubs_crate +
user_bin), which is rehearsal-shape only because the "library" half is
a derived artifact of the binary's own source. Multi-crate fixtures
exercise the real Sky shape: independent toylang libraries published
with their own sidecars, consumed by independent toylang binaries
that codegen the libs' bodies at the binary compile from the sidecar's
AST.

Phase 2 C is done (Session 8) and the generic/non-generic uniformity
sweep is done (Session 11). The biggest remaining architectural pieces
are the **Tier 3 facade-level shifts** (#7, #8, #12, #13) and **Phase E
Path 1's upstream rustc patch** (~1 day in our control to write +
weeks of PR review; the cheapest remaining unification win). See §7.

Read the architecture doc. Read course-correct.md (status table at the
top). Read `workstream-a-scope-notes.md` and `phase3-e6-scope-notes.md`
for the load-bearing context on the current codegen path and the
@GCMLZ bypass. Then start with §7 of this document.

Good luck. The architectural goal — making tests for the seven-case
taxonomy's hard cases EXIST so future drift back toward Approach B
fails loudly — is mostly met. Five hard cases (1b, 3, 4, 5, 6) were
the original target; four of those are now tested (1b, 3, 5, 6).
Adding Case 4 closes the taxonomy.

— previous engineer
