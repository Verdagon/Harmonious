# Course Correct: erw → Sky

This document catalogs the places in the current erw prototype that are on the *wrong track* relative to the Sky architecture (`rust-interop-architecture.md`). Each entry describes a pattern that must *change*, not something that must be added.

## Status snapshot (current)

| Status | Items |
|---|---|
| ✅ Done | #1 (Approach A), #2 (B2 linkage mutation), **#3 (`cgu_stash` retired)**, #4 (codegen channel), #5 (after_expansion hook), #6 (`__SKY_STUBS_MARKER`), #7 (LangPredicates → SkyUniverse), **#8 (SkyUniverse owns struct metadata)**, #9 (symbol_name side-effect retired), #11 (no per-lib `.o`), **#12 (@GCMLZ deadlock concern dissolved)**, #14 (CARGO_PRIMARY_PACKAGE), #15 (binary codegen site), #16 (per-Sky-library stub rlibs), #17 (cosmetic is_generic branches unified in stub_gen), #18 (build.rs comment refreshed) |
| 🟡 Partial | #10 (Instance-keyed collect_generic_rust_deps — landed; the rest needs E.5-style threading) |
| ⏳ Remaining | #13 (wrapper-mode retirement, bundles with Sky's actual toolchain shipping) |

**16 of 18 items done.** Phase 4.5 Path B (Session 15) landed the single-symbol architecture + #![no_builtins] LTO exclusion; Session 16 swept Tier 3 #8 (SkyUniverse absorbs consumer struct metadata via type-erased `Arc<dyn Any>`), #3 (`cgu_stash.rs` deleted; accessors collapsed into the regular function pipeline via parse-time `synthesize_accessor_pairs`; Case-1b discovery uses `default_collect_and_partition()` directly), and #12 (close-out: @GCMLZ doc + lib.rs comments refreshed; `FacadeMutableState` inlined; mutex stays for cross-worker-thread serialisation of `collect_generic_rust_deps` but no longer trap-fences the deadlock class). Only #13 — retiring wrapper-mode `@MRRIWMZ` for forked-rustc-as-CodegenBackend — remains.

**Session 11 (separate from course-correct numbering)**: the generic-vs-non-generic uniformity audit landed Phases A → B → C → F per a focused plan (`tmp/claude-plan-2026-06-15-ccc8939f.md`). `ToyFunction::has_abstract_args()` helper rename (A), generalized `DeferredTypeParam.query` + TypeParam guards on 4 ungated oracle entry points + Check 5/6 skip drop (B), §20.4 entry-point walk replacing the registry walk in `populate_toylang_instances_from_cgus` (C), grep-based CI fence `tests/architecture_fence.rs` (F). Phase E (struct-shape ICE) was investigated — see `phase-e-investigation.md`; the documented rustc debuginfo ICE still reproduces on rustc 1.95.0-dev, recommendation is to file the upstream patch first. 253/253 tests passing.

**Session 12 (separate from course-correct numbering)**: Phase E Path 2 — the `__ToylangOpaque<const T: u64>` wrapper-as-field migration (architecture §10.4.5 path 2 / §10.6) — landed in three commits (`72a929e`/`41423cf`/`90599cf`). Sky struct stubs now emit as newtypes around a content-addressed-typeid wrapper, with `layout_of` reporting source-matching field counts. Fork patch 4 (`e67de69ef35`) reverted; the debuginfo walker's source-vs-layout-field-count assumption holds structurally without a defensive clamp. 262/262 tests passing under unpatched rustc (only the 3 per_instance_mir patches remain in the fork).

See `tl-handoff.md` for the narrative summary and recommended next directions.

---

## Core architectural inversions

### 1. `rustc-lang-facade/src/queries/optimized_mir.rs` — wrong query, wrong substitution direction
This whole file embodies erw's **Approach B** (DefId-keyed `optimized_mir` override, rustc-side substitution). Sky requires **Approach A** (`per_instance_mir`, Instance-keyed, Sky-side substitution) — §3.1, §19.1.

- Lines 28–82 (`lang_optimized_mir`): keys on `LocalDefId`, builds with `identity_args` (line 77), returns Param-bearing bodies and relies on "rustc's collector substitutes Params per caller" (line 9–10, 68–69). Sky wants the opposite — Instance-keyed input, **pre-substituted** body (Sky-side comptime evaluation applied to `instance.args`).
- The file's own docstring (lines 4–10) explicitly says it "replaces the forked `per_instance_mir`" — Sky reverses that.
- `build_dependency_body`'s `instantiate(tcx, instance.args)` (line 110) plus its tolerance of residual Params (lines 99–101) all need to flip: Sky must reach a fully-monomorphized body before returning.

