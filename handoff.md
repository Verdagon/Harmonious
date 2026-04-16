# Handoff: Phase 6 Steps 2 and 3, plus Tech Debt #6

> **SUPERSEDED — ALL TASKS IN THIS DOC ARE DONE.** Kept for reference so
> future readers can see how prior handoffs framed similar work.
> Current handoff: see `handoff-phase7-uuid.md` in the project root.
>
> - Task A (Phase 6 step 2 — `visibility_override` callback): DONE.
>   Implementation differed from this doc's design — see `docs/architecture/rust-interop-guide.md` §10.6.4.
> - Task B (Phase 6 step 3 — naming + taxonomy): DONE via the two-family
>   trait split rather than the rename-only approach proposed below.
>   `toy_layout_of` → `lang_layout_of` and `toy_mir_shims` →
>   `lang_mir_shims` renames also landed. Accessor audit documented in
>   `rust-interop-guide.md` §10.6.4.
> - Task C (Tech debt #6 — FnCall CoercedParam dispatch): DONE.
>   `is_scalar_pair_type` deleted; FnCall now uses `push_arg_for_rust_call`.

**Audience:** A junior developer joining the project for the first time.
**Estimated effort:** 1-3 days for steps 2+3, plus a half day for debt #6.
**Prerequisites:** Comfortable with Rust. No prior rustc-internals experience required — but you'll need to read some.

---

## What this doc is

You are picking up Phase 6 of the **erw** project. Phase 6 step 1 is done (just shipped). This doc walks you through:

- **Task A (Phase 6 step 2)** — replace a hardcoded string check in our forked rustc with a callback that lets toylang answer the question instead. Pure refactor. ~half day if smooth.
- **Task B (Phase 6 step 3)** — naming consistency pass on the facade callbacks (rename `toy_*` to `lang_*`), document the two callback "families" in the architecture guide, audit one related code path. ~half day.
- **Task C (Tech debt #6)** — finish migrating the FnCall path to use `CoercedParam` for ABI dispatch (similar fix to one we just did for MethodCall). ~half day.

Read this whole document before touching any code. It contains the context that took the prior implementers (and me) several false starts to discover.

---

## Quick orientation

### What is erw?

A two-crate workspace that lets you embed a custom programming language ("toylang") into rustc's compilation pipeline. The high-level idea: pretend toylang code is Rust, get rustc to do all the linking/typechecking/ABI work for free, generate the actual machine code yourself via LLVM (Inkwell), then have rustc include your `.o` file in the final binary.

```
erw/
├── rustc-lang-facade/   # The reusable library. Knows nothing about toylang.
├── toylangc/            # The example consumer. Defines toylang the language.
└── docs/                # Architecture, arcana, historical
```

Nothing about Phase 6 step 2 or step 3 changes the runtime behavior — they're refactors. Tests must pass identically before and after each change.

### Where the action is

Almost all your time will be spent in:

```
rustc-lang-facade/src/lib.rs              — callback registration, mutable state, trampolines
rustc-lang-facade/src/queries/            — the 4 query providers (mod.rs, layout.rs, drop_glue.rs, per_instance.rs, symbol_name.rs)
toylangc/src/toylang/callbacks_impl.rs    — toylang's implementation of the LangCallbacks trait
toylangc/src/llvm_gen.rs                  — toylang's LLVM IR codegen (FnCall stuff lives here)
/Users/verdagon/rust/compiler/rustc_monomorphize/src/partitioning.rs
                                          — the rustc fork patch you'll be removing/replacing
```

### Building and testing

The project uses a forked rustc toolchain called `rustc-fork`. It's installed at `/Users/verdagon/rust/build/host/stage2` and symlinked as `~/.rustup/toolchains/rustc-fork`.

```bash
# Build toylangc:
cargo +rustc-fork build -p toylangc

# Run all tests (this is your "did I break anything" check):
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-handoff.txt
grep "test result:" /tmp/erw-handoff.txt
# Expected: 60 unit + 116 integration + 4 standalone = 180 tests, 0 failures
```

**Build/test convention** (in CLAUDE.md): always pipe cargo output to a fixed file in `/tmp` (e.g. `/tmp/erw-handoff.txt`) using `tee` as the LAST command. Don't chain with `| grep` or `| tail` — that defeats the purpose of being able to re-grep without re-running the build. Run the build and the inspection as separate commands.

If you change anything in the rustc fork at `/Users/verdagon/rust/compiler/`:

```bash
cd /Users/verdagon/rust && python3 x.py build --stage 2 compiler/rustc 2>&1 | tee /tmp/erw-rustc-build.txt
# Takes ~3-5 minutes. Build is incremental — most rebuilds are fast.
```

You don't need to manually relink anything after rebuilding rustc — the toolchain symlink updates automatically because it points at the build output dir.

---

## Background you need before starting

### The four (soon-to-be-five) facade callbacks

The facade installs four query providers into rustc that toylang can hook into:

| Query provider | File | What it does |
|---|---|---|
| `layout_of` | `rustc-lang-facade/src/queries/layout.rs` | Returns memory layout for a type. Toylang intercepts for its own struct types and returns a 0-field opaque layout. |
| `mir_shims` | `rustc-lang-facade/src/queries/drop_glue.rs` | Returns drop glue MIR. Toylang intercepts for its own types and returns a no-op. |
| `per_instance_mir` | `rustc-lang-facade/src/queries/per_instance.rs` | Returns the MIR body for a specific monomorphization. Toylang intercepts for its functions and synthesizes a body that mentions all Rust deps as ReifyFnPointer casts (so rustc's mono collector codegens them). |
| `symbol_name` | `rustc-lang-facade/src/queries/symbol_name.rs` | Returns the linker symbol for an instance. Toylang intercepts for its functions and returns its own mangled name (`__toylang_impl_foo` instead of rustc's `_ZN4test3foo...`). |

These callbacks are stored in a `CallbackVtable` (file `rustc-lang-facade/src/lib.rs` line 128) — a struct of higher-ranked function pointers. The vtable is wrapped in `FacadeConfig` and stored in a `OnceLock<FacadeConfig>` static called `CONFIG` (line 200). Set once at startup, read freely thereafter. **No locking required for reads.** This is important for avoiding deadlock — see the @GCMLZ arcana for the full explanation.

Toylang's implementation of these callbacks lives in `toylangc/src/toylang/callbacks_impl.rs`. Toylang implements the `LangCallbacks` trait (defined at `rustc-lang-facade/src/lib.rs:67`). The facade auto-generates per-callback "trampoline" functions that downcast the `&dyn Any` callback storage to the concrete `C: LangCallbacks` type (lines 320-370), then stores those trampolines as plain fn pointers in the vtable.

### The two callback "families"

Read this section carefully — it's the conceptual model you need for Task A and Task B.

The four callbacks split into two families based on how they relate to rustc's existing query system:

**Family 1: Interception** — replaces rustc's answer for consumer-owned items only, falls through to rustc's default for everything else.
- `lang_per_instance_mir`: returns `Option<&Body>`. `None` means "not a consumer item, use rustc's default."
- `lang_symbol_name`, `toy_mir_shims`, `toy_layout_of`: these can't return `None` (the query API requires `T`), so they explicitly call the saved `default_*()` provider for non-consumer items.

**Family 2: Override** — answers for consumer items that rustc still owns, providing data rustc couldn't otherwise know.
- `toy_layout_of` is currently the only one — toylang owns the struct definitions but rustc allocates them, so toylang has to tell rustc the layout.
- The new `lang_visibility_override` you're adding in Task A is also in this family conceptually: the partitioner needs to know that `__lang_stubs` items have external linkage requirements that don't fit its normal heuristics.

Both families share the same registration mechanism (the OnceLock + vtable + trampoline pattern). The difference is semantic, not structural. We document the distinction so future hooks slot deliberately into the right family.

### How rustc's mono collector and partitioner relate to Phase 6

Skim `docs/architecture/rust-interop-guide.md` §10.6 for the full story. Short version:

When toylang compiles, it discovers Rust functions it needs to call (e.g., `Vec::push`, `__toylang_option_unwrap`). It pushes each `(def_id, generic_args)` into a `rust_deps` Vec inside `callbacks_impl::collect_toylang_fn_deps_inner`. Our `per_instance_mir` provider builds a synthetic MIR body that mentions each dep as a `ReifyFnPointer`. Rustc's mono collector walks ReifyFnPointer references and codegen's whatever it finds. Result: all of toylang's Rust callees get emitted by rustc.

Partitioner step is what Task A is about. After mono finishes, rustc groups items into Codegen Units (CGUs), then decides each item's linkage and visibility. The partitioner has a default rule: generic `#[inline(never)]` items in executable crates default to `Visibility::Hidden` + can-be-internalized = true. If their only user lives in the same CGU, `internalize_symbols` flips them to `Linkage::Internal`. Internal-linkage symbols are invisible to externally-linked `.o` files (i.e., toylang's). So toylang's wrapper function — `__toylang_option_unwrap<i32>` — gets internalized and the linker fails.

Phase 6 step 1 fixed this by patching `rustc_monomorphize/src/partitioning.rs::mono_item_linkage_and_visibility`: any item in `__lang_stubs` gets `(External, Default)` returned early, before the internalization logic runs. The patch hardcodes a string check for `__lang_stubs` in the DefPath. **Task A removes that hardcode.**

### Critical arcana to read first

Before you touch anything, read these (each is ~40-80 lines):

1. `docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md` — why `tcx.symbol_name` and `Instance::expect_resolve` are read-only; the `rust_deps` registration is what drives codegen. **You won't be modifying anything that depends on this for Tasks A/B/C, but understanding it will save you from confusion if you read adjacent code.**
2. `docs/arcana/DefPathStrIsForDiagnosticsOnly-DPSFDOZ.md` — `tcx.def_path_str` ICEs in normal-compilation contexts; the partitioner check uses `tcx.def_path(...).data` walking instead. **You'll be working with this code.**
3. `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` — why config is in OnceLock (no lock) and only mutable state is in a Mutex. **Relevant: any new callback you add must read from CONFIG without locking, exactly like the existing four.**

---

# Task A: Add `lang_visibility_override` callback (Phase 6 step 2)

## Goal

Replace the hardcoded `__lang_stubs` string check in `rustc_monomorphize/src/partitioning.rs` with a callback the facade exposes and toylang implements. After this change, the rustc fork has zero knowledge of `__lang_stubs` (or anything toylang-specific). The string `__lang_stubs` should appear nowhere in `/Users/verdagon/rust/`.

## Why

The fork is supposed to be consumer-agnostic. Currently four of the five hooks already follow this pattern (the consumer answers, the fork just asks). The partitioner check we added in step 1 is the odd one out — it has consumer-specific knowledge baked in. Step 2 brings it in line.

This is a **pure refactor**. Test results before and after must be identical: 60 unit + 116 integration + 4 standalone = 180 tests, 0 failures.

## Where the existing code is

The hardcoded check you're replacing is at:
```
/Users/verdagon/rust/compiler/rustc_monomorphize/src/partitioning.rs
```

Look for the comment `// Phase 6 (toylang/erw):` inside the function `mono_item_linkage_and_visibility`. It's a ~20-line block right after the `explicit_linkage` fast-path. Read it now to know what you're replacing.

## Step-by-step plan

### A.1 Add the callback to the trait

In `rustc-lang-facade/src/lib.rs`, find the `LangCallbacks` trait at line 67. After `monomorphize_fn` (around line 94), add:

```rust
/// Optionally override an item's linkage and visibility during CGU partitioning.
/// Return None to defer to rustc's normal logic. Return Some((Linkage, Visibility))
/// to force a specific assignment (and prevent internalization).
///
/// Called from `rustc_monomorphize::partitioning::mono_item_linkage_and_visibility`
/// AFTER the explicit_linkage fast-path and BEFORE the generic/non-generic split.
///
/// Per @GCMLZ, this provider runs during normal compilation (not inside
/// generate_and_compile). It must NOT lock MUTABLE_STATE. It only reads CONFIG,
/// which is lock-free.
fn visibility_override<'tcx>(
    &self,
    state: &mut dyn Any,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)>;
```

**Why `Instance` and not `MonoItem`?** A `MonoItem` is `Fn(Instance) | Static(DefId) | GlobalAsm(ItemId)`. The consumer only cares about `Fn(instance)` and would `match`-and-ignore the other two. Passing `Instance` directly avoids importing `rustc_monomorphize` types into the facade's public surface. The other two MonoItem variants will still work — see step A.6.

**Why `Option<(Linkage, Visibility)>`?** Matches the `per_instance_mir` convention: `None` = use default logic, `Some(...)` = override. This keeps the partitioner's own logic intact for non-consumer items.

### A.2 Provide a default impl on the trait

To avoid forcing every existing test/consumer to add an empty `visibility_override`, add a default impl that returns None:

```rust
fn visibility_override<'tcx>(
    &self,
    _state: &mut dyn Any,
    _tcx: TyCtxt<'tcx>,
    _instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)> {
    None
}
```

Move the original (without default) up to be the actual trait method, and put the default right below the others as the `default fn`. Look at how `monomorphize_type` and similar are written — copy that style.

### A.3 Add to CallbackVtable

In `rustc-lang-facade/src/lib.rs` line 128, the `CallbackVtable` struct. Add a new field at the end:

```rust
visibility_override: for<'tcx> fn(
    &(dyn Any + Send + Sync),
    &mut (dyn Any + Send + Sync),
    TyCtxt<'tcx>,
    ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)>,
```

### A.4 Add the trampoline

Trampolines live around line 320-370 in `lib.rs`. Add one for the new callback:

```rust
fn trampoline_visibility_override<'tcx, C: LangCallbacks + 'static>(
    data: &(dyn Any + Send + Sync),
    state: &mut (dyn Any + Send + Sync),
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)> {
    data.downcast_ref::<C>().unwrap().visibility_override(state, tcx, instance)
}
```

### A.5 Wire the trampoline into install_callbacks

`install_callbacks` is at line 373. Add `visibility_override: trampoline_visibility_override::<C>,` to the vtable struct literal alongside the other trampolines.

### A.6 Add the call_* helper

Look at `call_monomorphize_fn` at line 255 for the pattern. Add this helper right below `call_after_rust_analysis`:

```rust
/// Call the consumer's visibility_override.
///
/// IMPORTANT: this runs during the partitioner pass, NOT inside
/// generate_and_compile. We must NOT lock MUTABLE_STATE here, because the
/// partitioner runs in a context where doing so could deadlock with other
/// codegen-time queries. Read CONFIG only (per @GCMLZ).
///
/// But we still need to thread `&mut state` for trampoline-signature uniformity
/// with the other callbacks. We pass a transient empty Any since the consumer's
/// visibility_override should not need real state — it's a pure predicate over
/// (tcx, instance). If a future consumer needs state here, revisit this.
pub(crate) fn call_visibility_override<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)> {
    let c = CONFIG.get()?;  // None during early init — fall through to default
    let func = c.vtable.visibility_override;
    let callbacks_ptr: *const (dyn Any + Send + Sync) = &*c.callbacks;
    // Use a stack-allocated empty state. We don't lock MUTABLE_STATE.
    let mut empty_state: () = ();
    let state_ptr: *mut (dyn Any + Send + Sync) = &mut empty_state;
    (func)(unsafe { &*callbacks_ptr }, unsafe { &mut *state_ptr }, tcx, instance)
}
```

⚠️ **Re-verify the no-state assumption** before committing. If toylang's `visibility_override` needs to call any helper that touches `state`, you'll deadlock the partitioner. The simplest visibility check (`is_from_lang_stubs(tcx, instance.def_id())`) doesn't need state — it's a pure function of `tcx` and the def_id — so this should be fine. But sanity-check by grep'ing whether `is_from_lang_stubs` calls anything that touches MUTABLE_STATE.

### A.7 Toylang side: implement the callback

In `toylangc/src/toylang/callbacks_impl.rs`, find the `impl LangCallbacks for ToylangCallbacks` block. Add:

```rust
fn visibility_override<'tcx>(
    &self,
    _state: &mut dyn Any,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)> {
    use rustc_codegen_ssa::back::write::Linkage;
    use rustc_middle::mir::mono::Visibility;
    if rustc_lang_facade::is_from_lang_stubs(tcx, instance.def_id()) {
        Some((Linkage::External, Visibility::Default))
    } else {
        None
    }
}
```

Wait — read the @DPSFDOZ arcana. `is_from_lang_stubs` uses `def_path_str` which can ICE outside `generate_and_compile`. The partitioner runs OUTSIDE `generate_and_compile`. So you can't call `is_from_lang_stubs` directly from this hook.

Instead, inline the safe check (mirrors what's already in the partitioner patch):

```rust
fn visibility_override<'tcx>(
    &self,
    _state: &mut dyn Any,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Option<(rustc_codegen_ssa::back::write::Linkage, rustc_middle::mir::mono::Visibility)> {
    use rustc_codegen_ssa::back::write::Linkage;
    use rustc_middle::mir::mono::Visibility;
    use rustc_hir::definitions::DefPathData;
    // Per @DPSFDOZ, can't use is_from_lang_stubs here (it calls def_path_str,
    // which ICEs during normal compilation). Walk DefPath data manually.
    let in_lang_stubs = tcx.def_path(instance.def_id()).data.iter().any(|d| {
        matches!(d.data, DefPathData::TypeNs(name) if name.as_str() == "__lang_stubs")
    });
    if in_lang_stubs {
        Some((Linkage::External, Visibility::Default))
    } else {
        None
    }
}
```

**Stretch goal**: refactor `is_from_lang_stubs` itself to use the safe `def_path` walk so it works in both contexts, then this method can call it. Optional — only do this if you're feeling bold and have time.

### A.8 Update the partitioner patch in the rustc fork

In `/Users/verdagon/rust/compiler/rustc_monomorphize/src/partitioning.rs`, find the block we added in step 1 (search for `Phase 6 (toylang/erw)`). Replace it with:

```rust
if let Some(explicit_linkage) = mono_item.explicit_linkage(tcx) {
    return (explicit_linkage, Visibility::Default);
}
// Defer to consumer's visibility_override callback if registered.
// The callback receives the Instance (for MonoItem::Fn) or we synthesize
// one for Static/GlobalAsm so it can answer using only def_id queries.
if let MonoItem::Fn(instance) = mono_item {
    if let Some((linkage, vis)) = rustc_lang_facade::call_visibility_override(tcx, *instance) {
        *can_be_internalized = false;
        return (linkage, vis);
    }
}
// MonoItem::Static and MonoItem::GlobalAsm: skip for now. If/when toylang
// emits these into __lang_stubs, extend the callback signature to accept
// MonoItem instead of Instance.
let vis = mono_item_visibility(...);  // ← unchanged
```

You'll need to add `extern crate rustc_lang_facade;` at the top of `partitioning.rs` if it isn't already there. Check `Cargo.toml` for `rustc_monomorphize` — you may need to add `rustc-lang-facade` as a dependency. ⚠️ Watch out for circular dependencies; the facade depends on rustc internals. If this gets complicated, consider an indirection through a trait-object-style query hook stored in a `tcx.sess`-level location instead.

**Important**: the partitioner runs BEFORE the consumer's `install_callbacks` may have completed in some compilation modes. `call_visibility_override` already uses `CONFIG.get()?` (returns `None` if config isn't set yet) so it falls through to the default logic safely. Don't change that to `expect`.

### A.9 Rebuild rustc and test

```bash
cd /Users/verdagon/rust && python3 x.py build --stage 2 compiler/rustc 2>&1 | tee /tmp/erw-handoff-rustc.txt
# After it finishes (3-5 min), build toylangc:
cd /Users/verdagon/erw
cargo +rustc-fork build -p toylangc 2>&1 | tee /tmp/erw-handoff.txt
# Run the full suite:
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-handoff.txt
grep "test result:" /tmp/erw-handoff.txt
# Expected: still 180 tests, 0 failures, 0 ignored.
```

If `test_option_unwrap_basic` (or any of the 5 unwrap tests) fails with a linker error about `undefined symbol`, the callback isn't being called or it's returning the wrong thing. Debug with:

```bash
# Add to your toylangc binary (in main.rs or wherever it's permissible):
# args.push("-Zprint-mono-items=lazy".to_string());
# Then re-run a single test and look for the wrapper symbol in the output.
# It should say [External], not [Internal].
```

### A.10 Verify the string `__lang_stubs` no longer appears in the rustc fork

```bash
cd /Users/verdagon/rust && grep -rn "__lang_stubs" compiler/
# Expected: no matches. (If you see matches in toolchain dependencies under
# compiler/rustc_*/Cargo.toml, that's fine — those reference the facade, which
# does have the string.)
```

### A.11 Update docs

- `docs/architecture/rust-interop-guide.md` §10.6.4 — replace the inline patch code block with a one-paragraph note that the callback is now `lang_visibility_override`, and link to the facade's trait definition.
- `docs/architecture/known-tech-debt.md` item #7 — mark as resolved (or move to the resolved table at the bottom).
- `quest.md` Phase 6 — mark step 2 done.

## Common pitfalls

1. **Trying to call `is_from_lang_stubs` from the partitioner.** ICEs because of @DPSFDOZ. Use the inline `def_path` walk.
2. **Locking MUTABLE_STATE in the new callback's body.** Will deadlock during partitioning. Don't do it. The callback should be a pure function of (tcx, instance).
3. **Forgetting to set `*can_be_internalized = false` after the callback returns Some.** The callback's `Visibility::Default` already prevents internalization (because the gate is `Hidden && can_be_internalized`), so this is technically defensive — but keep it for documentation value, matching how the inline patch did it.
4. **Forgetting the `CONFIG.get()?` early-return.** During very early rustc init, config isn't set yet. If you `expect`, you'll panic.

---

# Task B: Naming consistency pass (Phase 6 step 3)

## Goal

Three small things:

1. Rename `toy_*` callbacks to `lang_*` so all 5 callbacks share the `lang_` prefix.
2. Document the interception-vs-override callback families in the architecture guide.
3. Audit accessor wrappers to confirm they're either covered by `lang_visibility_override` or structurally immune.

## Why

Two `toy_*` and three `lang_*` is just historical inconsistency. Standardizing makes future hooks slot deliberately into the right naming pattern. Also, the family taxonomy (interception vs override) is real architectural information that should be findable, not just discovered by reading all 5 callbacks.

## Step-by-step plan

### B.1 Rename `toy_layout_of` to `lang_layout_of`

```bash
grep -rn "toy_layout_of" /Users/verdagon/erw/
```

Should find ~3-4 hits: the function definition in `queries/layout.rs`, the registration in `queries/mod.rs`, possibly a doc reference. Rename in all places. Build + test.

### B.2 Rename `toy_mir_shims` to `lang_mir_shims`

Same drill:

```bash
grep -rn "toy_mir_shims" /Users/verdagon/erw/
```

Rename in all places. Build + test.

### B.3 Document the two families

In `docs/architecture/rust-interop-guide.md`, find Part 2 ("The Four Query Providers") around line 102. After the four subsections (2.1 layout_of, 2.2 per_instance_mir, 2.3 symbol_name, 2.4 mir_shims), add:

```markdown
### 2.5 lang_visibility_override

Returns `Option<(Linkage, Visibility)>` for an Instance during CGU partitioning.
None = use rustc's default logic. Some(...) = force this assignment.

Used by toylang to mark `__lang_stubs` items as External + Default visibility,
preventing the partitioner from internalizing them (which would hide them from
the externally-linked toylang `.o` file). See §10.6.4 for the full Phase 6
linkage story.

### 2.6 The two callback families

The five callbacks split into two semantic families:

**Interception family** — replaces rustc's answer for consumer-owned items only,
falls through to rustc's default for everything else.

- `lang_per_instance_mir`: returns `Option<&Body>` (None = use default).
- `lang_symbol_name`, `lang_mir_shims`, `lang_layout_of`: returns `T` directly,
  but explicitly calls `default_*()` for non-consumer items.

**Override family** — provides data for consumer items that rustc still owns,
overriding rustc's normal heuristic.

- `lang_visibility_override`: tells the partitioner not to internalize items
  rustc generated for consumer wrappers.
- `lang_layout_of` is also borderline override — toylang owns the struct
  definitions but rustc allocates them.

The distinction matters when adding new hooks: pick the family that matches
the data flow direction. Interception means "rustc would ask for X about a
consumer item; consumer answers." Override means "rustc has its own answer
for a consumer-related item but it's wrong; consumer corrects it."
```

### B.4 Audit accessor wrappers

Toylang generates "accessor wrappers" — non-generic functions in `__lang_stubs` that look up struct fields. They've been working without `lang_visibility_override` since Phase 1. Your audit answers: are they working because `lang_symbol_name` redirects callers to a toylang-emitted symbol BEFORE the partitioner ever sees them? Or because they happen to be cross-CGU referenced and external by accident?

To investigate:

```bash
# Find an existing test that uses accessors:
grep -n "test.*accessor\|test.*field" /Users/verdagon/erw/toylangc/tests/integration_tests.rs | head -3

# Pick one, run it, dump LLVM IR and check whether the accessor symbol is
# referenced as toylang-mangled (__toylang_accessor_*) or as the rustc-mangled
# name. If toylang-mangled, accessors never hit the partitioner's visibility
# path because lang_symbol_name redirected the caller.
```

Then write up findings (~10 lines) at the end of §10.6.4 in `rust-interop-guide.md`. If accessors ARE structurally immune, document that. If they're at risk, add them to `lang_visibility_override` (one-line check: `is_consumer_accessor_pub(tcx, def_id)` from `per_instance.rs`).

### B.5 Update docs

- `docs/architecture/known-tech-debt.md` — mark debt #7 fully resolved (after both Tasks A and B).
- `quest.md` Phase 6 — mark step 3 done.

---

# Task C: Migrate FnCall path to CoercedParam dispatch (Tech Debt #6)

## Goal

The MethodCall and StaticCall paths in `llvm_gen.rs` already dispatch per-arg on `CoercedParam` (Direct/Pair/Indirect/Ignore) using the cached `info.coerced_params`. The FnCall path doesn't — it routes pair detection on toylang's `is_scalar_pair_type(&a.ty)` heuristic instead.

This works today because `&[u8]` is the only ScalarPair toylang can construct, and toylang's `is_scalar_pair_type` agrees with rustc's CoercedParam::Pair for it. But that's two parallel oracles for the same question, where the toylang one is strictly less informed. Migrate FnCall to consult CoercedParam, then delete `is_scalar_pair_type` entirely.

## Why this matters

If you ever add another ScalarPair type (e.g. `&str`, which is `(ptr, len)` like `&[u8]`), or rustc's ABI rules change so a type that used to be Direct becomes Pair, the FnCall path will silently corrupt arg values. Same class of bug we just fixed for MethodCall (Vec::push). Better to fix it before it bites.

## Step-by-step plan

### C.1 Read the existing FnCall code

`toylangc/src/llvm_gen.rs:1100-1216`. The relevant section is around line 1192-1215:

```rust
for a in args {
    let val = lower_typed_expr(ctx, a).into_value(&ctx.builder);
    if is_scalar_pair_type(&a.ty) {
        let struct_val = val.into_struct_value();
        let first = ctx.builder.build_extract_value(struct_val, 0, "pair_first").unwrap();
        let second = ctx.builder.build_extract_value(struct_val, 1, "pair_second").unwrap();
        call_args.push(first.into());
        call_args.push(second.into());
    } else {
        call_args.push(val.into());
    }
}
```

Note: this DOES handle Direct correctly (passes `val.into()` — the value, not a pointer). The bug fixed in Task C is narrower than the MethodCall bug — the FnCall path doesn't pass pointers when it shouldn't. The migration is purely about **consulting the right source of truth** for pair detection.

### C.2 Refactor to per-CoercedParam dispatch

Replace the loop above with:

```rust
for (i, a) in args.iter().enumerate() {
    push_arg_for_rust_call(ctx, a, &coerced_params[i], &mut call_args);
}
```

Note: FnCall's `coerced_params` has NO self offset (free functions have no receiver), so it's `coerced_params[i]`, not `coerced_params[1 + i]`.

`push_arg_for_rust_call` is the helper we wrote for MethodCall, defined around line 491. It already handles all four CoercedParam variants. You're just adopting it.

### C.3 Update the assertion (already exists at line 1212)

The existing `assert_eq!(call_args.len(), param_types.len(), ...)` is fine — keep it.

### C.4 Delete `is_scalar_pair_type`

```bash
grep -rn "is_scalar_pair_type" /Users/verdagon/erw/
```

After Task C.2, the only remaining caller should be the FnCall code you just changed. Delete the function definition. If `cargo build` complains about other callers, you have remaining migration work to do.

### C.5 Test

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-handoff.txt
grep "test result:" /tmp/erw-handoff.txt
# Expected: 180 tests, 0 failures.
```

The existing byte-string test (`test_byte_string_passed_to_rust_fn` or similar — grep for "byte_string") exercises the ScalarPair path. If it passes, you've done it right.

### C.6 Update tech debt

`docs/architecture/known-tech-debt.md` debt #6 — move to the resolved table.

---

## Common pitfalls across all three tasks

1. **Forgetting to rebuild rustc after changing the rustc fork.** A change in `compiler/rustc_*/` only takes effect after `python3 x.py build --stage 2 compiler/rustc`. If your test results don't match the change you made, this is the first thing to check.

2. **Confusing the two `lang_stubs` checks.** There are now (or will be, after Task A) THREE places that check for `__lang_stubs`:
   - `rustc-lang-facade/src/lib.rs::is_from_lang_stubs` (uses `def_path_str`, only safe inside generate_and_compile)
   - `toylangc/src/toylang/callbacks_impl.rs::visibility_override` (uses `def_path` walk, safe everywhere)
   - The Cargo dependency you might add to `rustc_monomorphize` (if you do the Task A.8 indirection that way)
   Keep them straight. Each has different constraints.

3. **Adding the new callback breaks existing tests because they don't impl it.** The default impl on the trait (Task A.2) prevents this. If you skip the default impl, you'll have to add the empty version to every consumer that implements `LangCallbacks`. Toylang has only one implementor today, so it's not catastrophic, but it's still ergonomically worse.

4. **Tests pass locally but fail in CI.** The build/test convention from CLAUDE.md applies — pipe to `/tmp/erw-handoff.txt` and inspect with separate commands. Don't combine the redirect with `| grep` in the same line.

5. **Touching unrelated code.** This is a refactor pass. Resist the urge to "clean up" things you notice while you're in the file. Each task should be a small, reviewable diff. If you find unrelated bugs, file a known-tech-debt entry instead of fixing them inline.

---

## Where to look when stuck

- **"My partitioner change isn't being called"**: Add `eprintln!` inside the partitioner block before `mono_item_visibility`. Rebuild rustc. Run a test. If you don't see your eprintln, the file isn't being recompiled (do a full rebuild, not incremental). If you do see it but the wrapper still gets internalized, your callback returned `None` when it should have returned `Some`.

- **"Linker error about undefined symbol"**: Same as the prior attempts hit. Run `cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_basic 2>&1 | tee /tmp/erw-handoff.txt` and check the binary's symbols:
  ```bash
  # Find the test binary the test compiled (varies — check the test framework's tempdir)
  # Then:
  nm /path/to/test_bin 2>/dev/null | grep __toylang_option_unwrap
  ```
  If absent: mono didn't happen — check `rust_deps` registration in callbacks_impl.rs.
  If present but unresolved at link time: linkage is wrong — check your `visibility_override` is being called and returning Some.

- **"`'trimmed_def_paths' called` ICE during compilation"**: You called `def_path_str` from a non-diagnostic context. Read @DPSFDOZ. Switch to `def_path(def_id).data` walking.

- **"A test that has nothing to do with my change is failing"**: First, run the test without your change to confirm it's actually a regression. If it was failing before, it's not your problem. If it's a real regression, go through your changes systematically — the most common cause is the new callback throwing or returning the wrong thing for an unrelated mono item.

- **"The build hangs / takes forever"**: Rustc rebuilds from scratch can take 20+ minutes the first time after a clean. Incremental rebuilds should be 1-3 minutes. If you see "Compiling rustc_*" lines, it's normal. If you see no progress for >5 min, something's wrong (try `cd /Users/verdagon/rust && cargo clean -p rustc_monomorphize` then rebuild).

- **General reading order**: When confused, read in this order:
  1. `docs/architecture/rust-interop-guide.md` (the big architecture doc)
  2. The relevant arcana (`docs/arcana/*.md`)
  3. The function-level comments in `lib.rs` and the file you're editing
  4. The previous attempts' writeups in `docs/historical/phase6-attempt*.md` (for context on what NOT to do)

---

## Definition of done

Task A:
- [ ] `LangCallbacks` trait has `visibility_override` with default impl returning None
- [ ] `CallbackVtable` has the new field, populated by trampoline
- [ ] `call_visibility_override` helper exists and reads CONFIG only (no MUTABLE_STATE lock)
- [ ] Toylang's `ToylangCallbacks` implements `visibility_override` using safe `def_path` walking
- [ ] `partitioning.rs` calls `call_visibility_override` instead of inline string match
- [ ] `grep -rn "__lang_stubs" /Users/verdagon/rust/compiler/` returns no matches in rustc source
- [ ] Full test suite passes: 180 tests, 0 failures, 0 ignored
- [ ] `quest.md`, `rust-interop-guide.md`, `known-tech-debt.md` updated

Task B:
- [ ] `toy_layout_of` renamed to `lang_layout_of` everywhere
- [ ] `toy_mir_shims` renamed to `lang_mir_shims` everywhere
- [ ] Architecture guide §2 has new subsections 2.5 (lang_visibility_override) and 2.6 (the two families)
- [ ] Accessor wrappers audited — finding documented in §10.6.4
- [ ] Full test suite still passes: 180 tests, 0 failures
- [ ] `quest.md` and `known-tech-debt.md` updated

Task C:
- [ ] FnCall loop uses `push_arg_for_rust_call` with `&coerced_params[i]`
- [ ] `is_scalar_pair_type` function deleted
- [ ] Full test suite still passes: 180 tests, 0 failures
- [ ] `known-tech-debt.md` debt #6 marked resolved

---

## Final notes

- All three tasks are pure refactors. Test results before and after each task must be identical (180 tests, 0 failures, 0 ignored). If a test starts failing, your refactor changed behavior — back out and retry.
- Each task should be a separate commit. Don't combine them. Reviewable diffs save everyone time.
- After each task, append your findings to this handoff doc under a "## Status as of <date>" section, so the next person sees current state. Or just delete the doc when all three tasks are done — `git log` is the durable record.
- When in doubt, ask. The previous attempts wasted time pattern-matching on superficially-similar code instead of asking for context. A 5-minute clarifying conversation beats a half-day rabbit hole.

Good luck. The architecture is solid; you're filling in the last pieces of consistency.
