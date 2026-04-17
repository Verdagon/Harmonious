Hey,

Good writeup. Spent a couple hours digging through the actual code + every historical doc + the fork patches to answer your questions honestly. The short version is: **most of your alternatives are more viable than the architecture guide's framing suggests, because several weren't explicitly considered when the current design was picked.** Long version below, with the six questions answered in order.

---

## Before I answer your six questions: the architectural principle to separate from the implementation

One framing point up front, because it reshapes how to read everything below.

**The load-bearing thing is interleaving with rustc's monomorphization phase, not the specific `per_instance_mir` query we added to do it.** Rustc's collector is the one entity that walks Rust source. When consumer code calls a Rust generic, that call edge lives inside consumer source, which rustc never parses. Something has to inject the consumer's concrete type-argument tuples into the collector's walk so that rustc's own trait resolution and generic substitution machinery can discover everything transitively reachable. The facade's job is narrow — tell rustc the leaves, let rustc walk the rest — and doing that job requires firing per-Instance during monomorphization. Any fork-reduction path has to preserve this interleaving, or it loses the capability.

**What this means for your investigation specifically:** both of your proposed alternatives do preserve interleaving, which is why they're genuinely viable rather than sleight-of-hand. `override_queries` on `optimized_mir` fires during monomorphization (the whole point of `optimized_mir` is that rustc's collector calls it while walking); a `CodegenBackend` plugin alone doesn't (CGUs arrive post-walk), which is why plugin-alone fails, but plugin *paired with* an `override_queries` installation covers both monomorphization-time injection (query side) and post-monomorphization emission control (plugin side). Interleaving is the invariant; which specific rustc hook implements it is the implementation decision, and your proposed swap is valid because it preserves the invariant while replacing the mechanism.

The full taxonomy of when interleaving is required — a seven-case analysis covering which consumer architectures pre-pass can handle, which require interleaving, and why — is in `docs/reasoning/why-interleaved-monomorphization.md` in the erw repo. It explicitly names Vale's planned interop model as an example of the cases that require interleaving (Vale types participating in Rust trait systems, Vale closures flowing into Rust generic APIs, both of which land in Cases 4 and 6 of that doc's taxonomy). That doc is the canonical statement of *why* this architecture exists; the response you're reading is specifically about fork-reduction tactics that preserve the architecture. If you haven't read the taxonomy yet, it's worth the ~20 minutes before you commit to a specific zero-fork path — both for validating the invariant and for confirming your planned interop genuinely sits where the doc predicts it does.

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

**POC update (2026-04-16).** Built and ran a scaffolding-grade prototype of Alternative A against the erw repo on branch `poc/optimized-mir-override` (commits `b425094` → `1ee3800` → `119d287` → `e597fdd`). Env-var gated via `ERW_POC_OPTIMIZED_MIR=1`; gate-off path is a no-op. Confirmed predictions:

