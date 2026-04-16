# Plan: Phase 6 — `.unwrap()` on `Result`/`Option` (Wrapper Functions + Partitioner Fix)

## Status

Previous attempt (2026-04-15) got the toylang-side wiring right but hit a linkage wall: the wrapper `__toylang_option_unwrap::<i32>` was correctly monomorphized by rustc's collector (confirmed via `-Zprint-mono-items=lazy`) but the CGU partitioner marked it `Linkage::Internal`, making it invisible to toylang's separately-linked `.o` file. Full post-mortem: `attempt-linkage-visibility-problem.md`.

Root cause: `mono_item_linkage_and_visibility` in `rustc_monomorphize/src/partitioning.rs` sees (a) `CrateType::Executable` ⇒ `local_crate_exports_generics() == false`, (b) wrapper's only user lives in the same CGU, so the item falls through to `Visibility::Hidden` + `can_be_internalized = true` and `internalize_symbols` flips it to Internal.

The existing toylang-side implementation (oracle redirect table, callbacks_impl dep injection, llvm_gen redirect, stub_gen wrapper emission) is architecturally correct and stays. The fix is a small rustc-fork patch to force `__lang_stubs` items external, later promoted to a facade callback.

---

## Context

Toylang can receive `Result<T, E>` and `Option<T>` from Rust functions but has no way to extract the inner value. `.unwrap()` is an inherent method on both types. `#[inline(always)]` functions have no external callable symbols — rustc only inlines them, never emitting global symbols.

**Solution**: Generate non-inline wrapper functions in `__lang_stubs` (`#[inline(never)] pub fn __toylang_option_unwrap<T>(o: Option<T>) -> T { o.unwrap() }`). Toylang redirects its method dispatch to the wrapper at dep-registration time. Rustc inlines `.unwrap()` into the wrapper's body, producing a linkable symbol with inlined performance.

**Scope**: Affects ~100+ inline stdlib functions (Option, Result, Vec methods) and `#[track_caller]` functions.

**Outcome**: Wrapper generation + rustc partitioner patch, 5 tests pass, Phase 7 unlocked.

---

## Why Wrapper Functions (Not Other Approaches)

Approaches investigated:

1. **Make ReifyShim WeakODR** (~50-100 lines rustc change)
   - ❌ Breaks on macOS (no COMDAT support)
   - ❌ Still doesn't solve `#[track_caller]`
   - ❌ Platform-specific

2. **Generate Wrapper Functions + partitioner patch** ✅ **SELECTED**
   - ✅ Works on macOS, Linux, Windows
   - ✅ ~5-line rustc patch (extends an existing fork pattern, not a new layering sin)
   - ✅ Solves both inline and `#[track_caller]` issues
   - ✅ Scales to every future stdlib wrapper for free

3. **Add Wrapper InstanceKind** (~500-800 lines rustc, ~150 lines toylang)
   - ✅ Most systematic
   - ❌ Overkill for Phase 6

4. **Per-instantiation `#[no_mangle]` shims** (pure vanilla Rust, no fork patch)
   - ❌ Needs `(wrapper, type_args)` pairs before `generate_stubs()` fires — structural driver change
   - ❌ `#[no_mangle]` collisions across workspace crates
   - ❌ Doesn't scale cleanly as wrapper count grows

5. **`#[linkage = "external"]` on the wrapper**
   - ❌ Requires `#![feature(linkage)]` at crate root — leaks nightly-feature opt-in into user code

**Chosen: Wrapper functions + partitioner patch (step 1), promoted to facade callback (step 2), unified with existing callbacks (step 3).**

---

## Investigation Summary

**Key discoveries**:
- `#[inline(always)]` functions intentionally produce no callable symbols.
- ReifyShim has internal linkage by design.
- Rustc's collector reliably picks up the wrapper via the `ReifyFnPointer` cast that `per_instance_mir` already emits for `rust_deps` entries (`rustc_monomorphize/src/collector.rs:709-717` — `CollectionMode::UsedItems`, not hint-level).
- Partitioner defaults generic items in executable crates to `Visibility::Hidden` + internalizable. Single-CGU co-location with the caller means `internalize_symbols` flips them to Internal.
- The `explicit_linkage` fast-path at `partitioning.rs:741-743` in `mono_item_linkage_and_visibility` returns BEFORE the generic/non-generic split and BEFORE `can_be_internalized` is ever set — same structural position where our check belongs.

