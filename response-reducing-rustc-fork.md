Hey,

Good writeup. Spent a couple hours digging through the actual code + every historical doc + the fork patches to answer your questions honestly. The short version is: **most of your alternatives are more viable than the architecture guide's framing suggests, because several weren't explicitly considered when the current design was picked.** Long version below, with the six questions answered in order.

---

## Before I answer your six questions: the load-bearing thing the architecture guide doesn't spell out

The guide describes *what* `per_instance_mir` does but not *why it has to fire during monomorphization specifically*. That's the piece that changes the framing of your whole investigation, so putting it here first.

**The core reason the facade interleaves with rustc's monomorphization phase is that monomorphization is where rustc is actively discovering transitively-reachable generic instantiations, and we need to inject toylang's concrete type-argument tuples into that walk.**

Concrete motivating shape. This is the one where the interleaving stops being optional:

```
toylang:  fn use_it<T>(x: T)               calls rust_fn<T>(x)
rust:     fn rust_fn<T: SomeTrait>(x: T) { x.some_method() }
toylang:  fn main() { use_it<i32>(42) }
```

At link time the binary needs:

1. Code for `rust_fn<i32>` — rustc must generate this.
2. Code for `<i32 as SomeTrait>::some_method` — rustc must generate this.
3. Toylang's `use_it<i32>` — toylang generates this, calls `rust_fn<i32>`.

For (1), rustc's mono collector needs to *discover* `rust_fn<i32>` is reachable. It walks from entry points outward via direct calls, `ReifyFnPointer` casts, etc. But `use_it<i32>` is toylang's — rustc never parses it, never walks it. Without intervention, the edge `use_it<i32> → rust_fn<i32>` is invisible, and the link fails with an undefined symbol.

For (2), once (1) happens, it's automatic. Rustc walks `rust_fn<i32>`'s body, sees `x.some_method()`, resolves `<T as SomeTrait>::some_method` with `T=i32`, queues `<i32 as SomeTrait>::some_method` for codegen. This is rustc's normal generic-trait resolution — it just works, as long as rustc knows about the caller.

So the handoff is **one-way and minimal**: toylang tells rustc the *leaves* (specifically, the Rust items directly called from toylang with concrete type args), rustc walks everything transitively reachable from there. That's all `per_instance_mir` + the `ReifyFnPointer` statements do — each toylang instance's synthetic MIR body enumerates "here are the Rust items I need for this concrete instantiation", and the collector picks up from there.

**The alternative rules itself out once you try to do it.** To discover `<i32 as SomeTrait>::some_method` without rustc's collector, toylang would have to:

- Walk Rust MIR itself
- Resolve trait impl candidates (the `for_each_relevant_impl` + `Instance::expect_resolve` machinery)
- Handle associated-type projections + normalization
- Handle default trait methods, blanket impls, conditional impls, specialization
- Re-derive generic arg substitution across the whole transitive call graph

That's tens of thousands of lines of rustc internals reimplemented. The interleaving isn't a coordination between two monomorphizers — it's a way to **not** reimplement rustc's trait/generic machinery. We defer to rustc for all generic resolution; we only supply the concrete type tuples it can't see otherwise.

Why this changes the framing of your investigation:

- **It reinforces Alternative A (the `optimized_mir` override).** `optimized_mir` also fires during monomorphization, also per-Instance. The interleaving requirement doesn't push you away from that path — it just rules out pre-pass or post-pass alternatives, of which there are none among your proposals.
- **It's the real objection to the pure CodegenBackend plugin route.** A `-Zcodegen-backend=foo` plugin receives CGUs post-monomorphization. At that point rustc has already walked the reachable set; if `use_it<i32> → rust_fn<i32>` was invisible during that walk, `rust_fn<i32>` isn't in the CGU list, and you can't codegen it from the backend no matter how clever you are. The plugin-only route is structurally incompatible with this model. **But** — and this is the thing worth underscoring — being a CodegenBackend *and* installing query overrides via `Config::override_queries` lets you have both: the emit-time control of a backend plus the monomorphization-time injection of a query provider. That combination, not either alone, is the zero-fork target.
- **It's silent in the current arch guide and it shouldn't be.** "Why the interleaving" is the answer I'd want if I were you, and it's not spelled out. I'll add a subsection capturing this after I hear back from you on which direction you're taking; what I write will depend on whether you're going fork-reduction or pure-plugin.

