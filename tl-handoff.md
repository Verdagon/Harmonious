# Handoff: erw → Sky / clean checkpoint

Hi, future-you. The toylang prototype has reached **a major clean
checkpoint**. **266/266 tests passing** against
unpatched-aside-from-`per_instance_mir`-trio + fork-patch-4-for-
`extra_modules`-hook rustc. **16 of 18 course-correct items done.**
The seven-case interop taxonomy is fully tested. Cross-language
ThinLTO inlining is **empirically CI-fenced** by disassembly assertion
(`test_lto_smoke` verifies `lto_smoke::main` constant-folds Sky's
body down to `mov w8, #50` with no remaining `bl` to Sky symbols).

There is no pressing toylang work left. The remaining course-correct
item (#13, wrapper-mode retirement) bundles with Sky's actual
toolchain shipping. The architecturally interesting next moves all
live outside toylang — Sky comptime spike, async two-type split,
groups/regions — and are separate projects.

**If you only need a quick read: §1 → §8 → §10.** §6 (Tier 3 plan)
and the active inline-codegen plan
(`/Users/verdagon/.claude/plans/parsed-singing-globe.md`) are now
**historical** — preserved for reference; execution is done.

---

## 1. Current state (one paragraph)

`erw` is a two-crate workspace: `rustc-lang-facade` (a reusable library
that hooks into rustc via query overrides) and `toylangc` (a toy consumer
language exercising the facade). Architecturally we're aiming at **Sky**,
the design locked in `rust-interop-architecture.md` (5,148 lines). The
divergence catalog is `course-correct.md` (18 items, **16 done**: #1,
#2, **#3 (cgu_stash retired + accessors collapsed)**, #4, #5, #6, #7,
**#8 (SkyUniverse owns struct metadata)**, #9, #11, **#12 (@GCMLZ
deadlock concern dissolved)**, #14, #15, #16, #17, #18; #10 partial;
**only #13 remaining** — wrapper-mode retirement, bundles with Sky's
toolchain shipping). The seven-case interop taxonomy (1a/1b/2/3/4/5/6)
has been fully tested since Session 8; the architecturally-hard cases
(rustc-walking-a-Rust-generic-body-dispatching-to-a-Sky-impl) all have
fixtures. Toylang now uses **Approach A** (Instance-keyed
`per_instance_mir`), **per-library stub rlibs** with `__SKY_STUBS_MARKER`,
**sidecar-driven** (`.sky-meta` carries Temputs), **codegen-at-binary**
(no per-rlib `.o`), **`export`-gated** stub generation (Sky §9),
**wrapper-as-field** Sky struct stubs (`__ToylangOpaque<HASH>` per §10.6),
**single-symbol architecture** (Path B — Sky's bitcode emits under the
rustc-mangled name), **`#![no_builtins]` stub rlibs** (excluded from
ThinLTO's IR linker pool), and **inline codegen via patch (c)
extra_modules hook** (Sky's bitcode contributed to rustc's pipeline
through `consumer_emit_modules`). **Cross-language ThinLTO inlining
empirically verified** by `test_lto_smoke`'s disassembly assertion.
Four rustc fork patches in effect — `per_instance_mir` trio + the
`extra_modules` hook.

---

## 2. Session history (compressed)

| Session | Commits | What landed |
|---|---|---|
| 1 | (none, doc) | `course-correct.md` — 18 wrong-track items |
| 2 | (pre-`411c2f5`-era) | Approach A restored: 3-patch rustc fork rebuilt, facade switched from `optimized_mir` to `per_instance_mir`, toylang substitutes Sky-side from `instance.args`. |
| 3 | (Session 4 batch) | Sidecar S.1–S.3: format spec doc + `SidecarHeader` types, bincode serialization + BLAKE3 checksum, sidecar written at rlib-compile `after_rust_analysis`. |
| 4 | `671f002` | S.4 (facade reads upstream `.sky-meta` via new `on_sky_lib_loaded` callback); S.5 (sidecar determinism test); **oracle cross-crate sweep** (finishes stage-5a — `find_extern_fn_def_id` falls back to walking upstream `__lang_stubs`); **Workstream A** (codegen moves from rlib compile to user-bin compile — registry-driven discovery + `walk_and_stash_internal_callees`). |
| 5 | `671f002`, `1a72a64`, `7278f4a`, `88b56d2`, `dc52833` | Phase 3 E.1–E.6 (multi-toylang-crate end-to-end including `case6_basic`; @GCMLZ deadlock fixed via thread-local fat-pointer bypass diagnosed by `sample <pid>`); Phase 1 D rust_caller + Cases 1a/1b/3/5; #14 (CARGO_PRIMARY_PACKAGE retired); #2 (B2 linkage mutation retired); A.5 byte-identical pass-through CI. |
| 6 | `6c19e53`, `01d98fd`, `e81cf6d`, `7c23f63` | Tier 1 sweep: #4 (codegen-wrapper channel via `LangOngoingCodegen`), Workstream B (oracle TypeParam tolerance via `RustTypeDeferred`), #5 (`after_expansion` hook — turned out trivial). |
| 7 | `1c27b09`, `4f5cc8a`, `7a203b0` | #18 (build.rs comment refresh); #17 (cosmetic `is_generic` branches unified via `generics_for_impl_block` / `fn_generics_clause`); #3 audited — deferred (bundled with #7/#9). |
| 8 | `6e9e7a8`, `5b1babd`, `b56cf4c`, `22a1390` | Phase 2 C (Case 4): toylang `impl rust_trait for toylang_type` parser/AST/registry, type-resolver, stub_gen impl emission, facade discriminator (`is_consumer_trait_impl_method`), symbol_name routing, llvm_gen impl method bodies, auto-deref bug fix. Seven-case taxonomy now 7/7. |
| 9 | `d65ef81`, `a7683fc` | Honest audit of case fixtures → sharpened case4/5/6 with new `some_rust_lib/` (true Rust generic intermediary, `pub fn duplicate<T: Clone>(&T) -> T`). |
| 10 | `1b738e6`, `0d728f9` | `export` keyword: non-export body-bearing fns get NO `pub fn` shell in stub rlib (Sky §9). CI fence `non_export_body_bearing_fn_gets_no_stub_shell`. |
| 11 | `8faca57`, `5a1e7d0`, `d87638d`, `a43569c`, `4c19bec`, `8a9adc8`, `70e3069`, `c17cf7e`, `747d0e6`, `ed4e07e`, `a3a7c94`, `09d50bb` + fork `e67de69ef35` | Generic/non-generic uniformity sweep (Phases A/B/C/F); Phase E investigation; fork patch 4 (debuginfo clamp) shipped; struct shape unified to `pub struct Foo<P...>(PhantomData<(P...)>);`; vestigial `__toylang_impl_*` + `__toylang_accessor_*` extern decls retired. **CLAUDE.md compiler-law violation count: zero.** |
| 12 | `72a929e`, `41423cf`, `90599cf`, `7f6bf97` + fork `003f91e4df9` | **Phase E Path 2**: `__ToylangOpaque<const T: u64>` wrapper-as-field migration (architecture §10.4.5 path 2 / §10.6). typeid helper + wrapper emission + typeid table (Phase 1), const-generic-u64 encode/decode (Phase 2), Sky struct stub shape migration + layout-field-count match (Phase 3), fork patch 4 reverted (Phase 5). **262/262 against unpatched rustc.** |
| 13 | `c801638`, `fa3fdd3`, `45e903b`, `c4fc74a` | **Tier 3 #7 + #9**: `LangPredicates` → `SkyUniverse`, then symbol_name side-effect channel retired. `SkyUniverse { typeids, fn_names, type_names }` populated at sidecar load + local registry build; predicates are O(1) RwLock reads. `LangPredicates` trait + `PredicateVtable` + trampolines + toylang's per-callbacks name mirrors all gone. Then: `notify_concrete_entry_point` callback replaced by stateless `consumer_symbol_for_callback_name`; the @GCMLZ thread-local fat-pointer bypass (Session 5) retired with it. **264/264 passing.** Both landed in ~1.5h vs the handoff's ~4-week sum estimate — the chokepoint pattern repeats. |
| 14 | (no code commits — planning session) | **Strategic pivot**: deep investigation into Sky's cross-language inlining design. Two prior-investigation errors corrected: (i) "share LLVMContext with rustc" was overstated — rustc runs one context per CGU and patch (c) just adds Sky's module as another, (ii) "std stays uninlineable" was wrong — rustc's own LTO bitcode-extraction handles `.llvmbc` rlibs natively. **Plan written:** `/Users/verdagon/.claude/plans/parsed-singing-globe.md` (historical now — execution complete). |
| 15 | `0f539d29d0d` (fork) + `690b7d2`, `ec77f6d`, `3f8ac11`, `08a65ad`, `f9f9c03` (main) | **Phase 0–3 of the inline-codegen plan**. Fork patch 4 (`extra_modules` hook on `ExtraBackendMethods` + `ModuleLlvm::parse_from_tcx` + visibility upgrades). Facade: hook installation, `consumer_emit_modules` trait + vtable + trampoline, attribute mirroring. Toylang: codegen migrates from `.ll`/`llc` shell-out to `ModuleCodegen<ModuleLlvm>` via bitcode round-trip; legacy `.o` path retired. Findings: coordinator handshake protocol forced synchronous extras processing; LLVM 20→21 bitcode skew forced Inkwell bump; Inkwell bitcode bug → `llvm-as` shell-out workaround. **264/264 passing.** |
| 15 (cont) | `8fbd928`, `745aed3`, `6bd793a` (main) | **Phase 4.5 Path B + touch points 5+6**. Single-symbol architecture: Sky's bitcode emits each rustc-visible body under the rustc-mangled name; the synthesised `__toylang_impl_*` retirement collapses two symbols → one so ThinLTO sees Sky's body as the sole def. `#![no_builtins]` excludes stub rlibs from LTO's IR linker pool. `lto_smoke` integration fixture + manifest `lto`/`opt-level` knobs verify the LTO path doesn't panic. **265/265 passing.** Key debugging lessons in §5 traps #11–#15. |
| 16 | `63beb0c`, `c0a83fe`, `b92f101`, `1730c53` (main) | **Tier 3 sweep close-out**. #8 (SkyUniverse absorbs consumer struct metadata via type-erased `Arc<dyn Any>`; toylang-side `upstream_structs` mirror retires). #3 (cgu_stash + accessor-inline retired; accessors collapsed into the regular function pipeline via parse-time `synthesize_accessor_pairs`; Case-1b discovery via `default_collect_and_partition()`). #12 close-out (@GCMLZ doc rewrite, lib.rs comment refresh, `FacadeMutableState` inlined; mutex retained for `collect_generic_rust_deps` worker-thread serialisation but no longer trap-fences). `test_lto_smoke` tightened from "doesn't panic" → "cross-language inlining empirically verified via disassembly assertion." **266/266 passing.** **16/18 course-correct items done.** |
| 17 | (one commit, pending) | **`llvm-as` shell-out retired** via in-process IR text round-trip through `Context::create_module_from_ir` (Inkwell's wrapper around `LLVMParseIRInContext`). Investigation reframed the long-running B10 risk: the LLVM 21 bitcode-writer bug lives below the Rust/C boundary (Inkwell's bitcode emitters are thin FFI shims), so no Inkwell patch can fix it. The IR parser canonicalises the in-memory module enough that the round-tripped module emits valid bitcode. `assemble_text_to_bitcode` + `find_sysroot_tool` deleted; vendoring not needed. Architecture doc B10 + Appendix F.8 updated. **266/266 passing.** |

Anchor commits worth knowing: `c38d7e0` is the doc cleanup right before
Session 12 started; `ce437ae` is the last commit with full Approach A
before the Approach B "stage 3" detour (don't go there). The fork lives
at `~/rust` on `per-instance-mir`. Its tip is the revert of patch 4 on
top of patch 4 itself — three patches in effect.

---

## 3. Critical context (the load-bearing pieces)

### 3.1 Approach A: why `per_instance_mir` matters

`rust-interop-architecture.md` §3.1 + §19.1. One paragraph: rustc's mono
collector substitutes generic args inline as it walks. For toylang's
generics (rustc-representable types), Approach B would work. For Sky's
comptime args (arbitrary Sky-typed values), rustc literally cannot
represent them, so substitution MUST happen Sky-side before MIR
construction. The Instance-keyed `per_instance_mir` query is the only
viable mechanism. `docs/historical/approach-a-reference/` has the
structural template from the pre-B-detour era. The
`debug_assert!(!instance.args.has_param())` in
`rustc-lang-facade/src/queries/per_instance.rs::build_dependency_body` is
load-bearing — if it fires, Approach B has snuck back in somewhere.

### 3.2 Sidecar architecture

`rust-interop-architecture.md` §7 + §8 + `docs/architecture/sidecar-format.md`.
Sky libraries compile to **rlib + sidecar only** — no Sky `.o`. The rlib
contains Rust stub source with `unreachable!()` bodies. The sidecar
(`.sky-meta`) is a binary blob (bincode + BLAKE3 checksum + 64-byte
versioned header) carrying the typed AST for every item — exports AND
non-exports. The binary compile reads sidecars from every Sky-marked rlib
and codegens **every reachable Sky item across all libs** into one `.o`.

This is course-correct items #11 + #15 in their locked end state. Done.

### 3.3 The two-symbol architecture

Toylang emits three layers of symbols per item:
1. `__toylang_internal_<name>__<mangled_targs>` — toylang↔toylang ABI.
2. `__toylang_impl_<name>__<mangled_targs>` — Rust-ABI-coerced extern
   wrapper. What Rust callers actually invoke.
3. The rustc-mangled name (`__lang_stubs::wrap::<i32>`) — what Rust
   source sees. `rustc-lang-facade/src/queries/symbol_name.rs:31-80`
   rewrites this to `__toylang_impl_*`.

Important pre-existing-mental-model correction: this is **not** a "symbol
mismatch" needing reconciliation. The symbol_name override IS the bridge.
There's a load-bearing implication for Tier 3 #9 (retiring the
`symbol_name` side-effect channel): the OVERRIDE stays — what goes is the
`notify_concrete_entry_point` call inside it that discovers Instances for
internal-callee stashing.

### 3.4 Two compiles per build

`toylangc build` invokes rustc twice per project:
- **rlib compile**: `__lang_stubs_<project>` crate. Produces rlib +
  sidecar, no Sky `.o`. `is_user_bin_compile = false`.
- **user-bin compile**: user's binary crate. Reads upstream sidecars,
  codegens every reachable Sky body, produces the binary.
  `is_user_bin_compile = true`.

Independent processes; no in-memory shared state across them. The callback
log file is shared on disk. The gate variable lives in
`callbacks_impl.rs` and `main.rs:145-195`.

### 3.5 The `@`-arcana invariants

`docs/architecture/rust-interop-guide.md` Part 8 has the index;
`rust-interop-architecture.md` §26 lists Sky's 14 cross-cutting
invariants. The ones most likely to bite during Tier 3 work:

- **@SyMINCZ** — computing a symbol name does NOT drive codegen.
  Only `ReifyFnPointer` casts in the `per_instance_mir` body do that.
  Critical for #9 — you cannot replace the side-effect with an
  innocent-looking `tcx.symbol_name(instance)` and expect codegen to
  follow.
- **@GCMLZ** — Sky's `MUTABLE_STATE` mutex is held during
  `generate_and_compile`. Query providers must NOT lock it. Session 5's
  thread-local fat-pointer bypass handles re-entrant `symbol_name` calls;
  #12 retires the whole mutex.
- **@DPSFDOZ** — `tcx.def_path_str()` ICEs outside diagnostic contexts.
  Use `tcx.def_path(def_id).data` walks or `tcx.crate_name(def_id.krate)`
  checks instead.
- **@ELASZ** — every `GenericArgs` Sky builds for a Rust item fills
  lifetime slots with `tcx.lifetimes.re_erased`.

---

## 4. File map

### 4.1 Facade — `rustc-lang-facade/src/`

| File | Role | Tier 3 relevance |
|---|---|---|
| `lib.rs` | Trait `LangCallbacks`, `LangPredicates`, two vtables (`PredicateVtable`/`StatefulVtable`), trampolines, `MUTABLE_STATE` mutex, `is_from_lang_stubs` marker walk | **#7 #9 #12** all touch this heavily |
| `queries/per_instance.rs` | Approach A's provider. `debug_assert!(!instance.args.has_param())` is load-bearing. | Mostly stable |
| `queries/layout.rs` | `layout_of` override. Today calls `monomorphize_type` callback. | **#8** rewrites this |
| `queries/symbol_name.rs` | Rust-mangled-name → `__toylang_impl_*` redirect. Today fires `notify_concrete_entry_point` side effect. | **#9** retires the side effect |
| `queries/drop_glue.rs` | `mir_shims` override for consumer types. Calls back for type name. | Touched by #7 |
| `queries/partition.rs` | CGU filter — strips consumer items from rustc's mono partition. | Mostly stable; #2 already retired the linkage mutation |
| `queries/upstream_monomorphization.rs` | Forces consumer types to local mono. | Stable |
| `cgu_stash.rs` | 87-line LIFETIME-ERASED-`'tcx`-to-`'static` stash of upstream CGU references for codegen-time walk. | **#3** deletes this |
| `mir_helpers.rs`, `abi_helpers.rs` | Inherited wholesale per arch §26.5–26.6. | Don't touch |
| `driver.rs` | `Callbacks::after_expansion` hook + sidecar load loop. | **#7** populates the universe here |

### 4.2 Toylang — `toylangc/src/`

| File | Role | Tier 3 relevance |
|---|---|---|
| `toylang/callbacks_impl.rs` | Trait impl, registry state, validation checks, sidecar write, oracle cross-crate probe, Phase 2 round-trip probe, populate-from-CGUs entry-point walk | **#7 #8 #9** all heavily |
| `toylang/registry.rs` | `ToylangRegistry`, `ToyStruct`, `ToyFunction`, `ToyImpl`, `typeid_table` (Phase 1.3). `BTreeMap` for sidecar determinism. | #7's "universe" is structurally the registry |
| `oracle.rs` | rustc-querying helpers. `find_extern_fn_def_id` cross-crate, `is_toylang_opaque`/`extract_typeid_from_args`/`build_opaque_args` (Phase 2), trait/impl resolution. | Stable; touched by #8 (Sky-side layout) |
| `llvm_gen.rs` | LLVM IR emission via Inkwell. Reads `state.toylang_instances` populated by `populate_toylang_instances_from_cgus`. Walks `upstream_cgus(tcx)` at user-bin compile for accessors + Case-1b generics from Rust callers. | **#3** kills the CGU walk once #7+#9 land |
| `stub_gen.rs` | Emits stub rlib's `lib.rs`. Phase E Path 2 — Sky structs are wrapper-as-field newtypes. | Stable |
| `typeid.rs` | BLAKE3-truncated-to-u64 hash over `(name, type_args)`. `Widget` typeid hard-pinned at `0x48723b0bb65d86f7`. | Stable |
| `sidecar.rs` | Sidecar serialization. `SidecarHeader`, `serialize_sidecar`, `deserialize_sidecar`, 15 unit tests. | Stable |
| `main.rs` | Two-mode entry point (orchestrator + rustc-wrapper). `is_user_bin_compile` gating. | Stable |
| `build.rs` | Generates `.toylang-build/` workspace, fans out per-Sky-lib stub crates, wires rust_caller. | Stable |
| `manifest.rs` | `toylang.toml` schema + multi-crate dep graph resolver. | Stable |
| `cgu_stash.rs` (DOES NOT EXIST in toylangc — it's facade-side) | — | — |

### 4.3 Reference materials

- **`rust-interop-architecture.md`** (repo root, 5187 lines) — locked Sky design.
- **`course-correct.md`** (repo root) — 18-item divergence catalog with status table.
- **`docs/architecture/sidecar-format.md`** — sidecar binary format.
- **`docs/architecture/rust-interop-guide.md`** Part 8 — `@`-arcana index.
- **`docs/historical/approach-a-reference/`** — pre-stage-3 Approach A code snapshots.
- **`docs/historical/rebuilding-rustc-fork.md`** — 5-step fork rebuild procedure.
- **`phase-e-investigation.md`**, **`phase-e-rustc-pr-draft.md`** — Path 1 patch + PR draft, preserved for upstream submission (Sky-side no longer needed).
- **`workstream-a-scope-notes.md`**, **`phase3-e6-scope-notes.md`** — Sessions 4 + 5 completion notes; @GCMLZ deadlock + thread-local-fat-pointer pattern documented in the latter.

---

## 5. Discipline (the non-negotiable rules)

From CLAUDE.md (both project + user-global):

- **No `cd && cargo`.** Use `cargo --manifest-path /absolute/path/Cargo.toml`.
  `cd` is OK only when the user explicitly asks.
- **Don't pivot unilaterally.** If you discover the plan won't work, STOP
  and ask before changing direction. Session 5's "don't revert before
  diagnosing" lesson — `sample <pid>` a hanging process before writing
  scope notes speculating about cause.
- **Don't make temporary debug programs.** Use probe patterns (`eprintln!`
  with `[PROBE]` prefix → remove) or add as a test.
- **No `git checkout -- file` to revert.** Use `git diff` and apply
  manually in reverse.
- **Always pipe to a fixed tmp file per session.** This session used
  `./tmp/quarter-of-work.txt` — same name for the whole session, never
  rotate, never chain `cargo test | tail`.
- **Relative paths in `cargo` commands.** No `/Volumes/V/...` or
  `/Users/verdagon/...`.

### Real traps you will hit

1. **Stale `integration-projects-cache`.** When tests fail in seemingly
   random ways: `rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache`.
   Bit prior sessions multiple times.
2. **Wrapper-vs-build mode.** `toylangc` is BOTH the orchestrator AND the
   rustc wrapper. Same binary, different argv. See `main.rs::run_wrapper_mode`
   vs `build::build_project`. Debugging "why isn't my callback firing"
   starts with determining WHICH MODE the failing invocation was in.
2. **`tcx.crates(())` excludes the local crate.** At rlib-compile time the
   wrapper / Sky items live in LOCAL_CRATE; at user-bin-compile time they
   live in extern crates. The `find_toylang_opaque_def_id` pattern
   (oracle.rs Phase 2) is the template: walk LOCAL first, then
   `tcx.crates(())`.
3. **`tcx.output_filenames(())`** — key is `()`, not a CrateNum. Easy to
   get wrong.
4. **@SyMINCZ.** Computing a symbol name doesn't drive codegen. A new call
   to `tcx.symbol_name(instance)` registers nothing; only `ReifyFnPointer`
   casts in the `per_instance_mir` body do.
5. **@TVIMDGAZ.** When building an Instance for `<MyType as Trait>::method`,
   use the trait def's method DefId with `[Self=MyType, …]` args; let
   `Instance::expect_resolve` map to the impl method.
6. **`instantiate_identity()` needs a comment.** Per CLAUDE.md compiler
   law. Only valid for structural inspection. Every call site explains why
   we're not substituting.
7. **bincode v2 ≠ v1.** Use `bincode::serde::encode_to_vec` /
   `decode_from_slice` (NOT `bincode::serialize` / `bincode::deserialize`).
8. **§4.5 marker-parentage check.** Glob re-exports (`use __lang_stubs::*;`)
   can lift `__SKY_STUBS_MARKER` into a downstream crate's
   `module_children`. The parentage check (`def_id.krate == cnum`) protects
   against this. `find_toylang_opaque_def_id` already does it; replicate
   for any new universe lookup.
9. **The `[toylang] layout_of intercepted for: ...` stderr line.** Layout
   probe integration tests grep this. If #8 changes the format, fix the
   tests too — search for `layout_of intercepted` in
   `toylangc/tests/integration_projects.rs`.
10. **The `arch-fence-allow` markers.** `tests/architecture_fence.rs`
    scans `callbacks_impl.rs`, `type_resolve.rs`, and `stub_gen.rs` for
    `type_params.is_empty()` / `type_args.is_empty()` branches.
    Substituted-args fast paths + degenerate-case helpers carry inline
    `// arch-fence-allow: <reason>` markers (on the same line OR the
    immediately-preceding line — not further back).
11. **LTO tests need the wrapper engaged.** When manually testing under
    `[profile.dev] lto = "thin"` (e.g., reproducing the Path B / Phase 4.5
    empirical proof), invoking `cargo build` directly bypasses
    `RUSTC_WORKSPACE_WRAPPER` and the facade's
    `extra_modules_hook` is never installed — Sky contributes zero
    bitcode, the binary panics or links to the stub's `unreachable!()`,
    and you wrongly conclude "patch (c) is broken under LTO." Set
    `RUSTC_WORKSPACE_WRAPPER=$ERW/target/debug/toylangc` AND
    `DYLD_LIBRARY_PATH=$RUSTUP_HOME/toolchains/rustc-fork/lib` (plus
    `LD_LIBRARY_PATH` on Linux) before invoking cargo. `toylangc build`
    sets these automatically; direct `cargo build` does not. Watch for the
    probe line `[lang-facade] extra_modules hook fired; consumer
    returned N module(s)` (gated on `LANG_FACADE_EXTRA_MODULES_PROBE=1`)
    to confirm the hook actually runs.
12. **Cargo profile overrides only live at workspace root.** A
    `[profile.dev]` block in a workspace MEMBER package is silently
    ignored. To enable LTO for an integration test, edit the
    workspace's top-level `Cargo.toml`
    (`.toylang-build/Cargo.toml`), not the member's
    (`.toylang-build/user_bin/Cargo.toml`). Cargo doesn't warn; the
    rustc command just won't carry `-C lto=thin`.
13. **Path B touch-point 3 misjudged the plumbing.** The Phase 4.5 plan
    said `ToylangInstance.extern_symbol` flows from
    `compute_consumer_symbol`; it actually flows from `compute_fn_symbol`
    (line ~1525) and `compute_fn_symbol_from_type_args` (now
    `compute_internal_symbol_from_type_args`, line ~1119) at four sites:
    `callbacks_impl.rs:241/400/1337` and `llvm_gen.rs:2048`. PLUS the
    `llvm_gen.rs` string-replace `extern_symbol.replace(
    "__toylang_impl_", "__toylang_internal_")` at the codegen loops
    (lines 2096, 2101) — broken the moment extern_symbol becomes the
    rustc-mangled name. Future "trust the plan's surface area" reviews
    should walk the actual call graph for the field being changed.
14. **`find_trait_impl_method_def_id` had a hidden ambiguity.** Pre-Path
    B, when the consumer's self-type-name was `Box`, the helper's
    `tcx.all_impls(Clone)` walk matched both `case6_lib::Box` (Sky-
    defined) AND `std::ffi::os_str::Box<OsStr>` (stdlib-defined). The
    pre-Path-B synthesized `__toylang_impl__Box__Clone__clone` extern
    name didn't care which DefId was returned; Path B uses the DefId's
    rustc-mangled name directly, so picking the std impl produced a
    symbol Sky never defines. Fix at `oracle.rs:705-712` filters
    `tcx.all_impls` results via
    `rustc_lang_facade::is_from_lang_stubs(tcx, adt_def.did())`. Any
    future oracle helper that walks `tcx.all_impls(...)` for a consumer
    type should apply the same filter — the self-type-name check is
    name-only and inherently ambiguous across the crate graph.
15. **`#[inline(never)]` is not a fix for symbol-priority bugs.** When
    two strong defs of a symbol compete (e.g., stub rlib's
    `unreachable!()` body vs Sky's real body under ThinLTO),
    `#[inline(never)]` on one side doesn't change which def wins — it
    only relocates *where* the panic happens. The fix is at the symbol-
    resolution layer (LTO inclusion via `#![no_builtins]`, linkage
    attributes, partitioner filtering), not at the inliner layer. Don't
    reach for inline controls to fix a definition-priority bug.

---

## 6. Where to start now — Tier 3 plan

### 6.0 Latest direction (Session 14) — read this first

**Execute the plan at `/Users/verdagon/.claude/plans/parsed-singing-globe.md`.**
It supersedes the original §6.1 sequencing below for items #3, #4-deeper,
and #12.

What changed: Session 14's deep investigation found that the architecturally
right way to retire `cgu_stash`, finish #4's inline-codegen rewrite, and
retire `MUTABLE_STATE` is to land all three as a single inline-codegen
prototype that **also empirically validates cross-language ThinLTO inlining**
— Sky's load-bearing perf claim. The pieces fall together because:

- A new fork patch (~15 lines + visibility upgrades on `ExtraBackendMethods`)
  exposes a `extra_modules` hook between rustc's CGU loop and
  `codegen_finished`.
- Toylang's `consumer_emit_modules` returns its IR as a `ModuleCodegen<ModuleLlvm>`
  via Inkwell→bitcode→`ModuleLlvm::parse` round-trip.
- Sky's module rides rustc's normal optimize → ThinLTO-summary → emission
  pipeline as just another CGU. Cross-language inlining (including std)
  happens via rustc's existing LTO machinery — no LLD plugin needed.
- `generate_and_compile` retires → `cgu_stash` retires → `MUTABLE_STATE`
  retires. Three Tier 3 items in one ~6–8 day workstream.

Plan structure (per the plan file): Phase 0 fork patch, Phase 1 smoke test
(hard-coded extra module), Phase 2 toylang as `ModuleCodegen<ModuleLlvm>`,
Phase 3 attribute matching (mirror `rustc_codegen_llvm/src/attributes.rs:376-583`),
Phase 4 cross-language inlining fixtures + benchmark, Phase 5 Tier 3 cleanups
(retire `MUTABLE_STATE` + `cgu_stash`).

**Decisions confirmed by the user before plan was finalized:**
- IR transport: bitcode round-trip (safe; avoids LLVMContext sharing).
- Old `.o` path removed once patch (c) works (no dual-path maintenance).
- Net fork patch count goes from 3 to 4. Upstream PR for the hook is on
  the deferred list; the patch is forward-portable and would benefit
  cranelift/gcc-rs/spirv etc.

**Tier 3 item status after Session 14 plan is executed:**
- #3 (retire `cgu_stash`) — falls out of Phase 5.
- #4-deeper (inline-codegen rewrite) — IS the plan.
- #12 (retire `MUTABLE_STATE` + two-vtable) — falls out of Phase 5.
- #8 (`layout_of` walks Sky-side) — **still its own item**, not in scope for
  this plan; do it after. ~1–2 days under the chokepoint pattern.
- #13 (wrapper-mode retirement) — still explicitly deferred.

The §6.1–6.6 detailed sub-plans below are kept as **historical reference**
for items #7 (done) and #9 (done). For #3, #8, #12 the new plan file is
authoritative.

### 6.1 Dependency graph (historical; for #7/#9 reference)

```
       #7 LangPredicates → SkyUniverse
         |       |       |
         v       v       v
        #9      #8       |
         |       |       |
         v       |       |
        #3       v       |
                #4-deeper (inline codegen) ──┐
                 |                            |
                 +────────────────────────────v
                                     #12 retire MUTABLE_STATE
```

- **#7** is the foundation. It introduces a facade-owned `SkyUniverse`
  data structure (basically owning what's today `ToylangRegistry +
  upstream_registries`) and retires every `is_consumer_type(name)` /
  `is_consumer_fn(name)` vtable call. Without #7, the rest don't have a
  place to put what they're moving.
- **#8** is mostly orthogonal — Sky-side layout walker replacing the
  `monomorphize_type` callback. Benefits from #7's universe but can be
  done first if you want.
- **#9** depends on #7 (somewhere to register Instances at after_expansion
  instead of via `symbol_name` side effect).
- **#3** falls out of #9 (once `symbol_name` isn't driving discovery, the
  remaining CGU walk is just for accessors + Case-1b, which can be
  rewritten directly during codegen without a `'static`-stashing dance).
- **#12** falls out of #7 + #9 + the deeper half of #4 (the
  `codegen_crate` rewrite to walk the queue inline via Inkwell). Today
  Session 6 only landed #4's *channel* part; the *emission* part is still
  outstanding.

**Recommended sequencing:** #7 first (foundation), then #8 in parallel
with #9, then #3 as cleanup, then the deeper #4 + #12 together. Total:
~8–10 weeks sequential, ~6–8 with parallelism.

### 6.2 Item #7 — replace `LangPredicates` with sidecar-loaded universe

**Effort:** ~2 weeks.

**The problem today.** The facade asks the consumer "is `Foo` a consumer
type?" via vtable. `rustc-lang-facade/src/lib.rs:89` declares the trait:

```rust
pub trait LangPredicates: Send + Sync + Any {
    fn is_consumer_type(&self, name: &str) -> bool;
    fn is_consumer_fn(&self, name: &str) -> bool;
}
```

Call sites (the ones that need to change):
- `rustc-lang-facade/src/lib.rs:346` `is_consumer_type` helper
- `rustc-lang-facade/src/lib.rs:492` `is_consumer_accessor_safe`
- `rustc-lang-facade/src/lib.rs:519` `is_consumer_trait_impl_method`
- `rustc-lang-facade/src/lib.rs:530` `is_consumer_fn` helper
- `rustc-lang-facade/src/queries/layout.rs:65`
- `rustc-lang-facade/src/queries/drop_glue.rs:53`
- `rustc-lang-facade/src/queries/symbol_name.rs:42`
- `rustc-lang-facade/src/queries/per_instance.rs:62`

Plus the trampoline `trampoline_is_consumer_type` at `lib.rs:682` and the
predicate-vtable plumbing at `lib.rs:230-237, 821`.

Sky's locked design (architecture §7, §8, §9, §10.8): a content-addressed
**Sky universe** owned by the facade, populated at sidecar-load time
(`Callbacks::after_expansion` — the hook is already at the right place).
Predicates are O(1) lookups against the universe with no vtable hop.

**The migration.**

1. **Define the universe.** In `rustc-lang-facade/src/lib.rs`, introduce:

   ```rust
   pub struct SkyUniverse {
       pub typeids: HashSet<u64>,
       pub fn_names: HashSet<String>,      // for is_consumer_fn
       pub type_names: HashSet<String>,    // for is_consumer_type
       // Future: full Temputs entries for #8's Sky-side layout walk.
   }
   ```

   Stored in an `OnceLock<RwLock<SkyUniverse>>` (replacing what
   `MUTABLE_STATE` partially does). Note: #12 eventually retires
   `MUTABLE_STATE`; #7 introduces the lock-free read path that #12 will
   inherit.

2. **Populate at sidecar load.** Today's `LangCallbacks::on_sky_lib_loaded`
   takes raw bytes and lets the consumer deserialize. Add a parallel
   facade-side path that extracts the universe-relevant subset from each
   sidecar's `ToylangRegistry` (or whatever the consumer's typed AST type
   is) — alternatively, change `on_sky_lib_loaded`'s signature to return
   the relevant subset. Plus a `before_main_pass` hook that populates the
   universe with the LOCAL crate's items (since the local sidecar isn't
   written yet at this point).