Full technical analysis: `docs/historical/phase6-inline-functions-investigation.md` and `attempt-linkage-visibility-problem.md`.

---

## Prior-Attempt Post-Mortem (READ BEFORE IMPLEMENTING)

Two failed attempts so far. Their lessons survive verbatim; read both to avoid re-falling.

### Attempt 1 (`monomorphization-not-generated-case-orig.md`)

The wrapper redirect lived only in `llvm_gen::get_or_resolve_rust_method` at codegen time. `tcx.symbol_name(wrapper_instance)` is a read-only query that doesn't drive monomorphization. Wrapper never entered `rust_deps` → `per_instance_mir` never reified a ReifyFnPointer to it → mono collector never walked it.

**Rule**: the wrapper redirect happens at **dep-registration time** (where `RustMethodDep` is built, in `callbacks_impl.rs` / oracle), not at symbol-string-computation time. If the only line that changes is a `tcx.symbol_name` call site, you're in the wrong file.

### Attempt 2 (`attempt-linkage-visibility-problem.md`)

Got dep registration right. `-Zprint-mono-items=lazy` showed:
```
MONO_ITEM fn __lang_stubs::__toylang_option_unwrap::<i32> @@ test.3743c738498b5a4-cgu.3[Internal]
```
Mono happened. Partitioner internalized it. The `phase-6-plan.md` gotcha claiming "cross-CGU reference + `-C codegen-units=16` forces external linkage" was wrong — the partitioner co-located caller and wrapper in the same CGU, and the internalization path ran.

**Architectural lesson**: `ReifyFnPointer`-based dep registration is necessary but not sufficient. We also need to force external linkage in the partitioner. Toylang cannot do this from the outside; it requires rustc cooperation.

### Anti-patterns to reject in review

- ❌ Any `should_use_wrapper` / `compute_wrapper_name` helper inside `llvm_gen.rs`.
- ❌ Any `find_wrapper_fn_def_id` shortcut that bypasses `rust_deps`.
- ❌ `#[used]`, fn-pointer statics, or per-type monomorphic wrappers as a workaround.
- ❌ "It's linking now because I added `#[no_mangle]`" — no_mangle changes symbol names, not mono or linkage class.
- ❌ Branching on `is_generic` in the partitioner patch. Non-generic `__lang_stubs` items must get the same treatment. Project invariant: non-generic is the degenerate case of generic.

---

## Implementation Scope

### Toylang-side (KEEP — already landed in working tree from attempt 2, architecturally correct)
1. **toylangc/src/oracle.rs** — `WRAPPERS` table, `wrapper_fn_name`, `find_wrapper_fn_def_id`, `redirect_to_wrapper`.
2. **toylangc/src/toylang/callbacks_impl.rs** — redirect injected in inherent method branch of `collect_toylang_fn_deps_inner`; wrapper Instance lands in `rust_deps`.
3. **toylangc/src/llvm_gen.rs** — same redirect in `get_or_resolve_rust_method`; extern decl uses wrapper symbol.
4. **toylangc/src/stub_gen.rs** — emits `__toylang_option_unwrap<T>` and `__toylang_result_unwrap<T, E: Debug>`.

Both redirect sites call the same `oracle::redirect_to_wrapper` so `tcx.symbol_name` produces identical output on both paths.

### Rustc-fork patch (NEW — step 1, the blocker)
5. **rustc_monomorphize/src/partitioning.rs** — 3–5 line check after `explicit_linkage` fast-path (line ~744).

### Facade callback (NEW — step 2, cleanup)
6. **rustc-lang-facade/src/lib.rs** + **rustc_monomorphize/src/partitioning.rs** — replace inline check with `lang_visibility_override` callback.

### Consistency pass (NEW — step 3, debt)
7. Rename `toy_*` callbacks to `lang_*`; document interception-vs-override families.

