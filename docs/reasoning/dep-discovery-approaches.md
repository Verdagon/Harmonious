# Per-Instance vs DefId-Keyed Dep Discovery

There are two valid architectures for doing per-Instance Rust-dep discovery in this facade, and the project has shipped both of them in succession. This document compares them, explains why both work, and names the non-obvious insight that makes the second one viable.

**TL;DR:** both approaches correctly produce the full per-Instance Rust-dep set that rustc needs to queue for codegen. One does it by running a callback per-Instance (we substitute concrete types ourselves). The other does it by running a callback per-DefId (rustc substitutes from Param placeholders). The key surprise: dep discovery's OUTPUT must describe each per-Instance dep, but its COMPUTATION can be symbolic and run once per DefId. Rustc's collector substitutes the placeholders per caller.

---

## Context

The facade's core technical problem: a consumer function like `wrap<T>(x: T) -> Wrapper<T>` may call Rust items that depend on `T` (e.g., `Vec::<T, Global>::new`). When Rust code instantiates `wrap<i32>` and `wrap<i64>`, rustc's collector needs to queue `Vec::<i32, Global>::new` AND `Vec::<i64, Global>::new` for codegen. Since the consumer's source is invisible to rustc, the facade has to tell rustc what Rust items each consumer Instance needs.

Call this the **dep-discovery problem**. The shared goal across all solutions: produce the concrete `(DefId, concrete_args)` tuples for every Rust item transitively reachable from every reachable consumer Instance, and get them into rustc's collector queue.

The architectural invariant established by `docs/reasoning/why-interleaved-monomorphization.md` is that whatever mechanism solves this problem must plug into rustc's monomorphization phase — otherwise we miss Instances that only rustc can discover (Cases 1b, 3, 4, 5, 6 of that doc's taxonomy). Pre-pass fails; we must interleave.

That constraint leaves room for multiple valid interleaving mechanisms.

---

## The non-obvious insight

Before diving into the two approaches, name the insight that makes both viable: **dep discovery's output needs to describe per-Instance deps, but its computation can be per-DefId and symbolic.**

Intuitively, "what Rust items does `wrap<i32>` call?" sounds like a question you need concrete types to answer. But the SHAPE of the answer (which deps, at which call sites, with what args-structure) is the same for every instantiation of `wrap`. Only the concrete values at each type slot differ.

So there are two separable axes:

- **Output content.** Must describe the per-Instance deps — concrete args plugged into each slot.
- **Computation granularity.** Can be per-Instance (we substitute while computing) OR per-DefId (we return a Param-shaped skeleton; someone else substitutes later).

If we produce a symbolic answer (`Vec::<Param(T)>::new` instead of `Vec::<i32>::new`), we've captured the full shape. Rustc's own substitution machinery — the same engine that turns the stdlib's Param-typed `Vec::push` body into its concrete monomorphizations — can substitute Params → concrete per caller, producing `Vec::<i32>::new` when walking `wrap<i32>` and `Vec::<i64>::new` when walking `wrap<i64>`.

This is the "caught me off guard" insight. At first glance dep discovery feels inherently per-Instance. It isn't — it can be symbolic with rustc doing the per-Instance concretion afterward. That opens a second valid architecture.

---

## Approach A: Instance-keyed callback (`per_instance_mir`)

**Status:** shipped in erw until stage 3 of the fork-reduction roadmap (see `rustc-fork-design-space.md`). Retired in favor of Approach B.

**Mechanism.** A custom rustc query, `per_instance_mir(Instance) -> Option<&Body>`, added via fork patches 1, 2, 4. Rustc's collector calls it during its walk (patch 2 wires the call site). Our override receives the concrete `Instance` with concrete args already resolved.

**Where substitution happens.** In our code. The walker uses `instance.args` to substitute `T → i32` (or whatever) through the consumer's body as it collects deps. The returned dep list has concrete args at every slot: `(Vec::new_DefId, [i32, Global])`.

**Callback invocations.** Once per Instance. If consumer code instantiates `wrap<i32>` and `wrap<i64>`, the callback fires twice.

**Fork cost.** Three patches (1, 2, 4) for the query plumbing. Plus patch 3 to skip codegen for our returned bodies. Plus patch 5 for an unrelated visibility hook.

---

## Approach B: DefId-keyed callback (`optimized_mir` override)

**Status:** shipped in erw starting stage 3.

**Mechanism.** Override rustc's existing `optimized_mir(LocalDefId) -> &Body` query via `rustc_interface::Config::override_queries` — the sanctioned extension point that rust-analyzer, clippy, and miri all use. No custom query; no fork patches for the query itself.

