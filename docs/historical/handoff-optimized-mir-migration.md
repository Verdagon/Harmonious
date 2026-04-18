# Handoff: Migrate `per_instance_mir` → `optimized_mir` override (retire fork patches 1/2/4)

**Task owner:** a junior engineer joining the erw project. **This stage is larger than stages 1–2 — ~1–2 weeks, involves rustc fork surgery and a toolchain rebuild.** Read §1 and §9 carefully before committing to the work.
**Branch:** work on `main` directly. Commit when tests are green.
**Risk level:** medium-high — rustc-internals work, toolchain rebuild, partial fork reshape. Mitigated by (a) a staged migration with a long verification checkpoint (§5), (b) POC #1 having already verified the rustc-side mechanism works end-to-end.

---

## 1. Context

### Why this is being done

Today's facade keeps toylang working via a 5-patch fork of rustc (`~/rust` branch `per-instance-mir`). Three of those patches (1, 2, 4) exist only to define and plumb a custom `per_instance_mir` query. Rustc already has a sanctioned extension point — `rustc_interface::Config::override_queries` on the existing `optimized_mir` query — that produces the same behavior the fork currently gets via the custom query. POC #1 on branch `poc/optimized-mir-override` (see `/Users/verdagon/erw-poc-optimized-mir/findings.md`) verified the mechanism works: rustc's monomorphization collector substitutes Param placeholders in an override-returned body per caller and queues the same Rust dependencies the custom query produces today.

Retiring patches 1/2/4 does NOT eliminate the fork. Patch 3 (the codegen skip) and patch 5 (the partitioner visibility hook) both stay for now. Stage 4 of the long-term roadmap (a `CodegenBackend` plugin, documented in `docs/reasoning/rustc-fork-design-space.md` §4.2) eliminates both remaining patches. This stage is preparation — after it lands, we have a cleaner, smaller fork with exactly two patches, both of which are symmetric "facade installs a hook" patterns.

Concretely:

**Before this stage (5 patches):**
1. `rustc_middle/src/queries.rs` — define `per_instance_mir` query.
2. `rustc_monomorphize/src/collector.rs` — collector check for `per_instance_mir` before `instance_mir`.
3. `rustc_codegen_ssa/src/mono_item.rs` — codegen skip when `per_instance_mir` returns `Some`.
4. `rustc_mir_transform/src/shim.rs` — default provider returning `None`.
5. `rustc_monomorphize/src/partitioning.rs` — `VISIBILITY_OVERRIDE_HOOK` static + call site.

**After this stage (2 patches):**
- Patch 3 RESHAPED: `rustc_codegen_ssa/src/mono_item.rs` — codegen skip when a new `CODEGEN_SKIP_HOOK: OnceLock<fn ptr>` returns `true` for the Instance. Same shape as patch 5 today.
- Patch 5 UNCHANGED.

Patches 1, 2, 4 are deleted. Branch renamed (optional, see §7).

### What prompted it

Stages 1 (callback split) and 2 (cross-crate backend cleanup) were preparatory: the trait contract and backend assumptions are now correctly shaped for a DefId-keyed query override. This is the stage where that work cashes out.

### Intended outcome

- Fork patches 1, 2, 4 deleted from `~/rust`. Patch 3 reshaped to use a facade-installed hook (symmetric with patch 5). Patch 5 untouched. Toolchain rebuilt and reinstalled.
- Facade: `rustc-lang-facade/src/queries/per_instance.rs` deleted. New `queries/optimized_mir.rs` implements the override. `CODEGEN_SKIP_HOOK` wired at startup. Trait signature of `collect_generic_rust_deps` loses its `Instance` parameter (DefId-only now).
- Consumer: `ToylangCallbacks::collect_generic_rust_deps_inner` updated to use identity (`Param`-typed) args instead of substituted concrete args. Walker produces `(DefId, GenericArgsRef)` pairs where the args may contain Params. Rustc's collector substitutes per caller during its walk.
- All 211 tests pass. Zero warnings.

---

## 2. Required reading before you code (3–4 hours)

Read in this order; stop when oriented.