### 2. `queries/partition.rs` — the B2-risk linkage mutation
Sky §5.1–5.2 explicitly delete this mechanism.

- Lines 80–88 mutate `MonoItemData.linkage = (External, Default)` for `__lang_stubs` items post-partition. That **is** the B2-timing assumption Sky's full-CodegenBackend design eliminates. Sky's wrappers and Sky-emitted code are co-located in the binary at link time, so default `Hidden` is sufficient — the mutation should go.
- The whole rationale comment (lines 22–28) is the description of risk B2; under Sky it disappears.

### 3. `cgu_stash.rs` — the B5 lifetime-erased stash
This whole file (87 lines) shouldn't exist in Sky. It exists because toylangc walks rustc's unfiltered CGU slice to *discover* concrete consumer Instances (accessor methods, entry points). Under Sky's pipeline (§20.4, §20.7), Sky's frontend populates the codegen queue from sidecars + local Temputs at `after_expansion`, then Sky's `codegen_crate` walks that queue. Rustc's partition output is irrelevant for Sky-item discovery.

**Status: DONE (commit `c0a83fe`).** Accessor discovery moved to `after_rust_analysis` via `synthesize_accessor_pairs` (per-`(struct, field)` `ToyFunction` synthesis); Case-1b generic instantiations from Rust callers now go through `default_collect_and_partition()(tcx, ())` called directly inside `codegen_crate` with live `'tcx`. `cgu_stash.rs` deleted; `stash_upstream_cgus` / `clear_upstream_cgus` / `upstream_cgus` retired. Architecture risks.md §B5 closed.

### 4. `codegen_wrapper.rs` — wrong emission point and channel
- Lines 96–108: `inner.codegen_crate(tcx)` runs *first*, then `generate_and_compile` produces a `.o` and `join_codegen` injects it. Sky inverts: Sky's `codegen_crate` (§5.3, §20.7) calls inner first too, but then walks **Sky's own queue inline** and emits LLVM IR via Inkwell directly in `codegen_crate`. No callback returning a path; no `set_lang_compiled_object` cross-phase channel.
- The `set_lang_obj_path` / `get_lang_obj_path` `OnceLock`/Mutex round-trip (lib.rs 500–509) is the symptom of this misshapen pipeline and goes away.
- The `-C codegen-units=16` flag justification (lines 21–23 comment) is tied to the linkage-mutation story and is no longer needed once §5.2 holds.

### 5. `driver.rs` — wrong hook point + stash dance
- Line 76 (`after_analysis`): Sky's frontend fires at `Callbacks::after_expansion` (§20.3) — *before* rustc typechecks the stub bodies, so Sky's universe is populated before mono collection. `after_analysis` is too late.
- Line 68 (`clear_upstream_cgus`): the whole stash-clearing dance disappears with §3 above.

---

## Single-rlib model → per-library stub rlibs + marker-based detection

### 6. `lib.rs::is_from_lang_stubs` (lines 325–327)
Hardcoded `tcx.crate_name(def_id.krate).as_str() == "__lang_stubs"`. Sky uses **marker-based** detection — walk `tcx.module_children(crate_root)` for `__SKY_STUBS_MARKER` (§4.5, §6.3, §6.5). The single special name `__lang_stubs` dies; per-Sky-library rlibs are named after the library itself (`my_utils.rlib`, §6.1, §6.5).

### 7. `lib.rs::is_consumer_type` / `is_consumer_fn` + `LangPredicates` (lines 86–104, 308–311, 386–389)
Name-based "is this a consumer type/fn?" predicates supplied by the consumer. Sky identifies items by content-addressed typeid / sidecar-loaded universe (§8.5, §10.8, §10.9). The name-registry pattern (and the entire `LangPredicates` trait) is the wrong shape — Sky's frontend resolves identity via the sidecar's typeid table, not via "does this string match a registered name?".

### 8. `lib.rs::LangCallbacks::monomorphize_type` (lines 131–136)
Asks consumer "what are the concrete field types of `Foo<i32>`?" so the facade can call `tcx.layout_of` on each. This is Approach-B-shaped: it leans on rustc's layout machinery for composition. Sky's `layout_of` override (§10.5, §8.8) computes the layout from the sidecar's structural Temputs Sky-side, recursively. The callback dissolves into "Sky's layout machinery walks Sky's universe."