- **Dep discovery works end-to-end.** Collector substituted per-caller Params into our generic body; the `declare` lines for transitively-reachable Rust items (e.g. `stdout`, `Uuid::new_v4`, `<Stdout as Write>::write_all` for the uuid standalone) showed up in rustc's emitted LLVM IR. Every case today's `per_instance_mir` handles, including the trait-generic-callback shape, carried over.
- **Link fails with duplicate symbols.** 257 linker errors across 117 failing integration tests + 6 failing standalone tests, partitioned into 56 unique `__toylang_impl_*` collisions and 19 unique `__toylang_accessor_*` collisions. `__toylang_internal_*` symbols unaffected. Release + LTO doesn't help. Failure was uniform — not a grab-bag of different issues.
- **One second-order finding worth calling out.** The POC surfaced a facade-side wrinkle I hadn't anticipated: the consumer's current `monomorphize_fn` callback is Instance-keyed and does *two* jobs simultaneously — external Rust-dep registration AND internal-callee stashing into `state.toylang_instances`. A DefId-keyed `optimized_mir` override can drive job 1 (via generic ReifyFnPointer + collector substitution) but not job 2 (needs concrete `instance.args`, which a DefId-keyed context doesn't have). The natural redesign splits the callback trait across two queries that *already* exist and *already* fire at the right keying: `optimized_mir` drives `collect_generic_rust_deps(def_id)` (Param-typed), `symbol_name` drives `notify_concrete_entry_point(instance)` (concrete, does the recursive internal walk). No new rustc-exposed hook. Details in the reasoning doc's §4.1.

For Vale specifically this matters because I'd initially costed the zero-fork migration at 2–4 weeks; the POC surfaced the callback-boundary redesign as a line item that wasn't in the earlier number. Revised estimate: 4–8 weeks. Updated numbers below.

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

**Verdict: the `-Zcodegen-backend=valec` + `override_queries` combination is viable in principle**, with the `override_queries` half confirmed by the POC detailed in §1 and the plugin half still unbuilt. The honest framing is: the current architecture could have been built this way, and the only reason it wasn't is that when I started, the `rustc_driver::Callbacks` path was the documented one and I followed it — at that point, the query-override alternative also wasn't explicitly on my radar, so the combination never got framed as a possibility. The POC work this week closes the query-override-half unknown; the plugin integration stays as the one remaining architectural spike before this combination is fully de-risked.

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

**Cost I'd budget for the zero-fork target architecture (as you outlined), revised after the 2026-04-16 POC:**

- **`optimized_mir` override with trampoline bodies:** 1-2 weeks for the query-override plumbing itself. Confirmed straightforward — the POC hit no rustc-side walls in the override installation. The MIR construction code mostly carries over.
- **`LangCallbacks` trait job-split + consumer-side adoption:** 2-3 weeks. This is the line item my earlier estimate elided. The POC surfaced that the current Instance-keyed callback contract can't be driven from a DefId-keyed override without reshaping the trait — specifically, splitting `monomorphize_fn` into a DefId-keyed `collect_generic_rust_deps` (for external Rust-dep registration, consumed by the collector) and an Instance-keyed `notify_concrete_entry_point` (for internal-callee walking + stashing, keyed off `symbol_name`). Conceptual split, not a new rustc-exposed hook; consumer-side work of rewiring the callback implementations + verifying `state.toylang_instances` coverage under the split.
- **`-Zcodegen-backend=valec` + `rustc_codegen_llvm` delegation:** 2-3 weeks of architecture work. The associated-type visibility issue on `ModuleLlvm` is the one real wall — you'd either cap what you intercept or upstream a small rustc change (which isn't a fork but is contribution cost). Plugin integration also has to reconcile with the facade's current `rustc_driver::Callbacks` entry points. **Note: this line item also absorbs the separate-crate integration work** that would otherwise sit under patch-5 elimination — plugin mode subsumes the stub-rlib codegen-skip problem and the cross-crate consumer-DefId handling naturally, per POC #2 (§4.3 of the reasoning doc).
- **`#[linkage = "external"]` in generated stubs + `upstream_monomorphization` override:** ~5 LoC of query override on top of the plugin work. Effectively a rounding error once the plugin is in place. The feature-gate-at-crate-root mechanics are confirmed to work (POC #2 Step 3). For a greenfield consumer designing separate-crate from day one, this isn't a separate line item — it's absorbed into the plugin integration.
- **Migrating consumer read-surface to `rustc_public`:** 1-2 weeks. Pure win on churn reduction.
- **Risk contingency** — MIR construction API surface churning between rustc versions: budget ~1 week per 6-month bump regardless.

**Total rough estimate for zero-fork: 4-8 weeks engineering + ongoing 1 week per rustc bump.** (Revised up from 2-4 weeks after the POC surfaced the callback-boundary work.) Vs maintaining a 5-patch fork: ~2-3 days per rustc bump (patches are small and stable in shape).

The math only favors zero-fork if you're doing many rustc bumps per year and user installation friction is a real cost to you. If you're bumping twice a year and `rustup toolchain link vale-fork` is acceptable in your installer — the fork is probably still cheaper over a 2-year horizon. For a "just install valec" distribution story targeting non-Rust-native users, the calculus flips.

---

## Bottom line for Vale

1. **The principle to preserve is interleaving; the specific fork patches aren't load-bearing.** The architectural invariant (see `why-interleaved-monomorphization.md`) is that *some* rustc-side hook must fire per-Instance during monomorphization to supply consumer bodies — today that's `per_instance_mir`, but it could equivalently be `override_queries` on `optimized_mir` paired with a `CodegenBackend` plugin for emission control. Your proposed swap preserves the invariant, which is why it's viable. `override_queries` alone handles dep discovery (every case including your trait-generic-callback example); pairing with a plugin handles the emission-skip problem that dep discovery alone leaves behind. Either alone is insufficient — together they replace all five fork patches. **Confirmed by prototype** (branch `poc/optimized-mir-override`, see the reasoning doc's §4.1 for specifics).
2. **Your `#[linkage]` alternative (patch 5) works for a greenfield separate-crate architecture, and it's absorbed into the plugin work — not a separate line item.** POC #2 on branch `poc/separate-crate-stubs` prototyped this; the mechanical part works (feature-gate at a real crate root, `#[linkage = "external"]` on wrappers), but it surfaced that a toylang-brownfield retrofit hits architectural assumptions in the single-crate-compile backend that cost ~1-2 weeks to unwind. For Vale-greenfield with the plugin path, those assumptions don't exist from day one, so the integration cost drops to approximately zero + ~5 lines of `upstream_monomorphization` override. The rejection in the arch guide was specific to the single-crate-compile integration model, not universal. See `rustc-fork-design-space.md` §4.3 for the full brownfield/greenfield cost split.
3. **What I actually did vs what I claimed I did are different in a few places.** The architecture guide's framing — "the query is needed because of per-Instance keying" and "we rejected `#[linkage]`" — undersells the design space. I wrote that guide after the decisions were made, and "what we picked" drifted into "what was necessary" in the prose. I've updated the guide and written a dedicated reasoning doc (`docs/reasoning/rustc-fork-design-space.md`) with the honest accounting. Both POCs' findings are incorporated.
4. **The separate-crate stub model works mechanically; integration cost depends on your starting architecture.** POC #2 confirmed `#![feature(linkage)]` at a real crate root + `#[linkage = "external"]` on wrappers are sufficient for the CGU-partitioner-internalization problem (patch 5's purpose). The POC also surfaced three secondary blockers (`upstream_monomorphization` routing, `unreachable!()` body emission in the stub rlib, single-crate-compile assumptions in the backend's MonoItems walk) that bite a brownfield retrofit but dissolve under a greenfield plugin-mode design. For Vale, this means: if you're already doing plugin mode, add separate-crate for approximately free; if you're skipping plugin mode, add ~1-3 days of upfront design to handle cross-crate consumer DefIds in your backend.
5. **`rustc_public` helps with consumer code, not the framework.** Useful for Vale's Vale-facing oracle, not useful for the integration layer.
6. **If you go zero-fork, budget for MIR-construction churn + the callback-boundary job-split.** The POC surfaced that the current Instance-keyed callback contract can't be driven by a DefId-keyed override without splitting the trait across two queries (`optimized_mir` + `symbol_name`). This is why the estimate moved 2–4 → 4–8 weeks. Trait-level work, but no new rustc hooks needed — the job-split rides on queries that already fire at the right keying.

