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

The longer argument, using a worked example that captures the shape:

### The motivating case

```
toylang:  fn use_it<T>(x: T)                calls rust_fn<T>(x)
rust:     fn rust_fn<T: SomeTrait>(x: T)  { x.some_method() }
toylang:  fn main() { use_it<i32>(42) }
```

For the final binary to link:

1. Code for `rust_fn<i32>` must exist. Rustc generates this.
2. Code for `<i32 as SomeTrait>::some_method` must exist. Rustc
   generates this.
3. Code for toylang's `use_it<i32>` must exist. Toylang generates
   this; it calls into `rust_fn<i32>`.

For (1), rustc's monomorphization collector walks from entry points
outward via direct calls, `ReifyFnPointer` casts, drop glue, etc.
The edge `use_it<i32> → rust_fn<i32>` lives entirely inside toylang
source; rustc never parses or walks it. Without facade intervention,
rustc never sees that `rust_fn<i32>` is reachable, the collector
never queues it for codegen, and the link step fails with an
undefined symbol.

For (2), once (1) happens it's automatic. Rustc walks `rust_fn<i32>`'s
body, sees `x.some_method()`, resolves `<T as SomeTrait>::some_method`
with `T=i32`, queues `<i32 as SomeTrait>::some_method` for codegen.
Rustc's normal generic-trait resolution handles this — **as long as
rustc knows about the caller**.

### The one-way handoff

The facade's job, in this framing, is narrow: **tell rustc the
leaves** — the concrete-type-args tuples for every Rust item
directly called from consumer code. Rustc walks the transitive
closure from there.

Mechanically, this is what `per_instance_mir` + the `ReifyFnPointer`
statements it emits do. Each consumer instance's synthetic MIR body
enumerates "here are the Rust items I need for this concrete
instantiation", as `ReifyFnPointer` casts of their DefIds with
substituted generic args. The collector visits those casts during its
normal walk and adds the referenced items to the reachable set.
Transitively-reachable items cascade through rustc's own machinery
without further facade involvement.

### Why the alternative doesn't work

The apparent alternative — have the facade do its own monomorphization
walk of Rust deps ahead of codegen — rules itself out once you try to
build it. To discover `<i32 as SomeTrait>::some_method` without
rustc's collector, the facade would need to reimplement:

- Rust MIR walking
- Trait impl candidate resolution (`for_each_relevant_impl` +
  `Instance::expect_resolve`)
- Associated-type projections + normalization
- Default trait methods, blanket impls, conditional impls,
  specialization
- Generic argument substitution across the transitive call graph

That's tens of thousands of lines of rustc internals reimplemented.
The interleaving isn't a coordination between two monomorphizers —
it's a way to **not** reimplement rustc's trait/generic machinery.
The facade defers to rustc for all generic resolution and only
supplies rustc with the concrete type tuples it can't see otherwise.

### Subtler cases the mechanism also covers

The same one-way-handoff model handles:

- **Nested generics.** Consumer calls `Vec::extend<i32, Global, I>(v, iter)`
  with I concrete (e.g., `Range<i32>`); rustc walks from there into
  `<Range<i32> as IntoIterator>::into_iter` and all trait chain
  downstream.
- **Consumer type parameters flowing into Rust generics.** Consumer
  holds a `HashMap<K, V>` where K/V are consumer type parameters; at
  instantiation these become concrete, and rustc walks every
  `<HashMap<K,V> as _>::method` call the consumer or Rust deps make.
- **Drop glue.** The `mir_shims` provider constructs
  `InstanceKind::DropGlue(_, Some(Vec<i32>))` so rustc walks the
  destructor chain for concrete instantiated types.

Each of these is "consumer supplies the concrete tuple, rustc walks
from there." No separate mechanism is needed — the same monomorphization
handoff drives all of them.

### Phase timing: why not before or after

**Before monomorphization (e.g., `Callbacks::after_analysis`).** The
facade would need `fn_abi_of_instance` and full trait resolution for
concrete type args — both only available during codegen, not after
analysis. An `after_analysis` pre-pass has access to type-checking
results but not to the monomorphization machinery.