### Tests
- `toylangc/tests/integration_tests.rs` — 5 new tests. Template: `test_vec_pop_returns_option` at ~line 3305. Toylang cannot construct `Option::Some` directly, so tests need a Rust shim imported via `use`, OR chain off `Vec::pop()`.

### Documentation
- `quest.md` — Phase 6 DONE.
- `docs/architecture/rust-interop-guide.md` — status banner (+5 tests), §10.6 wrapper solution + partitioner patch, interception-vs-override taxonomy.
- `docs/arcana/<new>-<arcanid>.md` — document the partitioner visibility override and why `__lang_stubs` items must be external.

---

## Three-Step Sequencing

**Step 1 — inline string check in `partitioning.rs` (blocker fix).**
Proves the linkage decision is the only thing blocking Phase 6. Tiny blast radius: if something else is also wrong, we find out before investing in API design. Check goes before the generic/non-generic split, uniform for both.

**Step 2 — replace with `lang_visibility_override` facade callback.**
Pure refactor of step 1, same runtime behavior, cleaner layering. The string `__lang_stubs` no longer appears in the fork; `is_from_lang_stubs` stays in the facade where it belongs.

**Step 3 — consistency pass across all five callbacks.**
Unify registration/naming (`lang_*`), document interception-vs-override families in the architecture guide so future hooks slot in deliberately. Audit whether non-generic accessor wrappers also need the new callback or are structurally immune (and annotate whichever).

---

## Existing functions and patterns to reuse

- `oracle::find_inherent_method` (oracle.rs:63) — finds `unwrap`
- `oracle::rust_method_return_type` (oracle.rs:222) — handles generic arg slicing
- `oracle::get_or_resolve_rust_method` (oracle.rs:299) — inherent path via `receiver_ty=None`
- `callbacks_impl::monomorphize_fn` — deep-walks dependencies
- `rustc-lang-facade::is_from_lang_stubs` (lib.rs:218) — `tcx.def_path_str(def_id).starts_with("__lang_stubs::")`
- Accessor wrapper pattern in `stub_gen.rs:54-115` — existing generic wrapper emission (`impl<T> StructName<T> { ... }`)
- `per_instance.rs:106-173` — reifies `rust_deps` entries as `ReifyFnPointer` casts inside synthesized MIR bodies
- `run_toylang_test` (integration_tests.rs:34) — test helper

---

## Step 1: Inline partitioner patch (BLOCKER FIX)

Use `/tmp/erw-phase6.txt` as the tee file for all cargo runs.

### File: `rustc_monomorphize/src/partitioning.rs`

In `mono_item_linkage_and_visibility` (around line 734), immediately after the `explicit_linkage` fast-path at line 741-743:

