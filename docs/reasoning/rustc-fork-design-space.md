# Rustc Fork Design Space

This document records the *why* behind the rustc-lang-facade's fork
patches: specifically, why the architecture interleaves with rustc's
monomorphization phase at all, which of the current choices were
design-space picks vs. technical necessities, and which zero-fork
alternatives exist but have not been implemented.

Written after a 2026-04-16 investigation triggered by a reviewer
(working on Vale's interop story) asking whether the 5-patch fork
could be reduced to zero. The investigation surfaced that several
alternatives were never seriously evaluated, and that the existing
architecture guide's framing conflated "what we picked" with "what
was necessary." This doc keeps the honest version so the next
person evaluating the design doesn't have to re-derive it.

---

## Part 1: Why interleave with rustc's monomorphization phase

This is the *foundational* question — the reason the facade exists
inside rustc's query providers at all rather than as a pre-pass or
post-pass tool. The one-sentence answer:

> Monomorphization is where rustc is actively discovering
> transitively-reachable generic instantiations, and the facade needs
> to inject the consumer's concrete type-argument tuples into that
> walk.

**For the full argument — including a seven-case taxonomy of consumer
architectures showing precisely which cases pre-pass can handle and
which force interleaving, with complete code examples for each, and
the generic-method-on-generic-trait case that kills the last
over-approximation workaround — see
`docs/reasoning/why-interleaved-monomorphization.md`.**
This reasoning doc takes the "interleaving is required" conclusion
as given from here; the rest of this doc is about *which rustc-side
mechanism* implements interleaving (specifically, whether the current
forked `per_instance_mir` query can be replaced by sanctioned
extension points).

The shortest useful summary for readers who just need the setup to
follow Parts 2–4:

- Rustc's monomorphization collector is the only entity with
  visibility into both Rust source *and* (via the facade) consumer
  source. The collector's walk is the one place where the full
  reachable set of generic instantiations gets discovered.
- The facade's job is to **tell rustc the leaves** — the concrete
  type-argument tuples for every Rust item called directly from
  consumer code. Rustc walks the transitive closure from there
  (trait resolution, associated types, nested generics, drop glue).
- The alternative — the facade reimplementing trait impl candidate
  resolution, associated-type projection, blanket impls,
  specialization, etc. — is tens of thousands of lines of rustc
  internals reimplemented, and over-approximation doesn't rescue
  the pre-pass approach once you include generic methods on generic
  traits.
- Pre-monomorphization (`Callbacks::after_analysis`) lacks rustc's
  concrete-arg machinery; post-monomorphization (pure
  `CodegenBackend` plugin receiving CGUs) arrives after the walk
  is complete. Either position loses the cases where Rust code
  originates the concrete instantiation of a consumer item.

These timing constraints are why monomorphization is the one correct
phase for the handoff. *What* hook fires during monomorphization is
separately negotiable — Parts 2–4 explore that design space.

---

## Part 2: Why `per_instance_mir` is a new query, not an override

The architecture guide previously implied `per_instance_mir` was a
new query "because existing queries are DefId-keyed" while
`per_instance_mir` is Instance-keyed. That framing under-specifies the
choice. The honest picture is more nuanced.

### What's actually true about the keying

- **`per_instance_mir`** (our fork patch) is **Instance-keyed**. The
  provider receives a concrete `Instance<'tcx>` and returns a body with
  type args already substituted.
- **`optimized_mir`** (rustc's existing query) is **DefId-keyed**. The
  provider receives a DefId and returns a generic body. Substitution
  per-Instance happens *during the collector's walk* via rustc's normal
  substitution machinery — the same machinery that handles every
  generic Rust function in every crate.

Both queries result in the collector walking a per-Instance body and
queueing transitively-reachable items. The difference is who does the
substitution:

- `per_instance_mir`: the consumer does it, before returning the body.
- `optimized_mir`: rustc does it, as it walks.

### Why a new query was picked

**A new query was picked for taste/pragmatic reasons, not technical
necessity.** At the time the architecture was designed, "introduce a
new query that the consumer owns" felt cleaner than "hijack rustc's
existing `optimized_mir` query and return synthetic bodies for
consumer DefIds." The former localizes the consumer's hook; the
latter means the consumer intercepts a query that rustc's normal MIR
pipeline also uses, which felt riskier.

In retrospect, using `optimized_mir` would have been arguably
*simpler* — it means we stop doing our own pre-substitution and rely
on rustc's collector-time substitution, which is code we'd be
inheriting for free rather than maintaining ourselves. The taste
preference went the wrong way. For toylang the cost is small (a fork
patch); for a consumer with a zero-fork target the cost compounds.

### What an override actually handles — and doesn't

`override_queries` on `optimized_mir` is **sufficient for dependency
discovery** — the collector's walk through our returned body, with
`ReifyFnPointer` statements substituted per-Instance, produces the
same `rust_deps` graph that `per_instance_mir` produces today. Every
case works: trait-generic callbacks, nested generics, consumer types
flowing into Rust generics, drop glue. See Part 1's worked example.

It is **not sufficient for emission control**. Rustc will still try
to compile whatever body `optimized_mir` returns at whatever symbol
`symbol_name` assigns. The trampoline body emitted by rustc collides
at the linker level with the consumer's separately-emitted real
implementation. One of the two has to be skipped, and `optimized_mir`
override alone has no mechanism to do that.

This is what current fork patch 3 solves: it teaches codegen to skip
`codegen_instance` when `per_instance_mir` returned `Some`. Without
that skip, rustc would emit a trampoline function at the consumer's
symbol, and the consumer's backend would emit a conflicting real
function at the same symbol.

**A zero-fork path therefore needs `override_queries` plus one of:**

- Keep a single fork patch equivalent to patch 3 (codegen skip). Goes
  from 5 patches to 1, not 0.
- Be a `CodegenBackend` plugin (`-Zcodegen-backend=consumer`). The
  consumer controls what gets emitted; it simply declines to emit the
  stub body while separately emitting the real implementation.
  Fork-free, but shifts the architectural complexity into backend
  integration (see §4.2).

Not sufficient on its own:

- `override_queries` alone: dep discovery works, emission conflicts.
- `CodegenBackend` plugin alone: emission works, dep discovery fails
  — the backend receives CGUs after the collector walk, so any edge
  that was invisible during the walk is already lost.

The interesting zero-fork combination is *both*, not either.

### What `per_instance_mir` actually carries

Implementation detail worth pinning down, since it informs the
"could this be `optimized_mir` instead" question:

The body returned by `per_instance_mir` has the same structural shape
across all instances of a generic consumer function. Only two things
vary per-Instance:

1. **The substituted function signature.** Parameter types and return
   type after `tcx.fn_sig(def_id).instantiate(tcx, instance.args)`.
2. **The list of `ReifyFnPointer` casts.** Each consumer instance
   discovers a different set of Rust deps during the deep walk (e.g.,
   `use_it<i32>` needs `Vec::new<i32, Global>`, `use_it<i64>` needs
   `Vec::new<i64, Global>`). Each dep becomes a `ReifyFnPointer` cast
   in the returned MIR body.

Under `optimized_mir` override, we'd return a generic body containing
the unsubstituted versions of both — e.g.,
`ReifyFnPointer(Vec::<T, Global>::new)` — and rustc's collector would
substitute `T = i32` during its walk of `use_it::<i32>`.

### Summary table

| Claim in earlier arch guide | Actual truth |
|---|---|
| "new query needed because existing queries are DefId-keyed" | Partially true. `optimized_mir` is DefId-keyed, but the collector substitutes per-Instance during its walk, producing the same effect. The Instance-keying of `per_instance_mir` saved us from writing substitution logic; it wasn't load-bearing for correctness. |
| "the choice between new query and override was deliberate" | False — no design-doc evaluation of `override_queries` exists; the pick was pragmatic. |
| "override_queries on optimized_mir would be a complete replacement for the fork" | False — it handles dep discovery completely, but emission-skip (current patch 3) is a separate problem that the override alone doesn't solve. |
| "the load-bearing property of the query is that it fires during monomorphization" | True. Both `per_instance_mir` and `optimized_mir` satisfy this; that's why the override path works at all. |

---

## Part 3: Why the `#[linkage = "external"]` rejection is integration-specific

The architecture guide §10.6.5 rejects `#[linkage = "external"]`
under `#![feature(linkage)]` for toylang's Phase 6 partitioner hook,
with the framing "requires `#![feature(linkage)]` at the crate root,
which propagates a nightly feature flag into user-controlled
territory." This framing is correct but under-specifies *why* it
doesn't apply universally.

### The mechanism works

Verified by reading the partitioner code (for this investigation, on
the rustc-fork branch at `~/rust/compiler/rustc_monomorphize/src/partitioning.rs`):

```
fn mono_item_linkage_and_visibility<'tcx>(...) {
    if let Some(explicit_linkage) = mono_item.explicit_linkage(tcx) {
        return (explicit_linkage, Visibility::Default);  // fast path
    }
    // ... (internalization candidate logic below, lines 254+)
}
```

`explicit_linkage(tcx)` queries `tcx.codegen_fn_attrs(def_id).linkage`,
which is set by the `#[linkage = "external"]` attribute. The fast
path returns before the internalization candidate logic, so an item
with `#[linkage = "external"]` entirely bypasses the internalization
code path. The technical merit is confirmed.

### Why toylang rejected it

Toylang injects `__lang_stubs.rs` into the **user's test crate** via
`FileLoader`. `#![feature(linkage)]` is a crate-root attribute — it
applies to the crate where it appears, and there's no module-level
escape. So if toylang's generated `__lang_stubs.rs` contained
`#![feature(linkage)]`, that feature gate would activate on the
user's entire crate.

Toylang's design goal is to compile arbitrary user Rust without
requiring user opt-in to nightly features. Activating a nightly
feature flag on the user's crate violates that goal. Hence the fork
patch — it keeps the `#[linkage]` mechanism inside rustc where no user
attribute is needed, at the cost of 5 patch lines.

### When the rejection doesn't apply

The rejection is integration-specific: **it applies when consumer
stubs and user code share a crate.** If a consumer architecture
generates its stubs as their own rlib (compiled separately from user
code), `#![feature(linkage)]` stays inside the stub crate, activating
only on consumer-authored generated code. The user's crate is
unaffected.

Put differently: the trade-off is

- toylang model: stubs injected into user crate → `#[linkage]` leaks
  to user, fork patch is the fix
- separate-crate model: stubs in their own rlib → `#[linkage]` stays
  inside generated code, no fork patch needed

This matters because the separate-crate alternative was **never
seriously evaluated** for toylang's Phase 6. The facade's `FileLoader`
injection model made it architecturally inconvenient to split. A
consumer building from scratch could choose the separate-crate model
and avoid the patch entirely.

---

## Part 4: Alternatives to the fork (design space)

The 5-patch fork is the current implementation, but most of what it
achieves is reachable through sanctioned rustc extension points. This
section catalogs the alternatives so future consumers (or a future
fork-reduction effort on toylang itself) don't have to re-derive them.

Each alternative is marked with its evidence level:
- **Tested**: implemented and verified to work in some prototype
- **Mechanism-verified**: the rustc-side mechanism checked by code
  reading, no consumer-side prototype yet
- **Proposed**: plausible from first principles but unchecked

### 4.1 `override_queries` on `optimized_mir` (prototype-verified)

Instead of adding `per_instance_mir` as a new query (fork patches
1–4), override `optimized_mir` via
`rustc_interface::Config::override_queries`. Return a generic MIR
body (un-substituted, with type parameters intact) for consumer
DefIds; fall through to rustc's default for everything else. The
collector substitutes per-Instance during its walk, exactly as it
does for every other generic Rust function — which gives us the same
dep-discovery behavior `per_instance_mir` produces today.

**Prototype verification (2026-04-16).** This alternative was run
end-to-end on a branch (`poc/optimized-mir-override` in the erw
repo, commits `b425094` → `1ee3800` → `119d287` → `e597fdd`) under
an env-var gate `ERW_POC_OPTIMIZED_MIR=1`. Findings in that branch's
`findings.md`. Key confirmations:

- **Dep discovery works through the collector.** The consumer
  callback under the POC reported identical Rust-dep counts to the
  current `per_instance_mir` path. Exp 5 of the POC inspected
  rustc's emitted LLVM IR and confirmed the expected `declare`
  lines for transitively-reachable Rust items (e.g. for the `uuid`
  standalone: `declare ... stdout`, `declare ... Uuid::new_v4`,
  `declare ... <Stdout as Write>::write_all`). The collector
  substituted per-caller Params in our generic body and queued the
  concrete Rust items exactly as predicted.
- **Link fails with duplicate symbols, as predicted.** Across the
  POC's full-suite run: 257 linker errors across 117 failing
  integration tests (+ 6 failing standalone tests), split into
  56 unique `__toylang_impl_*` collisions and 19 unique
  `__toylang_accessor_*` collisions. Every test that reached
  codegen hit the same failure shape. `__toylang_internal_*`
  symbols were unaffected (as expected: rustc never emits those).
- **Release-mode and LTO don't help.** `--release` produces
  identical duplicate-symbol errors; LTO can't DCE the bodies in
  time to prevent link conflict.

What this eliminates:

- Fork patch 1 (new query definition)
- Fork patch 2 (collector calls `per_instance_mir` before
  `instance_mir`) — the collector already calls `optimized_mir`
  (via `instance_mir`) in its normal flow
- Fork patch 4 (default provider returning None) — no new query, no
  default needed

**What it does NOT eliminate — and this is important:**

- **Fork patch 3 (codegen skip)** is *not* a "nice to have" under this
  approach; it's load-bearing for linker correctness. Rustc will
  compile whatever body `optimized_mir` returns at whatever symbol
  `symbol_name` assigns. Without a skip, rustc emits a trampoline
  function at the consumer's symbol. The consumer's backend separately
  emits the *real* implementation at the same symbol. At link time:
  duplicate definitions.

  POC #2 (`poc/separate-crate-stubs`) surfaced a second instance of
  this same "patch 3 is load-bearing" problem, in a different crate:
  under the separate-crate stub model (§4.3), the *stub rlib's*
  compile also needs patch-3-equivalent behavior to skip
  `unreachable!()` bodies that plain rustc would otherwise codegen
  into the rlib. So patch 3's purpose applies at both crate compile
  sites under separate-crate, not just the user-bin compile under
  single-crate.

  Three ways to resolve this, none of which is pure `override_queries`:
  1. Keep a fork patch equivalent to patch 3 (reduce 5 patches to 1,
     not 0).
  2. Route calls and emissions to *different* symbols via a carefully-
     designed trampoline structure (tried several formulations during
     the reasoning-doc review; each collapses back to option 1 because
     the trampoline body still needs to live somewhere).
  3. Pair with a `CodegenBackend` plugin (§4.2) that declines to emit
     the stub body. The plugin's emission control replaces patch 3 —
     and, per POC #2, plugin mode naturally subsumes the stub-rlib
     version of the problem too, because the plugin IS the codegen
     backend for each crate compile and decides what to emit in both.

  So `override_queries` alone is a **3-patch reduction, not a
  zero-fork solution**. Zero fork requires pairing with §4.2. The
  plugin path retires patch 3 across all compile sites; any non-plugin
  path leaves a patch-3-equivalent somewhere.

- **The consumer-callback boundary needs a job-split.** This is a
  facade-side issue the POC surfaced that's not apparent from the
  rustc-side mechanism alone. Today's callback
  (`monomorphize_fn_inner`) is Instance-keyed and does two jobs at
  once inside a single call:

  1. **External Rust-dep registration.** Walks the consumer body,
     collects `(DefId, GenericArgsRef)` pairs for Rust items called
     directly from consumer source, returns them to the facade.
     These become `ReifyFnPointer` casts in the synthetic MIR body
     the facade constructs; rustc's collector substitutes per-caller
     and queues the concrete Rust items.
  2. **Internal-callee stashing.** Walks the same body, finds
     consumer-side (toylang→toylang) callees, recursively walks
     *their* bodies with substituted args, accumulates into
     `state.toylang_instances`. This list drives the consumer's
     own codegen in `generate_and_compile` — rustc never sees these
     functions at all.

  A DefId-keyed `optimized_mir` override can drive job 1 naturally
  (emit a generic body with Param-typed `ReifyFnPointer` statements;
  rustc's collector substitutes as it walks). It **cannot** drive
  job 2 from a DefId-keyed context — job 2 needs concrete
  `instance.args` to walk internal bodies with real type information,
  and an `optimized_mir` override only has the generic DefId.

  The natural redesign is a **trait-level job-split across two
  queries that already fire at the right keying**:

  ```rust
  trait LangCallbacks {
      // DefId-keyed: called from optimized_mir override. Emits
      // Rust-dep references with Param placeholders intact. Does NOT
      // populate state.toylang_instances, does NOT compute symbols.
      fn collect_generic_rust_deps<'tcx>(
          &self, state: &mut dyn Any,
          tcx: TyCtxt<'tcx>, def_id: LocalDefId,
      ) -> Vec<(DefId, GenericArgsRef<'tcx>)>;

      // Instance-keyed: called from symbol_name override for
      // concrete consumer entry-point Instances. Recursive internal-
      // callee walk + stashing. Returns extern symbol name.
      fn notify_concrete_entry_point<'tcx>(
          &self, state: &mut dyn Any,
          tcx: TyCtxt<'tcx>, instance: Instance<'tcx>,
      ) -> String;
  }
  ```

  Credit: this split shape was proposed in the POC's round-2
  writeup (commit `119d287`); the recursive-walk cascade and
  drop-glue bounds were confirmed in round-3 (`e597fdd`).

  Properties of the split:

  - **Uses queries that already fire.** `optimized_mir` is overridable
    via `Config::override_queries`. `symbol_name` is already overridden
    today. No new rustc-exposed hook is required.
  - **Recursive internal-callee walk is unchanged.** Under the split,
    `notify_concrete_entry_point` keeps walking transitively:
    entry-point → internal → internal → internal. Entry points are
    the Instances rustc queries `symbol_name` for (they're what Rust
    callers reference from outside the consumer stub module);
    internal callees are discovered purely consumer-side via the
    recursion, not via rustc-query triggering. The existing
    `state.visited_symbols` dedup set handles repeat walks. The split
    is a conceptual separation of which query's side-effect does
    which job, not a rewrite of the walker.
  - **Drop-glue path is untouched.** The `mir_shims` override
    (`InstanceKind::DropGlue(_, Some(ty))`-keyed) and the
    `monomorphize_type` callback (`Ty<'tcx>`-keyed) both already
    receive concrete args at every invocation. They don't have the
    DefId-keyed-query-needs-Instance-keyed-callback mismatch that the
    function-body path has, because their queries themselves key on
    concrete things. The job-split applies only to the function-body
    path.
  - **One subtlety worth flagging.** Under the split, the call to
    `monomorphize_fn_inner` from `symbol_name` must NOT also accumulate
    `rust_deps` — those flow through `collect_generic_rust_deps` +
    rustc's collector substitution. Double-registration would
    double-code the deps. A small change to the existing walker on
    the Instance-keyed path.

  The redesign is conceptually smaller than "new callback entry
  point" reads as, but it is a genuine trait-level change that a
  consumer adopting the zero-fork path must absorb. Cost is covered
  in §4.2's estimate.

- Fork patch 5 (partitioner visibility hook). Orthogonal; see §4.3.

Implementation cost estimate (from the investigation + POC): 1-2
weeks for the query override itself; +2-3 weeks for the
`LangCallbacks` trait job-split and its consumer-side adoption;
+ the plugin integration in §4.2. Most of the MIR construction
code in `mir_helpers.rs` carries over unchanged.

### 4.2 `-Zcodegen-backend=consumer` plugin + `override_queries` (proposed)

The `rustc_codegen_cranelift` / `rustc_codegen_gcc` integration model:
replace rustc's default codegen backend wholesale via
`-Zcodegen-backend`. The consumer's backend receives CGUs after
monomorphization, emits `.o` files, and delegates to
`rustc_codegen_llvm` as a library for Rust deps.

A pure plugin — no query overrides — does **not** work for this
architecture (see Part 1: by the time a backend plugin receives CGUs,
rustc's reachable-set walk is complete, and any edge into a Rust
generic that was invisible during that walk is now uncodegen-able).

The viable combination is **plugin + `override_queries`**. The query
overrides fire during monomorphization (as in §4.1); the plugin
handles post-monomorphization emit. Together they cover the full
facade surface without a fork.

Specifically, the plugin is what eliminates §4.1's residual dependency
on fork patch 3. Where patch 3 teaches rustc's codegen to skip
emitting the stub body, the plugin simply *is* rustc's codegen: it
decides what to emit. Stub bodies never get emitted because the
plugin doesn't emit them. The consumer's real implementation is
emitted separately via the plugin's own IR-producing path. No symbol
conflict, no patch needed.

Known complications:

- `ModuleLlvm` and several associated types in `rustc_codegen_llvm`
  are `pub(crate)`. The existing `codegen_wrapper.rs` works because
  it delegates *everything* to the inner backend; selective
  interception would require either upstream changes to make these
  types public, or type-erasure hacks.
- The facade's current `rustc_driver::Callbacks` integration
  (`install_callbacks` at startup, `after_analysis` for validation)
  doesn't map directly to a backend plugin's lifecycle. The plugin
  would need to install callbacks from within its `init` or
  `codegen_crate` method, which interacts with rustc's Rayon-threaded
  compilation in non-obvious ways.

Implementation cost estimate (revised after §4.1 POC): **4–8 weeks**
from the current wrapper baseline, broken down approximately as:

- 1–2 weeks: `optimized_mir` override plumbing (§4.1's rustc-side half)
- 2–3 weeks: `LangCallbacks` trait job-split and consumer-side adoption
  (the §4.1 callback-redesign half, surfaced by the POC — this was
  the line item the earlier 2–4 week estimate elided)
- 2–3 weeks: plugin integration, including resolving the `ModuleLlvm`
  `pub(crate)` wall (upstream contribution or type-erasure) and the
  `rustc_driver::Callbacks` → backend-lifecycle reconciliation

The earlier 2–4 week figure costed only the plugin half (treating the
query override as a drop-in replacement for `per_instance_mir`
requiring no facade-side reshaping). The POC's prototype run surfaced
that the facade's Instance-keyed callback contract doesn't fit a
DefId-keyed override without the job-split, which is where the extra
2–3 weeks lives.

**Plugin mode absorbs the separate-crate integration work at
approximately zero marginal cost.** POC #2 (§4.3) surfaced that a
greenfield consumer adopting the plugin path for zero-fork naturally
handles the stub-rlib emission-skip problem (plugin is the codegen
backend, decides what to emit), the cross-crate consumer DefId
handling (plugin fires per crate-compile), and the generic-wrapper
mono routing (~5 LoC of `override_queries` on
`upstream_monomorphization` returning None for consumer DefIds).
The 4-8 week estimate therefore covers both `#[linkage]`-based patch-5
elimination AND the plugin work; a consumer isn't paying separately
for each. See §4.3 for the detailed subsumption argument.

### 4.3 Separate-crate stub model (prototyped, with caveats)

Place `__lang_stubs.rs` in its own rlib, compiled separately from
user code. The original framing for this section — "apply
`#[linkage = "external"]` in a different integration location" —
is correct but under-specifies the cost for brownfield consumers.
The separate-crate alternative was prototyped on branch
`poc/separate-crate-stubs` (commits `2463248` → `acc62d2` →
`2c004a6`) under an env-var gate `ERW_POC_SEPARATE_STUBS=1`. The
POC's `findings.md` has the full writeup; highlights below.

**Mechanism confirmation (positive):**

- **`#![feature(linkage)]` at a real crate root works** as Part 3
  predicted. The stub rlib compiles cleanly with `#[linkage = "external"]`
  on the generic wrappers; no E0658 "experimental feature" error
  that triggers under the single-crate FileLoader model. Step 3
  of the POC confirmed this mechanically with `nm` inventory of
  the produced rlib.
- **Cargo orchestration is clean.** ~100 LoC of `build.rs`
  produces a two-member workspace (stub rlib + user bin) with a
  path dep. `RUSTC_WORKSPACE_WRAPPER` dispatches correctly per
  crate; `rust-toolchain.toml` at workspace root applies uniformly.
  No cargo wrinkles surfaced. Prediction 3 of the POC held.

**Blockers for a toylang-brownfield retrofit:**

The POC surfaced three architectural blockers that block a working
binary without substantial backend work. All three stem from
single-crate-compile assumptions in toylang's LLVM backend (FileLoader
injection enforces the model, but the assumptions live in the backend,
not the injection mechanism).

- **Risk #1** (predicted pre-POC, empirically confirmed): a generic
  `#[inline(never)]` wrapper in an rlib with no local caller is
  defined in rlib metadata only, never codegenned, because rustc's
  mono collector walks from rlib-local roots and finds none. The
  downstream bin's `Instance::upstream_monomorphization` returns
  `Some(rlib_cnum)` due to `InlineAttr::Never`, routing link to the
  rlib's non-existent mono. `#[linkage = "external"]` is an attribute
  on the eventual monomorphization, not a forced-codegen directive,
  so it doesn't rescue.
- **Risk #8** (surfaced by the POC): plain rustc compiling the stub
  rlib codegens every `unreachable!()` body, because fork patch 3's
  `codegen_instance` skip is useless when toylang processing isn't
  installed for that compile. These bodies win at link time over
  anything the consumer's backend might provide at the same Rust-mangled
  symbol.
- **Risk #9** (surfaced by the POC): `llvm_gen::generate_with_tcx`'s
  MonoItems walk filters with `def_id.as_local()`. Under the single-crate
  model, all consumer DefIds are local to the user bin's compile;
  under separate-crate they're in the stub rlib. User bin compile
  skips them, backend emits no extern wrappers, the rlib's forwarding
  bodies dangle. *This is a property of the single-crate-compile
  integration model, not of FileLoader specifically*: any separate-crate
  architecture would trip the same filter unless the backend is
  designed cross-crate from day one.

**Brownfield vs greenfield cost split:**

These blockers are toylang-brownfield-specific. They don't block a
consumer designed for separate-crate from day one.

*Toylang brownfield (retrofit):* ~1-2 weeks of backend surgery.
Three remediation paths:

- **A. Dual-crate backend split.** Distribute toylang's LLVM codegen
  across both the stub rlib and user bin compiles. Clean but
  substantial — requires role-aware codegen in each compile, careful
  dedup at link. ~1-2 weeks.
- **B. Relax `as_local()` filter.** Make the MonoItems walk at
  `llvm_gen.rs:1896` accept non-local consumer DefIds. Interacts
  with every ABI query call site (`fn_abi_of_instance`, etc.) —
  they may behave differently for non-local instances. ~3-5 days.
- **C. In-Rust forwarding bodies in stub_gen.** Generate real
  forwarding bodies (`extern "C" { fn __toylang_impl_X(...) -> T; } {
  unsafe { __toylang_impl_X(...) } }`) for non-generic items.
  Dismisses the patch-3-skip reliance. For generics, needs
  per-T forwarding + anchor generation. ~1-2 weeks for generic
  support.

Plus ~1 week for risk #1's anchor generation (requires rerunning
toylang's deep walk ahead of stub-gen, with latent `TyCtxt`-ordering
concerns).

Total for toylang brownfield: ~1-2 weeks of architecture work,
substantially more than fork patch 5's ~2-3 day per-bump
maintenance cost. The POC recommends leaving patch 5 in place for
toylang.

*Greenfield consumer with plugin path (§4.2):* approximately free.

- **D. Plugin mode subsumes risks #8 and #9 natively.** Under
  `-Zcodegen-backend=consumer` + `override_queries`, the plugin
  is the codegen backend for each crate compile. Risk #8 disappears
  because the plugin decides what to emit per compile and can skip
  `unreachable!()` bodies. Risk #9 disappears because the plugin
  fires per crate-compile, so consumer DefIds are always local to
  whichever compile is processing them — the `as_local()` filter
  becomes moot. Essentially plugin mode IS remediation A (dual-crate
  codegen split) expressed through rustc's sanctioned API, at no
  additional cost beyond the plugin work itself.
- **Risk #1 reduces to ~5 LoC** of `override_queries` on
  `upstream_monomorphization`, returning None for consumer DefIds
  so the downstream bin mono'd the wrapper locally. The plugin's
  downstream-compile codegen then emits the wrapper directly. No
  stub-gen-time T-harvest, no anchor bookkeeping.

For a greenfield consumer like Vale that's already doing the plugin
work (§4.2), separate-crate integration is absorbed at approximately
zero marginal cost. The §4.2 cost estimate (4-8 weeks) already covers
it. See POC commit `2c004a6` §4.3.D for the full argument.

**Summary table:**

| Consumer shape | Separate-crate cost | Notes |
|---|---|---|
| Toylang brownfield | ~1-2 weeks backend surgery (remediations A/B/C) | More expensive than maintaining patch 5; recommendation is to leave the fork in place |
| Greenfield + plugin | ~0 (absorbed into plugin work) | Plugin mode subsumes #8/#9 natively; #1 is ~5 LoC of upstream_monomorphization override |
| Greenfield + `per_instance_mir`-equivalent | ~1-3 days upfront design | Backend designed for cross-crate consumer DefIds from day one; no retrofit |

**Design-space gap the POC closed:**

Before the POC, this section said the separate-crate model was
"proposed; not prototyped." The POC shows that framing was incomplete
in two directions. The mechanical part (`#[linkage = "external"]`
at a real crate root + clean cargo orchestration) genuinely works as
predicted. But the integration cost for a consumer retrofitting
separate-crate onto a single-crate-compile backend was
under-specified; and the plugin-mode subsumption (which drops that
cost to zero for greenfield) was not identified until the POC
surfaced it. The reasoning doc now distinguishes these paths
explicitly.

### 4.4 `rustc_public` (formerly `stable_mir`) for consumer-side code

Orthogonal to the fork question: the consumer-side code (oracle +
ABI/symbol-name call sites in the LLVM backend) can migrate to
`rustc_public`, reducing churn exposure to rustc's internal type
surface.

Coverage as of early 2026 (verified by reading rustc_public docs):

| Consumer operation | rustc_public coverage |
|---|---|
| Type inspection (`TyKind`, adt_def, item_name) | Covered |
| Instance resolution (`Instance::resolve`, `mangled_name`) | Covered |
| MIR *reading* (Body, BasicBlock, Terminator) | Covered |
| `fn_abi_of_instance` | Covered via `Instance::fn_abi()` |
| Layout reading | Covered (`Layout`, `LayoutShape`, `TyAndLayout`) |
| Custom query providers | **Not covered** |
| MIR *construction* (arena alloc, custom Body building) | **Not covered** |
| `CodegenBackend` integration | **Not covered** |
| `FileLoader` stub injection | **Not covered** |
| Partitioner hooks | **Not covered** |

By rough LoC: `rustc_public` could absorb ~40–50% of the facade's
rustc-internal surface — specifically, the read-side operations.
The architecturally load-bearing pieces (query providers, codegen
backend integration, partitioner hook, MIR construction) remain on
rustc-internal APIs. Migrating consumer code to `rustc_public` helps
with ongoing churn but does not eliminate the fork by itself.

Note: `rustc_public` remains nightly-only and explicitly unstable
("This API is still completely unstable and subject to change" per
the crate docs).

### 4.5 MIR construction churn (the hole nobody escapes)

Regardless of which alternative is picked, **MIR construction stays
on rustc-internal APIs**. The ~250 LoC of synthetic body construction
in `queries/per_instance.rs` + `mir_helpers.rs` — arena allocation,
`set_required_consts`, `set_mentioned_items`, `ReifyFnPointer` cast
shaping, source info, `UnwindAction`, `CastKind` — uses
`rustc_middle::mir` directly. None of it is in `rustc_public`, and
none of it has a stable equivalent.

Budget ~1 person-week per 6-month rustc bump for MIR-construction API
drift, independent of fork choice. This is the biggest recurring
cost; everything else is one-time migration.

---

## Part 5: The 5-patch-fork cost accounting

For a toylang-style deployment (research project, occasional rustc
bumps, user installation via `rustup toolchain link`):

- Fork maintenance: ~2-3 days per rustc bump. Patches are small
  (5 sites, ~50 lines total) and shape-stable across rustc releases.
- MIR construction churn: ~1 person-week per bump (same as zero-fork).

Total: ~1.5 weeks per bump, fork included.

For a zero-fork target (distribution to non-Rust-native users, where
"install a forked rustc" is user-visible friction):

- Migration: 4-8 weeks engineering (sum of §4.1, §4.2, §4.4 costs)
- MIR construction churn: ~1 person-week per bump (same)
- Fork rebase: 0

Total: 4-8 weeks upfront, ~1 week per bump ongoing.

The math favors zero-fork if the consumer does frequent bumps AND
user installation friction is a real cost. For toylang the math has
not favored zero-fork so far; for Vale's deployment story it
likely will.

---

## Summary

| Question | Answer |
|---|---|
| Why interleave with rustc's monomorphization? | To avoid reimplementing rustc's generic/trait resolution machinery — supply the concrete tuples, defer the walk. |
| Was `optimized_mir` override considered? | No documented evaluation exists; the new query was picked on taste. |
| Does `optimized_mir` override fully replace the fork? | No — it handles dep discovery completely, but emission-skip (current patch 3) is a separate problem. The override *alone* reduces 5 patches to 1. Pairing with a `CodegenBackend` plugin (§4.2) eliminates that remaining patch, making the combination the zero-fork target. |
| Was `#[linkage]` tested for toylang? | Yes, briefly. Reverted because stubs share a crate with user code and the feature flag would leak. |
| Does that rejection apply universally? | No — it's specific to toylang's FileLoader injection model. A separate-crate consumer avoids it. |
| Was the separate-crate stub model evaluated? | Never. Real gap. |
| Was the `CodegenBackend` plugin route considered? | Only as a wrapper inside the current driver flow, not as `-Zcodegen-backend=consumer`. The plugin-plus-override-queries combination is the interesting zero-fork design. |
| Can `rustc_public` replace the fork? | No. It covers ~40-50% of the consumer's rustc-internal surface (read-side); the framework (query providers, codegen backend, partitioner hook) is not covered. |

This document is not a plan; it is a map of the design space as
understood after the 2026-04-16 review. An actual fork-reduction
effort would combine §4.1 (query override) with §4.2 (backend plugin)
to reach zero fork patches, since neither alternative is sufficient
on its own — §4.1 alone retains patch 3 for emission skip, and §4.2
alone loses dep discovery. §4.3 is orthogonal and addresses patch 5
independently.
