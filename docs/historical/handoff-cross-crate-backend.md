# Handoff: Cross-Crate Backend Cleanup

**Task owner:** a junior engineer joining the erw project.
**Estimated effort:** 3–5 days for a junior, including ramp time.
**Risk level:** low — the filter being removed is empirically dead code under the current architecture; the value is architectural (removing a latent assumption) rather than behavioral.
**Branch:** work on `main` directly, commit when tests are green. No feature branch needed — changes are small and standalone.

---

## 1. Context

### Why this is being done

Toylang's LLVM backend (`toylangc/src/llvm_gen.rs`) bakes in a single-crate-compile assumption: in `generate_with_tcx`, the MonoItems walk filters with `def_id.as_local()` before emitting code for each consumer item. Today this filter is effectively dead — `__lang_stubs.rs` is injected into the user's crate via `FileLoader`, so every consumer DefId is already local to the user bin's compile, and the filter always passes.

But the filter encodes an architectural assumption that would bite any future cross-crate integration model. A 2026-04 POC on branch `poc/separate-crate-stubs` (see `/Users/verdagon/erw-poc-separate-crate-stubs/findings.md` §4.2 risk #9) proved that moving `__lang_stubs` to its own rlib would cause the user bin's compile to skip every cross-crate consumer DefId — producing an empty `.o` and a link failure. The filter is a time bomb.

We're defusing it now as a standalone cleanup. This is independent of any zero-fork or plugin-backend work, but is a clean precondition for both. It follows the same pattern as the `CLAUDE.md` compiler law "non-generic is the degenerate case of generic" — we treat **single-crate as the degenerate case of cross-crate** and write the general path.

### What prompted it

The POC above surfaced three separate-crate blockers (risks #1, #8, #9); #9 is specifically this `as_local()` filter. The POC's `findings.md` §4.3 noted that fixing risk #9 alone is one of the three remediation paths for retrofitting toylang to separate-crate. We're doing it now because:

- It's small and mechanical — good standalone-win profile.
- It doesn't commit to the separate-crate architecture; it just stops obstructing it.
- It consolidates a piece of infrastructure (a cross-crate-safe `is_from_lang_stubs` predicate) that the partitioner visibility hook has already paid for inline and that future work will want.

### Intended outcome

A single PR (or short chain) that:

- Removes the `as_local()` filter at `toylangc/src/llvm_gen.rs:1897` (line number as of the current HEAD).
- Drops the now-unused `LocalDefId` parameter from `ToylangCallbacks::notify_concrete_entry_point_inner`.
- Updates the accessor handling path in `llvm_gen.rs` to pass `def_id` instead of `local_def_id`.
- Consolidates the safe-to-call-from-any-phase "is this DefId in `__lang_stubs`?" check into a new facade helper `is_from_lang_stubs_safe`, and replaces the inline `tcx.def_path(...).data` walk inside `ToylangCallbacks::visibility_override` with a call to this helper.
- All 211 tests still pass, zero new warnings.

Explicitly **not** changing: the `def_id.as_local().is_some()` guard at `rustc-lang-facade/src/queries/symbol_name.rs:58`. That guard encodes a legitimate architectural concern (only trigger the consumer's `notify_concrete_entry_point` for DefIds local to the current compile) that's cross-crate-aware, not a single-crate assumption. Touching it is a separate-crate/plugin concern.

---

## 2. Required reading before you code (1–2 hours)

Read in this order; stop when you feel oriented.

1. `/Users/verdagon/erw/CLAUDE.md` — especially the "non-generic is the degenerate case of generic" compiler law. That's the shape of the reasoning behind this refactor.
2. `/Users/verdagon/erw/HANDOFF-TL.md` — overall project orientation.
3. `/Users/verdagon/erw/docs/architecture/rust-interop-guide.md` §2 (four query providers) + §10.6.4 (visibility override + accessor-immunity discussion). You don't need the whole file.
4. `/Users/verdagon/erw/docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md` — critical. This is why the new helper can't use `def_path_str`. Small file, read all of it.
5. `/Users/verdagon/erw-poc-separate-crate-stubs/findings.md` §4.2 — the POC writeup that surfaced risk #9 (the filter this refactor removes). Skim for context; don't memorize.

---

## 3. Current surface (verified file:line references)

### The filter being removed

`toylangc/src/llvm_gen.rs:1867–1975` — `generate_with_tcx<'tcx>`. Inside it, at roughly line 1889 onwards, a loop over rustc's `collect_and_partition_mono_items` output:

```rust
let (_, cgus) = tcx.collect_and_partition_mono_items(());
for cgu in cgus.iter() {
    for (&mono_item, _) in cgu.items() {
        let rustc_middle::mir::mono::MonoItem::Fn(instance) = mono_item else { continue };
        let def_id = instance.def_id();
        if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            continue;                                                      // ← line ~1895
        }
        let Some(local_def_id) = def_id.as_local() else { continue };     // ← line 1897 — THE FILTER
        // ... accessor path, then regular-function path ...
    }
}
```

The filter at line 1897 is what we're removing. The `is_from_lang_stubs` check one line above stays — that's the real consumer-item filter.

### Where `local_def_id` flows after the filter

Only one site: the accessor path at `llvm_gen.rs:1899–1920`. Specifically line 1912:

```rust
let extern_symbol = callbacks.notify_concrete_entry_point_inner(
    state, &callback_name, tcx, local_def_id, instance,
);
```

The `local_def_id` parameter of `notify_concrete_entry_point_inner` is **already unused** (underscore-prefixed at the function definition — see `toylangc/src/toylang/callbacks_impl.rs` around line 124, the `_def_id: LocalDefId` parameter). So switching the caller to pass `def_id: DefId` (after changing the callee's type) is a one-site semantic no-op.

### The regular-function path (after the accessor block)

Lines ~1921–1943 process consumer entry-point functions that aren't accessors. This path doesn't use `local_def_id` at all (uses `def_id`, `instance`, and names pulled from `tcx.item_name(def_id)`). No changes needed to this block.

### The facade-side pattern to consolidate

`toylangc/src/toylang/callbacks_impl.rs` around lines 206–208, inside `visibility_override<'tcx>`:

```rust
let in_lang_stubs = tcx.def_path(instance.def_id()).data.iter().any(|d| {
    matches!(d.data, DefPathData::TypeNs(name) if name.as_str() == "__lang_stubs")
});
```

This is the safe-everywhere version of the "is it a consumer item?" check. It walks `DefPathData` structurally instead of going through `def_path_str`, so it's safe outside `generate_and_compile` (per @DPSFDOZ). We're extracting this into a facade helper.

### The existing `is_from_lang_stubs` — keeps its name, keeps its body

`rustc-lang-facade/src/lib.rs:306`:

```rust
pub fn is_from_lang_stubs(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let path = tcx.def_path_str(def_id);
    path.starts_with("__lang_stubs::")
}
```

**Do not delete this.** It has five callers inside facade query providers (`per_instance.rs:30`, `symbol_name.rs:25`, `layout.rs`, `drop_glue.rs:39`, and `llvm_gen.rs:1894` itself). All of them run inside rustc query provider contexts, which only fire during `generate_and_compile` — the diagnostic-safe phase per @DPSFDOZ. Converting them to the safe variant is out of scope; the string check is marginally faster than the DefPath walk, and the safety constraint isn't being violated today.

The new helper `is_from_lang_stubs_safe` is a SEPARATE function. It exists for callers that can't use the diagnostic-only version (the partitioner, any future pre-`generate_and_compile` hooks). Today that's just `visibility_override`'s inline walk, which this refactor collapses into the helper.

---

## 4. Proposed design

Four changes, ordered from smallest to largest:

### 4.1 New facade helper: `is_from_lang_stubs_safe`

Add to `rustc-lang-facade/src/lib.rs` next to the existing `is_from_lang_stubs`:

```rust
/// Cross-crate-safe variant of `is_from_lang_stubs`. Unlike the existing
/// helper (which uses `def_path_str` and is therefore @DPSFDOZ-gated to
/// diagnostic contexts), this version walks `DefPathData` structurally
/// and is safe to call from any phase — including the partitioner,
/// pre-`generate_and_compile` hooks, and any future cross-crate paths.
///
/// Slightly more expensive than `is_from_lang_stubs` (an iterator walk
/// vs a string check), but negligibly so — both are dominated by the
/// `tcx.def_path` query underneath.
///
/// Use this in preference to `is_from_lang_stubs` when the call site
/// might run outside `generate_and_compile`, or when you want the
/// compile-time guarantee that @DPSFDOZ can't bite.
pub fn is_from_lang_stubs_safe(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    use rustc_hir::definitions::DefPathData;
    tcx.def_path(def_id).data.iter().any(|d| {
        matches!(d.data, DefPathData::TypeNs(name) if name.as_str() == "__lang_stubs")
    })
}
```

Export from the facade (same `pub` visibility as `is_from_lang_stubs`).

### 4.2 Refactor `visibility_override` to use the new helper

In `toylangc/src/toylang/callbacks_impl.rs` around lines 200–212, replace the inline DefPath walk with:

```rust
let in_lang_stubs = rustc_lang_facade::is_from_lang_stubs_safe(tcx, instance.def_id());
```

Remove the now-unused `DefPathData` import if it was only used for this walk.

### 4.3 Remove the `as_local()` filter at `llvm_gen.rs:1897`

Delete the line:

```rust
let Some(local_def_id) = def_id.as_local() else { continue };
```

The accessor path at line 1912 currently uses `local_def_id`. Change it to pass `def_id`:

```rust
let extern_symbol = callbacks.notify_concrete_entry_point_inner(
    state, &callback_name, tcx, def_id, instance,
);
```

This requires step 4.4.

### 4.4 Change `notify_concrete_entry_point_inner`'s `_def_id` parameter from `LocalDefId` to `DefId`

In `toylangc/src/toylang/callbacks_impl.rs` at the `notify_concrete_entry_point_inner` function definition (around line 124 — the exact line may have shifted since the callback-split refactor; search for `fn notify_concrete_entry_point_inner`):

```rust
// Before:
pub fn notify_concrete_entry_point_inner<'tcx>(
    &self,
    state: &mut ToylangState,
    name: &str,
    tcx: TyCtxt<'tcx>,
    _def_id: LocalDefId,    // ← change this
    instance: ty::Instance<'tcx>,
) -> String { ... }

// After:
pub fn notify_concrete_entry_point_inner<'tcx>(
    &self,
    state: &mut ToylangState,
    name: &str,
    tcx: TyCtxt<'tcx>,
    _def_id: DefId,         // ← to this
    instance: ty::Instance<'tcx>,
) -> String { ... }
```

Since the parameter is underscore-prefixed (already unused), this is purely a type change at the function signature. No call-site arithmetic inside the function body changes.

**However** — there's one more call site to fix. Grep:

```
rg notify_concrete_entry_point_inner
```

You should find two calls:
- `llvm_gen.rs:1912` — fixed in step 4.3 by passing `def_id` instead of `local_def_id`.
- The trait impl in `callbacks_impl.rs` itself (the `notify_concrete_entry_point` trait method delegating to `_inner`). That path receives `instance: Instance<'tcx>` and currently does `instance.def_id().as_local().expect(...)` to convert. **After this refactor**, change it to just `instance.def_id()` — no conversion, no expect, no panic risk for cross-crate Instances. The exact line should be around `callbacks_impl.rs:379` (the line cited in the investigation report).

### 4.5 Drop the now-unreferenced `LocalDefId` import

After the above, `llvm_gen.rs` may no longer need its `LocalDefId` import (search for `use rustc_hir::def_id::LocalDefId` or similar). Remove if unused. Same for `callbacks_impl.rs` — check after steps 4.2 and 4.4 whether `LocalDefId` or `DefPathData` imports are still reached.

---

## 5. Implementation steps

Land as separate commits if you want history clarity, or as one commit if you prefer a single coherent unit. All steps should leave the codebase building and tests passing.

### Step 1 — Add `is_from_lang_stubs_safe` to the facade (pure addition)

Write the new function per §4.1. Nothing uses it yet. Run:

```bash
cargo +rustc-fork check -p rustc-lang-facade 2>&1 | tee /tmp/cross-crate.txt
tail -20 /tmp/cross-crate.txt
```

Expect zero errors, zero warnings. If the `DefPathData` import isn't already present in `lib.rs`, add it inside the function via `use` or at module scope — either works.

### Step 2 — Migrate `visibility_override` to the new helper

Per §4.2. Also run `cargo +rustc-fork check -p toylangc`. Tests should still pass:

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/cross-crate.txt
grep "test result:" /tmp/cross-crate.txt
```

Expect 211/211 pass. Especially watch `test_option_unwrap_basic` and related Phase 6 tests — they exercise `visibility_override` indirectly via `__lang_stubs` wrappers.

### Step 3 — Change `notify_concrete_entry_point_inner`'s signature from `LocalDefId` → `DefId`

Per §4.4. This breaks the two call sites simultaneously. Fix both:
- `llvm_gen.rs:1912` — change `local_def_id` to `def_id` (this step is interleaved with step 4 below, but the compile will only succeed if both callers are updated together).
- The trait impl in `callbacks_impl.rs:~379` — change `instance.def_id().as_local().expect(...)` to `instance.def_id()`.

### Step 4 — Remove the `as_local()` filter in `llvm_gen.rs`

Per §4.3. Delete the filter line; update the accessor path's pass-through.

### Step 5 — Clean up unused imports

Per §4.5. Run `cargo +rustc-fork check -p toylangc` and fix any `unused_imports` warnings.

### Step 6 — Full test run + grep verification

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/cross-crate.txt
grep "test result:" /tmp/cross-crate.txt
cargo +rustc-fork check -p toylangc 2>&1 | tee /tmp/cross-crate.txt
grep -E "warning:|error:" /tmp/cross-crate.txt
```

Expect 211 passing, 0 failed, 0 ignored. Zero warnings.

Required greps:

```bash
rg "as_local\(\)" toylangc/src/llvm_gen.rs    # expect zero hits
rg "local_def_id" toylangc/src/llvm_gen.rs    # expect zero hits in generate_with_tcx
rg "DefPathData" toylangc/src/toylang/callbacks_impl.rs    # may still have one match (see §6.2)
```

### Step 7 — Documentation updates

Small — this refactor is mostly invisible:

- `docs/architecture/rust-interop-guide.md` §4.1 ("Instance discovery") — the passage about "MonoItems walk... entry-point functions have a rustc `Instance`" is still accurate. Add a sentence after it noting that the walk uses `is_from_lang_stubs` as the sole consumer-item filter; DefIds may be local or cross-crate.
- `docs/architecture/rust-interop-guide.md` §10.6.4 — there's a paragraph near the end of the visibility-override discussion that explains the inline DefPath walk in `visibility_override`. Update to say "uses the shared `is_from_lang_stubs_safe` helper" and cite the new function by name.
- `docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md` — add a sentence at the end noting that `is_from_lang_stubs_safe` is the canonical safe alternative for code paths outside `generate_and_compile`, so future code doesn't re-derive the inline walk.

### Step 8 — Commit

One commit is fine (the changes are tightly related). Suggested message:

```
Remove single-crate-compile assumption from LLVM backend's MonoItems walk.

The `def_id.as_local()` filter at llvm_gen.rs:1897 was a dead proxy under
the current FileLoader-based integration (all consumer DefIds are local
to the user bin's compile anyway, so the filter always succeeded). Under
a hypothetical separate-crate integration — POC #2 risk #9 — the filter
would silently skip every cross-crate consumer DefId and produce an
empty .o. Remove it; the is_from_lang_stubs check one line above is the
actual consumer-item filter.

Downstream consumer of the LocalDefId was notify_concrete_entry_point_inner,
which had an unused _def_id parameter. Change the parameter type to DefId
and drop the instance.def_id().as_local().expect(...) conversion in the
trait impl — Instance.def_id() is safe regardless of crate.

Consolidate the "is this DefId in __lang_stubs?" check for cross-crate-safe
contexts (partitioner, visibility_override) into a new is_from_lang_stubs_safe
facade helper using tcx.def_path(...).data structural walk. Existing
is_from_lang_stubs kept for diagnostic-gated callers (query providers
running inside generate_and_compile) where the string check is fine per
@DPSFDOZ.

No behavioral change under current single-crate architecture. 211/211
tests pass, zero warnings. Preparation for eventual cross-crate work
(separate-crate stubs, plugin backend) documented in
future-architecture-investigations.md.
```

---

## 6. Critical subtleties

### 6.1 `symbol_name.rs:58` stays

There's a similar-looking `def_id.as_local().is_some()` check at `rustc-lang-facade/src/queries/symbol_name.rs:58`. **DO NOT touch it.** It's not a single-crate-compile proxy — it's a cross-crate-aware safety guard that says "only trigger the consumer's notify callback if the DefId is local to the current compile." Under separate-crate, that guard prevents the user bin from triggering notify for stub-rlib DefIds (which belong to the stub rlib's compile). Removing or rewriting it is a separate-crate-specific concern tied to the plugin migration; it's out of scope here.

If you see this check during your grep and feel the urge to "clean it up" for consistency with the `llvm_gen.rs` removal, resist the urge. Leave a comment in the PR description acknowledging you saw it and decided not to touch it. The TL will confirm this was the right call.

### 6.2 `DefPathData` import in `callbacks_impl.rs`

After §4.2, `callbacks_impl.rs` no longer contains an inline walk using `DefPathData`. You might be tempted to remove the `use rustc_hir::definitions::DefPathData;` import. **Grep first.** If `DefPathData` is used anywhere else in the file (it may be — other paths walk DefPath for symbol mangling), leave the import. If not, remove it.

### 6.3 `is_from_lang_stubs` still has four non-`llvm_gen` callers

Your refactor changes how `visibility_override` handles the check. It does **not** change the other callers of `is_from_lang_stubs` (in `queries/per_instance.rs`, `queries/symbol_name.rs`, `queries/layout.rs`, `queries/drop_glue.rs`). Leaving them on the string-based helper is intentional — all four run inside query provider contexts that only fire during `generate_and_compile`, so `def_path_str` is safe per @DPSFDOZ. Converting them to the safe variant is pointless churn.

If a future refactor finds one of those callers moving to a phase outside `generate_and_compile`, migrate that specific site — not a bulk rename.

### 6.4 Don't accidentally change query-provider keying

`per_instance_mir` remains `LocalDefId`-keyed in the facade. **Do not** try to change its signature "for consistency" with the other refactors. The query-level LocalDefId requirement there is a rustc-imposed constraint (the query provider must work on local items — rustc expects its own MIR cache to hold cross-crate items). This refactor is strictly about the backend's MonoItems walk, not about query-level keying.

### 6.5 Verify the accessor path under both type-parameter scenarios

`test_diamond_call_pattern` and similar tests exercise accessor methods on generic consumer types (`Pair<A, B>`, `Wrapper<T>`). Under the refactor, the accessor path at `llvm_gen.rs:1899–1920` now passes `def_id` (not `local_def_id`) to `notify_concrete_entry_point_inner`. Verify these tests still pass. If they don't, something about the `_def_id` parameter was load-bearing after all — unlikely given it's underscore-prefixed, but possible if someone added a use without updating the prefix. Grep the function body carefully.

### 6.6 No behavioral change under current architecture

This is the most important thing to internalize: **nothing observable should change** after this refactor. The filter was dead code; removing it removes the deadness without exposing new behavior. If your test suite shows any diff (symbols emitted differently, test output changes, etc.), something is wrong — stop and investigate, don't paper over it.

The refactor's value is architectural, not behavioral. It's a clean precondition for future work. Don't try to "prove" the refactor works by making it do something observable.

---

## 7. Verification

### Build & test

Per `CLAUDE.md`'s build-redirect convention:

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/cross-crate.txt
grep "test result:" /tmp/cross-crate.txt
```

Expected:
- `test result: ok. 67 passed; 0 failed` (unit tests — `--bin toylangc`)
- `test result: ok. 129 passed; 0 failed` (integration tests — `--test integration_tests`)
- `test result: ok. 15 passed; 0 failed` (standalone tests — `--test standalone_tests`)
- **Total: 211 passing, 0 failed, 0 ignored.** Matches the current baseline.

Zero warnings:

```bash
cargo +rustc-fork check -p toylangc 2>&1 | tee /tmp/cross-crate.txt
grep "warning:" /tmp/cross-crate.txt
```

Should return zero lines.

### Specific tests worth watching

```bash
cargo +rustc-fork test -p toylangc --test integration_tests test_diamond_call_pattern 2>&1 | tee /tmp/cross-crate.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_basic 2>&1 | tee /tmp/cross-crate.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_trait_static_call 2>&1 | tee /tmp/cross-crate.txt
```

- `test_diamond_call_pattern` — exercises the accessor path you edited.
- `test_option_unwrap_basic` — exercises `visibility_override` via `__lang_stubs` wrappers.
- `test_trait_static_call` — exercises the MonoItems walk with trait-method instances.

Also run all Phase 7 standalone tests:

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests 2>&1 | tee /tmp/cross-crate.txt
grep "test result:" /tmp/cross-crate.txt
```

All 15 should pass.

### Observable-behavior sanity check

Pick any Phase 7 standalone test (e.g., `uuid_test`) and run it twice — once before your changes, once after:

```bash
cd toylangc/tests/standalone/uuid_test
rm -rf .toylang-build
cargo +rustc-fork run --bin toylangc --manifest-path /Users/verdagon/erw/Cargo.toml -- build
# Run the produced binary:
.toylang-build/target/debug/uuid_test
# Should print: "uuid ok"
```

Check the `.ll` files produced in `.toylang-build/target/debug/` (LLVM IR dumps) before and after. They should be byte-identical modulo timestamps and rustc version strings — the refactor doesn't change what gets codegenned.

### Required greps

Before opening the PR:

```bash
rg "as_local" toylangc/src/llvm_gen.rs        # expect zero hits
rg "local_def_id" toylangc/src/llvm_gen.rs    # expect zero hits in generate_with_tcx
rg "is_from_lang_stubs_safe" rustc-lang-facade/src/    # expect at least one hit (the new definition)
rg "is_from_lang_stubs_safe" toylangc/src/    # expect at least one hit (visibility_override caller)
rg "DefPathData" toylangc/src/toylang/callbacks_impl.rs   # may or may not still have hits; investigate
```

---

## 8. Out of scope / follow-ups

**Not in this task** (flag but don't do):

- `rustc-lang-facade/src/queries/symbol_name.rs:58`'s `as_local()` guard. See §6.1.
- Migrate the four other `is_from_lang_stubs` callers to the safe variant. See §6.3.
- Change `per_instance_mir`'s LocalDefId keying. See §6.4.
- Any actual separate-crate integration work (new rlib structure, cargo orchestration). That's a larger future task (the eventual CodegenBackend plugin migration per `future-architecture-investigations.md`).
- Performance optimization of the DefPath walk vs string check. The difference is imperceptible.

**Follow-up ticket to file** after landing:

"Evaluate whether `generate_with_tcx`'s accessor path and regular-function path can be deduplicated" — after this refactor both paths flow more symmetrically (both pass `def_id`), and there may be shared extraction opportunities. Small cleanup, not urgent, in its own PR.

---

## 9. If you get stuck

Come find me (the TL). Specifically ask for help if:

- Any test fails after §4.3 and you can't immediately see why. The refactor is supposed to be a no-op behaviorally; any failure means something subtle is off.
- You find a fifth `is_from_lang_stubs` caller that the grep-audit should have caught. Means my investigation missed something.
- The `DefPathData` walk in `is_from_lang_stubs_safe` fails compilation with an error about `definitions` module visibility. The import path in rustc's internals can shift across versions; try `use rustc_hir::def::DefPathData` or grep the rustc sysroot for the correct path.
- `test_option_unwrap_basic` fails after §4.2. The `visibility_override` change is the only place where the semantics of the check actually matter (every other call site is belt-and-suspenders for the partitioner). A failure here would mean the helper's walk is looking for the wrong `DefPathData` variant or depth.

Don't be shy. The refactor reads long but the actual code change is ~15–30 lines across 3 files. Most of this doc is context and guardrails; the mechanical work is smaller than the callback split was.
