# Phase 3 E.6 — LANDED

**Status: complete.** `test_case6_basic_multi_crate` passes; the first
multi-toylang-crate program builds, runs, and prints `42`. E.6 itself
shipped at 236/236; subsequent Phase 1 D + cleanup work brought the suite
to **243/243 passing** (90 unit + 137 integration + 16 standalone).

## The root cause (what I got wrong before)

The earlier "scope notes" speculated about panics and @GCMLZ. The actual
cause, identified by `sample`-ing the hung process:

```
trampoline_generate_and_compile (lib.rs:703)  ← holds MUTABLE_STATE
  → rustc query: symbol_name
    → lang_symbol_name (queries/symbol_name.rs)
      → call_notify_concrete_entry_point
        → std::sync::Mutex::lock on MUTABLE_STATE  ← DEADLOCK
```

`std::sync::Mutex` is **not reentrant**. The same thread holding the lock
from the outer `call_generate_and_compile` blocks itself waiting on the
inner `call_notify_concrete_entry_point`. No panic, no error — just 0%
CPU, the classic same-thread-mutex deadlock signature.

Single-project tests don't hit it because their `symbol_name` queries are
cached by rustc during the earlier mono walk (before
`generate_and_compile` starts), so the override doesn't re-fire during
codegen. In the multi-crate case, the cross-toylang-lib call site for
`double_it` forces a fresh `tcx.symbol_name(instance)` query during
codegen, which re-enters the facade.

## The fix

A thread-local fat pointer stashed by `trampoline_generate_and_compile`
exposes the held state to re-entrant callers. `call_notify_concrete_entry_point`
checks the TL — if set, bypass the lock and use the existing pointer; if
unset (the normal mono-collection path), lock as before. Implemented in
`rustc-lang-facade/src/lib.rs`:

- `GENERATE_AND_COMPILE_FAT_STATE` thread-local + `GcState` struct.
- `set_generate_and_compile_state` / `clear_generate_and_compile_state`
  bracket the consumer's `generate_and_compile` call.
- `try_get_generate_and_compile_state` returns `Option<*mut dyn Any+Send+Sync>`
  for use at the re-entry site.

Soundness: the pointer's lifetime is bounded by the trampoline's stack
frame, and rustc query execution is single-threaded per session, so
there's no aliasing across threads.

## The other piece that was needed

Even after the deadlock was fixed, the link still failed with
`__toylang_impl_double_it` undefined. The IR dump showed:

```
declare i32 @__toylang_impl_double_it(i32)        ← from main's call site
define  i32 @__toylang_impl_double_it.1(i32 %0)   ← from upstream wrapper, LLVM renamed
```

LLVM disambiguated the second `define` to `.1` because the codegen had
already emitted a `declare` for the same name. Root cause: the
`FnCall` codegen path at `llvm_gen.rs:1161` checks `ctx.registry`
(local-only) to decide between toylang-internal-ABI vs Rust-extern-ABI.
For `double_it` (in upstream only), it took the Rust-extern path and
emitted a `declare`. The populate-iteration loop then `define`d the
same symbol — LLVM mangled the latter to `.1`.

Fix: build an `effective_registry` (local merged with upstream) inside
`generate_and_compile` and pass that to `generate_with_tcx`. Now
`registry.functions.get("double_it")` is `Some` at codegen time too,
the FnCall takes the toylang path, no conflicting declare gets emitted.

## What landed this session under E.6

Beyond the deadlock fix:

1. **`is_consumer_fn` / `is_consumer_type` upstream mirror** on
   `ToylangCallbacks`. `on_sky_lib_loaded` populates `upstream_fn_names`
   + `upstream_type_names` sets that the predicates consult after the
   local-registry miss. The `Arc<Mutex<HashSet<String>>>` is held only
   for the duration of a `contains()` lookup; the @GCMLZ deadlock is in
   `MUTABLE_STATE`, not these mutexes.

2. **Upstream iteration in `populate_toylang_instances_from_cgus`**.
   Clones `state.upstream_registries.values()` so we can hold an
   immutable borrow during the outer `for reg in registries` loop while
   the inner `walk_and_stash_internal_callees` mutates `state`. Each
   upstream consumer fn gets a `ToylangInstance` with `stub_def_id`
   resolved via `find_stub_fn_in_stub_rlib` (which iterates ALL
   marker-bearing crates per E.6's earlier fix).

3. **`effective_registry` merge in `generate_and_compile`**. Mirrors the
   E.5 typecheck-side merge so codegen sees the same view.

4. **Test fixtures + integration test**:
   - `case6_lib/` — exports `double_it(x: i32) -> i32 { x * 2 }`.
   - `case6_app/` — depends on case6_lib, calls `double_it(21)`.
   - `case6_app/expected_output.txt` = `42`.
   - `test_case6_basic_multi_crate` in `integration_projects.rs`.

## Files touched in this E.6-debug-and-land round

Facade:
- `rustc-lang-facade/src/lib.rs` — thread-local state-pointer + bypass.

Toylang:
- `toylangc/src/toylang/callbacks_impl.rs` —
  - `upstream_fn_names` + `upstream_type_names` fields on `ToylangCallbacks`
  - `is_consumer_fn` / `is_consumer_type` upstream consult
  - `on_sky_lib_loaded` mirror population
  - `populate_toylang_instances_from_cgus` upstream iteration
  - `generate_and_compile` effective_registry build
- `toylangc/src/main.rs` — construct `ToylangCallbacks` with the new fields.

Test:
- `toylangc/tests/integration_projects/case6_lib/{toylang.toml,main.toylang}`
- `toylangc/tests/integration_projects/case6_app/{toylang.toml,main.toylang,expected_output.txt}`
- `toylangc/tests/integration_projects.rs` — `test_case6_basic_multi_crate`.

## Architectural significance

This is the **first test** that exhibits Sky's locked invariant from
architecture §5.5 / §9.6:

> Sky libraries do not ship precompiled bodies. They ship only the Rust
> stub source + the Sky source + the Temputs sidecar. Every Sky body in
> the final binary is codegenned at the binary's compile, from the
> library's Sky AST stored in the sidecar.

`case6_lib`'s rlib has NO `.o` for `double_it`. case6_app's user-bin
compile reads case6_lib's `.sky-meta` sidecar at `on_sky_lib_loaded`,
populates `state.upstream_registries["case6_lib"]`, and codegens
`double_it`'s body into the binary's `.o` from the upstream typed AST.
That's literal Sky architecture, no rehearsal.

Course-correct items now done:
- #6 (marker-based detection) ✓
- #11 (binary-compile codegens everything) ✓
- #15 (codegen site moved to user-bin) ✓
- #16 (per-Sky-library stub rlibs + cross-crate flow exercised) ✓

## What remains
- Other interop cases from the seven-case taxonomy (1b, 3, 4, 5) — need
  fixtures + the specific machinery each one requires (e.g. Case 4 needs
  toylang `impl Trait for Type` which is a real language feature, Phase 2).
- Doc updates to `tl-handoff.md` § handoff narrative for Phase 3 +
  E.6's hard-won lessons.