3. **Migrate call sites.** Each `crate::is_consumer_type(&name)` becomes
   `crate::sky_universe().contains_type(&name)`. Lock-free reads
   (`RwLock::read()`).

4. **Retire the vtable slot.** Once all call sites use the universe,
   `LangPredicates::is_consumer_type` becomes an empty trait or is
   removed entirely. Toylang's
   `callbacks_impl::is_consumer_type/is_consumer_fn` impls go away.

5. **Toylang side.** `ToylangState.upstream_type_names` and
   `upstream_structs` (Phase 3 E.5 mirrors) can stay or be replaced by
   reading directly from the facade's universe via a `get_universe()`
   accessor — minor cleanup.

**Verification.**
- Suite passes 262/262 throughout. Each migration step is one PR; tests
  green between steps.
- Add a unit test that asserts `SkyUniverse::contains_type("Widget")`
  returns true after loading a fixture sidecar.
- Smoke test: temporarily replace one `is_consumer_type` call site with
  a sentinel that panics if reached → verify the universe path fires
  instead, then remove the sentinel.

**Pitfalls.**
- The universe needs to be populated BEFORE the first query that consults
  it. `Callbacks::after_expansion` fires once per rustc invocation; the
  sidecar loader runs there too (driver.rs:130). For local-crate items,
  populate from the consumer's just-built registry; that happens later in
  `after_rust_analysis`. So queries between `after_expansion` and
  `after_rust_analysis` (Rust-source typecheck) need the universe
  pre-populated, but won't yet have local items — handle this gracefully
  (queries about local items can't fire before the local registry is
  built; queries about upstream items can).