**Where substitution happens.** Not in our code. We return a body with `GenericArgs::identity_for_item(def_id)` — Params remain in every type slot. The dep list contains `(Vec::new_DefId, [Param(T, 0), Global])` with `ty::TyKind::Param` nodes where `T` would be. Rustc's collector then substitutes those Params per-caller during its walk — the same substitution engine that handles every generic Rust function in existence.

**Callback invocations.** Once per DefId. If consumer code instantiates `wrap<i32>` and `wrap<i64>`, the callback still fires only once — rustc caches the `optimized_mir(wrap_def_id)` result and walks the cached body with different Instance args each time.

**Fork cost.** Zero patches for the query itself (sanctioned override). Patch 3 still needed (reshaped as a hook). Patch 5 still needed (unrelated).

---

## Why both satisfy the interleaving invariant

`why-interleaved-monomorphization.md` requires that the facade be callable during rustc's monomorphization phase — so the collector can query us about consumer items while it's walking. Both approaches satisfy this:

- **Approach A**: our callback fires during the collector's walk. Per Instance.
- **Approach B**: our callback fires during the collector's walk. Per DefId (cached). The collector substitutes Params per-Instance using the cached body.

The invariant is about PHASE (we must be reachable during monomorphization), not GRANULARITY (once per Instance vs once per DefId). Both approaches plug into the phase; neither is a pre-pass. A pre-pass running before rustc starts would fail to see Instances rustc later discovers from Rust-side call sites (Cases 1b, 3, 4, 5, 6 of the taxonomy).

Worth naming a subtle consequence: under Approach B, the work our callback does is conceptually phase-independent — it's a symbolic scan of consumer source, producing Param-typed deps. We run it lazily during monomorphization because that's when rustc's query system first requests it, but the computation itself doesn't depend on monomorphization-time state. This is what the user-facing insight "the deep scan doesn't need to be at monomorphization time" captures — the COMPUTATION is phase-independent even though the INVOCATION happens during the phase.

---

## Tradeoff comparison

| Axis | Approach A (per_instance_mir) | Approach B (optimized_mir override) |
|---|---|---|
| Query | Custom (3 fork patches) | Rustc's existing (no patches) |
| Keying | Instance | DefId |
| Callback invocations | O(consumer_fns × monomorphizations) | O(consumer_fns) |
| Substitution performed by | Our walker | Rustc's collector |
| Walker complexity | Higher (handles `instance.args`) | Lower (identity args only) |
| Substitution correctness risk | Our code, maintained against rustc internals | Rustc's engine — already-tested |
| Fork reduction potential | Blocks patches 1, 2, 4 | Compatible with retiring patches 1, 2, 4 |
| Mental overhead | "My callback has concrete args, life is easy" | "My callback produces Param-typed output, rustc substitutes later" |

Both produce the same result (identical dep sets queued for rustc codegen). The project picked A initially because it was easier to reason about; migrated to B because it's cleaner structurally and blocks fewer fork patches.

---

## What the insight does NOT apply to: internal toylang callees

The "Params OK, rustc substitutes" trick works for Rust deps specifically because rustc's collector is the downstream substitutor — its `EarlyBinder::instantiate` machinery turns Param-typed `optimized_mir` bodies into concrete per-Instance monos during its walk. There's a parallel problem the facade handles where this trick does NOT work: producing toylang-LLVM-IR for **internal consumer callees** (consumer functions called only by other consumer functions, invisible to rustc).

### Worked example

```toylang
// Consumer library
fn wrap<T>(x: T) -> Wrapper<T> {
    let v = Vec::new<T, Global>();
    internal_helper<T>(v, x)
}

fn internal_helper<T>(v: Vec<T, Global>, x: T) -> Wrapper<T> {
    // ... implementation body ...
}
```

```rust
// Rust code calling into the consumer
fn main() {
    let w: Wrapper<i32> = toylang_lib::wrap::<i32>(42);
}
```

For this compile to produce a working binary, three chunks of machine code must exist:

