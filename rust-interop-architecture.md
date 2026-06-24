# Sky: Compiler & Rust Interop Architecture

This is the master design document for Sky's compiler architecture as it relates to Rust interop. It is the product of an extended design conversation grounded in the prior work on `rustc-lang-facade` (the `erw` project, formerly `OnRust`, now `RustCompilerBridge`), and it locks in the architectural decisions Sky's first implementation should follow.

The architecture is deliberately opinionated. Where decisions have been made, this document states them as decisions; where alternatives were considered and rejected, the rejection is recorded with reasoning so a future reader can re-open the question with full context. The default reading posture is "Sky is committed to these decisions — change them only with deliberate cause."

---

## Documents This References

This document is self-contained at the architectural level; it does not require external references to understand any locked decision. Provenance lives in companion docs in the `erw` repository (renamed `RustCompilerBridge` on GitHub):

- **Inherited reasoning** (`docs/reasoning/`): `why-interleaved-monomorphization.md` (the 7-case taxonomy, §1.4/§2/§30), `dep-discovery-approaches.md` (Approach A vs B, §3.1/§30), `why-outer-params-suffice.md` (bounded-expressibility, §13.7.5), `architecture-decisions.md`, `rustc-fork-design-space.md`.
- **Inherited risk + architecture** (`docs/architecture/`): `risks.md` (the A/B/C taxonomy §25 inherits and B2 analysis §5.2/§6.6.5 references), `rust-interop-guide.md` (erw's shipping architecture).
- **Rustc background** (`docs/background/`): `rustc-extension-points.md` covers the hooks Sky uses (`Config::override_queries`, `CodegenBackend`, `Callbacks`).
- **Inherited arcana** (`docs/arcana/<HammerCaseTitle>-<ID>.md`): SMINCZ, GCMLZ, DPSFDOZ, ELASZ, ACRTFDZ, TCHAPZ, RTMEIZ, UTAIRZ, MBMRVZ, IVTDBTZ, TVIMDGAZ, ETASTZ — 12 of the §26 arcana are direct erw analogs. `@MRRIWMZ` (wrapper-mode manifest re-read) has no Sky analog (Sky uses in-process forked rustc). §26.7 (migratory/cancellable propagation) and §26.8 (Sky source = no Pin / `for<'a>` / Rust lifetime syntax) are Sky-specific.
- **External Rust docs (orientation only):** `rustc_interface` / `rustc_driver` / `rustc_codegen_ssa` rustdoc on doc.rust-lang.org/nightly/nightly-rustc; the `rustc_private` feature in the unstable book; rustc-dev-guide.rust-lang.org; llvm.org/docs.

For navigation: `docs/architecture/rust-interop-guide.md` Part 8 indexes erw arcana, or grep `@<ID>` in erw source for cross-references at the code site.

---

## 0. Document Meta

**Audience.** Architects (top-to-bottom), implementers (the chapters relevant to their subsystem + cross-refs at chapter ends), reviewers (governing chapters for the touched subsystem; highest risk are §5.2, §5.3, §6.6, §19.5, §26.x where invariant violations can pass tests silently), and Sky users (§1, §12–17, §21–22 plus brief worked examples in chapter bodies; companion `docs/usage/` will be the canonical user reference).

**Scope.** The Rust interop story — how Sky compiles, cooperates with rustc, exposes items to Rust callers, consumes Rust libraries, and projects Sky source-level concepts (groups, linear types, comptime, async) onto the Rust ABI. NOT in scope: Sky's frontend internals, runtime (executor/allocator/channels), or stdlib design — those have their own documents. Non-interop concerns appear only enough to characterize the interop boundary.

**Decision status.** Three categories appear inline:
- **Locked** — imperative phrasing ("Sky uses X"); deviating requires the user to sign off on re-opening the section.
- **Recommended** — "Sky should use X" / "we recommend X"; not yet formally locked.
- **Open** — design space catalogued, decision deferred; tagged `[OPEN]` and enumerated in §29.

Most of the doc is locked or recommended. Document version: **0.1.0** (initial draft, pre-implementation); moves to 1.0.0 when Sky's first implementation lands, after which re-opening a locked decision requires a written deviation note. Section-level version annotations may appear in headings where a decision has materially shifted.

**Reading paths.** Full read ~3–5 hrs; architect path: §§1–7, 19, 20, 25, 28, 29 (~2–3 hrs); implementer path: target subsystem plus its cross-refs; Sky-user path: §1, §§12–17, §§21–22; code-review path: jump to the governing chapter for the touched subsystem.

---

## 1. Goals and Constraints

This chapter records what Sky is trying to be and the non-negotiable design constraints that shape every subsequent decision. The constraints in this chapter are not negotiable within the scope of this document — re-opening any constraint would require re-opening the entire architecture.

### 1.1 What Sky is, in one sentence

Sky is a memory-safe systems language with first-class compile-time metaprogramming and a deeply integrated relationship to the Rust ecosystem, intended for greenfield projects whose authors want stronger safety guarantees than Rust provides while keeping access to the crates Rust users already depend on.

That single sentence packs several decisions. Unpacking:

- **Memory-safe.** Sky enforces memory safety statically. The mechanism is groups + linear types, both Sky-native concepts. Sky's safety model is strictly more expressive than Rust's borrow checker for the use cases Sky's authors care about (region-style ownership across nested borrows, linear resources that the compiler refuses to drop silently), at the cost of being unable to express certain things Rust users take for granted (cancellation by drop, fearless reuse of borrowed data through `Rc<RefCell<T>>`-style runtime checks).
- **Systems language.** Sky compiles ahead-of-time to native code through LLVM. There is no GC, no managed runtime, no JIT. Performance characteristics are intended to be comparable to Rust and C++. The language is suitable for OS kernels, embedded software, performance-critical servers, simulation engines, game engines, and any domain where C, C++, or Rust would be the natural choice.
- **First-class compile-time metaprogramming.** Sky's comptime is Zig-style — the same expression language runs at compile time as runs at runtime, with the comptime evaluator simulating a "RAM-like slab" so that arbitrary Sky values (including new type definitions, function definitions, and runtime values) can be constructed and consumed during typechecking. This is the load-bearing differentiator from Rust's `const fn` story, and it is also the technical reason Sky must fork rustc rather than building zero-fork on top of vanilla rustc (see Section 3).
- **Deeply integrated relationship to the Rust ecosystem.** Sky source can directly import and use Rust crates. Rust source can directly use Sky-defined items (within constraints documented below). The integration is bidirectional, both calling-direction directions and both ecosystem-direction directions (Sky publishing to crates.io and Sky consuming from crates.io). The integration is not "interop in the sense of `extern "C"`" — it is "interop in the sense that Sky's typechecker has direct visibility into Rust signatures via rustc's `TyCtxt` and Sky-defined items are first-class Rust types from a calling perspective."
- **Greenfield.** Sky is not intended as a Rust replacement for existing Rust projects to migrate to. The interop story is rich, but the linear-type/group system is not a drop-in replacement for Rust's borrowing — porting non-trivial Rust code to Sky will involve real rewrites. Sky is for *new* projects that want Sky's properties from day one and want to leverage the Rust ecosystem as a library substrate.
- **Stronger safety than Rust.** The selling point. Sky's groups give region-style memory safety without the per-borrow lifetime annotation burden. Sky's linear types prevent silent drops of resources whose deallocation order matters (file handles, network connections, transactions). Sky's typechecker enforces invariants Rust's typechecker can express only at runtime (e.g., "this resource has been consumed by exactly one consumer"). The trade-off is that Sky's safety model rejects some patterns Rust accepts; the bet is that those patterns are not the patterns users want, but the loss is real and worth being explicit about.

The "in one sentence" version of Sky's goals is intentionally compressed. The remaining subsections expand each component and pin the implications for the interop story.

### 1.2 Memory model: groups, linear types, slab-based comptime

This subsection sketches Sky's source-level memory model only as much as the interop architecture needs. The full design lives elsewhere; what follows is the minimum required for a reader of this document to understand why the interop story has the shape it does.

**Groups.** A group is a named memory region — a contiguous address space within which a set of related objects live. Groups are explicit in Sky source: a function declaration like `fn process<G>(x: &G T)` says "process takes a reference to a `T` that lives in group `G`." The group annotation is Sky's analog to Rust's lifetime annotation, but it carries more information: a group is a runtime construct (Sky's allocator manages group regions and frees them as a unit when the group ends), and Sky's typechecker tracks which references belong to which groups.

Two key differences from Rust's lifetime model:

1. **Groups are runtime-realized.** Sky's allocator implements groups as bump-allocated arenas (or similar region-based allocation strategies). Freeing a group frees every object inside it in a single operation. Rust's lifetimes are purely a compile-time construct with no runtime existence; Sky's groups have both compile-time and runtime existence.
2. **Groups can be nested explicitly.** Sky source can declare that group `G1` lives inside group `G2`, expressing region containment. References can be promoted from `&G1 T` to `&G2 T` when `G1 ⊂ G2`. This is more expressive than Rust's `'long: 'short` outlives bounds because Sky tracks the containment structure, not just the outlives relation.

For interop purposes, groups erase to Rust's `'re_erased` lifetime at the rustc boundary. The mechanism is identical to erw's `@ELASZ` pattern: Sky's frontend generates `GenericArgs` for Rust items by populating each lifetime slot with `tcx.lifetimes.re_erased`. From rustc's view, every borrow Sky produces appears with an erased lifetime, post-borrowck-style. Sky's typechecker has already proven the borrow valid Sky-side; rustc trusts the erasure.

The group-to-lifetime mapping is asymmetric: Sky's groups carry information rustc lifetimes don't (containment, runtime regional existence), but the erasure pattern projects Sky groups onto a single Rust lifetime kind (`re_erased`) at the boundary. Sky's frontend reconciles any Rust lifetime constraints on a Rust API with Sky's group-level constraints; this reconciliation happens during stub generation and during typechecking. Section 11 covers the boundary in full.

**Linear types.** A linear type is a type whose values cannot be silently dropped — they must be explicitly consumed by either being returned, passed to a consumer function, or destructured. Sky's typechecker enforces this at compile time. A linear file handle, for example, cannot be left at end-of-scope without an explicit `close()` call; the typechecker will reject any program that allows the linear value to escape consumption.

Linearity is a property of the type, not of the value or the binding. Some Sky types are linear, some are affine (Rust-style — droppable, but consumed when moved), and some are unrestricted (`Copy`-equivalent). The distinction is declared at type-definition time.

For interop purposes, linear Sky types pose a problem: rustc has no concept of linearity. When a Sky linear type appears in Rust source — perhaps because the Sky type is passed to a Rust generic and the Rust code stores it in a `Vec<T>` — rustc may decide to drop it. Sky's typechecker can't prevent this; the type has crossed into rustc's domain.

Sky's solution: every linear Sky type's source-level `impl Drop` body calls `sky_runtime_panic("Sky linear type X was dropped from Rust")` followed by `abort()`. The compiler's AST-rewrite pass synthesizes `Drop::drop(&local)` calls at scope-end positions for any let-binding whose type has a Drop impl; rustc's auto-generated `drop_in_place` chain reaches the user's panic-and-abort body via the same path it reaches any other Sky-emitted trait impl method. The program terminates with a clear diagnostic. Sky's user has not violated linearity from inside Sky source; Rust has violated it, and Sky responds by killing the program before further damage. Section 15 covers the drop story; the AST-rewrite mechanism is §15.7 in detail.

**Slab-based comptime.** Sky's comptime is Zig-style: the same expression language runs at compile time as runs at runtime. A `comptime` block evaluates immediately at the surrounding compile point; a `comptime` parameter is bound to a specific compile-time value that becomes a property of the type. Sky's comptime evaluator implements this by simulating a "RAM-like slab" — an in-memory byte buffer with allocator services — that holds comptime-constructed values. Comptime values are referenced by their slab address (a `usize`-typed offset).

This is the load-bearing technical detail for Sky's interop with rustc. Consider:

```sky
fn zork<const T: Spaceship>() { ... }

let s = comptime Spaceship::new()  // s is at slab offset 0x1220
zork::<s>()
```

When Sky compiles `zork::<s>`, the comptime argument `s` is bound to slab offset `0x1220`. From rustc's view, the const generic argument is just `0x1220` — a `usize`. Rustc has no representation for `Spaceship` (it's a Sky type), so rustc cannot substitute a `ConstKind::Param<Spaceship>` with the actual Spaceship value. But rustc can substitute a `ConstKind::Param<usize>` with `0x1220`, because `usize` is in rustc's const-generic universe.

So Sky's comptime values, when they need to cross into rustc-visible territory, do so as integer slab addresses. The actual Spaceship at offset `0x1220` lives in Sky's slab, which is per-rustc-invocation state in Sky's frontend (which lives inside our forked rustc, per Section 4). Sky's `layout_of` override, when asked about `zork::<0x1220>`, dereferences the slab pointer in Sky's universe to recover the actual Spaceship value, and then evaluates Sky's comptime-machinery to produce the layout.

The slab is per-rustc-invocation. It is created when Sky's machinery activates (after sidecar load); populated during typechecking and during per_instance_mir queries; discarded when the invocation ends. The slab is never serialized to disk. Comptime results that need to persist across invocations are baked into the Temputs (sidecar) in their resolved form, not as slab references. Section 13 covers comptime in full.

**Why this matters for the interop architecture.** Sky's comptime allows arbitrary-typed const generic parameters. Rust's const-generic machinery does not. So Sky's per_instance_mir provider must do its own substitution — it cannot rely on rustc's collector substituting Sky's comptime args for it, because rustc's substitution engine has no `ConstKind::Param` semantics for arbitrary Sky types. This forces Sky into Approach A (Instance-keyed dep discovery, Sky substitutes), where erw was able to use Approach B (DefId-keyed, rustc substitutes). The slab-pointer-as-usize trick relaxes this constraint *somewhat* (because the integer surface is rustc-compatible), but only partially — Sky's per_instance_mir still needs to evaluate Sky-side comptime when generating the substituted body, and that evaluation needs the Instance's args concretely. Section 19 covers this in full.

### 1.3 Rust ecosystem integration as a first-class concern

Sky is designed from day one to consume Rust crates and to be consumable from Rust. This is not retrofitted; it is in the language's bones. Sky source's import statements directly name Rust crate paths (`import rust.std.vec.Vec`); Sky's typechecker queries rustc's `TyCtxt` directly for Rust signatures during typechecking; Sky's codegen emits LLVM IR that interoperates with rustc-emitted LLVM IR at the symbol level; Sky's build system orchestrates cargo as a subprocess.

The implications are pervasive. Among them:

- Sky cannot define a type system that rustc cannot represent at all. Every Sky type that crosses into Rust-visible territory must be expressible as some rustc-known type. The mechanism is opaque stub structs (Section 10) where Sky owns the layout via the `layout_of` query override; from rustc's view, an exported Sky type is an opaque sized blob. Sky's internal type structure is invisible to rustc.
- Sky cannot have a calling convention that rustc cannot match. Sky must produce code that conforms to the Rust ABI at every cross-language call site. This means Sky's codegen must compute the same `FnAbi` rustc does for the same signature, applying the same coercions (ScalarPair splits, sret returns, hidden track_caller parameters, etc.). Sky inherits erw's ABI helpers; see `@ACRTFDZ` and `@TCHAPZ`.
- Sky cannot ignore rustc's monomorphization model. When Sky source calls a Rust generic function, the concrete Rust monomorphization must exist in the final binary. Sky's codegen does not produce the Rust monomorphization itself (it has no idea how to compile Rust source); rustc does. Sky's job is to *tell rustc* which Rust monomorphizations Sky needs. This is the dep-discovery problem, covered in Section 19.
- Sky cannot have a lifetime model that surfaces incompatible information to rustc. The group erasure to `re_erased` is the mechanism (Section 11). It is asymmetric — Sky understands more than rustc sees — but Sky's typechecker enforces correctness Sky-side, so the asymmetry does not produce unsafety.
- Sky cannot have an error-handling model that surfaces incompatible behavior to rustc. Sky uses `panic = "abort"` exclusively (Section 16). Unwinding across the boundary is forbidden; foreign exceptions are UB. This is a real constraint on what Rust APIs Sky can comfortably consume; some Rust APIs assume unwinding semantics and won't function correctly under abort.
- Sky cannot have a drop model that produces silent rustc-visible misbehavior. Sky's linearity must be preserved across the boundary; rustc-emitted drop glue for Sky linear types panics rather than silently leaking the linearity-violating program. Section 15 covers the drop story.
- Sky cannot have an async/concurrency model that produces silent rustc-visible misbehavior. Sky's futures are exposed to Rust as types implementing `std::future::Future`; Sky's source-level discipline ensures that the exposed surface satisfies whatever bounds Rust callers need (or rejects the API at Sky's typecheck time). Sections 14, 15, 17 cover the async story.

All of these are constraints on Sky's design that follow from "Sky integrates deeply with the Rust ecosystem." They are real. Sky pays a price for the integration: its design space is more constrained than a hypothetical "pure" Sky design that ignored Rust. The price is judged worthwhile because the Rust ecosystem is enormous and growing, and a Sky that could not access it would be relegated to research-language status.

### 1.4 Bidirectional interop (cases 1b/3/4/5/6 from the seven-case taxonomy)

The erw project (`docs/reasoning/why-interleaved-monomorphization.md`) enumerated seven architectural cases for consumer-language Rust interop. Sky's interop story is bidirectional and covers cases 1b, 3, 4, 5, and 6 of that taxonomy. The taxonomy is summarized below; Section 2 walks through it in detail with Sky-flavored examples.

| Case | Top-level | Middle | Bottom | Sky relevant? |
|------|-----------|--------|--------|---------------|
| 1a   | Rust      | —      | Sky (non-generic only) | Yes |
| 1b   | Rust      | —      | Sky    | Yes |
| 2    | Sky       | —      | Rust   | Yes |
| 3    | Rust      | Sky    | Rust (same top) | Yes |
| 4    | Sky       | Rust   | Sky (same top) | Yes |
| 5    | Rust      | Sky    | different Rust | Yes |
| 6    | Sky       | Rust   | different Sky | Yes |

Every case is in scope. Sky source can be the top-level program (Sky calls Rust); Rust source can be the top-level program (Rust calls Sky); intermediate layers in either language are supported; Sky types can flow into Rust generics; Rust types can be passed as type arguments to Sky generics; Sky types can implement Rust traits; Rust types can implement Sky traits (within orphan-rule constraints, Section 6.6).

The taxonomy's hard-case section (cases 1b, 3, 4, 5, 6) is where pre-pass dep enumeration fails and interleaved monomorphization is required. Sky inherits this constraint from the underlying compilation model. Section 2 spells out the worked examples; this subsection just records that Sky is fully in the interleaving-required regime.

The closure-extension cases (Sky closures passed to Rust APIs taking `Fn`, Sky state machines exposed as `Future` impls, Sky impls of Rust traits with HRTB bounds) are also in scope but are addressed in Sections 11 (HRTBs), 14 (closures and async), and 5 (trait orphan rule) rather than as separate taxonomy entries.

### 1.5 Long-term correctness over short-term simplicity

The decision posture for this document is: when a design choice trades implementation complexity for long-term correctness or future flexibility, the trade favors long-term correctness. The Sky author has been explicit about this — "I'm rarely convinced by 'it will take too long.'"

Concretely, this affects design decisions in three patterns:

1. **Avoid baking publish-time decisions into shipped artifacts.** When two designs are available and one bakes a publisher-time decision into the shipped artifact (e.g., precomputed layouts in the Temputs blob) while the other recomputes at consumer-compile time, the recompute design wins. The compile-time cost is real but bounded; the staleness risk of baked-in decisions is unbounded. Sections 7, 8, 10 carry this pattern.
2. **Avoid time-saving shortcuts that make future architecture harder.** When the simpler design today produces a system harder to evolve, the more complex but evolvable design wins. The Sky compiler's clean phase boundaries, the strict marker-based per-crate activation, the deterministic skyc output — these all pay setup costs to keep options open later.
3. **Prefer fork patches over fragile-mechanism plumbing.** Sky accepts a rustc fork. Sky uses `per_instance_mir` as a custom query (Instance-keyed) rather than `optimized_mir` override (DefId-keyed). Sky's codegen integration is a full plugin (`-Zcodegen-backend=skyc` baked into the forked rustc) rather than a partitioner-override CGU-filter that depends on linkage-mutation timing. Each of these is more work; each eliminates a fragile mechanism that erw documented as load-bearing risk (B2 partitioner timing assumption, B1 mono collector drift). Section 3 covers the fork; Section 5 covers the plugin.

The posture is not "ignore costs." Costs are real, and Section 25 (risks) and Section 28 (phasing) document them honestly. The posture is: when costs are at the level of "this takes weeks rather than days" or "this requires a new fork patch," they are not by themselves disqualifying. They are tradeoffs to be weighed against the long-term correctness benefit.

### 1.5.5 Non-generic is the degenerate case of generic (uniform N=0 / N≥1 handling)

A positive design discipline Sky's compiler implementer is expected to follow throughout, especially at every architectural boundary that touches generic items:

**Non-generic is the degenerate case of generic. Never branch on "does this function/type have type parameters?" A non-generic item is simply one with zero type args — it goes through the same instantiation path as a generic one. Code that special-cases the non-generic path creates false distinctions and latent bugs when items gain type params or the code is reused in a more general context. Always write the general path; zero args is a valid input to it.**

The principle is expressed positively. Concretely:

- A populate loop that iterates concrete monomorphizations handles N=0 (one mono, empty args) and N≥1 (one mono per instantiation, non-empty args) through the same code. The N=0 case falls out as one iteration with `concrete_args = []`.
- A substitution helper that zips `type_params` against `instance.args.types()` produces an identity substitution for N=0 (empty zip, empty map). No `if N == 0 { skip }` branch.
- A discovery channel that captures monomorphizations from rustc's mono walker captures N=0 entries (concrete_args = []) the same way it captures N≥1 entries (concrete_args = [i32, ...]).
- A symbol-mangler that suffixes type args produces no suffix for N=0 (empty iteration) and the proper suffix for N≥1. Same code path.

**Where Sky may not follow this discipline:** three classes of forced exceptions, each must be `arch-fence-allow`-annotated when it appears:

1. **Rust syntax constraints.** `impl<>`, `Foo<>`, and `Self<>` are parse errors in Rust. Sky's stub_gen emission must skip the `<>` decoration when N=0. The branch lives at the syntactic surface, not in the architecture.
2. **External rustc behavior with no consumer override.** When a rustc query's contract differs for N=0 vs N≥1 in a way Sky cannot influence (e.g., debug-info walker assumptions that pre-Phase-E required different stub-gen shapes per N), the asymmetry is forced. Document and fence; remove when the upstream lifts the constraint.
3. **Approach A invariants.** The `debug_assert!(!instance.args.has_param())` in Sky's `per_instance_mir` body construction is a load-bearing assertion that arguments are concrete by the time Sky sees them. It is not "N==0 vs N>0"; it is "substituted vs unsubstituted." Keep.

**Why this matters for Sky's frontend implementer.** Sky's eventual implementation will land before all the generic-shape surfaces exist (e.g., comptime args, group params, method-level type params on impl methods). The discipline ensures every code path written for non-generic items extends to generic ones without an intervening refactor. When a code path is written with a `type_params.is_empty()` check, the path is essentially declaring "I don't know what to do for the generic case yet"; that declaration ages badly. Better to write the general path from the start and let N=0 fall out.

**Empirical reinforcement.** Branches on type-param emptiness are tripwires that fire when a new generic-shape surface appears (impl blocks gain type params, multi-param impls, method-level generics, generic accessors). Every branch carries an unspoken claim "I don't know what to do for the generic case yet"; the claim ages badly. Better to write the general path from the start and let N=0 fall out.

The corresponding cross-cutting invariant in §26.15 (NNGZ) gives this discipline a named tag for invocation in code comments. Source-level `arch-fence-allow:` markers + a grep-based architecture-fence CI test enforce the discipline mechanically.

### 1.6 Nightly rustc forever

Sky pins to a nightly rustc release. There is no path to stable rustc compatibility for Sky's compiler. The decision is locked.

The constraint flows from two unavoidable dependencies:

1. **`#![feature(rustc_private)]`.** Sky's compiler links against `rustc_driver`, `rustc_middle`, `rustc_codegen_ssa`, `rustc_codegen_llvm`, `rustc_monomorphize`, and related internal crates. Every one of these is gated by `rustc_private`, which is nightly-only and explicitly unstable. Rust-lang has stated (in multiple RFC discussions) that `rustc_private` remains the "all bets off" escape hatch indefinitely; there is no roadmap for stabilizing the internal API surface Sky uses.
2. **The `per_instance_mir` fork patch.** Sky adds a custom query to rustc. This patch never lands upstream as a generic mechanism (or if it does, it's the long-term outcome of multi-year RFC work; Section 29 discusses this). Sky maintains the patch locally and rebuilds rustc against each nightly bump.

The user-side implication: Sky users install a Sky-specific rustup toolchain. The toolchain contains a forked rustc binary (Sky's `rustc` is also Sky's codegen backend, statically linked together; see Section 4), the Sky orchestrator binary (`skyc`), the LLVM shared libraries the forked rustc and Sky's backend share, and a vanilla cargo binary. Installation is `rustup toolchain link sky-nightly /path/to/sky-toolchain` or via a custom rustup distribution; `rust-toolchain.toml` in Sky projects pins to `sky-nightly`. From the user's perspective, this is no different from installing a custom nightly toolchain via rustup — it's a known model, supported by the rustup tool, and minimally surprising.

The maintenance implication: Sky pays rustc bump costs proportional to the gap between bumps and the rustc internal API surface area Sky uses. Section 25 covers the risk profile in detail; the empirical data point from erw's 15-month bump (~half a workday of focused engineering) calibrates the rough order of magnitude.

The strategic implication: Sky is firmly outside the "stable Rust" ecosystem from a build-tooling perspective. The Sky ecosystem (Sky-published libraries, Sky-aware tooling) is its own world that overlaps with the Rust ecosystem at the source level (Sky source consumes Rust crates) but not at the build-tooling level (Sky users use skyc, not cargo directly). Section 21 covers the distribution model and its implications.

### 1.7 What Sky explicitly does NOT do

This subsection records what Sky is *not* — design choices that have been considered and rejected. Recording them here saves future readers from rediscovering rejected alternatives.

**Sky does not unwind.** All Sky-emitted code is compiled with `panic = "abort"`. No landing pads are emitted. Rust dependencies that the binary links are also compiled with `panic = "abort"` (cargo enforces consistency across the build graph). The implications: no `catch_unwind`, no panic-as-cancellation-signal, no exception-style nonlocal control flow. Sky's error model is Result-based; Sky's cancellation model is channel-based (Section 14). Section 16 covers this in full.

**Sky does not implement Rust-style "cancellation by drop."** When a Sky future is dropped while executing, Sky aborts the program rather than silently cancelling. Linear types may not be dropped at all; Sky's typechecker enforces this. The implication: tokio APIs that depend on drop-as-cancel (`select!`, `timeout`, `JoinHandle::abort`) are incompatible with Sky's default linear futures. Sky users use Sky-native equivalents (channel-based cancellation) or opt into cancellable futures via `into_cancellable(future, cleanup_handler)` for tokio interop. Sections 14, 15 cover the async story.

**Sky does not silently convert Rust types into Sky-shaped views.** Every Rust type used by Sky source must be explicitly imported. The import statement (`import rust.std.vec.Vec`) introduces the type name into Sky's namespace; without the import, the type cannot be named. Sky inherits this discipline from erw's `@RTMEIZ` pattern. Auto-discovery of types from method calls is explicitly rejected.

**Sky does not infer generic type arguments at call sites.** Every call to a generic function or generic type constructor must spell out the type arguments at the call site (or use a Sky source-level mechanism that desugars to explicit arguments — e.g., a hypothetical `auto` placeholder that the typechecker fills in). The Sky compiler does not do bidirectional type inference. Sky inherits this discipline from toylang's experience with inference-related complexity.

**Sky does not have Send/Sync as runtime-checkable properties.** Sky's typechecker statically tracks send-ability and sharedness of values through the type system; the runtime carries no marker information. Send/Sync as Rust trait bounds appear at the rustc boundary, but Sky lies to rustc — every Sky type carries a global `unsafe impl Send` so that Sky values flow into Rust-Sendable generics without explicit Send proofs. Sky's typechecker enforces actual send-correctness Sky-side. Section 12 covers this.

**Sky does not allow incoherent trait implementations.** Sky inherits Rust's orphan rule. A trait implementation can exist only in the crate that owns either the trait or the type. Sky's typechecker enforces this Sky-side, producing errors in Sky terms (not Rust errors leaking through the generated stub rlib). Section 6.6 covers this.

**Sky does not have separate type universes for runtime and compile-time.** Sky's comptime is Zig-style, same types at both stages. There is no "macro type" universe distinct from "runtime type" universe. Sky's frontend types every expression once. Section 13 covers this.

**Sky does not support reflection beyond what comptime can express.** Sky has no `typeof` operator, no runtime type information beyond the small amount LLVM's debug info layer provides, no dynamic dispatch over arbitrary types. Sky types are erased at runtime in the C-style sense. Section 13 implicit-covers this.

**Sky does not support unsized generic arguments outside specific reference patterns.** Sky inherits erw's `@UTAIRZ` pattern: unsized types (`str`, `[u8]`, `[T]` for non-`Sized` `T`) appear only as the inner type of a reference. Sky source cannot have a `T: ?Sized` generic in arbitrary position; it can have `&G T: ?Sized` references with specific size-known concrete instantiations.

**Sky does not surrender LLVM output control to rustc's codegen pipeline.** Sky's LLVM backend (Inkwell-based, contributed via patch 4's `fill_extra_modules` hook — see §5, §F.15) owns every byte of Sky-emitted LLVM IR. This is non-negotiable. It enables Sky to guarantee codegen quality, ABI discipline (`@ACRTFDZ`/`@TCHAPZ`), pin discipline (`@SMPLZ`), and emission-shape stability across rustc bumps. Designs that lower Sky source to rustc MIR and let rustc's codegen produce the bodies — periodically proposed as a "long-term simplification" because they would retire the discovery/synthesis machinery — are **rejected permanently** for this reason. Sky's frontend never emits rustc MIR for Sky-defined bodies; per_instance_mir returns synthetic dep-registering bodies (see §19), but the real bodies come from Sky's own LLVM backend, full stop.

**Sky does not treat drop as architecturally special.** Drop is just a function the language sometimes auto-calls. The compiler synthesizes `Drop::drop(&local)` AST nodes at scope-end positions during type resolution (see §15.7); after that pass, every downstream stage — dep walker, mono cascade, per_instance_mir, codegen, symbol resolution, link — treats the synthesized calls as ordinary trait static calls with no drop-specific code paths. The retired `mir_shims` query override (Phase E, 2026-06-23) had categorized drop as a thing that needed its own emission path; empirical validation showed the override had never actually worked for any shipping fixture, and its removal lost zero functionality. See §F.18 for the implementation lessons.

Many of these "does not do" entries are recovered through specific designed mechanisms — Sky doesn't unwind but Result is rich enough for error handling, Sky doesn't cancel by drop but channel-based cancellation is composable. The point of this subsection is not to enumerate Sky's limitations as a sales criticism but to make explicit the boundaries of the design space so future contributors know which questions are settled.

---

## 2. The Architectural Invariant: Interleaved Monomorphization

This chapter explains the foundational invariant that shapes everything downstream: **Sky's compiler must interleave with rustc's monomorphization phase.** A pre-pass design that enumerates Sky's required Rust monomorphizations before rustc starts, or a post-pass design that picks up after rustc finishes, cannot correctly handle Sky's interop cases.

The argument inherits from erw's `docs/reasoning/why-interleaved-monomorphization.md` with one substitution: Sky takes toylang's place. The seven cases in that document carry over verbatim; what Sky changes is which cases are *required* (Sky's interop story explicitly covers cases 1b, 3, 4, 5, 6 from day one; toylang's test corpus covered only Case 2). The mechanism is the same.

### 2.1 The seven-case taxonomy

A consumer language interoperates with Rust in one of seven architectural shapes. The shapes vary along three axes:

1. **Top-level language:** is the binary's entry point in Sky source or Rust source?
2. **Middle layer:** does the call graph pass through an intermediate library in the other language?
3. **Bottom-most callees:** are they in Sky source, Rust source, or both?

The seven cases enumerate the meaningful combinations. The table:

| Case | Top-level | Middle | Bottom | Sky-relevant? | Pre-pass works? |
|------|-----------|--------|--------|---------------|-----------------|
| 1a   | Rust      | —      | Sky (non-generic) | Yes | Yes |
| 1b   | Rust      | —      | Sky    | Yes | **No** |
| 2    | Sky       | —      | Rust   | Yes | Yes |
| 3    | Rust      | Sky    | Rust (same top) | Yes | **No** |
| 4    | Sky       | Rust   | Sky (same top) | Yes | **No** |
| 5    | Rust      | Sky    | different Rust | Yes | **No** |
| 6    | Sky       | Rust   | different Sky | Yes | **No** |

Five of the seven cases require interleaving. The remaining two (1a non-generic, 2 Sky-top-Rust-bottom) admit pre-pass solutions but interleaving handles them too. Sky covers all seven, so Sky must interleave.

The subsections below walk through each case with worked examples in Sky source (and Rust source where the case has Rust-side code). Read them sequentially the first time; on subsequent reads, the table above is sufficient as a reference.

### 2.2 Case 1a: Rust program calls a Sky library, non-generic only

The top-level is a Rust program. It depends on Sky code as a library. It calls only non-generic Sky entry points.

```rust
// main.rs - Rust top-level
extern crate sky_lib;

fn main() {
    sky_lib::emit_hello();
}
```

```sky
// Sky library
import rust.std.io.stdout
import rust.std.io.Stdout
import rust.std.io.Write

export fn emit_hello() {
    let out = stdout()
    Write::write_all(&out, b"hello\n");
}
```

**What must exist in the binary:**
- `emit_hello` — Sky-emitted, Sky's LLVM backend produces the body, linked at symbol `<crate name>::emit_hello` (or whatever the stub rlib names it).
- `stdout()` — rustc-emitted, stdlib body, normal Rust mono.
- `<Stdout as Write>::write_all` — rustc-emitted, stdlib trait method, normal Rust mono.

**Concrete-type-argument flow:** the only types flowing through the call graph (`Stdout`, `&[u8]`) are named directly in Sky source via explicit imports. Sky knows them statically.

**Why pre-pass works:** Sky's frontend, parsing its own source, can see the `stdout()` call and the `Write::write_all::<Stdout>` trait dispatch. Sky can emit an anchor function (a Rust-source helper containing `ReifyFnPointer`-equivalent casts of the Rust items needed) into the stub rlib's source. Rustc's monomorphization collector walks the anchor naturally, queues the Rust items, and cascades through their transitive Rust dependencies.

**Why this case admits pre-pass:** the flow of concrete type arguments is unidirectional — Sky source originates them, Rust code consumes them. There is no information rustc could discover that Sky's frontend doesn't already have.

Sky still handles this case via interleaving rather than pre-pass — interleaving is the general-case mechanism, and pre-pass is not separately implemented. But the case is recorded here because it bounds the worst case: at minimum, Sky's interop must do whatever pre-pass would do for this case, plus more for the other cases.

### 2.3 Case 1b: Rust instantiates Sky generics with Rust-defined types

Still Rust-as-top-level, Sky-as-library, but now the Rust side invokes a Sky generic with a Rust-defined concrete type.

```rust
// main.rs - Rust top-level
extern crate sky_lib;

struct LocalThing {
    value: i32,
}

fn main() {
    let t = LocalThing { value: 42 };
    let wrapped = sky_lib::wrap::<LocalThing>(t);
    drop(wrapped);
}
```

```sky
// Sky library
export struct Wrapper<T> {
    inner: T,
}

export fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}
```

**What must exist in the binary:**
- `wrap<LocalThing>` — Sky-emitted, with `T = LocalThing` substituted. Sky's codegen needs the layout of `LocalThing` to place the `inner` field correctly inside `Wrapper<LocalThing>`.
- Layout for `Wrapper<LocalThing>` — Sky-computed via the `layout_of` query override. Reports opaque-with-size to rustc; Sky's codegen knows the field structure internally.
- Drop glue for `Wrapper<LocalThing>` — Sky-emitted, needs to drop the contained `LocalThing` correctly (which involves calling Rust's drop glue for `LocalThing`).

**Concrete-type-argument flow:** Rust side originates the type. `LocalThing` is defined in `main.rs` and never appears in Sky source. Sky's frontend, parsing only Sky source, has no way to know that someone will instantiate `wrap<LocalThing>`.

**Why pre-pass fails:** Sky's frontend would have to read every `.rs` file in the Rust top-level crate (and every dependency), do type-checking to resolve which `wrap` instantiations the Rust code performs, substitute correctly for generic parameters inside the Rust code's own call graph, and propagate. That's rustc's job.

Over-approximation does not rescue pre-pass either. `LocalThing` is a user type defined in the top-level Rust code; Sky has no finite universe to over-approximate over. Any Rust-defined type could be passed to `wrap`. The set of instantiations is, in principle, unbounded.

**How Sky handles it via interleaving:** rustc's collector walks `main.rs`, sees the call `sky_lib::wrap::<LocalThing>(t)`, queues `wrap<LocalThing>`. Sky's `per_instance_mir` provider fires with `Instance(wrap_def_id, [LocalThing])`. Sky substitutes `T = LocalThing` (Sky-side substitution, since Sky's comptime machinery may participate), generates a synthetic MIR body whose drop semantics call into Sky's emitted `wrap<LocalThing>` symbol, and tells rustc that `wrap<LocalThing>` exists. Sky's codegen produces the actual machine code; rustc handles its half (the call site in `main.rs`, the linkage). Sky's `layout_of` provider answers when rustc queries the layout of `Wrapper<LocalThing>`. Drop semantics for `Wrapper<LocalThing>` flow through rustc's standard DropGlue path (post-2026-06-23 Phase E retirement of the `mir_shims` override — see §15.7).

### 2.4 Case 2: Sky program calls a Rust library

The top-level is a Sky program. It calls Rust generics with concrete types chosen by Sky source.

```sky
// Sky top-level
import rust.std.io.stdout
import rust.std.io.Stdout
import rust.std.io.Write
import rust.std.vec.Vec
import rust.std.alloc.Global

fn main() {
    let mut v = Vec::new<i32, Global>();
    v.push(1i32);
    v.push(2i32);
    v.push(3i32);

    let out = stdout()
    Write::write_all(&out, b"done\n");
}
```

**What must exist in the binary:**
- `main` — Sky-emitted.
- `Vec::<i32, Global>::new` — rustc-emitted.
- `<Vec<i32, Global>>::push` — rustc-emitted.
- Drop glue for `Vec<i32, Global>` — rustc-emitted.
- `stdout()`, `<Stdout as Write>::write_all` — rustc-emitted.

**Concrete-type-argument flow:** Sky source originates all of it. `i32`, `Global`, `Stdout` are all named in Sky's imports or call sites.

**Why pre-pass works:** every concrete instantiation of a Rust generic comes from a Sky source site. Sky enumerates them, emits an anchor function (a Rust-source helper containing `ReifyFnPointer`-equivalent casts), and rustc cascades through stdlib's Vec implementation. The chain is unidirectional.

**How Sky handles it via interleaving:** identical mechanism to Case 1b, but the entry point is `main` itself (a Sky function with no generic args; an entry point rather than a generic instantiation). The per_instance_mir body for `main` mentions every directly-called Rust item via ReifyFnPointer casts; rustc's collector walks the body and queues the Rust items; cascades from there.

This case is the bread-and-butter use case — Sky calling Rust libraries — and the one toylang exercises today. Sky inherits the mechanism wholesale.

### 2.5 Case 3: Rust → Sky library → back into Rust top-level's code

Rust-as-top-level, Sky-as-library, and the Sky library calls back into Rust-side code via trait dispatch on a Rust-defined type.

```rust
// main.rs - Rust top-level
extern crate sky_lib;

use std::clone::Clone;

struct MyCounter {
    count: i32,
}

impl Clone for MyCounter {
    fn clone(&self) -> MyCounter {
        MyCounter { count: self.count }
    }
}

fn main() {
    let c = MyCounter { count: 0 };
    let copied = sky_lib::clone_it::<MyCounter>(&c);
    drop(copied);
}
```

```sky
// Sky library
import rust.std.clone.Clone

export fn clone_it<T>(x: &T) -> T {
    Clone::clone(x)
}
```

**What must exist in the binary:**
- `clone_it<MyCounter>` — Sky-emitted, with `T = MyCounter` substituted.
- `<MyCounter as Clone>::clone` — **rustc-emitted** (the impl body lives in `main.rs`).

**Concrete-type-argument flow:** Rust side originates `MyCounter`. The concrete Instance `clone_it<MyCounter>` is reachable because the Rust top-level calls it.

**Why pre-pass fails:** the same reason as Case 1b — Sky cannot enumerate `clone_it<MyCounter>` without walking Rust source. Compounded: even if Sky somehow learned of the instantiation, walking `clone_it<MyCounter>`'s body with `T = MyCounter` substituted would discover the trait-dispatch `Clone::clone(x)` call, which resolves to `<MyCounter as Clone>::clone`. That target lives in `main.rs` — Rust source Sky cannot parse. Sky would need rustc's trait resolution machinery to handle this.

**How Sky handles it via interleaving:** rustc's collector walks `main.rs`, queues `clone_it<MyCounter>`. Sky's per_instance_mir provider fires with `Instance(clone_it_def_id, [MyCounter])`. Sky generates a synthetic body containing a ReifyFnPointer cast pointing at `<MyCounter as Clone>::clone` — with `MyCounter` substituted in. Rustc's collector walks the body, sees the ReifyFnPointer, queues the trait-method instantiation. Rustc resolves the trait dispatch: `<MyCounter as Clone>` matches the impl in `main.rs`. Rustc compiles the impl's `clone` method normally.

The key insight: Sky tells rustc "the leaves" (the trait-dispatch reference to `<T as Clone>::clone`, substituted to `<MyCounter as Clone>::clone`); rustc walks the rest (resolving the trait dispatch, codegenning the impl's method). Sky never has to parse `main.rs` or know about the `MyCounter` Clone impl directly.

### 2.6 Case 4: Sky → Rust library → back into Sky top-level's code

Sky-as-top-level, Rust-as-library, and the Rust library calls back into Sky-side code via trait dispatch on a Sky-defined type.

```sky
// Sky top-level
import some_rust_lib.duplicate
import rust.std.clone.Clone

struct Widget {
    id: i32,
}

impl Clone for Widget {
    fn clone(&self) -> Widget {
        Widget { id: self.id }
    }
}

fn main() {
    let w = Widget { id: 42 }
    let copy = some_rust_lib.duplicate<Widget>(&w)
    drop(copy)
}
```

```rust
// Rust library `some_rust_lib`
use std::clone::Clone;

pub fn duplicate<T: Clone>(x: &T) -> T {
    x.clone()
}
```

**What must exist in the binary:**
- `main` — Sky-emitted.
- `duplicate<Widget>` — **rustc-emitted** (Rust body, monomorphized with `T = Widget`).
- `<Widget as Clone>::clone` — **Sky-emitted** (Clone impl body is in Sky source).

**Concrete-type-argument flow:** Sky source originates `Widget`. The Rust generic `duplicate<Widget>` is reachable from Sky's main. The Rust body of `duplicate<Widget>` internally calls `x.clone()` which dispatches to `<Widget as Clone>::clone`.

**Why pre-pass fails:** Sky source mentions `duplicate<Widget>` directly, so Sky could in principle anchor it. But Sky would not know, from its own source, that `duplicate`'s Rust body calls `Clone::clone(x)` internally. Sky would have to walk Rust MIR to find out — which is rustc's job. Without that knowledge, Sky cannot enumerate `<Widget as Clone>::clone` as a required entry. Rustc walks `duplicate<Widget>`'s body during its own mono pass and queues `<Widget as Clone>::clone`; if Sky has not pre-anchored or otherwise surfaced the Sky-side `clone` impl in a form rustc can use, the link fails.

**The over-approximation workaround that almost works for simple cases:** Sky has a finite set of types with `Clone` impls. Sky could pre-emit anchors for all of them (every type's clone method). This compiles dead code (some clones never get called) but produces a complete reachable set.

**Why the over-approximation workaround dies in the general case:** generic methods on generic traits. Consider a Rust API:

```rust
pub trait Serialize<Format> {
    fn serialize<Writer: Write>(&self, f: Format, w: &mut Writer);
}

pub fn serialize_to<T, F, W>(x: &T, fmt: F, buf: &mut W)
where T: Serialize<F>, W: Write
{
    x.serialize(fmt, buf);
}
```

A Sky type with a `Serialize<JsonFormat>` impl, called from Sky source via `serialize_to<Widget, JsonFormat, Stdout>(&w, fmt, &mut out)`, ends up monomorphizing:

```
<Widget as Serialize<JsonFormat>>::serialize::<Stdout>
```

Three type arguments: `Widget` (the Self type), `JsonFormat` (the trait's type parameter from the impl block), `Stdout` (the method's type parameter from the call site). The first two come from Sky source; the third — `Writer = Stdout` — is chosen at the call site that passes the `&mut out`. In this example Sky source is the caller; if a different Rust caller of `serialize_to` passed a different buffer type, the same Sky-defined method body would need a different concrete instantiation.

For Sky to pre-anchor this, Sky would need to enumerate every concrete `Writer` type any Rust caller anywhere might pass. Unbounded. The cross-product of (Sky types with Serialize impls) × (trait type parameter values) × (method type parameter values rustc might substitute) is infinite in the general case.

**The trait-generic-method case kills the over-approximation workaround.** Sky needs interleaving.

**How Sky handles it via interleaving:** Sky's compiler emits an extern declaration for `duplicate` in the stub rlib (or wherever the Rust-fn-reference machinery places it; Section 6 covers this). Rustc's collector queues `duplicate<Widget>` when walking Sky's main. Rustc walks `duplicate<Widget>`'s Rust body, substitutes `T = Widget`, sees `x.clone()`, resolves to `<Widget as Clone>::clone`. The impl is owned by Sky (the impl is in Sky source). Sky's `per_instance_mir` provider fires for the trait-impl method's Instance. Sky generates the appropriate synthetic body (or, if Sky has chosen to register the impl as a stub-rlib item with a real Rust signature, the stub-rlib's `unreachable!()` body would be intercepted by Sky's codegen-backend plugin). Sky's codegen emits the real `clone` body.

The key insight again: Sky tells rustc that the impl exists (via the stub rlib's impl block); rustc walks the Rust caller's body and discovers the dispatch; rustc queues the trait method; Sky's per_instance_mir provides the body.

**Empirical correction (sessions 18-19).** The "rustc's collector queues `duplicate<Widget>` when walking Sky's main" sentence above is correct in terms of *cascade flow*, but it elides *when* the cascade fires. Empirically — verified by per_instance_mir-probing toylang's `case_generic_impl_block` fixture — the consumer-side `per_instance_mir` cascade for an item like Sky's `__sky_main` fires **at the stub rlib compile, not at the user-bin compile**. At user-bin compile, rustc's mono collector at `collector.rs::should_codegen_locally` gates on `is_reachable_non_generic(def_id) || instance.upstream_monomorphization(tcx).is_some()` for non-local items: when `__sky_main` is a non-generic upstream symbol (which it is — Sky's main lives in the bin's own stub rlib), this gate short-circuits and the collector never queries `per_instance_mir` for it at user-bin time. The cascade — and therefore the discovery of `duplicate<Widget>` and `<Widget as Clone>::clone` — is **exclusively a stub-rlib-compile-time mechanism.**

The current architecture handles this by emitting the cascade-surfaced trait-impl bodies **at the same compile session where the cascade fires** (i.e. at the stub rlib compile). At that session's `consumer_fill_modules`, after the mono walk completes (the post-mono timing avoids @GCMLZ re-entry that would deadlock at `after_rust_analysis`), the pure function `collect_consumer_trait_impl_instances(tcx) -> Vec<DiscoveredTraitImplInstance>` walks the unfiltered partition for `MonoItem::Fn(instance)` entries matching `is_consumer_trait_impl_method`, returns a local Vec, and the same callback drains it inline into Sky's codegen pipeline. No sidecar serialization, no `on_sky_lib_loaded` deserialization, no augmented-map query override — symbol canonicalization across crates is handled by Sky's `collect_and_partition_mono_items` filter (removes consumer items from rustc's CGU list so the only `.o` symbol comes from `fill_extra_modules`) plus `share_generics = true` at every Sky stub rlib compile (so rustc's natural cstore-walk records each cascade-surfaced monomorphization in the rlib's rmeta for downstream lookup). See §8.9.5 for the current pipeline and §F.13 / §F.14 for the deep-investigation findings.

### 2.7 Cases 5 and 6: transitive library structure

These are compositions of the simpler cases, one hop deeper.

**Case 5 (Rust top → Sky lib → different Rust lib):**

```rust
// Rust top-level
extern crate sky_lib;

struct Record { id: i32 }

fn main() {
    let r = Record { id: 99 };
    sky_lib::store_in_vec::<Record>(r);
}
```

```sky
// Sky library
import rust.std.vec.Vec
import rust.std.alloc.Global

export fn store_in_vec<T>(x: T) {
    let mut v = Vec::new<T, Global>()
    v.push(x)
}
```

Case 1b layered over Case 2. Rust top originates the concrete `Record`, which flows through Sky's `store_in_vec` into stdlib's Vec. If the top were Sky (Case 2), pre-pass would work. With Rust top, Case 1b's pre-pass failure propagates. Sky handles it via interleaving identically to Cases 1b and 2 combined.

**Case 6 (Sky top → Rust lib → different Sky lib):**

```sky
// Sky top-level
import some_rust_lib.duplicate
import sky_util.Pair

fn main() {
    let p = Pair::new<i32, i32>(1i32, 2i32)
    let copy = some_rust_lib.duplicate<Pair<i32, i32>>(&p)
    drop(copy)
}
```

```rust
// Rust library
pub fn duplicate<T: Clone>(x: &T) -> T { x.clone() }
```

```sky
// Different Sky library `sky_util`
import rust.std.clone.Clone

export struct Pair<A, B> {
    first: A,
    second: B,
}

impl<A, B> Clone for Pair<A, B>
where A: Clone, B: Clone {
    fn clone(&self) -> Pair<A, B> {
        Pair {
            first: Clone::clone(&self.first),
            second: Clone::clone(&self.second),
        }
    }
}

export fn new<A, B>(a: A, b: B) -> Pair<A, B> {
    Pair { first: a, second: b }
}
```

Case 4 with the Sky-defined trait impl living in a separate Sky library. Sky's interleaving handles it the same way Case 4 does: rustc's collector walks `duplicate<Pair<i32, i32>>`'s Rust body, queues `<Pair<i32, i32> as Clone>::clone`, resolves to the impl in `sky_util`, Sky's per_instance_mir provides the body. The cross-Sky-library trait-impl resolution falls out of standard Rust trait coherence — `sky_util` owns the impl, the impl's symbol lives in the binary's final `.o`, the linker resolves cross-crate.

### 2.8 The handoff: Sky tells rustc the leaves, rustc walks the rest

The unifying pattern across all interleaved cases: Sky tells rustc the leaves (the concrete Rust items called directly from Sky-defined bodies, or the trait dispatches Sky bodies make), and rustc walks the rest (transitive Rust closures, trait resolution, associated type projection, drop glue cascading, default method instantiation).

This is the load-bearing observation. Sky does *not* implement Rust's trait resolution machinery. Sky does *not* implement Rust's generic substitution beyond what its own type system needs. Sky does *not* implement Rust's associated-type projection, blanket-impl coherence checking, or specialization. Sky implements its own type system, projects Sky-defined items onto Rust-shaped surfaces (stub rlibs, layout queries, per_instance_mir bodies), and lets rustc handle Rust's side of the type system.

This is what makes Sky's interop architecturally tractable. The alternative — Sky reimplementing rustc's trait/generic machinery — would be tens of thousands of lines of code in Sky's compiler, every line of which must track rustc's actual behavior to avoid divergence. The interleaved-monomorphization design avoids this entirely: rustc *is* the substrate for Rust's type system; Sky borrows it.

### 2.9 Why interleaving is the general-case answer

Sky's interop covers all seven taxonomic cases. The five hard cases (1b, 3, 4, 5, 6) require interleaving. Cases 1a and 2 admit pre-pass alternatives but interleaving handles them identically. Sky implements only the interleaved mechanism.

A consumer architecture that strictly limits itself to Cases 1a and 2 can use a simpler pre-pass design and avoid most of the rustc-internal coupling Sky requires. Sky explicitly does not do this. Sky's interop story is bidirectional from day one because Sky's strategic position — a memory-safe systems language for greenfield projects that wants to leverage the Rust ecosystem — requires it. Forbidding Cases 1b, 3, 4, 5, 6 would mean Sky users could only write standalone Sky programs that call Rust libraries; they could not expose Sky libraries to Rust users, could not provide Sky types as type arguments to Rust generics, could not implement Rust traits on Sky types. The Sky ecosystem would be a research-language ecosystem.

The cost of supporting the full taxonomy is the interleaved-monomorphization machinery: a custom rustc query (`per_instance_mir`, Section 19), a codegen-backend plugin (Section 5), a stub rlib model (Section 6), and the operational discipline to handle the cross-cutting invariants (Sections 12, 16, 26). Sky pays this cost as a foundational architectural decision.

### 2.10 What "interleaving" means precisely

A note on terminology, because the word "interleaving" can be vague.

Interleaving here means: **Sky's compiler hooks fire during rustc's monomorphization collection phase, supplying per-Instance information about Sky-defined items as the collector encounters concrete Instances of those items.** The collector calls Sky's `per_instance_mir` query when it walks a body referencing a Sky-defined function; Sky's provider returns the body (substituted to the concrete Instance's args). The collector calls Sky's `layout_of` query when it needs a Sky type's layout; Sky's provider returns it. Drop semantics flow through rustc's standard DropGlue path post-Phase-E; Sky-defined Drop impls (`<Widget as Drop>::drop`) flow through the cascade-discovery + `fill_extra_modules` emission path like any other Sky trait-impl method. See §15.7 for the AST-rewrite mechanism that inserts the drop calls.

The collector is the driver. Sky is the responder. The collector's walk is what discovers the reachable set; Sky supplies the parts the collector doesn't have access to via Rust source.

What interleaving is *not*:
- It is *not* "Sky runs a separate phase before rustc and tells rustc what to compile." That's pre-pass.
- It is *not* "Sky runs a separate phase after rustc and picks up Rust's CGUs." That's post-pass (the `CodegenBackend` plugin alone, without query overrides). It can't see Rust-side discovery of Sky-item Instances.
- It is *not* "Sky implements its own collector and walks both Rust and Sky source." That's reimplementing rustc.

The mechanism is specifically: Sky's hooks fire *during* rustc's collection, *as part of* the collector's walk, *responding to* the collector's queries. The collector remains the single entity that walks the reachable set; Sky just answers questions the collector asks about Sky-shaped items it encounters.

---

## 3. The Fork

Sky maintains a fork of rustc. The fork is deliberate, not a fallback. This chapter explains why, what the fork contains, the cost model, and the long-term trajectory.

### 3.1 Why Sky forks

Sky needs a custom rustc query: `per_instance_mir`. The query is Instance-keyed (takes a concrete `Instance<'tcx>`, not a `LocalDefId`), and Sky's provider returns a MIR body with Sky's comptime evaluation already applied to the Instance's concrete args. There is no sanctioned rustc extension point that delivers this. Sky adds the query as a fork patch.

The reason Sky needs Instance-keyed substitution rather than DefId-keyed (as `optimized_mir` provides via the sanctioned `Config::override_queries`) is **arbitrary-typed const generic parameters**.

Rust's const generics are restricted to a small set of types: integers, `bool`, `char`, and (under `adt_const_params`) certain ADTs that satisfy strict valtree-encoding constraints. Rust's substitution engine has machinery to encode, compare, intern, and substitute `ConstKind::Param` values for these types. The machinery does not generalize to arbitrary user-defined types — there is no plugin extension point for "extend the const-generic universe with this Sky type, here's its equality semantics, here's its hashing."

Sky's comptime model produces values of *any* Sky type. Some of those values appear as comptime arguments to generic functions — Sky's analog to Rust's const generics, but without the type restriction. When Sky's compiler instantiates such a function for a specific compile-time-known argument value (say, a `Spaceship` produced by a `comptime` block), the generic argument is a concrete `Spaceship` value, not a type parameter.

If Sky used Approach B (DefId-keyed `optimized_mir` override, rustc-side substitution), Sky's provider would have to return a body with `Spaceship` as a `Param` placeholder. Rustc's collector would then try to substitute the placeholder per Instance. But rustc has no representation for `Spaceship`-as-Param — `Spaceship` is not in rustc's universe at all. The collector would either crash or silently produce wrong output.

The slab-pointer trick (representing comptime Sky values as integer slab addresses) helps, but does not fully resolve the issue. Sky *could* surface `Spaceship`-as-comptime-arg to rustc as `usize`-typed const args holding the slab address, and rustc could substitute those `usize` values across the body. But then Sky's body construction at substitution time would still need access to the *actual* Spaceship value (to evaluate comptime expressions involving the Spaceship), and that evaluation can't be deferred to rustc — only Sky's comptime evaluator understands Spaceship semantics. So Sky's substitution needs to happen at body-construction time with the Instance's concrete args in hand, which means the query must be Instance-keyed.

This is the Approach A constraint inherited from `dep-discovery-approaches.md`'s analysis: when the downstream substitutor (rustc's collector) cannot handle the value type, the upstream substitution (Sky's provider) must do it. Sky's compile-time metaprogramming makes Sky the only entity that can substitute Sky-typed comptime args correctly. Hence per_instance_mir, Instance-keyed.

### 3.2 The four patches

Sky's fork is four patches against vanilla nightly rustc. Three add the
`per_instance_mir` query — identical in shape to erw's pre-stage-3
fork. The fourth (added during toylang's Phase 4.5 / Session 15, rewritten
to rev 2 in the Approach B migration) adds a `fill_extra_modules`
allocator-callback hook on `ExtraBackendMethods` for inline codegen
contribution. None modify rustc's behavior for vanilla compiles
(default-empty / default-None providers preserve the pass-through
invariant; see §4.4).

**Patch 5 retired 2026-06-22.** The original patch 5 (`consumer_lang_active`
query + gated escape clause in `Instance::upstream_monomorphization`)
was load-bearing only because Option 4's `AvailableExternally` body
existed in IR to create the CGU-placement hazard the escape papered
over (§F.17). With Option 4 retired and the partition filter restored
(§F.14.1, §C.2), no AvailableExternally body reaches LLVM — the filter
removes consumer items from rustc's CGU list entirely. There's no
co-location hazard for patch 5 to protect against, so patch 5 retired
together with Option 4. See `tmp/patch5-empirical-2026-06-21/VERDICT.md`
for the empirical contrast probe (case5_no_lto runtime panic + CGU
listing + main disassembly) that drove the joint retirement decision.

**Patch 1: declare the query.** In `compiler/rustc_middle/src/query/mod.rs`, add a query declaration:

```rust
rustc_queries! {
    // ... existing queries ...
    query per_instance_mir(key: ty::Instance<'tcx>) -> Option<&'tcx mir::Body<'tcx>> {
        desc { "computing per-Instance MIR for {:?}", key }
        cache_on_disk_if { false }
    }
    // ... existing queries ...
}
```

The declaration plumbs the query through rustc's query macro infrastructure. It creates the `Providers` slot, the dispatch path, and the caching machinery.

**Patch 2: collector calls per_instance_mir.** In `compiler/rustc_monomorphize/src/collector.rs::collect_items_of_instance`, modify the per-Instance walk to call `per_instance_mir` before falling through to `instance_mir`:

```rust
fn collect_items_of_instance<'tcx>(tcx: TyCtxt<'tcx>, instance: Instance<'tcx>, ...) {
    let body = tcx.per_instance_mir(instance)
        .unwrap_or_else(|| tcx.instance_mir(instance.def));
    // ... walk `body` for dependency edges as before ...
}
```

When the plugin's provider returns `Some(body)`, the collector walks the plugin-returned body; when it returns `None`, the collector falls through to the default `instance_mir` path (rustc's normal MIR resolution).

**Patch 3: default provider returns None.** In `compiler/rustc_mir_transform/src/lib.rs::provide` (where rustc's MIR-related default providers are registered), add:

```rust
providers.per_instance_mir = |_tcx, _instance| None;
```

This makes the query a no-op for vanilla rustc. Without a plugin installing a real provider, the collector's `tcx.per_instance_mir(instance)` always returns `None`, the unwrap_or_else falls through to `instance_mir`, and rustc's behavior is unchanged from a non-forked rustc. This means the forked rustc, when compiling pure-Rust code (no Sky plugin active), produces byte-identical output to vanilla rustc — a testable invariant.

**Patch 4: `fill_extra_modules` allocator-callback hook for inline codegen (Approach B).** In
`compiler/rustc_codegen_ssa/src/traits/backend.rs`, add two methods on
`ExtraBackendMethods` plus a companion `ExtraModuleAllocator<M>`
trait and a generic `VecAllocator<'a, M, F>` driver:

```rust
pub trait ExtraModuleAllocator<M> {
    fn allocate(&mut self, name: &str) -> &mut M;
}

pub struct VecAllocator<'a, M, F: FnMut(&str) -> M> {
    pub modules: &'a mut Vec<ModuleCodegen<M>>,
    pub make_module: F,
}

pub trait ExtraBackendMethods: ... {
    // ... existing methods ...

    /// Backend-specific module constructor used by the codegen driver to
    /// mint freshly-allocated modules on the consumer's behalf. Default
    /// panics; adopting backends override to call their own per-CGU
    /// constructor (e.g. `ModuleLlvm::new(tcx, name)`).
    fn allocate_extra_module<'tcx>(&self, tcx: TyCtxt<'tcx>, name: &str) -> Self::Module { panic!(...) }

    /// Contribute extra modules to the codegen pipeline before
    /// `start_async_codegen`. The consumer calls `allocator.allocate(name)`
    /// to obtain a fresh rustc-owned `&mut Self::Module`, fills it in place
    /// via its own IR API, and returns. Rustc retains ownership throughout.
    fn fill_extra_modules<'tcx>(
        &self,
        _tcx: TyCtxt<'tcx>,
        _allocator: &mut dyn ExtraModuleAllocator<Self::Module>,
    ) { }
}
```

Default-no-op so non-adopting backends are unaffected. The LLVM
backend's impl in `compiler/rustc_codegen_llvm/src/lib.rs` consults a
process-global `OnceLock<FillExtraModulesHook>` settable via
`set_fill_extra_modules_hook(fn_ptr)`, and overrides `allocate_extra_module`
to construct `ModuleLlvm` via the same `ModuleLlvm::new(tcx, name)` path
rustc's per-CGU pipeline uses. Sky's facade installs the hook during
driver setup alongside `Config::override_queries`.

In `compiler/rustc_codegen_ssa/src/base.rs::codegen_crate`, call the
hook **synchronously on the main thread BEFORE `start_async_codegen`**:

```rust
let mut extra_modules: Vec<ModuleCodegen<B::Module>> = Vec::new();
{
    let mut allocator = VecAllocator {
        modules: &mut extra_modules,
        make_module: |name: &str| backend.allocate_extra_module(tcx, name),
    };
    backend.fill_extra_modules(tcx, &mut allocator);
}
// extra_modules passed into start_async_codegen; processed via
// execute_optimize_work_item before the worker pool starts.
```

Sub-patch: `ModuleLlvm::llcx_raw_mut() -> *mut c_void` and
`ModuleLlvm::llmod_raw() -> *mut c_void` exposed for FFI bridging into
externally-managed LLVM wrappers (Inkwell's `Context::new` wrapped in
`ManuallyDrop` + `Module::new_borrowed`). Type-erased to `c_void` to
avoid leaking private `llvm::Context` / `llvm::Module` types through
the public API.

**Approach B design.** Rustc owns each per-CGU module's lifecycle
(LLVMContext + LLVMModule + TargetMachine); the consumer's emitter
wraps the borrowed LLVM pointers in suppressed-Drop Inkwell handles
(`ManuallyDrop<Context>` + `Module::new_borrowed`) and emits IR
directly. No bitcode serialization, no LLVM-context migration, no
`parse_from_tcx` round-trip — closing risks B9 / B10 / B11 from §25.2
by construction. The earlier v1 bytes-as-interface shape (which had
`extra_modules() -> Vec<ModuleCodegen<M>>` and a `parse_from_tcx`
sub-patch) is retired; see §F.15 for the design history and §C.4 for
the full shipping shape.

**Patch 5 (retired 2026-06-22): historical design.** This subsection
preserves the patch's shape for archive purposes; the patch is no
longer in the fork. Patch 5 was a `consumer_lang_active(()) -> bool`
query + a gated CGU-placement escape clause in
`Instance::upstream_monomorphization`. It added three small sites
across `rustc_middle/src/query/mod.rs` (query decl), `rustc_mir_transform/src/lib.rs`
(default provider returning false), and `rustc_middle/src/ty/instance.rs`
(the gate's `&& !(consumer_lang_active(()) && upstream_monos_for(def).contains_key(args))`
escape).

Empirically (§F.17 + `tmp/patch5-empirical-2026-06-21/VERDICT.md`),
patch 5's load-bearing effect was CGU placement, not v0-mangler
disambig as originally framed. With Option 4's `codegen_fn_attrs`
override stamping consumer items `AvailableExternally`, rustc's
partitioner would co-locate the AvailableExternally `unreachable!()`
body with `main` in the same CGU; LLVM's intra-CGU inliner at -O3
could then inline the panic body into main, producing runtime SBMNBIZ.
Patch 5's escape, by causing `Instance::upstream_monomorphization` to
return `Some(upstream_crate)` instead of `None`, made the partitioner
treat the item as upstream-owned and place its body in a separate CGU
from callers — preventing the inliner from reaching it.

When Option 4 retired (2026-06-22, §F.14.1) and the partition filter
returned to the facade (§C.2), rustc stopped emitting the
AvailableExternally body to LLVM IR entirely. With no such body to
misplace, patch 5's protection was no longer needed. Both retired in
the same commit.

**Total patch surface:** approximately 28 lines for the
`per_instance_mir` trio (across `rustc_middle::query::mod`,
`rustc_mir_transform::lib`, `rustc_monomorphize::collector`) + ~194
lines for the `fill_extra_modules` hook + allocator trait (across
`rustc_codegen_ssa::traits::backend` + `traits::mod` + `base` +
`back::write`, plus `rustc_codegen_llvm::lib`) = ~222 lines across
8 files. Each patch is small, structurally local, and follows
established patterns in rustc's source. The patches collectively add two extension points
that rustc's existing infrastructure (query macros, collector
dispatch, the codegen backend trait) already accommodates
structurally; the patches just connect the dots.

### 3.3 Long-term: upstream as `adt_const_params`-extension or new query

Sky's long-term ambition is to push `per_instance_mir` (or an equivalent extension mechanism) upstream into rustc proper. The arguments for upstream landing:

- The mechanism is generally useful. Any consumer language with non-rustc-compatible compile-time arguments faces the same problem Sky does. A sanctioned extension point would benefit more than just Sky.
- The patch surface is small and structurally local. Upstream review can focus on the design (is this the right extension point?) without grappling with large-scale changes to rustc's internals.
- The semantic shape is familiar. `per_instance_mir` is an Instance-keyed analog of `optimized_mir`, which already exists and is overridable via `Config::override_queries`. The conceptual jump for upstream reviewers is small.

The likely upstream paths:

1. **`per_instance_mir` as a specific query, plugin-overridable.** Smallest upstream surface. Rust-lang would need to be convinced that plugin-defined Instance-keyed bodies are a sanctioned use case, where the existing answer is "use `optimized_mir` plus collector substitution." The hard sell: motivating the case where collector substitution is insufficient (Sky's arbitrary-typed comptime). The hard sell becomes easier with more consumer-language case studies; today Sky is one of few, but Vale and other interleaving-mechanism users could ride the same RFC.

2. **Generalized "plugin-defined substitution semantics" via an extension trait.** Lets plugins participate in `ConstKind::Param` substitution for plugin-defined types. Covers Sky's specific need (extend the const-generic universe with Sky types and provide equality/hash/substitution semantics). Doesn't add a new query; integrates into existing substitution infrastructure. Bigger RFC, but the right primitive — `per_instance_mir` is a workaround for "no extension to const-generic substitution"; the right answer is to add that extension.

3. **`adt_const_params` extended to allow externally-provided equality/hashing.** Even narrower than (2) — just lets non-valtree ADTs be const generics with plugin-provided semantics. The smallest viable upstream surface for Sky's specific use case. Probably the most palatable to upstream reviewers because it doesn't add new architecture, just extends an existing feature.

Sky's posture: pursue (3) as the primary upstream path, with (2) as a follow-on if (3) lands. (1) is the fallback if neither (2) nor (3) gain traction — it's the smallest behavioral change to upstream rustc that gives Sky what it needs.

The upstream effort is *not* on Sky's critical path. Sky ships with the fork, maintains it, and pursues upstream landing in parallel as a multi-year effort. The fork is sustainable indefinitely; upstreaming is the long-term cleanup.

### 3.4 Fork maintenance budget

The empirical baseline for fork maintenance is erw's pre-stage-3 experience: ~2-3 days per nightly bump for a 5-patch fork. Sky's fork is 4 patches (per_instance_mir trio + `fill_extra_modules` hook), and one of them touches the mono collector (a churn-prone area). Patch 5 was retired 2026-06-22 (see §3.2); its rebase cost is no longer a budget item. The realistic estimate:

- **Per-bump cost: ~1-2 days for the fork rebase.** Rebasing the four patches onto a newer nightly. The patches are small; rebases are typically clean. When they aren't (rustc has restructured the touched code), the rebase is a couple of hours of figuring out the new shape and re-applying the patch's intent. Patch 4 (`fill_extra_modules` hook) touches five files in the codegen stack: `rustc_codegen_ssa::traits::backend` (allocator trait + default trait methods), `rustc_codegen_ssa::traits::mod` (re-exports), `rustc_codegen_ssa::base::codegen_crate` (hook invocation on the main thread before `start_async_codegen`), `rustc_codegen_ssa::back::write` (param threading through `start_async_codegen` + `start_executing_work`, plus the coordinator-thread `execute_optimize_work_item` loop and `compiled_modules.extend`), and the LLVM impl in `rustc_codegen_llvm::lib` (hook `OnceLock` + `ExtraBackendMethods` overrides + raw-pointer accessors). These have moderate churn risk during periods when rustc restructures the codegen-coordinator/backend trait surface; the `back::write` site is the most churn-prone. The rebase may take a half-day during such windows. The debuginfo walker clamp (§10.4.5 / §25 B8) is no longer needed under the wrapper-as-field shape (§10.6) — the structural fix made the defensive patch obsolete.
- **Per-bump cost: ~1 week for MIR construction churn.** This is independent of the fork — it's the cost of Sky's per_instance_mir provider building synthetic MIR bodies, which uses rustc-internal MIR construction APIs that drift. The empirical erw data point: 15 months of MIR drift was ~1 hour for erw's 6 sites. Sky's count is higher (every generic Sky function exported produces per_instance_mir output containing ReifyFnPointer casts), so the per-bump cost is larger, but the per-site cost is similar.
- **Per-bump cost: ~1-2 days for ABI helpers drift.** PassMode variants, BackendRepr changes, layout-data shape shifts. Sky inherits erw's ABI helpers wholesale; the drift surface is identical.
- **Per-bump cost: ~0.5-1 day for everything else.** Driver entry-point changes, Callbacks trait additions, layout query key shape, providers struct restructuring. All small, all mechanical.

**Total per-bump cost: ~1.5-2 weeks for a focused engineer.** This is a real but bounded cost. It is paid every ~6 months if Sky chases bumps eagerly; less often if Sky lets the gap grow to ~12-18 months and absorbs more drift in larger batches.

Sky's recommended posture: bump to a ~3-month-old nightly every ~6 months, in dedicated sessions. Don't chase the latest nightly. Bump in dedicated commits, not interleaved with feature work, so bisection works cleanly. Run the full test suite cold and warm after each bump. Section 25 covers the bump strategy in detail; this subsection just budgets the cost.

### 3.5 Nightly pin and bump strategy

Sky pins to a specific nightly via `rust-toolchain.toml`:

```toml
[toolchain]
channel = "sky-nightly"  # custom rustup channel name, links to Sky's fork
```

The Sky toolchain itself is built against a specific upstream nightly (e.g., `nightly-2026-01-20`). The Sky-nightly version is bumped in coordinated releases, not independently per project.

**The bump strategy:**

1. **Decide to bump.** Triggered by a calendar event (~6 months since last bump) or by a forcing function (need a specific upstream feature, security update, etc.). Not triggered by chasing the latest nightly.
2. **Pick the target nightly.** ~3 months old. This window lets ecosystem-adjacent projects (cranelift, miri, rust-analyzer) report any drift issues with the target nightly before Sky encounters them.
3. **Snapshot.** Run the full test suite on the current pin. Record the test count.
4. **Bump the rustc fork.** Rebase the four patches onto the target nightly. Build the forked rustc. Resolve any patch conflicts. Patch 4 (`fill_extra_modules` hook) touches five files: `rustc_codegen_ssa::traits::backend`, `rustc_codegen_ssa::traits::mod` (re-exports), `rustc_codegen_ssa::base::codegen_crate`, `rustc_codegen_ssa::back::write` (param threading + coordinator-thread processing loop), and `rustc_codegen_llvm::lib`. It may take a half-day to rebase during periods when rustc restructures the backend trait surface or the codegen coordinator; the `back::write` site is the most churn-prone. The per_instance_mir trio is typically clean.
5. **Bump Sky.** Update Sky's `rust-toolchain.toml` to the new target. Run `cargo check` on Sky's compiler. Fix compilation errors (MIR construction drift, ABI helpers drift, etc.) in dedicated commits, each commit addressing one drift surface. The "one drift surface per commit" rule is for bisection — if a future bump reverts behavior, finding the commit that mattered is cleaner.
6. **Test cold.** Wipe all caches. Run the full Sky test suite. Diagnose any test failures as either drift-related (fix the drift) or environmental (fix the harness).
7. **Test warm.** Run the suite a second time. Catch incremental-compilation-related issues that only manifest on warm runs.
8. **Update documentation.** Specifically, update Section 25's empirical bump-cost data with this bump's observations. The next-bump architect uses this to calibrate.
9. **Ship.** Cut a Sky-nightly release.

The whole process is ~2-3 weeks for a focused engineer. It is scheduled work, not interleaved with feature development.

### 3.6 Cross-references

- Section 5 (codegen backend) — the partner mechanism to per_instance_mir, owns codegen-time emission.
- Section 19 (per_instance_mir and dep discovery) — the detailed mechanism for the patches' semantic role.
- Section 25 (risks) — the broader risk profile including bump costs.
- Section 28 (phasing) — when the fork patches must land relative to other implementation work.
- Section 29 (open questions) — the upstream-landing trajectory.

---

## 4. Distribution Shape

This chapter describes how Sky is packaged, distributed, and installed. The decisions here are operational — they don't change the architectural mechanism — but they shape every user's first interaction with Sky.

### 4.1 Forked rustc + Sky codegen backend + Sky frontend, statically linked

Sky's compiler binary `rustc` is the forked rustc with:

- The four fork patches applied: the `per_instance_mir` trio (patches 1-3) and the `fill_extra_modules` allocator-callback hook on `ExtraBackendMethods` (patch 4). See §3.2. (Historical patch 5, the `consumer_lang_active` gated escape, retired 2026-06-22 — see §F.14.1 / §F.17.)
- Sky's codegen backend (`SkyCodegenBackend`) statically linked into the binary. The backend wraps `LlvmCodegenBackend` for Rust items and emits Sky-emitted bodies for Sky items.
- Sky's frontend (parser, name resolver, typechecker, comptime evaluator, MIR generator for the per_instance_mir provider, Inkwell-based LLVM IR emitter) statically linked into the binary.

The binary is a single statically-linked executable (modulo the LLVM shared libraries it dynamically links, same as vanilla rustc). When cargo invokes it on a crate that does not have Sky's marker (Section 6.3), Sky's machinery is dormant and the binary behaves byte-identically to vanilla rustc. When cargo invokes it on a crate that does have the marker, Sky's machinery activates: Sky's frontend processes the crate's `.sky` source; Sky's codegen backend emits the Sky items.

**Why statically linked rather than cdylib plugin:** the alternative is "ship Sky's codegen backend as a separately loaded `libskyc_backend.so` and have cargo invoke vanilla rustc with `-Zcodegen-backend=skyc`." This is how cranelift and the gcc backend ship. The cdylib model has the benefit of decoupling Sky's backend from rustc's binary (a Sky user could install vanilla nightly + Sky as separate things). But Sky is already forking, so vanilla rustc is not on the table. The cdylib benefit doesn't apply. Static linking has matching benefits:

- One binary, one toolchain. Users install a rustup toolchain; everything is in it.
- No `dlopen` complexity, no "what if the cdylib was built against a different rustc version" failure mode.
- LLVM linkage is automatically consistent. Sky's codegen backend and rustc's `LlvmCodegenBackend` share LLVM unambiguously; cranelift has had bugs in this area when shipping as cdylib.
- Sky's frontend (which Sky's codegen backend calls during per_instance_mir queries and during the consumer-codegen phase) is in the same address space as rustc's `TyCtxt`. No IPC, no marshaling, no separate process boundary.

The cost: Sky's binary is larger than vanilla rustc's. Not by much (Sky's frontend + codegen backend are not large in compiled size compared to rustc itself), and not in a way users will care about. Sky's toolchain includes rustc + cargo + skyc + LLVM shared libraries; Sky's `rustc` being a few MB larger is invisible.

### 4.2 Two binaries: `skyc` (orchestrator) and `rustc` (forked compiler)

Sky's toolchain contains two distinct user-invokable binaries:

- **`skyc`** — the orchestrator. Parses `sky.toml`, generates the `.skybuild/` workspace, spawns cargo, copies the resulting binary out. Users invoke `skyc build`, `skyc test`, `skyc run`, `skyc check`, `skyc publish`, `skyc fmt`, `skyc new`, `skyc add`, `skyc inspect`. This is the user-facing entry point.
- **`rustc`** — Sky's forked rustc, statically linked with Sky's codegen backend and frontend. Cargo invokes this whenever cargo needs to compile a crate. Users typically don't invoke it directly; cargo does, automatically, because `rust-toolchain.toml` pins the Sky toolchain.

Both binaries are built from the same source tree and share Sky's core library (`libsky_core.rlib`) as a static dependency. Disk space cost is modest; the two-binary structure matches Rust-land convention (rustc, cargo, clippy-driver, rustfmt are separate binaries; Sky's skyc is the analog of cargo plus rustfmt plus clippy-driver all in one).

**Why two binaries rather than one with subcommands:**

- Clear mental model. `skyc` is what users run; `rustc` is what cargo runs. No "skyc has a hidden rustc mode" weirdness.
- Matches Rust convention. Rust users know to expect `rustc` as the binary cargo invokes.
- Easier to debug. When a build fails, the stack trace clearly originates in either `skyc` (orchestration issue) or `rustc` (compilation issue).
- Disk overhead is trivial. Both binaries are mostly Sky-core code; the marginal cost of having two binaries with similar content is microscopic.

### 4.3 Rustup toolchain model

Sky distributes as a rustup toolchain. The toolchain directory structure:

```
sky-toolchain/
  bin/
    rustc                    # Sky's forked rustc + Sky codegen backend + frontend
    cargo                    # vanilla cargo from the upstream nightly Sky tracks
    skyc                     # the orchestrator
    rustdoc                  # for doc generation (vanilla)
    rustfmt                  # optional, for source formatting (vanilla or Sky-specific)
  lib/
    librustc_*.dylib/so      # rustc's internal libraries (LLVM-shared)
    rustlib/                 # target-specific stdlib rlibs
  share/
    doc/                     # documentation
    rustup/                  # rustup metadata
```

**Installation paths:**

1. **`rustup toolchain link`.** Sky author distributes the toolchain as a tarball or as a directory. User downloads, extracts, runs `rustup toolchain link sky-nightly /path/to/sky-toolchain`. The Sky toolchain is then selectable via `rustup default sky-nightly` or via `rust-toolchain.toml`.
2. **Custom rustup distribution server.** Sky maintains a server hosting Sky toolchains in the rustup distribution format. Users add the Sky distribution server to their rustup configuration: `rustup self update --download-server https://sky-lang.org/rustup`. Then `rustup install sky-nightly` works. This is the polished v1.x path; v0/v1 ships with rustup toolchain link.
3. **Single-binary installer for non-rustup users.** A standalone installer script that downloads the toolchain and configures `~/.sky/bin` in PATH. Bypasses rustup; useful for users who don't already have rustup installed. v1.x feature.

A Sky project's `.skybuild/rust-toolchain.toml` pins `channel = "sky-nightly"`. Cargo invocations inside `.skybuild/` automatically use the Sky toolchain. Outside `.skybuild/` (i.e., the user's regular Rust projects), cargo uses whatever toolchain that project pins. Sky's installation doesn't interfere with the user's existing Rust setup.

### 4.4 Pass-through invariant for pure-Rust crates

When Sky's `rustc` is invoked by cargo to compile a crate that has no Sky involvement (no `.sky` source, no `__SKY_STUBS_MARKER` in the crate), Sky's machinery must produce byte-identical output to vanilla nightly rustc for the same inputs.

This is a *testable* invariant. Sky's CI runs a corpus of Rust crates through:
1. Vanilla nightly rustc (the upstream nightly Sky tracks).
2. Sky's `rustc` binary (forked + Sky machinery dormant).

Output objects are byte-compared. Any divergence is a regression.

**The corpus.** A representative set of crates covering: small (a hello-world bin), medium (a serde-derive consumer), large (a tokio-based async program), generic-heavy (an iter-pipeline-heavy program), trait-heavy (a multi-trait coercion program), build-script-using (a sys-crate wrapper). Each corpus crate is built, the resulting `.rlib`/`.bin` is hashed, and the hash is compared against the vanilla-baseline hash.

**Mechanism for the invariant.** Sky's `rustc` checks `__SKY_STUBS_MARKER` early in startup (after argv parsing, before any Sky-specific machinery installation). If the marker is absent on the crate being compiled, Sky's machinery does not install: per_instance_mir provider is the default (returns `None`), Sky's codegen backend's `init()` is a no-op, the rest of the compile proceeds via Sky-binary-but-not-Sky-active path.

The pass-through path must be airtight. Three concrete risks:

1. **Side effects from Sky's `init()` or marker check.** Any environment-variable writes, file-system touches, or process state changes in Sky's startup before the marker check produce divergence. Sky's startup is structured to do nothing observable until after the marker check.
2. **Differences in arg parsing.** Sky's binary parses argv the same way vanilla rustc does; specifically, the same argv parser code is shared (rustc's argv parsing happens before custom Callbacks fire). No drift.
3. **Differences in panic handler installation.** Sky installs its panic handler conditionally on marker presence. With no marker, the panic handler is whatever vanilla rustc installs.

The byte-identical invariant is the hardest invariant in this document. Maintaining it is a continuous discipline.

### 4.5 Marker-based per-crate activation

Sky's machinery activates only for crates that contain the `__SKY_STUBS_MARKER` item. The marker is a `pub const __SKY_STUBS_MARKER: () = ();` declaration at the crate root. Skyc emits this into every generated stub rlib's `lib.rs` automatically.

**Detection.** Sky's `rustc`, at startup, runs:

```
1. Parse argv.
2. Identify the crate being compiled (CARGO_PKG_NAME or the --crate-name arg).
3. After rustc's normal parsing and expansion (Callbacks::after_expansion), check the local crate's items via tcx.module_children_local(CRATE_DEF_ID).
4. If any item is named `__SKY_STUBS_MARKER` and is in the value namespace, Sky's machinery activates.
5. Otherwise, Sky's machinery stays dormant; the compile proceeds vanilla.
```

The detection happens after `after_expansion` rather than at startup because rustc needs to have parsed the crate to know its items. The very first Sky-machinery installation step (registering Sky's query providers — `per_instance_mir`, `layout_of`, `collect_and_partition_mono_items`, `cross_crate_inlinable` — and the `fill_extra_modules` codegen hook) is gated on the detection result. (`mir_shims` was previously in this set; retired 2026-06-23 — see §15.7 / §F.18. `symbol_name` was also in this set; retired 2026-06-24 Phase F — see §5.4 / §26.1.)

**Why per-crate marker rather than per-invocation env var.** Earlier in the design conversation, the activation mechanism was `CARGO_PRIMARY_PACKAGE=1`. That's cargo's signal that this is the primary workspace package. The problem: a published Sky library, depended on by a user's Sky project, gets built by cargo as a normal dep — `CARGO_PRIMARY_PACKAGE` is unset. The Sky lib has `.sky` source that needs Sky processing, but with `CARGO_PRIMARY_PACKAGE` unset, Sky's machinery would stay dormant. The Rust stub bodies (`unreachable!()`) would be codegenned into the rlib, and runtime calls would panic.

The marker-based detection is per-crate. Every Sky stub rlib has the marker, regardless of whether it's the primary package or a transitive dep. Sky's machinery activates on every Sky-marked crate compile. Pure-Rust crates (which never have the marker) stay byte-identical to vanilla.

**Caching the marker check.** Within a single Sky `rustc` invocation, the marker check fires once at startup. Across multiple cargo-invoked rustc subprocesses (one per crate in the build graph), each subprocess does its own marker check; the check is per-crate-load cheap (a single `module_children` walk).

For checking *upstream* crates' markers (when loading rlib metadata), Sky maintains a `HashSet<CrateNum>` of "known Sky stub rlibs" populated lazily on first item-from-crate query. Subsequent queries about the same crate are O(1). The detection mechanism scales to large dep graphs.

**Gotcha: glob re-exports across Sky deps.** The marker check must verify the marker DefId's parent crate (`def_id.krate`) matches the crate being inspected — not just that a symbol named `__SKY_STUBS_MARKER` is visible from the crate's `module_children`. The reason: a downstream Sky crate that does `use sky_lib::*;` (or any glob re-export from a Sky lib) inadvertently re-exports `__SKY_STUBS_MARKER` into its own crate root. A naive `module_children` walk that just looks for the symbol name would falsely flag the downstream crate as a Sky stub rlib. The consequences are immediate and bad: Sky's `collect_and_partition_mono_items` filter would remove the downstream's own items (including its `fn main`) from rustc's CGU list, so rustc's LLVM backend would emit no `.o` symbol for them, and the link would fail with an undefined `_main` symbol. The fix is a one-line DefId-parentage check (`def_id.krate == crate_num`). Toylang's empirical hit on this trap (Session 11, during the marker-detection migration) is recorded here so future implementers don't skip the check. The trap also extends to other marker conventions Sky might add later — anything name-based that's matched via cross-crate item iteration should include the parentage check.

### 4.6 Cross-references

- Section 6.3 — the `__SKY_STUBS_MARKER` item and its emission by skyc.
- Section 5 — the codegen backend's init, query overrides (notably the `collect_and_partition_mono_items` filter that suppresses rustc's `.o` emission for consumer items), and the `fill_extra_modules` hook.
- Section 18 — the cargo orchestration that produces the `.skybuild/` workspace.
- Section 25 — the per-bump test invariants including the byte-identical pass-through check.

---

## 5. The Codegen Backend

Sky's compiler integrates with rustc as a full `CodegenBackend` plugin, statically linked into the forked rustc binary. This chapter covers the backend's interface, its mechanism, and how it eliminates the partitioner-timing risk (B2) that erw's partitioner-override mechanism carries.

### 5.1 Why Option C (full CodegenBackend plugin) over partitioner-override

The design conversation considered three options for Sky's emission control:

- **Option A: Partitioner-override emission-skip.** Sky overrides `collect_and_partition_mono_items` via `Config::override_queries`, delegates to upstream's partitioner, filters Sky items out of the returned CGU list before LLVM codegen sees them. This is erw's current shipping mechanism (post-2026-06-22 retirement of Option 4). A brief 2026-06-21 → 2026-06-22 experiment with Option 4 (a `codegen_fn_attrs` query override stamping `AvailableExternally`) was reversed when the AvailableExternally body was found to create a CGU-placement hazard that required rustc-fork patch 5 to paper over — see §F.14.1 / §F.17 for the design history.
- **Option B: Trampoline emission via stub bodies.** Sky's per_instance_mir returns a body that calls an extern "C" target; rustc compiles the trampoline at the stub symbol; Sky emits the real body separately at the extern symbol; both are linked together.
- **Option C: Full CodegenBackend plugin.** Sky's plugin *is* rustc's codegen backend. Sky controls emission directly. Sky decides which items to emit and at what linkage during `codegen_crate`. No partitioner-mutation timing assumption.

Sky chose Option C.

**Why not Option A:** the B2 partitioner-timing risk. Option A depends on a specific assumption about rustc's internal phases: that the plugin's `MonoItemData.linkage` mutation, performed after rustc's default partitioner runs, survives to LLVM emission. The mechanism is:

1. Rustc's default partitioner runs.
2. The default partitioner calls `mono_item_linkage_and_visibility`, which sets each MonoItem's initial linkage.
3. The default partitioner runs `internalize_symbols`, which downgrades linkage on items eligible for internalization (Hidden + `can_be_internalized`).
4. The default partitioner returns the CGU list.
5. Sky's override receives the returned list and mutates `MonoItemData.linkage` on the Phase-6-style generic wrappers in `__lang_stubs`, forcing `(External, Default)`.
6. LLVM emits according to the mutated linkage.

The assumption: step 6 reads the linkage from step 5, not from a re-derived call to `mono_item_linkage_and_visibility` (which would overwrite step 5's mutation). Erw verified this assumption empirically and ships on it. The risk per erw's risks.md: 20-30% probability of breakage over 5 years. The mechanism could change in any of four ways (internalization timing shifts, attribute-re-reading added, LLVM backend re-queries, query boundary restructures); any one breaks the mechanism silently. The break manifests as `__lang_stubs` items being internalized when Sky needed them external; Phase-6 unwrap tests fail with linker errors.

**Why not Option B:** Option B works for non-generics but breaks for generics. The trampoline body in the stub rlib has to call an `extern "C"` target. For a non-generic Sky item, that's fine — the stub rlib declares the extern, the trampoline calls it. For a *generic* Sky item, the extern target is different per monomorphization. Rust's `extern "C"` doesn't support generics. Three escape hatches, all bad:

1. **Pre-enumerate every monomorphization at stub-gen time.** Sky's stub_gen enumerates the closure of generic instantiations Rust might use and emits a separate extern decl per. Loses interleaving — Sky now needs to know the closure ahead of time.
2. **Per-monomorphization extern decls generated lazily during per_instance_mir.** Sky's per_instance_mir provider would generate new DefIds at substitution time, which the source-language layer (stub rlib's pre-existing items) can't carry.
3. **Indirect dispatch via a runtime function table.** Every Rust→Sky call goes through a table lookup. Defeats LTO; adds indirect-call overhead at every cross-language boundary.

Option B is unviable for Sky's generic-rich interop surface.

**Why Option C wins:**

- **B2 risk eliminates.** Sky's plugin owns codegen. Linkage is whatever Sky's plugin sets when emitting. No assumption about partitioner timing or LLVM re-reading.
- **The pattern is well-precedented.** Cranelift and the gcc backend both ship as `CodegenBackend` plugins. The trait surface (init, provide, codegen_crate, join_codegen, link) is documented and stable.
- **Sky controls Sky-item emission completely.** Sky's per_instance_mir produces synthetic bodies for rustc's collector walk; Sky's `codegen_crate` filters those items out of the LLVM backend's CGU list and emits them via Sky's Inkwell-based codegen path. Rustc never sees them at LLVM time.
- **Sky controls Rust-item emission via delegation.** Sky's backend wraps `LlvmCodegenBackend` and forwards every Rust item through it. Rust items get rustc's standard codegen path, unchanged.

The cost is the implementation work: a full `CodegenBackend` impl is ~300-500 lines of Rust, plus the cargo orchestration to invoke the forked rustc with Sky's backend active. The cost is paid once; the architectural risk it eliminates pays back continuously.

### 5.2 No B2 risk: Sky controls emission, no linkage mutation

A specific consequence of Option C worth pinning down: Sky's plugin never mutates `MonoItemData.linkage` post-partition. The mechanism that erw's risks.md §B2 describes does not apply to Sky in its full historical form.

> **Note (2026-06-22).** Erw briefly tried `codegen_fn_attrs` (Option 4) as an alternative to the partition filter — that mechanism didn't mutate linkage either; it stamped `AvailableExternally` via a sanctioned query override. The combination of Option 4 + patch 5 was retired 2026-06-22 because Option 4 created a CGU-placement hazard that required patch 5 to paper over. Erw is now back on the partition filter (a `collect_and_partition_mono_items` override that rebuilds CGUs with consumer items removed). The filter doesn't mutate linkage either — it filters items entirely before LLVM sees them. So B2 (the linkage-mutation timing risk) remains dissolved for both Sky and erw. What erw's filter DOES carry is a smaller residual B2 — fidelity to the upstream `CodegenUnit` shape when rebuilding (any new field on `CodegenUnit` that we don't copy = silent drop). The framing below preserves the original Sky-vs-erw contrast as design history; the residual risk is documented in §25.2 B2.

How Sky's plugin handles Phase-6-style generic wrappers (Sky's analog of erw's `__toylang_option_unwrap<T>` — generic Rust-syntax wrappers in the stub rlib that exist purely to give Sky a stable symbol to call into for items like `Option::unwrap` that have the `#[inline(never)]` attribute or other emission-affecting properties):

1. Sky's stub_gen emits these wrappers as ordinary `pub fn` items in the stub rlib's Rust source. They are real Rust functions that rustc compiles normally.
2. Rustc's collector walks them when Sky source calls them. The collector queues them for emission.
3. Sky's per_instance_mir provider is *not* installed for these items — they are not Sky-defined, they are Rust wrappers. Their MIR comes from rustc's normal path.
4. Sky's `CodegenBackend::codegen_crate` does *not* filter them out of the CGU list — they are not Sky items.
5. Sky's `CodegenBackend::codegen_crate` delegates the CGU containing the wrappers to `LlvmCodegenBackend::codegen_crate`. Rustc emits them via its normal path.
6. The wrappers get whatever linkage rustc gives them by default — typically `Hidden`, since they are `pub fn` in an rlib.
7. Sky's items reference the wrappers' symbols via Sky's codegen-emitted call sites. The wrappers are in the same crate as Sky's emitted code (in the binary's final `.o`), so the linker resolves the references intra-crate. `Hidden` linkage is sufficient.

The architecture difference: erw needed `External` linkage on the wrappers because erw's setup had the wrappers in `__lang_stubs` and consumers across a crate boundary calling them. Sky has the wrappers in the stub rlib and *Sky's emitted code in the same final binary* calling them — they're functionally co-located at link time even though they live in different intermediate artifacts. The default `Hidden` linkage works because the linker sees both ends at final link.

This is a real architectural simplification. Sky doesn't need the post-partition linkage mutation that erw needs. The B2 risk simply doesn't apply to Sky's design.

### 5.3 Suppressing rustc's `.o` emission for Sky items

**Sky overrides `collect_and_partition_mono_items` via `Config::override_queries`.** The override delegates to the default partitioner, then rebuilds each CGU with consumer-defined items removed. Rustc's LLVM backend processes the filtered partition normally and emits `.o` symbols only for items left after filtering — i.e. Rust items (including Phase-6 wrappers in the stub rlib). Sky's `fill_extra_modules` hook contributes the sole bodies for consumer items, with `External` linkage; the linker resolves cross-`.o` references at final link.

**The filter predicate (Phase C/D, 2026-06-24; handoff Decision 3).** The "which items get removed" decision is a two-gate attribute conjunction, not a structural / name-based union:

```rust
pub fn is_consumer_codegen_target<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    is_from_lang_stubs(tcx, def_id)                    // Gate 1: marker-bearing crate
        && tcx.has_attrs_with_path(def_id, &[          // Gate 2: tagged at emission
            Symbol::intern("skyc"),
            Symbol::intern("emit_consumer_body"),
        ])
}
```

Skyc's stub_gen emits `#![register_tool(skyc)]` at the stub source crate root (under `#![feature(register_tool)]`) and decorates each Category B item with `#[skyc::emit_consumer_body]` at emission time. Tool attributes encode cross-crate by default (rustc_metadata's `encode_cross_crate` returns `true` for any non-builtin attribute), so the predicate works at both stub-rlib compile and user-bin compile against any DefId.

**Category A vs Category B (the items the filter sees).** Items in the stub rlib fall into two buckets:

| Category | Examples | Tagged? | Filter behavior | Codegen source |
|---|---|---|---|---|
| **A: real Rust bodies** | `__SKY_STUBS_MARKER` const, `__ToylangOpaque<T>` wrapper struct, Sky struct declarations, `pub use` re-exports, `extern "C"` declarations, Phase-6 `__toylang_*_unwrap` `#[inline(never)]` helpers, `pub use core::ops::Drop` | **No** | LEAVE ALONE — survives the filter | Rustc's normal codegen (real source body or extern decl resolved at link) |
| **B: `unreachable!()` placeholders** | exported Sky fns + `__sky_main` (toylang's `__toylang_main`), accessor methods on Sky types, trait-impl methods on Sky types (incl. `<SkyType as Drop>::drop`) | **Yes** | REMOVE from CGU list | Sky's `fill_extra_modules` emits the real body with `External` linkage under the same rustc-mangled name (single-symbol architecture, §6.2) |

**The 1:1 invariant.** Every tagged item ↔ exactly one Sky emission per concrete Instance the cascade reaches. Skyc-side bugs in either direction are loud:
- **Tag without emission**: link error (rustc filters body out; Sky never emits → undefined symbol).
- **Emission without tag**: competing `.o` symbol (rustc emits the `unreachable!()` body, Sky emits the real body → linker error or, under LTO, non-deterministic IR-linker tiebreak that can pick the `unreachable!()` and panic at runtime).

The toylang reference implementation enforces this both ways via `stub_gen::tests::emit_consumer_body_tags_only_category_b_items` (every `unreachable!()` body within 6 lines below a tag; every tag within 8 lines above an `unreachable!()` body; exactly 3 tags on a representative registry).

**The same predicate must be used by both the partition filter AND the `per_instance_mir` override** — they must agree on what counts as consumer-owned. Sky's facade calls `is_consumer_codegen_target` from both sites; any future predicate variant would need a CI fence to keep them in lockstep.

> **Design history.** This file resurrected the partition filter from `a51bd7c~1` after a brief 2026-06-21 → 2026-06-22 detour through Option 4 (a `codegen_fn_attrs` override that stamped `AvailableExternally` linkage on consumer items so they were IR-only). Option 4 was smaller in pure LOC but created a CGU-placement hazard: rustc's partitioner could co-locate the AvailableExternally `unreachable!()` body with `main` in the same CGU, and LLVM's intra-CGU inliner at -O3 would inline the panic body into main → runtime SBMNBIZ. Rustc-fork patch 5 papered over this by causing the partitioner to put the AvailableExternally body in a separate CGU. Retiring Option 4 + patch 5 together (2026-06-22) dropped the fork from 5 → 4 patches and eliminated the @SBMNBIZ invariant entirely (no AvailableExternally body to protect against). See §F.14.1 + §F.17 for the design history; `tmp/patch5-empirical-2026-06-21/VERDICT.md` for the empirical contrast probe.

> **Predicate-shape history.** Pre-Phase-C/D (i.e. before 2026-06-24), `is_consumer_codegen_target` was a three-way union: `is_consumer_fn(name)` (name → universe lookup) || `is_consumer_accessor_safe(tcx, def_id)` (structural ADT walk + name check) || `is_consumer_trait_impl_method(tcx, def_id).is_some()` (structural walk for `impl <RustTrait> for <ConsumerType>` methods). The migration to the attribute-based two-gate conjunction (handoff Decision 3) shed the name-based universe dependency, the structural ADT walks, and the ambiguity between Drop-impl-bridge fns and Sky drop fns (both of which had similar name shapes). Phase F (2026-06-24) retired the `symbol_name` query override (handoff Decision 2) and with it `is_consumer_accessor_safe` (no remaining callers). `is_consumer_fn` stays alive in `queries/per_instance.rs` (callback-name routing into the consumer's dep-collection callback); `is_consumer_trait_impl_method` stays alive in the toylangc-side cascade drain at `consumer_fill_modules` (case 4 / case 6 trait-impl detection in the mono partition).

After the filter override is installed, the rebuilt CGU list reaches `LlvmCodegenBackend::codegen_crate` with consumer items already excised. Rustc compiles only the items that survive the filter (Rust items emitted as part of the stub rlib's source, like Phase-6 wrappers); consumer items never reach LLVM lowering at all.

**Sky's separate emission of Sky items happens via patch 4's `fill_extra_modules` hook**, not via `join_codegen` `.o` injection. The hook fires synchronously on the main thread inside `codegen_crate` *before* `start_async_codegen`; Sky's modules then ride rustc's standard optimize → ThinLTO-summary → emit pipeline as just-another-CGU. Cross-language inlining works because Sky's modules are in the same LTO pool as user-bin's bitcode and the Rust deps' rlib bitcode.

The orchestration:

```rust
impl CodegenBackend for SkyCodegenBackend {
    fn provide(&self, providers: &mut Providers) {
        self.inner.provide(providers);
        sky_install_query_overrides(providers);
        // Install patch 4's bitcode-contribution hook. (The setter itself
        // lives in rustc_codegen_llvm and is invoked from the driver's
        // config setup — see install_consumer_modules_hook in
        // rustc-lang-facade/src/extra_modules_hook.rs — not from provide().)
    }

    fn codegen_crate(&self, tcx: TyCtxt<'_>) -> Box<dyn Any> {
        // Pass-through. Sky's bitcode is contributed via the
        // fill_extra_modules hook installed at driver init.
        self.inner.codegen_crate(tcx)
    }

    fn join_codegen(&self, ongoing: Box<dyn Any>, sess: &Session, outputs: &OutputFilenames) -> (CodegenResults, FxIndexMap<WorkProductId, WorkProduct>) {
        // Pure pass-through. Sky's modules are already in `ongoing`.
        self.inner.join_codegen(ongoing, sess, outputs)
    }

    fn link(&self, sess: &Session, codegen_results: CodegenResults, metadata: EncodedMetadata, outputs: &OutputFilenames) {
        self.inner.link(sess, codegen_results, metadata, outputs)
    }
}

// The fill_extra_modules hook signature (Approach B / rev 2):
//
// fn consumer_fill_modules<'tcx>(
//     tcx: TyCtxt<'tcx>,
//     allocator: &mut dyn ExtraModuleAllocator<ModuleLlvm>,
// ) {
//     // Walk Sky's codegen queue (populated by the frontend at
//     // after_expansion), allocate one rustc-owned ModuleLlvm per Sky
//     // CGU via allocator.allocate(name), wrap its borrowed LLVMContext
//     // + LLVMModule pointers in suppressed-Drop Inkwell handles, emit
//     // IR directly. No bitcode serialization, no parse_from_tcx,
//     // no context migration. See §C.4 + §F.15 for the full pattern.
// }
```

The two intervention points are the `collect_and_partition_mono_items` override (filters consumer items out of rustc's CGU list so rustc never emits a competing `.o` symbol; B17) and the `fill_extra_modules` hook (Sky's bitcode contribution into rustc's pipeline; patch 4 / §F.15). The first uses a sanctioned mechanism (query override) and the second is the load-bearing fork patch (patch 4).

**Why not `.o` injection.** An earlier design considered emitting Sky items via Inkwell + `llc`, producing a `.o`, and injecting it through `join_codegen` as a `CompiledModule`. This works for correctness (the linker resolves everything) but leaves Sky's bitcode **outside** rustc's LTO module pool. Under `lto = "thin"`, ThinLTO sees user-bin's bitcode + Rust deps' rlib bitcode but NOT Sky's `.o` — no cross-language inlining between Sky bodies and Rust callers. Patch 4's hook puts Sky's bitcode in the LTO pool. The inlining test matrix (Thread C, `toylangc/tests/integration_projects/inlining/`) empirically verifies cross-language inlining at -O3 across every LTO mode (no_lto / thin / fat) — Sky's bodies constant-fold into Rust callers (see §F.2, §F.4). **Important historical correction:** the original `test_lto_smoke` fixture purported to verify this empirically but its assertion was vacuously satisfied for a long stretch (a `bl\t` substring check that didn't match the `b\t` tail-jump LTO actually emits). The matrix's Priority A surfaced the gap during the F1 investigation (2026-06-20); the underlying cause was `#[inline(never)]` on Sky-item stubs in `stub_gen` blocking the LLVM inliner. With that removed, the matrix's disassembly assertions catch any future regression.

**Critical implementation detail.** Patch 4 submits extras **before** `start_async_codegen`, not between the CGU loop and `codegen_finished`. The obvious-looking insertion point trips the coordinator's `main_thread_state == Codegenning` assertion. Submit synchronously on the main thread, mirroring how rustc handles the allocator module. (See §F.4.)

### 5.4 LlvmCodegenBackend delegation for Rust items

Sky's backend delegates every Rust-shaped operation to `LlvmCodegenBackend`. The trait's surface (~20 methods) is dispatched via a wrap-and-delegate pattern:

```rust
pub struct SkyCodegenBackend {
    inner: LlvmCodegenBackend,
}

impl SkyCodegenBackend {
    pub fn new() -> Self {
        Self { inner: LlvmCodegenBackend::new() }
    }
}

impl CodegenBackend for SkyCodegenBackend {
    fn init(&self, sess: &Session) {
        self.inner.init(sess);
        // Sky-specific init: install Sky's panic handler, configure logging,
        // etc. But only if the local crate has __SKY_STUBS_MARKER.
        if has_sky_marker(sess) {
            sky_init(sess);
        }
    }

    fn provide(&self, providers: &mut Providers) {
        // Inner's providers first.
        self.inner.provide(providers);
        // Sky's overrides:
        //   - per_instance_mir (Sky-defined items' synthetic dep-discovery body),
        //   - layout_of (for Sky types),
        //   - collect_and_partition_mono_items (restored 2026-06-22:
        //         filters consumer items out of the CGU list so rustc
        //         emits no .o symbol for them; the consumer's
        //         fill_extra_modules body is the sole def at link time),
        //   - cross_crate_inlinable + extern_queries.cross_crate_inlinable
        //         (B16: false in Sky-active compiles to keep real .o symbols
        //         for items Sky's call sites reference).
        // Retired (do NOT install):
        //   - symbol_name (retired 2026-06-24, Phase F — single-symbol
        //         architecture means Sky's bitcode emits each rustc-visible
        //         body under the same rustc-mangled name rustc's default v0
        //         mangler would give the stub fn; no override needed. See
        //         §6.2 + §26.1, handoff Decision 2.);
        //   - mir_shims (retired 2026-06-23, Phase E — drop semantics now
        //         flow through rustc's standard DropGlue path; per-Sky-type
        //         bodies come from a compiler-synthesized `Drop::drop(&local)`
        //         AST node inserted by `insert_scope_end_drops`; cascade
        //         discovery + fill_extra_modules emits the bodies. See
        //         §15.7 for the AST-rewrite mechanism, §F.18 for the
        //         implementation lessons.);
        //   - codegen_fn_attrs + extern_queries.codegen_fn_attrs
        //         (Option 4, retired 2026-06-22 alongside patch 5 — see
        //         §F.14.1 + §F.17 design history);
        //   - consumer_lang_active (patch-5 shim, retired 2026-06-22 with
        //         patch 5 removal from the fork);
        //   - upstream_monomorphizations / upstream_monomorphizations_for
        //         (A.2, retired 2026-06-21 in §5.5 Step 3).
        if let Some(sess) = current_session() {
            if has_sky_marker(sess) {
                sky_install_query_overrides(providers);
            }
        }
    }

    // codegen_crate, join_codegen, link as shown above.

    fn name(&self) -> &'static str { "sky" }

    // ... other methods delegate to inner unchanged.
}
```

The forked rustc binary, at startup, constructs `SkyCodegenBackend::new()` via `Config::make_codegen_backend = Some(Box::new(|opts, target| SkyCodegenBackend::new()))`. The construction is unconditional — Sky's backend is always the active codegen backend. The marker-based activation happens *inside* the backend's methods (init, provide), gating Sky-specific behavior on marker presence. When the marker is absent, Sky's backend is functionally identical to `LlvmCodegenBackend`.

### 5.5 `.o` emission point (owning-crate compile for non-generics, binary compile for generics)

Sky's `.o` content is produced at every crate compile where Sky's machinery activates. The post-Step-2 (2026-06-21) split:

- **Each library's compile** produces (a) the stub rlib's `.o` carrying rustc-compiled Rust-side machinery — including Rust generic intermediaries the cascade surfaces (e.g. `some_rust_lib::duplicate<Widget>`); the `pub fn ... { unreachable!() }` stub bodies for Sky exports are **filtered out of the CGU list** by Sky's `collect_and_partition_mono_items` override, so rustc emits no `.o` symbol for them — plus (b) Sky's `fill_extra_modules` contribution: real `External`-linkage bodies for the **non-generic** Sky items defined in this library, and for the cascade-discovered trait-impl monomorphizations whose impl lives in this library, and (c) a sidecar (Temputs for the library's full surface, used by downstream typechecking).
- **The final binary's compile** produces the binary's `.o`, carrying (a) the bin's Rust-emitted code, (b) Sky bodies for the binary's own non-generic items, and (c) Sky bodies for **generic** monomorphizations the binary reaches transitively (since the substituted instances don't exist at any library's compile — they materialize only when concrete args are supplied downstream).

This is the post-§5.5-Step-2 split. The locked architectural commitment is that Sky libraries do not ship precompiled bodies *for items whose final emission depends on the downstream's concrete args* — i.e. generic items, comptime-dependent items. Non-generic items, whose emission is the same regardless of downstream usage, emit at the owning crate's compile. Every Sky body in the final binary is still codegenned by *some* invocation of Sky's compiler from the library's Sky AST; the question Step 2 answered is "which invocation" — owning-crate for non-generics, binary for generics.

**Trade-off the split introduced.** Cross-library Sky-body inlining at `lto = false` (cargo dev profile default) is lost. Pre-Step-2, the binary's compile owned every Sky body and rustc's thin-local LTO (Level 3, §F.16) could inline Sky bodies into the bin's `main` because they were peer CGUs of the same invocation. Post-Step-2, non-generic Sky bodies live in the upstream rlib's invocation; cross-crate visibility requires `lto = "thin"` or `"fat"` (Levels 4–5). The 8 `_no_lto` inlining-matrix fixtures were flipped from "Sky body inlined / constant-folded" to "tail-jump present" to lock in this honest semantics. See §F.16 for the full ladder. Empirical perf cost (2026-06-24 benches, see §22.4): **~1.5× slowdown on hot Sky-call paths at `lto = false`** (Bench 1: O3 nolto 86.7ms vs O3 thin 57.9ms) and **~26× slowdown on drop-heavy paths** (Bench 3: O3 nolto 10.0ms vs O3 thin 0.4ms). The Bench 1 ratio matches what a pure-Rust cross-crate baseline shows (Sky's emission gives LLVM the same inlining opportunity Rust's does); the Bench 3 ratio is amplified because drop chains accumulate per-call overhead 10M-fold and disappear entirely when LTO inlines + vectorizes the empty body. **Recommendation: use `[profile.release] lto = "thin"`** for any perf-sensitive build.

**Qualifier — what "library-owned Sky bodies" does and doesn't mean.** The stub rlib's `.o` carries Sky-emitted bodies only for items owned by *that* library (non-generic exports + cascade-discovered trait-impl methods whose impl is in that library). It also carries Rust-side machinery rustc emits during its own flow:

1. **Generic Rust monomorphizations that Sky's `per_instance_mir` cascade surfaces** (e.g., `some_rust_lib::duplicate<Widget>` reached through a Sky synthetic body's ReifyFnPointer cast). Those Rust bodies *are* emitted into the stub rlib's `.o`. From the user-bin compile's perspective, `duplicate<Widget>` is then a normal upstream Rust monomorphization, resolved through the standard share-generics mechanism. Sky's `LangDriver::config` heuristic forces `share_generics = true` at every Sky stub rlib compile so rustc records these monomorphizations in the rlib's rmeta, where the downstream's cstore walk finds them naturally. No facade-side augmentation needed.
2. **The Sky stub fn shells in the stub rlib's Rust source** (`pub fn __sky_main() { unreachable!() }`). Rustc parses, type-checks, and borrow-checks these normally; they pass trivially. But the `collect_and_partition_mono_items` filter removes them from the CGU list before LLVM codegen, so rustc never emits machine code for them. Sky's `fill_extra_modules` contribution at the same compile session emits the real `External`-linkage body under the same rustc-mangled name (per §6.2's single-symbol architecture). The metadata declaring the items is in the rlib so downstream compiles can typecheck calls.

The "library emits its own non-generics" rule is what Step 2 added; the "binary emits generics" rule is the pre-existing downstream-substitutor model that's still in place for items whose monomorphization depends on the downstream's call sites. References ReifyFnPointer-style: ReifyFnPointer is used only for Rust-dep targets reached from Sky's synthetic body, not for Sky-internal callees (those have no rustc DefId — see the locked principle in chain context: "Sky's per_instance_mir at mono time has one job: walk Sky's call graph to report back the Rust things Sky transitively calls. Sky-internal callees are not its concern.").

**The implication:** compile cost is now distributed across the dep graph rather than concentrated at the binary. Each library compile pays Sky's codegen cost for the non-generic items it owns; the binary compile pays for its own non-generic items plus generic monomorphizations reached transitively plus cascade-surfaced Rust intermediaries. A library with 100 exported generic functions contributes its 100 AST entries to the binary's per_instance_mir work whenever the binary reaches any of them; a library with 100 exported non-generic functions pays its own codegen cost at its own compile and the binary only pays the link-time cost of the resulting symbols. Sky library authors should think about generic-vs-non-generic API surface design with this in mind.

Section 8.8 (no pre-computed layouts) carries the analogous decision for layouts, and the rationale is the same: ship the AST, recompute at consumer compile.

### 5.6 Cross-platform / cross-compile

Sky's backend supports cross-compilation in the standard rustc way: the user passes `--target=<triple>`. Rustc's normal cross-compile machinery handles target detection, target-specific sysroot selection, ABI configuration. Sky's emission of Sky items respects the target via the LLVM target triple and target data layout (Sky reads these from `tcx.sess.target` like any rustc-internal code would).

Cross-compile concerns specific to Sky:

- **Sky's runtime must be cross-compilable.** Sky's runtime (channels, async executor, allocator) is itself written in Sky and compiled per target. No special cross-compile machinery.
- **Sky's standard library must be cross-compilable.** Same.
- **Sky's intrinsics (slab operations, panic handler) must be present per-target.** Sky maintains a small runtime support library compiled into each Sky binary; this is per-target rustlib-style content shipped with the toolchain.

The cross-compile cost is the same as Rust's cross-compile cost: install the target's rustlib (`rustup target add aarch64-unknown-linux-gnu`), and Sky's toolchain ships analogous Sky rustlib content for each target. For v1, supported targets are limited to: x86_64-unknown-linux-gnu, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc. Section 28 covers phasing for additional targets.

### 5.6.5 LLVM and Inkwell version pinning

Sky's codegen uses Inkwell (Rust bindings to the LLVM C API) to emit LLVM IR. Sky's forked rustc uses `rustc_codegen_llvm`, which links against the same LLVM. **The two must use the same LLVM version, or runtime symbol-resolution failures occur.**

The pinning model:

- Sky's forked rustc tracks a specific nightly rustc version (Section 3.5). That nightly version determines the LLVM version (e.g., `nightly-2026-01-20` corresponds to LLVM 19).
- Sky's Inkwell dependency in skyc's `Cargo.toml` pins to a version compatible with the same LLVM (e.g., `inkwell = { version = "...", features = ["llvm19-0"] }`).
- Sky's CI verifies the LLVM version match by checking that the LLVM dynamic library Sky's `rustc` loads at runtime is the same one Inkwell expects to bind to.

**The dynamic library issue.** rustc statically links some of its LLVM code and dynamically links the rest (via `libLLVM-19.dylib` or equivalent). Sky's codegen uses Inkwell, which calls into the same LLVM dynamic library. The library is loaded once per process; both rustc's codegen path and Sky's codegen path share it. Same LLVM means same data structures; cross-path interaction is safe.

A mismatch produces obscure runtime failures: Inkwell calls into a `libLLVM` function that no longer exists or has different argument layout, segfault. Caught by Sky's CI (the test harness verifies that simple Sky code compiles cleanly; mismatched LLVM fails the test).

**Per-bump implication.** When Sky bumps its nightly pin, the LLVM version may change. Sky's CI re-runs to verify Inkwell still works with the new LLVM. Sometimes Inkwell needs a version bump too (it has its own LLVM-version-feature flags). Sky's bump procedure includes "verify Inkwell version" as a step.

**Cross-compile implication.** When cross-compiling, Sky targets a non-host LLVM target triple. LLVM itself is target-agnostic (the same LLVM library can emit code for many targets); the target is selected via the LLVM target machine. Sky's codegen passes the right target machine to Inkwell. The library is the same regardless of target.

### 5.6.6 Sky source path syntax and import resolution

Sky source uses dotted-path syntax for imports:

```sky
import rust.std.vec.Vec                    // Rust type
import rust.std.io.Write                   // Rust trait
import rust.tokio.spawn                    // Rust function

import sky.my_utils.Widget                 // Sky type from another lib
import sky.my_utils.module.helper          // Sky fn from a submodule

import self.internal_helper                // Sky item in current crate
import super.shared_state                  // Sky item in parent module
import crate.types.SharedType              // Sky item from crate root
```

The path syntax distinguishes namespace via the first segment:

- **`rust.`** — Rust crates. Resolves via cargo's dep graph + rustc's `module_children` walks. The path after `rust.` is the Rust path (`rust.std.vec.Vec` → `::std::vec::Vec` in rustc terms; `rust.tokio.spawn` → the `tokio` crate's `spawn` function).
- **`sky.`** — Sky libraries from cargo deps. Resolves via Sky's universe loaded from sidecars. The path after `sky.` is the Sky lib's name + qualified item path within the lib.
- **`self.`** — items in the current Sky file's module.
- **`super.`** — items in the parent module.
- **`crate.`** — items in the current crate's root.

The namespace separation makes the source unambiguous: a reader can immediately tell whether a name refers to a Rust item, a Sky item from another lib, or a Sky item in the current crate. The Sky frontend's name resolver routes each path through the appropriate resolution mechanism.

**Why dotted-path syntax rather than `::` like Rust:** Sky source visually distinguishes from Rust source. Sky users who skim both languages can tell which language they're reading without context. The dotted form is also slightly easier to type than `::`.

**Re-exports:** Sky source can re-export Sky items via `export use sky.my_utils.Widget` — making `Widget` available from the current crate under its short name. Standard re-export semantics.

### 5.7 Cross-references

- Section 3 (the fork) — patches that the codegen backend exploits.
- Section 6 (stub rlib model) — what gets filtered by the CGU filter.
- Section 8 (Temputs format) — the data Sky's codegen reads to know what to emit.
- Section 19 (per_instance_mir) — the mechanism the codegen backend's codegen depends on.
- Section 25 (risks) — the operational risks Sky's backend implementation pays attention to.

---

## 6. The Stub Rlib Model

This chapter covers how Sky-defined items are projected onto Rust-shaped surfaces for rustc to typecheck and (via `per_instance_mir` overrides) for rustc's mono collector to walk. The mechanism is the stub rlib: a generated Rust crate that contains Rust-source declarations of every export Sky item, with `unreachable!()` bodies that rustc compiles normally but Sky's codegen backend filters out before LLVM emission.

### 6.1 Per-Sky-library stub rlib (multi-rlib model)

Each Sky library compiles to its own stub rlib. A project with three Sky libraries — `my_app` (the bin), `my_utils` (a Sky library), `my_runtime` (another Sky library) — produces three stub rlibs:

- `my_app_stubs.rlib` — contains stub declarations for items in `my_app`'s Sky source.
- `my_utils.rlib` — contains stub declarations for items in `my_utils`'s Sky source.
- `my_runtime.rlib` — contains stub declarations for items in `my_runtime`'s Sky source.

(Naming convention: Sky libraries' stub rlibs are named directly after the Sky library — `my_utils.rlib`, not `my_utils_stubs.rlib`. The binary's stub rlib is named after the binary's crate: `my_app.rlib` if my_app is a bin. The "stubs" qualification is internal to skyc's bookkeeping.)

**Why per-library rather than one combined stub rlib:**

The single-rlib alternative would gather every Sky item (from every library in the project + every library's transitive deps + the binary's own source) into one Rust source file, compile it into one rlib. Simpler in the abstract; catastrophic for cargo's incremental compilation.

Per-library stub rlibs let cargo:

- Cache each library's stub rlib independently. When `my_utils` doesn't change, cargo skips re-compiling its stub rlib.
- Invalidate selectively. When `my_utils` changes, cargo invalidates `my_utils.rlib` and the binary's compile (which depends on it), but `my_runtime.rlib` is untouched.
- Parallelize compile jobs across libraries. Cargo's standard parallel compile model just works.

The single-rlib alternative forces a full recompile of every Sky item on every change, because cargo cannot tell what changed without parsing the giant combined source. Sky's compile times would scale with project size, not with diff size.

Per-library is locked. The cost (more disk usage in `target/` from multiple rlibs; slightly more cargo orchestration in `.skybuild/`) is small. The benefit (incremental compile that works) is large.

### 6.2 Export-only items in the stub rlib

The stub rlib contains declarations only for items marked `export` in Sky source. Non-export items are *not* in the stub rlib. Rustc literally cannot name them. They live entirely in Sky's universe (the sidecar) and in Sky's codegen output (the binary's final `.o`).

The mechanism: skyc's frontend, when generating the stub rlib's Rust source, walks the Sky library's items and emits a Rust declaration for each `export` item. Non-export items are skipped at stub-gen time. The stub rlib's source is small relative to the Sky library's total surface.

**What an export item generates:**

For an exported Sky function:
```sky
export fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}
```

The stub rlib generates:
```rust
#![feature(register_tool)]
#![register_tool(skyc)]
#![feature(rustc_private)] // for sky-specific items if any
#![feature(fn_traits, unboxed_closures)] // for closures that may flow through
// ... other features as needed

pub const __SKY_STUBS_MARKER: () = ();

pub struct Wrapper<T>(::std::marker::PhantomData<T>);

#[skyc::emit_consumer_body]
pub fn wrap<T>(x: T) -> Wrapper<T> {
    ::std::unreachable!()
}
```

The `#[skyc::emit_consumer_body]` tool attribute is what the partition filter keys on; see §5.3 for the two-gate predicate. Items in the stub rlib that AREN'T tagged (the marker const, the `__SkyOpaqueType` wrapper, Sky struct declarations, `pub use` re-exports, `extern "C"` blocks, the Phase-6 `__sky_*_unwrap` helpers) survive the filter and rustc compiles their real bodies normally — they're "Category A" per the §5.3 split.

For an exported Sky struct with public fields (note: Sky's "fields" don't surface to rustc by default; opacity is the default — see Section 10):
```sky
export struct Point {
    x: I32,
    y: I32,
}
```

The stub rlib generates:
```rust
pub struct Point(()); // Unit tuple struct — opaque to rustc.
                       // Layout supplied by Sky's `layout_of` override.
```

For an exported Sky trait impl on a Sky type:
```sky
export struct Widget {
    id: I32,
}

impl rust.std.clone.Clone for Widget {
    fn clone(&self) -> Widget {
        Widget { id: self.id }
    }
}
```

The stub rlib generates:
```rust
pub struct Widget(());

impl ::std::clone::Clone for Widget {
    #[skyc::emit_consumer_body]
    fn clone(&self) -> Widget {
        ::std::unreachable!()
    }
}
```

The Clone impl is in Sky's stub rlib because Sky owns the Widget type (orphan rule satisfied; Section 6.6). The method body is `unreachable!()` and the method is tagged `#[skyc::emit_consumer_body]` — Sky's `per_instance_mir` provider intercepts when rustc tries to use it, returning Sky's substituted body; Sky's `fill_extra_modules` emits the real body into the same compile session's `.o` under the rustc-mangled symbol; the partition filter (§5.3) removes the `unreachable!()` placeholder so it never reaches LLVM.

**Single-symbol architecture: Sky's bitcode emits each rustc-visible body under the *rustc-mangled name rustc would have given the stub fn*. Path B (empirically verified by toylang's `test_lto_smoke`).** The original design described a two-symbol scheme: stub fn `pub fn clone_widget` mangled by rustc as one name, Sky's bitcode emits the real body under a Sky-chosen name (`__sky_impl_clone_widget`), and the `symbol_name` query override redirects the rustc-mangled name to Sky's name at link time. **That scheme works under non-LTO but breaks under ThinLTO** — LLVM's IR linker sees two definitions of the logically-same function (the rustc-mangled stub with `unreachable!()` body, and Sky's `__sky_impl_*` with the real body) and non-deterministically picks the stub. Result: the binary panics at the inlined `unreachable!()`.

The fix is the single-symbol architecture: Sky's bitcode emits the real body under the **same rustc-mangled name rustc would have given the stub fn**. Only one definition reaches the LTO IR linker; Sky's body is the sole def; cross-language inlining works correctly.

**Phase F (2026-06-24, handoff Decision 2): the `symbol_name` query override is retired.** Pre-Phase-F, Sky installed a `symbol_name` override that did shape classification (`is_consumer_fn` / `is_consumer_trait_impl_method` / `is_consumer_accessor_safe`), built a callback name, asked the consumer for a symbol — but the consumer impl ignored the callback name and returned rustc's default mangler result. The classification work was entirely unused at the symbol_name layer. With the override retired, **rustc's default v0 mangler produces every consumer symbol directly**, and Sky's bitcode emits real bodies under that same name. Single-symbol architecture is now a property of "Sky reads `tcx.symbol_name(instance)` at every emission site" — no override, no bypass needed.

To compute the rustc-mangled name from Sky's side, call `tcx.symbol_name(instance)` directly:

```rust
fn sky_mangled_name_for(tcx: TyCtxt<'_>, instance: Instance<'_>) -> Symbol {
    tcx.symbol_name(instance).name
}
```

Pre-Phase-F, Sky's emission helper (`compute_fn_symbol` in `toylangc/src/toylang/callbacks_impl.rs`) went through a saved upstream provider pointer (`rustc_lang_facade::default_symbol_name()(tcx, instance)`) to bypass re-entrance through Sky's own override. Post-Phase-F there's no override to bypass — direct `tcx.symbol_name(...)` is the canonical path.

Sky-internal items (non-export, reached only through Sky's own call graph) keep using Sky-internal mangling — rustc never sees them, so there's no symbol-priority concern there. The single-symbol discipline applies only to rustc-visible items (exports + trait-impl methods on Sky types).

**Stub rlibs' `unreachable!()` bodies never reach LLVM.** Even with single-symbol naming, the stub rlib's `unreachable!()` body would still be a problem if it entered LLVM IR alongside Sky's real body — the IR linker might pick the wrong def. The shipping mechanism that prevents this is Sky's `collect_and_partition_mono_items` override: it filters consumer-defined items out of rustc's CGU list before LLVM codegen. The unreachable body's MIR still exists (rustc parses, type-checks, and borrow-checks the stub source), but it's never lowered to LLVM IR. Sky's `fill_extra_modules` hook contributes the real body with `External` linkage as the sole def. No IR linker race, no @SBMNBIZ invariant needed.

> **Retired layer:** an earlier belt-and-suspenders mechanism emitted `#![no_builtins]` at the stub rlib's crate root to skip its bitcode in `back/link.rs::ignored_for_lto` and keep it out of LLD's LTO pool entirely. That layer was retired 2026-06-21 once Option 4 made it redundant; the full inlining matrix passes without it. See §F.3 for the archive. Post-2026-06-22 (partition filter restored), the unreachable bodies don't reach LLVM at all, so the same redundancy holds for a different reason.

Cross-language inlining is unaffected — the stub rlib's `.o` carries Rust-emitted machinery (Rust generic intermediaries, Phase-6 wrappers) but no consumer-item bodies (the filter removed them). `pub use std::clone::Clone` re-exports don't carry bitcode of their own (the symbol's actual code lives in std, which participates in LTO independently). The inlining Sky cares about happens between Sky's `fill_extra_modules` bodies and Rust deps' bitcode at LTO time.

**Sky-emitted symbols do not need forward declarations in the stub rlib.** A natural temptation when adding a new emission shape is to also add an `extern "C" { pub fn __sky_<thing>(...); }` block to the stub rlib so that "rustc knows the symbol exists." Don't. The single-symbol architecture above is what makes it work: rustc resolves a Rust-source call `sky_lib::clone_widget(...)` to the stub rlib's `pub fn clone_widget` DefId; rustc's default mangler produces the symbol name; Sky's `fill_extra_modules` emits the body under that same name. The forward declaration adds nothing. Toylang carried such forward declarations as vestigial scaffolding from a pre-single-symbol-architecture era; removing them eliminated a generic-vs-non-generic asymmetry (extern "C" blocks can't contain generic items) without affecting any visible behavior. (Body-less Sky source declarations of *real* Rust functions — the analog of toylang's `fn println_int(x: i32);` syntax for binding to existing Rust functions — do still produce extern decls in the stub rlib, because those describe symbols the linker resolves to a Rust-defined body. That's an unrelated use case.)

### 6.3 `__SKY_STUBS_MARKER` for activation

Every generated stub rlib carries a marker item at the crate root:

```rust
pub const __SKY_STUBS_MARKER: () = ();
```

Sky's `rustc` (the forked one with Sky's codegen backend statically linked) detects this marker at startup. Marker present → Sky's machinery activates for this crate compile. Marker absent → Sky's machinery stays dormant; the compile proceeds vanilla.

The marker check uses `tcx.module_children_local(CRATE_DEF_ID)`, iterating root-level items and looking for one named `__SKY_STUBS_MARKER` in the value namespace. The check is O(N) in the count of root items, which is small. Cached per `CrateNum`.

**Why a marker item rather than a crate attribute:**

A `#![sky_stubs]` attribute would require a registered tool attribute or a built-in attribute (compiler-side support). The marker item works without any rustc-internal support — it's just a regular pub const. Visible from rustc's normal item-iteration machinery. Future-proof against rustc internal changes.

The marker item adds one extra item to every stub rlib (compile-time and disk-space cost: negligible). It is also visible to Rust code that depends on the stub rlib, which is fine — Rust code could in principle use the marker for runtime "is this a Sky lib?" checks, although that's not the intended use.

### 6.4 Skyc-generated; user never edits

The stub rlib is entirely generated by skyc. User never edits the generated Cargo.toml, the generated `src/lib.rs`, or any other generated file. The user's editing surface is `.sky` source files and `sky.toml`.

The generation is deterministic: same `sky.toml` + same `.sky` files → byte-identical generated stub rlib source. Section 18.5 covers this in detail; the rule is:

- No timestamps in generated files (no `// Generated at YYYY-MM-DD` comments).
- Sorted iteration order for any HashMap/HashSet content surfaced in output.
- Deterministic name generation for synthesized stubs (e.g., closure-lifted state machines named via stable hashes of source location, not random IDs).
- No host-system-dependent paths in generated source (use cargo-relative paths, not absolute paths).

Determinism is a testable invariant. Sky's CI builds a corpus of Sky projects twice (with cache wipes between) and byte-compares the generated stub rlib sources.

### 6.5 Stub rlib carries the Sky library's name directly

The stub rlib is named exactly as the Sky library is named. `my_utils` library's stub rlib is `my_utils.rlib`. Rust users who depend on a Sky library write `use my_utils::Foo` naturally — no `_stubs` suffix.

**Implication:** the `is_from_sky_stubs(tcx, def_id)` predicate (Sky's analog of erw's `@DPSFDOZ` mechanism) cannot rely on crate name matching `"__lang_stubs"`. Sky needs a different "is this a Sky stub rlib?" mechanism.

The mechanism: marker-item detection. `is_from_sky_stubs(tcx, def_id)` checks whether the crate containing `def_id` (i.e., `def_id.krate`) has the `__SKY_STUBS_MARKER` item at its root. The check is performed via `module_children` (cross-crate, since the crate may be an upstream rlib).

```rust
fn is_from_sky_stubs<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> bool {
    let crate_num = def_id.krate;
    // Cache the result per CrateNum.
    SKY_STUBS_CRATES.with(|cache| {
        *cache.borrow_mut().entry(crate_num).or_insert_with(|| {
            let crate_root = DefId { krate: crate_num, index: CRATE_DEF_INDEX };
            tcx.module_children(crate_root).iter().any(|child| {
                child.res.opt_def_id().is_some()
                && tcx.opt_item_name(child.res.def_id()) == Some(Symbol::intern("__SKY_STUBS_MARKER"))
            })
        })
    })
}
```

(Sketch; exact rustc API will drift across nightlies.)

The check is O(N) on first call per crate, O(1) on subsequent calls. The cache is per `TyCtxt` invocation (Sky's session-scoped cache, populated on first per-crate query).

### 6.6 Cross-rlib orphan rule (Path 1: match Rust's exactly)

Sky implements Rust's orphan rule unchanged. An impl block can exist only in the crate that owns either the trait or the type. Sky's typechecker enforces this Sky-side, producing errors in Sky terms.

**Why match Rust's rule rather than relax it:**

Three reasons.

1. **The interop story does not require relaxation.** The seven-case taxonomy (Section 2) was walked against the orphan-rule constraint. Every locked Sky interop case (1b, 3, 4, 5, 6) falls in an allowed combination: the trait is in one crate, the type is in another, the impl is in either of those two crates. Cases where someone wants to `impl ForeignTrait for ForeignType` (the only orphan-rule violation) don't appear in Sky's interop story; they're handled by the same idioms as in Rust (newtype wrappers, extension traits).
2. **Coherence is a real correctness invariant for separately-compiled code.** Rust's orphan rule is the mechanism that prevents incoherent linkage. Sky has the same separately-compiled-libs problem; it needs the same kind of solution. Relaxing the rule pushes the coherence problem onto Sky without giving Sky tools to solve it.
3. **Compile-time-metaprogramming changes the calculus eventually, not now.** Sky's comptime could in principle express "this impl wins if no other lib also impls this trait for this type, checked at link" — a link-time coherence rule expressed in Sky's metaprogramming surface. But that's a future feature. For v1, match Rust.

**The five idioms that make Path 1 livable:**

- **Newtype with cheap delegation.** Sky's `struct MyVec(Vec<i32>)` should auto-implement passthrough for everything on `Vec<i32>` unless overridden. Sky's `impl` macros / derive system makes this one-liner. Rust has `#[repr(transparent)]` + macros; Sky can do better with proper language support.
- **Extension trait pattern.** "I can't impl ForeignTrait for ForeignType, but I can define MyExt with the same method signatures and `impl MyExt for ForeignType`." Sky's trait system + Sky-owned-trait rule make this clean.
- **Top-level binary's stub rlib counts as local.** The user binary's stub rlib (e.g., `my_app.rlib`) is a Sky-owned crate. Impls in the user's `main.sky` source live in that rlib. Common patterns (`impl Display for MyConfig` in user code) work naturally.
- **Sky's typechecker emits the orphan-rule error in Sky terms.** Don't let users discover orphan-rule violations via rustc's error message on the generated stub rlib. Sky's frontend has the full picture and can point at Sky source with a workaround suggestion.
- **`#[fundamental]` for Sky's `&G T`-style references.** Rust has `#[fundamental]` for `&T` and `Box<T>` to allow narrow exceptions; Sky inherits the same convention for its own reference-and-owned-pointer types.

**Closures and async lift to named types in the source's stub rlib.** A closure in `my_utils/src/foo.sky` becomes `__closure_42` in `my_utils.rlib`. The `Fn`/`FnMut`/`FnOnce` impls live alongside. The closure type's owning crate is the same as the impl's owning crate. Orphan rule satisfied. Similarly, `async fn` desugars to a named state machine type in the source's stub rlib; the `Future` impl lives alongside. Owns the type. Section 14 covers both in detail.

### 6.6.5 Phase-6 generic wrappers in the stub rlib

Some Rust items cannot be called directly by Sky source through normal extern-declaration mechanisms. The canonical example is `Option::unwrap` — it's `#[inline(never)]` *sometimes* depending on the stdlib build profile, it has `#[track_caller]` semantics, and its symbol may or may not be present in the linked binary depending on whether some other Rust code happened to call it. Sky's call sites need a stable, predictable symbol to link against.

The solution Sky inherits from erw (Phase 6, see `risks.md §B2` historical context): emit `#[inline(never)]` generic wrapper functions in Sky's stdlib's stub rlib. The wrappers have stable symbols that Sky's codegen can always find:

```rust
// In Sky's stdlib stub rlib (skyc-generated):
#[inline(never)]
pub unsafe fn __sky_option_unwrap<T>(o: *mut ::std::option::Option<T>) -> T {
    ::std::ptr::read(o).unwrap()
}

#[inline(never)]
pub unsafe fn __sky_result_unwrap<T, E: ::std::fmt::Debug>(r: *mut ::std::result::Result<T, E>) -> T {
    ::std::ptr::read(r).unwrap()
}

// ... similar wrappers for other "tricky" stdlib operations ...
```

Sky source that calls `option.unwrap()` is desugared by Sky's frontend to a call to `__sky_option_unwrap<T>(ptr_to_option)`. The wrapper is generic; rustc instantiates it per concrete T. The wrapper body is `#[inline(never)]` so the symbol survives; rustc inlines the inner `.unwrap()` call inside the wrapper body, so optimization isn't lost. `#[track_caller]` falls out for free because the wrapper injects the location.

**Linkage discipline.** These wrappers are *not* Sky-defined items in Sky's universe; they are Rust source in the stub rlib. Rustc compiles them normally via the standard `optimized_mir` → mono walk → codegen path. Sky's `collect_and_partition_mono_items` filter does *not* remove them because they aren't consumer items per `is_consumer_codegen_target`. They reach LLVM and emit normally with whatever linkage rustc gives them by default. For a generic `pub fn` in an rlib, that's typically `Hidden`. Sky's binary-emitted code references them through extern declarations; the final binary's link resolves the references intra-binary (since Sky's `.o` and rustc's `.o` for the stub rlib's wrappers are both in the same binary at link time).

**This is where erw's risks.md §B2 doesn't apply to Sky.** Erw needed `External + Default` linkage on the wrappers because erw's setup had the wrappers in `__lang_stubs` and *consumers across crate boundaries* calling them. Sky's setup — Sky's emitted code in the same binary as the wrappers — needs only `Hidden`. The post-partition linkage mutation is unnecessary; the default linkage works. The B2 risk dissolves by architectural choice.

**Open: which Rust stdlib operations need wrappers.** v1 ships wrappers for `Option::unwrap`, `Result::unwrap`, `Option::expect`, `Result::expect`, and a small set of related panic-prone operations. Sky's stdlib team maintains the list. Additional wrappers can be added without breaking architectural invariants.

**Distinction from the F1 finding (2026-06-20).** The `#[inline(never)]` on Phase-6 wrappers above is a SEPARATE discipline from the historical `#[inline(never)]` on Sky-item stubs (accessor methods, wrapper functions, trait-impl methods in `stub_gen`). The historical attribute on Sky-item stubs was removed during F1 because: (a) its stated rationales (share-generics gate bypass, MIR inliner leak protection) were obsolete (patch 5 era / §F.1 respectively; patch 5 itself retired 2026-06-22), (b) its v2-precompiled-bodies concern (B12) is gated by build.rs in v1, and (c) it blocked LLVM's cross-language inliner. The Phase-6 wrappers' `#[inline(never)]` is retained because it serves an unrelated purpose: making the wrapper's symbol stably nameable from Sky's bitcode regardless of whether other Rust code happens to have called the wrapped stdlib operation. When v2 precompiled-bodies work begins, both `#[inline(never)]` policies should be re-evaluated together.

### 6.7 Sky source file ships alongside

Every published Sky library ships its `.sky` source files alongside the generated artifacts. The cargo package layout for a published Sky library:

```
my_utils/
  Cargo.toml                # skyc-generated
  src/
    lib.rs                  # skyc-generated Rust stub source
    lib.sky                 # user-authored Sky source (shipped verbatim)
    [other .sky files]      # user-authored
  Cargo.lock                # not in published package, generated downstream
  build.rs                  # skyc-generated, enforces Sky toolchain presence
  my_utils.sky-meta         # sidecar (Temputs blob) — adjacent to the generated rlib
```

The `.sky` source is shipped because:

- **User inspection.** Users browsing a Sky library on crates.io can read the source. Critical for understanding what a library does without running it. Critical for security review.
- **Source-level debugging.** Debug symbols in the final binary reference `.sky` source lines. The source files must be findable.
- **IDE / tooling.** Rust-analyzer (or a future Sky-analyzer) can show Sky source on hover. Without the source, users have to chase down the source separately.
- **No closed-source Sky libraries in v1.** A future feature might allow source-less Sky libs (ship only the sidecar + Rust stubs), but v1 always ships source.

The disk-space cost is modest. Sky source is text; cargo packages compress; published Sky libraries are typically a few KB to tens of KB.

### 6.8 Cross-references

- Section 8 — what's in the sidecar (Temputs) and what's not.
- Section 9 — the export keyword and its semantics.
- Section 10 — what types look like in the stub rlib (opaque, with layout supplied via override).
- Section 18 — cargo orchestration that produces the stub rlib via skyc.
- Section 21 — the distribution model for published Sky libraries.

---

## 7. The Sidecar

The sidecar is Sky's per-library binary blob containing the Temputs (Sky's typing-pass output) for every item in the library — exports and non-exports both. The sidecar lets downstream consumers (the binary's compile, or another Sky library that depends on this one) reconstitute Sky's universe without re-parsing source.

### 7.1 Sidecar location and naming convention

The sidecar file is adjacent to the rlib, with a `.sky-meta` extension. For `my_utils.rlib`, the sidecar is `my_utils.sky-meta`. Both files live in cargo's target directory or in the published cargo package.

```
target/deps/
  my_utils-abc123.rlib
  my_utils-abc123.sky-meta
```

When Sky's `rustc` loads an rlib at crate-load time, it checks for an adjacent `.sky-meta` file with the same basename. Present → load the sidecar into Sky's universe. Absent → either it's not a Sky lib (no marker check is needed; the rlib doesn't have the marker), or it's a Sky lib with a missing sidecar (error: "Sky sidecar missing for `my_utils`; required for compilation").

**Why sidecar-adjacent rather than embedded in the rlib:**

Earlier in the design conversation, two options were considered:

- **Approach B: embed the sidecar in the rlib as a custom section.** Rlibs are `ar` archives; adding a `sky-meta.bin` entry alongside Rust's `rmeta` is mechanically straightforward.
- **Approach C: ship the sidecar as a separate file alongside the rlib.**

The user picked C. Reasoning: easier to inspect (a `.sky-meta` file at a known path can be examined with `skyc inspect`; an embedded section requires extraction first); cleaner missing-file failure mode (if the sidecar is missing, the error is "file not found at this path" — obvious; if the embedded section is missing, the error is "no sky-meta section in rlib" — less obvious); cargo's package mechanism ships both files naturally via the `include` field.

The cost of C: one more file per Sky library on disk and in cargo packages. Modest.

### 7.2 Versioned header

The sidecar starts with a versioned header:

```
Bytes 0-3: Magic number "SKYM" (0x534B594D)
Bytes 4-7: skyc_version_major (u32, LE)
Bytes 8-11: skyc_version_minor (u32, LE)
Bytes 12-15: format_version (u32, LE)
Bytes 16-23: capabilities_bitset (u64, LE)
Bytes 24-31: payload_offset (u64, LE)
Bytes 32-39: payload_length (u64, LE)
Bytes 40-47: payload_checksum (u64, LE, BLAKE3-truncated)
Bytes 48+: padding to 64-byte alignment
Bytes 64+: payload (encoded Temputs)
```

The header is fixed-size and trivially decodable. The payload starts at a 64-byte-aligned offset, allowing potential memory-mapping of the payload directly.

**`skyc_version`** is the version of skyc that produced the sidecar. Used for diagnostics ("This sidecar was produced by skyc 0.5.3").

**`format_version`** is the version of the Temputs binary format. Different from skyc_version because the format can stay stable across skyc releases that only fix bugs or extend features in backwards-compatible ways.

**`capabilities_bitset`** is a u64 of feature flags. Bits indicate which optional capabilities the payload uses (e.g., bit 0: contains comptime synthesis recipes; bit 1: contains async state machine descriptions; bit 2: contains user-extended-stdlib annotations). Consumers can quickly check whether they support the sidecar's capabilities without parsing the whole payload.

**`payload_checksum`** is a BLAKE3 hash of the payload bytes, truncated to 8 bytes. Used to detect corruption (the sidecar was truncated, or its bytes were modified). On read, skyc recomputes the hash and compares; mismatch → "sidecar corrupted" error.

### 7.3 Serialization format recommendation

The sidecar's payload format is a recommendation (not yet locked): **bincode + custom-serializable types**.

Bincode is a binary serialization format from the Rust ecosystem with the properties Sky needs:

- Deterministic: same input produces same bytes.
- Self-describing-enough: with `#[derive(Serialize, Deserialize)]` on Sky's Temputs types, the format is derived directly from the type structure.
- Efficient: binary, compact, fast to read/write.
- Mature: years of production use in Rust projects.
- Schema evolution via versioning: changes to the type require a format_version bump; readers that don't understand the new format error out cleanly.

Alternatives considered:

- **Cap'n Proto.** Zero-copy reads, schema-evolution-friendly. Heavier toolchain (schema compiler). More complex to integrate. The zero-copy benefit isn't important — sidecar reads happen once per crate load, not per-query.
- **FlatBuffers.** Similar to Cap'n Proto, slightly more mainstream in non-Rust contexts. Same trade-off.
- **Protocol Buffers.** Most mature for cross-version evolution. Runtime cost is real but bounded. Wide tooling. Recommended if developer familiarity matters more than tightness.
- **Postcard.** Sky-ecosystem-style serializer. Similar to bincode but no_std-friendly.

Bincode is the recommendation because it minimizes integration complexity and Sky's typing pass already operates over Rust-shaped data structures. The actual choice (bincode vs alternatives) is deferable to implementation time without changing the architecture.

### 7.4 Determinism requirement

The sidecar is byte-deterministic given Sky source input. Same `.sky` files → byte-identical sidecar.

The determinism is enforced by:

- Sky's typing pass producing deterministic output (no HashMap iteration order in serialized content; sorted iteration where collections are involved).
- The serialization format being deterministic (bincode is).
- No timestamps, no host-system-dependent content, no random IDs in the payload.

This is a CI-testable invariant. Sky's CI builds a corpus of Sky projects twice (with cache wipes between) and byte-compares the produced sidecars.

Determinism enables:

- Reproducible builds: a Sky binary built today can be byte-reproduced at any later time given the same source and toolchain.
- Cargo's incremental compile correctness: cargo's fingerprinting can hash the sidecar; if the hash matches a prior build's hash, cargo can skip recompiling downstream consumers.
- Content-addressed typeids (Section 10.8): the typeid for a Sky-defined type is a hash of its source path + structure; consistent typeids across Sky compiler invocations require deterministic source-to-output mapping.

### 7.5 Backward compatibility: design now, implement at 1.0

The sidecar format carries a `format_version`. Pre-1.0 skyc:

- Refuses to load any sidecar whose `format_version` is different from the current skyc's expected version. Error: "Sidecar `my_utils.sky-meta` is format version 5; this skyc supports format version 7. Please rebuild `my_utils` with a matching skyc version."
- This is strict but predictable. Sky users in v0/v1 know to keep their toolchain consistent across the project. They cannot mix-and-match Sky library versions built with different skyc versions.

At Sky 1.0, the policy changes. Skyc 1.x:

- Reads sidecars with format_versions in the range `[N, M]` for some N ≤ current version ≤ M.
- For sidecars with `format_version < current`, applies a migration path: a sequence of format-version-translation functions that bring an older format up to current. The migration is read-only (the sidecar on disk is unchanged); the in-memory representation matches current.
- For sidecars with `format_version > current` (the consumer is newer than the producer is — common: consumer downloaded a newer Sky lib that doesn't yet know about), Sky errors cleanly: "This sidecar was produced by a future skyc version; please upgrade."

The migration machinery is non-trivial and not free. Pre-1.0, Sky defers the work; v0/v1 skycs simply require format-version match. v1.x adds migrations as needed.

### 7.6 Missing sidecar is a hard error

If Sky's `rustc` loads an rlib with the marker but cannot find an adjacent sidecar, the compile fails immediately:

```
error: Sky sidecar missing for crate `my_utils`
  expected at: target/deps/my_utils-abc123.sky-meta
  crate marker present: yes
  hint: this rlib was built without the corresponding sidecar
  hint: rebuild `my_utils` with the Sky toolchain
```

The error is informative and actionable. Users know exactly what to do.

The hard-error policy is correct because: an rlib with the marker but no sidecar means Sky machinery was supposed to be active during the rlib's compile but wasn't (or the sidecar was deleted). Sky cannot proceed without the sidecar — it has no way to type-check Sky source against the lib's exported items, no way to know the lib's types' layouts. Falling back to "treat the rlib as a normal Rust lib" is wrong because the rlib's `unreachable!()` bodies would propagate to runtime panics.

The error path is rare in practice. Cargo's build graph ensures that when a Sky library is recompiled, its sidecar is rewritten alongside the rlib. The only way to hit the error is to have an out-of-sync target directory (manually deleted sidecar, corrupted cargo state). The error tells the user how to recover.

### 7.7 Cross-references

- Section 8 — what's in the sidecar's payload.
- Section 13 — comptime evaluation that produces sidecar content.
- Section 22 — cargo's incremental machinery that interacts with sidecars.
- Section 18 — cargo orchestration that places sidecars next to rlibs.

---

## 8. Temputs Format

This chapter covers what's in the sidecar's payload. The format is named "Temputs" after Vale's pre-existing typing-pass output, which Sky inherits and extends.

### 8.1 Vale's Temputs as the basis

Vale's typing pass produces Temputs — a typed AST representation that captures every item's structure, type information, body (for typed items), and source position. Sky inherits this representation wholesale. The shape:

- **Types** are represented as nominal structures: a struct's typed AST includes its name, type parameters, field names + types, group parameters, linearity status, layout information, and source position.
- **Functions** are represented similarly: name, type parameters, parameter names + types, return type, group parameters, body (a typed expression tree), source position.
- **Impl blocks** are represented as references between trait DefIds and concrete-impl bodies.
- **Modules** are nested namespaces with items.

The exact bit-level layout of each Temputs element is determined by the typing pass's output. Sky's typing pass is a port of Vale's typing pass, with extensions for Sky-specific concepts (groups, linear types, comptime).

### 8.2 Extensions for cross-crate item references

Vale's Temputs encodes intra-module item references natively. Sky extends this to cross-crate references:

A reference to an item from another crate uses an absolute path:
```
RustRef("std::vec::Vec")        — A reference to Rust's Vec type.
SkyRef("my_utils::Widget")      — A reference to Widget in another Sky library.
SkyRef("self::internal_helper") — A reference to a non-export Sky function in the current library.
```

The path is the canonical form Sky's typechecker uses for cross-crate name resolution. Cross-crate resolution happens at sidecar load time: when the consumer (the binary's compile) loads `my_utils.sky-meta`, the references inside become first-class objects in Sky's universe with concrete DefIds populated.

Vale already had some support for foreign-item references (for C interop); Sky extends this to Rust-language references. The mechanism is:

```rust
enum ItemRef {
    Internal(SkyItemId),                // reference to an item by Sky-side identity
    RustPath(RustAbsolutePath),         // reference to a rustc item by its absolute path
    SkyPath(SkyAbsolutePath),           // reference to a Sky item in another lib
}
```

`SkyItemId` is Sky-internal (a u64 or similar). `RustAbsolutePath` and `SkyAbsolutePath` are dotted-name representations like `"std::vec::Vec"` or `"my_utils::module::Widget"`. Resolution to a concrete DefId happens lazily on first use.

### 8.3 Extensions for Rust call encoding

When Sky source has `vec.push(x)`, the Temputs node representing the call has a `RustCall` variant:

```
RustCall {
    target: RustRef("Vec::<T>::push"),   // absolute path with generic args
    args: [SelfArg, x],                  // typed AST nodes for arguments
    return_type: Unit,                   // typed return
    group_effects: { mutates: G1 },      // if any group is affected
}
```

The `RustCall` AST node tells Sky's codegen "emit an LLVM call to the rustc-mangled symbol for this Instance, passing these args with the appropriate ABI coercions."

Sky's per_instance_mir generator processes `RustCall` nodes by emitting `ReifyFnPointer` casts of the target's substituted DefId into the synthetic MIR body. Rustc's collector then queues the substituted Instance for monomorphization. Sky's codegen, at emit time, emits the actual call site with the correct ABI.

### 8.4 Extensions for Rust trait impl markers

When Sky source has `impl rust.std.clone.Clone for MyType`, the Temputs records:

```
RustTraitImpl {
    rust_trait_path: "std::clone::Clone",
    trait_args: [],                              // no generic args on Clone
    self_type: SkyTypeRef("MyType"),
    method_bodies: [
        (method_name: "clone", body: typed_expr_for_clone_body),
    ],
}
```

This entry is processed at stub-gen time to produce the Rust stub source's `impl ::std::clone::Clone for MyType { ... }` block. The actual method body in the stub is `unreachable!()`; Sky's `per_instance_mir` provides the real body at codegen time, sourced from the `body` field of the Temputs entry.

For HRTBs (higher-ranked-trait-bounds, see Section 11) on Sky's trait impls of Rust traits: the Temputs records the HRTB structure as a binder over a Sky-group parameter. Sky's stub generator, when emitting the impl block, generates the equivalent `for<'a>` Rust syntax. The substitution machinery handles the binder boundary.

### 8.5 Typeid table for `SkyOpaqueType` wrapper

Sky's interop architecture uses a universal `SkyOpaqueType<const T: u64>` wrapper to express Sky-side types that rustc shouldn't know about by name (Section 10.6). Each typeid is a stable, content-addressed identity for a Sky type.

The sidecar contains a typeid table:

```
SkyTypeId {
    typeid: 0xABCD,
    source_identity: SkyPath("my_utils::Internal::Hidden"),
    layout: Layout { size: 16, align: 8 },
    drop_glue_symbol: "__sky_drop_typeid_abcd",
}
```

The table is populated at typing-pass time for source-defined types. For comptime-produced types, entries are added during comptime evaluation; the typeid is the hash of the canonical construction recipe.

Each Sky library's sidecar contains typeid entries for types defined in that library. Cross-crate references work because typeids are content-addressed: lib_a and lib_b compute the same typeid for the same logical type independently, because the typeid is a hash of the type's source identity (not its CrateNum or any per-compile state).

### 8.6 Item bodies: typed AST shipped for all items

The sidecar contains typed AST for every item — exports and non-exports. This is the locked decision from the design conversation: Sky libs ship only AST, downstream codegens everything.

Reasons:

- **Sky-version independence.** A Sky lib produced by skyc 0.5 is consumed by skyc 0.6; the binary that uses both is compiled by skyc 0.6. The binary contains Sky-emitted bodies for every Sky item it reaches, all of them produced by skyc 0.6's codegen. There is no precompiled body from skyc 0.5 in the binary.
- **Simplicity.** No per-library `.o` file to track. No cross-library symbol resolution for non-exports.
- **Cross-platform.** Sky lib publishes once; consumers compile for their own target. No need to ship pre-compiled bodies for every target.

The cost is compile time at the binary's compile: every Sky body the binary reaches must be re-codegenned. This is acceptable per Section 5.5.

### 8.7 Source position info

Every Temputs item carries source position (file, line, column). The file is referenced by index into a per-sidecar file table. The file table maps indices to filenames relative to the cargo package root.

Source positions enable:

- Diagnostics ("error in `my_utils::widget.sky` line 42").
- Debug info (the binary's DWARF references `.sky` source lines).
- IDE tooling (jump-to-definition crosses crate boundaries via the source position).

The size cost is modest: a u32 line, u32 column, u16 file index per AST node. ~12 bytes per node.

### 8.8 No pre-computed layouts (layouts derived at consumer compile time)

The sidecar does *not* contain pre-computed layouts. Layouts are derived at the consumer's compile time from the structural information in the typed AST.

This decision is locked. Reasoning:

- **Sky version independence for layouts.** Different skyc versions might compute layouts differently. If a layout is baked into a sidecar by skyc 0.5, then consumed by skyc 0.6 which has improved layout decisions (better packing, niche optimization), the baked layout would be stale. Re-deriving at consumer time means all layouts in the binary are consistent with the consumer's skyc version.
- **Comptime-driven layouts work naturally.** A Sky type whose layout depends on a comptime evaluation needs the consumer's comptime state to derive. Pre-baking would require enumeration of all instantiations; re-derivation handles instantiation-at-use naturally.
- **Layout flexibility for future Sky compiler improvements.** Sky's codegen can change layout decisions over time (better cache behavior, target-specific tuning); each Sky version's layout decisions apply uniformly to all libs in the binary.

The cost: layout_of fires many times during a compile (once per derived type rustc encounters: `*mut T`, `&T`, `Option<T>`, etc.). Each fires triggers Sky's layout machinery: walk the type's structural Temputs, recursively compute child layouts, compose into the type's layout. Memoize within the rustc invocation (a `HashMap<(typeid, args), Layout>` keyed by content-addressed identity).

Section 10 covers the layout mechanism in detail.

### 8.9 Inspection tool: `skyc inspect`

A `skyc inspect <sidecar-path>` command dumps the sidecar in a human-readable form. Shipped from v0. Used for:

- Debugging "what's in this sidecar?" questions.
- Inspecting published Sky libraries before depending on them.
- Verifying determinism in CI.

The output format is text (probably JSON or YAML). Each section of the sidecar (header, typeid table, item table) is dumped in turn.

### 8.9.5 Discovered trait-impl instances

For Sky-defined types implementing Rust traits (case 4 / case 6 of §2's taxonomy), the cascade that discovers each concrete `<SkyType as RustTrait>::method` monomorphization fires **at the stub rlib compile**, not at user-bin (§2.6's empirical correction; Appendix F's `is_reachable_non_generic` collector gate at `should_codegen_locally`). The user-bin compile can't re-run the cascade for non-generic upstream symbols. The current architecture handles this with an **in-process capture-drain at the same compile session where the cascade fires** — no sidecar shipment, no cross-process state, no augmented-map query override.

**Mechanism.** At the stub rlib's `consumer_fill_modules` callback (NOT `after_rust_analysis` — capturing there would trigger mono walk and re-enter MUTABLE_STATE per @GCMLZ; the `consumer_fill_modules` timing is post-mono-walk and avoids the re-entry), the pure function `collect_consumer_trait_impl_instances(tcx)` walks rustc's partition for `MonoItem::Fn(instance)` entries matching `is_consumer_trait_impl_method` and returns a `Vec<DiscoveredTraitImplInstance>`. The same callback drains the returned Vec inline: for each tuple it looks up the impl across the loaded registries, substitutes the impl-method body with the captured args, and pushes the resulting Instance into the Sky codegen queue for `fill_module` to emit at this same compile session. The Vec lives microseconds — produced and consumed within one function call.

Each entry records:

| Field | Type | What |
|---|---|---|
| `self_type_name` | string | The Sky struct the impl is for (e.g. "Wrapper"). |
| `trait_name` | string | The Rust trait's short name (e.g. "Clone"). |
| `method_name` | string | The method's source-level name. |
| `concrete_args` | `Vec<ResolvedType>` | Concrete instantiation args (impl-block params followed by method-level params, in source order). Empty for non-generic impls; one entry per type param for generic impls. |

Rust shape:

```rust
#[derive(Clone, Debug)]
pub struct DiscoveredTraitImplInstance {
    pub self_type_name: String,
    pub trait_name: String,
    pub method_name: String,
    pub concrete_args: Vec<ResolvedType>,
}

// In the toylang reference implementation this is the return type of
// `collect_consumer_trait_impl_instances(tcx) -> Vec<DiscoveredTraitImplInstance>`.
// It is NOT a field of any registry/sidecar struct — the data is purely
// in-process, single-compile-session.
```

Determinism (§7.4) still requires sorting before drain — a stable key like `(self_type_name, mangled(concrete_args), trait_name, method_name)` — so that the emitted-body order is reproducible.

**N=0 / N≥1 uniformity.** The drain loop handles non-generic impls and generic impls through the same code path; non-generic is the degenerate case with empty `concrete_args` (per @NNGZ, §26.15).

**Cross-Sky-crate (case 6) caveat.** The impl body may live in a different upstream from where the discovery was captured (the bin's stub rlib captures the cascade that crosses crates). The drain loop searches all loaded registries (local ∪ upstream registries loaded via `on_sky_lib_loaded`) for a matching impl rather than assuming locality.

**Symbol canonicalization across crates.** The stub rlib's `duplicate<Wrapper<i32>>` body references `<Wrapper<i32> as Clone>::clone`; for the symbol name to match wherever both sides reference it, the v0 mangler at every reference site must pick the same instantiating-crate disambig. Under the current architecture this works **without** an `upstream_monomorphizations` augmentation:

1. The owning crate (where the cascade fires) emits the trait-impl body at its own compile session via the drain above. The body is stamped `External` linkage by Sky's `fill_extra_modules` contribution; the rustc-emitted stub source for it is **filtered out of the CGU list** by Sky's `collect_and_partition_mono_items` override before LLVM codegen, so no competing `.o` symbol is produced.
2. Sky's `LangDriver::config` heuristic forces `share_generics=true` at stub rlib compiles, which makes rustc record the trait-impl Instance in the rlib's natural `upstream_monomorphizations_for` map via the standard rmeta encoding.
3. At downstream (user-bin) compiles, the standard share-generics gate in `Instance::upstream_monomorphization` consults the cstore-walk-populated map and resolves to the owning crate's disambig. (Pre-2026-06-22 architecture used rustc-fork patch 5 to force this resolution at -O>=2 with share_generics=false; with the partition filter restored, the AvailableExternally body no longer exists to misplace, and the standard gate's outputs suffice for both opt levels.)

The previous (now-retired) mechanism shipped the discoveries via the sidecar, pushed them into a `SkyUniverse.discoveries` field at `on_sky_lib_loaded`, and used a `synthesize_upstream_monomorphizations` callback + `upstream_monomorphizations_for` query override to inject synthesized entries into rustc's map. All of that retired 2026-06-21 (commits b09a90b for the §5.5 Step 3 retirement, ff0cfe8 for the dead-code cleanup) once Option 4 (`AvailableExternally` linkage) + §5.5 Step 2 (owning-crate emission) made the rustc-natural map sufficient. See §F.13 / §F.14 / §F.14.1 for the deep-investigation findings that drove the retirement.

### 8.10 Cross-references

- Section 6 — what gets generated into the stub rlib (exports only).
- Section 9 — the export keyword's effect on sidecar content (full universe, regardless of export status).
- Section 10 — typeid mechanism's role in cross-crate type identity.
- Section 13 — comptime that may add typeids and other entries to the sidecar.
- Section 2.6 + Appendix F (§F.13, §F.14, §F.14.1) — the cascade-fires-at-stub-rlib-compile empirical correction that motivates §8.9.5's in-process capture-drain.

---

## 9. Export and Visibility

The `export` keyword in Sky source determines what rustc knows about. This is a critical architectural property of Sky: most of Sky's surface area stays invisible to rustc; only export items cross into the rustc-visible boundary.

### 9.1 The `export` keyword

Sky source uses the `export` keyword to mark items that should be visible to Rust callers:

```sky
export struct Widget { id: I32 }
export fn make_widget(id: I32) -> Widget { Widget { id: id } }

struct Internal { count: I32 }     // non-export
fn helper(x: I32) -> I32 { ... }   // non-export
```

The export keyword applies to: structs, enums, traits, type aliases, functions, constants, modules. The semantics:

- **Export struct** generates a stub declaration in the stub rlib. Rustc has a DefId for it. Rust callers can name it. Sky's layout_of override fires for it. Drop glue flows through rustc's standard DropGlue path (post-Phase-E retirement of mir_shims override — see §15.7).
- **Export fn** generates a function declaration in the stub rlib. Rust callers can call it. Sky's per_instance_mir override fires for it. Sky's codegen emits its body.
- **Export trait** generates a trait declaration in the stub rlib. Rust types can impl the trait (Sky inherits Rust's orphan rule; impls must be in the trait's owning crate or a type-owning crate, Section 6.6).
- **Export impl** (an impl block marked `export` or with both trait and type being export) generates an impl declaration in the stub rlib.
- **Non-export items** are absent from the stub rlib. Rustc has no DefId for them. They live entirely in Sky's universe (the sidecar) and in Sky's codegen output (the binary's final `.o`).

### 9.2 Per-item granularity

Export is a per-item attribute. There is no `export mod foo` that bulk-exports everything in the module. The user marks each item individually. This is intentional:

- **Clarity.** A reader looking at a Sky source file sees explicitly which items are exposed to Rust callers and which are internal. The export status is local source information, not a cascade from a parent module.
- **No accidental exposure.** Bulk export risks accidentally exposing items the author didn't intend. Per-item export forces the author to think about each item.
- **Compatibility with Sky's typechecker.** Sky's coherence and orphan-rule machinery operates per-item. Per-item export aligns with the underlying mechanism.

If users want bulk-export ergonomics, future Sky versions might add a `pub use my_module::*` style mechanism that desugars to per-item exports. Not a v1 feature.

### 9.3 What rustc sees of exports vs non-exports

For an exported Sky item:
- Rustc has a DefId in the stub rlib's crate.
- Rustc can name the item via its absolute path.
- Sky's per_instance_mir provider answers when rustc queries the item.
- Sky's layout_of override answers when rustc queries the item's layout.
- Drop glue resolution flows through rustc's standard path; per-type Drop bodies (for Sky-defined `impl Drop`) come from `fill_extra_modules` via the cascade-discovery + AST-rewrite path described in §15.7.
- Rustc's default v0 mangler determines the symbol name (single-symbol architecture, §6.2). Sky's emission writes the body under exactly that name, so call sites and the definition share one symbol without any rename.

For a non-exported Sky item:
- Rustc has no DefId. The item doesn't exist from rustc's view.
- Sky's typing pass produces an entry in the sidecar for the item.
- Sky's codegen emits the item's body into the binary's `.o`.
- Sky-internal callers of the item (other Sky items in the same library) reference it via Sky-internal symbols.
- Rust code cannot reference the item by name.

This is the **architecturally important property**: Sky's surface to rustc is proportional to Sky's chosen export surface, not to Sky's total type universe. For a Sky library with 100 non-export items and 5 exports, rustc sees 5 items; Sky internally manages 105.

### 9.4 Non-export items: invisible to rustc at every level (no DefIds, no symbols)

Non-export items are *not* compiled by rustc. They have no rlib entries, no DefIds, no rustc-known symbols.

This is in contrast to a hypothetical alternative where non-exports were "pub(crate)" — accessible from within the stub rlib's compile but not externally. Sky's design rejects this. Non-exports are entirely Sky-side; rustc doesn't even know they exist.

Mechanism:

1. Sky's stub_gen, when emitting the stub rlib's Rust source, walks the Sky library's items and emits Rust declarations only for exports. Non-exports are skipped.
2. Sky's typing pass produces Temputs for the full library — exports and non-exports both.
3. Sky's `per_instance_mir` provider fires only for export items (because only exports have DefIds rustc knows about).
4. Sky's codegen, at the binary's compile time, walks Sky's universe (loaded from sidecars + the binary's own Temputs) and codegens *every* Sky item reachable from the binary's entry points — exports and non-exports both. The walk happens in Sky's codegen, not via rustc's mono collector.
5. The emitted `.o` contains Sky-internal symbols for both export and non-export items. Export items also get the Rust-mangled extern symbol; non-export items get only the Sky-internal symbol.

The Rust-mangled extern symbol and the Sky-internal symbol may be the same (Sky may choose to name exports with their Sky-internal name) or different (Sky may use rustc's v0 mangling for exports to enable Rust callers to find them, while using a Sky-specific scheme internally). Section 6 covers the stub generation; Section 5 covers the codegen choices.

### 9.5 Transitive Rust deps surface through nearest exported ancestor

When a non-export item transitively calls Rust items, those calls must surface to rustc somehow — rustc must monomorphize the Rust items, even though the call graph passes through Sky-internal territory rustc can't see.

The mechanism: the synthetic MIR body Sky's `per_instance_mir` provides for an exported item enumerates *all* transitive Rust dependencies — including ones reached through non-export Sky callees.

Worked example:

```sky
fn deep_helper<T>(x: T) -> Vec<T> {
    let mut v = Vec::new<T, Global>()
    v.push(x)
    v
}

export fn make_container<T>(x: T) -> Vec<T> {
    deep_helper<T>(x)
}
```

When rustc walks `make_container<i32>`, the per_instance_mir provider needs to enumerate Rust deps. Sky's frontend walks the call graph: `make_container<i32>` → `deep_helper<i32>` (non-export, internal) → `Vec::new<i32, Global>` (Rust), `Vec::push<i32>` (Rust). The provider returns a synthetic body containing ReifyFnPointer casts for both Rust deps.

Rustc's collector walks the body, sees the casts, queues the Rust items. Rustc monomorphizes `Vec::new<i32, Global>` and `Vec::push<i32>` normally. Sky's codegen, separately, emits `make_container<i32>` and `deep_helper<i32>` into the binary's `.o`.

The non-export item `deep_helper` never gets a DefId. Rustc never sees its name. But its transitive Rust deps surface through `make_container`'s per_instance_mir body. The dep graph closure is preserved.

**Memoization.** Sky's per_instance_mir provider caches the walk per `(exported_def_id, concrete_args)` so subsequent queries for the same Instance return cached results. Within a single rustc invocation, the cache is fully effective. Across invocations, the work is redone (this is per the no-pre-computed-bodies decision in Section 5.5; deep walks are part of "codegen everything at the binary's compile").

### 9.6 No cross-crate Sky-internal symbol resolution problem

A common worry when designing systems with non-export internal items: how do cross-crate calls to non-exports resolve at link time? If `my_app` (the binary) calls into `my_utils`'s non-export `helper`, and Sky-internal symbol mangling uses different names in different crates, the linker can't find the right symbol.

**For Sky, this problem doesn't exist.** All Sky-emitted bodies — exports and non-exports — use the rustc-mangled name rustc would have given the stub fn (per §6.2's single-symbol architecture). Every reference to such a body, regardless of which crate the call site lives in, resolves to that same mangled name. The linker sees a normal cross-crate symbol resolution: Sky's emitted body in one `.o` file, references to it in other `.o` files, all using the rustc-mangled name. No bespoke Sky-internal mangling scheme; no per-call-site coordination.

**Under post-§5.5-Step-2 (commit 41f7ae4),** the locations of Sky-emitted bodies are:
- Non-generic Sky items (exports, cascade-discovered trait-impl methods): emitted at the **owning crate's** compile via `fill_extra_modules`. The stub rlib's `.o` carries them with `External` linkage; rustc-emitted stub source bodies at the same compile session are removed from the CGU list by the partition filter, so no competing `.o` symbol is produced.
- Generic Sky items: emitted at the **first compile session** where a concrete instantiation arises (typically the binary's compile, but also another Sky library's compile if it's the first to instantiate). Same single-symbol naming.
- Non-export Sky-internal items reached transitively: emitted at the same session as the export that reached them, via `walk_and_stash_internal_callees` Sky-side discovery.

The mangling scheme is rustc's v0 mangler applied to the stub fn's DefId — well-defined, deterministic, content-addressed by Sky source. Future Sky versions can change Sky's internal compilation strategy without affecting the cross-crate symbol contract, because the contract is rustc's mangler output, not Sky's choice.

(Earlier framings of this section claimed all Sky bodies emit at the binary's compile, leaving no per-library Sky `.o` for the linker to see. That was true pre-Step-2; post-Step-2, library compiles DO emit Sky `.o` content for items they own. The single-symbol architecture means cross-crate linking still "just works" — the linker resolves references to whichever crate emitted the body.)

### 9.7 Cross-references

- Section 6 — what gets generated into the stub rlib (exports only).
- Section 8 — what gets serialized into the sidecar (full universe).
- Section 5 — codegen emits everything.
- Section 19 — per_instance_mir's dep-enumeration walk.

---

## 10. Type Representation Across the Boundary

This chapter covers how Sky-defined types are represented in Rust-visible territory. The core mechanism is opacity: Sky owns type layouts, rustc sees opaque sized blobs. Sky's layout_of override reports size and alignment; rustc never inspects fields.

### 10.1 Sky types as opaque stubs in the rlib

For each exported Sky struct, the stub rlib contains a Rust source declaration:

```rust
pub struct MySkyType(());                     // for a non-generic struct
pub struct MyGenericSkyType<T>(::std::marker::PhantomData<T>);  // generic
pub struct MyGroupParametricSkyType<'a>(::std::marker::PhantomData<&'a ()>);  // group-parametric
```

The `(())` (unit tuple struct) and `PhantomData<T>` shapes are deliberately empty — from rustc's source-level view, the type has zero data. Sky's `layout_of` override at query time tells rustc "actually, this type has size N and alignment M." Rustc trusts the override.

The PhantomData entries serve to satisfy rustc's "all generics must be used" rule — a generic parameter T must appear in the struct's definition somewhere. PhantomData<T> uses T without contributing to the type's runtime representation.

### 10.2 PhantomData<T> wrapping for generic Sky types

The PhantomData wrapper has two purposes:

1. **Satisfies rustc's "all generics must be used" rule.** A `struct Foo<T>(())` would fail rustc's type-checking; the T must appear somewhere. PhantomData<T> uses T as a phantom (compile-time-only) marker without affecting layout.
2. **Communicates variance to rustc.** PhantomData<T> says "this struct is covariant in T." If Sky's actual variance differs (Sky has a more nuanced variance model than Rust), Sky uses one of PhantomData's variance-modulating forms — PhantomData<*const T>, PhantomData<*mut T>, PhantomData<fn(T) -> T>, etc. The variance choice affects how Rust callers can pass and store Sky values; Sky's typechecker validates the variance choice is correct.

The PhantomData wrapper is a layout-time concept (rustc believes the struct is zero-size and contains a phantom T marker); Sky's layout_of override supplies the actual size. The two layers cohabitate without interference.

### 10.3 Layout authority: Sky decides via layout_of override

For every Sky type that has a DefId (i.e., every exported Sky type), Sky's `layout_of` query override fires when rustc needs the layout. The override returns a `LayoutData` constructed by Sky's layout machinery:

```rust
fn lang_layout_of<'tcx>(
    tcx: TyCtxt<'tcx>,
    input: PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>> {
    let ty = input.value;

    // Filter: only intercept TyKind::Adt and only for specific Sky types.
    if let ty::TyKind::Adt(adt_def, args) = ty.kind() {
        let def_id = adt_def.did();
        if is_from_sky_stubs(tcx, def_id) && !args.has_param() {
            // Sky-side computation.
            let sky_layout = sky_compute_layout(tcx, def_id, args);
            let layout_data = LayoutData {
                size: sky_layout.size,
                align: AbiAlign { abi: sky_layout.align },
                backend_repr: BackendRepr::Memory { sized: true },
                fields: FieldsShape::Arbitrary {
                    offsets: IndexVec::new(),
                    memory_index: IndexVec::new(),
                },
                variants: Variants::Single { index: rustc_abi::FIRST_VARIANT },
                largest_niche: None,
                uninhabited: false,
                max_repr_align: None,
                unadjusted_abi_align: sky_layout.align,
                randomization_seed: rustc_hashes::Hash64::ZERO,
            };
            let layout = tcx.arena.alloc(layout_data);
            return Ok(TyAndLayout { ty, layout });
        }
    }

    // Fall through to rustc's default for non-Sky types.
    DEFAULT_LAYOUT_OF.get().expect("default layout_of not installed")(tcx, input)
}
```

(Sketch; exact API will drift across nightlies.)

### 10.4 Opaque-with-size shape (zero visible fields, Sky-computed size/align)

The LayoutData returned by Sky's override has the following critical properties:

- **`fields: FieldsShape::Arbitrary { offsets: [], memory_index: [] }`** — zero visible fields. Rustc cannot introspect the struct; it has no field offsets to project, no fields to reorder, no scalar pair to decompose.
- **`backend_repr: BackendRepr::Memory { sized: true }`** — the type is an opaque memory blob, allocated in memory rather than passed in registers. Rustc's ABI machinery handles it via memory operations (memcpy, sret returns, indirect parameter passing).
- **`size` and `align`** — Sky-computed, reported to rustc.
- **`uninhabited: false`** — Sky types are inhabited by default (they have at least one value). Sky doesn't surface uninhabited types to rustc via this override; if a future feature requires it, the design will be extended.

The combination of these properties is what "opaque sized blob" means in rustc's terms. Rustc allocates the type's space, can pass references to it, can sret-return it. Rustc cannot inspect it. Sky's codegen knows the type's internal structure; rustc doesn't.

### 10.4.5 Debuginfo walker's source-vs-layout-field-count assumption

The opaque-with-size shape collides with an implicit assumption inside rustc's debuginfo emitter. `rustc_codegen_llvm::debuginfo::metadata::build_struct_type_di_node` and `build_union_type_di_node` iterate an ADT's source-level `FieldDef`s and query `layout.field(cx, i)` / `layout.fields.offset(i)` per source field:

```rust
variant_def.fields.iter().enumerate().map(|(i, f)| {
    let field_layout = struct_type_and_layout.field(cx, i);
    build_field_di_node(..., struct_type_and_layout.fields.offset(i), ...)
})
```

Under rustc's normal model, `variant_def.fields.len() == struct_type_and_layout.fields.count()` always holds. Under Sky's `layout_of` override returning `FieldsShape::Arbitrary { offsets: [], memory_index: [] }` (zero layout fields, per §10.4) for a source struct that has at least one `FieldDef` (e.g., the `PhantomData` wrapper needed to satisfy "all generics must be used"), the assumption breaks — the walker calls `offset(0)` on an empty offsets vec and panics in `rustc_abi/src/lib.rs::FieldsShape::offset` with `index out of bounds: the len is 0 but the index is 0`. ICEs the debuginfo walker.

The crash only surfaces when the Sky ADT appears inside a Rust generic (e.g., `Vec<MySkyType>`), because the outer Rust type's `build_generic_type_param_di_nodes` recurses into each type-param's DI node. Sky ADTs that don't cross into Rust-generic-debuginfo contexts dodge it accidentally. Toylang surfaced the bug empirically via `r_t_r_vec_of_ship` (a `Vec<ToyShip>` test); the original toylang workaround was to emit `pub struct Foo;` (a unit struct with zero `FieldDef`s) for the non-generic case while keeping `pub struct Foo<T>(PhantomData<T>);` for generics — a forced asymmetry that violated the compiler law "non-generic is the degenerate case of generic."

Two mitigation paths, both architecturally compatible with §10.4's opaque-with-size shape:

1. **Defensive clamp upstream.** Patch both `build_struct_type_di_node` and `build_union_type_di_node` to clamp the source iter to `min(variant_def.fields.len(), layout.fields.count())`:

    ```rust
    let visible_field_count = std::cmp::min(
        variant_def.fields.len(),
        struct_type_and_layout.fields.count(),
    );
    variant_def.fields.iter().take(visible_field_count).enumerate()...
    ```

   ~5–10 LOC, no-op on unoverridden layouts, defensive for any plugin overriding `layout_of` (cranelift, miri, Sky). Sibling sites worth auditing: the enum variant walker (`build_enum_variant_struct_type_di_node` in `metadata/enums/mod.rs`) iterates `0..variant_layout.fields.count()` first then indexes source — the inverse direction, so the underreport case doesn't reach a panic there. Toylang briefly shipped this clamp as a fourth fork patch (commit `e67de69ef35`) during investigation and verified it eliminates the ICE on rustc 1.95.0-dev; the patch was retired in favor of Path 2 (wrapper-as-field, §10.6) once the structural fix was validated end-to-end. Path 1 remains the recommended *upstream-PR* shape if the wrapper machinery is undesirable for other consumers — a forkable patch Sky could carry indefinitely as a fallback if Path 2 ever proved insufficient.

2. **`SkyOpaqueType<typeid>` wrapper-as-field for every Sky struct.** §10.6 already defines this wrapper for non-export and comptime types; extending it to every Sky struct means each struct's source representation becomes a newtype around the wrapper — `pub struct Foo<P...>(__SkyOpaqueType<HASH>, PhantomData<(P...)>);` for the generic case, or `pub struct Foo(__SkyOpaqueType<HASH>);` for the non-generic case. The wrapper carries a content-addressed u64 typeid as its sole const-generic argument and has zero source fields itself. `layout_of` reports a matching field count (1 non-generic / 2 generic), so the walker's `offset(i)` queries succeed at every level: 1 query into Foo (returns 0, the wrapper at offset 0), then 0 queries into the wrapper (it has 0 source fields and we report 0 layout fields). `BackendRepr::Memory { sized: true }` keeps rustc from decomposing the struct into scalars; the wrapper field is itself opaque-with-size so even with per-field exposure rustc can't unpack it. Sky structs keep their own DefIds, so all existing `impl Trait for Foo` blocks work unchanged. Costs (toylang empirical): ~3 days across five staged commits — typeid helper + wrapper emission + typeid table (Phase 1), const-generic-u64 encode/decode helpers (Phase 2), Sky struct stub shape migration + layout field-count update (Phase 3), wrapper-layout-intercept (Phase 4, often unnecessary when toylang doesn't surface the wrapper as a top-level Rust generic arg), fork-patch removal and docs (Phase 5). Toylang shipped this as commits `72a929e`/`41423cf`/`90599cf` etc. and verified the suite passes 262/262 with fork patch 4 retired. The wrapper machinery needs to be built anyway for §13's comptime types and §10.7 Cases 2/3, so the migration amortizes. **Recommended.**

Either path is locked-design-compatible. Pre-1.0 Sky may ship either; path 2 is preferred because it amortizes against the wrapper machinery §13 will need anyway and retires the fork patch entirely.

### 10.5 Layouts computed at per_instance_mir / layout_of time, memoized per invocation

The layout computation happens lazily at query time. Sky's layout machinery:

```rust
fn sky_compute_layout<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    args: GenericArgsRef<'tcx>,
) -> SkyLayout {
    // Cache lookup.
    let cache_key = (sky_typeid_for(tcx, def_id), canonicalize_args(args));
    if let Some(cached) = LAYOUT_CACHE.lock().unwrap().get(&cache_key) {
        return *cached;
    }

    // Cache miss: compute.
    let item = sky_universe::lookup(tcx, def_id);
    let layout = sky_universe::compute_layout(&item, args);

    LAYOUT_CACHE.lock().unwrap().insert(cache_key, layout);
    layout
}
```

The cache is invalidated at end-of-rustc-invocation (it's per-invocation state, like the slab). Across invocations, the cache is re-populated. The lookup is content-addressed: same Sky type + same concrete args → same cache key → same layout.

For pre-computable layouts (Sky types whose layout doesn't depend on comptime evaluation), Sky's typing pass populates the cache during the after_analysis hook (Section 20). For comptime-dependent layouts, evaluation happens at query time. The cache makes both equally fast on subsequent queries.

### 10.6 `SkyOpaqueType<const T: u64>` universal wrapper

Sky's stdlib pre-declares a universal wrapper type:

```rust
pub struct SkyOpaqueType<const T: u64>(::std::marker::PhantomData<()>);
```

This type appears in rustc-visible territory whenever Sky needs to surface a type that rustc shouldn't know about by name — non-export types appearing in Rust generic arguments, comptime-produced types, anything Sky-side that needs a rustc-visible-but-opaque identity.

The `T` const parameter is a content-addressed typeid that Sky's universe maps to the actual Sky type (or comptime recipe).

**Wrapper-as-field shape (the shipped design, validated by toylang's Phase E Path 2).** Per §10.4.5's analysis the wrapper is used as a *field* of each Sky struct's stub, not as a substitute for the struct identity:

```rust
// Non-generic Sky struct:
pub struct Widget(SkyOpaqueType<HASH_FOR_WIDGET>);

// Generic Sky struct:
pub struct Wrapper<T>(SkyOpaqueType<HASH_FOR_WRAPPER>, ::std::marker::PhantomData<T>);
```

This shape satisfies all three of rustc's debuginfo walker's invariants: (a) the source has exactly the field count rustc expects (1 for non-generic, 2 for generic — the wrapper + the PhantomData carrier), (b) the layout `layout_of` reports has matching field counts at matching offsets (`SkyOpaqueType` field at offset 0 occupying the whole size; PhantomData ZST at offset = `total_size`), (c) the wrapper itself is a unit struct with zero source fields and a default ZST layout, so the walker's recursive `layout.fields.offset(i)` queries succeed at every depth.

Each Sky struct keeps its own rustc DefId (it isn't collapsed to `SkyOpaqueType<HASH>`). The DefId is what trait impl blocks attach to, what `tcx.item_name` returns for diagnostics, and what cross-crate identity hangs on. The wrapper is the field-level opacity carrier that satisfies layout while leaving the struct's identity intact.

Toylang shipped this as `__ToylangOpaque<const T: u64>` in Phase E Path 2 (commits `72a929e`/`41423cf`/`90599cf`). The earlier "opaque-with-zero-fields" shape from §10.4 hit a rustc debuginfo walker ICE under `Vec<SkyT>` (the walker iterates source FieldDefs but the layout reports zero fields → out-of-bounds in `FieldsShape::offset`). The wrapper-as-field shape resolves it structurally; **no fork patch needed**. The fork patch 4 (debuginfo-clamp) that briefly landed during the investigation was retired once wrapper-as-field was in place.

### 10.7 When the wrapper applies (non-exports in Rust generics, comptime-produced types)

Three cases:

**Case 1: Export Sky type inside a Rust generic.** Direct stub representation.

```sky
let v = Vec::<Widget>::new();  // Widget is exported
```

Rustc sees `Vec<Widget>`. Widget has its own DefId. No wrapper needed.

**Case 2: Non-export Sky type inside a Rust generic.** Wrapper applies.

```sky
struct MySkyInternalType { ... }  // not exported

let v = Vec::<MySkyInternalType>::new();
```

Sky's frontend, when generating the Vec instantiation's args, rewrites `MySkyInternalType` to `SkyOpaqueType<typeid_for_MySkyInternalType>`. Rustc sees `Vec<SkyOpaqueType<42>>`. The 42 is the content-addressed typeid; Sky's universe knows that 42 maps to MySkyInternalType.

**Case 3: Comptime-produced type inside a Rust generic.** Wrapper applies.

```sky
const N: I32 = 42
let v = Vec::<comptime_type<N>>::new()
```

Sky's frontend evaluates `comptime_type<42>` at comptime, gets a Sky-internal type representation, assigns it a typeid based on the construction recipe, rewrites to `SkyOpaqueType<typeid>`. Rustc sees `Vec<SkyOpaqueType<typeid>>`.

In all three cases, rustc treats `SkyOpaqueType<N>` as a normal generic struct instantiation. `layout_of(SkyOpaqueType<N>)` fires; Sky's override looks up N in the universe, returns the layout. Drop semantics flow through the AST-rewrite mechanism (§15.7): if the universe's entry for typeid N marks the type as needing drop, the compiler synthesizes `Drop::drop(&local)` at scope-end positions; cascade discovery + `fill_extra_modules` emits the body via the same path as Sky-defined `<Widget as Drop>::drop`.

### 10.8 Content-addressed typeids for cross-crate stability

Typeids are content-addressed: the typeid for a Sky type is a stable hash of the type's identity, computable independently in any rustc invocation that has access to the type's source.

For source-defined Sky types: typeid = hash(qualified_path). E.g., typeid for `my_utils::MySkyInternalType` is `hash("my_utils::MySkyInternalType")`. Stable across crates and invocations.

For comptime-produced types: typeid = hash(canonical_construction_recipe). The "canonical construction recipe" is a deterministic serialization of the comptime call graph that produced the type — for `comptime_type<42>`, the recipe is something like `(comptime_type_def_id, [42])`. Deterministic in: typing pass output, comptime arg values (the integer 42 in this example).

Stability properties:

- **Same Sky lib + same Sky version → same typeids.** Reproducible builds.
- **Same Sky lib + different Sky versions → same typeids, if the hashing algorithm hasn't changed.** Cross-version sidecar compatibility (up to format_version compatibility).
- **Different Sky libs that define structurally similar but separately-source-located types → different typeids.** No collisions across libs.
- **Comptime-produced types with same recipe → same typeid.** Two different call sites with the same comptime arg produce the same typeid.

The hashing algorithm: BLAKE3 with truncated output to fit in u64. The pre-image is the canonicalized source path (or canonicalized recipe). Sky's hashing is documented in the format spec and constitutes a part of the format_version's stable semantics.

### 10.9 Type identity in Sky's universe vs in rustc

Sky-side identity and rustc-side identity for the same logical type are different things mapped via the typeid table:

- **Sky-side identity:** a SkyTypeId (or qualified path) in Sky's universe. Sky's typechecker, layout machinery, and codegen all use Sky-side identity.
- **Rustc-side identity:** a DefId. For exports, the DefId is in the stub rlib (and is created by rustc when it parses the stub rlib's Rust source). For non-exports and comptime types, the DefId is the `SkyOpaqueType<const T: u64>` ADT's DefId (one DefId, parameterized by the typeid).

The mapping from rustc-side to Sky-side happens at sidecar load time: Sky's machinery, on seeing a crate with the marker, walks `module_children(crate_root)`, computes each item's qualified path, looks up the Sky item by path, builds a `HashMap<DefId, SkyItemId>` and an inverse `HashMap<SkyItemId, DefId>`. Subsequent queries are O(1).

For the `SkyOpaqueType<const T: u64>` wrapper, the mapping is: given an instantiation `SkyOpaqueType<42>`, the typeid 42 is looked up in the sidecar's typeid table to recover the Sky type. The typeid table is built during sidecar load; entries are added as needed during comptime evaluation.

### 10.10 Cross-references

- Section 6 — stub rlib's role in carrying export type declarations.
- Section 8.5 — typeid table format in the sidecar.
- Section 11 — group params on Sky types appear as PhantomData-tied lifetime slots.
- Section 13 — comptime-produced types' typeid assignment.

---

## 11. Group System and the Boundary

This chapter covers how Sky's group system — Sky's analog to Rust's lifetime system — projects onto Rust's lifetime model at the rustc boundary. The mechanism is erasure: every group erases to `re_erased` at the boundary, with Sky's typechecker enforcing the real lifetime correctness Sky-side.

### 11.1 Groups as Sky's lifetime-equivalent

A Sky group is a named, possibly hierarchical, possibly runtime-realized memory region. Sky source explicitly names groups in references and function signatures:

```sky
fn process<G>(items: &G [Widget]) -> &G Widget {
    &items[0]
}
```

The `&G` annotation says "this reference lives in group G." Sky's typechecker tracks which groups a function operates on, which references belong to which groups, and ensures that no reference outlives its group.

Groups can be hierarchical: `G1 ⊂ G2` declares G1 as a sub-region of G2. References valid for G1 are valid for G2 (a value living in a sub-region also lives in the containing region). This is more expressive than Rust's `'long: 'short` because Sky tracks containment, not just outlives.

Groups are runtime-realized: Sky's runtime allocates group-region memory and frees it as a unit when the group ends. This is the bump-allocator-arena pattern from region-based memory management literature. The exact runtime mechanism varies (bump allocator for short-lived groups, more sophisticated allocators for long-lived groups) but is always region-based.

### 11.2 `&G T` erasure to `&'re_erased T` (per @ELASZ pattern)

When Sky source references `&G T`, Sky's frontend, when generating Rust-shaped code (for stub rlib generation, for `GenericArgs` construction at Rust call sites, etc.), erases the group annotation to `re_erased`:

```sky
fn process<G>(x: &G T) { ... }
```

In stub rlib:
```rust
pub fn process<T>(x: &T) -> () { ::std::unreachable!() }
```

Rustc elides the lifetime annotation into an early-bound lifetime, effectively `pub fn process<'a, T>(x: &'a T) -> ()`. By monomorphization time, the lifetime is post-borrowck and Sky's `GenericArgs` populates the 'a slot with `tcx.lifetimes.re_erased`.

This pattern is inherited verbatim from erw's `@ELASZ`. Every Sky → Rust call site uses `oracle::build_generic_args_for_item` (or Sky's analog), which uses `ty::GenericArgs::for_item(tcx, def_id, |param, _| ...)` to fill each generic slot: user-supplied types fill Type slots, lifetime slots get `re_erased`, const slots get the comptime-determined integer values (Section 13).

`re_erased` is semantically correct because Sky's borrow-checking (Sky's analog of Rust's borrowck) has already completed during Sky's typing pass. The borrows carry no remaining lifetime meaning at codegen time; `re_erased` is rustc's standard "this lifetime has no remaining role" placeholder.

`re_erased` is preferred over `'static` because some Rust trait impls discriminate on lifetime (`impl Deserialize<'static>` is strictly narrower than `impl<'de> Deserialize<'de>`). `re_erased` is rustc's neutral placeholder; `'static` would lie about the lifetime in a way that could affect trait dispatch.

### 11.3 Sky types with group params → PhantomData-tied lifetime slots

A Sky struct with a group parameter generates a stub with a corresponding lifetime parameter:

```sky
export struct Region<G> {
    data: &G [I32]
}
```

In stub rlib:
```rust
pub struct Region<'a>(::std::marker::PhantomData<&'a ()>);
```

The PhantomData<&'a ()> uses 'a in a way that satisfies rustc's "all generics must be used" rule. At call sites in stub-rlib-generated impl bodies or wherever a `Region<G>` reference appears, the lifetime slot is populated as `re_erased` (per @ELASZ).

From rustc's view, `Region<'re_erased>` is a normal generic struct instantiation. From Sky's view, the group G has its real, runtime-realized identity (the region this Region belongs to).

### 11.4 Sky reconciles Rust lifetime constraints with Sky groups; Sky owns wrapper generation

Sky's frontend, when reading a Rust signature with lifetime bounds, reconciles those bounds with Sky's group structure. The reconciliation is automatic for simple cases:

- Each Rust lifetime parameter becomes a Sky group parameter.
- `'a: 'b` (outlives bound) becomes Sky's group hierarchy `B ⊂ A` (A contains B; references in B are valid for A).
- HRTBs (`for<'a> Fn(&'a T) -> bool`) get a Sky-specific treatment (Section 11.8).

For advanced cases (lifetime-discriminating dispatch, nested HRTBs), sidecar annotations (Section 11.8) can express the reconciliation manually. The annotation format includes a `lifetime_binding` field that maps Rust lifetime names to Sky group names with bounds.

### 11.5 Aliasing rules: multi-mut intra-Sky, single &mut at boundary

Sky's source-level aliasing rules are more permissive than Rust's:

- Multiple `&G mut T` references to the same data can exist within Sky source. Sky's typechecker tracks which references are visible from which scopes; at most one is "active" at any given source position.
- A scope with a single visible mutable reference can apply the `noalias`/restrict marking. LLVM's `noalias` attribute is correct.
- A scope with multiple visible mutable references cannot apply noalias. LLVM gets no aliasing hint.

At the Rust boundary, aliasing rules tighten to match Rust:

- A single `&G mut T` can be projected to a Rust `&mut T` at the call site. Rust's noalias semantics are honored.
- Multiple `&G mut T` references in scope at the boundary cannot be projected; Sky's typechecker rejects the call site.

The rule's effect: Sky source can have rich multi-mutable patterns internally (operations on shared mutable state with Sky-side coordination), but at the moment a `&mut T` crosses into Rust, only one such reference can be live. Sky's typechecker enforces this at compile time.

### 11.6 Restrict-pointer marking via single-visible-mut scope analysis

Sky's codegen emits LLVM IR with `noalias` (LLVM's restrict-pointer attribute) on parameters when Sky's typechecker can prove single-visibility. Three patterns:

- **Local variable, no aliasing other muts in scope.** Mark noalias.
- **Function argument promised single-mut by the caller's contract.** Mark noalias on the parameter.
- **Field access through a single-mut reference.** Mark noalias on the resulting load/store.

The noalias marking is an optimization hint; it does not affect correctness. LLVM uses it to make alias analysis tighter, enabling optimizations like load-store reordering and register promotion.

The mechanism is entirely Sky-side: Sky's typing pass annotates each reference with a "single-visible-mut" boolean; Sky's codegen consults the annotation when emitting LLVM IR. Rustc has no role in this — at the boundary, the single-visible-mut property has either been proven (Sky-side) and the projection is OK, or hasn't been proven and the projection is rejected.

### 11.7 Outlives bounds expressed via Sky-native group constraints

When a Rust API has lifetime outlives bounds (`'a: 'b`), Sky's frontend translates to Sky's group hierarchy. For example, a Rust API:

```rust
fn copy_from<'src, 'dst: 'src, T>(src: &'src T, dst: &'dst mut T) { ... }
```

Becomes a Sky binding:

```sky
fn copy_from<S, D, T>(src: &S T, dst: &D mut T) where D ⊂ S
```

Wait, that's not quite right. The Rust bound `'dst: 'src` means dst outlives src, so 'src ⊂ 'dst (src is contained in dst's region). Translating:

```sky
fn copy_from<S, D, T>(src: &S T, dst: &D mut T) where S ⊂ D
```

Sky's frontend handles the translation automatically based on the Rust signature's bounds. Sky source users see Sky-style group constraints; the underlying Rust ABI gets the corresponding lifetime bounds via the erasure-and-substitution machinery.

### 11.8 HRTBs: auto-generated where possible, sidecar annotations for advanced cases

Higher-ranked trait bounds (HRTBs, `for<'a>` quantification) appear in three Rust-interop contexts:

1. **Closures Sky passes to Rust APIs.** Iterator combinators, callback patterns, predicate functions. Sky's closure-to-trait-impl machinery generates HRTB-shaped `Fn` impls automatically. Section 14.

2. **Sky impls of Rust traits with lifetime params.** Trait methods that take lifetime parameters. Sky's typechecker reads the trait signature, generates the corresponding impl block with the lifetime parameter included.

3. **Sky APIs taking Rust callbacks with HRTB bounds.** Most common case is `for<'a> Fn(&'a T) -> bool`. Sky's frontend auto-translates: a Sky function taking a callback with group-typed reference becomes a Rust signature with HRTB-quantified lifetime.

For these three cases, auto-generation handles the HRTB correctly. The mechanism is mechanical: identify the HRTB in the Rust signature, generate the Sky-side analog (a Sky group parameter that takes the HRTB role), and the boundary code handles the binder management.

For advanced cases (HRTBs that Sky's frontend cannot auto-handle): sidecar annotations express the binding manually. The annotation format:

```toml
# In a Rust crate's sidecar annotations file
[binding."serde::de::Deserialize"]
hrtb_lifetime = "de"
sky_group_role = "input_group"
custom_bounds = []

[binding."tokio::time::timeout"]
drops_args = true
```

Sky's typechecker uses these annotations during typecheck to handle Rust APIs with complex HRTB structure. For v1, the annotation system handles the cases where automatic translation fails; future versions may extend automatic translation to cover more cases.

### 11.9 HRTBs deferred for v2: lifetime-discriminating dispatch, nested HRTBs

Two HRTB-related cases are deferred to v2:

**Lifetime-discriminating trait dispatch.** Some Rust APIs have specialized impls based on lifetime (`impl Foo for Bar<'static>` vs `impl<'a> Foo for Bar<'a>` with different behavior). Sky's group erasure to `re_erased` makes this dispatch ambiguous from Sky's view. v1 forbids Sky source from invoking such APIs through paths that hit lifetime-discriminating dispatch. v2 considers whether to add a Sky source mechanism that lets users commit to a specific lifetime path.

**Nested HRTBs.** `for<'a> Trait<for<'b: 'a> InnerTrait<'a, 'b>>`. These appear in advanced trait systems (some DSLs use them). Sky's auto-translation doesn't handle them; sidecar annotations can express them but the annotation format is more complex. v2 considers whether to add Sky source syntax for nested HRTBs.

For v1, users with HRTB-heavy interop needs use sidecar annotations or work around the limitation with thin Rust wrapper crates.

### 11.10 Cross-references

- Section 12 — Send/Sync/'static decisions interact with group erasure.
- Section 14 — closures with HRTBs in their Fn impls.
- Section 13 — comptime can interact with group analysis when comptime values are group-related.
- Section 24 — sidecar annotations format.

---

## 12. Send, Sync, 'static, Unpin

This chapter covers how Sky handles Rust's marker traits at the boundary. The mechanism mixes "honest" claims (where Sky's types genuinely satisfy the Rust property) and "honest lies" (where Sky asserts the property via `unsafe impl` and Sky's typechecker enforces the real correctness Sky-side).

### 12.1 Global `unsafe impl Send for SkyT` (Sky lies, enforces source-level)

Every Sky type that has a stub rlib representation gets an `unsafe impl Send` at the stub rlib level:

```rust
pub struct Widget(());
unsafe impl Send for Widget {}
```

This makes every Sky type `Send` from rustc's view, regardless of whether it's actually safe to share across threads. Sky lies to rustc.

Sky's typechecker enforces actual send-correctness Sky-side. Sky source has a notion of "sendable" types; types that contain non-sendable parts (a thread-local handle, a non-thread-safe lock, etc.) cannot be sent. The typechecker rejects Sky source that violates the sendability rules.

**Why the lie:** rustc's `Send` is a marker trait that Rust generic constraints check. Many Rust APIs require `T: Send` for their generic parameters. Without `unsafe impl Send`, Sky types couldn't be used as those type parameters. The Sky source-level enforcement is the real correctness boundary; rustc's `Send` is a phantom from Sky's perspective.

The `unsafe` is a real `unsafe`: the impl makes an assertion that rustc cannot verify. The assertion is justified by Sky's typechecker's separate proof. The `unsafe` keyword signals that the impl is part of Sky's trust boundary, not a contract rustc verifies.

**Edge case: default async fn state machines.** A specific exception to the global rule — see Section 14.5. Default (non-migratory) async fn state machines do NOT get `unsafe impl Send`. Sky's frontend, when generating their stub rlib code, omits the Send impl. From rustc's view, these futures are !Send. tokio::spawn cannot accept them.

### 12.2 'static falls out by construction (groups erase, no Rust-visible borrows)

Sky types are `'static` from rustc's view by construction, not by lying. The mechanism:

- Sky types have no Rust lifetime parameters in their definition (PhantomData<T> for type parameters, PhantomData<&'a ()> for group parameters; the 'a is erased to re_erased at use). So the type has no lifetime params surfacing to rustc; rustc treats it as having 'static-equivalent independence from external borrows.
- Sky types don't carry Rust borrows in their fields (Sky's typechecker enforces this — fields of Sky types are either values, owned references inside groups Sky tracks, or other Sky types; not Rust borrows surfaced to rustc).
- Group references erase to re_erased at the boundary, so even Sky source borrows that are stored in fields don't appear as Rust lifetime params.

Result: a Sky type that holds Sky data and has its group erased to re_erased is genuinely 'static from rustc's view. No lying needed. Rust APIs that require `T: 'static` accept Sky types automatically.

The 'static-ness is honest. Sky's typechecker enforces the real lifetime correctness (the group is alive when the type is used); rustc just sees a 'static type.

### 12.3 Unpin: per-future basis (migratory yes, default no)

Sky futures (Section 14) are NOT all Unpin. The split:

- **Migratory async fn state machines:** generate `impl Unpin for X {}`. They're Send + Unpin + 'static. tokio::spawn accepts them.
- **Default (non-migratory) async fn state machines:** do NOT generate `impl Unpin`. They're `!Unpin`. Pin is real for them. tokio::spawn doesn't accept them (the bound `F: Future + Send + 'static` is met, but `Unpin` doesn't apply — wait, actually `tokio::spawn` doesn't require Unpin directly, but the spawn point's Pinning machinery is different; see Section 17).

The Unpin honesty is preserved because it has runtime consequences: Pin's safe API forbids moves out of `!Unpin` types. Rust callers honoring Pin will correctly avoid moving non-migratory Sky futures. Sky's typechecker forbids moves of non-migratory futures from Sky source. Both sides honor pinning.

The mechanism: Sky's frontend, when generating the stub rlib's `impl Future for X` block, conditionally generates `impl Unpin for X {}` based on whether the source-level async fn is marked `migratory`. Migratory yes; default no.

### 12.3.5 Sync

Sync is handled analogously to Send. Sky's stub generation emits `unsafe impl Sync for SkyType {}` for Sky types that Sky's typechecker has proven are safe to share across threads. The default posture differs from Send though:

- **Send: opt-out via type definition.** Most Sky types are Send by default (global `unsafe impl Send`); types containing non-sendable parts (thread-local handles, etc.) opt out via a Sky-side `!Send` marker that suppresses the stub's `unsafe impl Send`.
- **Sync: opt-in via type definition.** A Sky type is Sync only if its source explicitly marks it as such (e.g., `sync struct Counter { value: AtomicI32 }`). Stub generation emits `unsafe impl Sync` only when explicitly marked.

The asymmetry is because Send-ability is a more common default (most data can be moved between threads), while Sync-ability requires careful design (most data shouldn't be shared across threads simultaneously). Sky's typechecker enforces Sync-correctness Sky-side; rustc just sees the `unsafe impl Sync` for opt-in types.

For Sky types that Rust APIs require to be Sync (e.g., values stored in `Arc<T>` where T must be Send + Sync), Sky source must mark the type as Sync explicitly. Sky's typechecker validates this is safe — the type's fields must themselves be Sync-eligible.

### 12.4 What's a "lie" vs "honest" claim and why

The distinction between lying and honest claims matters because they have different correctness obligations.

**Honest claim:** the property genuinely holds. Rustc's view and Sky's view of the type are consistent. The marker trait can be safely impl'd without `unsafe`. Examples in Sky: 'static (groups erase), some specific Sky types are Sync (single-threaded reads).

**Lie:** the property is asserted but not verified by rustc. The marker trait is impl'd with `unsafe`. The correctness is Sky's responsibility, enforced Sky-side. Examples in Sky: Send (Sky's typechecker enforces actual sendability; rustc sees `unsafe impl Send` regardless).

The reason Sky lies about Send rather than honestly impl'ing it: Sky has more nuanced send-correctness rules than Rust does. Some Sky types are sendable only in some scope-relativized ways that rustc's `Send` cannot express. Lying lets Sky use these types in Rust generic positions while keeping the real correctness enforced Sky-side.

The reason Sky doesn't lie about Unpin: Pin has runtime semantics. A non-Unpin type whose `unsafe impl Unpin` was a lie could be moved by Rust callers (via Pin's safe API), and Sky has no Sky-side mechanism to catch the move. Pin is a real correctness boundary; lying about it would produce real unsafety.

The reason Sky doesn't lie about 'static: it's honest, no lying needed.

### 12.5 Cross-references

- Section 14 — async fn migratory vs default distinction.
- Section 17 — tokio interop's bound-checking around these markers.
- Section 11 — group erasure that makes 'static honest.

---

## 13. Comptime

This chapter covers Sky's compile-time metaprogramming model in detail. The model is Zig-style: same expression language at comptime and runtime, with a slab-based representation of comptime values.

### 13.1 Zig-style comptime

Sky's comptime is Zig-style: the same expression language runs at compile time as runs at runtime. There is no separate "macro type system" — Sky has one type universe, used at both stages.

```sky
const N: I32 = 42

comptime {
    let x: I32 = 1 + 2  // evaluates at compile time
    print(x)
}

fn use_const() -> I32 {
    N + 1  // N is comptime-known; result is computed at runtime against compile-known N
}

fn use_comptime_arg<const T: Spaceship>() -> I32 {
    T.crew_count  // accesses a field of a comptime-known Spaceship value
}
```

The comptime evaluator is a tree-walking interpreter for Sky's normal expression language. There are no separate macro types; macro code (Sky comptime code) operates on actual values of the actual types.

Comptime is restricted:

- **No IO.** Comptime code cannot read files, environment variables, network resources. Determinism is a hard requirement.
- **No nondeterminism.** No timestamps, no random numbers, no system queries. Same input → same output.
- **Terminating.** Comptime evaluations must terminate. Sky's evaluator imposes a time budget (configurable; default ~10s); exceeded budgets produce a clear error.

These restrictions ensure that comptime evaluation is reproducible across machines, across Sky compiler versions, across compile sessions.

### 13.2 Slab-based machine simulation

Comptime values are represented in a slab — a contiguous byte buffer simulating RAM. The slab has:

- An offset register (slab pointer, conceptually). Each comptime allocation gets an offset; subsequent accesses use the offset to find the value.
- A typed-pointer-style representation. Each pointer in the slab carries enough metadata to dereference correctly (type, offset, possibly bounds).
- Standard allocation/deallocation via a bump-allocator-like discipline. Comptime values are typically short-lived (within a single comptime block); allocation is cheap.

```sky
comptime {
    let s = Spaceship::new()  // s is allocated in the slab; s is conceptually at slab offset 0x1220
    print(s.name)             // dereferences offset 0x1220, accesses the name field
}
```

The slab approach lets Sky's comptime simulate arbitrary computation. Sky's evaluator can model anything from simple arithmetic to elaborate data-structure construction. The slab is the "memory" the comptime program has access to; the evaluator is the "CPU" executing the comptime program.

### 13.3 Slab pointers as integer values to rustc

When a Sky comptime value crosses into rustc-visible territory (as a const generic argument to a Sky generic that's called from Rust source, for instance), the value is represented as a `u64` carrying the slab offset.

```sky
fn zork<const T: Spaceship>() { ... }

let s = comptime Spaceship::new()  // s at slab offset 0x1220
zork::<s>()
```

The stub rlib representation of `zork`:
```rust
pub fn zork<const T: u64>() -> () { ::std::unreachable!() }
```

The const generic parameter is `u64`, not `Spaceship`. Rustc has no representation for `Spaceship`. The 0x1220 slab offset is the value rustc substitutes.

When Sky's per_instance_mir provider is called for `zork::<0x1220>`, the provider:
1. Looks up offset 0x1220 in Sky's slab.
2. Recovers the actual Spaceship value.
3. Substitutes `T = Spaceship(crew_count=42, ...)` into `zork`'s body, evaluating comptime expressions that depend on T.
4. Produces the substituted MIR body.
5. Returns it to rustc's collector.

The collector walks the substituted body and queues whatever Rust deps it finds. Sky's codegen emits `zork`'s body with the comptime substitution applied.

### 13.4 Slab lifecycle: per-invocation, never serialized

The slab is per-rustc-invocation state. Created when Sky's machinery activates (after sidecar load, during after_expansion); discarded when the invocation ends. Never serialized to disk.

**Why not serialize:** the slab is dynamic compile-time state representing in-flight comptime values. It depends on the invocation's call graph (which comptime expressions evaluate, in what order, with what intermediate values). Serializing the slab would couple a Sky library's published artifact to the specific compile-session that produced it — a different compile session would produce different slab contents (different offsets, different allocation order).

Instead, comptime results that need to persist across invocations are baked into the Temputs in their resolved form. For example, `comptime fn compute_size() -> usize` evaluated at lib_a's compile time produces a `usize` result. That `usize` is what gets stored in lib_a's Temputs (as a literal, not as a slab reference). Downstream compiles read the `usize` from the Temputs and use it directly; no slab reference, no comptime re-evaluation.

For comptime values that genuinely cannot be resolved at the producing lib's compile (e.g., comptime that depends on a type parameter only known at the downstream's compile), the Temputs records the construction recipe, and the downstream's compile re-evaluates with the downstream's slab.

### 13.5 Comptime determinism requirement

Comptime evaluation must be deterministic:

- Same Sky source + same Sky version → same comptime values.
- Cross-machine, cross-compile-session reproducibility.

The restrictions in Section 13.1 (no IO, no nondeterminism) enforce this. Sky's evaluator also imposes ordering discipline (iteration over collections happens in deterministic order; comptime closures execute in deterministic order).

Determinism enables:

- Content-addressed typeids for comptime-produced types (Section 10.8).
- Reproducible builds.
- Cache-friendly comptime: a cache of `(comptime_fn_def_id, args) → result` is sound, because deterministic evaluation gives identical results on repeat calls. v2 may implement such caching.

### 13.6 Comptime can produce types, functions, values

Sky's comptime is rich: it can construct new types, new functions, and arbitrary values:

```sky
const MyOutput: Type = make_struct_type<MyInput>()  // comptime, returns a type

fn foo(x: MyOutput) -> MyOutput { ... }  // uses the comptime-produced type
```

Sky's typechecker, on seeing `MyOutput`, calls into the comptime evaluator to compute the type. The result is a new Sky type with a recipe (the call to `make_struct_type` with `MyInput` as arg). The recipe is content-addressed; the type gets a stable typeid.

Comptime-produced functions follow the same pattern — Sky's typechecker calls comptime to compute the function definition, gets back a new Sky function with a recipe and a typeid (well, a Sky-function-id; functions don't go through the SkyOpaqueType wrapper but do get Sky-side identity).

Comptime-produced values (Sky values constructed at compile time and used as generic arguments) are represented as slab pointers, as described above.

### 13.7 Synthetic items via `SkyOpaqueType<typeid>` wrapper (Option C from synthetic-items walk)

When a comptime-produced type appears in a Rust generic, Sky uses the `SkyOpaqueType<const T: u64>` wrapper to represent it:

```sky
const Widget = make_widget_type<I32>()    // comptime, returns a type

fn process(w: Widget) -> I32 { ... }       // uses the comptime-produced type
```

The stub rlib's representation of `process`:
```rust
pub fn process(w: SkyOpaqueType<typeid_widget>) -> i32 { ::std::unreachable!() }
```

Where `typeid_widget` is a content-addressed hash of the recipe `(make_widget_type, [I32])`.

Sky's `layout_of` override fires for `SkyOpaqueType<typeid_widget>`; the override looks up the typeid in the sidecar's typeid table, recovers the recipe, evaluates comptime with `I32` as arg, computes the layout, returns to rustc.

This is "Option C" from the design conversation's synthetic-items walk — the alternative that avoids new fork patches by representing synthetic types as parameterized wrappers around a stable identity. Section 10.6 covers the wrapper machinery in full.

### 13.7.5 Bounded expressibility of comptime substitution

A natural worry about Sky's Approach A substitution (Sky's per_instance_mir provider substitutes Sky-typed args itself): could the substituted body grow unboundedly complex? Could deeply nested call trees with intricate comptime evaluation produce arbitrarily large substituted bodies?

The answer (analog of erw's `why-outer-params-suffice.md` argument adapted for Approach A): **no, the substitution is bounded by source-level scoping.**

The structural argument:

1. **Sky source has finite scope.** Any expression in Sky source can only mention names that are in scope at that location — names of outer-fn params, names of explicitly-imported types/functions, names defined in Sky source itself. This is just well-typedness; the AST wouldn't exist otherwise.
2. **Substitution preserves the property.** When Sky's substitution engine replaces a type param `T` with a concrete type `MyType` in the body, every occurrence of `T` becomes `MyType`. If `MyType` is well-defined (it must be — Sky source named it), then the substituted body remains well-defined.
3. **Comptime evaluation produces well-typed Sky values.** Comptime is restricted to deterministic, well-typed computation. The result of comptime is a Sky value of a Sky type, possibly newly synthesized but still expressible in Sky's type system.
4. **Substituted bodies have bounded complexity per source location.** A given call site has a finite set of source-level dependencies (other Sky items, Rust items). After substitution, those become concrete dependencies. The list grows linearly with source complexity, not exponentially.

The worst-case substitution complexity is the source-program complexity. Sky's substitution engine handles any program Sky's typechecker accepts; if the substitution would explode, the source would already be unbounded.

Operational concern (not a soundness concern): for very large Sky projects with deep generic call graphs, the substitution work per Instance can be expensive. Memoization (Section 19.5) keeps it bounded within a single rustc invocation.

### 13.8 Content-addressed typeids for cross-compile stability

Typeids for comptime-produced types are computed as: `hash(canonical_construction_recipe)`. The canonical recipe is a deterministic serialization of the comptime call graph that produced the type.

The recipe includes:
- The DefId of the comptime function that produced the type.
- The args to that function, recursively canonicalized (Sky type args become their typeids; integer args become themselves; etc.).
- The comptime call graph leading to the type (if the comptime function called other comptime functions, those are included).

The recipe is deterministic: same Sky source, same comptime args → same recipe → same hash → same typeid. Cross-crate stability follows: lib_a and the binary's compile both compute the same typeid for the same logical type, because both have the same source and Sky version.

### 13.9 No synthetic DefIds in rustc

Sky's synthetic-items mechanism avoids the temptation of synthesizing new DefIds in rustc's namespace. The reason: synthesizing new DefIds would require additional fork patches to extend rustc's DefId allocator and item-loading machinery. The `SkyOpaqueType` wrapper approach is more elegant: all synthetic types share the wrapper's single DefId, parameterized by a const u64 typeid.

The result: Sky's fork stays at the four patches in §3.2 (per_instance_mir trio + `fill_extra_modules` hook). No additional fork machinery for synthetic items. The wrapper pattern handles everything via Sky's typeid table + sidecar.

### 13.10 Cross-references

- Section 1.2 — Sky's memory model and groups, which interact with comptime evaluation.
- Section 3 — the fork patches and why per_instance_mir is needed for arbitrary-typed comptime.
- Section 10 — the SkyOpaqueType wrapper machinery.
- Section 19 — per_instance_mir's interaction with comptime evaluation.

---

## 14. Closures and Async

This chapter covers how Sky source's closure and async constructs project onto Rust's Fn-trait and Future-trait infrastructure. The mechanism is named struct lifting + auto-generated impl blocks in the stub rlib.

### 14.1 Sky lifts closures to named struct types in stub rlib

When Sky source defines a closure that needs to flow into a Rust API (passed as `Fn`, `FnMut`, `FnOnce`, or any generic Rust API), Sky's frontend lifts the closure to a named struct type in the source file's containing stub rlib.

```sky
fn process_items(items: &[Widget]) {
    items.iter().filter(|w| w.is_active())  // closure
}
```

The closure `|w| w.is_active()` becomes a Sky-internal named struct, say `__sky_closure_42` (the suffix is a stable hash of source location). The struct contains captured state; the captured Widget reference is one field. The struct gets a stub rlib representation:

```rust
pub struct __sky_closure_42<'a>(::std::marker::PhantomData<&'a ()>);
```

And an `Fn` impl:
```rust
impl<'a> Fn<(&'a Widget,)> for __sky_closure_42<'a> {
    extern "rust-call" fn call(&self, args: (&'a Widget,)) -> bool {
        ::std::unreachable!()
    }
}

impl<'a> FnMut<(&'a Widget,)> for __sky_closure_42<'a> {
    extern "rust-call" fn call_mut(&mut self, args: (&'a Widget,)) -> bool {
        ::std::unreachable!()
    }
}

impl<'a> FnOnce<(&'a Widget,)> for __sky_closure_42<'a> {
    type Output = bool;
    extern "rust-call" fn call_once(self, args: (&'a Widget,)) -> bool {
        ::std::unreachable!()
    }
}
```

The stub rlib's `#![feature(fn_traits, unboxed_closures)]` is required for the trait impls. (Sky is already on nightly, so this is fine.)

### 14.2 Closure Fn/FnMut/FnOnce impls auto-generated with HRTB-compatible parameterization

The Fn impls are parameterized over `'a` to match the HRTB-compatible shape Rust callers expect (`F: for<'a> Fn(&'a Item) -> bool`). The 'a is a lifetime parameter that Rust's compiler binds at each call site.

Sky's auto-generation produces:
- One closure type per closure source.
- A set of Fn/FnMut/FnOnce impls based on the closure's capture pattern (move vs ref vs mut ref).
- The HRTB-compatible parameterization on all references in the call signature.

The Sky source doesn't see any HRTB syntax. The frontend handles the translation.

### 14.3 Async fns lower to named state machine types

A Sky `async fn` desugars to a named struct type in the source's containing stub rlib, similar to closures.

```sky
async fn fetch_widget(id: I32) -> Widget {
    let url = format!("/widgets/{}", id)
    let response = http_get(url).await
    parse(response).await
}
```

Becomes a named struct (say `__sky_async_fetch_widget`) with an `impl Future` block. The state machine's fields capture each `.await` point's state.

The naming convention for v1: auto-generated names like `__sky_async_<sourceloc_hash>` for closures, `__sky_async_<fnname>_<sourceloc_hash>` for async fns. Future Sky versions may add explicit naming syntax (`async fn foo() -> i32 as FooFuture { ... }`) for source-level clarity.

### 14.4 Linear futures (default): drop-while-executing = abort

Sky's default async fn produces a future state machine that is **linear** (cannot be silently dropped). Linearity is enforced via:

- Sky's typechecker: a default async fn's state machine type is linear. Sky source cannot drop it without explicit consumption.
- Sky's drop glue (Section 15): if a linear future is dropped (e.g., from Rust source, which Sky cannot prevent), the drop glue panics + aborts.

This means: dropping a default Sky future from Rust source aborts the program. Section 14.7 and Section 15 cover the mechanism.

Default async fns can have cross-await borrows (groups borrows across .await points). Sky's typechecker tracks the group lifetimes correctly. Pinning is real for default futures: rustc sees `!Unpin` on their stub rlib representation.

### 14.5 Migratory futures: opt-in via `migratory` keyword; Send + Unpin to rustc

Sky source can mark an async fn as `migratory`:

```sky
migratory async fn worker_task(state: State) -> ()  {
    loop {
        let event = state.recv().await
        process(event).await
    }
}
```

Migratory async fns are sendable across threads, movable, and explicitly cannot have cross-await borrows. Sky's typechecker enforces:

- No `&G T` borrow held across `.await`.
- All captures must be sendable.
- No self-references in the state machine.

In exchange, the state machine type gets:

- `impl Unpin` in the stub rlib. From rustc's view, the future is movable.
- The global `unsafe impl Send` (which applies by default — Section 12.1). Future is sendable.
- Implicit `'static` (groups erase).

So a migratory future satisfies `F: Future + Send + 'static + Unpin`. tokio::spawn accepts it. tokio's `JoinHandle::abort()` and `tokio::select!` (which drop futures) work — the drop glue is normal (drops captures, not panic).

Wait, that creates an inconsistency. If migratory futures' drop glue is normal but default futures' drop glue panics, how does Sky distinguish?

The mechanism: linear types' source-level `impl Drop` body is the panic-and-abort code; non-linear types' source-level `impl Drop` body (if any) is normal cleanup. The compiler's AST-rewrite pass (§15.7) inserts `Drop::drop(&local)` calls uniformly; the body that runs at scope end is whatever the user wrote, supplied via the standard cascade-discovery + `fill_extra_modules` pipeline. Linear vs non-linear is purely a Sky-source-level discipline tracked by the typechecker; the codegen path is the same for both.

### 14.6 Migratory propagation through call graph

Sky's typechecker propagates migratory-ness through the await graph:

- A migratory async fn can `.await` another migratory async fn. Fine.
- A migratory async fn cannot `.await` a non-migratory (default) async fn. Compile error: "migratory function cannot await a non-migratory future; non-migratory futures may hold borrows that can't cross threads."
- A non-migratory async fn can `.await` a migratory async fn. Fine.

The propagation is upward: a function that wants to be migratory must commit to its callees being migratory too.

### 14.7 Cancellable futures: opt-in via `into_cancellable(future, handler)`

Sky's default future is linear: dropping it aborts. To use a Sky future with Rust APIs that drop futures (tokio::select!, tokio::time::timeout, etc.), the user wraps the future:

```sky
let cancellable = into_cancellable(my_future, || {
    cleanup()
    log("future cancelled")
})

tokio::select! {
    result = cancellable => { handle(result) }
    _ = shutdown_signal.recv() => { /* cancellable dropped here; cleanup runs */ }
}
```

`into_cancellable<F, H>(future: F, handler: H) -> CancellableFuture<F, H>`:

- F is the underlying linear future. After wrapping, F is "consumed" — Sky's typechecker prevents accessing the original.
- H is a `FnOnce()` cleanup handler. Captures whatever state cleanup needs.
- `CancellableFuture<F, H>` is non-linear; can be dropped.
- The wrapper's Future impl polls F transparently.
- The wrapper's drop glue: if F completed normally (Ready), skip the handler. If F is still executing, run the handler then drop F.

Cancellable futures are opt-in. Sky source explicitly invokes the wrapper. Sky's typechecker tracks which futures are cancellable.

### 14.8 Migratory and cancellable are orthogonal

A future can be migratory but not cancellable (spawnable but not selectable), cancellable but not migratory (selectable but bound to its thread), both, or neither.

Sky source can express the combination:

```sky
let cancellable_migratory = into_cancellable(some_migratory_future, || cleanup())
let cancellable_default = into_cancellable(some_default_future, || cleanup())
```

Both are valid. The first is `Send + 'static + Unpin + droppable-via-handler`. The second is `!Send + !Unpin + droppable-via-handler`. Each is appropriate for different Rust APIs.

### 14.9 Pin handling: wrapper pattern, not type-system Pin in Sky source

Sky source does not have Pin in its type system. Sky's groups + linear types handle the equivalent role.

At the Rust boundary, the wrapper's Future impl includes:

```rust
impl<F: Future, H: FnOnce()> Future for CancellableFuture<F, H> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = unsafe { Pin::into_inner_unchecked(self) };
        // ... poll F, return Poll value ...
    }
}
```

For non-migratory futures, `!Unpin` is honored by Rust callers (Pin's safe API forbids moves). The `unsafe Pin::into_inner_unchecked` is correct because Sky's runtime guarantees no move has occurred (the future is stored in a stable location).

For migratory futures, `Unpin` is impl'd; `Pin::into_inner_unchecked` is trivially safe.

Sky source never writes Pin syntax. The wrapper handles all Pin-related work at the boundary.

### 14.10 Pre-execution vs running futures: the two-type split (Option B)

A locked decision from the design conversation: each Sky async fn produces **two distinct Sky types**, not one type with an internal "started" flag.

- **`SkyNotStarted_foo`** — the not-yet-executed future. Movable, droppable. Captures are stored, but no state machine progress has been made. Sky source can construct one, change its mind, drop it without consequence.
- **`SkyRunning_foo`** — the executing future. Linear. Cannot be dropped from Sky source. Sky's typechecker tracks this; drop from Rust source triggers the panic-on-drop destructor.

Transition is explicit: a `start()` method on `SkyNotStarted_foo` consumes it and produces a `SkyRunning_foo`. Once started, the future is committed to running to completion (or being cancelled via the cancellable wrapper).

**Why two distinct types rather than one type with an internal flag:** the type-system distinction makes the safety boundary visible to both Sky's typechecker and to Rust callers reading the stub rlib's API. The internal-flag alternative hides the distinction; the two-type alternative surfaces it. For Sky's "long-term correctness over short-term simplicity" posture (Section 1.5), the explicit type distinction is preferred.

**The hybrid for Rust caller ergonomics.** Forcing Rust callers to write `sky_fn().start().await` rather than `sky_fn().await` is unergonomic. So `SkyNotStarted_foo` also impls `Future` (or `IntoFuture`), with the `start()` transition happening implicitly inside its first `poll`. From Rust's view, calling `.await` on a `SkyNotStarted_foo` works naturally; the transition to `SkyRunning_foo` is internal Sky bookkeeping. Sky source uses the two types explicitly; Rust source sees a single Future-implementing API.

The mechanism:

```rust
// In the stub rlib for a Sky `async fn foo(x: i32) -> Result<Data, Error>`:

pub struct SkyNotStarted_foo(());
unsafe impl Send for SkyNotStarted_foo {}
impl Unpin for SkyNotStarted_foo {}  // pre-execution is always Unpin

impl SkyNotStarted_foo {
    pub fn start(self) -> SkyRunning_foo {
        ::std::unreachable!()
    }
}

impl Future for SkyNotStarted_foo {
    type Output = Result<Data, Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        ::std::unreachable!()
    }
}

pub struct SkyRunning_foo(());
unsafe impl Send for SkyRunning_foo {}
// Default async fn → no Unpin impl.
// Migratory async fn → impl Unpin for SkyRunning_foo {}

impl Future for SkyRunning_foo {
    type Output = Result<Data, Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        ::std::unreachable!()
    }
}

pub fn foo(x: i32) -> SkyNotStarted_foo {
    ::std::unreachable!()
}
```

**Drop semantics differ by type.** `SkyNotStarted_foo`'s drop glue frees captures normally. `SkyRunning_foo`'s drop glue panics (linear; see Section 15.7) for default async fns; for migratory async fns, drop glue is the standard captures-free path (migratory state machines have no self-references that need protected unwinding).

**Sky's typechecker tracks the two types separately.** A Sky function that returns a `SkyNotStarted_foo` is OK; a Sky function that returns a `SkyRunning_foo` is rare and triggers extra scrutiny (the running future must be explicitly threaded through to its consumer). Sky source rarely manipulates `SkyRunning_foo` directly; it's an intermediate type in the await pipeline.

### 14.11 Cross-references

- Section 12 — Send/Sync/'static/Unpin marker handling for futures.
- Section 15 — drop glue mechanism.
- Section 17 — tokio interop's interaction with futures.
- Section 11.8 — HRTBs that closures naturally produce.

---

## 15. Async Drop and Cancellation

This chapter covers Sky's drop and cancellation semantics in detail. The model: Sky doesn't have Rust-style async cancellation; cancellation is channel-based, and linear types panic-on-drop.

### 15.1 Sky-native race/select with channel-based cancellation

Sky's stdlib provides race/select primitives that do not drop losing branches. Instead, cancellation is signaled via Sky channels:

```sky
race {
    result = future_a => { use(result) }
    msg = shutdown_chan.recv() => { handle_shutdown() }
}
```

In Sky's race, if `shutdown_chan.recv()` fires first, Sky's race sends a cancellation signal to `future_a` via Sky's internal task-cancellation channel; the future receives the signal at its next yield point and exits cleanly. No drop, no panic. Sky's race doesn't drop any futures.

Sky's race is a v1 stdlib primitive. Its implementation uses Sky's runtime's task-cancellation mechanism (Section 17).

### 15.2 `into_cancellable` interface and semantics

When Sky source needs tokio compatibility (where Rust's drop-based cancellation is the model), the user wraps via `into_cancellable`:

```sky
let cancellable = into_cancellable(linear_future, cleanup_handler)
```

The wrapper's drop glue, as described in Section 14.7:
- Checks: did the underlying future complete normally?
- If yes (Ready was returned): skip the handler, free the wrapper's state.
- If no (still Pending or never polled): run the handler, then drop the underlying future.

The cleanup handler can do anything sync-allowed: send cancel signals to other tasks, free non-managed resources, log.

### 15.3 Sync cleanup handlers in v1, async cleanup deferred

For v1, cleanup handlers are sync (`FnOnce()`). The handler runs synchronously when drop fires.

Why sync:
- Simpler. Drop semantics match Rust's drop semantics (synchronous, returns when done).
- Async cleanup introduces complexity: when does cleanup actually complete? What if Sky's runtime is shutting down before cleanup finishes? Real questions, real work to handle correctly.
- v1 users can do most useful cleanup synchronously: release channel slots, send non-blocking signals, log events, mark slab values as freed.

Async cleanup is a v2 feature, introduced when concrete use cases prove necessary.

### 15.4 Drop ordering: outer cleanup first, then nested fields

When a cancellable future is dropped, the drop order is:
1. Outer cleanup handler runs (with access to whatever the outer wrapper captured).
2. Then nested fields drop in declaration order. If a nested field is itself a cancellable future, its cleanup runs as it gets dropped.

Outer cleanup gets to do its thing first (notify of cancellation, mark in-flight ops), then inner cleanup propagates from outside-in as fields drop.

### 15.5 Cleanup failure = abort

If a cleanup handler panics or otherwise fails, the program aborts. Same posture as Rust's drop: best-effort; if can't complete, the program is in an unrecoverable state.

Cleanup handlers should be simple, fail-safe code. They should not perform operations that can panic. Sky's typechecker can enforce: cleanup handlers cannot return Result (forcing simple, side-effect-only cleanup).

### 15.6 Normal completion skips cleanup, cancellation runs it

The wrapper's "started" or "completed" flag tracks state:

- `poll` returns Ready: the underlying future has completed. Mark "completed". The wrapper extracts the value, returns it; drop later sees "completed" and skips cleanup.
- `poll` returns Pending some times: the underlying future is still executing. Drop later sees "incomplete" and runs cleanup.

Cleanup runs only on cancellation. Normal completion is handler-free. If a user wants code to run on both completion and cancellation, they put it in two places (the completion branch of their async code, and the cleanup handler) — or use a shared helper.

### 15.7 The drop emission mechanism — AST-rewrite synthesis

**Updated 2026-06-23 (Phase E + E.b/c/d).** Drop is not architecturally
special; it is just a function that the language sometimes auto-calls.
Sky's frontend implements this principle through a single AST-rewrite
pass that runs immediately after type resolution. After the pass,
every downstream stage — dep walker, mono cascade, per_instance_mir,
codegen, symbol resolution, link — treats drop calls as ordinary
trait static calls with no drop-specific code paths.

**The mechanism in five steps.**

1. **Source-level Drop impls are normal trait impls.** A Sky type that
   needs cleanup carries an `export impl Drop for X { fn drop(&mut self) { ... } }`
   in Sky source. From Sky's typechecker's view, Drop is just a Rust
   trait that Sky source can impl, no different from `Clone`.

2. **stub_gen emits the impl declaration.** Stub source contains
   `impl Drop for X { fn drop(&mut self) { unreachable!() } }` (with
   `unreachable!()` body — Sky's `fill_extra_modules` supplies the
   real body, same as case4's Clone pattern). Stub_gen also emits
   `pub use core::ops::Drop;` unconditionally (gated on absence to
   avoid colliding with any user `use std::ops::Drop` import) so the
   trait DefId is always resolvable from the synthesis pass.

3. **Type resolution runs.** `resolve_fn_body` produces the typed AST
   as usual — no drop knowledge anywhere.

4. **`insert_scope_end_drops` synthesizes drop calls into the typed AST.**
   This is the ONE site that knows about Drop. The pass:
   - Skips entirely when the function has a non-void return (the
     caller's let-binding takes ownership of the returned value via
     move semantics; dropping our local would double-drop).
   - For each `let` binding in the function body whose type needs a
     drop (predicate `local_needs_scope_drop(tcx, ty, registry)` —
     see below), appends a synthetic `TypedStmt::ExprStmt(TypedExprKind::StaticCall { ty: "Drop", method: "drop", args: [Ref(Var(local_name))] })`
     to the block's stmts in REVERSE declaration order (LIFO — Rust's
     drop order).

5. **The synthesized calls flow through the existing pipeline unchanged.**
   - `walk_typed_body_for_deps` collects them via its standard
     `rust_method_deps` arm.
   - `collect_rust_deps_recursive` queues `<T as Drop>::drop` as a
     Rust dep through the trait-method dispatch path.
   - The per_instance_mir cascade surfaces the dep so rustc's mono
     collector queues the concrete monomorphization.
   - For Sky-defined Drop impls (e.g. `<Widget as Drop>::drop`), the
     cascade discovery (`is_consumer_trait_impl_method`) captures the
     instance and `fill_extra_modules` emits Sky's body — exactly the
     same path case4's Clone uses.
   - For std-defined Drop impls (e.g. `<Vec<Widget> as Drop>::drop`),
     rustc emits the body from std's source as part of its normal
     cross-crate generic mono.
   - `lower_typed_expr`'s `StaticCall` arm emits the LLVM call at the
     resolved symbol.

**The predicate.** `local_needs_scope_drop(tcx, ty, registry)` returns
true when the type has a callable Drop trait method:
- **Sky struct types** (`StructRef` / `Struct`): true iff
  `registry.trait_impls` contains a matching `impl Drop for <name>` entry.
- **Rust types** (`RustType`): true iff `tcx.adt_destructor(adt_def.did())`
  returns `Some(_)`. This is rustc's query for "does this ADT have an
  explicit `impl Drop`?" — returns `Some` for Vec/String/Box/etc.,
  `None` for Option/Result/Stdout/primitives whose drop semantics
  flow through auto-generated `DropGlue` with no trait-method symbol.
  Without this filter, synthesizing `Drop::drop` on the latter types
  ICEs rustc's mono collector ("failed to resolve instance for
  `<Option<i32> as Drop>::drop`").
- Everything else (primitives, refs, unsized types, type params): false.

**Linear types** are simply Sky source types whose user-written
`impl Drop` body invokes `sky_runtime_panic` + `abort()`. There is no
separate mechanism — the same compiler pass synthesizes the same kind
of `Drop::drop(&local)` call; the body that runs at scope end is the
user's panic-and-abort code, supplied via the standard cascade
discovery + `fill_extra_modules` pipeline.

```sky
// Sky source — linear type:
linear struct LinearWidget { fd: I32 }

export impl Drop for LinearWidget {
    fn drop(&mut self) {
        sky_runtime_panic("Sky linear type LinearWidget was dropped from Rust source. \
                          Linear types must be consumed via Sky-native operations.");
        abort();
    }
}
```

When rustc emits drop glue for `LinearWidget` (because Rust source
stored it on the stack, in a Vec, or anywhere else it gets dropped),
the drop glue calls `<LinearWidget as Drop>::drop`, which is the body
Sky's `fill_extra_modules` emitted from the source's panic-and-abort
implementation. The program terminates with the user's diagnostic.

The implication: Rust code that uses Sky linear types must explicitly
consume them via Sky-side operations (call `sky_consume(value)` or
similar). Without consumption, drop fires; abort happens. This is the
documented constraint on Rust callers of Sky linear-returning APIs.
Sky cannot enforce the consumption requirement from Sky source (the
consumer is Rust code); the panic+abort body is the safety net.

**Why this is principled.** "Drop is just a function the language
sometimes auto-calls" is honored at ~95%:
- ONE site knows about Drop: `insert_scope_end_drops` (~30 lines) +
  `local_needs_scope_drop` predicate (~25 lines) +
  `synth_scope_drop_call` AST builder (~20 lines) + one `pub use Drop`
  line in stub_gen.
- After the synth pass runs, every downstream stage treats drop calls
  as ordinary `StaticCall` AST nodes. No drop-specific code paths in:
  type resolution, dep walker, mono cascade, per_instance_mir, codegen,
  symbol resolution, link.
- Remaining 5% leak: the predicate hardcodes the trait name "Drop"
  (and method name "drop") at the registry lookup. Could be
  generalized to a marker-trait registry, but Drop is the only such
  trait Sky needs today.

**Historical (pre-2026-06-23): the retired mir_shims model.** Before
Phase E, Sky's `mir_shims` query override generated synthetic
DropGlue bodies that called Sky-emitted `__toylang_drop_<TypeName>`
symbols. The mechanism was:
1. Source-level Drop impls were the same as today.
2. Sky's `mir_shims` provider intercepted `InstanceKind::DropGlue(_, Some(ty))` for consumer types and returned a body calling `__toylang_drop_<TypeName>(ptr)`.
3. Sky's codegen emitted `__toylang_drop_<TypeName>` separately.
4. The chain `rustc-emitted drop_in_place → Sky-synthesized DropGlue body → `__toylang_drop_X` → Sky's body` carried the drop call to its destination.

The retirement was empirically validated 2026-06-23: the previous
override's `consumer_struct_name` lookup never fired across the entire
test suite (Finding A.5/A.6). `drop_in_place::<Widget>`'s body was a
no-op and `<Widget as Drop>::drop` was absent from binaries. The
machinery had never worked in any shipping fixture, so its removal
lost zero functionality.

### 15.8 Cross-references

- Section 14 — async fn linearity and migratory split.
- Section 17 — tokio's cancellation primitives that interact with cancellable wrappers.
- Section 24 — sidecar annotations that mark Rust APIs as drop-cancelling.

---

## 16. Panic Propagation

Sky uses `panic = "abort"` exclusively. This chapter covers the implications.

### 16.1 `panic = "abort"` enforced at the binary level

Sky's generated `.skybuild/Cargo.toml` includes:

```toml
[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"
```

Skyc regenerates the manifest on every build. Users cannot override the setting; the regenerated content always sets panic = "abort". Cargo applies this to the binary's compilation; all Rust dependencies inherit it (cargo enforces consistency across the build graph — you can't mix panic strategies in one build).

`proc-macro` crates and `build.rs` scripts compile with the host's panic strategy, not the target's. They're not in the final binary, so the strategy doesn't matter for runtime semantics.

### 16.2 No unwinding, no landing pads

Sky's compiled bodies do not emit landing pads. Sky's codegen does not model unwinding as a control-flow concept. If a Rust panic unwound through Sky frames, LLVM's unwinder would hit frames without landing pad metadata; behavior would be undefined.

`panic = "abort"` prevents unwinding entirely. At a panic point, rustc emits abort intrinsics. The process immediately calls `abort()`. Dies cleanly. No unwinding through Sky frames.

### 16.3 No `catch_unwind` semantics

`catch_unwind` doesn't work under `panic = "abort"`. There's no panic to catch — the process is gone before the catch would execute. Rust libraries that internally use `catch_unwind` for sandboxing silently lose the sandbox under panic=abort.

This is a known consequence of `panic = "abort"`, well-documented in the Rust ecosystem. Sky users encountering Rust libraries that depend on catch_unwind should use Sky-native error recovery patterns (Result-based) instead of trying to catch panics.

### 16.4 Result-based error model for recoverable failures

Sky's error model is Result-based. Recoverable failures return `Result<T, E>`; unrecoverable invariant violations panic (which under panic=abort, aborts).

Rust APIs that return `Result<T, E>` map naturally to Sky's Result type. Sky source uses pattern matching on Result, the same way Rust does:

```sky
match http_get(url) {
    Ok(response) => process(response),
    Err(e) => log_error(e),
}
```

Rust APIs that panic (e.g., `Vec::index` for out-of-bounds) are exposed to Sky source as-is. If Sky source calls them with bad inputs, the panic aborts the process. Sky source should typically use checked variants (`Vec::get`).

### 16.5 Foreign exceptions across FFI: UB, document don't

Compiling a Sky binary that links to C++ libraries which throw exceptions across the C++/Rust boundary is undefined behavior. Same posture as `panic = "abort"` Rust programs that link to such libraries.

Sky doesn't support libraries that throw foreign exceptions across the FFI boundary. Documented constraint; binary correctness undefined if violated. Users who need C++ interop must ensure the C++ side catches all exceptions before returning to Rust/Sky. Standard FFI hygiene.

### 16.6 Async cancellation is not a panic

When a Sky future is dropped:
- If linear: panic + abort (Section 15.7).
- If cancellable wrapper: cleanup handler runs (Section 15.6).
- If migratory: normal drop (free captures).

None of these are panics. Cancellation is normal scope exit (or Sky-specific abort for linearity violation), not a panic. Drop glue runs in normal scope exit contexts; under panic=abort no drop glue runs (the process dies first).

The distinction matters: Sky's cancellation model is orthogonal to Sky's panic model. Section 15 covers cancellation; this section covers panics. They don't interact.

### 16.7 Cross-references

- Section 15 — drop and cancellation semantics.
- Section 17 — runtime interop, including handling of panics from Rust async code.

---

## 17. Tokio and Runtime Interop

This chapter covers how Sky's runtime coexists with tokio (and other Rust async runtimes), and how Sky futures interact with Rust async APIs.

### 17.1 Sky's runtime and tokio's runtime coexist

Sky has its own runtime (executor, channels, allocator, group manager). Tokio is a separate runtime. They coexist as independent runtimes in the same process. Bridging is "Sky calls `tokio::spawn(future)` from Sky source" — normal Sky-calls-Rust mechanics, no new infrastructure.

When a Sky-defined migratory future is spawned via tokio::spawn, the future runs in tokio's executor, not Sky's. Sky's runtime doesn't know this future exists. tokio owns the lifecycle.

### 17.2 Waker integration via standard `Waker` ABI

Wakers cross between runtimes via Rust's standard `Waker` ABI. The ABI is thread-safe by design (`Waker: Send + Sync`); cross-runtime wakeups are safe.

When a Sky future running on tokio awaits something tokio-driven (a TCP read), the waker passed in is tokio's. The Sky future stores it; when the future yields, it returns the waker to tokio. When the resource becomes ready, tokio fires the waker, which schedules the future for re-poll. Standard cross-runtime waker pattern.

When a Sky future running on Sky's runtime awaits something Sky-driven (a Sky channel), the waker passed in is Sky's. Same pattern, just with Sky-owned waker.

When a Sky future running on tokio awaits a Sky-runtime resource, the Sky resource's signal fires the tokio waker. tokio schedules re-poll. Cross-runtime, works.

### 17.3 Cross-runtime wakeup hops and latency

Wakers crossing runtimes add hops:

- Sky future on tokio awaits tokio resource: 0 cross-runtime hops. Same runtime throughout.
- Sky future on tokio awaits Sky-runtime resource: 1 cross-runtime hop (Sky's resource fires tokio waker). Sky's resource thread invokes tokio's executor thread.
- Sky future on Sky-runtime awaits tokio resource: 1 cross-runtime hop. tokio's reactor thread invokes Sky's executor thread.

Each hop adds latency. For high-throughput async code, users may prefer to commit to one runtime and stay on it. For mixed-runtime code, hops are acceptable; Sky's documentation explains the cost.

### 17.4 Sky futures spawned on tokio execute on tokio's threads

When `tokio::spawn(sky_future)` is called, the Sky future runs on tokio's worker threads. The future's `poll` method runs there. Sky's state machine logic executes on tokio's threads.

If Sky code is thread-affinity-sensitive (holds non-Send state that only makes sense on one thread), this can surprise. Sky's typechecker forbids non-Send Sky source from spawning to tokio (the migratory bound, Section 14.5). For Sky-runtime-spawned futures, Sky's runtime keeps them on Sky's threads.

### 17.5 `spawn_blocking` separated per runtime in v1, unified in v2

Sky's runtime provides `sky.spawn_blocking(closure)` for CPU-intensive sync work. Tokio provides `tokio::task::spawn_blocking(closure)`. v1 keeps them separate. Sky users pick based on context (am I spawning Sky-async work or Rust-async work?).

v2 considers a unified API: `spawn_blocking(closure)` that dispatches to whichever runtime is currently active. The current-runtime detection is a thread-local query; the dispatch is automatic.

For v1, separate APIs. Users pick.

### 17.6 Mixed-runtime deadlock as a Sky-side correctness concern

A Sky future spawned on tokio awaits a Sky-runtime resource that fires only when a Sky-runtime task completes. If Sky's runtime is itself blocked (e.g., its threads are all parked waiting on something tokio is responsible for), deadlock.

This is a Sky-source correctness concern, not Sky's compiler's responsibility to prevent. Standard concurrent system reasoning applies. Sky's documentation should explain the patterns to avoid; Sky's typechecker is silent on it.

### 17.7 Cross-references

- Section 14 — migratory and cancellable futures' role in tokio interop.
- Section 15 — drop and cleanup semantics that interact with tokio's cancellation primitives.
- Section 22 — runtime infrastructure (Sky's stdlib spawn and channel APIs).

---

## 18. Build Orchestration

This chapter covers skyc's orchestration of cargo and rustc for a Sky build. The user invokes `skyc build`; skyc generates a `.skybuild/` workspace, spawns cargo, copies the result.

### 18.1 `sky.toml` as single user-facing configuration

Sky users write only `.sky` source files and a `sky.toml` manifest. They never edit a Cargo.toml directly. The sky.toml shape:

```toml
[project]
name = "my_app"
version = "0.1.0"
authors = ["Alice <alice@example.com>"]
edition = "2026"

[dependencies]
my_utils = { path = "../my_utils" }       # Sky library, local path dep
my_runtime = "1.2.0"                       # Sky library from crates.io
serde = { version = "1.0", features = ["derive"] }   # Rust crate
tokio = { version = "1", features = ["full"] }       # Rust crate

[features]
default = ["sync"]
sync = []
async = ["tokio"]

[[bin]]
name = "my_app"
source = "src/main.sky"
```

The manifest captures everything skyc needs to translate to a cargo workspace: project metadata, dependencies (both Sky and Rust, undifferentiated at the manifest level — skyc figures out which is which based on whether the crate has a Sky marker), feature flags, binary/library targets.

The format is TOML, mirroring Cargo.toml's style. Sky users familiar with cargo will recognize the structure; Sky-specific fields are clearly marked.

### 18.2 `.skybuild/` workspace generation

When skyc build runs, it generates a workspace at `.skybuild/` (gitignored by default; skyc emits the `.gitignore` entry into the project's `.gitignore` automatically on first build):

```
.skybuild/
  Cargo.toml                                  # workspace manifest
  Cargo.lock                                  # produced by cargo, committed by user
  .cargo/
    config.toml                               # rustflags, panic=abort, etc.
  rust-toolchain.toml                         # pins sky-nightly
  my_app/                                     # binary crate
    Cargo.toml
    build.rs                                  # Sky toolchain check
    src/
      lib.rs                                  # stub source for main.sky
      lib.sky                                 # symlink or copy of user's main.sky
      main.rs                                 # shim: `fn main() { __sky_main(); }`
    target/                                   # cargo's output directory
  my_app.sky-meta                             # sidecar for my_app's bin items
```

The exact layout is implementation detail; the principles:
- One workspace member per Sky crate.
- Generated Rust stub sources colocated with the user's Sky sources.
- `build.rs` enforces Sky toolchain presence (Section 21.3).
- Cargo.lock is committed (deterministic Rust dep versions).

### 18.3 Skyc translates `sky.toml` to `Cargo.toml`

For each Sky crate in the workspace, skyc generates a Cargo.toml:

```toml
[package]
name = "my_app"
version = "0.1.0"
edition = "2024"

[dependencies]
my_utils = { path = "../my_utils" }
my_runtime = "1.2.0"
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1", features = ["full"] }

[build-dependencies]
# Used by build.rs to detect Sky toolchain

[features]
default = ["sync"]
sync = []
async = ["tokio"]

[[bin]]
name = "my_app"
path = "src/main.rs"

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"
```

The Cargo.toml is generated, never user-edited. It's regenerated on every `skyc build`. Skyc reads sky.toml, walks the project for `.sky` files, generates the Rust stubs in src/lib.rs (and the bin shim in src/main.rs), writes Cargo.toml.

### 18.4 `Cargo.lock` placement

`.skybuild/Cargo.lock` is what cargo manages. Users commit it (skyc generates `.skybuild/.gitignore` excluding everything *except* Cargo.lock).

Alternatively, the lock could be placed at the project root as `sky.lock`. The design conversation didn't fully lock this. **Recommendation: `.skybuild/Cargo.lock`.** Reason: it's what cargo expects natively; no transformation logic needed; user's git interactions with the lock file are standard cargo flow.

### 18.5 Deterministic skyc output (no timestamps, sorted dicts)

Skyc's generated workspace content is bytewise deterministic given identical inputs (sky.toml + Sky source files):

- No timestamps in generated files.
- HashMap iteration order replaced with sorted iteration where it would affect output.
- No random IDs.
- No host-system-dependent paths (use workspace-relative paths in generated files).

This is a CI-testable invariant. Sky's CI builds projects twice (with cache wipes) and byte-compares the generated `.skybuild/` contents.

Determinism enables cargo's incremental compilation to work correctly: if the generated stub source is byte-identical to last time, cargo's fingerprint hash matches, cargo skips re-compilation.

### 18.5.1 Cargo's role: the build graph, parallelism, and one-process-per-crate

Skyc spawns cargo once (via `cargo build --manifest-path=.skybuild/Cargo.toml`). Cargo then owns the build graph orchestration:

- **Dependency resolution.** Cargo reads `.skybuild/Cargo.lock` (or generates it on first build). Pinned versions are honored. Rust deps are downloaded from crates.io if not cached.
- **Build graph topology.** Cargo computes the build order from dependency relationships. Independent crates can compile in parallel.
- **Process spawning.** Cargo spawns one rustc subprocess per crate. The Sky toolchain pin (`rust-toolchain.toml`) tells cargo to use Sky's forked rustc binary as that subprocess.
- **Incremental skipping.** Cargo checks fingerprints; unchanged crates skip compilation entirely.
- **Parallelism limit.** Cargo's `-j` flag controls how many rustc subprocesses run simultaneously. Default is "number of CPU cores."
- **Linking.** Cargo invokes the linker on the final binary's `.o` plus all dep rlibs.

**One rustc subprocess per crate.** This is rustc's standard model — cargo throws away each rustc process after one crate and starts fresh for the next. Each crate's compile is its own process, with its own TLS, its own query system state, its own panic hooks.

The model is enforced by rustc's design. rustc was not designed to compile multiple crates in one process; the `run_compiler` API expects a one-shot invocation. While calling it twice in the same process *technically works* on most nightlies, it is an under-tested code path with periodic regressions (interner sanity check failures, TLS pollution, etc.).

Sky's per-crate process model means each Sky-machinery activation is independent. Sky's universe is re-loaded from sidecars at each invocation. Sky's slab is created fresh and discarded at end-of-invocation. There is no cross-invocation state (no in-memory cache that persists across crate compiles within one cargo build).

The cost: cargo build of a large project may invoke rustc dozens of times, each with full Sky-machinery startup overhead (load sidecars, build typeid table, etc.). The overhead per invocation is small (~tens of milliseconds for sidecar loading), so the total overhead is bounded. Section 22 covers incremental compilation that skips entire crate compiles.

**Skyc does not spawn rustc subprocesses directly.** Earlier in the design conversation, an alternative was discussed where skyc spawns subprocesses of itself per rustc invocation (`skyc internal-rustc <args>`), bypassing cargo as a build-graph orchestrator. That model was rejected: cargo's build-graph machinery (dependency resolution, fingerprinting, parallelism) is months of work to replicate, and replicating it produces a worse build system. Skyc invokes cargo; cargo invokes Sky's `rustc` binary per crate.

### 18.5.2 Workspace-level Cargo.toml

`.skybuild/Cargo.toml` is the workspace manifest:

```toml
[workspace]
resolver = "2"
members = [
    "my_app",        # the bin crate
    "my_utils",      # local path-dep Sky lib (if present in project)
    # cargo will fetch other deps to target/deps/ automatically
]

[workspace.package]
edition = "2024"
rust-version = "1.86.0"

[workspace.dependencies]
# Workspace-shared dependency versions (optional optimization)
serde = "1.0"
tokio = "1"

[profile.dev]
panic = "abort"
debug = true

[profile.release]
panic = "abort"
lto = "thin"
debug = false
strip = true
```

The workspace-level manifest centralizes:
- Profile settings (panic = "abort" enforced at workspace level so it applies to every member crate).
- Workspace-shared dependency versions (so each member's Cargo.toml can reference them via `serde = { workspace = true }`).
- Edition and rust-version settings (shared across members).

Skyc regenerates this whole file on every `skyc build`. Users don't edit it. If a future Sky version needs additional profile settings (e.g., target-specific overrides), the change goes here in skyc's regeneration logic.

### 18.5.3 Skyc subcommand summary

The skyc binary exposes the following user-facing subcommands. Each is a thin wrapper that translates user intent to cargo invocations after generating the appropriate `.skybuild/` content.

- **`skyc build`** — Compile the project. Generates workspace, invokes `cargo build`. Default if no subcommand given.
- **`skyc build --release`** — Compile with the release profile.
- **`skyc build --target=<triple>`** — Cross-compile (Section 18.7).
- **`skyc check`** — Type-check only; don't codegen. Generates workspace, invokes `cargo check`. Much faster than build for typecheck-only feedback. Useful for IDE save-on-type loops.
- **`skyc test`** — Run tests. Generates workspace with test target enabled, invokes `cargo test`. Sky tests are Sky functions marked `#[test]` (Sky source attribute, same name as Rust's). Skyc generates Rust test wrappers that call into them. See Section 18.5.7.
- **`skyc run`** — Build and execute. Generates workspace, invokes `cargo run`. Arguments after `--` are forwarded to the binary.
- **`skyc fmt`** — Format Sky source files according to Sky's style guide. Sky-source-specific formatter (rustfmt cannot format `.sky` files).
- **`skyc new <name>`** — Create a new Sky project skeleton. Generates `sky.toml`, `src/main.sky` or `src/lib.sky`, `.gitignore`. Standard convention scaffolding.
- **`skyc add <crate>`** — Add a dependency. Updates `sky.toml` with the new dep. Optionally specifies version, features, path/git.
- **`skyc publish`** — Publish a Sky library to crates.io. See Section 21.1.
- **`skyc inspect <sidecar-path>`** — Dump a `.sky-meta` file in human-readable form. See Section 8.9.
- **`skyc clean`** — Wipe `.skybuild/` and `target/`. Sometimes needed to recover from corrupted cache state.
- **`skyc doc`** — Generate documentation from doc comments. Wraps `cargo doc` after generating workspace with doc-comment-preserving stub generation.

Each subcommand has rich `--help` output. Common flags (`--verbose`, `--quiet`, `--manifest-path`) work as in cargo.

### 18.5.4 Sky's testing model

Sky source has unit tests via attribute marking:

```sky
fn double(x: I32) -> I32 { x * 2 }

#[test]
fn test_double_basic() {
    assert_eq!(double(5), 10)
}

#[test]
fn test_double_zero() {
    assert_eq!(double(0), 0)
}
```

`skyc test` generates a Rust test harness alongside the normal stub rlib generation:

```rust
// In .skybuild/<crate>/src/lib.rs, when --test is active:

#[cfg(test)]
mod sky_tests {
    extern "Rust" {
        fn __sky_test_test_double_basic();
        fn __sky_test_test_double_zero();
    }

    #[test]
    fn test_double_basic() {
        unsafe { __sky_test_test_double_basic(); }
    }

    #[test]
    fn test_double_zero() {
        unsafe { __sky_test_test_double_zero(); }
    }
}
```

Sky's codegen emits `__sky_test_test_double_basic` and `__sky_test_test_double_zero` symbols. The Rust harness calls into them. If a test panics (via `assert!`, etc.), the program aborts (since panic=abort); the cargo test runner detects the abort and reports the test as failed.

**Limitation under panic=abort:** the test runner can only detect test failure (test process aborted with non-zero exit), not the specific assertion that failed. The error message must come from Sky source's assert macros, which print to stderr before aborting. Sky's assert helpers do this:

```sky
macro assert_eq!(left, right) {
    if !($left == $right) {
        sky_runtime_eprintln!("assertion failed at {}:{}: {} != {}", file!(), line!(), $left, $right)
        sky_runtime_abort()
    }
}
```

The eprintln goes to stderr; the abort signals failure. Cargo's test runner shows both.

Each test runs in its own process (cargo's default behavior). Test isolation is preserved despite the abort-on-failure model.

**Integration tests** (cross-module Sky tests) work via Sky's `tests/` directory convention, mirroring cargo's `tests/` directory. Skyc generates Rust integration test wrappers similarly.

**Doc tests** (Sky equivalent of Rust's doc tests) are deferred to v1.x.

### 18.6 Cargo invocation and `rust-toolchain.toml` pinning

Skyc spawns cargo as a subprocess:

```
skyc build → cargo build --manifest-path=.skybuild/Cargo.toml
```

Cargo inside `.skybuild/` picks up `.skybuild/rust-toolchain.toml`, which pins to `sky-nightly`. All rustc invocations during the build use Sky's forked rustc. Sky's machinery activates on every crate compile where the marker is present (Section 4.5).

Skyc's process model:
1. Parse sky.toml.
2. Walk Sky source files; produce internal representation.
3. Generate `.skybuild/` workspace (Cargo.toml files, stub rlib sources, lib.sky symlinks, build.rs files).
4. Spawn `cargo build --manifest-path=.skybuild/Cargo.toml [--release if requested]`.
5. Wait for cargo to complete.
6. Copy the produced binary from `.skybuild/target/<profile>/<binary_name>` to `./target/<binary_name>`.
7. Cleanup or persist `.skybuild/` based on user preference (default: persist for incremental).

Cargo's progress output flows through skyc to the user's terminal. Cargo errors are visible. Skyc's own errors (sky.toml parse failures, missing source files, etc.) are clearly marked as skyc errors, not cargo errors.

### 18.7 Cross-platform / cross-compile

For cross-compilation, skyc adds the cargo cross-compile flags:

```
skyc build --target=aarch64-unknown-linux-gnu
↓
cargo build --target=aarch64-unknown-linux-gnu --manifest-path=.skybuild/Cargo.toml
```

The cross-compile machinery is cargo's; skyc just passes through. Sky's runtime support library is built for the target during the cross-compile (Sky's toolchain ships the runtime support for supported targets, like Rust's rustlib).

Supported targets for v1: x86_64-unknown-linux-gnu, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc.

### 18.8 Cross-references

- Section 4 — toolchain installation that makes the orchestration work.
- Section 6 — stub rlib generation that skyc performs during workspace generation.
- Section 21 — distribution to crates.io includes published cargo-package layouts.
- Section 22 — incremental compilation interaction with the workspace.

---

## 19. Per_instance_mir and Dep Discovery

This chapter covers Sky's per_instance_mir provider in detail — the mechanism that supplies synthetic MIR bodies for Sky items during rustc's monomorphization phase.

### 19.1 Approach A (Instance-keyed) for arbitrary-typed comptime

Sky uses Approach A: Instance-keyed per_instance_mir, Sky-side substitution. The reason, as covered in Section 3.1: arbitrary-typed comptime arguments cannot be substituted by rustc's collector. Only Sky's frontend understands Sky-side comptime values.

The contract:

```
per_instance_mir(instance: Instance<'tcx>) -> Option<&'tcx mir::Body<'tcx>>
```

Sky's provider:
1. Checks: is the instance's def_id a Sky-defined item (from a Sky stub rlib, or one of Sky's synthetic items via SkyOpaqueType)? If not, return None (falls through to rustc's default `instance_mir`).
2. Looks up the item in Sky's universe by def_id.
3. Walks the item's body with instance.args substituted Sky-side. If the body involves comptime, evaluates comptime with the concrete args available.
4. Asks the consumer for the set of **Rust** items transitively reachable from this Sky item (the consumer is responsible for any Sky-internal traversal needed to enumerate those Rust deps — Sky-internal callees are not reported here; see the locked principle below). Constructs a synthetic MIR body that mentions each of those Rust deps as a `ReifyFnPointer` cast. Sky's codegen emits the actual body separately via `fill_extra_modules`.
5. Returns the synthetic MIR body wrapped in Some.

**Locked principle (chain context, 2026-06-21):** *Sky's per_instance_mir at mono time has one job — walk Sky's call graph to report back the **Rust** things Sky transitively calls. Sky-internal callees are not its concern.* Non-export Sky items have no rustc DefId at all (§9.3, §9.4); ReifyFnPointer requires a DefId target, so it is structurally impossible to use ReifyFnPointer for Sky-internal callees. Step 4 of the §5.5 chain — which proposed extending ReifyFnPointer to consumer→consumer edges — was dropped permanently as a category error. Sky-internal callees are discovered Sky-side (`walk_and_stash_internal_callees` in toylangc) and emitted by Sky's `fill_extra_modules` hook; rustc has no role.

### 19.2 Sky-side substitution

Sky's substitution engine is part of Sky's typing pass + comptime evaluator. It handles:

- Type parameter substitution (Sky's analog to rustc's `instantiate`).
- Comptime arg substitution (slab pointer values).
- Group param substitution (always to re_erased at the boundary, but Sky-side groups carry their full identity through Sky's substitution).
- Nested generics (Sky's analog to rustc's nested arg substitution).

The substitution machinery is well-defined Sky-side because Sky owns its type system. Rustc's substitution operates on rustc-known types; Sky's substitution operates on Sky-known types (which may include Sky-side concepts rustc has no representation for).

### 19.3 Synthetic MIR body construction for exports

The synthetic MIR body for an exported Sky item:

```mir
fn sky_synthetic_body(args) -> ReturnType {
    bb0: {
        // Mention each transitive Rust dep as a ReifyFnPointer cast.
        let _0: Vec_new_T_i32_Global = Vec::<i32, Global>::new as fn() -> Vec<i32, Global>;
        let _1: Vec_push_T_i32_Global = <Vec<i32, Global>>::push as fn(&mut Vec<i32, Global>, i32) -> ();
        // ... more casts for other deps ...
        
        // Mention each Sky type's size info via NullOp::SizeOf (where applicable).
        let _2: usize = SizeOf(MyType);
        
        // Terminator: unreachable.
        return;
    }
}
```

The body's only purpose is to drive rustc's collector to queue the transitive Rust deps for codegen. The body itself is never executed (the terminator is `Unreachable` or `Return` with a placeholder), and the body never produces a competing `.o` symbol because Sky's `collect_and_partition_mono_items` override filters consumer items out of rustc's CGU list before LLVM codegen — the body's MIR exists (rustc's mono walker uses it to discover Rust deps via the ReifyFnPointer casts) but never reaches LLVM lowering. Sky's `fill_extra_modules` hook is the sole emitter of consumer-item bodies. See §5.3.

Constructing valid MIR is fiddly (per @SMINCZ and the MIR construction notes from erw's docs):

- `Statement` and `BasicBlockData` are `#[non_exhaustive]`; use the constructor functions, not struct literals.
- `set_required_consts` and `set_mentioned_items` must be called with empty vecs on synthetic bodies (the normal `mir_promoted` pass doesn't run for them; the mono collector panics if these aren't set).
- Every BasicBlockData needs a terminator. `TerminatorKind::Unreachable` is valid for bodies that are never executed.
- `TypingEnv::fully_monomorphized()` is a typing-mode flag, not an input assertion; bodies containing `ty::TyKind::Param` placeholders flow through cleanly. Per Section 19.5, Sky's bodies do NOT have Param placeholders (Sky pre-substitutes them); the body is fully concrete.

### 19.4 ReifyFnPointer casts for Rust deps

The `Rvalue::Cast(CastKind::PointerCoercion(ReifyFnPointer(Safety::Safe), _))` is the mechanism that queues a Rust item for codegen. Rustc's collector walks the cast as a "use" of the target item, which triggers codegen for the target's concrete monomorphization.

Sky generates one ReifyFnPointer cast per direct Rust dep:

```rust
let _0 = Vec::<i32, Global>::new as fn() -> Vec<i32, Global>;
```

The cast's source is `Vec::new` (as a generic Rust fn item); the cast's target is `fn() -> Vec<i32, Global>` (as a concrete function-pointer type). Rustc's collector substitutes the cast's source with `T=i32, A=Global` (Sky's args, pre-substituted), queues the concrete `Vec::<i32, Global>::new` for monomorphization. The cast's target tells rustc the concrete shape; the source tells rustc which generic to instantiate; together they form a complete instruction for the collector.

Sky's per_instance_mir provider builds the casts for every dep enumerated during Sky's walk. The walk is recursive through Sky-internal callees: when the export's body calls a non-export Sky function, Sky walks the non-export's body too, enumerating its Rust deps, and includes those casts in the export's synthetic body.

### 19.5 Per-entry-point subtree memoization keyed by `(def_id, concrete_args)`

The walk should be memoized to avoid redundant work. The intended cache:

```rust
type WalkCache = HashMap<(DefId, GenericArgsRef<'tcx>), Vec<RustDep>>;
```

For each Sky item walked, with its concrete args, the resulting list of Rust deps would be cached and reused on subsequent walks of the same Instance. Across rustc invocations (different cargo crate compiles) the cache rebuilds — different compiles may have different reachable sets.

**Implementation status:** the toylang reference does NOT yet implement this memoization (per the module doc-comment in `rustc-lang-facade/src/queries/per_instance.rs`: "not implemented in this file yet"); every Instance currently recomputes its dep list. For toylang's fixture sizes this is negligible. Tracked as future work for Sky's larger projects.

The cache lookup is content-addressed: `(def_id, concrete_args)` is the key. For Sky internal callees with type params, the concrete args reflect the substitution at the call site; multiple paths to the same `(def_id, args)` see the same cache entry.

### 19.6 Default trait method resolution via `Instance::expect_resolve`

For Sky types that impl Rust traits, default trait methods (methods Sky didn't override in the impl block) are resolved via rustc's normal trait resolution. The mechanism: when Sky source calls `widget.clone_from(other)` (using `clone_from`, which `Clone` has as a default method), Sky's per_instance_mir provider generates a cast referencing the trait def_id with the substituted args, and `Instance::expect_resolve` (or rustc's analog) maps to the concrete default-method instantiation.

This is the same pattern as `@TVIMDGAZ`'s "use trait-def DefId + `[Self, ...]` args" rule. Sky's code that constructs rustc Instances for trait methods always uses the trait definition's method DefId, with args starting with the Self type.

```rust
let trait_method_def_id = tcx.associated_items(clone_trait_def_id)
    .find_by_name_and_kind(tcx, Symbol::intern("clone_from"), AssocKind::Fn, clone_trait_def_id)
    .unwrap()
    .def_id;
let args = tcx.mk_args(&[Widget.into()]);
let instance = Instance::expect_resolve(tcx, ParamEnv::empty(), trait_method_def_id, args);
let symbol = tcx.symbol_name(instance);
```

(Sketch.)

### 19.7 Cross-references

- Section 3 — the fork patches that make per_instance_mir possible.
- Section 5 — codegen's role after per_instance_mir provides the body.
- Section 10 — type representation that the synthetic body's casts reference.
- Section 9 — the export-only constraint that determines which items have DefIds.

---

## 20. Pipeline Ordering

This chapter describes the order of operations within a single Sky-active rustc invocation: when Sky's frontend runs, when Sky's codegen runs, how the phases interact with rustc's own pipeline.

### 20.1 Skyc invokes cargo; cargo invokes forked rustc

The outer pipeline:

```
1. User runs `skyc build`.
2. Skyc parses sky.toml, generates .skybuild/ workspace.
3. Skyc invokes `cargo build --manifest-path=.skybuild/Cargo.toml`.
4. Cargo walks the build graph, spawning rustc subprocesses for each crate.
   - For Sky-marked crates (stub rlibs + bin): Sky's forked rustc, with Sky machinery active.
   - For pure-Rust crates: Sky's forked rustc, with Sky machinery dormant (vanilla behavior).
5. Cargo invokes the linker on the bin's output.
6. Skyc copies the linked binary to ./target/.
```

The inner pipeline (one rustc subprocess for one Sky-marked crate compile) is the next subsections.

### 20.2 Forked rustc loads rlibs; sidecars deserialize into Sky's universe

When Sky's rustc compiles a Sky-marked crate:

1. **Startup.** Forked rustc starts. Parse argv. Identify the crate being compiled.
2. **Default Callbacks::config().** Sky's codegen backend is constructed; query overrides are installed (`per_instance_mir`, `layout_of`, `collect_and_partition_mono_items` (the partition filter — see §5.3 / §C.2), `cross_crate_inlinable` + `extern_queries.cross_crate_inlinable` (B16)). The driver also installs the `fill_extra_modules` hook via `extra_modules_hook::install_consumer_modules_hook()`. Retired overrides: `symbol_name` (retired 2026-06-24 Phase F — single-symbol architecture means rustc's default v0 mangler suffices; see §6.2 / §26.1), `mir_shims` (retired 2026-06-23 Phase E — see §15.7 / §F.18 for the AST-rewrite replacement), `codegen_fn_attrs` + `extern_queries.codegen_fn_attrs` (Option 4, retired 2026-06-22 alongside patch 5 — see §F.14.1 / §F.17), `consumer_lang_active` (patch-5 shim, retired 2026-06-22 with patch 5 removal from the fork), `upstream_monomorphizations[_for]` (A.2, retired in §5.5 Step 3).
3. **Rustc parses the local crate's Rust source.** The stub rlib's `src/lib.rs` (skyc-generated). Trivially fast.
4. **Rustc loads upstream rlibs.** Each loaded rlib goes through rustc's metadata-loader. Sky's machinery checks each for `__SKY_STUBS_MARKER`.
5. **For each Sky-marked rlib loaded:** Sky's machinery locates the adjacent sidecar (`my_utils.sky-meta`), deserializes the Temputs into Sky's in-memory universe.
6. **Callbacks::after_expansion().** All crates are loaded. Sky's machinery has full access to upstream universes plus the local crate's parsed Rust source.

### 20.3 Hook point: `Callbacks::after_expansion`

Sky's frontend runs at `Callbacks::after_expansion`. At this point:

- All loaded rlibs' Sky universes are loaded.
- Rustc's TyCtxt is populated; signatures, ADT defs, etc. are queryable.
- The local crate's Rust source (the stub rlib's lib.rs) is parsed.

Sky's frontend can now:
1. Parse the local crate's `.sky` source files.
2. Build Sky's local universe (typed AST for items defined in this crate).
3. Cross-reference with upstream Sky universes for items defined elsewhere.
4. Cross-reference with rustc's TyCtxt for Rust signatures used by Sky source.
5. Run Sky's typechecker. Resolve names, check types, validate group constraints.
6. Run Sky's comptime evaluator on any comptime expressions reachable from the typing pass.
7. Build Sky's Temputs for the local crate.

### 20.4 Sky's frontend runs: parse, typecheck, queue codegen

The frontend's output:

- **Sky's local universe** populated with typed AST for every local item.
- **Sky's codegen queue** populated with `(SkyItemId, concrete_args)` pairs for every item to emit. For libs (rlib compiles), the queue contains items that survive the export filter. For bins, the queue contains everything reachable from main, plus everything reachable from exports of the bin (since the bin can have exports too).
- **Sky's typeid table** populated for any synthetic types produced during comptime.

The codegen queue is consumed in step 6.7 (Sky's codegen pass). The Temputs is written to the sidecar at end-of-invocation.

### 20.5 Rustc typecheck/borrowck on stub bodies (trivial)

Rustc proceeds with its normal pipeline: type-check, borrow-check the local crate's Rust source. The stub rlib's `unreachable!()` bodies pass trivially — they're valid Rust, valid MIR, valid borrows. No errors expected.

### 20.6 Monomorphization fires per_instance_mir on exports

The mono collector starts. For each Instance the collector encounters whose def_id is Sky-defined, it calls Sky's `per_instance_mir` provider. The provider supplies the synthetic body (with ReifyFnPointer casts for Rust deps). The collector walks the body, queues the Rust deps, cascades through their transitive Rust dependencies.

For Sky-defined types whose layout is needed: the collector calls Sky's `layout_of` override. Sky's provider returns the opaque-with-size LayoutData.

For Sky-defined types whose drop glue is needed: rustc's standard DropGlue path produces the body. If the Sky type has an `export impl Drop for X`, the collector queues `<X as Drop>::drop`; the cascade discovery captures it; `fill_extra_modules` emits Sky's body — same path as case4's Clone. The compiler-synthesized `Drop::drop(&local)` calls (§15.7) are what drive the collector to queue the Instance in the first place.

For Sky-defined functions' symbol names: rustc's default v0 mangler produces the name (Phase F retired Sky's `symbol_name` override; single-symbol architecture per §6.2 means Sky's `fill_extra_modules` body uses the same name).

These overrides fire interleaved as the collector encounters their triggers. Sky's responses are computed Sky-side; rustc consumes them and continues its walk.

### 20.7 Sky's CodegenBackend produces `.o` for full reachable Sky universe

Codegen happens concurrently with rustc's pipeline rather than as a discrete post-mono pass:

1. **Before `start_async_codegen`,** rustc's `codegen_crate` calls `backend.fill_extra_modules(tcx, allocator)` synchronously on the main thread (patch 4 / §F.15). Sky's hook (`consumer_fill_modules`) walks Sky's codegen queue, asks the allocator to mint a rustc-owned `ModuleLlvm` per Sky CGU, wraps the borrowed `LLVMContext` + `LLVMModule` in suppressed-Drop Inkwell handles, and emits LLVM IR directly into the rustc-owned module. No bitcode serialization, no parse step.
2. **Rustc's standard partition runs, then Sky's filter rebuilds the CGU list.** Sky's `collect_and_partition_mono_items` override delegates to the default partitioner, then walks each CGU rebuilding it with consumer items removed (`is_consumer_codegen_target` returns true → skip). The filtered partition reaches `LlvmCodegenBackend::codegen_crate`; consumer items never appear in LLVM IR via the rustc-emitted path.
3. **`LlvmCodegenBackend::codegen_crate` processes the filtered partition** as usual: Rust items get their real bodies (including Phase-6 wrappers, Rust generic intermediaries, etc.); consumer items are absent from the partition entirely. Sky's contributed CGUs (from step 1) ride the standard optimize → ThinLTO-summary → emit pipeline as just-another-CGU. Cross-language inlining works because Sky's modules are in the same LTO pool as user-bin's bitcode and the Rust deps' rlib bitcode.
4. **`join_codegen` and `link` pass through** to the inner `LlvmCodegenBackend` unchanged. Sky's modules were registered with rustc by the allocator in step 1, so rustc finalizes them with everything else.

### 20.8 Output: rlib + sidecar (per-lib); per-lib `.o` for owned non-generics; binary `.o` for generics

For library compiles (stub rlib compiles):

- Output: `my_utils.rlib` + `my_utils.sky-meta`.
- The rlib's `.o` carries (a) Rust-side machinery rustc emits during its own flow — Rust generic intermediaries the cascade surfaces (case 4/6) and Phase-6 wrappers with their default `Hidden` linkage. Consumer-item stub bodies are filtered out of the CGU list by Sky's `collect_and_partition_mono_items` override, so they produce no `.o` symbol. Plus (b) Sky's `fill_extra_modules` contribution: real `External`-linkage bodies for the **non-generic** Sky items defined in this library and for cascade-discovered trait-impl methods whose impl lives in this library.
- The sidecar contains Sky's full Temputs for the library — exports and non-exports both.
- Sky does NOT produce per-library `.o` content for **generic** Sky items — those materialize only when concrete args are supplied downstream.

For binary compiles:

- Output: the linked binary at `target/<profile>/<binary_name>`.
- The compile emits Sky bodies for (a) the binary's own non-generic items and (b) generic monomorphizations reached transitively from the binary's entry points, and links everything together with the upstream rlibs' bodies.
- See §5.5 for the post-Step-2 split rationale + the `lto = false` cross-crate inlining trade-off.

### 20.8.5 Cross-crate Sky generic monomorphization

A common point of confusion: when Sky source in `my_app` (the binary) instantiates a Sky generic defined in `my_utils` (a library), where does the monomorphization happen?

**Answer: at the binary's compile, not at `my_utils`'s compile.** Sky inherits Rust's "downstream substitutor" model.

Walk through a concrete case. `my_utils` defines:

```sky
// my_utils/src/lib.sky
export fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}
```

`my_app` calls it:

```sky
// my_app/src/main.sky
import my_utils.wrap

fn main() {
    let w = wrap<I32>(42i32)
    print(w)
}
```

Timeline:

1. **Cargo compiles `my_utils` first.** Sky's `rustc` invocation produces:
   - `my_utils.rlib` containing the Rust stub source (with `pub fn wrap<T>(x: T) -> Wrapper<T> { unreachable!() }`) compiled by rustc.
   - `my_utils.sky-meta` (the sidecar) containing the typed AST for `wrap` (with `T` as a Sky generic parameter — unsubstituted).
   - **No `.o` containing `wrap<I32>` or any other monomorphization** — those don't exist yet because no caller has named a concrete T.

2. **Cargo compiles `my_app` next.** Sky's `rustc` invocation loads `my_utils.rlib` + `my_utils.sky-meta` into Sky's universe. Now Sky has access to `wrap`'s typed AST with `T` as a placeholder.

3. **`my_app`'s Sky's frontend walks main.** It sees `wrap<I32>(42i32)`. Sky records "I need to codegen `wrap<I32>`."

4. **Sky's per_instance_mir provider fires when rustc's collector asks about `wrap<I32>`.** Sky substitutes `T = I32` in `wrap`'s body, produces the synthetic MIR for rustc's collector to walk for Rust deps.

5. **Sky's codegen, at `my_app`'s compile, emits LLVM IR for `wrap<I32>`.** The substituted body. The emitted symbol lives in `my_app`'s `.o`.

6. **The linker resolves.** `my_app`'s binary contains both `main` (Sky-emitted from `my_app`'s sources) and `wrap<I32>` (Sky-emitted from `my_utils`'s AST, substituted at `my_app`'s compile time).

The mechanism is structurally identical to Rust's cross-crate generic monomorphization. Rust's collector also walks downstream-crate bodies and substitutes the upstream-crate generic MIR per Instance; the difference is who does the substitution. Sky does Sky-side substitution (per_instance_mir, Approach A); Rust does collector-side substitution. Either way, the downstream crate's compile is where the monomorphized body materializes.

**Implication:** the binary's compile is heavy. Every Sky generic the binary reaches needs Sky's frontend (substitute) plus Sky's codegen (Inkwell IR + llc) to produce. Section 5.5 covered this; this subsection makes the per-Instance timing explicit.

**Implication:** Sky libraries that are heavily generic produce small `.rlib` files but contribute substantial work to downstream compiles. A library with 100 exported generic functions contributes its 100 AST entries to the binary's per_instance_mir work whenever the binary reaches any of them.

**Implication:** the binary's compile timing depends on Sky's reachable surface across all libs. Adding a new exported generic to a deeply-used Sky library can slow down all downstream binaries' compiles. Sky library authors should think about API surface design with this in mind.

### 20.9 Cross-references

- Section 19 — per_instance_mir's role during mono collection.
- Section 5 — codegen backend's role during the codegen phase.
- Section 7 — sidecar's role at rlib-load time and write time.
- Section 6 — stub rlib's role in surfacing exports to rustc.

---

## 21. Sky Library Distribution

This chapter covers how Sky libraries are published to crates.io and consumed by other projects (Sky-aware and Rust-only).

### 21.1 Sky libs publish to crates.io via `skyc publish`

Sky uses crates.io as its primary distribution channel. `skyc publish` wraps `cargo publish`:

1. Skyc validates the project (typecheck, all tests pass, no errors).
2. Skyc regenerates the `.skybuild/` workspace (deterministically).
3. Skyc invokes `cargo publish --manifest-path=.skybuild/Cargo.toml --package=<name>` for each Sky library being published.
4. Cargo packages and uploads to crates.io.

The cargo package contains:
- The user's Sky source files.
- The skyc-generated `Cargo.toml`.
- The skyc-generated stub rlib Rust source.
- The skyc-generated `build.rs` (Section 21.3).
- The skyc-generated sidecar (yes, the sidecar is shipped in the cargo package).

Crates.io stores it as a normal Rust crate. Downstream cargo dependencies resolve it normally.

### 21.2 Generated artifacts (Cargo.toml, stub source, Sky source, sidecar)

A published Sky lib's cargo package layout:

```
my_utils-1.2.0/
  Cargo.toml                                  # skyc-generated
  build.rs                                    # skyc-generated, enforces Sky toolchain
  src/
    lib.rs                                    # skyc-generated Rust stubs
    lib.sky                                   # author's Sky source (verbatim)
    [other .sky files]                        # author's
  my_utils.sky-meta                           # skyc-generated sidecar
  README.md                                   # author-provided
  LICENSE                                     # author-provided
```

The `include` field in Cargo.toml controls what cargo packages:
```toml
include = [
    "Cargo.toml",
    "build.rs",
    "src/**",
    "*.sky-meta",
    "README.md",
    "LICENSE",
]
```

All Sky-specific files are included. Cargo packaging respects the `include`.

### 21.3 `build.rs` enforces skyc toolchain presence

The skyc-generated `build.rs` script:

```rust
fn main() {
    // Sky toolchain detection.
    if std::env::var("SKY_TOOLCHAIN_ACTIVE").is_err() {
        eprintln!("ERROR: This crate is a Sky library and requires the Sky toolchain to build.");
        eprintln!();
        eprintln!("Install: https://sky-lang.org/install");
        eprintln!("Then build with: skyc build");
        std::process::exit(1);
    }
    
    // Belt-and-suspenders: verify rustc identifies as Sky's fork.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(&rustc)
        .arg("-V")
        .output()
        .expect("failed to run rustc");
    let version = String::from_utf8_lossy(&output.stdout);
    if !version.contains("sky") {
        eprintln!("ERROR: The configured rustc isn't Sky's forked version.");
        eprintln!("Got: {}", version.trim());
        std::process::exit(1);
    }
    
    // Tell cargo to rerun build.rs only if it changes.
    println!("cargo:rerun-if-changed=build.rs");
}
```

Skyc sets `SKY_TOOLCHAIN_ACTIVE=1` when invoking cargo. The forked rustc's version string identifies as Sky (e.g., `rustc 1.86.0 (sky-2026-01-01 ...)`).

If a Rust user (no Sky toolchain) tries `cargo build my_utils`, the build.rs fails immediately with a clear error. No runtime panic surprise.

### 21.4 Pure-Rust users get a clear error, not a runtime panic

The build.rs check is the safety net. Without it, vanilla rustc would compile the stub rlib's `unreachable!()` bodies into real `panic!("unreachable")` code, producing an rlib that compiles and links but panics at runtime when any Sky function is called. The user wouldn't know why.

With the check, the failure is at build time, with an explicit "this requires the Sky toolchain" message and a link to the install instructions. Recoverable.

### 21.5 What works without skyc: `cargo doc`, IDE awareness via stub signatures

Even without the Sky toolchain installed, some things work:

- **`cargo doc` on Sky libs.** Rustdoc reads the Rust stub source, generates documentation for the Sky lib's API as it appears to Rust callers. The doc comments in the Rust stubs can be auto-generated by skyc from Sky source's doc comments. Useful for Rust users browsing a Sky lib on docs.rs.
- **IDE awareness.** rust-analyzer reads the stub rlib's Rust source, provides completion / hover / goto-def on Sky lib APIs. Basic editing experience works.
- **Crates.io publishing and search.** Sky libs appear in crates.io search results, with their metadata visible.

These work because cargo's metadata, rustdoc, and IDE tools all operate on Rust source. The Rust source is the stub rlib's `lib.rs`, which exists in the cargo package.

What doesn't work without skyc:
- `cargo build`. Build.rs errors out.
- `cargo install` of a binary that depends on Sky libs.
- Any operation that requires compiling the Sky lib.

### 21.5.5 Rust-only consumers can use non-generic exports only

A specific consequence of the stub-rlib model (Section 6): when cargo invokes vanilla rustc on a Rust crate that depends on a Sky library, the Sky lib's stub rlib bodies are `unreachable!()` panic stubs. Sky's machinery is *not* active in the Rust crate's compile (vanilla rustc has no Sky plugin), so Sky's `fill_extra_modules` hook never runs and the real bodies never materialize. The Rust crate's compile sees the stub bodies and treats them as ordinary panic code. (Even under post-Step-2 owning-crate emission, the real bodies are emitted by Sky's `fill_extra_modules` hook which only runs when Sky's machinery is active; vanilla rustc bypassing Sky's machinery sees only the stub `unreachable!()` bodies.)

This means: a Rust binary built by vanilla cargo against a Sky lib will compile (the stub rlib's `unreachable!()` is valid Rust) and link (the stub rlib has all symbols rustc expects), but calling any Sky function panics at runtime with "unreachable code reached."

**Mitigation 1: Sky's build.rs (Section 21.3).** Catches this at build time with a clear error message. Pure-Rust users without the Sky toolchain can't even build a Sky lib dependency.

**Mitigation 2: design constraint on Sky lib authors.** A Sky lib that wants to be usable from pure-Rust consumers *must* opt into the v2 precompiled-bodies feature (Section 21.7) and ship only non-generic exports for the Rust-consumable surface. Generic exports require monomorphization at the consumer's compile, which requires Sky's machinery active, which requires Sky's toolchain.

For v1: Sky libs are skyc-required. The build.rs check enforces this; there is no Rust-only path. Section 21.3 covers the user experience.

For v2: Sky lib authors can mark their library as "Rust-compatible" (no comptime, no advanced features) and skyc-publish ships pre-compiled bodies for common targets. Even then, only non-generic exports work for pure-Rust consumers (generics can't be precompiled into the lib's `.o` because the consumer hasn't supplied concrete args).

The constraint is documented loudly in Sky lib authoring guides. Users who want broad Rust ecosystem compatibility design their public surface around the constraint (non-generic facade types, generic types only for Sky-aware consumers).

### 21.6 What requires skyc: `cargo build`, transitive deps in Rust crates

The full Sky toolchain is required for:
- Compiling a Sky-marked crate.
- Compiling any crate that transitively depends on a Sky-marked crate (because cargo will eventually need to compile the Sky-marked crate, which requires skyc).

The transitive constraint: a Rust crate that depends on a Sky library makes its own consumers require the Sky toolchain. Standard ecosystem-split propagation.

For Sky users, this is fine — they have skyc. For Rust users who happen to depend on a Sky library, they need to install the Sky toolchain. Documented constraint.

### 21.7 v2: opt-in precompiled bodies for Rust-compatible Sky libs

A v2 feature: opt-in precompiled bodies. A Sky lib could declare itself as "Sky-pure" (no comptime, no advanced features that require skyc to compile), and skyc publish would precompile bodies for common targets. The published cargo package would contain:
- The Rust stub source.
- The Sky source.
- The sidecar.
- Pre-compiled `.o` files for common targets (linux-x86_64, macos-x86_64, etc.).
- A modified build.rs that detects vanilla-rustc compile and links the appropriate pre-compiled `.o`.

Pure-Rust users could use the lib natively (the build.rs falls back to the pre-compiled `.o` if Sky toolchain is absent).

The cost: complexity in skyc publish (pre-compile for each target), distribution size, cross-platform fan-out. The benefit: expands Sky's ecosystem fit. Deferred to v2 when concrete need emerges.

### 21.8 Cross-references

- Section 4 — toolchain installation that users need.
- Section 6 — what's in the stub rlib that users see.
- Section 8 — sidecar format that travels with the lib.
- Section 18 — skyc build orchestration that produces the published artifacts.

---

## 22. Incremental Compilation

This chapter covers how Sky's compile times interact with cargo's incremental machinery. v1 uses cargo's crate-level granularity; finer-grained Sky-internal incremental is deferred.

### 22.1 Cargo's crate-level incremental in v1

Cargo's standard incremental machinery operates at the crate level:

- For each crate in the build graph, cargo computes a fingerprint hash of inputs (source files, dep versions, profile settings).
- If the fingerprint matches the previous build's cached fingerprint, cargo skips the crate's compile.
- Cached `.rlib` and `.sky-meta` files are reused from `target/deps/`.

This works for Sky because skyc's workspace generation is deterministic (Section 18.5). When the user changes one `.sky` file, only the crates whose stub rlib source changed are invalidated. Pure-Rust deps are untouched.

**Performance characteristics:**

- Single-file changes in a Sky library that doesn't affect exports → only that library's compile + binary compile invalidated.
- Single-file changes in a Sky library that affects exports (signature change) → that library + all downstream crates that depend on it invalidated.
- Pure-Rust dep version change → that Rust crate + downstream invalidated; Sky crates not affected unless they depend on that Rust crate.
- Sky source change in the binary's main → only the binary's compile invalidated.

These are cargo's standard behaviors; Sky inherits them.

### 22.2 Sky-internal fine-grained dep tracking deferred to v2

Within a single crate compile, Sky walks every reachable item and codegenens it. There's no "this item is unchanged, skip its codegen" granularity in v1. Each compile redoes the work.

For small Sky projects (~hundreds of items), this is fine — codegen is fast. For larger projects, the per-compile cost grows.

v2 considers Sky-internal fine-grained dep tracking: a Sky-side query system (similar to rustc's red-green incremental) that caches `(SkyItemId, args) → codegen output` per-item. Changes to a single item invalidate only its downstream codegens. Cached codegen outputs are reused.

The mechanism is non-trivial:
- Sky needs its own fingerprinting machinery for items.
- Cached codegen outputs need to be storage-efficient.
- Cache invalidation must be correct (cross-item dependencies must be tracked).

For v1, Sky doesn't implement this; cargo's crate-level cache is sufficient.

### 22.3 Sidecar fingerprinting for future incremental machinery

The Temputs format includes a `fingerprint` field per item — a content-addressed hash of the item's typed AST plus its referenced types. v1 doesn't use the fingerprint; it's reserved for v2.

v2's Sky-side cache can use the fingerprint to detect "this item is unchanged from last build" without walking the AST.

Adding the fingerprint to the format from v1 is forward-compatibility for the v2 work. Cost: a few bytes per item in the sidecar.

### 22.4 Perf model

Sky's call-boundary cost is structurally similar to Rust's
`pub extern fn` cost at the non-LTO baseline: a real cross-crate call
with no inlining. With cross-language LTO (`lto = "thin"` or
`lto = "fat"`), Sky's emitted bitcode lives in the same LTO module pool
as Rust callers (per the `fill_extra_modules` patch, §F.15), so LLVM's
IR inliner crosses the language boundary the same way it crosses Rust
crate boundaries.

**Empirical measurements (2026-06-24, M-series macOS, LLVM 21.1.8).**
Bench fixtures live under `toylangc/tests/integration_projects/perf_bench/`;
the runner is `toylangc/tests/scripts/run_perf_bench.sh`.

| Bench | Workload | nolto baseline | thin LTO | Ratio |
|---|---|---:|---:|---:|
| 1 | 100M `add(a,b)` calls, with `black_box` defeating fold | O3: 86.7ms | O3: 57.9ms | **1.50×** |
| 1 baseline | Same shape, Rust→Rust (`test_helpers::bench_baseline_add`) | O3: 90.7ms | O3: 58.1ms | 1.56× |
| 2 K=100 | 10M calls, 100 units of work per call | O3: 11.7ms | O3: 7.9ms | 1.48× |
| 3 | 10M `<Widget as Drop>::drop` calls via `Vec<Widget>` drop | O3: 9.5ms | O3: 0.4ms | **25.5×** |
| 3 baseline (single-crate) | Widget defined IN user_bin; intra-crate inlining | O3: 0.33ms | O3: 0.38ms | 0.89× (both at floor) |
| 3 baseline (cross-crate) | Widget in `test_widgets` sibling crate (apples-to-apples vs Sky) | O3: 9.9ms | O3: 0.36ms | **27.5×** |
| 3 baseline (inline_never) | `WidgetNoInline` in `test_widgets` with `#[inline(never)]` Drop | O3: 9.4ms | O3: 9.5ms | 0.99× (floor) |

(Bench 3 numbers refreshed 2026-06-24 alongside the baselines; the
new median for `bench3_drop_o3_nolto` is 9.5ms vs the 10.0ms reported
in the original Phase B run — within the expected ~10% run-to-run
variance. The ratio is 25.5× now where it was 26.5× then; both fall
in the same "drop-heavy paths benefit massively from LTO" regime.)

Three findings drive the user-facing recommendation:

1. **Sky's call boundary essentially matches Rust's own cross-crate cost.**
   At O3 thin, Sky's 57.9ms and Rust-baseline's 58.1ms differ by 0.3% —
   well within run-to-run variance. Sky adds no measurable overhead
   beyond what Rust's own cross-crate boundary costs. The 1.5× LTO ratio
   on Bench 1 is the SAME ratio the Rust baseline shows; it's a property
   of the cross-crate boundary, not of Sky's specific emission.

2. **Drop-heavy code amplifies the LTO win massively, and the
   amplification is inherited from Rust's cross-crate Drop chain — not
   Sky-specific.** Bench 3's ~25× LTO speedup reflects LLVM inlining
   Sky's `Drop::drop` body into `Vec::drop`'s element loop, eliding the
   empty body, and vectorizing the resulting no-op. The pure-Rust
   cross-crate baseline (`bench3_rust_baseline_cross_crate_*`: Widget
   in the `test_widgets` sibling crate) shows **27.5×** under the same
   structural setup — within run-to-run variance of Sky's 25.5×. And
   Sky's thin-LTO result (371μs) matches the cross-crate Rust baseline
   (360μs) at 3% delta, mirroring Bench 1's 0.3% Sky-vs-Rust finding.
   The conclusion: **the ~26× LTO speedup is a property of the
   cross-crate Drop chain under LLVM's inliner, not anything
   Sky-specific.** Sky's drop emission gives LLVM the same elimination
   opportunity a pure-Rust cross-crate Drop impl does. Without LTO, the
   chain pays full FFI cost per element (~0.95ns/drop, matching the
   `inline_never` floor and Bench 1's nolto baseline). With LTO, it
   disappears in both languages.

3. **The `single_crate` and `inline_never` baselines bracket what LTO
   can deliver.** `single_crate` (Widget defined in the user_bin
   itself) shows nolto ≈ thin both at ~0.35ms — LLVM's intra-crate
   inliner already eliminates the empty Drop body at O3 without LTO;
   LTO has nothing left to do. `inline_never` (Widget in
   `test_widgets`, Drop impl carries `#[inline(never)]`) shows nolto ≈
   thin both at ~9.5ms — LTO literally cannot help when the inliner is
   forbidden. The per-element cost in `inline_never` (~0.95ns/drop)
   matches Bench 1's nolto baseline (~0.87ns/call), confirming the
   per-call cross-crate cost is dominated by dispatch overhead at this
   scale regardless of what the function body does. The two baselines
   sandwich Sky's actual operating point.

**User-facing recommendation.** For dev iteration use the cargo dev
profile's default (`lto = false` — thin-local LTO §F.16 still bridges
intra-invocation CGUs). For release builds and any perf-sensitive
testing, set `[profile.release] lto = "thin"`. Fat LTO trades faster
runtime for slower compile and a 2× symbol table without materially
beating thin LTO at these workloads.

#### 22.4.1 Queries Sky touches and their cache policy

Per the handoff's Decision 14 audit (2026-06-24): every Sky-overridden
rustc query is cache-safe by construction. The original prescription
(force `cache_on_disk_if(false)` via Provider slots) used an API that
doesn't exist on current nightly — `cache_on_disk_if` is a
query-declaration-time modifier in rustc's macro DSL, not a Provider
slot. The audit re-derived the safety story from upstream declarations:

| Query | Upstream `cache_on_disk_if` | Why safe under Sky |
|---|---|---|
| `per_instance_mir` | `false` (Sky fork patch) | Never disk-cached; per-compile re-derive. |
| `layout_of` | (none → default `false`) | Never disk-cached. Re-derived from sidecar at every compile. |
| `cross_crate_inlinable` | (none → default `false`) | Never disk-cached. Sky's override return depends on `is_sky_active(tcx)` marker walk. |
| `collect_and_partition_mono_items` | (none + `eval_always`) | Re-runs every compile; never cached. |
| ~~`symbol_name`~~ | ~~`true` upstream~~ | **Override retired 2026-06-24 (Phase F, handoff Decision 2).** Rustc's default v0 mangler is Instance-keyed, so the cache key includes type args; Sky's universe state changes flow through correctly via Instance variation. No active staleness risk. |

The CI fence at `toylangc/tests/cache_audit.rs` asserts every override
file carries a `cache-audit:` marker comment describing its
cache-safety reasoning. New overrides MUST add a marker; the test
catches drift.

The B21 risk entry (§25.2) tracks "per-query disk-cache staleness"
should rustc evolve the cache API in a way that makes one of the above
queries unsafe.

#### 22.4.2 Reproducing the benches

The bench fixtures + runner ship in the toylang reference implementation
under `toylangc/tests/integration_projects/perf_bench/` and
`toylangc/tests/scripts/run_perf_bench.sh`. The runner is **not** wired
into `cargo test` (integration tests are enumerated by name; perf
fixtures are intentionally not enumerated so the suite stays fast).
Run it manually.

**Prerequisites:**
- The Sky-fork rustc toolchain (`rustc-fork` per the workspace's
  `rust-toolchain.toml`). Verify with `rustup toolchain list` —
  `rustc-fork (active)` should be present and selected. If not, see
  `docs/historical/rebuilding-rustc-fork.md`.
- The `toylangc` binary built at `target/debug/toylangc`. Build with
  `LLVM_SYS_211_PREFIX=<rust-fork-tree>/build/<host-triple>/ci-llvm
  cargo build --bin toylangc`. The `LLVM_SYS_211_PREFIX` points at the
  LLVM tree the rustc fork was built against (one of `ci-llvm` or
  `llvm/build` under the fork's build dir).
- `llvm-objdump` on `$PATH`. Any LLVM version's objdump works for
  inspecting Mach-O / ELF symbol tables and disassembly; the runner
  auto-detects via `command -v llvm-objdump`.

**Single command:**

```bash
bash toylangc/tests/scripts/run_perf_bench.sh > tmp/perf-bench-results.md
```

The runner:
1. Walks each fixture directory under `tests/integration_projects/perf_bench/`.
2. For each: wipes `.toylang-build/`, invokes `toylangc build` with
   `CARGO_TARGET_DIR` pointing at the shared cache dir, runs the
   resulting binary 5 times, parses each `BENCH_ELAPSED_US=N` line,
   reports the median.
3. Captures `.text` size + symbol count via `llvm-objdump -h` /
   `llvm-objdump -t`.
4. Archives `_main`-symbol disassembly per fixture to
   `tmp/perf-bench-disasm/<fixture>.main.disasm`.
5. Emits a markdown report on stdout.

**Total wall-clock cost:** ~5-10 min cold (most of which is the
shared cargo target cache populating Rust deps once); ~2-3 min warm.
Bench iteration counts (100M for Bench 1, 10M for Bench 2 and Bench 3)
are tuned so each binary's measured region takes ~10-1000ms even
under the slowest configuration (O0 nolto) — large enough to swamp
process-startup noise but small enough that a 5-run median is stable.

**Smoke-test a single fixture without the runner:**

```bash
# Build:
cd toylangc/tests/integration_projects/perf_bench/bench1_o3_thin
rm -rf .toylang-build
SYSROOT="$(rustup run rustc-fork rustc --print=sysroot)"
DYLD_LIBRARY_PATH="$SYSROOT/lib" \
LD_LIBRARY_PATH="$SYSROOT/lib" \
CARGO_TARGET_DIR=../../../target/integration-projects-cache \
  ../../../../../target/debug/toylangc build

# Run:
DYLD_LIBRARY_PATH="$SYSROOT/lib" \
  ../../../target/integration-projects-cache/debug/bench1_o3_thin
```

The binary prints `<sum>` then `BENCH_ELAPSED_US=<microseconds>`.

**Adding a new bench cell.** Copy an existing fixture directory; edit
the `name` and `lto`/`opt-level` keys in `toylang.toml`; append the
new fixture name to the appropriate loop in `run_perf_bench.sh`. Keep
the `expected_output.txt` line `BENCH_ELAPSED_US=` (with no value)
so the runner's `grep -oE 'BENCH_ELAPSED_US=[0-9]+'` parsing
succeeds.

#### 22.4.3 Interpretation assumptions

The bench numbers in §22.4 are anchors, not absolutes. The user-facing
recommendation ("use thin LTO for release") holds across reasonable
variation. The specific ratios will drift; if you re-run and get
substantially different numbers, the following assumptions are where
to look first.

**Host hardware sensitivity.** The 2026-06-24 anchor numbers come from
an M-series macOS host with LLVM 21.1.8. Different microarchitectures
will produce different absolute numbers (linux-x86_64 vs macOS-arm64
differ measurably in branch predictor + cache hierarchy + LLVM
codegen-quality decisions). The LTO RATIOS are more stable than the
absolute numbers — the structural claim "Sky's call boundary matches
Rust's cross-crate boundary under LTO" should hold cross-platform; if
it doesn't on linux-x86_64, that's a bug worth investigating, not just
expected variance.

**Constant-fold defense (`black_box`).** Bench 1 + Bench 2's
`rust_caller.rs` files wrap the loop input and accumulator in
`std::hint::black_box`. Without this, LLVM at O3+thin folds the entire
100M-iter loop into a compile-time integer (`mov w8, #<closed-form-sum>`)
and the timed region is empty. The original 2026-06-24 morning bench
ran without `black_box` and reported 0μs everywhere LTO ran — a
finding that initially looked like "LTO is so good it costs nothing"
but actually meant "we measured nothing." If you write new benches,
copy the black_box pattern from `bench1_o3_thin/rust_caller.rs`.

**`black_box` doesn't defeat all optimization.** It forces a single
value through an opaque-to-LLVM pipe. The inliner can still inline
the called function; the vectorizer can still vectorize the loop;
the constant propagator can still propagate constants WITHIN the
function being called. What it prevents is reducing the WHOLE LOOP
to a closed form. If a future bench needs to measure call cost in
isolation (no inlining at all), use `#[inline(never)]` on the target
function instead — but note that doing so changes what's being
measured. The current Bench 1 reports "Sky's call cost when LLVM is
free to inline" which is the user-facing relevant number.

**Thermal / frequency drift.** macOS aggressively throttles when the
CPU heats up. The bench runner takes a 5-run median per fixture but
does NOT pin the CPU thermal state or pre-warm the frequency. Running
the full 17-fixture matrix takes ~3 min warm — short enough that
thermal drift is small but not zero. If you see > 10% drift between
consecutive runs of the same fixture, suspect thermal throttling
first (close other apps, run with `caffeinate -i`). The `tmp/perf-bench-disasm/`
archive should be identical between runs even when timing isn't —
that's a useful sanity check.

**Run-to-run variance.** Median-of-5 is sufficient to dampen noise
but not to characterize variance. Treat the reported numbers as
"approximately right ± 10%." For tighter confidence intervals,
re-run the runner multiple times and look at across-invocation
variation. Future work (handoff B-22) would migrate the runner to a
`criterion`-style framework with proper outlier detection + adaptive
sample size.

**Bench 3 (drop chain) runs through a Rust caller because of the
B10 residual.** The natural design — Sky `main` allocating
`Vec<Widget>` directly — triggers the LLVM 21 BitcodeWriter bug
(§25.2 B10) at opt-level ≥ 1 because ThinLTO's cross-CGU import
phase encodes/decodes bitcode containing the trigger pattern.
Moving Vec allocation to `rust_caller.rs` sidesteps it (Sky's
bitcode no longer contains the trigger pattern). The drop chain
itself still measures Sky's `Drop::drop` invocations — Vec's
stdlib drop iterates and calls `<Widget as Drop>::drop` per element.
The architectural claim being measured (cross-language LTO inlines
Sky's Drop body into Rust's Vec::drop loop) is the same regardless
of who owns the Vec allocation.

**LTO ratio interpretation.** The handoff's decision gate uses Bench 1
ratio to characterize Sky's "synthetic peak call overhead." Bench 1
nolto at O3 is ~87ms / 100M = 0.87ns per call — well within the range
of "real cross-crate function call cost on M-series." Bench 1 thin at
O3 is ~58ms; the 1.50× ratio is what an inliner gains by stitching
the loop body's `add(i, 1)` into the caller and letting subsequent
passes (vectorizer, scheduler) work on the unified body. **The same
ratio shows up in the Rust baseline (1.56×).** Conclusion: the 1.5×
isn't Sky-specific; it's a property of the cross-crate boundary under
M-series LLVM 21. Sky inherits it; doesn't add to it.

**Drop chain ratio interpretation.** Bench 3 measures the worst case:
10M empty Drops. Real Drop bodies do work (free a handle, decrement a
refcount, run cleanup logic), and that work cost will be at least
comparable to the per-call dispatch cost — meaning the ~25× ratio
shrinks toward the Bench 1 ratio (1.5×) as Drop bodies grow. **Don't
quote "Sky drops are 25× faster with LTO" as a user-facing number.**
The honest framing: "drop-heavy workloads benefit most from LTO; for
Drop bodies that do significant work, the LTO speedup approaches the
generic call-boundary ratio (~1.5×)."

**Apples-to-apples Rust baseline (added 2026-06-24, Phase B+).** The
pure-Rust Bench 3 baselines (`bench3_rust_baseline_*`) confirm the
above ratio is not Sky-specific. Pure-Rust with Widget in a sibling
crate (`test_widgets`) shows an LTO ratio of **27.5×** (nolto 9.9ms,
thin 0.36ms) — within run-to-run variance of Sky's 25.5×. Sky's
thin-LTO result (371μs) matches the cross-crate Rust baseline (360μs)
at 3% delta, mirroring Bench 1's 0.3% Sky-vs-Rust finding. The 25×
LTO speedup is therefore "a property of cross-crate Drop chains under
LLVM's LTO inliner that Sky inherits from Rust," NOT anything Sky
adds. The `bench3_rust_baseline_inline_never_*` variant (Drop has
`#[inline(never)]`) establishes the floor: when the inliner literally
cannot help, the per-element chain cost is ~0.95ns/drop — matching
Bench 1's general cross-crate call cost (~0.87ns/call). And the
`bench3_rust_baseline_single_crate_*` variant (Widget in the user_bin
itself) shows nolto ≈ thin at ~0.35ms — LLVM's intra-crate inliner
already eliminates the empty Drop body at O3 without LTO, so LTO has
nothing left to do. These three baselines sandwich Sky's actual
operating point in three independent ways, all of which confirm the
structural-equivalence story.

### 22.5 Deterministic output as a CI invariant

Sky's CI verifies deterministic build outputs as a regression test. The mechanism:

1. Build the project once. Hash all outputs (`.rlib` files, `.sky-meta` files, the binary).
2. Wipe `target/`. Rebuild. Hash all outputs again.
3. Compare hashes. Mismatch = determinism regression.

The CI invariant catches regressions in:
- Skyc's workspace generation (timestamps, random IDs, host paths leak into output).
- Sky's typing pass output (HashMap iteration order, etc.).
- Sky's codegen (any non-deterministic LLVM IR generation).
- Sidecar serialization.

Without the invariant, non-determinism would accumulate silently until a user noticed "my builds keep changing the binary's hash even with no source changes."

### 22.6 Cross-references

- Section 7.4 — sidecar determinism.
- Section 18.5 — skyc-generated workspace determinism.
- Section 28 — phasing of v2 incremental work.

---

## 23. Error Reporting and Diagnostics

This chapter covers how Sky reports errors to users, especially errors that span the Sky/Rust boundary.

### 23.1 Sky frontend errors in Sky terms, pointing at Sky source

Sky's typecheck/comptime errors are reported in Sky terms, with file/line/column references into Sky source files. The error messages explain Sky concepts in Sky terminology:

```
error: type 'Widget' is not 'Sendable' as required by tokio.spawn
  --> src/main.sky:42:5
   |
42 |     tokio::spawn(make_widget())
   |     ^^^^^^^^^^^^^ requires F: Migratory + Future
   |
   = note: Widget contains a Sky-internal lock that's not safe to share across threads
   = help: consider using tokio::task::spawn_local or Sky's own runtime
```

The error format mirrors rustc's well-known style (clarity, source highlighting, helpful suggestions). Sky users can read errors as easily as Rust users read rustc errors.

### 23.2 Rustc errors on stub rlib (rare; usually a Sky frontend bug)

If rustc's compile of the stub rlib produces errors, that's almost always a Sky frontend bug — Sky generated invalid Rust stub source. The error surfaces as rustc's output, pointing at the generated `lib.rs`.

Sky's error wrapper, when it sees a rustc error on a stub rlib, decorates the error with a note: "This error is in skyc-generated Rust source; please file a bug at [issue tracker URL]." The actual rustc error is preserved for the bug report.

### 23.3 Source position info in Temputs

Every Sky item in the Temputs carries source position info: file index, line, column. The file table maps indices to filenames relative to the cargo package root.

Source positions enable:
- Cross-crate error messages ("error in `my_utils::widget.sky` line 42").
- Debugging (the binary's DWARF references `.sky` source lines).
- IDE jump-to-definition that crosses crate boundaries.

### 23.4 Sky source files shipping enables cross-crate error context

Published Sky libraries ship their `.sky` source. When a downstream user gets a Sky error from a published lib (e.g., a generic argument doesn't satisfy a Sky trait bound), the error message can show the lib's source code at the relevant location:

```
error: type 'MyType' doesn't satisfy 'Serializable' as required by 'my_utils::publish'
  --> .skybuild/.../my_utils-1.2.0/src/lib.sky:130:5
   |
130|     pub fn publish<T: Serializable>(item: T) -> Result<...> {
   |                     ^^^^^^^^^^^^ bound here
```

Without the source shipping, cross-crate errors would have to refer to the lib only by name, not by source location. The shipping makes errors actionable.

### 23.5 Sidecar annotation skew detection

When a Sky lib's sidecar annotations specify expected Rust signatures, Sky's typechecker cross-checks them against rustc's actual signatures during typechecking. Mismatch produces an error:

```
error: sidecar annotation skew detected for tokio::spawn
  expected signature: F: Migratory + Future + Send + 'static
  actual signature: F: Future + Send + 'static (no Migratory bound)
  
  This usually means:
  - The annotated tokio version doesn't match the resolved version in Cargo.lock
  - Run 'skyc fix-annotations' to update
```

The skew detection runs at typecheck time. The cost is mechanical (one signature comparison per annotated API); the benefit is catching breakage early.

### 23.6 Cross-references

- Section 18 — how skyc's error output is structured.
- Section 24 — sidecar annotations format and skew detection.

---

## 24. Sidecar Annotations

This chapter covers the sidecar annotation format — supplemental information about Rust APIs that Sky's typechecker uses when reading those APIs.

### 24.1 What they are and what they cover

Sidecar annotations are TOML files alongside Rust crates. They describe:

- Group effects of Rust methods (which groups they mutate, return references into).
- HRTB structure of complex Rust APIs.
- Outlives bounds that don't naturally translate to Sky's group hierarchy.
- "drops_args" markers for Rust APIs that drop their arguments (e.g., tokio::select! drops losing branches).
- Linearity propagation rules for Rust APIs (whether a Rust API preserves linearity of its inputs).
- Other Rust-API-specific semantic information that Sky's frontend cannot infer from the Rust signature alone.

Sidecar annotations live in a separate file from the Rust crate's normal Cargo.toml. The annotation file is named `<crate>.sky-annotations.toml` and is discovered automatically:

```
.cargo/
  registry/cache/index.crates.io-.../
    tokio-1.32.0/
      Cargo.toml
      src/
      [...]
      tokio.sky-annotations.toml    # if present, Sky's typechecker picks it up
```

Annotations can be shipped with the crate (in the cargo package's `include`), or maintained out-of-band by Sky's ecosystem (a separate registry of community-maintained annotations for popular crates).

### 24.2 Primary source for binding info Sky's frontend can't infer

For Rust APIs where Sky's frontend can infer the correct binding from the Rust signature alone (most APIs), no annotation is needed. The typechecker reads the signature, generates the Sky-side binding, and proceeds.

For APIs that require Sky-specific information (group effects, HRTB structure, drop semantics), the annotation is the source of truth. Sky's typechecker reads the annotation; if missing for a needed property, Sky errors with a helpful "consider adding an annotation" message.

The annotation file format (sketch):

```toml
[crate]
name = "tokio"
version = "1.32.0"

[[binding]]
path = "tokio::spawn"
returns = "JoinHandle<F::Output>"
bounds = ["F: Migratory", "F: Future + Send + 'static"]

[[binding]]
path = "tokio::select"
drops_args = true
description = "Drops losing branches when one branch completes"

[[binding]]
path = "tokio::time::timeout"
drops_args = true
description = "Drops the inner future on timeout"

[[binding]]
path = "tokio::join"
drops_args = false  # joins, doesn't drop
```

### 24.3 Cross-checked against rustc's actual signatures at typecheck

Sky's typechecker reads the annotation and the actual Rust signature for each annotated API; if there's skew, errors per Section 23.5.

The cross-check catches:
- Cargo.lock version mismatch (annotated for v1.32, resolved version is v1.33 with different signature).
- Annotation typos.
- Stale annotations (the annotated crate has evolved, the annotation hasn't).

The skew detection is opt-in per crate (controllable via a crate-level flag in the annotation file); always-on for v1 (catches issues early; cost is mechanical).

### 24.4 Per-Rust-crate annotation files

Each Rust crate that has annotations gets its own file. Multiple crates can have annotations in the same project; Sky's annotation loader discovers them all and indexes by crate path.

The per-crate granularity matches cargo's package model. Annotations for one crate are tied to that crate's version. Updating one crate's annotation is independent of other crates' annotations.

### 24.5 Discovery convention

Sky's annotation discovery:

1. For each Rust crate the Sky project depends on (per Cargo.lock), Sky checks for an annotation file in the cargo cache directory.
2. If absent, Sky checks for a project-local override at `<project>/sky-annotations/<crate>.toml`.
3. If still absent, Sky uses defaults: assume no special group effects, no special drop semantics.

The project-local override lets users add annotations for crates that don't ship them (in-house crates, less-common crates, etc.). The user maintains the annotation; Sky picks it up.

### 24.6 Use cases: HRTBs, group effects, drop-cancellation, complex bounds

Examples of when annotations are needed:

**HRTBs.** Serde's `Visitor<'de>` pattern uses HRTBs that Sky's automatic translation can't handle. The annotation specifies the binding manually:

```toml
[[binding]]
path = "serde::de::Visitor"
sky_group_binding = { "de" = "input_group" }
```

**Group effects.** A Rust method that mutates a shared group, but the signature doesn't surface it:

```toml
[[binding]]
path = "MyCrate::SharedState::update"
mutates_groups = ["shared"]
```

**Drop-cancellation.** Rust APIs that drop their arguments:

```toml
[[binding]]
path = "tokio::select"
drops_args = true
```

**Complex bounds.** Rust APIs whose generic bounds don't naturally translate:

```toml
[[binding]]
path = "MyCrate::process"
sky_bounds = ["T: SkyEquivalent"]
notes = "Requires Sky-Equivalent which substitutes for Rust's PartialEq + custom Hash"
```

### 24.7 Cross-references

- Section 11 — HRTBs and group system that annotations help with.
- Section 23 — error reporting for skew detection.
- Section 14, 15 — drop semantics and cancellable wrappers that drops_args informs.

---

## 25. Risks

This chapter catalogs the long-term risks Sky's architecture faces, grouped by category. The taxonomy is inherited from erw's `docs/architecture/risks.md`, with Sky-specific adjustments. Each risk has a probability estimate, an impact estimate, a canary (an early-warning test), and a reaction strategy.

The general posture: Category A risks are unlikely but catastrophic; Category B risks are realistic ongoing concerns with bounded rework costs; Category C risks are operational discipline whose failure is self-inflicted but caught by tests.

### 25.1 Category A: rustc_private locked down, override_queries removed, query system replaced

**A1. `rustc_private` locked down.** Probability: <5% over 5 years. Impact: Sky's architecture ends. Sky's compiler depends on `#![feature(rustc_private)]` to access rustc's internal crates; without it, Sky cannot compile. Canary: deprecation warning on `rustc_private`, RFCs proposing removal, compiler-team communications about sunset. Reaction if it fires: collaborate with rust-lang on unlocking the specific surface Sky needs, or migrate to whatever replacement they provide. Sky has years of notice in any realistic scenario.

**A2. `Config::override_queries` removed.** Probability: <5% over 5 years. Impact: Sky's query override layer collapses. Canary: rust-analyzer or miri publicly migrating away, tracking issue for replacement. Reaction: depends on replacement; redesign around new mechanism. Weeks-to-months of rework per migrated query.

**A3. Query system replaced.** Probability: <1% over 5 years. Impact: Multi-month re-architecture of Sky's integration. Canary: major rust-lang announcement, multi-year tracking issue. Reaction: multi-month rebuild; concepts (interleaved monomorphization, opaque stubs, two-sided codegen) transfer; specific hooks don't.

### 25.2 Category B: mono collector drift, MIR construction API drift, ABI helpers drift, CGU lifetime issues

**B1. Mono collector behavior drift.** Probability: 30-50% over 5 years. Impact: 1-3 weeks repair per occurrence. Sky's per_instance_mir returns synthetic bodies containing ReifyFnPointer casts; the collector walks them and queues Rust deps. If the collector restructures, this mechanism may break. Canary: deep-dep-graph tests start failing with missing-symbol link errors. Reaction: read updated `rustc_monomorphize/src/collector.rs`, adapt Sky's body construction.

**B2. Partitioner restructure.** Probability: ~20-30% over 5 years (back at the historical level after the 2026-06-22 Option 4 retirement restored the partition filter). The B2 surface is "rustc restructures `collect_and_partition_mono_items` such that Sky's override's filter-and-rebuild pattern no longer suppresses consumer-item emission." Specific drift modes: (a) the partitioner adds a new field to `CodegenUnit` that Sky's rebuild loop doesn't copy (silently dropping data), (b) the partitioner's call ordering shifts such that some pass runs between Sky's filter and LLVM codegen and re-introduces consumer items, (c) the partitioner consults an internal cache that bypasses Sky's override output. (a) is the most likely; (b)/(c) are the failure modes that bit erw's pre-2026-06-21 partition filter empirically. Canary: link-time duplicate-symbol errors (Sky's `fill_extra_modules` body conflicting with rustc's now-real-bodied stub), or runtime SBMNBIZ-style panics from inlined unreachable bodies, or test fixtures that previously passed silently regressing. Reaction: 1-3 days repair — typically copying a new field, occasionally adapting to a partitioner restructure. The Sky-vs-erw contrast: Sky's Option C plugin re-asserts linkage during emission, so even if the filter partially fails on Sky's side, Sky's plugin owns the final say; erw doesn't have that backstop.

**B3. MIR construction API drift.** Probability: 100% per 6-month bump (some drift); ~40% over 5 years for structural rework. Sky's synthetic MIR body construction uses `rustc_middle::mir` directly; it churns. Canary: compile errors in `build_dependency_body`. Reaction: ~1 hour to 1 week per bump, depending on severity. Standard cost.

**B4. ABI helpers drift.** Probability: 15-25% over 5 years. Sky inherits erw's ABI helpers; PassMode variants and similar surface drift. Canary: ABI-shape tests fail. Reaction: 1-2 weeks repair.

**B5. CGU lifetime erasure fragility.** **CLOSED architecturally.** The earlier concern was that Sky might stash CGU references across the partition-override → codegen gap with `'static`-erased pointers (a delicate lifetime-laundering pattern erw briefly used). The architectural answer: **don't stash, re-call.** Call the saved upstream provider directly from inside `codegen_crate` where `'tcx` is live — `default_collect_and_partition()(tcx, ())` returns a sound `'tcx`-bound slice with no unsafe. Calling `tcx.collect_and_partition_mono_items(())` does NOT work for this; the in-memory query cache memoizes Sky's override's filtered result. The raw fn pointer bypasses cleanly. See §F.5 for the pattern.

**B6 (Sky-specific). Slab/comptime interaction with incremental cache.** Probability: ~30% over 5 years. Sky's per-invocation slab plus query-cache interactions may produce non-determinism if the slab is touched in incremental-cache-skippable code paths. Erw's B6 pattern applies here: query-provider side-effect fragility. Canary: tests fail deterministically on warm runs but pass on cold. Reaction: move side-effects to up-front walks (Sky's analog of erw's `populate_toylang_instances_from_cgus`). Pre-emptively designed in Sky's pipeline (Section 20).

**B7 (Sky-specific). Comptime evaluator nondeterminism.** Probability: ~20% over 5 years. Sky's comptime evaluator must be deterministic; if a regression introduces nondeterminism (HashMap iteration order leaking into comptime output), the reproducible-build invariant breaks. Canary: byte-comparison CI catches it. Reaction: identify the nondeterministic source, fix.

**B8 (Sky-specific). Debuginfo walker's source-vs-layout-field-count assumption.** **CLOSED architecturally** by the wrapper-as-field shape (§10.6). Rustc's `build_struct_type_di_node` and `build_union_type_di_node` iterate source-level `FieldDef`s and query `layout.fields.offset(i)` per source field, assuming `source.len() == layout.fields.count()`. The wrapper-as-field shape — `pub struct Foo(SkyOpaqueType<HASH>);` (1 source field) or `pub struct Foo<P...>(SkyOpaqueType<HASH>, PhantomData<(P...)>);` (2 source fields) — matches the count rustc expects, so the bound check holds. **Sky should adopt the wrapper-as-field shape from day one.** Under the older "opaque-with-zero-fields" shape (PhantomData only, layout reports 0 fields), the walker ICE'd whenever a Sky ADT appeared inside a Rust generic like `Vec<SkyType>`.

**B9 (Sky-specific). LLVM-binding-crate version skew with rustc's LLVM.** **CLOSED architecturally** by Approach B (patch 4 rev 2). Under the rustc-owns-lends shape, rustc constructs each per-CGU `ModuleLlvm` (`LLVMContext` + `LLVMModule` + `OwnedTargetMachine`) via `ModuleLlvm::new(tcx, name)` and lends Sky the borrowed pointers through an `ExtraModuleAllocator` callback. Sky's emitter wraps them in suppressed-Drop Inkwell handles (`Context::new` + `Module::new_borrowed`) and emits IR directly. **Sky has zero TargetMachine configuration to drift from rustc's** — the failure mode (Inkwell-bundled LLVM vs rustc's LLVM disagreeing on bitcode record format) cannot arise because no bitcode is serialized and no parallel context is constructed. The historical concern survives only as discipline on Inkwell's *Rust-binding* layer matching rustc-fork's LLVM major (so the FFI symbols resolve); the LLVM versions themselves are guaranteed identical by construction.

**B10 (Sky-specific). LLVM 21's bitcode writer drops FUNCTION records under ABI-coerced extern call signatures.** **CLOSED for the primary fill_extra_modules path; residual trigger under ThinLTO cross-CGU import.** The primary trigger (Sky's prior `write_bitcode_to_memory` → `parse_from_tcx` pipeline) is gone under Approach B; Sky's IR lands directly in rustc's `LLVMModule` and rides rustc's optimize → ThinLTO → emission pipeline as just another CGU. No `BitcodeWriter::writeModuleInfo` call happens in Sky's primary path. **However, ThinLTO's internal cross-CGU import phase still encodes/decodes bitcode**, and the bug re-fires under a narrow shape: Sky `main` + Vec containing a Sky struct with an `impl Drop` + opt-level ≥ 1. The trigger pattern is the same as the original B10 — an ABI-coerced extern call site whose declared param type differs from the call-site type. Empirically reproduced 2026-06-24 while scaffolding the perf bench: `bench3_drop_*` had to be restructured with a Rust caller driving the Vec allocation; under the Rust-caller pattern, Sky's bitcode no longer contains the trigger pattern and the bug doesn't fire. The historical IR-text round-trip workaround (formerly `llvm_gen::roundtrip_text_to_bitcode`) was retired in the Phase 4 migration. Until the upstream LLVM bug is fixed, Sky's emission discipline should avoid creating ABI-coerced extern call signatures in IR that may flow through ThinLTO import; the toylangc-side codegen-quality follow-up (eliminate the signature mismatch at the emission layer) is tracked in handoff.md's deferred-investigation list. See §F.18 for the empirical narrative.

**B11 (Sky-specific). Round-trip workaround scaling cost unmeasured.** **CLOSED — no round-trip occurs.** B11 was the meta-risk that B10's mitigation might scale poorly. With B10's mitigation retired (no bitcode is written, no IR text is printed, no parse happens), the question of round-trip cost at production scale is moot. The per-build cost contributed by Approach B is whatever Inkwell's direct IR construction already costs — the same path Sky was using to build the in-memory module before the round-trip, minus the round-trip itself. Memory pressure also returns to baseline (no triple-buffered original-module + IR-text + re-parsed-module peak).

**B12 (Sky-specific). MIR inliner cross-crate inlinable leak on Sky export stubs.** **PARTIAL — gated by build.rs in v1; revisit at v2 precompiled-bodies.** Originally closed by the `#[inline(never)]` discipline on every Sky export stub in `stub_gen` (toylangc commit `82a9c4d`). The concern: generic Sky exports default to `cross_crate_inlinable = true`; rustc's `encoder.rs` exports their `optimized_mir` (the `unreachable!()` body) into rmeta; a downstream Rust crate compiled outside Sky's machinery could in principle inline the `unreachable!()` body into a Rust caller. `#[inline(never)]` makes `cross_crate_inlinable` return false, blocking the export. **F1 investigation (2026-06-20) re-evaluated**: in v1 Sky's distribution model is source-shipped libs (§21.1-21.5), and `build.rs` (§21.4) hard-errors on missing `SKY_TOOLCHAIN_ACTIVE` before any vanilla-rustc consumer reaches codegen. The B12 scenario can't fire in v1. Meanwhile, `#[inline(never)]` blocks LLVM's cross-language inliner from inlining Sky's body even within a Sky-aware build (matrix-empirically: Sky's body remained as a tail-jump from main rather than being constant-folded). The attribute was removed from the three Sky-item sites (accessor/wrapper/trait-impl-method) in `stub_gen.rs` to unblock LTO inlining; the Phase-6 stdlib helpers (`__toylang_option_unwrap` / `__toylang_result_unwrap`) keep their `#[inline(never)]` per §6.6.5 (stable-symbol concern, not cross_crate_inlinable). Note that B16's `cross_crate_inlinable` query override (which returns `false` for all consumer items in Sky-active compiles via `is_sky_active(tcx)`) now also provides a second line of defense at the rmeta-export level — equivalent to what `#[inline(never)]` previously did, but uniformly applied without per-stub attribute decoration. **When v2's opt-in precompiled-bodies feature (§21.7) is designed and built, this protection should be re-introduced gated on a stub_gen `precompiled_rlib_mode` flag** — because precompiled rlibs ship as binary artifacts that vanilla rustc CAN consume without hitting the build.rs check. The disambig portion of the historical rationale (gate-bypass at -O>=2) is independently closed by share_generics=true at the stub rlib + the partition filter (so no AvailableExternally body exists to misplace); see B14. Test fence: the inlining matrix (Thread C) Priority A/C/D fixtures, which now codify Sky-body inlining at every -O3 mode + -O1+ across opt-levels.

**B13 (Sky-specific). Sky's emitted rustc-visible symbols stripped by LLVM `GlobalOpt` + `GlobalDCE` at -O>=2.** **CLOSED architecturally** by pinning every Sky-emitted extern wrapper in `@llvm.used` (initially landed as `@llvm.compiler.used`; later upgraded to `@llvm.used` per B15, which subsumes the B13 protection and also defeats LTO `internalize`). The discovery was: at user-bin compile, Sky's emitted `<Wrapper<T> as Clone>::clone` body has no in-module caller (the only references come from the stub rlib's `duplicate<Wrapper<T>>` body, which is in a different compile unit). LLVM's `GlobalOpt` pass internalizes globals with no in-module use, then `GlobalDCE` removes them. Sky's `__toylang_main` accidentally survived this because the user-bin's bin shim (`fn main() { __toylang_main(); }`) provided an in-module reference. The fix is the same `llvm.used` mechanism rustc uses for its own critical emissions; see `toylangc/src/llvm_gen.rs::pin_in_llvm_used`. Test fence: `test_release_mode_smoke` (the `release_mode_smoke` fixture at `opt-level = "3"`).

**B14 (Sky-specific). Originally framed as "v0 mangler instantiating-crate disambig mismatch at -O>=2"; reframed empirically as "AvailableExternally `unreachable!()` body co-located with caller's CGU at -O>=2"; closure mechanism reset 2026-06-22 with the partition filter restored.** **CLOSED** by the restored `collect_and_partition_mono_items` filter (see §C.2) which removes consumer-defined items from rustc's CGU list before LLVM codegen. The B14 failure mode (whether framed as mangler-disambig or as CGU-placement-of-AvailableExternally-body) requires rustc to emit *some* body for the consumer item to LLVM; under the restored filter, rustc emits nothing for consumer items, so neither failure mode can arise. The 2026-06-22 contrast probe (`tmp/patch5-empirical-2026-06-21/`) empirically confirmed that the AvailableExternally-co-located-with-main scenario was the failure mechanism under Option 4 + patch-5-off, and that the partition filter prevents it structurally. Pre-2026-06-22 closure (Option 4 + patch 5 + share_generics + cstore-walk) is retained below as design history.

**Symptom (original framing):** `cargo build --release` on any toylang/Sky fixture matching `case_generic_impl_block`'s shape (Sky-owned trait impl reached through a Rust generic intermediary) fails at link with undefined symbols mismatching on instantiating-crate disambig (`__lang_stubs` vs the bin's crate name).

**Symptom (post-Option-4 empirical re-investigation, §F.17):** under the Option-4 architecture, the failure mode was runtime SBMNBIZ (binary panics at the `unreachable!()` stub body), not a link error. The mangled symbol names are identical with or without patch 5 (verified empirically via `llvm-objdump -t`). What patch 5 actually changed was the partitioner's CGU placement: with patch 5 ON, rustc placed the AvailableExternally `unreachable!()` body in a separate CGU from callers; with patch 5 OFF, it co-located them, and LLVM's intra-CGU inliner pulled the `panic` body directly into main → SBMNBIZ at runtime. This is design history as of 2026-06-22 — see §F.17 retirement.

Pass-through preserved: the partition filter is a no-op on pure-Rust crates because `is_consumer_codegen_target` returns false for every item (no marker, no consumer items). Toylang reproducer + fence: `tests/integration_projects/release_mode_smoke/` + `test_release_mode_smoke`. **Patch-5 retirement was investigated and ultimately shipped 2026-06-22** by retiring Option 4 + patch 5 together; the contrast probe at `tmp/patch5-empirical-2026-06-21/VERDICT.md` empirically validated §F.17 before the retirement landed.

**B15 (Sky-specific). LTO `internalize` pass changes External linkage on Sky-emitted rustc-visible symbols at `lto = "fat"`.** **CLOSED architecturally** by pinning every Sky-emitted extern wrapper in `@llvm.used` (not the weaker `@llvm.compiler.used` variant). The distinction matters: `@llvm.compiler.used` blocks only `GlobalDCE` from removing the function; `@llvm.used` ALSO blocks LTO `internalize` from changing the linkage AND blocks the linker's dead-strip pass. Under `lto = "fat"` (and only fat — ThinLTO and non-LTO don't run `internalize` the same way), Sky's emitted clone bodies were being silently demoted from `External` to `Internal` linkage during LTO's pre-merge `internalize` step. The resulting `.o` had the symbol present but as a local definition (objdump: `l F` vs `g F`); the stub rlib's cross-crate reference to it then went unresolved at the linker step. Symptoms before the fix: identical-looking "undefined symbol" link errors as B14, but at `lto = "fat"` only — `lto = "thin"` and `lto = false/off` worked fine. The disambig (`__lang_stubs`) was correct in the symbol name, masking the real cause (linkage demotion) as a disambig issue. Toylang reproducer + fence: `tests/integration_projects/opt_level_3_fat_lto_smoke/` + `test_opt_level_3_fat_lto_smoke`. See `toylangc/src/llvm_gen.rs::pin_in_llvm_used` for the implementation + the doc-comment that captures the three LLVM passes the pin defeats. Codified as `@SMPLZ` arcanum.

**B16 (Sky-specific). `cross_crate_inlinable` items emitted as `available_externally` linkage at -O>=1 — Sky's emitted call sites can't resolve.** **CLOSED architecturally** by an override of the `cross_crate_inlinable` query (both `queries.cross_crate_inlinable` for local items and `extern_queries.cross_crate_inlinable` for upstream items read from rmeta) that returns `false` when `is_sky_active(tcx)` returns `true`. (Post-patch-5-retirement 2026-06-22, the override calls the marker-walk helper `crate::is_sky_active(tcx)` directly rather than going through the deleted `consumer_lang_active` query.) Symptoms before the fix: at -O>=1, Sky's `__lang_stubs::clone_it<UserBinType>` body references `<UserBinType as RustTrait>::method` (the user-bin's derived impl). Rustc's default `cross_crate_inlinable = true` (auto-attracted by `#[derive(Clone)]` etc.) makes rustc emit such items with LLVM `available_externally` linkage — body present in IR for inlining, no `.o` symbol produced. Normal Rust callers inline via LLVM's IR inliner, satisfying the reference. Sky's emitted call sites go through direct LLVM call instructions and don't engage rustc's IR-level inlining, so the symbol reference dangles at link. Same root cause for upstream items (e.g. `<Vec<i64>>::new` in case 5 fixtures) — rmeta-encoded `cross_crate_inlinable` value carries the same flag, override applies symmetrically via `extern_queries`. Pass-through preserved: `is_sky_active(tcx)` returns `false` for pure-Rust crates (no marker anywhere), so the override delegates to the default provider. Cost in Sky-active compiles: marginal code-size increase (cross-crate-inlinable items get real `.o` symbols rather than `available_externally`), no semantic effect. The fix replaces what was previously documented as a "sibling-of-B14" gap (matrix-tracked as 12 ignored fixtures: case 3 / case 5 across LTO modes + Priority B annotation variants). The link errors looked superficially similar to B14 (undefined symbol with user-bin disambig) but had a completely different mechanism — B14 was about WHICH disambig the v0 mangler picked at the call site; B16 is about WHETHER rustc emits a real symbol at the definition. Test fence: the un-ignored case 3 / case 5 fixtures in the inlining matrix.

**B17 (Sky-specific). Partition filter as an architectural mechanism for the consumer-emits-bodies / rustc-emits-bodies collision.** **CLOSED 2026-06-22 by the restored partition filter** (after a brief 2026-06-21 detour through Option 4's `codegen_fn_attrs` AvailableExternally stamping, which had to be reversed because the AvailableExternally body created a CGU-placement hazard that required keeping rustc-fork patch 5 — see §F.14.1, §F.17). The filter walks rustc's CGU list post-partition, removes every item matching `is_consumer_codegen_target`, and returns the filtered list. Consumer items reach LLVM only via the consumer's `fill_extra_modules` hook (rustc fork patch 4) — sole def at link time. Description of the pre-2026-06-22 Option 4 mechanism retained below as design history. Background: Sky's `stub_gen` emits `pub fn foo() { unreachable!() }` for every export. Rustc's default codegen would compile each body to a `panic("unreachable")` blob, producing a competing `.o` symbol next to the consumer's `fill_extra_modules` body (rustc fork patch 4). The historical fix (~107 lines in `rustc-lang-facade/src/queries/partition.rs`, file deleted in commit a51bd7c) walked rustc's CGU list after the default partitioner and removed every consumer-defined item before the LLVM backend saw it. Option 4's replacement: rustc emits the body as `AvailableExternally` (LLVM IR-only, no `.o` symbol), so the partition-filter is unnecessary. The partitioner short-circuits at `rustc_monomorphize::partitioning::mono_item_linkage_and_visibility` when `mono_item.explicit_linkage(tcx)` returns Some, which it does via the override (`codegen_fn_attrs` → `codegen_instance_attrs` → `explicit_linkage`). Pass-through preserved: override gated by `is_consumer_codegen_target(tcx, def_id)` which only fires for items in marker-bearing crates. F1's LTO inlining promise preserved: `AvailableExternally` bodies still participate in LTO IR linker pool; the inlining matrix's 40 fixtures verified post-Option-4 that case 1a/2/4/6 main bodies still constant-fold (`mov w8, #42`) cross-crate. The codified safety invariant for the AvailableExternally body is @SBMNBIZ (§26.17). **Update (later 2026-06-21):** A.1.X (capture-ship-replay drain at user_bin) and A.2 (`synthesize_upstream_monomorphizations` query overrides) have also been retired — cascade trait-impl discoveries now drain inline at the stub_rlib compile via the pure function `collect_consumer_trait_impl_instances(tcx)` (§8.9.5), and the augmented `upstream_monomorphizations_for` map is no longer needed because share_generics=true at the stub rlib populates rustc's natural cstore-walk map (commits b09a90b, ff0cfe8). A.1.Y (the consumer's intra-Sky transitive-callee walk, `walk_and_stash_internal_callees`) stays — it's Sky's correct architectural place for discovering Sky-internal callees and has nothing to do with rustc's mono walker. Test fence: the inlining matrix + the integration suite. See §F.14.1 for the full retirement rationale.

**B14 and B15 produce byte-identical link errors.** Both surface as `Undefined symbols for architecture arm64: "..._12___lang_stubsINtB4_7WrapperlENtNtCs..._4core5clone5Clone5cloneB4_", referenced from: ...duplicate... in lib__lang_stubs-XXX.rlib[N](...rcgu.o)`. The disambig `__lang_stubs` is correct in both cases — the difference is invisible from the link error alone. **The disambiguating check is `llvm-objdump -t` on the user_bin's post-LTO `.o` file:** if Sky's emitted symbol is `g F __TEXT,__text _<symbol>` (global), the symbol is correctly linkage-pinned and the chain breakage is upstream — investigate the share-generics gate, the augmented `upstream_monomorphizations_for` map, and the stub rlib's metadata-recording of the offending Rust intermediary. If the symbol is `l F __TEXT,__text _<symbol>` (local), LTO `internalize` demoted it — SMPLZ discipline is wrong (pin missing or using `@llvm.compiler.used` instead of `@llvm.used`). Future investigators hitting this error pattern: check the objdump first.

**Investigation playbook for "Sky symbol disappeared somewhere in the LLVM pipeline".** When Sky-related symbols vanish (undefined-symbol link errors, mysterious runtime traps, or behavior that works at one opt-level but not another), the recipe that surfaced B15 generalizes:

1. **Preserve intermediate artifacts.** Build with `RUSTFLAGS="-C save-temps"`. Rustc keeps every CGU's bitcode at every stage (pre-opt `.no-opt.bc`, post-opt-pre-LTO `.lto.input.bc`, final `.o`). They live in `<target>/debug/deps/`.

2. **Identify Sky's CGU.** Sky uses `build_sky_cgu_name(tcx)` which embeds `sky.0` in the CGU name. Multiple CGUs are produced for the bin; only one is Sky's. The fastest filter: `llvm-dis -o - <each .no-opt.bc> | grep "@llvm.used\|@llvm.compiler.used"` — Sky's CGU is the one with the pin global.

3. **Verify Sky's emission at the pre-opt stage.** `llvm-dis -o - <sky.cgu>.no-opt.bc | grep "^define"`. The expected set is `__toylang_main`, the per-fn internals (`__toylang_internal_*`), and the rustc-mangled extern wrappers (`_RNvXs_..._5cloneB4_` etc.). Missing entries here mean the emission code itself never ran; check `populate_toylang_instances_from_cgus` and `fill_module`.

4. **Check post-LTO (or post-codegen for non-LTO) `.o` symbol table.** `llvm-objdump -t <post-lto.o> | grep -E "g     F|l     F"`. Compare what's `g F` (global, exportable) vs `l F` (local, internal). The shape of the demotion is the clue:
   - All present as `g F`: the emission survived and the issue is upstream (disambig coordination, missing entries in upstream_monomorphizations_for, etc.).
   - Present as `l F`: LTO `internalize` ran and demoted them. Check the `@llvm.used` global in the pre-LTO bitcode is correct (`@llvm.used`, not `@llvm.compiler.used`, and contains every rustc-visible symbol).
   - Absent entirely: `GlobalDCE` removed them. Same fix — `@llvm.used` pin missing.

5. **For LTO-specific issues, check both `lto = "thin"` and `"fat"`.** They run different internalize and DCE passes. A fixture clean at one but failing at the other is a strong hint about which pass is responsible. ThinLTO is less aggressive about internalize than fat; fat LTO is the canary for SMPLZ-class issues.

This recipe found B15 in under an hour. Future "symbol disappeared" investigations should start here.

**B21 (Sky-specific). Per-query disk-cache staleness if rustc's cache API evolves.** Probability: ~15% over 5 years. Handoff Decision 14 originally prescribed forcing `cache_on_disk_if(false)` on every Sky-overridden query via Provider slots. Audit at 2026-06-24 found the prescribed API doesn't exist on current nightly — `cache_on_disk_if` is a query-DECLARATION-time modifier in rustc's macro DSL, not a Provider slot. The audit (see §22.4.1) verified every Sky-overridden query is cache-safe by construction: `per_instance_mir`, `layout_of`, `cross_crate_inlinable`, and `collect_and_partition_mono_items` are all declared non-disk-cached or `eval_always`. (`symbol_name` was previously on this list; Phase F retired the override 2026-06-24 — rustc's default v0 mangler is Instance-keyed and correctly invalidates via Instance variation, so no override means no staleness concern at that layer.) **Risk: if rustc evolves the cache API such that one of these queries gains disk-caching, OR Sky adds a new override on a disk-cached query without considering universe-state-changes-invalidation, Sky's incremental builds could return stale results silently.** Canary: `toylangc/tests/cache_audit.rs` requires every override file to carry a `cache-audit:` marker comment. Adding a new override forces a marker, forces a fresh audit. Reaction: identify the new staleness risk; if rustc exposes a Provider-slot override API in the future, install it. Otherwise document as a known limitation and disable incremental compile globally when Sky's machinery is active.

**B27 (Sky-specific). Bench-detected creeping perf regression between nightly bumps.** Probability: ~30% over 5 years. Sky pins a specific rustc-fork nightly; each bump (§3.5) inherits whatever LLVM/rustc upstream evolution happened in the interval. The perf model in §22.4 anchors on empirical bench numbers from a specific date (2026-06-24); the numbers drift as the toolchain bumps. Canary: re-run `bash toylangc/tests/scripts/run_perf_bench.sh` after every nightly bump. If Bench 2 K=100 LTO ratio regresses >10% (currently 1.48×; >1.63× would fail), or Bench 3 drop-chain ratio regresses >20% (currently 25.5× for Sky, 27.5× for the pure-Rust cross-crate baseline; <20.4× for either would fail), investigate. The Sky/Rust delta at thin LTO (currently 3% on Bench 3, 0.3% on Bench 1) is also worth tracking — if it grows >10%, Sky's emission has drifted from Rust's structurally. Reaction: bisect upstream nightly range with bench script as the regression detector; report upstream or pin to last-known-good. The `tmp/perf-bench-disasm/` archive (per-fixture main()-symbol disassembly) helps localize whether the regression is in inlining behavior vs scheduling vs register allocation.

### 25.3 Category C: operational invariants

**C1. Don't use def_path_str outside diagnostics.** Sky's analog of `@DPSFDOZ`. `tcx.def_path_str()` ICEs outside diagnostic contexts. Sky's `is_from_sky_stubs` and all path-based matching uses `tcx.def_path(...)` walks or `tcx.crate_name` checks, never `def_path_str`. Canary: panic messages mentioning `trimmed_def_paths`.

**C2. Don't introduce new locking sites during generate_and_compile.** Sky's analog of `@GCMLZ`. Sky's `MUTABLE_STATE` (if any — Sky may not need a heavy mutex due to its in-process design) is held during codegen; query providers must not lock it. Canary: tests hang with 0% CPU.

**C3. Preserve the codegen plugin's CGU filter invariant.** New query providers must understand that Sky items have been filtered out of the CGU list. New consumers of CGU contents must respect the filter. Canary: tests fail with "consumer item missing from CGU list."

**C4. Sky's comptime evaluator must be deterministic.** Section 13.5. Canary: byte-comparison CI.

**C5. Sidecar must be deterministic.** Section 7.4. Canary: byte-comparison CI.

**C6 (new — operational). Cargo profile overrides only live at workspace root.** Risk that Sky's `skyc` orchestrator emits profile blocks (LTO, opt-level, codegen-units, panic strategy) in per-package `Cargo.toml`s instead of the workspace root. Cargo silently ignores member-level profile blocks; the rustc command simply doesn't carry the override. Canary: any test that depends on a profile setting having actually been applied (e.g. cross-language inlining tests). Reaction: emit profile overrides only at the generated workspace's root `Cargo.toml`. Toylang's `build.rs::write_workspace_toml` is the reference shape. Documented at appendix F.10.

**C7 (new — operational). `RUSTC_WORKSPACE_WRAPPER` necessity for hook installation.** Sky's facade-side `extra_modules_hook` install happens during `LangDriver::config`, which only runs when Sky's binary is invoked as the rustc workspace wrapper. Direct `cargo build` invocations that bypass the wrapper get vanilla rustc-fork without Sky's hook installed; the build "succeeds" but produces a binary missing Sky's bodies (linked from the stub rlib's `unreachable!()` bodies). Canary: a test that runs the binary's output, not just that the build returns 0. Reaction: any integration test of patch (c) behavior must invoke through Sky's wrapper, not direct cargo. Operational discipline; documented in toylang's `tl-handoff.md` traps list and appendix F.11.

**C8 (new — operational). Stale incremental cache surfaces as mysterious test failures.** Rustc's incremental cache + Sky's universe pre-population at after_expansion can produce cache-shape mismatches when Sky's schema evolves. Toylang regularly hit this when bumping facade types. Canary: tests failing in seemingly random ways across runs of the same code. Reaction: wipe the integration-projects-cache directory and re-run. Operational; build a `skyc clean` command early.

### 25.3.5 The byte-identical pass-through invariant as a continuous discipline

Section 4.4 introduced the byte-identical pass-through invariant: Sky's `rustc` binary, when compiling a crate without the Sky marker, produces byte-identical output to vanilla nightly rustc for the same inputs. This invariant is the architecture's promise that Sky doesn't pollute pure-Rust ecosystem compiles.

Maintaining the invariant is a continuous discipline, not a one-time check. Three threat patterns:

**Threat 1: Side effects during Sky's startup before the marker check.** Any environment-variable read, file-system touch, process state change, or panic-hook installation that happens before Sky's marker check produces divergence. The discipline: Sky's `rustc` entry point reads only argv, performs only the minimal Callbacks::config setup that vanilla rustc does, and gates every Sky-specific behavior on the marker check.

**Threat 2: Sky's panic handler interfering with vanilla diagnostics.** Sky installs a custom panic handler for Sky-marked compiles. If the handler is installed unconditionally, it changes vanilla rustc's panic output (which uses the default). Discipline: install only after marker detection confirms Sky machinery should be active.

**Threat 3: Sky's `init()` or `provide()` methods leaking state into pure-Rust compiles.** Sky's CodegenBackend's methods run on every compile (Sky's binary is always the active backend). The methods must short-circuit when the marker is absent, leaving rustc in a state identical to what `LlvmCodegenBackend` would produce.

**The CI check.** Sky's CI maintains a corpus of representative Rust crates (small hello-world, medium serde-derive consumer, large tokio program, generic-heavy code, trait-heavy code, sys-crate wrapper). For each crate:

1. Build with vanilla nightly rustc. Hash all output objects.
2. Build with Sky's rustc binary (with marker absence). Hash all output objects.
3. Byte-compare the hashes.

Mismatch is a regression that blocks the toolchain release. The corpus is expanded as new threat patterns are identified.

**This is the hardest invariant in the document.** Maintaining it requires that every change to Sky's startup, callback installation, and backend `init`/`provide` methods is reviewed against the pass-through requirement. Section 26 documents this as a cross-cutting invariant; new contributors learn to think about it before touching Sky's `init` paths.

### 25.4 Mitigating factors

**Co-travelers.** Sky is not alone in the "deep rustc integration via nightly extension points" neighborhood. Erw (in maintenance), rust-analyzer, miri, clippy, cranelift codegen backend, rust-gpu. If rustc's API shifts threaten any of these, Sky has early warning. Monitor their issue trackers.

**`rustc_public` trajectory.** The stable-MIR effort covers ~40-50% of Sky's read-side rustc surface. Stabilization would reduce Sky's drift surface meaningfully. The load-bearing pieces (query providers, MIR construction, CodegenBackend, partitioner) have no stable equivalent on the roadmap, but partial migration is possible.

**Nightly-pin strategy.** Sky pins to a specific nightly. Bumping is conscious, not silent drift. Recommended strategy: bump every ~6 months to a ~3-month-old nightly; dedicated bump sessions, not interleaved with features; full test suite cold and warm after each bump; documentation updates with empirical bump-cost data.

### 25.5 Cross-references

- Section 3 — fork patches and their drift surface.
- Section 5 — codegen backend's design that eliminates erw's B2-style risk.
- Section 20 — pipeline that pre-empts B6-style cache-skip issues.
- Section 26 — operational invariants documented as cross-cutting invariants.

---

## 26. Cross-Cutting Invariants (Arcana-style)

This chapter documents the cross-cutting invariants Sky's implementation must respect — the analogs to erw's @-arcana. Each invariant has an ID, a brief description, and pointers to where it's load-bearing in the code.

**Quick reference (the rules at a glance):**

| ID | Rule (one sentence) |
|---|---|
| SyMINCZ | Symbol-name lookups are pure reads; drive codegen via `ReifyFnPointer` casts in `per_instance_mir` bodies. (Override retired 2026-06-24 Phase F; rustc's default mangler preserves the invariant.) |
| GCMLZ | Don't lock a consumer-state mutex from inside a rustc query provider. |
| DPSFDOZ | `tcx.def_path_str` ICEs outside diagnostics; use `def_path(...)` or `crate_name`. |
| ELASZ | Populate lifetime slots of `GenericArgs` with `re_erased`, never `'static`. |
| ACRTFDZ | LLVM extern declarations use rustc's ABI-coerced types, not Sky's representation. |
| TCHAPZ | Append a hidden `Location` arg at call sites for `#[track_caller]` Rust fns. |
| Migratory propagation | Migratory async cannot `.await` non-migratory. |
| Sky source = no Pin | Sky source never writes Pin / `for<'a>` / Rust lifetime syntax. |
| RTMEIZ | Every Rust type Sky source uses must be explicitly `import`ed. |
| UTAIRZ | Unsized types appear only as the inner of a reference. |
| MBMRVZ | `fn main()`'s tail expression is void; otherwise SIGBUS on the sret. |
| IVTDBTZ | Inherent vs trait dispatch is type-kind based, not argument-count based. |
| TVIMDGAZ | For trait methods, build `Instance` from the trait def's method DefId + `[Self, ...]`. |
| ATAFLBZ | Walks of `tcx.all_impls(...)` filter by `is_from_sky_stubs(self_type_did)`. |
| ETASTZ | `build_generic_args_for_item` silently truncates excess Type args. |
| NNGZ | Non-generic is the degenerate case of generic; don't branch on `type_params.is_empty()`. |
| SMPLZ | Sky-emitted rustc-visible symbols must be pinned in `@llvm.used` (not `@llvm.compiler.used`) so LTO `internalize` doesn't demote linkage. |
| SBMNBIZ | **VACUOUS 2026-06-22** — partition filter restored (Option 4 + patch 5 jointly retired) so no `AvailableExternally` `unreachable!()` body reaches LLVM. The invariant was load-bearing only during the brief Option 4 window. Preserved in §26.17 as design history. |

Each invariant is expanded in detail below.

### 26.1 SyMINCZ (Sky's Mangling Is Not Codegen)

Reading a symbol name for a Sky Instance — whether via Sky's own helpers or via `tcx.symbol_name(instance)` — is a **pure read**; computing the name does not drive codegen. To drive codegen of a generic Rust dep, Sky must emit a `Rvalue::Cast(CastKind::PointerCoercion(ReifyFnPointer(...)))` in the synthetic MIR body. The two surfaces are independent: symbol-name reads at call sites tell the linker which symbol to dispatch to; ReifyFnPointer casts in the per_instance_mir body tell rustc's mono collector which Instances to codegen.

**Where:** Sky's codegen call sites that need a symbol name for an Instance read `tcx.symbol_name(instance)` directly (post-Phase-F, 2026-06-24 — the `symbol_name` override was retired because rustc's default v0 mangler already produces the right name under the single-symbol architecture, see §6.2). Sky's per_instance_mir body construction uses the ReifyFnPointer path to drive monomorphization. The two are separate; conflating them silently misses dep registration.

**Load-bearing because:** if a Sky engineer adds a new Rust call site by only computing the symbol name (no ReifyFnPointer cast in the synthetic body), the link will fail with undefined symbol. The arcana documents that the symbol-name side and the codegen-driving side are separate code paths that both must be touched.

**Pre-Phase-F history:** Sky carried a `symbol_name` query override that did shape classification (`is_consumer_fn` / `is_consumer_trait_impl_method` / `is_consumer_accessor_safe`) and asked the consumer for a symbol via a `consumer_symbol_for_callback_name` callback — the consumer impl ignored the callback name and returned rustc's default mangler output. The classification work was entirely unused at the symbol_name layer. Phase F (handoff Decision 2) retired the override, the callback, and the saved `DEFAULT_SYMBOL_NAME` provider pointer; emission now reads `tcx.symbol_name(instance)` directly. The SyMINCZ invariant survives unchanged in spirit — what changed is that rustc's default mangler is now what every read consults.

### 26.2 GCMLZ (Generate Compile Mutex Lock)

**Rule:** if Sky uses a global mutex for any mutable consumer state, the mutex must not be locked from query-provider code paths during codegen.

**Mechanism:** Sky's architecture structurally avoids the failure mode by (a) keeping predicates (`is_consumer_type`, `is_consumer_fn`) as lock-free reads of the `SkyUniverse` (RwLock, populated during the populate-only phases — sidecar load via `on_sky_lib_loaded` and local-registry build via `after_rust_analysis`, both before codegen starts), (b) making any in-query callbacks stateless functions of `(tcx, instance)` (or — post-Phase-F 2026-06-24 — retired entirely; the `symbol_name` override and its `consumer_symbol_for_callback_name` callback are both gone, with rustc's default mangler used directly), and (c) using patch 4's `fill_extra_modules` hook for codegen contribution instead of a long-running stateful callback holding the consumer-state lock.

**Discipline:** any new query provider added must read from `SkyUniverse`, NOT from a `Mutex`-protected state. Any new stateful callback that takes `&mut consumer_state` must justify why it cannot fire from inside a rustc query.

**Failure mode:** silent deadlock — 0% CPU, no panic, no diagnostic output. The single diagnostic move is `sample <pid>` on the hung process to capture the stack; the re-entrant `lock()` call will be visible at the top of the stack.

**Where it bites:** any future change that adds `&mut state` to a query-provider callback signature. Type-system can't prevent it; reviewers must catch it.

**Surviving lock sites today:** `create_state` (once at init), `after_rust_analysis` (once between typecheck and codegen), `on_sky_lib_loaded` (once per upstream sidecar), `collect_generic_rust_deps` (concurrent — fires from rustc's rayon workers; the only path where the mutex is genuinely contended), and `consumer_fill_modules` (the main-thread `fill_extra_modules` callback that emits Sky's CGU into the rustc-supplied module). The mutex is retained for plain serialisation of the concurrent path, not for deadlock avoidance.

### 26.3 DPSFDOZ (DefPathStr Is For Diagnostics Only)

`tcx.def_path_str(def_id)` ICEs outside diagnostic contexts. Sky's path-based matching uses `tcx.def_path(def_id).data` walks or `tcx.crate_name(def_id.krate)` checks. Never `def_path_str`.

**Where:** `is_from_sky_stubs(tcx, def_id)` (Section 6.5) uses the marker-detection mechanism, which walks `module_children` rather than computing path strings. Sky's other path-based queries follow the same convention.

**Load-bearing because:** ICE messages from def_path_str are confusing (they blame compiler_builtins and trimmed-paths code). A Sky engineer hitting this would not immediately know that the issue is in their own code's choice of API.

### 26.4 ELASZ (Early-bound Lifetime Args Synthesized)

When Sky builds a GenericArgs for any Rust item, lifetime slots are populated as `tcx.lifetimes.re_erased`. Sky source supplies type args; lifetime slots are filled by Sky's helper based on the item's `generics_of` declaration.

**Where:** Sky's analog of `oracle::build_generic_args_for_item` (probably in Sky's frontend codegen-prep code) uses `ty::GenericArgs::for_item(tcx, def_id, |param, _| ...)` to walk the item's generic parameters and fill each appropriately.

**Load-bearing because:** if lifetime slots aren't filled (or are filled with `'static` instead of `re_erased`), trait dispatch can pick wrong impls (`Deserialize<'static>` vs `Deserialize<'de>` for any `'de`).

### 26.5 ACRTFDZ (ABI Coerced Return Type In Function Declarations)

When Sky declares an LLVM function that will be called as a Rust function, the return type must match rustc's ABI coercion, not Sky's representation. For an 8-byte struct, rustc may return `i64` (Direct scalar in register), but Sky's representation might be `[8 x i8]` (LLVM aggregate). The declared LLVM function must use the ABI-coerced type; the return value is reinterpreted via memory after the call.

**Where:** Sky's analog of erw's abi_helpers.rs (probably in Sky's codegen). All Rust call-site emission paths use the ABI-coerced type for declarations, with a memory-reinterpretation step for the conversion to Sky's representation.

**Load-bearing because:** ABI mismatch produces silent corruption — LLVM reads the return value from the wrong location, gets garbage. Symptoms are downstream segfaults, not link errors.

### 26.6 TCHAPZ (Track Caller Hidden ABI Parameter)

Many Rust standard library methods are annotated `#[track_caller]`. rustc's ABI computation appends a hidden `&'static Location` pointer parameter to these functions' signatures. Sky's call sites must pass a value for the hidden parameter (typically null, since Sky has no meaningful source locations to report).

**Where:** Sky's call-site emission for Rust methods checks `instance.def.requires_caller_location(tcx)` to detect the track-caller attribute; appends a null pointer arg if so.

**Load-bearing because:** without the hidden arg, the called function reads garbage from the slot where the Location pointer should be. For methods that internally pass the Location to other track-caller functions, the garbage propagates to allocation or panic paths, causing crashes.

### 26.7 Migratory and cancellable propagation rules (Sky-specific)

Sky's typechecker propagates the migratory and cancellable properties through call graphs:

- Migratory functions cannot `.await` non-migratory functions.
- Cancellable wrappers are explicit, not propagated automatically.
- Linear types panic on drop, regardless of context.

**Where:** Sky's typechecker, async-fn analysis, and stub generation.

**Load-bearing because:** propagation rules are what make the migratory split meaningful. Without enforcement, a migratory function could accidentally hold a non-migratory state machine, leading to send across threads while the inner state holds a non-Send group reference.

### 26.8 Sky source = no Pin, no for<'a>, no rust lifetime syntax

Sky source never writes Pin, never writes for-quantified lifetimes (`for<'a>`), never writes Rust lifetime annotations directly. Sky's group system covers what those concepts handle in Rust; Sky's frontend translates between Sky source and Rust signatures at the boundary.

**Where:** Sky's parser, type system, and stub generator.

**Load-bearing because:** if Sky source can write Rust-specific lifetime syntax, two consequences: (1) source-language users have to learn two lifetime systems (Sky's and Rust's), (2) the typechecker's responsibilities are doubled (validate Sky source against Sky's system AND validate against Rust's system at boundaries). Keeping these separate makes Sky's source language clean.

### 26.9 RTMEIZ analog (Rust Types Must Be Explicitly Imported)

Sky inherits erw's `@RTMEIZ` rule: every Rust type that Sky source uses — even transitively, even via types not named directly in source — must be explicitly imported. The mechanism:

- Sky's source has `import rust.std.vec.Vec`, `import rust.std.io.Stdout`, etc., declaring each Rust type explicitly.
- Sky's stub generator emits one `pub use std::vec::Vec` per import into the stub rlib.
- Sky's frontend's name-resolution looks up Rust types only in the stub rlib's `pub use` re-exports.
- Missing imports produce structured errors at typecheck time: "Rust type `Stdout` is not imported; add `import rust.std.io.Stdout` to your source."

**Where:** Sky's name resolver, stub_gen, frontend's RustTypeNotImported error path.

**Load-bearing because:** sky source can implicitly mention a Rust type through trait dispatch (`Write::write_all(&out, ...)` mentions `Stdout` as Self even though source doesn't name it) or through return type binding (`vec.pop()` returns `Option<T>` even though source doesn't name `Option`). Without the explicit-import discipline, Sky would either need to silently auto-discover types (rejected — produces non-determinism, ordering issues), or produce undecidable name resolution. Explicit imports keep the model clean.

The Sky-author user-facing error message points at the missing import with a suggested `import` line.

### 26.10 UTAIRZ analog (Unsized Types Appear Inside Ref)

Sky inherits erw's `@UTAIRZ` rule: Sky's unsized types (`str`, `[u8]`, slice-style `[T]`) appear only as the inner of a reference. Bare unsized types have no Sky representation; they're caught at the parser or type resolver.

**Where:** Sky's parser, type resolver, LLVM codegen for fat-pointer types.

**Load-bearing because:** unsized types have no size, no LLVM register class, no concrete memory representation. The wrapping `&G T` reference is what gives them a concrete representation (a ScalarPair fat pointer: ptr + length). The pattern requires synchronous wiring at every stage of the compiler — parser, type-resolver, oracle (Sky→rustc Ty conversion), stub generator (Sky→Rust source), codegen (LLVM IR emission). Adding a new unsized type (e.g., a Sky `CStr`) requires touching all six sites; missing one produces silent corruption.

### 26.11 MBMRVZ analog (Main Body Must Return Void)

Sky's `fn main()` body must have a void-typed tail expression. Sky's frontend enforces this at typecheck time, producing a clear error if the tail returns non-void.

**Where:** Sky's typechecker, specifically the after-resolve check for `fn main()`.

**Load-bearing because:** the auto-generated bin shim (Section 18.2 — `src/main.rs` with `fn main() { __sky_main(); }`) calls `__sky_main` expecting it to return `()`. If Sky's `fn main()` body has a non-void tail, the underlying Sky function would silently grow an sret parameter (Sky's ABI promotes structs to sret), and the shim's no-sret call would leave the sret register pointing into wherever it was previously — typically the binary's text segment. The internal body runs, completes its side effects, then SIGBUS or SIGSEGV on the final `str` to the sret buffer (writing to a read-only page).

This was a real toylang bug surfaced empirically (`docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md`). Sky inherits the discipline and the typecheck-time enforcement. The fix in source is always to terminate the last statement with `;` so main's tail is implicit unit, or use an explicit void-typed tail expression. Sky's typechecker rejects non-void main tails at typecheck time and emits a Sky-source-level error with a fix suggestion.

### 26.12 IVTDBTZ analog (Inherent vs Trait Dispatch By Type)

Sky's dispatch between inherent static calls (`MyType::method(args)`) and trait static calls (`MyTrait::method(receiver, args)`) is type-kind based, not argument-count based. A name is a trait iff `find_use_imported_trait_def_id(tcx, name).is_some()`. Nothing else (arg count, receiver presence, whether the name is in Sky's registry) influences the classification.

**Where:** Sky's type resolver's `StaticCall` dispatch path.

**Load-bearing because:** the wrong classification produces either an ICE (trait-path lookup fails) or a silently-wrong call (inherent-path takes args that don't match an inherent method's signature). The classification must be deterministic; the predicate is the oracle's "is this name a trait?" check.

### 26.13 TVIMDGAZ analog (Trait vs Impl Method DefId)

When Sky calls a Rust trait method, the rustc Instance is built from the **trait definition's** method DefId with `[Self, ...args]` as generic args — NOT the impl block's method DefId.

**Where:** Sky's codegen call-site emission and Sky's dep-registration walker.

**Load-bearing because:** the impl's method DefId has different generic params than the trait def's method DefId (the impl substitutes Self with the concrete type, so its generic params don't include Self). Building Instance with the wrong DefId or wrong args causes rustc to panic with "type parameter X out of range when instantiating." `Instance::expect_resolve` handles the mapping from trait-level args to impl-level args automatically — but only when given the trait def's method DefId.

### 26.13.5 ATAFLBZ (All-impls Walks Need Lang-Stubs Filter)

When Sky-side helpers walk `tcx.all_impls(trait_def_id)` to find a consumer-type impl of a Rust trait, the walk returns impls from *every* loaded crate including std. The self-type-name check (`tcx.item_name(adt_def.did()) == "Box"`) is **ambiguous** because std and Sky can both define a type named `Box` — std has `alloc::boxed::Box<T>` and `std::ffi::os_str::Box<OsStr>`, Sky has `case6_lib::Box`. The walk matches whichever appears first in `tcx.all_impls`'s iteration order.

**Where:** every Sky oracle helper that maps `(struct_name, trait_name, method_name) → DefId` via `all_impls` traversal.

**The discipline:** add `is_from_sky_stubs(tcx, adt_def.did())` (per §6.5) as a filter inside the impl walk. Only impls whose self type's ADT lives in a marker-bearing crate qualify as consumer impls. Toylang's `find_trait_impl_method_def_id` (`oracle.rs:693`) is the reference shape.

**Load-bearing because:** under the single-symbol architecture (§6.2), the wrong DefId produces the wrong rustc-mangled name when Sky's bitcode emits a body. Pre-single-symbol the synthesized `__sky_impl_*` name didn't care which DefId was matched; under single-symbol the rustc-default mangling derived from the std DefId points at a symbol Sky never defines. Toylang surfaced this bug in case6 (cross-Sky-crate `Box`) during the Path B implementation; the fix was a one-line filter addition.

**Bonus discipline:** the same filter applies to `tcx.inherent_impls(struct_def_id)` walks. Toylang's `find_inherent_method` doesn't need it today because the struct DefId itself is already in a marker-bearing crate (the caller looked it up via `find_local_struct_def_id`), but downstream uses that might walk inherent impls of `tcx.all_impls(SomeRustTrait)` would.

### 26.14 ETASTZ analog (Extra Type Args Silently Truncated)

`build_generic_args_for_item` silently discards user-supplied type args that exceed the item's `Type` slot count. Load-bearing for Sky's call-site convention where users name the type's generics (`Vec::new<I32, Global>()`) rather than the method's narrower generics.

**Where:** Sky's analog of `oracle::build_generic_args_for_item`.

**Latent risk:** if Sky ever gains a syntax for naming a non-default parent-type arg (a custom allocator for `Vec`, a non-default hasher for `HashMap`), silent truncation becomes a real bug — the user's explicit non-default would silently become the default. Documented as tech debt in Sky's known-debt list; fix is to validate truncation at the helper site (compare against the parent's default; error if non-matching).

### 26.15 NNGZ (Non-generic is the Normal-case-of-Generic)

Sky's source-level positive design principle (§1.5.5) elevated to arcanum form for in-code invocation: **non-generic is the degenerate case of generic. Never branch on "does this item have type parameters?"**

**Where:** every architectural surface that handles items with type parameters. Stub-gen emission, discovery channels, populate loops, substitution helpers, symbol mangling, layout queries.

**Mechanism:** write the general N≥1 path; let N=0 fall out as one iteration over an empty list / one entry with `concrete_args = []` / one identity substitution map. Don't gate on `type_params.is_empty()`.

**Forced exceptions** (each must be `arch-fence-allow`-annotated):
1. Rust syntax constraints — `impl<>` / `Foo<>` / `Self<>` are parse errors. Stub-gen emits no `<>` decoration for N=0.
2. External rustc behavior with no consumer override — when a query's contract differs for N=0 vs N≥1 without a sanctioned customization point.
3. Approach A invariants — `debug_assert!(!instance.args.has_param())` is "substituted vs unsubstituted," not "N=0 vs N≥1." Keep.

**Failure mode:** every gated `type_params.is_empty()` branch creates a tripwire that fires when a new generic-shape surface appears (impl blocks with type params, multi-param impls, method-level type params on impl methods, generic accessors). Toylang's session 17–18 audit cycle exhibited four such breakages; the audit response retired three more. Empirical: branches on type-param emptiness age badly.

**Detection:** a grep-based architecture-fence CI test (toylang's `tests/architecture_fence.rs`) walks Sky's frontend source for `type_params.is_empty()` patterns and asserts each is annotated `arch-fence-allow: <reason>` (where the reason names one of the three forced exceptions above). Unannotated occurrences fail the test.

**Cleanup:** when retiring a previously-fenced branch (because the consumer's mechanism now handles N=0 uniformly), remove the `arch-fence-allow` marker along with the branch.

### 26.16 SMPLZ (Sky Must Pin Linkage for External Refs)

Any Sky-emitted symbol whose only callers live in OTHER compile units' machine code must be pinned in the `@llvm.used` LLVM global (not the weaker `@llvm.compiler.used`). Three LLVM passes would otherwise remove or rewrite it: `GlobalDCE` deletes, LTO `internalize` demotes linkage from External to Internal, and the linker dead-strips. The weaker `@llvm.compiler.used` blocks only the first; under `lto = "fat"` the demotion silently breaks cross-crate references.

**Where:** every emission of a rustc-visible symbol in `toylangc/src/llvm_gen.rs`. Today the rustc-mangled extern wrapper symbols are emitted by `codegen_extern_wrapper` and the internal-symbol path emits its companions; `fill_module` then collects both extern and internal symbol names from `fn_items` and calls `pin_in_llvm_used` once at the end of the emission loop. Future Sky emission paths (drop glue, vtable shims, async state machine `poll` impls, closure `Fn` impls) must either route their symbols through `fn_items` so `fill_module`'s end-of-loop pin picks them up, or call `pin_in_llvm_used` themselves.

**Failure mode:** the resulting linker error is byte-identical to B14's share-generics-gate disambig mismatch — same surface symptom, completely different root cause. Disambiguating check: `llvm-objdump -t` on the post-LTO `.o`. `g F` means the chain is intact (B14-territory); `l F` means SMPLZ discipline broke (the pin is wrong or missing).

**Detection:** `tests/integration_projects/opt_level_3_fat_lto_smoke/`. Built at `opt-level = "3" + lto = "fat"`; reverting `pin_in_llvm_used` to `@llvm.compiler.used` reproduces the bug.

**Full arcana doc:** `docs/arcana/SkyMustPinLinkageForExternalRefs-SMPLZ.md`. Includes the investigation playbook for "Sky symbol disappeared somewhere in the LLVM pipeline" debugging (see §25.2 B15 too — the playbook is duplicated there inline).

### 26.17 SBMNBIZ (Stub Body Must Not Be Inlined) — VACUOUS 2026-06-22

**Status: vacuous as of 2026-06-22.** With the partition filter restored
(§C.2) and Option 4 + patch 5 retired together, rustc never emits an
`AvailableExternally` `unreachable!()` body for consumer items to
LLVM IR. The partition filter strips consumer items from rustc's CGU
list before codegen, so no body for inlining to grab. The invariant
that this arcanum codified — "ensure Sky's External body is in the
same IR pool to win the IR linker tie-break" — has nothing to
enforce anymore. Per_instance_mir's synthetic Unreachable-terminated
body still exists but is consumed only by rustc's mono collector
(for cascade discovery) and not by codegen. The text below is
preserved as design history for the brief Option 4 window (2026-06-21
to 2026-06-22) when the invariant was load-bearing.

---

`stub_gen` emits `pub fn foo() -> T { unreachable!() }` for every consumer export; the Option 4 `codegen_fn_attrs` override marks each such item with `AvailableExternally` linkage. Per_instance_mir's synthetic body (ReifyFnPointer casts + `Unreachable` terminator) lowers to the same shape. If LLVM ever inlines the unreachable body into a real caller, the result is undefined behavior: the caller's continuation becomes unreachable, the optimizer removes legitimate downstream code, and the program has UB if ever invoked. The failure is **silent** — no link error, no test crash from the build itself.

**Discipline:** at every compile session where rustc emits an `AvailableExternally` stub body for a consumer item, EITHER (a) Sky's `fill_extra_modules` also emits the real body with `External` linkage in the same session (the IR linker picks External over AvailableExternally), OR (b) no callers of the symbol exist in that session's IR (so no inlining can occur). LLVM's IR linker rule (External wins same-symbol conflicts) is what makes (a) safe. @F.13's `is_reachable_non_generic` collector gate at user_bin compile is what gives us (b) for non-generic upstream items under §5.5 Step 2.

**Where:** three coordinated sites maintain the invariant. `rustc-lang-facade/src/queries/codegen_fn_attrs.rs::lang_{,extern_}codegen_fn_attrs` (i.e. `lang_codegen_fn_attrs` and `lang_extern_codegen_fn_attrs`) produces the AvailableExternally stub bodies. `rustc-lang-facade/src/queries/per_instance.rs::build_dependency_body` produces the synthetic Unreachable-terminated body. `toylangc/src/llvm_gen.rs::fill_module` (gated by `toylangc/src/toylang/callbacks_impl.rs::consumer_fill_modules`) emits the real-body shadow at every compile session where a caller could exist.

**Failure mode:** silent UB. Standard CI doesn't catch this from build status — only fixtures whose runtime behavior would detect inlined unreachable (e.g., the inlining matrix's stdout-checked fixtures, or any binary that runs to completion and produces an unexpected result).

**Detection:** the inlining matrix at `toylangc/tests/integration_projects/inlining/` fences every bin's `main` containing `unsafe { __toylang_main(); }` against UB — if the stub body inlined into main, the binary would either trap (`udf`/`brk`) or constant-fold to nothing rather than producing the expected output. The 8 `_no_lto` matrix fixtures specifically lock in the §5.5 Step 2 behavior of "tail-jump present" (`b __lang_stubs::__toylang_main`); a successful UB-inducing inlining would replace the tail-jump with the unreachable body's expansion, which wouldn't match the expected disassembly shape.

**Full arcana doc:** `docs/arcana/StubBodyMustNotBeInlined-SBMNBIZ.md`. Includes the safety-condition proofs for each of the compile-session shapes Sky uses (pre-Step-2 and post-Step-2).

### 26.18 Cross-references

- Section 1.5.5 — the positive form of NNGZ (Sky's design principle).
- Section 5 — codegen path that respects ABI invariants.
- Section 11 — group system and HRTB handling.
- Section 14, 15 — async migratory/cancellable mechanism.
- Section 19 — per_instance_mir body construction that uses the codegen-driving cast.

---

## 27. Compatibility Promises

This chapter records what Sky promises about compatibility — across Sky versions, across Sky-source revisions, across toolchain updates.

### 27.1 Sky source compatibility across Sky versions (TBD)

Sky's source compatibility is unfinalized. The intent: pre-1.0 makes no promises (source may break across skyc versions); 1.0 and later guarantee source-level backward compatibility within a major version (skyc 1.x always accepts source that compiled on skyc 1.0, possibly with deprecation warnings).

The mechanism for 1.x backward compatibility: every Sky source feature has an `edition` (similar to Rust's editions). Source files declare their edition; skyc reads the edition and applies that edition's rules. Future major versions (Sky 2.x) may break source-level compatibility with a new edition.

This subsection is recommended-not-locked because the editions mechanism hasn't been formally designed yet. The principle is locked; the details are open.

### 27.2 Sidecar format versioning

The sidecar carries a `format_version` (Section 7.2). Sky's compatibility posture:

- **Pre-1.0:** format_version match required. Skyc refuses to load sidecars with mismatched format_version.
- **1.0 onward:** skyc reads sidecars in a range of format_versions, applying migrations as needed. Format changes that break older readers require major-version bumps.

The migration machinery is non-trivial work and is deferred to 1.0. Pre-1.0, the strict matching policy is acceptable because Sky's user base is small and toolchain consistency is enforceable.

### 27.3 Cross-Sky-version binaries forbidden (all crates same toolchain)

A Sky binary cannot link object code produced by different Sky compiler versions. All crates in a binary's dependency graph must compile with the same Sky toolchain.

Enforcement: cargo's `rust-toolchain.toml` pinning. The `.skybuild/rust-toolchain.toml` is written by skyc and pins a specific Sky-nightly version. All crate compiles use that pin. Linking different-version-compiled objects is impossible by construction.

Why enforce: Sky's codegen evolves across versions. Layouts may change, ABI emission may change, comptime semantics may change. Cross-version binaries would have inconsistent behavior. The pinning prevents the issue.

### 27.4 Sky's stdlib ABI evolution

Sky's standard library evolves with Sky's compiler. Each Sky compiler version ships its own stdlib (the toolchain bundles both). Stdlib breaking changes are coordinated with compiler version bumps.

For Sky 1.x: stdlib backward compatibility within the major version, same rules as Sky source compatibility (Section 27.1). Deprecations and warnings, no source-breaking changes.

For Sky 2.x: opportunity to evolve stdlib aggressively if needed. Source migrations would be tooling-supported (skyc migrate command).

### 27.5 Cross-references

- Section 7 — sidecar format and versioning.
- Section 25 — risks around bump compatibility.
- Section 28 — phasing of compatibility-related work.

---

## 28. Implementation Phasing

This chapter describes the order in which Sky's implementation should be built. Phasing matters because some subsystems depend on others; building them out of order produces stubs that can't be tested or wastes effort.

**Implementer quick-start map.** Before reading §28.1, the implementer's most-load-bearing prior sections, by phase:

| Phase | Must-read sections |
|---|---|
| Phase 1 (Fork + plugin) | §3 (fork patches), §4 (distribution, marker-based activation), §5 (codegen backend), §25.3.5 (pass-through invariant), §26 quick-reference table |
| Phase 2 (Sky frontend MVP) | §1.7 (non-goals), §5.6.6 (path syntax), §6 (stub rlib model), §7 (sidecar format), §8 (Temputs), §9 (export semantics) |
| Phase 3 (Generics) | §2.6 (case 4, with empirical correction), §8.9.5 (discovered trait-impl instances), §10 (type representation), §19 (per_instance_mir mechanism), §F.13 + §F.14 (cascade timing) |
| Phase 4 (Comptime) | §13 (full chapter), §10.6 (SkyOpaqueType wrapper) |
| Phase 5 (Groups + linear types) | §11 (groups), §15 (drop / cancellation), §26.4 ELASZ |
| Phase 6 (Async) | §14 (closures + async two-type split), §17 (tokio interop) |
| Throughout | §26 (cross-cutting invariants), Appendix F (toylang lessons) |

### 28.1 What v1 ships

**v1 scope (recommended phasing for the initial implementation):**

**Phase 1: Fork + minimal codegen plugin (4-8 weeks).**
- Apply the four fork patches: the three `per_instance_mir` patches + the `fill_extra_modules` allocator-callback hook on `ExtraBackendMethods` (see §3.2). (Historical patch 5 — the `consumer_lang_active` gated escape — was retired 2026-06-22; the partition filter restoration replaces its function.)
- Build the Sky codegen backend as a CodegenBackend impl that wraps LlvmCodegenBackend.
- Implement the marker-based per-crate activation.
- Install the `fill_extra_modules` hook (via `set_fill_extra_modules_hook` / `install_consumer_modules_hook`) as a no-op (no Sky-side bitcode contribution yet).
- Skip Sky's frontend for now: Sky's per_instance_mir provider always returns None (effectively, Sky's machinery activates but does nothing).
- Verify the byte-identical pass-through invariant for pure-Rust crates (Section 4.4). Set up the CI corpus from day one — pass-through is the single hardest invariant to maintain (§25.3.5).
- **Set up the architecture fence CI test from day one** (§26.15 NNGZ enforcement). The grep-based check that flags unannotated `type_params.is_empty()` branches is cheap to write and catches drift before any generic-shape surface exists; retrofitting the discipline after several months of "we'll get to it" produces dozens of fence-allow markers that all need re-evaluation. Toylang's session 11 wrote it after several violations had accumulated; Sky should write it first.

**Phase 2: Sky frontend MVP (8-12 weeks).**
- Parser for `.sky` source files.
- Basic name resolution.
- Simple typechecker (no generics, no comptime, no groups for v1.0; v1.1 adds these).
- Code generation via Inkwell for simple Sky functions.
- The slab and comptime evaluator MVP (just enough for non-generic Sky functions to work).
- Stub rlib generation from skyc.

**Phase 3: Generics (4-6 weeks).**
- Type-parametric Sky functions and structs (including generic impl blocks: `impl<T: Bound, ...> Trait for SkyType<T, ...>`).
- Sky's per_instance_mir provider returns real synthetic bodies for generic items.
- ReifyFnPointer-based dep registration in the synthetic body.
- Layout_of override for generic Sky types (handles abstract Param-bearing args by propagating `LayoutError::TooGeneric` from `tcx.layout_of` rather than gating on `has_param()` — same uniform code path as N=0 per §1.5.5).
- **Discovered-trait-impl-instances pipeline** (§8.9.5). The in-process capture-drain mechanism that handles cases 4/6 of the interop taxonomy: at the stub-rlib compile's `consumer_fill_modules` callback (NOT `after_rust_analysis` — @GCMLZ re-entry), the pure function `collect_consumer_trait_impl_instances(tcx) -> Vec<DiscoveredTraitImplInstance>` walks the partition for `MonoItem::Fn(instance)` entries matching `is_consumer_trait_impl_method`; the same callback drains the returned Vec inline, looks up the impl across loaded registries (local + upstream), substitutes the impl-method body with the captured args, and emits the bodies through Sky's standard codegen pipeline. Handles N=0 (non-generic impls) and N≥1 (generic impls) uniformly per @NNGZ (§26.15). Symbol canonicalization across crates is handled by the `codegen_fn_attrs` override marking consumer items `AvailableExternally` (Option 4) plus patch 5's gated share-generics escape — no augmented `upstream_monomorphizations` map required. (Earlier capture-ship-replay + A.2 augmentation retired 2026-06-21; see §F.13/§F.14/§F.14.1.)
- Architecture fence CI test (§26.15) catches non-generic special-cases introduced in Phase 3's discovery + populate machinery.

**Phase 4: Comptime (6-10 weeks).**
- Full comptime evaluator (Zig-style, slab-based).
- Comptime-produced types via SkyOpaqueType wrapper.
- Const generic Sky functions.
- Comptime-driven layouts.

**Phase 5: Groups and linear types (8-12 weeks).**
- Group system (parsing, type-checking, ABI translation to re_erased).
- Linear type system (parsing, type-checking; the panic-on-drop semantics come from the user-written `impl Drop` body, surfaced via the same AST-rewrite + cascade-discovery path as any other Sky trait impl — no separate mir_shims; see §15.7).
- Group-aware aliasing rules.

**Phase 6: Async (6-8 weeks).**
- Closure lifting to named structs in stub rlib.
- Async fn lowering to state machines.
- Migratory and cancellable splits.
- Sky-native race/select.

**Phase 7: Sky stdlib (4-6 weeks).**
- Channels, runtime executor, allocator.
- Sky's panic handler.
- Basic stdlib types and helpers.

**Phase 8: Crates.io distribution (2-4 weeks).**
- skyc publish that wraps cargo publish.
- Build.rs enforcement of Sky toolchain.
- Sidecar packaging.

**Phase 9: Tooling (4-8 weeks).**
- skyc check, run, test, fmt, new, add.
- Sky-aware IDE bindings (rust-analyzer compatibility layer).
- Source-level debugger integration.

**Total v1 estimated effort: ~50-80 weeks for a focused engineer (or smaller for a team).** This is a multi-year project at any reasonable team size.

**Empirical timing calibrations from toylang's implementation.** Toylang executed every facade-architectural piece of this plan end-to-end (the moral equivalent of Phases 1 + 3 minus comptime, plus all the facade-rebuild work). Four calibrations worth folding into Sky's estimate:

1. **The four-patch fork fits comfortably in Phase 1's 4-8 week budget.** The patches are small and structurally local. The wall-clock cost is dominated by rustc rebuilds (~15-20 min each), not patch authoring. (Toylang briefly carried a fifth patch and retired it 2026-06-22 — see §F.14.1/§F.17 — which reduces Sky's Phase 1 fork-patch surface accordingly.)

2. **Facade-internal refactors come in dramatically under estimate when chokepoints exist.** Items originally scoped at weeks by call-site count landed in hours when 1–2 helper functions carried the migration. See §F.12 for the analytic move. **Implication for Sky:** facade-rebuild work (predicate retirements, lock-free universe migration, symbol-name stateless conversion) is probably under-budget. New-architecture phases (comptime, async two-type split) don't get this discount.

3. **Cross-language inlining works empirically — but had to be re-verified, and Step 2 added a documented trade-off.** The perf claim ("interop is free with LTO" + "cross-crate inlines at -O3") is no longer architecturally-asserted; toylang's inlining test matrix (Thread C, `toylangc/tests/integration_projects/inlining/`) mechanically proves it across the meaningful subset of the 7-case taxonomy × 4 LTO modes (no_lto/thin/fat/explicit-off) × opt-level sweep × codegen-units {1, 16} cells — ~200 passing assertions as of the 2026-06-21 Tier-1 expansion. **Important historical correction**: the original `test_lto_smoke` purported to verify this empirically since pre-F1, but its `bl\t` substring check was vacuously satisfied against the `b\t` tail-jump LLVM actually emits. The matrix's Priority A surfaced the gap during the F1 investigation (2026-06-20); the root cause was `#[inline(never)]` on Sky-item stubs in `stub_gen` blocking the LLVM inliner. **Sky should land the matrix-equivalent fence in Phase 1**, not just a `test_lto_smoke`-equivalent. A single smoke test is insufficient evidence; the matrix's coverage across opt-levels + LTO modes is what makes regressions visible.

   **Step 2 trade-off (must be reflected in Sky's perf promise to users):** post-§5.5-Step-2 (commit 41f7ae4), cross-Sky/Rust body inlining at `lto = false` (cargo dev profile default) is LOST for Sky-top cases. Sky's `__toylang_main` body moves to the upstream `__lang_stubs` rlib's compile session under Step 2; thin-local LTO (§F.16 Level 3) cannot bridge across rustc invocations. Cross-crate inlining requires explicit `lto = "thin"` or `"fat"`. The 8 `_no_lto` matrix fixtures (case{1a,2,4,6}_no_lto + case4_o{1,2,s,z}) were flipped from "Sky body inlined" assertions to "tail-jump present" assertions to lock in this honest semantics. Rust-top cases (1b, 3, 5) still inline at `lto = false` because Sky's body for the called generic is mono'd at user_bin (same rustc invocation as `main`) and thin-local LTO bridges within an invocation. See §F.16 for the full inlining ladder.

4. **The five CI fences toylang built are forward-portable.** Sky should land equivalent fences in Phase 1 alongside the codegen plugin:
   - **Byte-identical pass-through corpus** (§25.3.5) — set up first; the hardest invariant.
   - **§9 export commitment** (non-export items get no rustc DefId) — fence with a stub_gen-equivalent unit test.
   - **Generic/non-generic uniformity** — grep-fence the discovery/typecheck/codegen paths for unmarked `type_params.is_empty()` branches (toylang's `architecture_fence.rs` is the reference).
   - **Cross-language inlining matrix** — toylang's inlining-matrix (Thread C) is the reference: 7-case taxonomy × 3 LTO modes + opt-level sweep + codegen-units variants, each asserting on disassembled binaries via `rustc-demangle` + `llvm-objdump`. A single `test_lto_smoke`-equivalent is NOT sufficient — the F1 investigation proved that vacuous assertions can survive for an extended period when the matrix is sparse.
   - **Sidecar determinism** — Section 7.4; build twice with isolated target dirs, byte-compare `.sky-meta`.

   Without these fences, the architectural invariants degrade silently. With them, regressions surface as named test failures with specific file/line refs.

**Sky stdlib bootstrap.** Sky's stdlib is itself a Sky library — it's written in Sky source and compiled by Sky's compiler. This is the standard bootstrap concern for compiled-language stdlibs. Sky's bootstrap path:

1. **Stage 0:** A small Sky stdlib written entirely in Sky source, compilable by an early Sky compiler that supports the minimal subset of features Sky stdlib needs (basic types, no comptime, no generics in some early phase). Stage 0 stdlib is the bootstrap floor.
2. **Stage 1:** Sky's compiler with features-needed-by-stdlib supported. Stage 1 compiles stage 0 stdlib. Now Sky's compiler can compile Sky source that uses stage 1 features.
3. **Stage N:** Each subsequent Sky compiler version compiles the previous version's Sky stdlib, then recompiles the stdlib using newly-added features.

The implication for skyc distribution: each Sky toolchain release bundles a pre-compiled stage-N stdlib. Users don't bootstrap from source; they install the bundled stdlib. Sky's CI bootstraps from source to verify the stdlib still compiles end-to-end on each release.

**Sky's runtime is similarly bootstrapped.** Channels, executor, allocator are Sky source. The runtime is compiled at toolchain release time and bundled.

The bootstrap concern is non-architectural — every compiled language faces it. Sky's posture: treat it like Rust's rustc bootstrap (multi-stage build, pre-compiled binaries shipped). Toolchain release process handles the bootstrap; users don't see it.

### 28.2 What's deferred to v2

**v2 features (not blocking initial Sky usability):**

- Fine-grained Sky-side incremental compilation (Section 22.2).
- Cancellable futures with async cleanup handlers.
- Opt-in precompiled bodies for Sky-pure libraries (Section 21.7).
- Sky-native registry (Sky's own crate registry, alternative to crates.io).
- Unified `spawn_blocking` API (Section 17.5).
- HRTBs for lifetime-discriminating dispatch and nested HRTBs (Section 11.9).
- Sky source-level editions (Section 27.1).
- Cross-Sky-version binary support via migration (Section 27.2/27.3).

### 28.3 What's deferred to Sky 1.0

**v1.0 represents Sky's first stable release.** Pre-1.0 versions are pre-release; breaking changes are allowed between minor versions. At 1.0:

- Source language is frozen (per editions).
- Sidecar format is frozen (per format_versions, with migration support).
- Sky stdlib's surface is frozen.
- Compatibility promises kick in.

1.0 is gated on confidence that the architecture is right. The signals: real Sky projects running in production for months, no major architectural surprises encountered, clear roadmap for v2 features.

### 28.4 Long-term: upstream contributions to rustc

Parallel to Sky's main implementation effort, Sky pursues upstream contributions to reduce the fork:

- File an RFC for arbitrary-typed const generics (Section 3.3).
- Engage with rust-lang's per_instance_mir-related discussions.
- Contribute LlvmCodegenBackend access improvements (the ModuleLlvm-wall PR direction from erw's spike).

These are background efforts, not on the critical path. They reduce Sky's long-term fork maintenance cost when they land.

### 28.5 Cross-references

- Section 3 — fork patches that Phase 1 lands.
- Section 13 — comptime that Phase 4 builds.
- Section 14 — async that Phase 6 builds.

---

## 29. Open Questions and Future Work

This chapter enumerates open questions that have not been resolved in this document. Each entry has a description, the relevant section it would extend, and the criteria for resolution.

### 29.1 HRTBs: lifetime-discriminating dispatch, nested HRTBs

Sky's automatic HRTB handling covers common cases. Two cases are explicitly deferred to v2: (1) Rust APIs with lifetime-discriminating impl dispatch (`impl Foo for Bar<'static>` vs `impl<'a> Foo for Bar<'a>`); (2) nested HRTBs (`for<'a> Trait<for<'b: 'a> InnerTrait<'a, 'b>>`).

Resolution criteria: (1) is resolved by either committing Sky source to a specific lifetime path (probably via syntax) or by sidecar annotations explicitly choosing the impl. (2) is resolved by extending the annotation format to express nested binders or by waiting for v2 to add Sky source syntax for them.

### 29.2 Async cleanup handlers

v1 has sync cleanup handlers. v2 may add async handlers for cases requiring async work during cancellation. Resolution criteria: concrete use cases that justify the complexity (graceful TCP close, distributed transaction commit/abort, etc.) emerge.

### 29.3 Sky-internal fine-grained incremental

v1 has crate-level incremental via cargo. v2 may add per-item incremental via a Sky-side query system. Resolution criteria: Sky compile times for real-size projects become a user pain point; the cost of building the query system is justified.

### 29.4 Sky's own registry (vs crates.io)

v1 uses crates.io. v2 may use a Sky-specific registry. Resolution criteria: Sky outgrows crates.io's affordances (Sky needs metadata cargo doesn't carry, Sky wants stricter version semantics, etc.).

### 29.5 Standard library design (Sky-native vs Rust-wrapping vs hybrid)

Sky's stdlib's high-level design is open. Three patterns:
- **Sky-native:** every stdlib type and function written in Sky source.
- **Rust-wrapping:** Sky stdlib mostly wraps Rust stdlib + select Rust ecosystem crates.
- **Hybrid:** core types Sky-native; performance-critical or platform-specific bits wrap Rust.

Resolution criteria: practical experience building stdlib reveals what works ergonomically and what's worth the wrap.

### 29.6 Fork-reduction trajectory

Sky's fork is 4 patches (§3.2) as of 2026-06-22, down from 5. Patch 5 (`consumer_lang_active` gated CGU-placement escape) retired jointly with Option 4 — see §F.14.1 / §F.17 for the design history. The remaining four are the `per_instance_mir` trio (patches 1-3) and the `fill_extra_modules` hook (patch 4).

Long-term, Sky pursues upstream landing (§3.3). The `fill_extra_modules` hook (patch 4) is the most upstreamable — it benefits cranelift, gcc-rs, spirv, and any backend wanting to contribute compiled modules to rustc's pipeline. The `per_instance_mir` trio (patches 1-3) is more Sky-specific and likely requires sustained RFC work to land — the canonical path is via an `adt_const_params` extension allowing plugin-provided semantics for non-valtree const-generic types, which would obviate Sky's Instance-keyed substitution need.

Resolution criteria: rust-lang lands a stable extension point that replaces `per_instance_mir`, or Sky's RFCs gain traction. Patch 4 may land first as an independent upstream contribution. If both upstream, Sky's fork shrinks to zero — but this is a multi-year arc and not on Sky's critical path.

### 29.7 Cargo.lock placement

Section 18.4 recommends `.skybuild/Cargo.lock`. Open: should Sky users see a `sky.lock` at project root instead (transformation logic on every read/write)?

Resolution criteria: design choice based on user feedback; bikeshedy but real.

### 29.8 Cross-references

- Each open question references its locked-context section (cross-referenced above).

---

## 29b. no_std and Embedded Posture

Sky positions as a systems language; embedded use cases are a real concern. This brief chapter records Sky's posture on no_std/embedded targets.

### 29b.1 v1: not supported

Sky v1 does not target no_std environments. Sky's runtime (executor, channels, allocator) is heavy-weight and assumes a hosted environment with file I/O, threading, and a heap allocator. Sky's stdlib depends on Sky's runtime. Targeting an embedded MCU without a heap or without a thread library is out of scope for v1.

### 29b.2 v2+: opt-in `#![no_std]`-equivalent

A v2 feature: Sky source can opt into a "Sky core" subset that doesn't require Sky's runtime. The subset includes:

- Basic types (integers, bools, fixed-size arrays).
- Functions and structs without async, without channels, without runtime-dependent features.
- A minimal allocator interface that the embedded application provides.
- Static memory regions in place of runtime-allocated groups.

The subset is approximately the Rust `core` + `alloc`-without-an-allocator scope. Sky source under the subset can compile to an embedded target without bringing in Sky's runtime.

### 29b.3 v2+: bare-metal target support

Targeting bare-metal triples (`thumbv7em-none-eabi`, etc.) is conditionally supported once Sky's core subset exists. Sky's codegen accepts the target triple; Sky's emitted code respects target-specific calling conventions; Sky's typechecker is unchanged.

The runtime support library is *not* available on bare-metal targets — there is no Sky runtime to call into. Sky source for bare-metal must be self-contained within the core subset.

### 29b.4 Posture vs Rust embedded

Rust's embedded ecosystem (`#![no_std]`, embedded-hal, etc.) is mature. Sky's embedded posture is conservative: don't compete with Rust's embedded story until Sky has a real story to tell. v1 stays away from embedded; v2 introduces minimum viable support; v3+ expands based on user demand.

The architectural decisions in this document do not preclude embedded support; they just don't prioritize it. Sky's interop mechanism (per_instance_mir, stub rlibs, sidecars) works at any target as long as the runtime support library is appropriately scoped.

---

## 30. Glossary

This chapter defines terms used throughout the document. Where a term is specific to Sky, it's marked [Sky]. Where it's inherited from another project, it's marked [Source]. Where it's Rust-standard, no mark.

**Group [Sky]** — Sky's analog to Rust's lifetime. A named, possibly hierarchical, possibly runtime-realized memory region within which a set of references live.

**Linear type [Sky]** — A type whose values must be explicitly consumed; cannot be silently dropped. Linearity is enforced by Sky's typechecker.

**Comptime [Sky, after Zig]** — Sky's compile-time evaluation. Same expression language as runtime; uses a "slab" (in-memory byte buffer with allocator) to represent comptime values.

**Slab [Sky]** — Sky's compile-time RAM-simulation. Comptime values are allocated in the slab; references to them are integer offsets.

**Migratory [Sky]** — A property of async fns: the future is sendable across threads, movable, and cannot hold borrows across `.await`. Marked with the `migratory` keyword.

**Cancellable [Sky]** — A property of futures (via `into_cancellable` wrapping): the future can be dropped while executing; a user-supplied cleanup handler runs on drop.

**Stub rlib [Sky]** — A skyc-generated Rust crate (rlib) containing Rust-source declarations of every Sky export item. Compiled by rustc as ordinary Rust; Sky's `collect_and_partition_mono_items` query override filters consumer items out of rustc's CGU list before LLVM codegen, so the stub bodies produce no `.o` symbol; Sky's `fill_extra_modules` hook emits the real `External`-linkage bodies. (A brief 2026-06-21 → 2026-06-22 detour through an `codegen_fn_attrs` override / `AvailableExternally` linkage was reversed because it created a CGU-placement hazard requiring rustc-fork patch 5; see §F.14.1.)

**Sidecar [Sky]** — A binary file adjacent to each stub rlib, containing the Temputs for the library.

**Temputs [Vale, adopted by Sky]** — Vale's typing-pass output. Sky inherits the representation and extends it for Rust interop concerns. The Temputs is the data the sidecar serializes.

**Marker [Sky]** — The `pub const __SKY_STUBS_MARKER: () = ();` declaration at the root of every Sky-generated stub rlib. Sky's `rustc` checks for the marker at crate-load time to decide whether to activate Sky's machinery.

**Typeid [Sky]** — A content-addressed u64 identifying a Sky-side type. Used in the SkyOpaqueType wrapper to project Sky-side types onto rustc-visible territory without naming the type directly.

**SkyOpaqueType<const T: u64> [Sky]** — A universal wrapper type pre-declared in Sky's stdlib. Used to represent Sky-side types that rustc shouldn't know about by name (non-exports in Rust generics, comptime-produced types, etc.).

**Per_instance_mir [Sky, also erw]** — Sky's custom rustc query. Instance-keyed; Sky's provider returns a synthetic MIR body for each Sky Instance during rustc's monomorphization phase. Added via three of Sky's four fork patches (the fourth is the `fill_extra_modules` allocator-callback hook for inline codegen contribution). Historical patch 5 (`consumer_lang_active` gated escape) was retired 2026-06-22 — see §F.14.1 and §F.17 for the design history.

**Approach A [Source: dep-discovery-approaches.md]** — Instance-keyed dep discovery; Sky substitutes args itself. Used by Sky.

**Approach B [Source: dep-discovery-approaches.md]** — DefId-keyed dep discovery via `optimized_mir` override; rustc's collector substitutes args. Used by erw; not by Sky.

**Interleaving [Source: why-interleaved-monomorphization.md]** — Sky's compiler hooks fire during rustc's monomorphization phase, supplying per-Instance information as the collector encounters concrete Instances. The opposite of pre-pass (Sky enumerates instantiations before rustc) and post-pass (Sky picks up after rustc).

**Pre-pass** — A hypothetical alternative design where Sky enumerates all required Rust monomorphizations before rustc starts. Insufficient for Sky's interop cases (Cases 1b, 3, 4, 5, 6 of the seven-case taxonomy).

**re_erased** — Rustc's lifetime placeholder for post-borrowck lifetimes. Sky's groups erase to re_erased at the boundary.

**HRTB** — Higher-Ranked Trait Bound. `for<'a> Trait<&'a T>`. Rust syntax for quantification over lifetimes.

**`.skybuild/`** — Skyc-generated cargo workspace directory. Contains the stub rlibs and the bin shim. Cargo operates on this directory.

**Marker-detection** — Sky's mechanism for "is this a Sky stub rlib?" Walks the crate root for `__SKY_STUBS_MARKER`.

**Forked rustc** — Sky's rustc binary, statically linking the codegen backend and frontend, plus the three per_instance_mir fork patches. Cargo invokes this binary for every crate compile.

**v1 / v2** — Sky version. v1 is the first usable release; v2 adds features that aren't blocking initial usability. Sky 1.0 is the first stable release.

**Edition [Source: Rust]** — A way for source compatibility to evolve. Sky considers using editions starting at 1.0.

**Build.rs** — A Rust crate's build-script file. Skyc-generated build.rs scripts enforce Sky toolchain presence (Section 21.3).

**`<crate>.sky-meta`** — File extension for sidecar files. Located adjacent to the stub rlib.

**`<crate>.sky-annotations.toml`** — File extension for sidecar annotation files. Provides additional binding information to Sky's typechecker.

---

## Appendices

### Appendix A. Worked Examples

End-to-end walked examples for the 7-case interop taxonomy live in §2 (one subsection per case, with Sky source + Rust source + the stub-rlib mechanism explained inline). For the artifacts each example produces:

- Stub rlib content (Rust declarations, opaque types, marker, impl blocks): see §6 plus the worked-example shapes in §10.
- Sidecar Temputs entries (typed AST, typeids, item bodies): see §8.
- per_instance_mir synthetic body shape (ReifyFnPointer casts for Rust deps): see §19.3.
- Drop glue + linear-type panic body: see §15.7.
- Migratory async + Future impl shape: see §14.10.

The original A.1–A.8 enumeration mostly duplicated §2's taxonomy-walk plus §6/§7/§8/§14 artifact details; it has been retired to keep this document focused.

### Appendix B. Reference: Fork Patches

The four patches Sky maintains against vanilla nightly rustc. The `per_instance_mir` trio (B.1–B.3) adds Sky's Instance-keyed MIR query. The `fill_extra_modules` allocator-callback hook (B.4) lets Sky's codegen emit LLVM IR directly into rustc-supplied modules. Historical patch 5 (`consumer_lang_active` gated escape) retired 2026-06-22 — see §F.14.1 / §F.17 for the design history; B.5 below preserves the patch shape for archive purposes. See §3.2 for full text and rationale.

#### B.1 per_instance_mir query declaration

`compiler/rustc_middle/src/query/mod.rs`:

```rust
rustc_queries! {
    // ... existing queries ...
    
    /// Sky's per-Instance MIR query. The provider supplies a synthetic MIR
    /// body for a given Instance, used by Sky's interleaving mechanism.
    query per_instance_mir(key: ty::Instance<'tcx>) -> Option<&'tcx mir::Body<'tcx>> {
        desc { "computing per-Instance MIR for {:?}", key }
        cache_on_disk_if { false }
    }
    
    // ... existing queries ...
}
```

#### B.2 Collector calls per_instance_mir before instance_mir

`compiler/rustc_monomorphize/src/collector.rs`:

```rust
fn collect_items_of_instance<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
    output: &mut MonoItems<'tcx>,
) {
    let body = tcx.per_instance_mir(instance)
        .unwrap_or_else(|| tcx.instance_mir(instance.def));
    // ... existing collector walk over `body` ...
}
```

#### B.3 Default provider returns None

`compiler/rustc_mir_transform/src/lib.rs::provide` (where rustc's MIR-related default providers are registered):

```rust
providers.per_instance_mir = |_tcx, _instance| None;
```

This makes the query a no-op for non-Sky use. Sky's codegen backend's `provide()` method overrides this with Sky's real provider.

#### B.4 `fill_extra_modules` allocator-callback hook (Approach B)

`compiler/rustc_codegen_ssa/src/traits/backend.rs`:

```rust
pub trait ExtraModuleAllocator<M> {
    /// Allocate a fresh backend module owned by the codegen driver and
    /// borrowed for the duration of the surrounding fill_extra_modules
    /// call. Subsequent allocate calls invalidate prior references.
    fn allocate(&mut self, name: &str) -> &mut M;
}

pub struct VecAllocator<'a, M, F: FnMut(&str) -> M> {
    pub modules: &'a mut Vec<ModuleCodegen<M>>,
    pub make_module: F,
}

impl<'a, M, F: FnMut(&str) -> M> ExtraModuleAllocator<M> for VecAllocator<'a, M, F> {
    fn allocate(&mut self, name: &str) -> &mut M {
        let module = (self.make_module)(name);
        self.modules.push(ModuleCodegen::new_regular(name, module));
        &mut self.modules.last_mut().unwrap().module_llvm
    }
}

pub trait ExtraBackendMethods: ... {
    // ... existing methods ...

    /// Backend-specific module constructor used to mint freshly-allocated
    /// modules. Default panics; backends that participate in extras
    /// override this to call their own per-CGU constructor.
    fn allocate_extra_module<'tcx>(
        &self,
        _tcx: TyCtxt<'tcx>,
        _name: &str,
    ) -> Self::Module { panic!("...") }

    /// Contribute extra modules. Called from `codegen_crate` synchronously
    /// on the main thread BEFORE `start_async_codegen`. Default no-ops so
    /// non-adopting backends are unaffected.
    fn fill_extra_modules<'tcx>(
        &self,
        _tcx: TyCtxt<'tcx>,
        _allocator: &mut dyn ExtraModuleAllocator<Self::Module>,
    ) { }
}
```

`compiler/rustc_codegen_ssa/src/base.rs::codegen_crate` constructs a `VecAllocator` around the in-flight extras vec, passes it to `backend.fill_extra_modules` (synchronously on the main thread before `start_async_codegen`), then forwards the filled vec into `start_async_codegen`. See §F.4 for the load-bearing detail about insertion-point timing.

`compiler/rustc_codegen_ssa/src/back/write.rs` is the largest patch-4 surface. `start_async_codegen` and `start_executing_work` each gain an `extra_modules: Vec<ModuleCodegen<B::Module>>` parameter; the value is threaded from `base.rs::codegen_crate` through to the body of `start_executing_work`'s `thread::JoinHandle`. Inside that body (i.e. on the **coordinator thread**, not the main thread), a `for extra_module in extra_modules` loop runs each module through `execute_optimize_work_item` synchronously, dispatching `Finished` to a `compiled_extra_modules` vec, `NeedsFatLto` to `needs_fat_lto`, and `NeedsThinLto` to `needs_thin_lto`. After the message loop completes, `compiled_modules.extend(compiled_extra_modules)` merges the finished extras into the regular-module list before the deterministic sort.

`compiler/rustc_codegen_ssa/src/traits/mod.rs` re-exports `ExtraModuleAllocator` and `VecAllocator` alongside the existing `BackendTypes` / `CodegenBackend` / `ExtraBackendMethods`.

`compiler/rustc_codegen_llvm/src/lib.rs` adds:

- An `allocate_extra_module` override calling `ModuleLlvm::new(tcx, name)` — the same constructor rustc's own per-CGU pipeline uses.
- A `fill_extra_modules` override reading a process-global `OnceLock<FillExtraModulesHook>` settable via `set_fill_extra_modules_hook(fn_ptr)`. The facade installs the hook in `LangDriver::config` alongside `Config::override_queries`. Process-global storage is forced by the crate-dependency graph (`rustc_session` is upstream of both `rustc_middle` and `rustc_codegen_llvm`, so the `TyCtxt`-typed hook can't live on `Session`); the hook is set once at init and read lock-free thereafter.
- `ModuleLlvm::llcx_raw_mut() -> *mut c_void` and `ModuleLlvm::llmod_raw() -> *mut c_void` — type-erased raw-pointer accessors for FFI bridging into externally-managed LLVM wrappers (Inkwell's `Context::new` wrapped in `ManuallyDrop` + `Module::new_borrowed`). Type-erased to `c_void` to avoid leaking private `llvm::Context` / `llvm::Module` types through the public API.

Total surface for patch 4: ~194 lines across 5 files (the four under `rustc_codegen_ssa` plus `rustc_codegen_llvm`). Default-no-op trait methods preserve vanilla rustc behavior. Forward-portable to other backends (cranelift, gcc-rs, spirv) — recommended as the first patch to attempt upstream landing.

**Approach B vs the earlier v1 bytes-as-interface shape.** The v1 patch 4 had `extra_modules() -> Vec<ModuleCodegen<M>>` and a `parse_from_tcx` sub-patch; Sky's emitter serialized Inkwell-built modules to bitcode bytes and rustc parsed them back. That shape worked but was interface-laziness — Sky's CGU context isn't migrating into rustc's, just being constructed and thrown away. Approach B eliminates the round-trip by having rustc own the LLVM resources and lend them to Sky via the allocator callback. Closes risks B9 (LLVM-binding version skew — structurally impossible), B10 (LLVM 21 BitcodeWriter bug — no bitcode is written), and B11 (round-trip scaling cost — no round-trip). See §F.15 for the design history.

#### B.5 `consumer_lang_active` gated CGU-placement escape (patch 5) — RETIRED 2026-06-22

**Status: design history.** Patch 5 retired alongside Option 4 on 2026-06-22. With the partition filter restored, no AvailableExternally body reaches LLVM, so the CGU-placement hazard the escape papered over cannot fire. The patch shape is preserved below for archive purposes; the current fork does not include it.

Historical text: three sites in `rustc_middle` and `rustc_mir_transform`. The patch declared the `consumer_lang_active(()) -> bool` query (default provider returns `false`), then in `Instance::upstream_monomorphization` gated the existing `!share_generics()` early-return on `!(tcx.consumer_lang_active(()) && upstream_monomorphizations_for(def).contains_key(args))`. Sky's facade installed an override that returned `true` when `__SKY_STUBS_MARKER` was detected locally or upstream. The marker-walk logic lives in the helper `pub fn is_sky_active(tcx) -> bool` (`rustc-lang-facade/src/lib.rs`); post-retirement, `cross_crate_inlinable` (B16) and other Sky-side gates call this helper directly without going through any query.

**Empirical retirement evidence:** `tmp/patch5-empirical-2026-06-21/VERDICT.md` documents the case5_no_lto contrast probe (build LINKS, runtime SBMNBIZ panic, byte-identical symbol names, co-located CGU vs separate-CGU comparison, main disassembly showing direct call to `core::panicking::panic` under patch-5-OFF) that validated §F.17's CGU-placement framing and justified the joint Option 4 + patch 5 retirement.

#### B.6 Future patch sites (if needed)

Sky may add additional fork patches if specific risks materialize. The current patch list is locked at 4; additions require explicit justification and signoff. Section 25 covers risks that might warrant new patches; §25.2 B5 and B8 are examples of risks that turned out to be addressable architecturally without new patches.

### Appendix C. Reference: Sky Codegen Backend Methods

Sketches of the methods Sky's CodegenBackend implementation provides.

#### C.1 init, provide, codegen_crate, join_codegen, link

```rust
impl CodegenBackend for SkyCodegenBackend {
    fn name(&self) -> &'static str { "sky" }
    
    fn init(&self, sess: &Session) {
        self.inner.init(sess);
        if has_sky_marker(sess) {
            sky_runtime_init(sess);
        }
    }
    
    fn provide(&self, providers: &mut Providers) {
        self.inner.provide(providers);
        if let Some(sess) = current_session_with_marker() {
            sky_install_query_overrides(providers);
        }
    }
    
    fn codegen_crate(&self, tcx: TyCtxt<'_>) -> Box<dyn Any> {
        // Pass-through to LlvmCodegenBackend. Sky's bitcode contribution
        // happens via the extra_modules_hook installed in provide();
        // rustc's pipeline submits Sky's modules synchronously before
        // start_async_codegen (patch 4, §3.2/§B.4) and runs them through
        // the standard optimize → ThinLTO summary → emit path as
        // just-another-CGU. No SkyOngoingCodegen wrapper, no .o injection.
        self.inner.codegen_crate(tcx)
    }
    
    fn join_codegen(&self, ongoing: Box<dyn Any>, sess: &Session, outputs: &OutputFilenames) -> (CodegenResults, FxIndexMap<WorkProductId, WorkProduct>) {
        // Pure pass-through. Sky's modules are already in `ongoing` via
        // patch 4; rustc finalises them with everything else.
        self.inner.join_codegen(ongoing, sess, outputs)
    }
    
    fn link(&self, sess: &Session, codegen_results: CodegenResults, metadata: EncodedMetadata, outputs: &OutputFilenames) {
        self.inner.link(sess, codegen_results, metadata, outputs)
    }
    
    // Other methods delegate to self.inner unchanged.
}
```

The `fill_extra_modules` hook (installed during driver setup via `rustc_codegen_llvm::set_fill_extra_modules_hook(...)` — see `install_consumer_modules_hook` in `extra_modules_hook.rs`, called from `LangDriver::config`, NOT from the backend's `provide()`) is where Sky's bitcode actually enters rustc's pipeline. Under the shipping Approach B shape (§C.4, §F.15), the hook receives an `&mut dyn ExtraModuleAllocator<Self::Module>`, calls `allocator.allocate(name)` to obtain a rustc-owned `&mut ModuleLlvm`, wraps the borrowed `LLVMContext` + `LLVMModule` pointers in Inkwell's suppressed-Drop handles (`ManuallyDrop<Context>` + `Module::new_borrowed`), and emits LLVM IR directly into the rustc-owned module. No bitcode serialization, no `parse_from_tcx` round-trip, no context migration — rustc retains ownership of every module throughout. See §C.4 for the full pattern.

#### C.2 Consumer item suppression: partition filter

Sky overrides `collect_and_partition_mono_items` via `Config::override_queries`. The override delegates to the default partitioner, then walks each CGU rebuilding it with consumer-defined items removed (`is_consumer_codegen_target` returns true → skip the item). The filtered partition reaches `LlvmCodegenBackend::codegen_crate`; consumer items never appear in LLVM IR via the rustc path. Sky's `fill_extra_modules` contribution at the same compile session emits the real `External`-linkage bodies as the sole definition. See `rustc-lang-facade/src/queries/partition.rs` for the shipping code (~95 LOC) and §5.3 / §F.14.1 for the design rationale.

A brief 2026-06-21 → 2026-06-22 experiment with Option 4 (a `codegen_fn_attrs` override stamping `AvailableExternally` linkage on consumer items) was reversed because the AvailableExternally body created a CGU-placement hazard that required rustc-fork patch 5 to paper over. The partition filter avoids the hazard structurally and let patch 5 retire. See §F.14.1 / §F.17 for the design history.

#### C.3 Cross-platform considerations

Sky's codegen produces target-specific LLVM IR. The target triple comes from `tcx.sess.target`. Cross-compile works automatically because Sky reads the target from rustc's session, not from any host-system-dependent source.

Target-specific runtime support (e.g., the runtime's I/O implementation differs between Linux and Windows) is selected at runtime build time, similar to how Rust's stdlib has target-conditional code.

#### C.4 Shipping patch 4 shape: rustc-owns-lends (Approach B)

Rustc allocates each per-CGU `LLVMContext` + `LLVMModule` via the standard `ModuleLlvm::new(tcx, name)` path and lends the borrowed pointers to the consumer through an `ExtraModuleAllocator<M>` callback. The consumer wraps the borrowed handles in suppressed-Drop Inkwell wrappers (`ManuallyDrop<Context>` + `Module::new_borrowed`) and emits LLVM IR directly into the rustc-owned module. No bitcode serialization, no `parse_from_tcx` round-trip, no context migration; rustc retains ownership throughout. See `rustc-lang-facade/src/extra_modules_hook.rs` + `toylangc/src/llvm_gen.rs::fill_module` for the shipping code, and §B.4 + §F.15 for the patch surface, design history, and the vendored `Module::new_borrowed(LLVMModuleRef) -> ManuallyDrop<Module<'ctx>>` helper (~5 LOC vendor patch at `vendor/inkwell/src/module.rs`; retires together with the workspace `[patch."https://github.com/TheDan64/inkwell"]` override when upstream Inkwell lands an equivalent API).

Fork patch surface: ~194 LOC across 5 files (the `ExtraModuleAllocator<M>` trait + generic `VecAllocator<'a, M, F>` driver + the `allocate_extra_module` + `fill_extra_modules` trait methods in `traits/backend.rs`; the re-exports in `traits/mod.rs`; the call-site change in `base.rs::codegen_crate`; the `extra_modules` parameter threading + coordinator-thread `execute_optimize_work_item` loop + `compiled_modules.extend` in `back/write.rs`; the `FillExtraModulesHook` OnceLock + installer + `ExtraBackendMethods` overrides + `ModuleLlvm::llcx_raw_mut` + `llmod_raw` raw-pointer accessors in `rustc_codegen_llvm::lib`). The earlier v1 bytes-in shape (`extra_modules() -> Vec<ModuleCodegen<M>>` plus `ModuleLlvm::parse_from_tcx`) was retired in the rev-2 rewrite — no backward-compatibility surface remains.

### Appendix D. Reference: Temputs Schema

The Temputs schema is an implementation concern, not an architectural one. The architecturally load-bearing properties are covered elsewhere:

- Sidecar header layout — §7.2.
- Sidecar serialization format + determinism — §7.3, §7.4.
- Format versioning + compatibility — §7.5, §27.
- Cross-crate item references (`RustRef`, `SkyRef`) — §8.2.
- Rust call encoding (`RustCall` AST node) — §8.3.
- Rust trait impl markers — §8.4.
- Typeid table — §8.5, §10.8.
- Item bodies + source positions — §8.6, §8.7.
- Discovered trait-impl Instances (in-process Vec, no longer a registry field) — §8.9.5.

For the actual struct shapes the toylang reference implementation uses, see `toylangc/src/toylang/registry.rs` (`ToylangRegistry`) and the surrounding `toylangc/src/toylang/sidecar.rs` (serialization).

### Appendix E. Sky Source Examples for Each Major Feature

Per-feature Sky source examples are inlined in the relevant chapters: groups (§11), linear types and drop (§14, §15), comptime (§13), async migratory/cancellable (§14), and so on. The examples in chapter bodies are the canonical source samples; this appendix used to duplicate them and has been retired.

### Appendix F. Lessons from the toylang prototype implementation

Empirical findings from implementing this architecture in `erw`'s
toylang prototype. Each finding either corrects a doc claim, adds a
load-bearing detail that wasn't anticipated, or records a debugging
trap worth knowing. The toylang repo is the reference implementation
for the patterns described here; commit-level archaeology is omitted
in favour of the architectural lesson.

#### F.1 Phantom constraints (things the doc may have over-feared)

**"Sky's emitter must share `LLVMContext` with rustc's LLVM backend."**
Wrong. Rustc itself runs **one `LLVMContext` per CGU**. Each CGU is its
own isolated type universe; cross-CGU symbol resolution happens at
link time, not at codegen time. Sky's modules just join as additional
CGUs (via patch (c) — see §5.3). No context sharing needed; no unsafe
transmute between `inkwell::Context` and rustc's wrapper; no need to
forsake Inkwell's safe API for rustc's private FFI surface. The
constraint that originally framed §5.3's "Sky's `codegen_crate` walks
the queue inline" approach was over-cautious.

**"Std stays uninlineable under cross-language LTO without `-Z build-std`."**
Wrong for the patch (c) architecture. The original concern was based
on stock rustup-shipped std rlibs being built with `embed-bitcode=yes`
+ `lto=off`, which means the bitcode is present in a `.llvmbc` section
but the `.o` files are machine code. LLD's plugin-LTO can't extract
the bitcode, so std stays a function-call boundary under LLD-driven
LTO. But **patch (c) makes Sky's modules ride rustc's own LTO**, not
LLD's. Rustc's `back/lto.rs::prepare_lto` extracts `.llvmbc` from
prebuilt rlibs natively — that's how `-C lto=thin` has worked since
forever. With Sky's module in rustc's pipeline, std inlining works
without `build-std`. Toylang's `test_lto_smoke` empirically verified
this: `lto_smoke::main` constant-folds Sky's `10 + 20*2` into `mov w8,
#50` baked into the Rust caller, with no `bl` to any Sky symbol.

**"`#[inline(never)]` on stub fn shells prevents the cross-language
inlining race."** Wrong. Symbol-priority bugs don't live at the
inliner layer. If the LTO IR linker picks the stub's `unreachable!()`
def over Sky's real def for the rustc-mangled symbol, `#[inline(never)]`
on the stub just relocates *where* the panic happens (from inlined
inside the caller to a real call landing on `unreachable!()`). The
fix is at the symbol-resolution layer: `#![no_builtins]` on the stub
rlib (§6.2) so the stub's body never enters the LTO IR linker pool.

**F1 investigation correction (2026-06-20).** `#[inline(never)]` on
Sky-item stubs was retained on three sites in `stub_gen.rs` past the
period when its protections were load-bearing. Empirically: removing
it unblocked Sky's body from inlining into the bin's main at every
-O3 mode (thin LTO, fat LTO, no LTO) AND across the cross-crate
boundary at -O1+ even without LTO. The original `test_lto_smoke`
assertion was vacuously satisfied (the `bl\t` substring check didn't
match the `b\t` tail-jump LTO actually emits), so the gap was
invisible until the inlining test matrix's Priority A surfaced it.
The architecture is consistent with removal: §F.3 establishes
`#![no_builtins]` as the LTO-race fix; §25.2 B14 closes the
share-generics gate via patch 5 independently; §25.2 B12 acquires
operational meaning only under v2's precompiled-bodies feature
(§21.7), gated in v1 by build.rs (§21.4). The Phase-6 stdlib
helpers (`__toylang_option_unwrap` / `__toylang_result_unwrap` etc.)
keep `#[inline(never)]` for a different reason — stable predictable
symbol to link against (§6.6.5).

#### F.2 Single-symbol over two-symbol (Path B)

Original design: stub fn carries rustc-mangled name; Sky's bitcode
emits real body under Sky-chosen name (`__sky_impl_*`); `symbol_name`
override redirects rustc-mangled → Sky-chosen at link time. This works
under non-LTO (the rlib's body lazy-loads from the archive only if
needed; Sky's def wins; the rlib's body never gets pulled). Under
ThinLTO it breaks: LTO's IR linker pulls *all* participating rlibs'
bitcode into one pool, sees two defs for the same logical function
(stub's `unreachable!()` under rustc-mangled name + Sky's body under
`__sky_impl_*`), and picks one. Sometimes the wrong one.

The fix: **Sky's bitcode emits each rustc-visible body under the
rustc-mangled name rustc would have given the stub fn.** Single
symbol. One def. LTO can't pick wrong because there's no choice.

To compute the rustc-mangled name from Sky's side, call
`tcx.symbol_name(instance)` directly. Pre-Phase-F (before 2026-06-24)
this went through a saved upstream provider pointer
(`default_symbol_name()(tcx, instance)`) to dodge re-entrance through
Sky's own `symbol_name` override. With the override retired (handoff
Decision 2), there's nothing to dodge — `tcx.symbol_name` reads
rustc's default v0 mangler directly.

Path B is the canonical design — §6.2 + §9.6 describe it.

#### F.3 `#![no_builtins]` on stub rlibs — RETIRED 2026-06-21

**Status: RETIRED.** This subsection documents the historical mechanism
for archive value; the attribute is no longer emitted by toylang's
`stub_gen.rs::generate`.

**Original (pre-Option-4) rationale:** Even with single-symbol naming
(F.2), the stub rlib's `.rcgu.o` sections still carried `unreachable!()`
bodies (rustc had to compile the source to *something*). Under
`lto = "thin"`, LLD's LTO machinery pulled that bitcode into the IR
linker pool unless the rlib opted out. `#![no_builtins]` was the
canonical opt-out — same one `compiler_builtins` uses — so the rlib's
bitcode never entered LTO's pool and Sky's bitcode was the sole def
of the symbol.

**Why it's obsolete:** Option 4 (shipped 2026-06-21; arch §F.14.1)
overrides `codegen_fn_attrs` to set explicit `AvailableExternally`
linkage on Sky-defined items. LLVM's IR linker unambiguously prefers
the `External`-linkage real body (from `fill_extra_modules`) over the
`AvailableExternally` placeholder (from rustc's normal compile of the
stub source), regardless of whether both end up in the same LTO pool.
The defensive `#![no_builtins]` layer became redundant once linkage
disambiguated the bodies directly.

**Empirical verification (Round 2 E2, 2026-06-21):** Removed
`#![no_builtins]` from `toylangc/src/build.rs`. Full inlining matrix
(39 fixtures × no_lto/thin_lto/fat_lto) passed; full integration suite
passed 317/0/1; `llvm-objdump` confirmed zero `udf`/`brk` traps in
Sky callers across every Sky-export fixture.

**If a future nightly rustc bump reintroduces a similar collision:**
the place to start is to verify (a) Sky's `codegen_fn_attrs` override
still produces `Some(AvailableExternally)` for Sky items, and (b)
LLVM still honors the External-over-AvailableExternally preference
at the IR linker. If both hold and the bug returns, restoring
`#![no_builtins]` is a one-line emit in `build.rs`.

#### F.4 Patch (c) — synchronous submission BEFORE async codegen

§5.3 describes patch (c) (`ExtraBackendMethods::extra_modules`) as a
hook between rustc's CGU loop and `codegen_finished`. **Implementation
gotcha**: that obvious-looking insertion point doesn't work. The
coordinator's `main_thread_state == Codegenning` assertion (see
`rustc_codegen_ssa/src/back/write.rs`) fires when an `extra_modules`
submission lands mid-CGU-loop. The submission triggers a `CodegenDone`
message which the coordinator interprets as "a worker thread just
finished a CGU"; the state machine assumes that means a worker is
currently active, which isn't true for a main-thread submission.

The right shape is a **two-stage split**: rustc's `base.rs::codegen_crate`
invokes `backend.fill_extra_modules(tcx, &mut allocator)` **synchronously
on the main thread, BEFORE `start_async_codegen`** is called; the
returned `Vec<ModuleCodegen<B::Module>>` is then passed into
`start_async_codegen` and threaded through to `start_executing_work`'s
`thread::JoinHandle` body. Inside that body — i.e. on the **coordinator
thread** spawned by `start_executing_work`, not the main thread —
a `for extra_module in extra_modules` loop runs each module through
`execute_optimize_work_item` synchronously (same path the allocator
module takes), dispatching results into the existing `compiled_modules`,
`needs_fat_lto`, and `needs_thin_lto` vecs before the message loop
starts. The fork's in-code comment at the loop site (`back/write.rs`
line ~1513) reads "Process synchronously on the coordinator thread,
mirroring the allocator module pattern" — that's the accurate
description; the main-thread piece is just the hook call itself, not
the per-module processing. See Appendix B.4 for the patch shape and
toylang's `~/rust` fork on the `per-instance-mir` branch for the
exact insertion sites.

#### F.5 Replace lifetime-erased CGU stash with direct provider re-call

Erw initially needed a `'static`/`*const`/unsafe-pointer stash to hold
the unfiltered CGU slice across the gap between the partitioner
override (which filters Sky items out so rustc's downstream pipeline
doesn't try to codegen them as Rust) and the consumer's codegen pass
(which still wants to walk the unfiltered list for Case 1b
discovery). That stash worked but was a maintenance liability — 87
lines of soundness-by-discipline code.

The shipped replacement: **call the saved upstream provider directly
from inside `codegen_crate`** with live `'tcx`. `Sky` exposes
`default_collect_and_partition() -> CollectAndPartitionFn` as a `pub`
accessor on the OnceLock-saved function pointer. Call it as
`default_collect_and_partition()(tcx, ()).codegen_units` and you have
a sound `'tcx`-bound slice with no unsafe. Cost: re-runs the mono
collector once. Negligible for toylang's fixture sizes; could matter
at larger Sky-project scale but is bounded by what rustc itself
already does.

Note: calling `tcx.collect_and_partition_mono_items(())` does NOT
work for this purpose — the in-memory query cache memoizes the
override's filtered result. The raw fn pointer bypasses cleanly.


#### F.6 Accessor methods as regular functions

Sky's design implies a special path for accessor methods (struct field
access from Rust source: `widget.field` → rustc generates a call to an
inherent impl method → Sky needs to emit the accessor body). Toylang
originally had a special-case discovery branch in the CGU walk plus a
specialized `codegen_accessor_inline` that emitted GEP + load.

**Empirical finding**: accessors can be modeled as **regular Sky
functions** with synthesized bodies. After parsing the Sky source,
walk every `(struct, field)` pair and synthesize a `ToyFunction`
(equivalent: Sky-internal function-equivalent) of the shape:

```
fn (self: &Struct) -> &FieldType { &self.field }
```

This function goes through the same registration, discovery,
substitution, mangling, and codegen pipeline as any other Sky
function. No special CGU walk branch, no `codegen_accessor_inline`,
no symbol-name special case. LLVM's inliner trivially collapses the
trivial wrapper at -O1+.

Three differences between accessor and regular function survive, all
of which are target-language requirements rather than architectural
choices: (i) Sky source uses `widget.field` syntax instead of
`field(widget)`, (ii) the stub rlib emits an `impl Foo { pub fn
field(&self) ... }` block (Rust requires inherent-method syntax for
`widget.field` to typecheck), (iii) the body is synthesized at parse
time rather than user-written.

The Sky-side architectural payoff: one fewer special case across the
discovery, codegen, symbol-mangling, and serialization paths. C#
treats accessors this way; Sky can too.

The specialized accessor codegen path retires entirely.

#### F.7 `SkyUniverse.struct_infos`: type-erased consumer metadata

Sky's design has the facade owning a content-addressed registry of
Sky items (§7). For trait-impl discovery, layout queries, and Case 6
cross-Sky-crate scenarios, the facade needs to look up "what fields
does Sky struct `Foo` have" without compile-time coupling to the
consumer's typed-AST format.

The shipped pattern: the universe carries a `HashMap<String, Arc<dyn
Any + Send + Sync>>` of struct metadata. The consumer inserts its own
typed metadata (toylang inserts `Arc::new(ToyStruct { ... })`);
lookups return the `Arc` and the consumer downcasts on read. Facade
stays consumer-agnostic; consumer-specific Temputs format stays
consumer-side; cross-crate layout queries (Case 6) go through a
single source of truth instead of dual-source workarounds.

The same pattern would let Sky's universe host plugin-defined
metadata (third-party Sky extensions registering their own Temputs)
without leaking into Sky's core schema.

The earlier alternative — a consumer-side mutex-protected mirror of
upstream struct metadata — retires under this pattern.

#### F.8 LLVM 21 bitcode-writer bug — historical, retired under Approach B

**Status: closed.** Under the shipping patch 4 rev 2 (Approach B, §F.15), no bitcode is serialized in Sky's pipeline — Sky emits LLVM IR directly into rustc-owned modules via Inkwell's suppressed-Drop borrowed handles. The `Module::write_bitcode_to_*` path that triggered the bug is never invoked from Sky's path, so the bug is structurally unreachable. The B10 entry in §25.2 marks the risk **CLOSED** for the same reason.

The original investigation found an LLVM 21 `BitcodeWriter.cpp` defect (dropped `FUNCTION` declaration record + stale `INST_CALL` value index, triggered when an `extern` declaration's declared param type differed from the call-site type due to ABI coercion). The shipping workaround at the time was an in-process IR-text round-trip (`llvm_gen::roundtrip_text_to_bitcode`, ~10 LOC); that helper was retired in the Phase 4 migration that wired toylangc through `fill_module`. If Sky ever re-introduces a serialize/parse step (e.g., for cross-process module caching), the workaround pattern is recoverable from git history — file an upstream `llvm/llvm-project` bug first.

#### F.9 LLVM version pinning includes Inkwell's LLVM

§5.6.5 covers LLVM version pinning between Sky and rustc. **Add**: if
Sky's codegen uses Inkwell (or any other LLVM-binding crate that
ships bundled LLVM headers), that crate's LLVM version must match
rustc-fork's LLVM exactly. Toylang's Phase 4.5 hit a 20.1.6 vs 21.1.8
record-format mismatch when Inkwell shipped LLVM 20 but the
rustc-fork ran LLVM 21. Fixed by bumping Inkwell to its `llvm21-1`
feature variant. Sky's pin discipline expands from "rustc nightly +
its LLVM" to "rustc nightly + its LLVM + every LLVM-binding crate Sky
uses."

#### F.10 Cargo profile overrides only live at workspace root

Operational lesson surfaced during `test_lto_smoke` development.
Putting `[profile.dev] lto = "thin"` in a workspace member package's
`Cargo.toml` is **silently ignored** by cargo — only the workspace
root's `Cargo.toml` honors profile overrides. The rustc command
generated by cargo simply doesn't carry `-C lto=thin` if the override
is in the wrong file. Cargo emits no warning.

**Implication for Sky's `skyc` orchestrator**: when generating the
`.skybuild/` workspace, profile overrides (LTO settings, opt-level,
codegen-units, panic-strategy) must live in the workspace root's
generated `Cargo.toml`, not in the per-package files. Toylang's
`build.rs::write_workspace_toml` is the reference.

#### F.11 `RUSTC_WORKSPACE_WRAPPER` necessity for LTO testing

Operational debugging detail. Toylang's first attempt to test LTO
build behavior used a plain `cargo build` invocation in the generated
workspace. Result: build succeeded, binary panicked at the
`unreachable!()` stub body. Hours of investigation eventually showed
the cause: invoking cargo directly bypasses `RUSTC_WORKSPACE_WRAPPER`,
so toylang's wrapper-mode dispatch never engaged, so the facade's
patch (c) hook was never installed, so Sky's bitcode contribution
returned zero modules, so Sky's real body was never in the binary,
so the linker fell back to the stub's `unreachable!()`.

The same trap will bite any future LTO-behavior testing. The
discipline: **integration tests of LTO behavior MUST invoke through
the toylang/skyc wrapper, not directly via cargo**. Toylang's
`test_lto_smoke` runs via `toylangc build` for this reason.

Sky's tooling should fail-fast if invoked in a context where its hook
won't fire — e.g., emit a startup diagnostic if the build is
configured to use `consumer_fill_modules` but the hook isn't installed.

#### F.12 The chokepoint pattern (meta-observation for estimation)

A recurring pattern across toylang's facade refactors: items estimated
at ~weeks of work landed in ~hours when the surface area routed
through 2–5 helper functions all callers funnelled through. Migrating
the chokepoint helpers migrated every call site implicitly.

The original estimates assumed direct migration of each call site —
linear in the call-site count. The chokepoint pattern is constant in
the chokepoint count, which is often much smaller.

**Sky-relevant implication**: when scoping facade refactors, audit
for chokepoints first. If the surface routes through 2–5 helpers,
budget hours, not weeks. If the surface has many independent paths,
the original linear estimate stands. Distinguishing the two shapes
before committing to a budget is the load-bearing analytic move.

Counter-example: retiring a multi-channel discovery mechanism (toylang
`cgu_stash` retirement, which combined accessor discovery + Case 1b
generic discovery from Rust callers) needed two separate path migrations
rather than one chokepoint helper; it took the estimated ~2 days. The
pattern doesn't always apply.

#### F.13 The per_instance_mir cascade fires at the stub rlib compile, not at user-bin

The single most load-bearing empirical correction. The case 4 / case
6 worked examples in §2.6 describe rustc's collector queuing `<Widget
as Clone>::clone` "at user-bin compile" when walking Sky's main. That
elides *when* the cascade actually fires. Probing the
`case_generic_impl_block` toylang fixture revealed:

**At user-bin compile**, rustc's mono collector walks
`main.rs::main`'s body and reaches the call to Sky's `__sky_main`.
`__sky_main`'s DefId is non-local (lives in the bin's own stub rlib).
At `rustc_monomorphize/collector.rs::should_codegen_locally`, the
collector gates on:

```rust
if tcx.is_reachable_non_generic(def_id)
   || instance.upstream_monomorphization(tcx).is_some() {
    return false;  // don't mono locally; it's upstream
}
```

For `__sky_main` (non-generic, upstream), `is_reachable_non_generic`
is `true`. The collector returns `false` — it **never calls
`per_instance_mir`** for `__sky_main` at user-bin time. Sky's
synthetic body for `__sky_main` doesn't fire at user-bin. The
cascade — and therefore the discovery of `duplicate<Widget>`,
`duplicate<Wrapper<i32>>`, `<Widget as Clone>::clone`, etc. — is
**exclusively a stub-rlib-compile-time mechanism.**

This has three architectural consequences:

1. **The stub rlib's `.o` IS where rustc-emitted Rust generic
   intermediaries land.** The Sky cascade at the stub rlib walks
   Sky's bodies, surfaces ReifyFnPointer casts on Rust deps, and
   rustc emits the substituted Rust bodies into the stub rlib's
   `.o`. Post-§5.5-Step-2 (2026-06-21), Sky's own non-generic bodies
   ALSO emit at the owning crate's compile via `fill_extra_modules`,
   not exclusively at the binary as the original §5.5 framing
   implied. See §5.5 for the current split.

2. **Case 4 / case 6 monomorphization discovery is captured and
   drained at the same compile session as the cascade.** Sky's
   binary compile can't replay the cascade for non-generic upstream
   symbols. At the stub rlib's `consumer_fill_modules` callback
   (post-mono-walk window so no @GCMLZ re-entry risk), the pure
   function `collect_consumer_trait_impl_instances(tcx)` walks the
   partition for `MonoItem::Fn(instance)` entries matching
   `is_consumer_trait_impl_method` and returns a local Vec; the same
   callback drains the Vec inline and emits the bodies through
   Sky's standard codegen pipeline. The previous capture-ship-replay
   variant — which serialized the discoveries into the sidecar and
   re-pushed them at `on_sky_lib_loaded` on the consuming side — was
   retired in §5.5 Step 3 (commit b09a90b) once `AvailableExternally`
   linkage made owning-crate emission viable. §8.9.5 covers the
   current pipeline.

3. **Symbol canonicalization across crates is handled by patch 5 +
   share_generics, not by an `upstream_monomorphizations`
   augmentation.** Without help, the user-bin compile's v0 mangler
   would short-circuit at -O>=2 and pick user-bin as the
   instantiating-crate disambig for trait-impl methods (mismatch →
   link error). The shipping fix: (a) Sky's `LangDriver::config`
   heuristic forces `share_generics=true` at stub rlib compiles so
   rustc records the trait-impl monomorphizations in the rlib's
   natural rmeta-encoded `upstream_monomorphizations_for` map; (b)
   patch 5's gated escape clause in `Instance::upstream_monomorphization`
   keeps the share-generics gate from short-circuiting at user-bin
   compile so the mangler consults the natural map. The previous
   `synthesize_upstream_monomorphizations` callback + whole-map
   `upstream_monomorphizations` query override (A.2) that injected
   synthesized entries into rustc's map was retired 2026-06-21 in
   §5.5 Step 3 + the dead-code cleanup (commits b09a90b + ff0cfe8) —
   the natural map populated by share-generics is sufficient.

The corresponding insight for handoff-style decision-making: **when
debugging a "Sky's body isn't getting emitted" failure, check WHICH
compile is supposed to emit it.** Under the current model, non-generic
Sky items emit at their owning crate's compile (post-Step-2), generic
items at the binary's compile, and cascade-discovered trait-impl
methods at the stub_rlib compile where the cascade fires.

#### F.14 Approach C (per_instance_mir suppression at stub rlib) is load-bearing-against

Three approaches were considered for fixing the disambig mismatch
(stub rlib's `duplicate<Widget>` body references clone with one v0
mangler disambig; user-bin's emission of clone uses a different one):

- **A** (partition filter extension at stub rlib): drop generic
  Rust intermediaries from the stub rlib's CGUs so user-bin re-emits
  them.
- **B** (the shipped fix, F.13 above): let the cascade fire at the
  stub rlib compile and have Sky's `fill_extra_modules` emit the
  resulting bodies at that same compile session. Symbol
  canonicalization across crates is handled by share_generics=true
  at the stub rlib + patch 5 at user-bin. (The earlier
  capture-ship-replay + A.2 augmented-map variant of B was retired
  2026-06-21 — see F.14.1 and §F.13's revised consequence 3.)
- **C** (suppress `per_instance_mir` at stub rlib): return `None`
  from Sky's provider when the local crate is a stub rlib, so the
  cascade never fires there.

Approaches A and C were empirically rejected. Both share the same
failure mode: they assume the user-bin compile would re-discover the
deps via its own collector walk. **It won't.** The
`is_reachable_non_generic` gate (F.13) blocks the user-bin
collector from calling `per_instance_mir` on `__sky_main` —
suppressing the cascade at the stub rlib means the cascade fires
nowhere, deps are queued nowhere, and the binary fails to link
`duplicate<Widget>` itself (much less the clone method). Probe
confirmation: when Approach C was prototyped, the failing symbol
became `duplicate<Widget>` rather than `<Widget as Clone>::clone`.

The cascade at the stub rlib compile is therefore architecturally
**load-bearing**, not a leak. Sky's `per_instance_mir` provider must
fire there for the rest of the system to function. The "lib compiles
produce rlib + sidecar only" guidance in §5.5 reads as a rule about
*Sky-emitted bodies*, not about *rust-emitted bodies the Sky cascade
queues*. The qualifier in §5.5 makes this explicit.

##### F.14.1 Partition filter retirement (Option 4, 2026-06-21) — UNDONE 2026-06-22

**Status: REVERSED 2026-06-22.** Option 4 itself was retired together
with rustc-fork patch 5; the partition filter (~95 LOC) returned to
the facade. Reason: Option 4's `AvailableExternally` body created the
CGU-placement hazard documented in §F.17, which patch 5 had to paper
over. Restoring the partition filter eliminates the hazard structurally
(no AvailableExternally body reaches LLVM at all) and lets patch 5
retire from the fork. Net: fork 5 → 4 patches; @SBMNBIZ arcanum
becomes vacuous; one fewer load-bearing invariant for future
engineers to track. The contrast probe at
`tmp/patch5-empirical-2026-06-21/VERDICT.md` documents the empirical
case5_no_lto behavior (build links + runtime SBMNBIZ panic + co-located
CGU + main's disassembly inlining `core::panicking::panic`) that drove
the decision. The text below is preserved as the design history of
Option 4 itself for archive purposes.

---

The "A.3 partition filter" — the `collect_and_partition_mono_items`
override that removed consumer-defined items from rustc's CGU list
before LLVM codegen — was retired and replaced by a `codegen_fn_attrs`
override that sets `linkage = Some(Linkage::AvailableExternally)` on
every consumer-defined item. Net effect: rustc's LLVM backend emits
the body for cross-module inlining (LTO IR) but produces no `.o`
symbol; the consumer's `fill_extra_modules` body (rustc fork patch 4)
is the sole `.o` definition at link time.

**What this retires vs what it doesn't (initial framing — see Update below).** Option 4 itself retired only A.3 (the CGU-list filter). The initial Thread A reading assumed A.1 (capture-ship-replay) and A.2 (`synthesize_upstream_monomorphizations`) remained load-bearing under Option 4 because the cascade still fires at stub_rlib compile (F.13 — `is_reachable_non_generic` gate is rustc's own behavior, not Sky's).

The CGU-list filter: ~107 lines retired from `rustc-lang-facade/src/queries/partition.rs`, file deleted in commit a51bd7c.

**Update (also 2026-06-21, follow-on chain).** A.1.X (the cross-session capture-ship-replay drain at user_bin) and A.2 (`synthesize_upstream_monomorphizations` + `upstream_monomorphizations[_for]` query overrides) were ALSO retired in §5.5 Step 3 (commit b09a90b) plus the dead-code cleanup (commit ff0cfe8, ~251 lines deleted, including the entire `rustc-lang-facade/src/queries/upstream_monomorphization.rs` file + the `SkyUniverse.discoveries` field + `synthesize_upstream_monomorphizations` callback/trait method/vtable slot/trampoline/accessor). Cascade trait-impl discoveries now drain inline at the stub_rlib compile via the pure function `collect_consumer_trait_impl_instances(tcx)` (§8.9.5). The augmented `upstream_monomorphizations_for` map is no longer needed because share_generics=true at the stub rlib populates rustc's natural cstore-walk map for the same items.

Still load-bearing post-chain:
- **A.1.Y** (`walk_and_stash_internal_callees`): Sky's intra-process discovery of Sky-internal transitive callees so `fill_extra_modules` can emit their bodies. Has nothing to do with rustc's mono walker. STAYS — this is the correct architectural place per the locked principle.
- **Patch 5** (`consumer_lang_active` gated CGU-placement escape, fork-side): historically described here as "STAYS at 5 patches" because retirement was blocked under Option 4's AvailableExternally architecture. **Update 2026-06-22:** patch 5 RETIRED jointly with Option 4 — restoring the partition filter eliminated the CGU-placement hazard structurally. Fork is now 4 patches; see §3.2 / §F.17 retirement note.
- **Capture itself** (`collect_consumer_trait_impl_instances`): still runs at stub_rlib compile, but its consumer changed from "ship to user_bin via sidecar" to "inline-drain at same session."

**Why this preserves F1's LTO inlining promise.** `AvailableExternally`
means the body IS in the IR pool, just not in the `.o`. LTO's IR
linker can still inline through it. The actual body LTO sees is the
one the consumer's `fill_extra_modules` hook contributed (single-symbol
architecture per §F.2). LLVM's IR linker unambiguously prefers the
`External`-linkage real body (emitted by `fill_extra_modules`) over the
stub's `AvailableExternally` `unreachable!()` body whenever both end up
in the same LTO pool, so the disambiguation works without any LTO-pool
exclusion. (An earlier belt-and-suspenders `#![no_builtins]` layer on
stub rlibs was retired in the Round 2 E2 verification — see §F.3.) The
inlining test matrix (post-Tier-1-expansion: ~190 fixtures across the
7-case taxonomy × 4 LTO modes × opt-level sweep × codegen-units
variants, at `toylangc/tests/integration_projects/inlining/`) verified
post-Option 4 that **at thin/fat LTO**, case 1a / 2 / 4 / 6 main
bodies still constant-fold (`mov w8, #42`) rather than tail-jumping to
a Sky symbol. **At `lto = false` (cargo dev default) post-§5.5-Step-2,
Sky-top cases tail-jump** (Sky's body lives in a different rustc
invocation; thin-local LTO §F.16 can't bridge). Rust-top cases still
inline at `lto = false` via thin-local LTO within user_bin's
invocation. See §5.5's "Trade-off the split introduced" paragraph for
the user-facing implication.

**Interaction with the F2 / B16 `cross_crate_inlinable` override.** The
two overrides target opposite directions and don't conflict. F2 forces
`cross_crate_inlinable` to false in consumer-active compiles so rustc
emits real `.o` symbols for items Sky's emitted call sites reference
(e.g. `<Vec<i64>>::new` for case 5). Option 4 sets explicit
`AvailableExternally` linkage on consumer-defined items so rustc
emits no `.o` for items Sky's `fill_extra_modules` will emit. F2's
`cross_crate_inlinable` override gates on `crate::is_sky_active(tcx)`
(an inline marker-walk helper, post-patch-5-Sky-cleanup; see commit
605fd2b); Option 4's `codegen_fn_attrs` override gates on
`is_consumer_codegen_target(tcx, def_id)`. Both
preserve byte-identical pass-through for pure-Rust crates.

**What was empirically tested.** At the time of the Option 4
verification, all 191 integration tests + the 40 inlining matrix
fixtures + 106 unit tests passed cold and warm. (Test totals drift as
the suite grows — see CLAUDE.md for current counts; the historical
post-Option-4 figures are recorded here as the verification baseline.) Open-question check: the MIR inliner does not pull the
`unreachable!()` body into Sky-defined callers within the stub_rlib
compile — no `udf`/`brk` instructions appear in any matrix-fixture's
main; Sky's body inlines through cleanly. The risk discussed in the
handoff's "Open question to validate" did not materialize.

A one-time cache-shape gotcha hit during the migration: rustc's
incremental cache rejects post-Option-4 `codegen_fn_attrs` results
with `Found unstable fingerprints` when the prior build's cache had
the un-overridden value. Mitigation: wipe the
`target/integration-projects-cache` directory once after pulling
Option 4 in. Subsequent runs are stable (`CARGO_INCREMENTAL=0` in the
three test helpers from the F2-era 11-failures fix keeps it solid
across cold/warm).

#### F.15 Patch 4 design history: from bytes-in to Approach B

**Status: Approach B is the shipping patch 4 design.** This subsection now reads as design history; the architectural conclusions below were locked in during the Phase 2 fork-patch rewrite and are reflected in §3.2 + §B.4 + §C.4.

---

The original v1 shape of patch 4 had the hook signature

```rust
fn extra_modules<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Vec<ModuleCodegen<Self::Module>>;
```

implemented by Sky's facade emitting LLVM IR via Inkwell, serializing
to bitcode bytes, calling `ModuleLlvm::parse_from_tcx(tcx, name, buffer)`,
and returning the parsed `ModuleLlvm`s. This worked (toylang shipped
it through Phase 4.5) but the bitcode round-trip was **interface-laziness in the patch
design, not a fundamental architectural requirement.**

The right question is: "why are we serializing an in-memory data
structure into bytes and immediately deserializing it back inside
the same process?" The honest answer: because patch 4's bytes-in
interface was the smallest possible patch that worked, and bytes are
ABI-trivial in a way that raw context/module pointers aren't.

**Sky is not migrating between LLVM contexts.** Rustc runs one
`LLVMContext` per CGU; contexts are isolated by design. Sky's
Inkwell-built module lives in Sky's own context; rustc's
`ModuleLlvm`s live in rustc-created contexts. Both are functionally
equivalent — "a per-CGU context that owns a per-CGU module." Sky's
should just be "another CGU." The serialization is solving an
*interface* problem (patch 4 doesn't accept raw pointers) rather
than a *substantive* problem (contexts that need merging).

**Two replacement designs:**

| | **Approach A (Sky owns, transfers)** | **Approach B (rustc owns, lends)** ★ recommended |
|---|---|---|
| Who creates `LLVMContext` | Sky (via Inkwell) | rustc |
| Who creates `TargetMachine` | Sky (must mirror rustc's settings) | rustc (inherited automatically) |
| Inkwell API needed | `Context::into_raw` + `Module::into_raw` (ownership transfer) | `Context::new` wrapped in `ManuallyDrop` + `Module::new_borrowed` (suppress-Drop wrappers) |
| `mem::forget` dance | yes | no |
| Risk of leaked Inkwell-internal state | yes (skips Inkwell's Drop) | no |
| Target-attr skew (B9) | possible (Sky configures independently) | impossible (inherited) |
| Hook shape | `Vec<ModuleLlvm>` return (unchanged) | `fn(allocator: &mut ExtraModuleAllocator)` (callback) |
| Inkwell upstream PR difficulty | hard (ownership transfer out of Inkwell) | conservative (read-only borrowed wrapper) |
| Fits rustc's lifecycle | awkward | natural |

**Approach B wins on every quality axis except patch size.** Its
patch surface is moderately larger (rustc has to define an allocator
trait and pass it through the codegen lifecycle) but the design is
architecturally correct.

**The recommended endgame path:**

**What actually shipped:**

1. **Vendor Inkwell + patch locally.** Rather than waiting for an
   upstream Inkwell PR, `vendor/inkwell/src/module.rs` got
   `Module::new_borrowed(LLVMModuleRef) -> ManuallyDrop<Module<'ctx>>`
   added in place (~5 LOC), and the workspace `Cargo.toml` carries a
   `[patch."https://github.com/TheDan64/inkwell"]` override pointing at
   the vendored tree. `Context::new` already existed in Inkwell as an
   `unsafe fn(LLVMContextRef) -> Self` constructor, so wrapping in
   `ManuallyDrop` at the call site sufficed — no separate
   `Context::from_raw_borrowed` was needed.
2. **Patch 4 rev 2.** The hook signature became
   `fill_extra_modules(&self, tcx, allocator: &mut dyn ExtraModuleAllocator<Self::Module>)`,
   with a companion `allocate_extra_module(&self, tcx, name) -> Self::Module`
   constructor method, an `ExtraModuleAllocator<M>` trait, and a generic
   `VecAllocator<'a, M, F>` driver. `rustc_codegen_llvm` exposes
   `ModuleLlvm::llcx_raw_mut()` + `llmod_raw()` as
   `*mut c_void` FFI bridges. The bitcode-bytes shape (`extra_modules`,
   `set_extra_modules_hook`, `parse_from_tcx`) was retired entirely;
   no backward-compatibility surface remains.
3. **Toylangc consumes the borrowed `ModuleLlvm`** through
   `llvm_gen::fill_module`. Inkwell wrappers
   (`ManuallyDrop<Context>` + `Module::new_borrowed`) suppress Drop;
   rustc retains ownership of the LLVM resources throughout.

**Architectural improvements that landed:**

- **B9 (LLVM-binding version skew):** **CLOSED.** No Sky-side
  TargetMachine configuration exists to drift from rustc's.
- **B10 (LLVM 21 BitcodeWriter bug):** **CLOSED.** No bitcode
  serialization happens in Sky's path; the bug is unreachable.
- **B11 (round-trip scaling cost):** **CLOSED.** No round-trip occurs.
- **The bitcode-writer canonicalization side effect** that previously
  masked B10 is gone. The upstream LLVM fix becomes a Sky-irrelevant
  cleanup rather than a Sky-blocking item.

The earlier guidance — *"treat bytes-as-interface as a v1 placeholder,
not a stable endpoint"* — was followed: the v1 placeholder did its
job during Phase 4.5 (proving the patch-4 + LTO pipeline worked at
all), then retired cleanly when Approach B landed.

#### F.16 Rustc's "thin-local LTO" between CGUs in a single invocation

A previously-undocumented mechanism that's load-bearing for the
"cross-Sky/Rust inlining works without LTO" perf property the inlining
matrix has been testing since F1: **rustc runs LLVM's ThinLTO BETWEEN
its own CGUs within a single rustc invocation, even when the user sets
`lto = false`.** Cargo's documentation calls this "thin-local LTO":

> `lto = false`: Performs "thin local LTO" which performs "thin" LTO
> over the local crate's codegen units. No LTO is performed if codegen
> units is 1 or opt-level is 0.

Cargo's dev profile default is `lto = false`, so thin-local LTO is ON
by default. Only an explicit `lto = "off"` disables it (along with all
other LTO levels).

**Discovery (2026-06-21).** The §5.5 Step 2 investigation surfaced this
when attempting to retire A.1.X (capture-ship-replay) for non-generic
trait impls. The inlining matrix's `_no_lto` fixtures asserted that
the bin's `main` constant-folds Sky's body — a property nominally
impossible without LTO at the cargo-visible layer. A `RUSTFLAGS="-C
save-temps"` build of `case4_no_lto` on the pre-Step-2 baseline
showed:

- Sky's CGU (`case4_no_lto-<hash>.<sky-cgu-id>.no-opt.bc`) contains
  the real body of `__toylang_main` with `External` linkage (emitted
  via patch 4's `fill_extra_modules`).
- The bin's main CGU's pre-opt IR has only a `declare void @__toylang_main`
  (no body).
- The bin's main CGU's `thin-lto-after-import.bc` contains
  `define available_externally void @__toylang_main()` with Sky's
  real body imported in. **This is the magic moment.**
- The bin's main CGU's post-pass-manager IR shows `case4_no_lto::main`
  constant-folded to `mov w8, #42` (Sky's body inlined and folded).

The file naming (`thin-lto-*`) makes the mechanism visible at build
time. The matrix's `_no_lto` fixtures were testing this thin-local
LTO mechanism, not pure non-LTO behavior.

**Why this matters for §5.5 Step 2 and beyond.** Thin-local LTO
operates over the CGUs of a **single rustc invocation**. CGUs in
DIFFERENT rustc invocations (= different crates) are isolated; the
import pass cannot reach them. Under pre-Step-2 architecture, Sky's
body was contributed via fill_extra_modules at the bin's user_bin
invocation, so Sky's CGU and the bin's main CGU were peers in the
same invocation — thin-local LTO bridged them. Under Step 2's
narrower-§5.5 (non-generic Sky bodies move to upstream stub_rlib's
invocation), Sky's body moves to a different rustc invocation —
thin-local LTO no longer reaches it.

**Step 2 trade-off, codified.** Under §5.5 Step 2, **cross-Sky/Rust
body inlining requires explicit cross-crate LTO** (`lto = "thin"` or
`lto = "fat"`). At `lto = false` (cargo dev profile default),
cross-crate calls cross the language boundary via real tail-jumps
(`b __lang_stubs::__toylang_main`). The 8 inlining matrix `_no_lto`
fixtures were updated to assert this new behavior (tail-jump present)
rather than the pre-Step-2 behavior (no tail-jump because of thin-local
LTO inlining).

**Five levels of inlining, summarized.** For future investigators
debugging "why isn't this inlining" questions:

| Level | Mechanism | Spans | Enabled at | Empirical 2026-06-24 |
|---|---|---|---|---|
| 1 | rustc MIR inliner | one item | `mir-opt-level >= 1` (default at -O>=1) | not directly measured |
| 2 | LLVM intra-CGU inliner | one CGU | -O>=1 | not directly measured |
| 3 | rustc-internal thin-local LTO between CGUs | CGUs of one rustc invocation | -O>=1 + `codegen-units > 1` + `lto != "off"` (where "off" is rustc's `-C lto=off`; cargo's `lto = false` is NOT `lto = "off"` — see paragraph below) | not measured in isolation; bench fixtures span multiple invocations so Level 3 alone is invisible |
| 4 | Cross-crate ThinLTO | all rlibs | `lto = "thin"` | Bench 1: 1.50× speedup; Bench 2 K=100: 1.48×; Bench 3 drop chain: **25.5×** (Sky) / **27.5×** (pure-Rust cross-crate baseline — inherited, not Sky-specific; see §22.4) |
| 5 | Cross-crate FatLTO | all rlibs | `lto = "fat"` | Bench 1: ~1.49× (~similar to Level 4); .text size smaller than thin (Bench 1 fat: 218KB vs thin: 239KB) but 2× symbol count |

Cargo's `lto = false` ≠ `lto = "off"`: `false` (dev default) leaves
Level 3 ON; `"off"` (explicit) disables it.

The user-facing promise post-Step-2 is: **for cross-Sky/Rust
inlining, use `lto = "thin"` or `"fat"`. At `lto = false`, Sky calls
cross the boundary via tail-jumps.** Most production builds opt into
LTO anyway; the cost is felt only at dev builds, where compile time
typically matters more than runtime perf.

This finding is also surfaced as @SBMNBIZ in §26.17 (the AvailableExternally
stub body must never be the inlinable winner, regardless of which level
of inlining is active).

#### F.17 What patch 5 actually does — empirical CGU-placement finding (2026-06-21) — RETIRED 2026-06-22

**Status: design history.** The empirical CGU-placement finding documented here is what motivated the joint retirement of Option 4 + patch 5 on 2026-06-22. With the partition filter restored and the AvailableExternally body no longer reaching LLVM, the CGU-placement hazard described below cannot fire. The text is retained because (a) the original §F.17 was overclaimed without archived evidence (per the prior author's reply at `tmp/patch5-empirical-2026-06-21/REPLY-to-reader.md`), (b) the 2026-06-22 contrast probe at `tmp/patch5-empirical-2026-06-21/VERDICT.md` validated the finding empirically, and (c) the CGU-placement mechanism is worth knowing about should a future engineer attempt to reintroduce AvailableExternally for any reason.

---

A `-C save-temps` probe of `case5_no_lto` with patch 5's `consumer_lang_active` override disabled revealed that patch 5's actual mechanism is **NOT** what the original B14 / §3.2 narrative describes. The investigation was prompted by a question: "if share_generics=off makes downstream re-monomorphize locally with LOCAL_CRATE naming, internally consistent, why does Sky need patch 5?"

**The empirical answer.** The mangled symbol names in the final binary are **byte-identical** with patch 5 ON vs OFF — `llvm-objdump -t` confirms both binaries have `__RINv...___lang_stubs12store_in_vec...case5_no_lto` (g F, global function) as the symbol, with Sky's real body present. Patch 5 doesn't change naming.

What patch 5 changes is **CGU partitioning at user_bin compile**:

| | Patch 5 OFF | Patch 5 ON |
|---|---|---|
| Bin's main CGU contains | `main` body + `define available_externally ... { panic(...) }` (rustc's mono of the stub source's `unreachable!()`) | `main` body + `declare ... store_in_vec` (no body, just a declaration) |
| Separate CGU contains | (nothing related) | `define available_externally ... { panic(...) }` |
| Sky's CGU contains | `define ... { real body }` (External linkage) | Same — Sky's real body |
| Binary runtime behavior | Panics at `unreachable!()` (SBMNBIZ violation) | Prints `3` (correct) |

The mechanism: `Instance::upstream_monomorphization` is consulted by both the v0 mangler AND **the partitioner**. When patch 5's gated escape fires, `upstream_monomorphization` returns `Some(__lang_stubs)`; the partitioner reads "upstream owns this" and places the AvailableExternally body in a separate CGU from local items (including `main`). When the escape doesn't fire, the return value is `None`; the partitioner treats the item as local and co-locates its body with other local items.

LLVM's intra-CGU inliner at -O3 aggressively inlines `AvailableExternally` bodies that are co-located in the same CGU as their callers. With the `unreachable!()` body co-located with `main`, the inliner pulls the `panic(...)` directly into main — runtime SBMNBIZ. With the body in a separate CGU, the intra-CGU inliner can't reach it, and the LTO import + IR-linker-resolve step (Level 3 from §F.16) correctly picks External (Sky's real body) over AvailableExternally.

**Why this isn't fixable with `InlineAttr::Never` on the AvailableExternally body.** Investigated empirically (3 attempts):

1. Set `inline = InlineAttr::Never` on both `lang_codegen_fn_attrs` (local) and `lang_extern_codegen_fn_attrs` (extern) for consumer items. Result: 13 inlining matrix fixtures regress (case1a/2/3/4/6 at thin/fat LTO + case3 at no_lto + case4 codegen-units variants). `inline=Never` blocks downstream from inlining Sky's real body too.
2. Same change on extern only (not local). Same 13 failures — the extern override fires at downstream when generics are mono'd, blocking Sky's body from being inlined into Rust callers.
3. Gate the extern's `inline=Never` on `tcx.generics_of(def_id).count() > 0` (only generic items). 10 of 13 recover, but 3 case3 fixtures still fail — case3's bin main is expected to inline Sky's generic `clone_it::<MyCounter>` body; my gate blocks that.

**Fundamental conflict:** case5 needs the AvailableExternally `unreachable!()` body to NOT be inlinable into co-located callers (SBMNBIZ). case3 needs the SAME logical function's External (Sky's real) body TO be inlinable into co-located callers (perf inlining assertion). Both share the same mangled name. `InlineAttr::Never` is a function-as-symbol attribute — it can't distinguish "stub body" from "real body." Function-attribute approaches cannot replicate patch 5's CGU-placement protection.

**Implication for documentation.** B14, §3.2 patch 5, and §B.5 historically frame patch 5 as "v0 mangler disambig escape." That framing describes one of two consumers of `Instance::upstream_monomorphization`'s return value (the mangler), but it's not the load-bearing one for correctness. A more accurate name is **"gated CGU-placement escape via the upstream_monomorphization return value."** Doc updates in §3.2, §B.5, B14 (2026-06-21) reference this section.

**Implication for retirement.** The remaining theoretically-viable paths to retire patch 5:

- **Force `share_generics=true` at user_bin** — would make the gate's first condition false, the gate's whole check would never fire, AvailableExternally would never enter the picture for shared generic Instances. Global rustc behavior change with latent-bug risk in pure-Rust deps that haven't been tested at share_generics=true at release.
- **Custom partitioner hook** — a new fork patch that directly controls CGU placement of AvailableExternally consumer items, without going through `upstream_monomorphization`. Larger fork surface; net-negative against the goal of fewer patches.
- **Don't emit the AvailableExternally body at all at downstream** — requires a different `codegen_fn_attrs` mechanism that suppresses emission rather than stamping linkage. Probably requires a new rustc query.

None of these is clearly more attractive than keeping patch 5. The retirement arc is closed for the foreseeable future; patch 5 stays.

**Side observation:** patch 5's gated escape clause condition `(consumer_lang_active && map.contains_key)` requires `contains_key` to be true for the escape to actually change behavior. For the case5 probe, `contains_key(store_in_vec<i32>)` must be true at user_bin compile time — but `__lang_stubs.rlib`'s `.o` files don't contain a `store_in_vec<i32>` symbol (verified via `ar x` + `llvm-objdump -t`), implying __lang_stubs's compile didn't mono it. So the natural `upstream_monomorphizations_for` map gets its entry from somewhere else — possibly rustc's local mono at user_bin populating the map mid-compile, possibly interactions between Option 4's AvailableExternally stamping and rustc's rmeta-encoded share_generics tracking. Pinning down the exact source of the natural-map entry was not done in this investigation but is the next probe if someone re-opens the retirement question.

#### F.18 Phase E: mir_shims elimination + AST-rewrite drop synthesis (2026-06-23)

The implementation arc for Decision 1's "drop is just a function"
principle. Three sub-phases, four key empirical findings, one
significant principle-honoring refactor mid-flight.

**Empirical baseline (Phase A).** Toylang had zero drop tests before
this session. Building one (Fixture 1: `Vec<Widget>` where Widget has
`impl Drop` that prints `self.id`) and instrumenting along the way
surfaced 10 findings; the headline (A.6) was that the previous
`mir_shims` override was not just vestigial — it was **silently
broken**. Even with a properly emitted `impl Drop for Widget` block
in the stub source, the override's `consumer_struct_name` lookup
path never fired. `drop_in_place::<Widget>`'s body was a no-op
(`sub sp, sp, #0x10; str x0, [sp, #0x8]; add sp, sp, #0x10; ret` —
just stack save/restore), and `<Widget as Drop>::drop` was absent
from the binary entirely. The previous machinery had never worked in
any shipping fixture. mir_shims removal lost zero functionality.

Other Phase A findings worth remembering:
- Toylang's parser rejected `&mut self` — Drop's receiver requires it. Extended `parse_impl_method` to accept the `mut` keyword after `&` and plumb `is_self_mut: bool` to `ToyImplMethod` (serde-default for back-compat).
- `oracle::find_trait_impl_method_def_id` requires the trait to be `use`-imported in toylang source (or `pub use`-re-exported by stub_gen).
- The cascade-discovery path (`collect_consumer_trait_impl_instances`) already captures `<Widget as Drop>::drop` correctly when the partition contains it — same path as case4's Clone. Sky's existing trait-impl-method emission machinery handles Drop with zero changes.

**Mid-flight refactor (Phase E.b → E.c → E.d).** The first two
implementation attempts emitted drop calls at the LLVM-IR layer
(`emit_scope_drops` / `emit_drop_in_place` / `emit_sky_struct_drop`
in llvm_gen, plus a `walk_typed_body_for_drop_types` walker in
callbacks_impl and dedicated drop dep arms in
`collect_rust_deps_recursive`). Worked, but leaked drop-specificness
across four sites in the pipeline — every site had a small "is this
the Drop call site? do something special" branch. Phase E.d
refactored to a single AST-rewrite pass: `insert_scope_end_drops`
appends synthetic `TypedExprKind::StaticCall { ty: "Drop", method:
"drop", args: [Ref(local)] }` AST nodes at scope-end positions; the
existing dep walker, codegen, and cascade-discovery machinery handle
them identically to any other trait static call.

**Specialness audit, principle adherence ~95%.** Drop-aware code
post-refactor lives in ONE site: `insert_scope_end_drops` (~30 lines)
plus `local_needs_scope_drop` predicate (~25 lines) plus
`synth_scope_drop_call` AST builder (~20 lines) plus one
`pub use core::ops::Drop` line in stub_gen. After the synth pass,
every downstream stage treats drop calls as ordinary `StaticCall`
AST nodes. No drop-specific paths in: dep walker, mono cascade,
per_instance_mir, codegen, symbol resolution, link.

The remaining 5% leak: the predicate hardcodes the trait name "Drop"
(and method name "drop") at the registry lookup. Could be generalized
to a marker-trait registry but Drop is the only such trait Sky needs
today.

**Two surprises caught during implementation.**

1. **`codegen_extern_wrapper` had unconditional `module.add_function`.**
   When the body's call site (`__toylang_internal_main`'s
   synthesized `Drop::drop` call) declared the extern symbol via
   `declare_external_fn` (which dedups), and then the cascade-drain
   later ran `codegen_extern_wrapper` for the same Instance, the
   unconditional add produced a SECOND function with the same name.
   LLVM disambiguated with a `.1` suffix; the call site bound to the
   bare name; linker failed with "undefined symbol". Fix: switch to
   `module.get_function(symbol).unwrap_or_else(|| module.add_function(...))`.
   This fix is RETAINED post-refactor — it covers any future case
   where call-site declarations precede body codegen.

2. **`tcx.adt_destructor(adt_def.did())` is load-bearing in the predicate.**
   Without it, the synth pass synthesized `Drop::drop` calls for
   `Option`/`Result`/`Stdout`/primitives whose drop semantics flow
   through auto-generated DropGlue with no trait-method symbol. Rustc's
   mono collector ICEs ("failed to resolve instance for
   `<Option<i32> as Drop>::drop`"). The query returns `Some(_)` iff
   the ADT has an explicit `impl Drop`; the predicate gates on this.
   Tests like `option_unwrap_basic`, `result_unwrap_basic`,
   `stdout_call` all surfaced this immediately when the broad
   predicate first ran without the filter.

**Two gotchas worth documenting.**

1. **Both dep-collection and codegen sites must run the synthesis pass.**
   Initially `type_resolve_body` (the dep-collection site) ran it; the
   codegen site (`codegen_internal_function`) called `resolve_fn_body`
   directly. Result: drops were queued as deps (rustc mono'd them)
   but the bodies of Sky functions didn't actually emit the calls
   (synth nodes weren't in the typed body codegen saw). The
   dep-queue / emitted-body disagreement surfaces as "binary builds
   and runs cleanly but the Drop body never fires." Fix: call
   `insert_scope_end_drops` at both sites with identical inputs.

2. **`pub use core::ops::Drop;` must be gated on absence.** Fixtures
   that explicitly `use std::ops::Drop` in toylang source surface as
   `pub use std::ops::Drop;` in the stub rlib. Adding a second
   unconditional `pub use core::ops::Drop;` produces an `E0252`
   ("the name `Drop` is defined multiple times") error. The gate
   walks `registry.imports` looking for any path whose last segment
   is "Drop" and skips the unconditional re-export when found.

**Move-semantics handling.** The synthesis pass skips entirely when
the function has a non-void return type. Functions returning a
Vec/Widget/etc. don't get scope-end drops — the caller's let-binding
takes ownership via move semantics and will eventually drop. This is
conservative; could be tightened later by tracking which locals the
return expression moves out. For void-returning functions, all
tracked locals drop in LIFO order at scope end.

**Test inventory.** 9 drop fixtures landed under
`toylangc/tests/integration_projects/drop/`:
- Fixture 1: `Vec<Widget>` with Widget's `impl Drop` — basic chain (Vec drop iterates, calls Widget drop per element).
- Fixture 2: two `Vec<Widget>` locals — LIFO scope-drop order across multiple locals.
- Fixture 3: `Vec<Widget>` in a void-returning helper fn — drop fires at helper exit.
- Fixture 4: empty `Vec<Widget>` — Vec deallocs but Widget's Drop body never fires.
- Fixture 5: `Vec<Widget>` where Widget has NO `impl Drop` — Vec drops cleanly, no Widget body calls.
- Fixture 6: single bare `Widget` local with `impl Drop` — direct trait dispatch path.
- Fixture 7: three Sky-struct locals with Drop — LIFO ordering for the direct-dispatch path.
- Fixture 8: bare Sky-struct local WITHOUT `impl Drop` — predicate skips, no spurious calls.
- Fixture 9: Sky-struct local in helper fn — drop fires at helper exit.

Plus 333 existing integration tests continue to pass cold under the
new model. Total: 342 / 0 / 1 ignored.

**Net diff.** 9 files modified, 2 deleted (`drop_glue.rs`,
`mir_helpers.rs`), ~640 insertions / ~33 deletions in the
implementation; +9 drop fixtures (~200 lines of toylang source +
toml + expected output).

**What stayed deferred.** The `__sky_drop_X<T>` bridge function
from Decision 1's original plan was not implemented. The direct
`Drop::drop` dispatch is functionally equivalent for the non-comptime
case (toylang has no comptime), and the bridge's primary value
(sharing one drop fn across content-hash-keyed `SkyOpaqueType<T>`
variants) is a v2 concern when Sky comptime lands. Also deferred:
linear-type compile-time error diagnostic spans, the `project_raw_field`
CI fence (not needed since the synth pass never directly accesses
field storage), and async-fn drop wiring (gated on async support
landing in toylang).

**Two follow-up codegen-quality findings (surfaced 2026-06-24 while
scaffolding the perf bench).**

*F3 — Sky `main` + tight allocation loop stack-overflows at scale.*
A Sky-main fixture allocating Widget temporaries inside a `while` loop
stack-overflows at ~1M iterations even with a 64MB stack. The math
(1M × sizeof(Widget) ≈ 4MB) doesn't account for the failure; the
likely cause is toylangc's O0 codegen emitting a fresh `alloca` per
loop-body let-binding without hoisting to the function's entry block.
LLVM convention is "alloca only at entry"; toylangc may violate this.
Bench 3 was restructured to drive the loop from a Rust caller to
sidestep the issue. Architectural impact: zero (this is a toylangc
codegen-quality bug). Implementation impact: any user-written Sky
program with deep loops over local allocating expressions will fail
in the same way. Concrete fix path: audit `lower_typed_expr`'s
`StructConstruction` arm in `toylangc/src/llvm_gen.rs` for unhoisted
allocas.

*B10 residual under ThinLTO cross-CGU import.* §25.2 B10 claimed
"CLOSED architecturally by Approach B" because Sky's primary path no
longer serializes bitcode. The bench surfaced that ThinLTO's internal
import phase still encodes/decodes bitcode and re-triggers the bug
under a narrow shape: Sky `main` + `Vec<SkyStructWithDrop>` + opt-level
≥ 1. The Phase E AST-rewrite drop synthesis emits ABI-coerced extern
call sites that match the original B10 trigger pattern. §25.2 B10 was
tightened to document the residual. Concrete fix path: align Sky's
emitted `Drop::drop` extern declaration's signature exactly with the
call-site type so no ABI coercion occurs in IR. Until then, Sky-main
+ Vec-of-Sky-drop fixtures should be restructured with Rust callers,
as Bench 3 does.

This is the master design document for Sky's compiler & Rust interop architecture. Total length: ~30 chapters, 6 appendices, approximately 100 pages.

The decisions herein are the product of a long design conversation, archived in commit history and across the `course-correct.md` / `future-architecture-investigations.md` summaries kept at the repo root.

The implementation of Sky's compiler is anticipated to take 50-80 weeks for a focused engineer. The phasing in Section 28 lays out the recommended order. Sky 1.0 is targeted as the first stable release; pre-1.0 versions are pre-release with breaking changes allowed between minor versions.

Sky is a long-term project. The architecture is designed for evolution; decisions trade short-term complexity for long-term correctness. The fork is sustainable indefinitely; upstreaming is pursued as background work.

Welcome to Sky.

— Document version 0.1.0