Order of operations I'd recommend if you commit to zero-fork:

1. **Replicate the POC on your side** or read through `poc/optimized-mir-override` branch in erw. The POC confirms (a) dep discovery works and (b) emission conflict is the one remaining blocker for pure `override_queries`. Rerun is `ERW_POC_OPTIMIZED_MIR=1 cargo +rustc-fork test -p toylangc`; findings.md on that branch has the full writeup.
2. **Build the `CodegenBackend` plugin integration** to resolve the emission problem. The `pub(crate)` issue on `ModuleLlvm` is the one place I'd expect to hit a real wall; worth a spike early. The reasoning doc's §4.2 has the specific known complications.
3. **Rework the `LangCallbacks` trait to the job-split shape** (`collect_generic_rust_deps` + `notify_concrete_entry_point`) before wiring the plugin. This is the facade-side change the POC surfaced — not a new rustc hook, just a trait reshape. Details in the reasoning doc's §4.1.
4. **The separate-crate stub model is absorbed into step 2's plugin work**, not a separate step. POC #2 confirmed that plugin mode subsumes the stub-rlib emission problem and cross-crate consumer DefId handling natively. Only additional cost is ~5 LoC of `upstream_monomorphization` override for generic-wrapper mono routing. See POC branch `poc/separate-crate-stubs` (commits `2463248` → `acc62d2` → `2c004a6`) for the full analysis.

The POC branch is at `poc/optimized-mir-override` on the erw repo; the env-var gate is `ERW_POC_OPTIMIZED_MIR=1`. Code is scaffolding-grade (~90-100 real code lines across 4 files in `rustc-lang-facade/src/`, or 137 insertions per `git diff --stat`; the remainder of the new file is doc-comment scaffolding). Gated so gate-off path is identical to current behavior. Not to be merged; kept as a reference for reproduction.

Two canonical references in the erw repo if you want to dig further:

- **`docs/reasoning/why-interleaved-monomorphization.md`** — the architectural rationale for interleaving with a seven-case taxonomy of consumer architectures. Read this first if you're deciding whether the facade is the right fit for your interop model.
- **`docs/reasoning/rustc-fork-design-space.md`** — takes "interleaving is required" as given and analyzes fork-reduction alternatives. Parts 2–4 cover `override_queries` on `optimized_mir`, `CodegenBackend` plugins, the separate-crate stub model, and `rustc_public` for churn reduction. Read this after committing to interleaving, when you're choosing the specific rustc hooks.

— e
