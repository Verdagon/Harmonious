# Workstream A — landed

Status: **DONE.** Course-correct.md items #11 (rlib produces no toylang `.o`)
and #15 (binary compile is the codegen site) are shipped. As of the
post-Phase 1 D state: **243/243 tests passing** (90 unit + 137 integration
+ 16 standalone). Workstream A itself shipped at 228/228; the remaining
+15 came from later Phase 3 (multi-crate, +1), Phase 1 D (rust_caller
fixtures for Cases 1a/1b/3/5, +4 + a small fix in
`codegen_extern_wrapper`'s `rust_ret_type` arm for direct-coerced
`RustType` returns), additional test infrastructure, and A.5.

This file's earlier draft documented an attempted A landing that failed at
the oracle blocker. That blocker was fixed and A re-attempted successfully
in the same session. The current file is the keeper version.

## What landed in A

| Sub | File | Change |
|---|---|---|
| A.1 | `toylangc/src/main.rs::run_toylang_compile` | `llvm_paths` allocation flipped: rlib gets `None`, user-bin gets `Some(...)`. Rlib compile produces no toylang `.o`. |
| A.2 | `toylangc/src/main.rs` + `toylangc/src/toylang/callbacks_impl.rs` | Renamed `is_downstream_of_stubs` → `is_user_bin_compile` throughout. Semantics identical; name describes the role rather than the dependency direction. |
| A.4 | `toylangc/src/toylang/callbacks_impl.rs::populate_toylang_instances_from_cgus` | Gate inverted (runs at user-bin, short-circuits at rlib). At user-bin time, replaced the upstream-CGU walk (which finds zero stub items there — see "Why" below) with **registry-driven discovery**: iterate `self.registry.functions`, push a `ToylangInstance` per non-generic body-bearing fn, look up its `pub fn` shell DefId in upstream `__lang_stubs`, then transitively `walk_and_stash_internal_callees` to surface generic monomorphizations. |
| A.4 codegen side | `toylangc/src/llvm_gen.rs::generate_with_tcx` | When iterating `state.toylang_instances`, build `Instance::new_raw(stub_def_id, empty_args)` for entries that carry a stub DefId so they qualify for extern-wrapper (`__toylang_impl_*`) codegen — otherwise only the internal symbol would be emitted and link fails. |
| (validation) | `callbacks_impl.rs::after_rust_analysis` | Gate flipped: rlib owns validation; user-bin trusts it. |

Two new helpers landed alongside (used by A but also generally useful):

- `toylangc/src/oracle.rs::find_stub_fn_in_stub_rlib` — looks up a `pub fn` shell at the upstream `__lang_stubs` crate root via `module_children`.
- `toylangc/src/toylang/callbacks_impl.rs::ToylangInstance.stub_def_id` — new `Option<DefId>` field that carries the looked-up DefId from populate → generate.

## The two key unlocks

Without these the A pivot doesn't land. Worth knowing if anyone touches the codegen-discovery path again:

### Unlock 1: oracle cross-crate sweep

`toylangc/src/oracle.rs::find_extern_fn_def_id` and
`find_wrapper_fn_def_id` previously walked only `tcx.hir_crate_items(())` —
LOCAL HIR items. At rlib compile every extern decl and Phase-6 wrapper is
local, so this worked. At user-bin compile the `__lang_stubs` rlib is
extern; the local walk finds nothing and the codegen path panics with
`extern fn 'println_i32' not found in Rust source`.

Fix landed earlier in the same session (stage-5a oracle sweep completion):
add a cross-crate fallback that walks `__lang_stubs`'s `module_children` +
each `extern "C"` ForeignMod's children. Pattern was already established
by stage 5a's `resolve_rust_path` for the `use`-imported lookups; it just
never got extended to `extern "C"` decls or wrapper-fn lookups.

**The cross-crate fallback is exercised by
`test_oracle_cross_crate_extern_fn_lookup`** — a smoke test that runs at
user-bin compile and counts successful resolves. If a future change
reverts the fallback to local-only iteration, this test catches it.

### Unlock 2: transitive callee walking for generic monomorphizations

The registry-driven discovery loop iterates non-generic toylang fns only —
generic ones panic on un-substituted `TypeParam` if reached without
concrete args. But some test fixtures have non-generic fns calling
generic ones (e.g. `wrap_i32(x)` calls `wrap<i32>(x)`). The generic call
needs to be emitted too, with concrete args substituted at the call site.

The existing `walk_and_stash_internal_callees` helper does exactly this
walk — it traverses a typed body, finds consumer-to-consumer calls with
concrete type args, recursively descends, and pushes `ToylangInstance`s
for each callee. Re-invoking it from the registry loop (per non-generic
entry) surfaces every transitively-reachable generic monomorphization
without needing rustc's mono collector to queue them.

**Without unlock 2, the 5 `test_generic_*` integration fixtures fail with
`Undefined symbols: __toylang_internal_wrap__i32` at link time.** With
it, they pass.

## Why the upstream CGU walk no longer finds consumer items at user-bin

This bit me empirically — adding a probe showed `total mono Fn=8,
from_stubs=0, registered=0` at user-bin time.

Rustc's monomorphization collector treats extern **non-generic** items as
"linked from upstream" by default. The user-bin's `main.rs` calls
`__toylang_main()`; rustc generates a call site but does NOT queue
`__toylang_main`'s body for local codegen — it expects the upstream
rlib's `.o` to provide the symbol. So the CGU list at user-bin has zero
consumer Fn items.

The pre-A architecture worked because at the rlib compile, `__toylang_main`
IS local — rustc queues it, the per_instance_mir override supplies a
synthetic body, and our walker discovers it via the CGU list. Under A
that whole flow is inverted, so the discovery mechanism has to change.

Registry-driven discovery sidesteps rustc's mono machinery entirely.
The architecture doc §5.5 explicitly endorses this: "Sky's codegen walks
Sky's universe, not rustc's collector."

## What this leaves for Workstream A.5 + later phases

- **A.5** (byte-identical pass-through corpus) — not landed. CI infrastructure work; deferred for a dedicated session.
- **Generic toylang fns directly callable from Rust source** — still out of scope. Course-correct item #17 (`stub_gen.rs` non-generic branches). When that lands, generic consumer items can be added to `populate_toylang_instances_from_cgus`'s top-level walk; the structure is ready.
- **Accessor methods at user-bin** — works because rustc's trait/method dispatch DOES queue them at the downstream compile (unlike extern non-generic fns). `generate_with_tcx`'s accessor walk via the CGU stash continues to function.
- **Layout-probe log emission** — the `[toylang] layout_of intercepted for: <ty> size=N align=M` log used to fire from the CGU walk's per-Instance signature traversal. Under registry iteration there's no per-Instance Ty to walk. The probe tests (`test_point_layout` etc.) still pass because the `lang_layout_of` query provider emits its own log line as a fallback. If a future test surfaces a gap, the re-engineering option is to construct an Instance per registry item and walk its signature — but it's not currently needed.

## Files touched (Workstream A only — not S.4, S.5, or oracle sweep)

- `toylangc/src/main.rs`
- `toylangc/src/toylang/callbacks_impl.rs`
- `toylangc/src/oracle.rs` (`find_stub_fn_in_stub_rlib` helper added)
- `toylangc/src/llvm_gen.rs` (Instance construction at codegen)

— previous engineer