- The cross-crate parentage check from #4.5 (def_id.krate match) is still
  needed when registering items into the universe — don't accept items
  from re-exports.

**Architecture refs:** §7.1–7.5, §8, §9.4, §10.8, §10.9.

### 6.3 Item #8 — `layout_of` walks Sky-side

**Effort:** ~1–2 weeks.

**The problem today.** `rustc-lang-facade/src/queries/layout.rs:70`
calls back to the consumer:

```rust
let result = crate::call_monomorphize_type(&name, tcx, ty);
let layout = build_layout(tcx, ty, &result.field_types, query.typing_env);
```

`call_monomorphize_type` (lib.rs:545) dispatches through `StatefulVtable`
to the consumer's `LangCallbacks::monomorphize_type`. The consumer
(toylang) looks up `name` in its registry, substitutes type params with
concrete args from `ty`, returns `MonomorphizeResult { field_types:
Vec<Ty<'tcx>> }`. The facade then queries `tcx.layout_of` on each field.

Sky's locked design (architecture §10.3–10.5, §8.8): `layout_of` walks
Sky's universe **recursively itself**, no callback. Sky owns the layout
machinery end-to-end.

**The migration.**

1. **Make the universe (from #7) carry full Temputs.** Each entry needs
   `Vec<Field { name, ResolvedType }>` and `type_params` — enough to do
   the substitution Sky-side.

2. **Add a Sky-side layout walker.** Most of toylang's
   `monomorphize_type` impl (callbacks_impl.rs around line 790) becomes a
   facade-side fn: takes `Ty<'tcx>`, extracts `(name, args)`, looks up
   the universe entry, substitutes its `field_types` per `args`,
   recursively converts each substituted `ResolvedType` back to
   `Ty<'tcx>` (the inverse of `rustc_ty_to_resolved_type`), then composes
   layout.

3. **The `ResolvedType → Ty<'tcx>` conversion** is the part that needs
   the most care. Toylang's `oracle::try_resolved_to_rustc_ty` already
   does this. Either move it facade-side (via universe), or keep it
   toylang-side but expose a single conversion callback to the facade
   (lighter touch).

4. **Retire `monomorphize_type` callback.** `LangCallbacks::monomorphize_type`
   trait method removed; `StatefulVtable.monomorphize_type` slot removed.
   The consumer no longer needs to deal with layouts at all.

**Verification.**
- All layout-probe integration tests pass (search
  `toylangc/tests/integration_projects.rs` for `layout_of intercepted`).
- The `[toylang] layout_of intercepted for: <ty> size=N align=M` log
  line stays at the same format (probe tests assert on it).
- Cross-check: `r_t_r_vec_of_ship` continues to work (the wrapper-as-field
  shape, layout-field-count match — Phase E Path 2).

**Pitfalls.**
- The `ResolvedType` representation has Sky-specific kinds (`TypeParam`,
  `RustType`, `StructRef`, etc.). Moving it facade-side requires either
  the facade depending on the consumer's typed-AST type, or generalizing
  it to a small lingua franca. Recommend keeping consumer-defined Temputs
  facade-stored as `Box<dyn Any>` + a registered conversion callback —
  not a perfect retirement of the callback but a strict simplification.
- Recursive layout queries can re-enter Sky's `layout_of` override.
  Currently safe because `monomorphize_type` is stateless (@GCMLZ note in
  layout.rs:43–52 documents this). Maintain statelessness in the new
  walker.

**Architecture refs:** §8.8 "no pre-computed layouts," §10.3–10.5, §13.7
(comptime adds work but is out of scope here).

### 6.4 Item #9 — retire `symbol_name` side-effect channel

**Effort:** ~1–2 weeks. Depends on #7.

**The problem today.** `rustc-lang-facade/src/queries/symbol_name.rs`
overrides `tcx.symbol_name(instance)`. For consumer Instances it
synthesizes the `__toylang_impl_*` symbol name (which is fine — that's
the architectural bridge per §3.3 above). BUT it also fires
`call_notify_concrete_entry_point` (symbol_name.rs:96) as a side effect.
This is how toylang discovers internal-callee Instances for stashing —
when rustc's mono walk queries the symbol name of a Sky item, toylang
records the Instance so its later codegen pass can emit it.

The Session 5 thread-local fat-pointer bypass exists because
`generate_and_compile` holds `MUTABLE_STATE` and the symbol_name override
re-enters trying to lock it.

Sky's locked design (architecture §19, §20.4, §26.1 SyMINCZ): discovery
happens at `Callbacks::after_expansion` via universe walk; the codegen
queue is populated there, not via `symbol_name` side effects.
`symbol_name` becomes a pure read.

**The migration.**

1. **Move the discovery to after_expansion.** Today's
   `populate_toylang_instances_from_cgus` (callbacks_impl.rs) is an
   entry-point walk that already does most of this — Phase C (Session 11)
   migrated to §20.4 shape. Extend it to also enumerate the items that
   `symbol_name`'s side effect was discovering: anything reachable from
   exports + main + trait-impl methods, traversing toylang→toylang calls.

2. **Remove the side effect from `symbol_name`.**
   `rustc-lang-facade/src/queries/symbol_name.rs:96` — delete the
   `call_notify_concrete_entry_point` call. The override becomes:
   "rewrite the symbol name, return it, no state mutation."

3. **Retire the callback trait method.**
   `LangCallbacks::notify_concrete_entry_point` (lib.rs:172) goes away.
   `StatefulVtable.notify_concrete_entry_point` slot too. The thread-local
   fat-pointer bypass becomes obsolete (no re-entrance because no
   side-effecting call).

4. **Consumer state cleanup.** Toylang's `walked_entry_points` set
   (callbacks_impl.rs) shrinks — no longer needs to dedupe against
   symbol_name firings.

**Verification.**
- Suite passes 262/262.
- The Phase 2 round-trip probe (`test_phase2_const_u64_round_trip`) still
  fires.
- Smoke test: temporarily add an `eprintln!("FIRED")` where
  `call_notify_concrete_entry_point` was, run the suite, observe it
  doesn't fire anymore.

**Pitfalls.**
- **@SyMINCZ is a real trap, not just an arcanum.** If you move discovery
  somewhere that uses `tcx.symbol_name` as a "now register this Instance
  please" hint, you'll silently miss Instances rustc never queries for.
  Only `ReifyFnPointer` casts inside `per_instance_mir` bodies drive
  rustc's mono collector.
- Don't accidentally break the `__toylang_impl_*` symbol rewrite. The
  override needs to STAY (that's the architectural bridge). What goes is
  ONLY the `call_notify_concrete_entry_point` call within it.
- The thread-local fat-pointer bypass (`phase3-e6-scope-notes.md`)
  exists specifically for symbol_name re-entrance during
  `generate_and_compile`. Once #9 lands, that bypass is dead code — but
  don't remove it in the same PR as #9; do it in a follow-up so you can
  bisect if anything's left.

**Architecture refs:** §19, §20.4, §26.1 (SyMINCZ), §26.2 (GCMLZ).

### 6.5 Item #3 — retire `cgu_stash.rs`

**Effort:** ~3–5 days. Depends on #7 + #9.

**The problem today.** `rustc-lang-facade/src/cgu_stash.rs` (87 lines)
holds CGU references with their `'tcx` lifetime erased to `'static`. The
consumer's codegen path (`toylangc/src/llvm_gen.rs:1994`) calls
`upstream_cgus(tcx)` to walk them and discover items rustc surfaced via
mono — specifically:

1. **Accessor methods** discovered via `opt_associated_item`.
2. **Case-1b generic toylang fns** instantiated from Rust callers
   (`__lang_stubs::wrap::<LocalThing>` in a `rust_caller.rs`). The
   registry walk skips these (no concrete args at root); the CGU walk is
   the only discovery path.

Sky's locked design (architecture §19, §20.4): the codegen queue is
populated at after_expansion. Cross-language generic instantiation (Case
1b) still flows through rustc's mono collector — that's architecturally
correct — but the discovery happens DURING `codegen_crate`'s queue walk,
not via a separately-stashed list.

**The migration.**

1. **Move accessor discovery to after_expansion.** Every Sky struct's
   fields are known from the universe (#7). Each `(struct, field)` pair
   becomes an entry point. No more `opt_associated_item` walk needed.

2. **Move Case-1b discovery into `codegen_crate`.** The user-bin
   compile's codegen path already iterates the CGU list (via the partition
   override at lib.rs / `collect_and_partition_mono_items`). Replace the
   `upstream_cgus(tcx)` stash dance with a direct iteration during
   codegen — pick up consumer Instances from the filtered CGU list while
   we still have the live `'tcx` (no need to stash, no lifetime erasure).

3. **Delete `cgu_stash.rs`.** And `upstream_cgus(tcx)`. And the
   corresponding `stash_cgus` call in `partition.rs`.

**Verification.**
- All `case1b*`, `case4*`, `case6*` fixtures still pass (those exercise
  Case 1b / Rust-walks-Sky-impl paths).
- Suite passes 262/262.
- Probe: temporarily replace `upstream_cgus(tcx)` with `panic!("stash
  consulted")` and verify nothing calls it.

**Pitfalls.**
- Accessor methods are weird — they exist as Sky-emitted symbols
  (`Foo::field` accessors), called from Rust source via the stub rlib's
  `impl Foo { pub fn field(&self) -> &T { unreachable!() } }`. The
  discovery channel today is "rustc's mono collector queues them; CGU
  stash holds them; toylang's codegen picks them up." Under #3, the queue
  must enumerate every accessor of every Sky struct as a potential entry
  point. They're cheap (one per field) so over-enumeration is fine.
- The `'tcx`-to-`'static` erasure in cgu_stash.rs is a controlled unsafe
  block. The codegen-time walk in #3 doesn't need it (we have live
  `tcx`). Don't add a new lifetime-erased path just because the old one
  existed.

**Architecture refs:** §19, §20.4, §20.8.5 (cross-crate Sky generic mono).

### 6.6 Item #12 — **DONE** (see commit history). All three deliverables
landed as side effects:

| Original deliverable | What actually retired it | When |
|---|---|---|
| Collapse the two vtables (`PredicateVtable` + `StatefulVtable`) | Tier 3 #7.4: predicates moved to `SkyUniverse` and the predicate vtable retired. Only `StatefulVtable` remains. | Session 13 |
| Retire the thread-local fat-pointer bypass (Session 5) | Tier 3 #9: `symbol_name` side effect retired, no re-entrant lock path remains, bypass deleted with it. | Session 13 |
| Replace `Mutex` with `RwLock` for read-mostly state | **Doesn't apply.** The `SkyUniverse` is already `RwLock` — that's where the read-mostly content lives. `MUTABLE_STATE` only wraps the writer state for the 4 stateful callbacks. None are readers. | (n/a) |

**What about `MUTABLE_STATE` itself?** Still load-bearing —
`collect_generic_rust_deps` fires from `lang_per_instance_mir` during
rustc's mono walk on rayon worker threads, so the mutex serialises
concurrent fires. Retiring it would require alternative thread sync
that isn't worth the churn for toylang's volume. The mutex's role is
now plain inter-callback serialisation, not @GCMLZ trap-fencing — see
`docs/arcana/GenerateCompileMutexLock-GCMLZ.md` for the current
locking contract.

The close-out commit refreshed:
- `@GCMLZ` arcana: history-and-current-reality, replacing the
  obsolete "two-vtable split enforces lock-freedom at type level"
  language.
- `lib.rs` comments throughout (the "Consumer callback trait"
  header, the `monomorphize_type` doc, the global state separation
  block).
- `FacadeMutableState` struct inlined to `Box<dyn Any + Send + Sync>`
  (was a single-field wrapper).
- This handoff §6.6 + course-correct.md #12 + status snapshot.

### 6.7 What's NOT in this plan

- **#13** (wrapper-mode `@MRRIWMZ` retirement). Architecture §4.1–§4.5.
  ~4–6 weeks; touches install, distribution, the whole startup model.
  Sequence with Sky's own toolchain shipping, not as part of this rebuild
  series.
- **#10** (partially done; `collect_generic_rust_deps` Instance-keyed via
  Approach A landed in Session 2). The remaining "Instance-keyed" surface
  for Sky's full design needs `per_instance_mir` to be Instance-keyed at
  the rustc query layer — which IS the existing fork patch. Done in
  effect.