1. **`wrap<i32>`** — consumer entry point. Rust calls it; rustc sees the declaration in `__lang_stubs.rs`. Toylang's LLVM backend emits the body.
2. **`Vec::<i32, Global>::new`** — Rust stdlib, monomorphized with `T=i32`. Rustc emits it.
3. **`internal_helper<i32>`** — consumer-internal, called only by `wrap`. Rustc has no idea it exists (it isn't in `__lang_stubs.rs`). Toylang's LLVM backend emits the body.

### How each item gets produced

**(2) — the Rust dep, where Params work.**
Under Approach B, `optimized_mir(wrap_def_id)` fires once. The walker returns `[(Vec::new_DefId, [Param(T, 0), Global])]` — symbolic. Rustc's collector walks that body with `args=[i32]`, substitutes `Param(T, 0) → i32`, queues `Vec::<i32, Global>::new` for codegen. Perfect — rustc did the per-Instance concretion via its standard substitution engine.

**(3) — the internal callee, where Params do NOT work.**
Toylang has to produce machine code for `internal_helper<i32>`. Could we try the same Param-deferral trick — stash a Param-typed `internal_helper<Param(T)>` ToyFunction and defer substitution? No. Two reasons:

- **Toylang's LLVM backend has no `ty::TyKind::Param`.** LLVM IR is concrete by construction; there is no `T` type in LLVM. To emit IR for `internal_helper`, we need to know what type each `T`-typed local actually is. We can't "emit Params now and substitute later" — there's no "Params" representation to emit.
- **No substitution engine exists on our side.** Rustc has `EarlyBinder::instantiate` to substitute Params → concrete before codegen. Toylang doesn't have an analog. Nobody downstream will do it for us, because rustc never sees `internal_helper` — it isn't in `__lang_stubs.rs`, isn't in any Instance rustc is monomorphizing, isn't in any `optimized_mir` call. Without a substitutor, deferred Params are deferred forever.

So toylang must substitute `T=i32` into `internal_helper`'s ToyFunction body itself, producing a fully concrete function (every `T` replaced with `i32`) BEFORE handing it to Inkwell. That's the job of `notify_concrete_entry_point` (Instance-keyed, per stage-1 callback split): fires when rustc queries `symbol_name(wrap<i32>_instance)`, walks the consumer call tree with `args=[i32]`, substitutes `T=i32` through `internal_helper`'s body, stashes the substituted ToyFunction in `state.toylang_instances`. Later, `generate_and_compile` iterates that list and emits LLVM IR for each already-concrete function.

### The principle

**Param-deferral requires a downstream substitutor.** Rustc has one; toylang's LLVM backend doesn't.

So whether Params are viable depends on where the output flows:

- **Rust-dep output → consumed by rustc's collector → Params OK** (rustc substitutes). This is where the "per-Instance output from per-DefId computation" insight applies.
- **Toylang-internal-callee output → consumed by toylang's LLVM backend → must be concrete** (nothing substitutes for us).

This asymmetry is why stage 1's callback split separated `collect_generic_rust_deps` (DefId-keyed, Param-typed output — fine, rustc substitutes) from `notify_concrete_entry_point` (Instance-keyed, concrete output — required, toylang consumes directly). It's also why Approach A vs B only changes the Rust-dep callback's shape; the internal-callee callback stays Instance-keyed under both approaches regardless.

---

## Why we migrated

Three reasons drove the stage-3 migration from A to B:

1. **Fork reduction.** Approach A requires 3 custom patches (1, 2, 4) defining and plumbing the custom query. Approach B uses the sanctioned `Config::override_queries` hook and drops all three. The project's long-term roadmap is toward zero rustc fork patches (see `rustc-fork-design-space.md` §5); B is a step along that path, A was a dead end.

2. **Correctness via reuse.** Approach A's walker has to carry substitution logic that mirrors — but doesn't reuse — rustc's internal substitution engine. Every rustc bump risks subtle drift. Approach B delegates substitution to rustc's own engine. If our walker produces well-formed `ty::TyKind::Param` nodes (which `GenericArgs::identity_for_item` does by construction), the substitution is guaranteed correct by rustc's standard machinery.

3. **Efficiency.** Per-DefId callback invocations scale better than per-Instance for generic-heavy consumer code. Not a dominant factor for current toylang workloads (tests run in 30–45s), but a cleaner asymptotic.

Trade-offs the other direction (arguments FOR A, considered and rejected):

- **A's callback signature is more direct** — concrete args are right there, no need to reason about Params flowing through the collector's substitution. Legitimate but stylistic; the correctness reasoning about B is localized and tractable.
- **A preserves today's walker structure** — migrating to B required walker rework (identity args, Param-preserving substitution). One-time cost; doesn't recur.

On balance: B is strictly better for a project planning to reduce its rustc fork. If erw were committed to its fork long-term (not the case), A would be a reasonable alternative.

---

## See also

- `docs/reasoning/why-interleaved-monomorphization.md` — the foundational argument for why dep discovery must happen during monomorphization at all. Both approaches in this document satisfy that constraint.
- `docs/reasoning/rustc-fork-design-space.md` §4.1 — the stage-3 migration from A to B, treated as a fork-reduction opportunity. Has the cost-accounting that made B the winner.
- `docs/architecture/rust-interop-guide.md` §2.2 — the shipping `optimized_mir` override architecture (Approach B).
- Stage-1 handoff (callback split) in `docs/historical/` — the precursor refactor that split the unified callback into the `collect_generic_rust_deps` + `notify_concrete_entry_point` pair, enabling the stage-3 migration.
- POC #1 at branch `poc/optimized-mir-override`, findings.md — the prototype that first verified Approach B works end-to-end.