```rust
fn mono_item_linkage_and_visibility<'tcx>(
    tcx: TyCtxt<'tcx>,
    mono_item: &MonoItem<'tcx>,
    can_be_internalized: &mut bool,
    export_generics: bool,
) -> (Linkage, Visibility) {
    if let Some(explicit_linkage) = mono_item.explicit_linkage(tcx) {
        return (explicit_linkage, Visibility::Default);
    }

    // Phase 6 (toylang): items generated in a `__lang_stubs` module are
    // referenced from externally-linked consumer `.o` files that the CGU
    // partitioner cannot see in `usage_map`. Without this override, generic
    // `#[inline(never)]` items in executable crates are marked Hidden +
    // internalizable and the caller/callee CGU co-location causes
    // `internalize_symbols` to flip them to Internal. Applies uniformly to
    // generic and non-generic items — we refuse to branch on is_generic
    // (project invariant: non-generic is the degenerate case of generic).
    if let Some(def_id) = mono_item.def_id_for_lang_stubs_check() {
        if tcx.def_path_str(def_id).starts_with("__lang_stubs::") {
            *can_be_internalized = false;
            return (Linkage::External, Visibility::Default);
        }
    }

    // ... existing generic/non-generic logic
}
```

where `def_id_for_lang_stubs_check` returns `Some(def_id)` for `MonoItem::Fn(instance)` (using `instance.def_id()`), `MonoItem::Static(def_id)`, and `MonoItem::GlobalAsm(item_id)` (via `item_id.owner_id.def_id.to_def_id()`). Implement as a method on `MonoItem` or inline three-way `match`.

### Why the return values are correct

- `Visibility::Default != Visibility::Hidden`, so the internalization guard at `partitioning.rs:254` (`if visibility == Hidden && can_be_internalized { internalization_candidates.insert(...) }`) fails — the item never enters `internalization_candidates`.
- Setting `*can_be_internalized = false` is defense-in-depth but not strictly required given the above.
- `(External, Default)` is the same combination emitted for `make_some_i32` / `println_i32` in the diagnostic output from attempt 2. Valid on Mach-O and ELF.
- No other rustc pass can downgrade External→Internal for a root mono item. `partitioning.rs:280` (inlined items) doesn't apply — our wrapper is a root item. `internalize_symbols` at line 591 is gated by membership in `internalization_candidates` which we've excluded ourselves from. LLVM intrinsic linkage changes don't apply to user functions.

### Verification

```bash
cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_basic -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt
grep "MONO_ITEM.*__toylang_option_unwrap" /tmp/erw-phase6.txt
```

Expected: `[External]` on the wrapper line instead of `[Internal]`. Link succeeds. Test passes.

If linker still complains:
```bash
find .toylang-build/target/debug -name '*.o' -o -name '*.rlib' | \
  xargs -I {} sh -c 'nm -g {} 2>/dev/null | grep __toylang_'
```
- Absent mangled symbol → mono didn't happen → bug is in `rust_deps` registration, not the patch. Re-inspect attempt 2's `[phase6] redirect ...` / `[per_instance_mir dep] ...` eprintlns are still firing.
- Present but unresolved → name mismatch between rustc emission and llvm_gen's extern decl. Compare strings directly.

---

## Step 2: Facade callback

### File: `rustc-lang-facade/src/lib.rs`

Add to `CallbackVtable` (around line 128-160, after existing callbacks):

```rust
pub lang_visibility_override: for<'tcx> fn(
    &dyn Any,
    &mut dyn Any,
    TyCtxt<'tcx>,
    Instance<'tcx>,
) -> Option<(Linkage, Visibility)>,
```

Rationale for signature choices (from Agent 2 verification):
- `Instance<'tcx>` not `MonoItem`: the consumer only cares about `MonoItem::Fn(instance)` and would match-and-ignore the other two variants. Passing `Instance` directly avoids importing `rustc_monomorphize` types into the facade's public surface and matches how `per_instance_mir` / `symbol_name` already take `Instance`.
- `Option<(Linkage, Visibility)>`: matches the `per_instance_mir` convention — `None` means fallthrough, `Some(...)` means replace. Belongs to the **interception** family.
- `&dyn Any` + `&mut dyn Any`: same state-threading convention as existing callbacks (consumer state, monomorphization state).

### File: `rustc_monomorphize/src/partitioning.rs`

Replace the inline string check with:

```rust
if let Some(def_id) = mono_item.def_id_for_lang_stubs_check() {
    if let Some(instance) = /* extract instance from mono_item */ {
        if let Some((linkage, vis)) = rustc_lang_facade::call_lang_visibility_override(tcx, instance) {
            *can_be_internalized = false;
            return (linkage, vis);
        }
    }
}
```

### Toylang registration

In `toylangc/src/toylang/callbacks_impl.rs`, register a closure that returns `Some((External, Default))` when `is_from_lang_stubs(tcx, instance.def_id())` is true, else `None`.

### Verification

Re-run the same test. Behavior must be identical to step 1. The string `__lang_stubs` no longer appears anywhere in `/Users/verdagon/rust`.

---

## Step 3: Consistency pass

### Naming

Existing shims use two prefixes inconsistently:
- `lang_symbol_name` (lang_)
- `lang_per_instance_mir` (lang_)
- `toy_layout_of` (toy_)
- `toy_mir_shims` (toy_)

Rename `toy_*` → `lang_*`. Single prefix for all 5 callbacks.