### 6.8 If something goes sideways

Per Session 5's lesson: **diagnose before reverting.** A 0%-CPU hang is
`sample <pid>`; a panic is `RUST_BACKTRACE=full`; a silent miss is a
probe. The thread-local fat-pointer pattern in
`phase3-e6-scope-notes.md` is the reference for re-entrance issues.

Per Session 4's lesson: **half-done refactors compound.** If a Tier 3
item feels MUCH harder than estimated, check whether an EARLIER refactor
(stage 5a's oracle cross-crate sweep was a real example) is half-done in
the area you're touching. Finish it first, then return to the planned
work.

---

## 7. Operational tips

### 7.1 Running tests

```bash
# Full suite (run with cache wiped):
rm -rf /Users/verdagon/erw/toylangc/target/integration-projects-cache
cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
grep -aE "^test result|FAILED" /Users/verdagon/erw/tmp/quarter-of-work.txt

# Just unit tests (toylangc bin):
cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --bin toylangc <pattern>

# Just one integration test:
cargo +rustc-fork test --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --test integration_projects <name>
```

Re-use `tmp/quarter-of-work.txt` for every command in your session.

### 7.2 Direct toylangc invocation (for probing)

```bash
cargo +rustc-fork run --manifest-path /Users/verdagon/erw/toylangc/Cargo.toml --quiet -- \
    build /Users/verdagon/erw/toylangc/tests/integration_projects/<fixture>/toylang.toml \
    > /Users/verdagon/erw/tmp/quarter-of-work.txt 2>&1
```

Bypasses cargo test, captures all stderr. Useful for seeing eprintln
output cargo test swallows.

### 7.3 Rebuilding the rustc fork

When you change `~/rust/compiler/`, see
`docs/historical/rebuilding-rustc-fork.md`. Five steps:

1. `cd ~/rust && python3 x.py dist rustc-dev` (~10 min)
2. `cd /tmp && tar xzf ~/rust/build/dist/rustc-dev-1.95.0-dev-aarch64-apple-darwin.tar.gz && cd rustc-dev-* && bash install.sh --prefix=$HOME/rust/build/host/stage2` (~3 min — see note below)
3. `rm -rf $HOME/rust/build/host/stage2/lib/rustlib/rustc-src` (REQUIRED — without this step 2 takes 30+ min on subsequent rebuilds because of `.old.old.old.old.old.old.old.old` backup cascades)
4. `cd ~/rust && python3 x.py build library --stage 2` (~5 min)
5. **REINSTALL rustc-dev** (steps 2 again) — step 4 wipes
   `lib/rustlib/<target>/lib/librustc_*.rmeta`. Without this you get 50+
   "can't find crate for `rustc_abi`" errors.

Total: ~20 min for a clean rebuild. Cached LLVM is enabled in
`config.toml`.

### 7.4 Sidecar inspection

No `skyc inspect` tool (deferred per arch §8.9). To inspect a sidecar
during debugging, add a temporary test in `sidecar.rs::tests` that calls
`deserialize_sidecar` on a known path and prints. Don't write a
freestanding tool.

---

## 8. Status snapshot (where you start)

**Tests**: **266/266** (107 toylangc unit + 3 facade unit + 1 fence + 140
integration + 16 standalone — includes `test_lto_smoke`'s tightened
cross-language inlining assertion) when run with
`integration-projects-cache` wiped.

**Seven-case taxonomy**: 7/7 tested (1a/1b/2/3/4/5/6).

**Course-correct.md items done**: **16/18** (#1, #2, #3, #4, #5, #6,
#7, #8, #9, #11, #12, #14, #15, #16, #17, #18). #10 partial. Only #13
remaining — wrapper-mode `@MRRIWMZ` retirement, bundles with Sky's
actual toolchain shipping. Out of scope for toylang.

**No active plan.** The previous active plan
`/Users/verdagon/.claude/plans/parsed-singing-globe.md` (inline-codegen
+ Tier 3 #3 + #12) is **historical** — Sessions 15 + 16 executed it
end-to-end. Preserved on disk for reference; nothing pending.

**Fork state**: `~/rust` on `per-instance-mir`, **4 patches** in effect:

1. `per_instance_mir` query declaration (`rustc_middle/src/query/mod.rs`).
2. Mono collector hook (`rustc_monomorphize/src/collector.rs`).
3. Default-None provider (`rustc_mir_transform/src/lib.rs`).
4. `extra_modules` hook + `ModuleLlvm::parse_from_tcx` + visibility
   upgrades on `submit_codegened_module_to_llvm` (`rustc_codegen_ssa` +
   `rustc_codegen_llvm`). Session 15 fork commit, supports patch (c)
   inline-codegen.

Built for nightly-2026-01-20 / rustc 1.95.0-dev / commit `d940e568`.
Installed as toolchain `rustc-fork`. The debuginfo-clamp patch
(`e67de69ef35`) that briefly landed in Session 11 was reverted
(`003f91e4df9`) in Session 12 — `__ToylangOpaque<HASH>` wrapper-as-field
made it unnecessary.

**Toolchain pin**: `rust-toolchain.toml` channel = `"rustc-fork"`. Four
sites stay in sync (toolchain file + `TOYLANG_NIGHTLY` in main.rs + two
test files).

**Codegen architecture**: post-Workstream-A + Phase 4.5 Path B —
rlib compile produces rlib + sidecar only (no toylang `.o`). User-bin
compile contributes Sky's bitcode via `consumer_emit_modules` → patch
(c) `extra_modules` hook, which submits a `ModuleCodegen<ModuleLlvm>`
into rustc's optimize → ThinLTO summary → emission pipeline. Sky's
bitcode emits each rustc-visible body under the rustc-mangled name
(single-symbol architecture); `#![no_builtins]` excludes the stub rlib
from LTO's IR linker pool so Sky's body is the sole def.

**Sky struct stub shape**: Phase E Path 2 wrapper-as-field newtype.
Non-generic: `pub struct Foo(__ToylangOpaque<HASH>);`. Generic:
`pub struct Foo<P...>(__ToylangOpaque<HASH>, PhantomData<(P...)>);`.

**CI fences** (any regression fires a named test):

- **§9 export commitment**: `non_export_body_bearing_fn_gets_no_stub_shell`
  (stub_gen unit test).
- **§4.4 byte-identical pass-through**:
  `test_a5_byte_identical_pass_through` (standalone test).
- **Compiler-law generic/non-generic uniformity**:
  `tests/architecture_fence.rs` (scans `callbacks_impl.rs` +
  `type_resolve.rs` + `stub_gen.rs` for unmarked
  `type_params.is_empty()`/`type_args.is_empty()` branches).
- **Cross-language ThinLTO inlining**: `test_lto_smoke`'s
  `assert_sky_inlined_into_main` — disassembles the binary, asserts the
  user_bin's Rust `main` contains no `bl` to Sky symbols. Fixture sets
  `lto = "thin"` + `opt-level = "3"` via manifest's new `Project::lto`
  + `Project::opt_level` fields.

**Working tree**: clean.

**Recent commits worth knowing** (newest first):

| Commit | What |
|---|---|
| `1730c53` | **`test_lto_smoke` tightened** — cross-language inlining proven via disassembly assertion. Manifest gains `opt-level`. |
| `b92f101` | **Tier 3 #12 close-out** — @GCMLZ doc rewrite, lib.rs comment refresh, `FacadeMutableState` inlined. |
| `c0a83fe` | **Tier 3 #3 retire `cgu_stash`** — accessors collapsed into the regular function pipeline via parse-time synthesis; Case-1b via `default_collect_and_partition()`. |
| `63beb0c` | **Tier 3 #8** — `SkyUniverse.struct_infos` (typed-erased `Arc<dyn Any>`); consumer-side `upstream_structs` mutex mirror retires. |
| `6bd793a` | `lto_smoke` fixture + integration test (originally just "doesn't panic"; tightened later in `1730c53`). |
| `745aed3` | `#![no_builtins]` excludes stub rlibs from LTO's IR linker pool. |
| `8fbd928` | **Phase 4.5 Path B** — single-symbol architecture; Sky's bitcode emits under rustc-mangled name. |
| `f9f9c03` | Phase 2 step 3 — inline-codegen path becomes default; legacy `.o` path retired. |
| `0f539d29d0d` (fork) | Phase 0 fork patch 4 — `extra_modules` hook + `parse_from_tcx` + visibility upgrades. |
| `fa3fdd3`, `c801638` | Tier 3 #7 + #9 — `LangPredicates` → `SkyUniverse`; `symbol_name` side effect retired. |
| `7f6bf97`, `72a929e` | Phase E Path 2 — `__ToylangOpaque<HASH>` wrapper-as-field migration. |
| `b56cf4c`, `1b738e6` | Phase 2 C Case 4 + Session 10 `export` keyword. |
| `671f002` | Approach A + Sidecar + Workstream A + Phase 3 multi-crate (big bang). |

Use `git log <commit>..HEAD` to walk forward.

---

## 9. When to escalate

Ping the user (don't pivot unilaterally) if:

- The rustc fork needs MORE patches beyond what the active plan calls for
  (the plan adds one — the `extra_modules` hook).
- You hit a test failure you can't explain after wiping cache twice.
- A plan phase's estimate slips past 1.5× the planned days. The plan is
  conservative; significant overrun signals a half-done earlier refactor
  or a design mismatch.
- You're tempted to revert Workstream A (Session 4), Phase 2 C's
  symbol_name routing (Session 8), Phase E Path 2's wrapper-as-field
  shape (Session 12), or the Tier 3 #7+#9 retirements (Session 13).
  These are load-bearing; any revert is an architectural regression.
- The plan's bitcode round-trip cost dominates (>100ms per build) —
  alternative IR transports were ranked-and-rejected but are on the table
  if cost is real.
- The plan's std-inlining fixture (Phase 4.2) doesn't pass — investigate,
  then either document `build-std` as opt-in or escalate. First-party
  inlining (Phase 4.1) passing is the minimum-acceptable outcome; std
  inlining is the stretch goal.
- Anything past the plan's Phase 5 is being started without an explicit
  "yes, do this next" agreement (e.g., #8 is not in the plan).

For routine "this took longer than I estimated" — keep going.

**Lessons from prior sessions worth re-reading:**

- **Session 4 — the half-done refactor pattern.** Workstream A's
  original ~2–3 week sizing didn't account for the oracle cross-crate
  sweep being half-done from stage 5a. Once finished, A landed in ~2
  hours. If a future workstream feels MUCH harder than estimated, look
  for half-done stage refactors blocking the obvious path.

- **Session 5 — diagnose before reverting.** A 0%-CPU hang at the
  user-bin compile was initially attributed to a panic + @GCMLZ unwind
  interaction. That was wrong. `sample <pid>` showed the real cause:
  std::sync::Mutex same-thread re-entrance at MUTABLE_STATE via
  `lang_symbol_name → call_notify_concrete_entry_point` from inside
  `generate_and_compile`. Fixed in ~30 minutes once the stack trace was
  in hand.

- **Session 12 — verify before assuming.** Phase 4's "wrapper layout
  intercept" turned out unnecessary because the wrapper's default ZST
  layout was already structurally safe. Confirmed empirically by running
  the suite with the wrapper's default layout in effect. Sometimes a
  planned phase collapses; trust the test corpus over the original
  prediction.

---

## 10. Closing notes

You're inheriting a **clean-checkpoint baseline** with every
architecturally interesting interop machine empirically fenced:

- Approach A fires per-Instance with concrete args (`case1b` exercises
  this directly).
- The rustc fork is 4 patches against nightly-2026-01-20, installed as
  `rustc-fork`: the `per_instance_mir` trio + the `extra_modules` hook
  for patch (c) inline codegen.
- Per-library stub rlibs with `__SKY_STUBS_MARKER` + adjacent
  `.sky-meta` sidecars work end-to-end.
- Multi-toylang-crate projects build (case6_basic + sharpened
  case4/5/6).
- The seven-case taxonomy is fully tested (7/7).
- Phase 4.5 Path B single-symbol architecture: Sky's bitcode emits
  every rustc-visible body under the rustc-mangled name. `#![no_builtins]`
  excludes stub rlibs from LTO's IR linker pool.
- `extra_modules_hook` contributes Sky's `ModuleCodegen<ModuleLlvm>`
  into rustc's optimize → ThinLTO → emission pipeline.
- **Cross-language ThinLTO inlining empirically verified.**
  `test_lto_smoke` builds with `lto = "thin"` + `opt-level = 3` and
  disassembles the binary; the user_bin's Rust `main` constant-folds
  Sky's `compute() = 10 + 20*2` down to `mov w8, #50` with no `bl`
  to Sky symbols. Any regression of Path B, `#![no_builtins]`, function
  attribute mirroring, or the patch (c) hook fires the assertion.
- Sky §9 export commitment is fenced; §4.4 byte-identical pass-through is
  fenced; generic/non-generic uniformity is fenced; Sky struct stub
  shape is wrapper-as-field per §10.6.
- 266/266 tests pass. 16/18 course-correct items done; #10 partial;
  only #13 (wrapper-mode retirement, bundles with Sky's toolchain
  shipping) remains.
- The @GCMLZ deadlock concern dissolved (Tier 3 #7 / #9 / #12 close-out).
  `MUTABLE_STATE` retained for `collect_generic_rust_deps`'s rayon
  worker-thread serialisation; not a trap-fence.

**There is no pressing toylang work left.** The bounded follow-ups
that could still land:

1. **File the rustc upstream PRs.** Two drafts exist:
   - `phase-e-rustc-pr-draft.md` (debuginfo clamp — Sky-side no longer
     needs it after Phase E Path 2 but benefits cranelift/miri/plugins).
   - The `extra_modules` hook isn't drafted as a PR; would benefit
     cranelift/gcc-rs/spirv backends. ~few hours to write up.
2. **Phase 4.4 benchmark.** Quantitative measurement of cross-language
   inlining perf delta (with/without `lto = "thin"`). Would validate
   "ThinLTO closes the cross-language gap" architecturally. ~½ day.

**The big shifts that aren't toylang work:**

- **Sky comptime spike** (§13 slab-based comptime). The highest-leverage
  Sky pre-1.0 derisking. 2–4 week vertical slice proving the slab-
  pointer-as-`usize` trick survives non-degenerate cases. Separate
  project; toylang can't validate it.
- **Sky async two-type split** (`SkyNotStarted_foo` /
  `SkyRunning_foo`, §14). Most distinctive Sky design choice; never
  exercised. Out of scope for toylang.
- **Toylang → Sky bootstrap planning.** A doc that maps which parts
  of erw forward-port to Sky and which were pure prototype scaffolding.

Read `course-correct.md` (status table at the top) to confirm the
scoreboard. Read the architecture doc if you'll touch Sky design.
Read this handoff §1, §8, §10 for current state. The Tier 3 plan in
§6 and the inline-codegen plan at
`/Users/verdagon/.claude/plans/parsed-singing-globe.md` are
**historical** — Sessions 13/15/16 executed them.

Good luck.

— previous engineer (Session 16 end)