**After monomorphization (e.g., `CodegenBackend` plugin receiving
CGUs).** By this phase rustc has already walked the reachable set. If
the edge from consumer code into a Rust generic was invisible during
that walk, the target item is not in the CGU list — and no amount of
codegen cleverness can retroactively add it. (A CodegenBackend plugin
*combined with* query overrides can still work, because the query
overrides fire during the walk; see Part 4.)

These timing constraints are why monomorphization is the one correct
phase for the handoff — it's the phase where rustc is doing the walk
the facade needs to participate in.

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

### 4.1 `override_queries` on `optimized_mir` (mechanism-verified)

Instead of adding `per_instance_mir` as a new query (fork patches
1–4), override `optimized_mir` via
`rustc_interface::Config::override_queries`. Return a generic MIR
body (un-substituted, with type parameters intact) for consumer
DefIds; fall through to rustc's default for everything else. The
collector substitutes per-Instance during its walk, exactly as it
does for every other generic Rust function — which gives us the same
dep-discovery behavior `per_instance_mir` produces today.

What this eliminates:

- Fork patch 1 (new query definition)
- Fork patch 2 (collector calls `per_instance_mir` before
  `instance_mir`) — the collector already calls `optimized_mir`
  (via `instance_mir`) in its normal flow
- Fork patch 4 (default provider returning None) — no new query, no
  default needed

**What it does NOT eliminate — and this is important:**

- **Fork patch 3 (codegen skip)** is *not* a "nice to have" under this
  approach; it's load-bearing for linker correctness. Here's why:
  rustc will compile whatever body `optimized_mir` returns at whatever
  symbol `symbol_name` assigns. Without a skip, rustc emits a
  trampoline function at the consumer's symbol. The consumer's backend
  separately emits the *real* implementation at the same symbol. At
  link time: duplicate definitions. Linker error.

  Three ways to resolve this, none of which is pure `override_queries`:
  1. Keep a fork patch equivalent to patch 3 (reduce 5 patches to 1,
     not 0).
  2. Route calls and emissions to *different* symbols via a carefully-
     designed trampoline structure (tried several formulations; each
     collapses back to option 1 because the trampoline body still
     needs to live somewhere).
  3. Pair with a `CodegenBackend` plugin (§4.2) that declines to emit
     the stub body. The plugin's emission control replaces patch 3.

  So `override_queries` alone is a **3-patch reduction, not a
  zero-fork solution**. Zero fork requires pairing with §4.2.

- Fork patch 5 (partitioner visibility hook). Orthogonal; see §4.3.

Implementation cost estimate (from the investigation): 1-2 weeks for
the query override itself. The additional cost of pairing with a
backend plugin is covered in §4.2.
Most of the MIR construction code in `mir_helpers.rs` carries over
unchanged.

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

Implementation cost estimate: 2–4 weeks of architecture work from
the current wrapper baseline.

### 4.3 Separate-crate stub model (proposed)

Place `__lang_stubs.rs` in its own rlib, compiled separately from
user code. This unlocks two things:

- **`#[linkage = "external"]` usage** (Part 3). `#![feature(linkage)]`
  stays inside the stub rlib's root, never reaches user code. The
  partitioner hook (fork patch 5) becomes unnecessary.
- **Cross-crate linkage defaults.** Wrappers in an rlib, compiled as
  `CrateType::Rlib` (which sets `local_crate_exports_generics() == true`),
  default to `Visibility::Default + can_be_internalized = false` for
  generic `#[inline(never)]` items — the opposite of the executable-crate
  default that motivated the visibility hook.

This alternative was **never explored for toylang**. The only mention
in historical docs was a passing note in
`docs/historical/report-stub-approaches.md` listing "cross-crate
scenarios" as a known-unknown. The Phase 5 build flow
(stubs injected into user crate via `FileLoader`) made splitting
architecturally awkward; nobody prototyped it.

Caveats:

- The cross-crate linkage defaults claim above needs empirical
  verification. The mechanism sounds right; nobody has run it.
- Cargo orchestration: the consumer's build flow (`toylangc build`
  in toylang's case) would need to compile the stub rlib as a
  separate step and wire it as a dependency of the main crate.
  Not complicated, but distinct from the current single-crate flow.

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