The subtler cases this mechanism also handles, in case they matter to Vale's roadmap:

- **Nested generics**: toylang calls `Vec::extend<i32, Global, I>(v, iter)` with I concrete (e.g., `Range<i32>`) — rustc walks from there into `<Range<i32> as IntoIterator>::into_iter` and all the downstream trait chain.
- **Type args introduced by Rust types that cross the boundary**: toylang holds a `HashMap<K, V>` where K/V become concrete at instantiation; rustc walks `<HashMap<K,V> as _>::method` for everything anyone calls on that HashMap.
- **Drop glue**: the `mir_shims` provider constructs `InstanceKind::DropGlue(_, Some(Vec<i32>))` so rustc walks the destructor chain for concrete instantiated types.

Every one of these is "toylang supplies concrete type args, rustc walks from there." If Vale types ever need to participate in Rust generic machinery on the callee side (which your framing implies they will, once Vale-in-Rust lands and you want Vale closures flowing into Rust generic APIs), this interleaving is how you get that for free.

---

## 1. Was `override_queries` on `optimized_mir` considered?

**No — not in any documented design discussion.** I went through every historical doc, commit message, plan file, and reasoning doc in `docs/`. Zero mentions of `override_queries` or `optimized_mir` as the hook for what became `per_instance_mir`. The new query was picked pragmatically; no alternatives analysis exists.

Technical assessment of whether your trampoline-body workaround would work, with one correction to frame it accurately:

- **Keying is more nuanced than I first wrote.** `optimized_mir` is **DefId-keyed** (not Instance-keyed, as I said in an earlier draft — that was wrong). But: when the collector consumes `optimized_mir(def_id)`, it substitutes the Instance's type args into the body *during its walk*, using the same substitution machinery it applies to every other generic Rust function. So the *effect* is per-Instance even though the query itself is DefId-keyed. We'd return a generic body once per DefId; rustc would substitute per-Instance while walking it. That's actually *simpler* than our current approach, where we pre-substitute in `per_instance_mir` ourselves.
- **What the current per_instance_mir body carries.** I looked at `rustc-lang-facade/src/queries/per_instance.rs:106-174`. The body's shape is identical across instances of the same generic function; only the substituted signature and the list of `ReifyFnPointer` statements vary per-Instance (because we pre-substitute). Under `optimized_mir` override, we'd return the unsubstituted versions and let rustc's collector substitute `T → i32` etc. during the walk.
- **Your trampoline approach reproduces the critical behavior for dep discovery.** A generic body containing `ReifyFnPointer(Vec::<T, Global>::new)` etc. is walked by the collector with `T` bound to whatever the caller's Instance says. Every case works — trait-generic callbacks, nested generics, consumer types flowing into Rust generics, drop glue. This is literally rustc's own substitution machinery doing the work.

**What doesn't go away with `override_queries` alone:**

Patches 1, 2, 4 all evaporate — they're plumbing for "we added a new query," and there's no new query to plumb.

Patch 3 is the problem. I thought it was a "you could live without it, just costs a few bytes per instance" patch, and I was wrong. Here's why:

Rustc compiles whatever body `optimized_mir` returns, at whatever symbol `symbol_name` assigns. If `symbol_name` says `use_it::<i32>`'s symbol is `__toylang_impl_use_it__i32`, rustc emits the compiled trampoline body at that symbol. The consumer's backend emits its *real* implementation at the same symbol. Duplicate definition at link time.

I tried to find a formulation where rustc's trampoline and the consumer's real impl live at different symbols, with some redirect making calls flow through the trampoline to the real impl. Each formulation collapses back to the same conflict, because the trampoline's tail has to reference *something* whose symbol becomes the consumer's impl, and that something is the impl. The redirect problem is isomorphic to the original conflict.

The clean resolutions:

1. **Keep a single fork patch** that teaches rustc's codegen to skip the stub body (equivalent to current patch 3). Gets you from 5 patches to 1, not 0.
2. **Pair `override_queries` with a `CodegenBackend` plugin.** The plugin *is* rustc's codegen for this compilation; it decides what to emit. It declines to emit the stub body while separately emitting the consumer's real impl. No conflict because only one emission happens. This is the combination I'll detail in question 2.
3. **Abandon the "consumer emits via its own LLVM backend" architecture** and have the consumer produce MIR that rustc compiles. Huge architectural shift — probably not what you want.

**Revised verdict on Alternative A.** The override handles dep discovery perfectly (every case today's `per_instance_mir` handles, including the trait-generic-callback motivating example). It does *not* handle emission skip on its own. `override_queries` alone is a 3-patch reduction (5 → 1). Zero fork requires pairing with the backend plugin.

What pushed toward a new query specifically, if I'm being honest: `optimized_mir` came with a vague sense of "we'd be intercepting rustc's normal MIR pipeline for existing DefIds, and that's scary," whereas a new query "owned" by the consumer felt cleaner. That's a taste preference, not a technical requirement. For your distribution constraints (zero-fork target), that taste is the wrong tradeoff — but you need the plugin in the mix too, not just the override.

## 2. What did the `CodegenBackend` route look like when evaluated?

**It wasn't evaluated as the integration mode** — and there's an important framing point here that connects to question 1.

A plugin alone doesn't work: it receives CGUs post-walk, so any edge from consumer code into a Rust generic that was invisible during the collector's walk is already lost. So the route was implicitly dismissed as "doesn't even address the primary problem." That dismissal is right *for a plugin-only approach*.

A plugin paired with `override_queries` is the actually-interesting design, and it's the one that gets you to zero fork patches. Here's why the combination specifically works:

- **`override_queries` on `optimized_mir`** provides the pre-codegen hook: it makes the collector see consumer functions' synthetic bodies (with `ReifyFnPointer` statements for Rust deps). This is what §1 covered. It handles dep discovery fully.
- **The plugin is what resolves the emission-skip problem** that §1 flagged as `override_queries`'s one remaining fork dependency. Where current patch 3 teaches *rustc's* codegen to skip the stub body, the plugin simply *is* rustc's codegen. The consumer decides what to emit. It declines to emit the stub body while separately emitting the consumer's real impl. No symbol conflict, because there's only one emission — and it's the one the consumer wants.

That's the full shape: query overrides cover the monomorphization-time injection that's invisible to a plugin alone; the plugin covers the emission control that's invisible to the query override alone. Either alone is insufficient; together they replace all five fork patches.

What the investigation found about the combination's implementation:

- **The facade's `codegen_wrapper.rs` is already a partial `CodegenBackend`.** It implements the trait, delegates everything to `LlvmCodegenBackend`, and intercepts `join_codegen` to inject the consumer's `.o`. Nothing in the docs maps out the "go full plugin via `-Zcodegen-backend=facade`" alternative. The seven approaches evaluated in `docs/historical/design-codegen-integration.md` all assume the wrapper lives *inside* rustc's driver flow — none ask "what if we're the backend rustc dispatches to?"
- **Delegating to `rustc_codegen_llvm` as a library is fragile.** `ModuleLlvm` and several associated types are `pub(crate)` in `rustc_codegen_llvm`. The current wrapper works because it delegates *everything*; selective interception (skip consumer stubs, emit Rust deps via LLVM, emit consumer real-impls via your own path) would require either upstream changes to expose those types or type-erasure hacks. This is the one real wall in this approach.
- **The facade's current `rustc_driver::Callbacks` integration** (`install_callbacks` at startup, `after_analysis` for validation) doesn't map directly to a backend plugin's lifecycle. The plugin would need to install callbacks from within its `init` or `codegen_crate` method, which interacts with rustc's Rayon-threaded compilation in non-obvious ways.

**Verdict: the `-Zcodegen-backend=valec` + `override_queries` combination is viable in principle**, but I haven't built it. The honest framing is: the current architecture could have been built this way, and the only reason it wasn't is that when I started, the `rustc_driver::Callbacks` path was the documented one and I followed it — at that point, the query-override alternative also wasn't explicitly on my radar, so the combination never got framed as a possibility.

## 3. `#[linkage = "external"]` — concrete failure beyond feature-flag propagation?

**It was tested briefly and reverted.** `docs/historical/phase6-attempt2-linkage-visibility.md` line 425: *"We tried this briefly and reverted because adding a feature gate to user-written Rust test files is brittle and user-visible."*

The technical mechanism *works*. I verified by reading the rustc fork's partitioning code: `explicit_linkage(tcx)` queries `tcx.codegen_fn_attrs(def_id).linkage`, which is set by `#[linkage = "external"]`. This check happens at `partitioning.rs:755-777` before the internalization candidate logic at line 254. Returning early completely bypasses internalization. Confirmed mechanically sound.

**The rejection is specific to toylang's architecture, not universal.** Toylang injects `__lang_stubs.rs` into the *user's test crate* via `FileLoader`. Feature flags are crate-root-scoped and there's no module-level escape. So every user test file would need `#![feature(linkage)]` at the top — unacceptable for a "general-purpose consumer compiler" that should accept arbitrary user Rust.

**Vale's distribution model likely doesn't have this constraint.** If your stubs live in their own generated crate (not injected into a user crate), `#![feature(linkage)]` stays in a file only your toolchain authors. Feature flag never reaches user-authored code. The toylang rejection doesn't transfer.

So: **no, there wasn't a hidden technical failure I'm not telling you about.** The rejection was integration-specific. For your setup, Alternative A is probably fine.

## 4. Is there rustc-internal context I'm missing?

One thing worth calling out, not a blocker but worth budgeting for: **constructing synthetic MIR bodies is harder than it looks.** `rustc-lang-facade/src/queries/per_instance.rs` + `mir_helpers.rs` together are ~250 lines of MIR construction — arena allocation, `set_required_consts`, `set_mentioned_items`, `ReifyFnPointer` cast shaping, source info / source scope boilerplate, `UnwindAction`, `CastKind`. Whatever path you pick (trampoline via `optimized_mir`, or full plugin), if you're synthesizing MIR you're spending time in this territory. The API is internal and churns — expect to eat ~1 person-week per 6-month rustc bump just on MIR construction surface diff.

This isn't an argument for the fork; it's an argument for **"whatever path you pick, the MIR-construction surface is your biggest churn vector."** `stable_mir`/`rustc_public` doesn't help here — it's read-oriented; construction APIs aren't exposed.

## 5. `stable_mir` readiness

Renamed to `rustc_public` mid-2025. Still nightly-only, still explicitly unstable ("This API is still completely unstable and subject to change" per the crate docs). Not on crates.io. My investigation of current surface vs what the facade uses:

**Covered (~40-50% of rustc-internal usage by LoC):**

- Type inspection (`Instance::ty()`, `TyKind`-equivalent, crate/item paths)
- Instance resolution (`Instance::resolve`, `mangled_name`, `body`)
- MIR *reading* (Body, BasicBlock, Terminator, Statement, Rvalue)
- `fn_abi` (via `Instance::fn_abi()`, covers `PassMode`/`FnAbi`/`ArgAbi`)
- Layout reading (`Layout`, `LayoutShape`, `TyAndLayout`, `Primitive`, `Scalar`)

**Not covered (the architecturally load-bearing pieces):**

- Custom query providers (`layout_of`, `symbol_name`, `mir_shims`, `per_instance_mir`) — no override hook
- MIR *construction* (arena alloc, set_required_consts, custom Body building) — read-oriented only
- CodegenBackend integration / wrapping
- `FileLoader` stub injection
- `VISIBILITY_OVERRIDE_HOOK` partitioner interaction
- `Config::override_queries` itself

**What this means for you:** `rustc_public` migrates the *consumer-side* code (toylang's `oracle.rs` and the ABI/symbol call sites in `llvm_gen.rs` — ~60% of the consumer's rustc-facing surface). It does **not** migrate the *framework* (the four query providers + codegen wrapper + visibility hook). The framework is the 5 patches; `rustc_public` doesn't touch the 5 patches.

If Vale's goal is "reduce churn exposure for the Vale-specific consumer code," `rustc_public` is worth adopting. If the goal is "eliminate the fork," it doesn't help.

## 6. Am I underestimating the cost of the trampoline / backend-plugin approaches?

**Cost I'd budget for the zero-fork target architecture (as you outlined):**

- **`-Zcodegen-backend=valec` + `rustc_codegen_llvm` delegation:** probably 2-4 weeks of architecture work if you're starting from the current wrapper. The associated-type visibility issue on `ModuleLlvm` is the one real wall — you'd either cap what you intercept or upstream a small rustc change (which isn't a fork but is contribution cost).
- **`optimized_mir` override with trampoline bodies:** maybe 1-2 weeks to build and validate. The MIR construction code mostly carries over.
- **`#[linkage = "external"]` in generated stubs:** a few hours, given your separate-crate architecture.
- **Migrating consumer read-surface to `rustc_public`:** 1-2 weeks. Pure win on churn reduction.
- **Risk contingency** — MIR construction API surface churning between rustc versions: budget ~1 week per 6-month bump regardless.

**Total rough estimate for zero-fork: 4-8 weeks engineering + ongoing 1 week per rustc bump.** Vs maintaining a 5-patch fork: ~2-3 days per rustc bump (patches are small and stable in shape).

The math only favors zero-fork if you're doing many rustc bumps per year and user installation friction is a real cost to you. If you're bumping twice a year and `rustup toolchain link vale-fork` is acceptable in your installer — the fork is probably still cheaper over a 2-year horizon. For a "just install valec" distribution story targeting non-Rust-native users, the calculus flips.

---

## Bottom line for Vale

1. **The fork is less load-bearing than I framed it, but the zero-fork target needs *two* pieces, not one.** `override_queries` on `optimized_mir` alone handles dep discovery completely — every case today's `per_instance_mir` handles, including your trait-generic-callback example. On its own, though, it can't replace fork patch 3 (codegen skip) because rustc will still compile whatever body the override returns and that collides with the consumer's real implementation at the linker. The combination `override_queries` + `-Zcodegen-backend=consumer` is what actually gets you to zero fork patches: the query covers pre-codegen injection, the plugin covers post-monomorphization emission control. Either alone is insufficient.
2. **Your `#[linkage]` alternative (patch 5) probably works for your architecture specifically.** Independent of the query/plugin discussion. The toylang rejection was integration-specific.
3. **What I actually did vs what I claimed I did are different in a few places.** The architecture guide's framing — "the query is needed because of per-Instance keying" and "we rejected `#[linkage]`" — undersells the design space. I wrote that guide after the decisions were made, and "what we picked" drifted into "what was necessary" in the prose. I've updated the guide and written a dedicated reasoning doc (`docs/reasoning/rustc-fork-design-space.md`) with the honest accounting.
4. **The genuinely unexplored alternative is the separate-crate stub model (your §Patch-5 B).** Nobody tried it. I think it works. If you're going zero-fork, it's independent of the query/plugin work and is likely the cheapest single win on patch 5.
5. **`rustc_public` helps with consumer code, not the framework.** Useful for Vale's Vale-facing oracle, not useful for the integration layer.
6. **If you go zero-fork, budget for MIR-construction churn.** That's the biggest recurring cost; neither the query override nor the plugin route escapes it, because both still synthesize MIR bodies using rustc-internal construction APIs.

Order of operations I'd recommend if you commit to zero-fork:

1. Prototype `override_queries` on `optimized_mir` on a toy reproducer. Verify dep discovery works (walk through a `use_it<i32> → rust_fn<i32: SomeTrait>` case) and confirm the emission conflict actually manifests. Low risk, cheap — mostly validates the claims in this document.
2. Build the `CodegenBackend` plugin integration to resolve the emission problem. The `pub(crate)` issue on `ModuleLlvm` is the one place I'd expect to hit a real wall; worth a spike early.
3. Separately, try the separate-crate stub model for patch 5. Orthogonal; can run in parallel with the above.

If it'd help, I can put ~4 hours into step 1 as a proof-of-concept inside erw — just to nail down whether there's a blocker I'm missing. Let me know.

— e