### 9. `lib.rs::LangCallbacks::notify_concrete_entry_point` + the `symbol_name` side-effect channel (lines 170–176, plus `queries/symbol_name.rs` lines 80–82)
Symbol-naming used as a back-channel: when rustc queries `symbol_name`, the callback fires and stashes the Instance for internal-callee discovery. Per §9.6 there is **no cross-crate Sky-internal symbol resolution problem in Sky** — all Sky bodies are codegenned at the binary compile from sidecars, with one consistent Sky-internal mangling owned by the binary. The notify mechanism, the dedup set it drives, and the whole "symbol_name fires → state mutation" pattern (the @SMINCZ trap fence, lines 19–25 of symbol_name.rs) are no longer needed in that shape.

### 10. `lib.rs::LangCallbacks::collect_generic_rust_deps` (lines 154–160)
Returns `Vec<(DefId, GenericArgsRef)>` for a DefId with Param-bearing args. Under Sky's Instance-keyed model it should return a substituted MIR body (or the structural recipe Sky's `per_instance_mir` uses to construct one) for a fully concrete `Instance<'tcx>`.

### 11. `lib.rs::generate_and_compile` returning per-rlib `.o` (line 182, codegen_wrapper.rs 106–108)
The whole "each Sky-marked crate produces an `.o`" model. Sky §5.5 / §9.6 locks the opposite: **library compiles produce rlib + sidecar only; the binary compile codegens every reachable Sky item across all libraries.** The callback's return type and the codegen_wrapper's path-injection model both invert.

### 12. `lib.rs::MUTABLE_STATE` mutex + two-vtable split (lines 197–264, 270–304, 415–465)
The whole `@GCMLZ` mutex architecture, the `PredicateVtable`/`StatefulVtable` split, the `_inner` bypass mentioned in `symbol_name.rs:14–18` — these exist to dodge re-entrant deadlocks during `generate_and_compile`. With Sky's pipeline (frontend populates queue at after_expansion; codegen walks queue inline; no callback returning a `.o` path; no symbol_name side-effect channel), the locking story is fundamentally different and most of this scaffolding is solving a problem that no longer exists.

**Status: DONE (as side effects + close-out commit).** The `PredicateVtable` retired in Tier 3 #7.4 (predicates now read from `SkyUniverse`). The `notify_concrete_entry_point` side-effect callback + Session-5 thread-local fat-pointer bypass retired in Tier 3 #9. The `generate_and_compile` callback retired in Phase 4.5 Path B (Session 15), replaced by `consumer_emit_modules` via the `extra_modules_hook` fork patch. Net result: no query provider can re-enter a `MUTABLE_STATE` lock today. The mutex itself remains — load-bearing because `collect_generic_rust_deps` fires from `lang_per_instance_mir` during rustc's mono walk on rayon worker threads — but its sole role is now plain inter-callback serialisation, not @GCMLZ trap-fencing. Close-out commit refreshes the @GCMLZ doc + lib.rs comments to reflect the dissolved deadlock concern, and inlines the single-field `FacadeMutableState` struct.

---

## Wrapper mode → in-process forked rustc

### 13. `toylangc/src/main.rs` — the whole wrapper mode (lines 39–55, 82–155)
This is `@MRRIWMZ`. The architecture doc explicitly calls it out as **one of the two erw arcana with no Sky analog** (the §"Inherited reasoning from erw" table). Sky's `rustc` is the forked rustc statically linked with the backend (§4.1) and is invoked by cargo via `rust-toolchain.toml` — no `RUSTC_WORKSPACE_WRAPPER`, no argv shuffling, no manifest re-read. The whole `run_wrapper_mode` block and `find_sysroot_tool` / `compile_llvm_ir` plumbing dies in its current shape.

### 14. `CARGO_PRIMARY_PACKAGE` activation (main.rs line 94)
§4.5 names this **by name as the wrong mechanism**: "the activation mechanism was `CARGO_PRIMARY_PACKAGE=1`… The problem: a published Sky library, depended on by a user's Sky project, gets built by cargo as a normal dep — `CARGO_PRIMARY_PACKAGE` is unset." Replace with `__SKY_STUBS_MARKER` detection.

### 15. `is_downstream_of_stubs` inversion (main.rs 145–195, callbacks_impl.rs 75/191/214/387)
The current logic: rlib compile generates the `.o` and walks internal callees; user-bin compile short-circuits `generate_and_compile`. Sky's model (§5.5, §9.6) **inverts**: the user-bin compile is where every Sky body materializes; library compiles produce no Sky `.o`. Also, the detection itself (`pkg_name.starts_with("lang_stubs_")`, line 150) is coupled to the single-name-pattern model and goes with §6.