### Document the two families

In `docs/architecture/rust-interop-guide.md`, add taxonomy section:

- **Interception family**: `lang_per_instance_mir`, `lang_symbol_name`, `lang_mir_shims`, `lang_visibility_override`. Consumer item detection via `is_from_lang_stubs` or similar predicate; returns `None`/default to fall through; replaces rustc's answer for consumer-owned items only.
- **Override family**: `lang_layout_of`. Intercepts for consumer types only, falls through via saved default provider (`DEFAULT_LAYOUT_OF` OnceLock).

The two families split on whether the query itself is new to the fork (no default exists → `None` fallthrough) or replaces a rustc-native query (default exists → saved provider fallthrough). Future hooks slot deliberately into the right family.

### Accessor wrapper audit

Non-generic accessor wrappers (existing pattern in `stub_gen.rs:54-115`) currently work without the visibility override. Verify why:
- If it's because `per_instance_mir`'s `lang_symbol_name` override redirects the caller to a toylang-generated symbol, accessors never hit the partitioner's visibility path. Document this structural immunity.
- If it's because they happen to be referenced cross-CGU and the partitioner leaves them External by default, the invariant is fragile — add them to the visibility override for safety.

---

## Gotchas (verified against existing code)

1. **`#[inline(never)]` on the wrapper is mandatory.** Without it, rustc can still inline the wrapper itself — producing no callable symbol and putting us back at the original problem. Load-bearing, not a performance knob. (Enforced by rustc's codegen backend as LLVM `noinline`.)

2. **External linkage comes from the partitioner patch, NOT from `#[inline(never)]` + `-C codegen-units=16`.** The previous plan's gotcha 1a claimed cross-CGU reference forces external linkage; attempt 2 disproved this empirically. The partitioner co-locates `__toylang_main` (caller) and the wrapper in the same CGU, so the cross-CGU-reference path never triggers. The patch is the only mechanism that guarantees External.

3. **ReifyFnPointer is the authoritative trigger for collection.** `rustc_monomorphize/src/collector.rs:709-717` promotes ReifyFnPointer targets into `used_items` (authoritative) regardless of the `Unreachable` terminator. That's why `per_instance.rs`'s pattern reliably forces codegen of the referenced function. Attempt 1 never got here because it skipped `rust_deps` registration entirely.

4. **DefId swap must happen before every downstream step.** Redirect in `get_or_resolve_rust_method` AND in `collect_toylang_fn_deps_inner` (both, identical helper call). Symbol mangling, `per_instance_mir` dep registration, and extern declaration generation must all see the wrapper DefId so they stay consistent.

5. **No special `compute_wrapper_symbol` mangling.** Call `tcx.symbol_name(wrapper_instance)` — rustc's standard mangling handles the wrapper like any other generic function. The wrapper is ordinary Rust code in `__lang_stubs`.

6. **Type args pass through unchanged.** The wrapper's generics mirror the original's, so `Instance { def_id: wrapper_def_id, args: <original args> }` is well-formed. No arg massaging.

7. **The wrapper's `where` bounds must mirror the original method's.** `Result::unwrap` is `pub fn unwrap(self) -> T where E: Debug`. Our wrapper `pub fn __toylang_result_unwrap<T, E: Debug>(r: Result<T, E>) -> T { r.unwrap() }` repeats that bound verbatim. `Option::unwrap` has no bound on `T`, so the wrapper doesn't either. Any future wrapper for a more exotic method (e.g., `Iterator::collect` with `FromIterator`) must copy bounds exactly or Instance resolution fails.

8. **Per_instance_mir exclusion is guaranteed by two orthogonal checks.** Wrappers live in `__lang_stubs` (passes `is_from_lang_stubs`) but are neither `is_consumer_fn` (not in `registry.functions`) nor `is_consumer_accessor` (no associated_item — they're free functions). Per_instance_mir returns None → rustc compiles normally. If a future refactor introduces a `__toylang_*` name pattern check anywhere, wrappers must be excluded from it.

9. **Visibility override must NOT branch on `is_generic`.** Non-generic `__lang_stubs` items (future accessor wrappers, static tables, whatever) need the same external-linkage treatment. Project invariant: non-generic is the degenerate case of generic.

---

## Testing Strategy

### Write 5 integration tests first (Phase D0)

Tests don't yet exist in `integration_tests.rs`. Template: `test_vec_pop_returns_option` at ~line 3305. Since toylang cannot construct `Option::Some` directly, each test needs either a Rust shim (`pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }`) imported via `use`, or chains off an existing Option-returning Rust method like `Vec::pop()`.

- `test_option_unwrap_basic` — build Option<i32> via shim, unwrap, verify i32.
- `test_result_unwrap_basic` — build Result<i32, SomeDebugErr> via shim, unwrap, verify i32.
- `test_option_unwrap_result_discarded` — call unwrap as ExprStmt (no use of return).
- `test_unwrap_arithmetic_chain` — `opt.unwrap() + 1`.
- `test_vec_pop_unwrap` — `v.pop().unwrap()` with nested MethodCall resolution.

Use `test_option_unwrap_basic` as the first real checkpoint. If it fails, drop to `nm` (see step 1 verification) — that distinguishes "mono never happened" (Phase C bug) from "symbol name disagrees" (helper drift) from "linkage still Internal" (patch bug).

If Option works but Result doesn't, suspect Debug bound propagation (gotcha 7) or arg count handling for 2-param generics.

### Run order

```bash
# Step 1 verification:
cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_basic -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_result_unwrap_basic -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_result_discarded -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_unwrap_arithmetic_chain -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt
cargo +rustc-fork test -p toylangc --test integration_tests test_vec_pop_unwrap -- --nocapture 2>&1 | tee /tmp/erw-phase6.txt

# Full suite after step 1:
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-phase6-full.txt
grep "test result:" /tmp/erw-phase6-full.txt
```

Expected: 60 unit + 110 integration + 4 standalone + 5 new = **179 tests**, 0 failures.

Step 2 must produce identical test results. Step 3 is rename-only and must also produce identical test results.

---

## Verification Checklist

### Step 1 (blocker fix)
- [ ] Write 5 tests in `integration_tests.rs`
- [ ] Patch `rustc_monomorphize/src/partitioning.rs` with inline `__lang_stubs` check
- [ ] `test_option_unwrap_basic` passes
- [ ] `test_result_unwrap_basic` passes
- [ ] `test_option_unwrap_result_discarded` passes
- [ ] `test_unwrap_arithmetic_chain` passes
- [ ] `test_vec_pop_unwrap` passes
- [ ] `-Zprint-mono-items=lazy` shows `[External]` on wrapper
- [ ] Full suite: 179 tests, 0 failures

### Step 2 (facade callback)
- [ ] Add `lang_visibility_override` to `CallbackVtable` with `Instance<'tcx>` signature
- [ ] Register toylang-side closure that checks `is_from_lang_stubs`
- [ ] Replace inline string check in `partitioning.rs` with callback dispatch
- [ ] String `__lang_stubs` absent from `/Users/verdagon/rust` tree
- [ ] Full suite: 179 tests, 0 failures (identical to step 1)

### Step 3 (consistency)
- [ ] Rename `toy_layout_of` → `lang_layout_of`, `toy_mir_shims` → `lang_mir_shims`
- [ ] Document interception-vs-override families in `docs/architecture/rust-interop-guide.md`
- [ ] Audit accessor wrappers; document structural immunity or add to override
- [ ] Full suite: 179 tests, 0 failures (identical to step 2)

### Documentation
- [ ] `quest.md` Phase 6 updated to DONE
- [ ] `docs/architecture/rust-interop-guide.md` updated (status, wrapper solution, partitioner patch, taxonomy)
- [ ] `docs/arcana/<name>-<arcanid>.md` for the partitioner visibility override
- [ ] Remove `eprintln!` diagnostics from working tree; keep `TOYLANG_DUMP_STUBS` / `TOYLANG_PRINT_MONO` env gates as dev tools

---

## Post-Implementation

After step 3 completes and all verification passes:
- Ask user: "sprongle anchor archie"