1. `/Users/verdagon/erw/CLAUDE.md` — project-wide instructions.
2. `/Users/verdagon/erw/HANDOFF-TL.md` — overall project orientation.
3. `/Users/verdagon/erw/docs/architecture/rust-interop-guide.md` §2 (four query providers) and §10.6.4 (visibility override hook — the reshape of patch 3 you're doing here mirrors patch 5's existing pattern). Skim §5 for state/locking context.
4. `/Users/verdagon/erw/docs/reasoning/rustc-fork-design-space.md` §4.1 — the design argument for this migration.
5. `/Users/verdagon/erw-poc-optimized-mir/findings.md` in full (~700 lines). **This is the most important document for this stage.** It's the POC that verified the rustc-side mechanism. Note especially: Exp 3 (dep discovery works), Exp 5 (LLVM IR receipt), Surprise 1 (`fully_monomorphized()` is a typing-mode, not an assertion), Q8 (code scaffold inventory). The POC's code lives on branch `poc/optimized-mir-override` — you will not adapt it wholesale (per scope decision, it predates the stage-1 callback split and doesn't match current `main`) but you'll reference its shape heavily.
6. `/Users/verdagon/erw/docs/usage/rebuilding-rustc-fork.md` — the toolchain rebuild procedure. You'll rebuild at least twice during this work. Memorize the 5-step workflow.
7. `/Users/verdagon/erw/docs/historical/handoff-cross-crate-backend.md` §4.1 — the `is_from_lang_stubs_safe` helper. Patch 3's reshape uses it.

---

## 3. Current surface (verified file:line references)

### Rustc fork (`~/rust` branch `per-instance-mir`)

Four patch locations (patch 5 excluded — not touched here):

- **Patch 1 — `~/rust/compiler/rustc_middle/src/query/mod.rs`** (or `queries.rs`, depending on rustc version; grep for `per_instance_mir`): the query definition. Something like `query per_instance_mir(key: Instance<'tcx>) -> Option<&'tcx Body<'tcx>>`. **Delete this block.**
- **Patch 2 — `~/rust/compiler/rustc_monomorphize/src/collector.rs`**: an `if let Some(body) = tcx.per_instance_mir(instance)` check that returns `body` before falling through to `instance_mir`. **Delete the check.** The collector should go straight to `instance_mir` (which calls `optimized_mir` internally for local items — that's where our override kicks in).
- **Patch 3 — `~/rust/compiler/rustc_codegen_ssa/src/mono_item.rs`**: currently something like `if tcx.per_instance_mir(instance).is_some() { return; }` inside the codegen dispatch. **Reshape this** — see §4.1 below.
- **Patch 4 — `~/rust/compiler/rustc_mir_transform/src/shim.rs`**: `providers.per_instance_mir = |_tcx, _instance| None;` or equivalent default provider. **Delete this.**
- **Patch 5 — `~/rust/compiler/rustc_monomorphize/src/partitioning.rs`** with `VISIBILITY_OVERRIDE_HOOK: OnceLock<...>` and its call site in `mono_item_linkage_and_visibility`. **Do not touch patch 5.** It's the pattern we're copying for patch 3's reshape.

### Facade (`rustc-lang-facade/src/`)

- `lib.rs:306` — `is_from_lang_stubs` (diagnostic-only, for within-`generate_and_compile` callers).
- `lib.rs:~320` — `is_from_lang_stubs_safe` (structural walk, safe everywhere). Added in stage 2.
- `queries/per_instance.rs` — the `per_instance_mir` override provider. **Delete in this stage.**
- `queries/optimized_mir.rs` — **create in this stage** (adapt shape from POC #1's `queries/optimized_mir_poc.rs`, but fresh — don't copy-paste).
- `queries/mod.rs` — query override installation. Swap out `per_instance_mir` wiring for `optimized_mir`.
- `queries/symbol_name.rs` — unchanged. `notify_concrete_entry_point` callback still drives internal-callee discovery.
- `queries/layout.rs`, `queries/drop_glue.rs` — unchanged. These don't depend on `per_instance_mir`.
- Trait `LangCallbacks::collect_generic_rust_deps` at `lib.rs:~140` — **drop the `instance` parameter** (see §4.2).
- `call_collect_generic_rust_deps` helper at `lib.rs:~313` — update to match the new trait signature (no Instance).

### Consumer (`toylangc/src/toylang/callbacks_impl.rs`)

- `ToylangCallbacks::collect_generic_rust_deps_inner` at `callbacks_impl.rs:~85` — **update the walker to use identity args** instead of `instance.args`. The function's unused `_def_id: LocalDefId` parameter becomes the load-bearing input.
- The trait impl of `collect_generic_rust_deps` (delegates to `_inner`) — update accordingly when dropping `Instance` from the trait signature.
- `collect_rust_deps_recursive` (the walker) — the substitution logic needs to change. Today it substitutes `instance.args` into the callee body. Tomorrow it uses identity args from the DefId, leaving `Params` in place.

### State and tests (mostly unchanged)

- `ToylangState` — no changes. `walked_entry_points` and `toylang_instances` are driven by `notify_concrete_entry_point`, which isn't changing.
- Integration tests — no changes expected. The migration should produce bit-identical consumer codegen output. If tests start failing, you've misimplemented something.

---

## 4. Proposed design

### 4.1 Rustc fork: reshape patch 3, remove patches 1/2/4

**Patch 3 reshape.** Replace the current `if tcx.per_instance_mir(instance).is_some() { skip }` pattern with a hook-based check, mirroring how patch 5 (`VISIBILITY_OVERRIDE_HOOK`) already works today. Two edits inside `rustc_codegen_ssa`:

Add in an appropriate module (e.g., `rustc_codegen_ssa/src/lib.rs` or `rustc_codegen_ssa/src/mono_item.rs`, at whichever level keeps visibility scoping simplest — follow patch 5's precedent in `rustc_monomorphize`):

```rust
// Facade-installed hook: should this Instance be skipped during
// CodegenBackend's mono item dispatch? If Some, return value is taken
// as authoritative. None means use default (don't skip).
pub static CODEGEN_SKIP_HOOK: std::sync::OnceLock<
    for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> bool,
> = std::sync::OnceLock::new();
```

Then in `mono_item.rs` replace patch 3's body check with:

```rust
if CODEGEN_SKIP_HOOK.get().is_some_and(|f| f(tcx, instance)) {
    return;  // same "skip codegen" behavior as before
}
```

The `return;` semantics are whatever patch 3 currently does (usually an early return from the `codegen_instance` dispatch arm that leaves the symbol as an extern declaration). Match the existing behavior exactly — this is a shape refactor, not a behavior change.

**Check rustc's `-D warnings` / `unreachable_pub`.** Per `docs/usage/rebuilding-rustc-fork.md`'s "Editing rules inside the fork": if you add a `pub static` inside a private `mod`, rustc refuses to compile. Flip the module declaration in the enclosing `lib.rs` to `pub mod` in the same patch. Patch 5's existing precedent will show you exactly how.

**Patches 1, 2, 4 deletion.** After deleting, grep the fork for `per_instance_mir`:

```bash
cd ~/rust
rg per_instance_mir
```

**Expect zero hits after deletion.** If anything references the removed query (stale comments, test fixtures, etc.), clean those too. A single lingering reference anywhere in the compiler will break compilation.

### 4.2 Facade trait: drop `Instance` from `collect_generic_rust_deps`

Current post-stage-1 signature at `rustc-lang-facade/src/lib.rs`:

```rust
fn collect_generic_rust_deps<'tcx>(
    &self,
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    instance: ty::Instance<'tcx>,   // ← remove
) -> Vec<(DefId, GenericArgsRef<'tcx>)>;
```

New:

```rust
fn collect_generic_rust_deps<'tcx>(
    &self,
    state: &mut (dyn Any + Send + Sync),
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> Vec<(DefId, GenericArgsRef<'tcx>)>;
```

Also update:
- The trampoline function signature in `lib.rs`.
- The `StatefulVtable` field signature.
- The `call_collect_generic_rust_deps` helper — drops the `instance` parameter.
- Every caller downstream.

The `instance` parameter drops out because the override runs at DefId granularity; rustc's collector substitutes per-Instance itself. The consumer no longer needs a concrete Instance to produce its dep list — just the DefId.

### 4.3 Facade: new `queries/optimized_mir.rs`

Structure (adapted from POC #1's `queries/optimized_mir_poc.rs` but reshaped for the current trait):

```rust
//! optimized_mir override — intercepts rustc's default MIR pipeline for
//! consumer function DefIds. Returns a synthetic body whose sole purpose
//! is to mention each Rust dep via ReifyFnPointer so rustc's monomorphization
//! collector discovers them via its standard per-caller substitution. Non-
//! consumer DefIds delegate to the saved default provider.
//!
//! Patch 3's reshaped CODEGEN_SKIP_HOOK ensures rustc skips emitting a body
//! for consumer items at codegen time — the consumer's LLVM backend provides
//! the real definitions.

use rustc_middle::mir::*;
use rustc_middle::ty::{self, GenericArgs, TyCtxt};
use rustc_span::def_id::LocalDefId;

pub fn lang_optimized_mir<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Body<'tcx> {
    let def_id_global = def_id.to_def_id();

    // Non-consumer DefIds: delegate to upstream default.
    if !crate::is_from_lang_stubs_safe(tcx, def_id_global) {
        return crate::default_optimized_mir(tcx, def_id);
    }

    // Consumer item. Ask the consumer for its generic (Param-typed) Rust deps.
    let Some(name) = compute_callback_name(tcx, def_id_global) else {
        return crate::default_optimized_mir(tcx, def_id);
    };
    let rust_deps = crate::call_collect_generic_rust_deps(&name, tcx, def_id);

    // Build a synthetic body with identity-args Instance shape.
    // Collector substitutes per-caller during its walk.
    let identity_args = GenericArgs::identity_for_item(tcx, def_id_global);
    let identity_instance = ty::Instance::new(def_id_global, identity_args);
    let body = build_dependency_body(tcx, identity_instance, &rust_deps);
    tcx.arena.alloc(body)
}
```

The `compute_callback_name` helper lifts the accessor-vs-regular-fn naming logic from the old `queries/per_instance.rs:23-68`. Copy that logic across; it's pure (no state mutation).

The `build_dependency_body` function at `queries/per_instance.rs:108-216` — **move it into `queries/optimized_mir.rs` unchanged**. It already handles Param-typed args per POC #1 Surprise 1; no refactoring required.

Wire the `DEFAULT_OPTIMIZED_MIR: OnceLock<...>` static in `lib.rs` following the precedent set by `DEFAULT_LAYOUT_OF`, `DEFAULT_MIR_SHIMS`, `DEFAULT_SYMBOL_NAME`. The `default_optimized_mir` accessor function takes the same shape those already use.

Installation in `queries/mod.rs`:

```rust
pub fn install_query_overrides(providers: &mut Providers) {
    // Save the upstream default BEFORE overriding, so we can delegate.
    let _ = crate::DEFAULT_OPTIMIZED_MIR.set(providers.optimized_mir);
    providers.optimized_mir = optimized_mir::lang_optimized_mir;

    // ... existing layout_of, symbol_name, mir_shims overrides stay ...
}
```

Install the codegen-skip hook at facade startup (same place you install `VISIBILITY_OVERRIDE_HOOK` today; see `lib.rs::install_callbacks`):

```rust
let _ = rustc_codegen_ssa::CODEGEN_SKIP_HOOK.set(|tcx, instance| {
    crate::is_from_lang_stubs_safe(tcx, instance.def_id())
});
```

### 4.4 Consumer: update walker to use identity args

`toylangc/src/toylang/callbacks_impl.rs`. The walker currently substitutes `instance.args` into the callee body to resolve concrete types. Under the override, args are identity — Params flow through unchanged. The same walker structure works, just with different input.

Change `collect_generic_rust_deps_inner`:

```rust
// Before (post stage-1):
pub fn collect_generic_rust_deps_inner<'tcx>(
    &self,
    state: &mut ToylangState,
    name: &str,
    tcx: TyCtxt<'tcx>,
    _def_id: LocalDefId,
    instance: ty::Instance<'tcx>,
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    // ... uses instance.args ...
}

// After (stage 3):
pub fn collect_generic_rust_deps_inner<'tcx>(
    &self,
    state: &mut ToylangState,
    name: &str,
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,     // no longer prefixed with _; now load-bearing
) -> Vec<(DefId, GenericArgsRef<'tcx>)> {
    let identity_args = ty::GenericArgs::identity_for_item(tcx, def_id.to_def_id());
    // Walker runs with identity_args: Params stay in place.
    // All downstream logic that currently takes instance.args takes identity_args.
    collect_rust_deps_recursive(tcx, &self.registry, toy_fn, name, identity_args, &mut cycle_guard)
}
```

Helper `resolve_caller_from_instance` becomes `resolve_caller_from_args(toy_fn, args, tcx)` or similar — the transformation is mechanical.

Update the trait impl to drop the `Instance` parameter when delegating to `_inner`.

### 4.5 Rebuild the toolchain

After fork edits, run the 5-step rebuild per `docs/usage/rebuilding-rustc-fork.md`. 8–12 minutes on a warm incremental build. Verify with the post-install sanity checks in that doc.

---

## 5. Implementation steps

Staged per our earlier decision (option 3a). Each step should leave the repository building and tests passing, OR explicitly gate the new behavior behind an env var during the transition.

### Step 0 — Branch + baseline

Confirm you're on `main` with a clean working tree. Run the full test suite to establish the 211/211 baseline:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

### Step 1 — Reshape patch 3 in the fork (not removing patches 1/2/4 yet)

Inside `~/rust`:

- Add the `CODEGEN_SKIP_HOOK: OnceLock<...>` static.
- Replace patch 3's body-check with the hook-based check.
- Leave patches 1, 2, 4, 5 alone.

Rebuild. Toolchain still has both mechanisms wired, but only one is active at a time.

At this point the facade still installs the `per_instance_mir` override AND doesn't yet install the `CODEGEN_SKIP_HOOK`. The reshaped patch 3 will always see `None` from the hook and default to "don't skip." But patch 3's old path (`per_instance_mir` returning Some) is gone — so the skip won't fire for consumer items — so `per_instance_mir`'s returned body will be codegen'd by rustc — link failure.

**To keep the tree green during Step 1**, also wire the hook at facade startup in the same commit:

```rust
let _ = rustc_codegen_ssa::CODEGEN_SKIP_HOOK.set(|tcx, instance| {
    crate::is_from_lang_stubs_safe(tcx, instance.def_id())
});
```

Now the hook says "yes, skip" for consumer items, exactly matching the old `per_instance_mir.is_some()` check's result under current `main`. Behavior is bit-identical; patches 1, 2, 4 are still present (unused by patch 3 but still firing via the `per_instance_mir` query).

Rebuild, run the full suite:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211. If anything fails here, the hook is misidentifying consumer items — check that `is_from_lang_stubs_safe` covers every DefId that the old `per_instance_mir.is_some()` branch covered. (They should match — both use `__lang_stubs` as the predicate.)

**Commit this as a single unit** to establish the "patch 3 reshape is green" baseline.

### Step 2 — Add `optimized_mir` override alongside `per_instance_mir`

Don't remove `per_instance_mir` yet. This step adds `queries/optimized_mir.rs` and wires it through, but the installer keeps BOTH overrides active behind an env-var gate (`ERW_USE_OPTIMIZED_MIR_OVERRIDE=1`):

- Gate off (default): install `per_instance_mir` override, skip `optimized_mir` override.
- Gate on: skip `per_instance_mir` override, install `optimized_mir` override.

Add the trait method `collect_generic_rust_deps_v2` (temporary name for the DefId-only signature) alongside the current one. Default impl on the trait delegates to the old one with a synthesized identity-args instance. This lets the facade call either version depending on the gate, without a breaking change.

In `queries/optimized_mir.rs`, use `collect_generic_rust_deps_v2`. In `queries/per_instance.rs`, keep using the old signature.

Baseline check without the gate:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211 — no behavior change, gate is off.

Gate-on check:

```bash
ERW_USE_OPTIMIZED_MIR_OVERRIDE=1 cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211. **This is the load-bearing checkpoint.** If the gated path passes all tests, dep discovery via `optimized_mir` override is working end-to-end, and stage 3's core mechanism is validated.

If gate-on fails tests, see §6 for common failure modes.

### Step 3 — Flip default behavior; delete `per_instance_mir`

Once Step 2 is green:

- Change the facade installer to install `optimized_mir` by default (not gated).
- Keep the gate as a fallback flag: `ERW_USE_PER_INSTANCE_MIR=1` reverts to the old path. This is temporary safety — you'll delete it a commit later.
- Run the full suite without any gate:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211.

Commit this.

Now delete:

- `rustc-lang-facade/src/queries/per_instance.rs` (the entire file).
- The `per_instance_mir` wiring in `queries/mod.rs`.
- The old `collect_generic_rust_deps` trait method. Rename `collect_generic_rust_deps_v2` → `collect_generic_rust_deps` and remove the temporary default impl shim.
- The temporary env-var gate in `queries/mod.rs`.
- The `ToylangCallbacks::collect_generic_rust_deps_inner`'s Instance-based path (update the signature per §4.4).

Rebuild (facade-side only, no fork changes needed here). Run full suite:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211.

### Step 4 — Delete fork patches 1, 2, 4 and rebuild toolchain

Inside `~/rust`:

- Remove patch 1 (query definition in `rustc_middle/src/query/mod.rs`).
- Remove patch 2 (collector check in `rustc_monomorphize/src/collector.rs`).
- Remove patch 4 (default provider in `rustc_mir_transform/src/shim.rs`).

Grep to verify:

```bash
cd ~/rust
rg per_instance_mir
```

Should return zero results.

Rebuild the toolchain (full 5 steps from `docs/usage/rebuilding-rustc-fork.md`). Then the full test suite:

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expect 211/211. This is the moment of truth — the toolchain no longer has the custom query at all, and the facade is running entirely through `optimized_mir`.

### Step 5 — Documentation pass

Update:

- `docs/architecture/rust-interop-guide.md` front-matter: "Minimal rustc fork with `per_instance_mir` query" → "Minimal 2-patch rustc fork (codegen skip + visibility override)." Add a line pointing at `future-architecture-investigations.md` for the remaining-fork context.
- `docs/architecture/rust-interop-guide.md` §2.2 ("per_instance_mir"): rewrite as §2.2 "optimized_mir override" — describe the DefId-keyed approach, Param substitution by rustc's collector, and how it relates to the reshaped patch 3. Note that the section's design-space cross-references to `rustc-fork-design-space.md` are now partially realized (§4.1 has landed; §4.2 is future work).
- `docs/architecture/rust-interop-guide.md` §10.6.4: nothing changes structurally but note in passing that the "facade-installs-a-hook" pattern the visibility override uses has a sibling now — the codegen-skip hook.
- `docs/reasoning/rustc-fork-design-space.md` §4.1: update the verdict from "prototype-verified, migration deferred" to "landed in stage 3." Cite the commit.
- `docs/reasoning/rustc-fork-design-space.md` §5 (cost accounting): the fork is now 2 patches, not 5. Update the maintenance-cost line.
- `future-architecture-investigations.md` Part 5: mark POC #1's findings as "incorporated into shipping architecture (stage 3)."
- `docs/architecture/known-tech-debt.md`: if any entries mention `per_instance_mir` or the 5-patch fork, update.
- `CLAUDE.md`: the "Background" section references "4-patch fork of `nightly-2025-01-15` with `per_instance_mir` query." Update to "2-patch fork with codegen-skip + visibility-override hooks."

### Step 6 — Optional: rename the fork branch

`~/rust` is on branch `per-instance-mir` today. After the migration, that name is misleading. Options:

- Rename to `facade-hooks` or `minimal-facade`.
- Leave the name; it's historical and the docs explain.

Recommend leaving the name for this stage — rebuilding the rustup toolchain link on a renamed branch is extra ceremony. Revisit when stage 4 (plugin) completes and the fork is gone entirely.

### Step 7 — Commit

Single coherent commit once all steps verify green. Draft message following the project's dense-paragraph style (see recent commits like `b345162` for the stage-2 example). Include test totals, patch-count reduction (5 → 2), and cite the POC branch as the reproducibility anchor.

---

## 6. Critical subtleties

### 6.1 The POC's "Surprise 1" is load-bearing but easy to miss

`TypingEnv::fully_monomorphized()` is a typing-MODE declaration, not an assertion that there are no Params in the input. `build_dependency_body` (moved into `queries/optimized_mir.rs`) passes Param-containing sigs through this typing env. POC #1 Surprise 1 confirmed this works — the normalizer leaves Params alone. **If you're tempted to "clean up" by switching to a different typing env or adding Param-is-None assertions, don't.** The name is misleading.

### 6.2 Identity args MUST come from `GenericArgs::identity_for_item`

Not from constructing `GenericArgs::empty()` or manually building a `GenericArgs::for_item` with no user types. The identity variant is specifically designed for this — it fills all slots with the declaration's own Params, including lifetimes (which get `re_erased` per stage-prior-phase-prior @ELASZ arcana handling). A hand-built variant will silently produce wrong sigs. See `@ELASZ` arcana if you're unsure.

### 6.3 The accessor callback name logic must port exactly

`queries/per_instance.rs:23-68` currently has logic that computes the callback name differently for accessors (`"StructName.field"`) vs regular functions (`"make_counter"`). Port this logic byte-accurately to `queries/optimized_mir.rs::compute_callback_name`. If you simplify or refactor in the move, it's easy to break accessor handling subtly — tests like `test_counter_construct` and `test_pair_accessors` would then fail.

### 6.4 `is_from_lang_stubs_safe` is load-bearing in two new places

Under this stage it appears in:
- The reshaped patch 3's hook body (via the closure the facade installs at startup).
- The `optimized_mir` override's consumer-vs-default filter.
- Existing: `ToylangCallbacks::visibility_override` (from stage 2).

Three call sites on the same helper. If you feel the urge to inline the DefPath walk anywhere for "performance" — don't. The helper is already cheap, and consolidation is what lets patch 3's reshape and the `optimized_mir` override compose cleanly.

### 6.5 Don't regress `per_instance_mir` usage before Step 4

During Steps 1–3, the `per_instance_mir` query and its fork patches are still present and partially live. Don't remove them piecemeal — the dependency between patch 2 (collector check) and `per_instance_mir` as a usable query means half-removals trip rustc compile errors. Full removal happens in Step 4, atomically.

### 6.6 Two toolchain rebuilds expected

You'll rebuild the fork twice: once in Step 1 (add the hook), once in Step 4 (remove patches 1/2/4). Each rebuild is 8–12 minutes per `docs/usage/rebuilding-rustc-fork.md`. Plan your work around these — don't try to squeeze a rebuild into an idle 5-minute window.

If you find yourself rebuilding more than twice, you're likely iterating on fork changes (normal during debugging — the 8-minute rebuild loop is the dominant feedback cost). Batch fork edits locally before rebuilding.

### 6.7 `VISIBILITY_OVERRIDE_HOOK` precedent for `CODEGEN_SKIP_HOOK`

Patch 5 already establishes the "pub static OnceLock<fn ptr> for the facade to install into" pattern. Your `CODEGEN_SKIP_HOOK` addition in patch 3 should mirror it exactly in shape, naming convention, and placement. If patch 5 lives in `rustc_monomorphize/src/partitioning.rs`, your hook lives in a parallel spot in `rustc_codegen_ssa` (not necessarily the same file — choose by where the calling `mono_item.rs` code actually lives and what visibility is cleanest).

### 6.8 The `visited_symbols` → `walked_entry_points` rename happened in stage 1

If you see references to `visited_symbols` anywhere — stop. That's a ghost from pre-stage-1. Current name is `walked_entry_points`. Not a thing this stage changes, but worth double-checking because the stage-1 refactor may have left stale references in docs or comments.

### 6.9 Tests will not exercise the Param-typed-deps path directly

The 211 tests test *observable behavior*: symbols emitted, programs running, panic messages, etc. None of them assert anything about what gets passed to the `optimized_mir` override internally. This is by design — the refactor is behavior-preserving. **You will not get strong signal that Params are flowing correctly from the test suite alone.** If you want direct confirmation, add a temporary `eprintln!` in `collect_generic_rust_deps_inner` that dumps the `GenericArgs` shape of returned deps; Params should appear for generic consumer functions (e.g., `wrap<T>` should produce deps with `Param(T)` in their args, not `i32`/`u8`/concrete types). Remove the eprintln before committing.

---

## 7. Verification

### Build & test (per CLAUDE.md's updated redirect convention)

Pipe to a fixed `/tmp/stage3.txt` via `>` (not `tee`):

```bash
cargo +rustc-fork test -p toylangc 2>&1 > /tmp/stage3.txt
grep "test result:" /tmp/stage3.txt
```

Expected final numbers:
- `test result: ok. 67 passed; 0 failed` (unit)
- `test result: ok. 129 passed; 0 failed` (integration)
- `test result: ok. 15 passed; 0 failed` (standalone)
- **Total: 211 passing, 0 failed, 0 ignored.**

Zero warnings:

```bash
cargo +rustc-fork check -p toylangc 2>&1 > /tmp/stage3.txt
grep "warning:" /tmp/stage3.txt
```

### Required greps (post-Step-4)

```bash
# In ~/rust (the fork):
cd ~/rust
rg per_instance_mir   # expect zero hits

# In the facade + consumer:
cd /Users/verdagon/erw
rg per_instance_mir rustc-lang-facade/ toylangc/   # expect zero hits in code; may appear in docs/historical/ etc, that's fine
rg collect_generic_rust_deps rustc-lang-facade/ toylangc/   # expect hits in lib.rs (trait), optimized_mir.rs (caller), callbacks_impl.rs (impl) — NOT in per_instance.rs since that file is deleted
rg CODEGEN_SKIP_HOOK   # expect hits in facade install_callbacks + (after fork rebuild) in ~/rust/compiler/rustc_codegen_ssa/
```

### Sanity: observable behavior unchanged

Pick any Phase 7 standalone test (e.g., `uuid_test`). Before Step 1, capture the produced LLVM IR:

```bash
cd toylangc/tests/standalone/uuid_test
rm -rf .toylang-build
cargo +rustc-fork run --bin toylangc --manifest-path /Users/verdagon/erw/Cargo.toml -- build
cp .toylang-build/target/debug/deps/*.ll /tmp/stage3-before-uuid.ll
```

After Step 4, same command into `/tmp/stage3-after-uuid.ll`. Diff the two. Expect:
- Same Rust `declare` lines (same dep discovery output).
- Same toylang-emitted function bodies.
- Possibly different rustc synthetic-body content (the dead trampoline body that patch 3 skips — its shape may differ because we switched from Instance-keyed to DefId-keyed synthesis). This is acceptable — the body is never codegenned into the binary, so its LLVM IR is a pre-DCE artifact that doesn't affect link.

If you see differences in ACTUAL function bodies (the toylang-emitted ones, or the Rust deps' emitted code), investigate before merging.

### Fork-rebuild sanity

After Step 4's toolchain rebuild, verify the install:

```bash
ls $HOME/rust/build/host/stage2/bin/rustc
ls $HOME/rust/build/host/stage2/lib/rustlib/aarch64-apple-darwin/lib/ | grep rustc_abi
```

Both must exist. If either is missing, consult `docs/usage/rebuilding-rustc-fork.md` "Verifying the install worked."

---

## 8. Out of scope / follow-ups

**Not in this task** (flag but don't do):

- Remove fork patch 3 entirely (requires the `CodegenBackend` plugin, stage 4).
- Remove fork patch 5 entirely (same).
- Migrate `symbol_name` override to use rustc's default_symbol_name for non-consumer items (it already does — `queries/symbol_name.rs` delegates to the saved default via `DEFAULT_SYMBOL_NAME`).
- Rename the fork branch from `per-instance-mir` to something else.
- Stability / MIR-construction-churn work. The `build_dependency_body` function still uses rustc-internal MIR types. That stays.
- Audit whether the existing `normalize_erasing_late_bound_regions(fully_monomorphized())` call in `build_dependency_body` could use a cheaper normalizer now that we're Param-typed. Probably not worth it; POC #1 already verified it doesn't ICE. Leave alone.

**Follow-up tickets to file** after landing:

- "Investigate rename of fork branch `per-instance-mir` → `facade-hooks` once stage 4 clarifies the final fork shape."
- "Consider if `queries/optimized_mir.rs::compute_callback_name` and the parallel logic in `queries/symbol_name.rs` can share a helper."

---

## 9. Rollback plan and if-you-get-stuck

### Rollback

Because the migration is staged, rollback is proportional to how far you got:

- **Failed at Step 1 (patch 3 reshape doesn't pass baseline tests):** Revert the fork edit in `~/rust`, rebuild. Facade unchanged. Back to 5-patch fork, zero loss.
- **Failed at Step 2 (gate-on fails tests):** The `optimized_mir` override is wrong somewhere. Keep the old `per_instance_mir` path as default (gate off); investigate the gated path without pressure. Don't merge Step 2 until the gate-on path is green.
- **Failed at Step 3 (removing `per_instance_mir` from facade breaks tests):** Revert the facade changes from Step 3; the fork still has patches 1/2/4 plus the reshaped patch 3. Back to a working state (slightly bigger fork than original, but functional).
- **Failed at Step 4 (removing fork patches breaks tests after rebuild):** The toolchain rebuild is reproducible. Revert the fork edits, run the 5-step rebuild again, verify the test suite. Don't proceed until green.

At every stage the tree is green before the next commit. Don't skip the full test run between steps — the 30–45 second cost is trivial compared to debugging a multi-step-removed-in-one-commit regression.

### Come find the TL if:

- After Step 2's gate-on runs, you see more than a handful of test failures. Expect zero to a few (which are educational); if tens fail, your `optimized_mir` override's Param handling is fundamentally wrong and we need to rethink.
- The fork rebuild fails mysteriously after a patch-3 reshape. Rustc's compile errors on `unreachable_pub`, `mismatched types`, or trait-object issues are common and usually mechanical fixes — but if you're hitting errors that look semantic (lifetime mismatches, trait bound failures, unexpected query-timing issues), stop and ask.
- The patch-3 hook installation conflicts with patch 5's existing `VISIBILITY_OVERRIDE_HOOK` somehow — e.g., visibility scoping forces a module structure change. Patch 5 is the precedent; mimicking it should Just Work, but if rustc's `-D warnings` fights you, the answer is usually to flip a `mod foo;` to `pub mod foo;` in the enclosing `lib.rs`, not to suppress the warning.
- POC #1's findings document references something in the code that doesn't match current `main`. The POC predates stages 1 and 2; we expect some drift. If you find major drift (not just naming — e.g., structural differences), document what you found and ask how to reconcile.
- A test that's supposed to be behavior-preserving starts producing different output. This is the single highest-signal failure mode — it means the Param-flowing path is subtly broken in a way tests do catch. Don't paper over with debug tweaks; investigate the root cause.

Don't be shy. The refactor reads long because it touches rustc fork internals; the actual per-step code change is small once you have the reshape and override shapes down. Most of this doc is guardrails and context so you don't have to re-derive what the POC already verified.