### 16. `toylangc/src/build.rs` — single shared stub crate
- Lines 41–73 (`stubs_dir = build_dir.join("lang_stubs_crate")`): one shared stub rlib for the whole project. Sky §6.1 locks **per-Sky-library** stub rlibs — each Sky lib (and the binary) gets its own. The orchestration needs to fan out across workspace members.
- Lines 87–124 (`write_user_bin_cargo_toml`'s re-listing rationale): "toylang's emitted `.o` (bundled into the rlib) calls into rust_dependencies symbols at the OBJECT-FILE level" — under Sky the `.o` lives at the binary, where the dependencies are already declared. The re-listing dance evaporates.

### 17. `toylangc/src/stub_gen.rs` — `is_generic` special-casing
CLAUDE.md states the compiler law: **"non-generic is the degenerate case of generic. Never branch on 'does this function/type have type parameters?'"** Every `if !is_generic { ... } else { ... }` branch in this file is on the wrong track:

- Line 59 → 86–97: struct shape (`pub struct Foo;` vs `pub struct Foo<T>(PhantomData<...>)`). Sky should pick one universal shape (§10.1 uses `(())` for plain, `PhantomData<T>` for parametric — but applied uniformly, with zero-param as the degenerate case).
- Lines 117–123, 185: extern declarations emitted only for non-generics. This is the Option B "extern can't be generic" workaround (§5.1) that Sky's design avoids structurally — Sky's codegen emits everything in the binary, no per-symbol extern-decl needed.
- Lines 127–146: impl-block divergence by genericity.

### 18. `toylangc/src/toylang/callbacks_impl.rs` — internal-callee walk via symbol_name
Lines 156 / 195 (`notify_concrete_entry_point_inner` "stashing"), the `toylang_instances` accumulator, the dedup-across-symbol_name-firings pattern. Per §20.4 Sky's codegen queue is populated **at frontend time** (after_expansion) from sidecars + local Temputs, not as a side-effect of rustc querying symbol names. The whole notify-driven discovery dies; the comment chain about it being a "primary discovery channel" inverts.

---

## Summary of the architectural pivots, in order of blast radius

| # | Status | File / spot | Wrong-track pattern | Sky direction |
|---|---|---|---|---|
| 1 | ✅ | `queries/optimized_mir.rs` (whole) | DefId-keyed, Param-bearing body, rustc-side substitution | Instance-keyed `per_instance_mir`, pre-substituted body, Sky-side substitution |
| 2 | ✅ | `queries/partition.rs:80–88` | Post-partition `(External, Default)` linkage mutation (B2 risk) | Delete; default `Hidden` works |
| 3 | ⏳ | `cgu_stash.rs` (whole file) | Walk rustc's unfiltered CGU slice for Sky-item discovery | Sky's frontend populates queue at after_expansion; rustc partitions are Rust-only. **Session 7 audit:** `llvm_gen.rs:1938` still uses the stash for accessor-method discovery (`opt_associated_item`) and Case-1b generic instantiations from Rust call sites — paths Workstream A's registry-driven walk doesn't cover. Retirement is bundled with #7 + #9. |
| 4 | ✅ | `codegen_wrapper.rs:96–108` + `lib.rs:500–509` | Callback returns `.o` path; `join_codegen` injects via `OnceLock` channel | Sky's `codegen_crate` walks queue inline and emits via Inkwell directly — landed as a `LangOngoingCodegen { inner, lang_obj_path }` wrapper around rustc's own ongoing-codegen `Box<dyn Any>`; the OnceLock channel + `FacadeMutableState.lang_obj_path` are retired. The inline-Inkwell rewrite is deferred (still calls consumer's `generate_and_compile`), but the cross-phase channel is gone. |
| 5 | ✅ | `driver.rs:76` | Hook at `after_analysis` | Hook at `after_expansion` (§20.3) — landed as a hook-point swap; toylang's oracle queries (fn_sig, adt_def, module_children) are all available at expansion-time. |
| 6 | ✅ | `lib.rs:325–327` (`is_from_lang_stubs`) | Crate-name match against `"__lang_stubs"` | Marker-based: walk `module_children` for `__SKY_STUBS_MARKER` |
| 7 | ✅ | `lib.rs:86–104, 308–311, 386–389` | Name-list-based `is_consumer_type/fn` | **Done in commit `c801638`** — `SkyUniverse { typeids, fn_names, type_names }` populated at sidecar load + local registry build; predicates are O(1) RwLock reads. `LangPredicates` trait, `PredicateVtable`, trampolines, and toylang's per-callbacks mirrors all retired. +2 facade unit tests. |
| 8 | ⏳ | `lib.rs:131–136` (`monomorphize_type`) | "Give me field types so rustc composes layout" | Sky's `layout_of` walks Sky's universe recursively itself |
| 9 | ✅ | `lib.rs:170–176` + `queries/symbol_name.rs:80–82` + `callbacks_impl.rs:156,195` | `symbol_name` query as side-effect channel for internal-callee discovery | **Done in commit `fa3fdd3`** — `notify_concrete_entry_point` callback retired, replaced by stateless `consumer_symbol_for_callback_name`. `symbol_name` override is now a pure read; the @GCMLZ thread-local fat-pointer bypass (Session 5) is retired with it. Mangling logic split into `compute_consumer_symbol` (stateless) + the codegen-side `_inner` wrapper that adds the diagnostic log push. |
| 10 | 🟡 | `lib.rs:154–160` | `collect_generic_rust_deps(LocalDefId) → Vec<(DefId, args-with-Params)>` | Instance-keyed body production (pre-substituted) — landed for the primary path; cross-crate effective-registry merging followed in E.5 |
| 11 | ✅ | `lib.rs:182` + emission point | One `.o` per Sky-marked rlib compile | Zero Sky `.o` from libs; entire reachable Sky universe codegenned at binary compile |
| 12 | ⏳ | `lib.rs:197–304, 415–465` | `MUTABLE_STATE` + two-vtable + `_inner` bypass | Mostly obsolete once #4 and #9 land |
| 13 | ⏳ | `main.rs:39–55, 82–155` | RUSTC_WORKSPACE_WRAPPER wrapper mode + manifest re-read (`@MRRIWMZ`) | Forked rustc statically linked with backend; cargo invokes directly via `rust-toolchain.toml` |
| 14 | ✅ | `main.rs:94` | `CARGO_PRIMARY_PACKAGE` gates activation | `__SKY_STUBS_MARKER` gates activation — replaced with manifest-vicinity check (the pre-expansion analog of the marker check that fires after expansion) |
| 15 | ✅ | `main.rs:145–195`, `callbacks_impl.rs:75,191,214,387` | Rlib compile makes the `.o`; bin short-circuits | Bin compile makes the `.o`; lib compiles short-circuit Sky `.o` emission |
| 16 | ✅ | `build.rs:41–73` | Single shared `lang_stubs_crate` per project | Per-Sky-library stub rlib; workspace member per Sky crate |
| 17 | ✅ | `stub_gen.rs:59, 86–97, 117–123, 127, 148, 185` | `if !is_generic { … }` branches | Single universal path; zero type params is the degenerate case (CLAUDE.md "compiler law") — landed in full via four steps: (1) cosmetic impl-block + wrapper-fn header unification via `generics_for_impl_block` + `fn_generics_clause`; (2) struct-shape unification to `pub struct Foo<P...>(PhantomData<(P...)>);` via fork patch 4 (`e67de69ef35`, debuginfo clamp); (3) removal of the vestigial `__toylang_impl_*` extern decls (Sky's `symbol_name` query override routes Rust callers to the Sky-emitted symbols without needing a forward decl — the extern block was leftover from before symbol_name routing existed); (4) removal of the vestigial `__toylang_accessor_*` extern decls (Sky's codegen emits the symbols directly; no Rust source references them by name). The only `extern "C" { ... }` content left is the body-less toylang fn decls (toylang source's "talk to existing Rust fn" syntax, e.g., `fn println_int(x: i32);` → `extern "C" { pub fn println_int(...); }`), which can't take toylang-source generics by nature. The architecture_fence test scans both `type_params.is_empty()` and `type_args.is_empty()`. See `phase-e-investigation.md`. |
| 18 | ✅ | `build.rs:87–124` rust_deps re-listing | Justified by "lib `.o` references rust_deps at object level" | Comment refreshed (Session 7) — the re-listing remains load-bearing under Workstream A's binary-codegen model because Phase 1 D's `rust_caller.rs` lives inside user_bin and names rust_dependencies directly. Without the direct cargo dep, user_bin's compile fails at "unresolved import" before linking. |

---

## What's NOT on the wrong track

Mostly direction-correct, just needs different content:

- `queries/layout.rs` — opaque-with-size shape is right; the `is_from_lang_stubs` predicate name needs to change to marker-check.
- `queries/drop_glue.rs` — the InstanceKind::DropGlue → synthetic body shape is right; the consumer-type identification predicate changes (and Sky's linear types add a panic-body branch §15.7, but that's an addition).
- `queries/upstream_monomorphizations_for.rs` — the "force local mono" decision is still correct; only the `is_from_lang_stubs` predicate identity changes.
- `abi_helpers.rs`, `mir_helpers.rs` — explicitly inherited wholesale per §26.5–26.6.
