# Phase E — debuginfo ICE investigation

**Status (Session 12 update): Path 2 LANDED.** Sky structs now emit as
wrapper-as-field newtypes around `__ToylangOpaque<HASH>`; source and layout
field counts match; the ICE doesn't reproduce. Fork patch 4 was reverted —
the assumption violation it patched is now structurally impossible, not
just masked. 262/262 tests pass under unpatched rustc. See `tl-handoff.md`
§12 and commits `72a929e` / `41423cf` / `90599cf` for the migration.

The investigation notes below are preserved for the architectural reasoning
trail. The PR draft at `phase-e-rustc-pr-draft.md` is also preserved — it
remains the right upstream submission for plugin authors who don't migrate
to the wrapper-as-field shape (cranelift, miri, future plugins). Sky-side
the patch is no longer needed.

---

## Original investigation (Session 11)

Status: investigation complete; recommendation made; no code change yet.

## TL;DR

The rustc debuginfo ICE described in `stub_gen.rs:127-132` (and re-stated as
plan-doc site #4) **still reproduces** under our pinned `rustc 1.95.0-dev`
(commit `d940e568`, `nightly-2026-01-20`). The unit-struct workaround
(`pub struct Foo;`) is load-bearing for non-generic Sky types; switching to
the tuple shape (`pub struct Foo(())`) crashes `r_t_r_vec_of_ship` cleanly
in rustc's debuginfo walker.

Two paths to remove the asymmetry, both with concrete costs.

## The exact crash site

Reproduction: modified `stub_gen.rs` to emit `pub struct #ident(());` for
non-generic types instead of the unit `pub struct #ident;`. Ran
`r_t_r_vec_of_ship` (fixture wraps a Sky struct inside `Vec<ToyShip, Global>`).

```
thread 'rustc' panicked at compiler/rustc_abi/src/lib.rs:1676:66:
index out of bounds: the len is 0 but the index is 0
stack backtrace:
   3: <rustc_abi::FieldsShape<FieldIdx>>::offset
   4: build_struct_type_di_node closure (mapping over FieldDef iter)
   6: build_type_with_children<build_struct_type_di_node>
   7: build_struct_type_di_node
   8: spanned_type_di_node
   9: build_generic_type_param_di_nodes closure
  10: SmallVec extend  
  11: build_generic_type_param_di_nodes
  12: build_type_with_children<build_struct_type_di_node>  ← Vec<ToyShip>'s di node
  13: build_struct_type_di_node                            ← Vec<ToyShip>
```

### What's happening

1. Rustc codegens `Vec<ToyShip>::push`. The debuginfo emitter constructs a
   DI node for `Vec<ToyShip>` via `build_struct_type_di_node`.
2. That walker iterates Vec's *generic params* (`build_generic_type_param_di_nodes`)
   to emit the type-param DI nodes. T = ToyShip.
3. For each generic param, it calls `spanned_type_di_node(ToyShip)`, which
   recursively calls `build_struct_type_di_node(ToyShip)`.
4. `build_struct_type_di_node` enumerates ToyShip's source-level `FieldDef`s
   (count: 1 — the unit tuple field). For each `(i, field)`, it queries
   `layout.fields.offset(i)`.
5. Sky's `layout_of` override returns `FieldsShape::Arbitrary { offsets: [], memory_index: [] }`
   — len 0. `FieldsShape::offset(0)` indexes `self.offsets[FieldIdx::from_usize(0)]`,
   panics out-of-bounds at `rustc_abi/src/lib.rs:1676`.

### Why `pub struct Foo;` dodges it

A unit struct has *zero* `FieldDef`s. The `.enumerate()` loop runs zero times.
No `offset(i)` call, no panic.

### Why `pub struct Foo<T>(PhantomData<T>)` dodges it

PhantomData is special-cased by rustc's debuginfo walker: `build_generic_type_param_di_nodes`
sees `PhantomData<T>` as a ZST marker and skips recursing into its layout.
The closure at frame 4 still iterates ToyShip's one source field, but for
a PhantomData field the walker doesn't reach `offset()` on the inner
layout — it short-circuits at the marker check.

The current non-generic workaround can't use PhantomData (would need a type
param to phantom over); the unit-struct shape is the only `0 source fields`
form that satisfies rustc's "all generics must be used" rule (vacuously,
since there are no generics).

## Two paths to remove the asymmetry

### Path 1 — patch rustc upstream

The debuginfo walker should clamp its source-field loop to the layout's
field count. Sketch:

```rust
// rustc_codegen_llvm/src/debuginfo/metadata.rs, in build_struct_type_di_node:
let field_count = std::cmp::min(adt_def.non_enum_variant().fields.len(),
                                layout.fields.count());
adt_def.non_enum_variant().fields.iter().take(field_count).enumerate()
    .map(|(i, f)| {
        // … existing closure body, but i is bounded by layout.fields.count()
    })
```

Probably 5-10 LOC change in `rustc_codegen_llvm`. The patch is *general* —
any plugin (cranelift backend, miri, future Sky, anyone overriding
`layout_of`) that reports fewer layout fields than source-level FieldDefs
hits this. A simple defensive clamp is the right shape.

**Cost in our control:** ~1 day to write the patch, write a minimal
reproducer Sky-free (cranelift-backend test using a custom layout override),
file the bug, submit the PR.

**Cost out of our control:** rustc PR review latency. Weeks to months.
The patch can ship in our 3-patch fork while upstream is in flight.

**Risk:** review pushback ("override safety isn't a concern; plugins
shouldn't lie about layouts"). The counter is §10's locked Sky design —
opaque-with-size *is* the architecture, not a hack.

### Path 2 — migrate to `SkyOpaqueType<typeid>` universal wrapper

Architecture §10.6 locks: non-export Sky types appearing in Rust generics
get projected onto `SkyOpaqueType<const T: u64>`. The wrapper carries the
typeid as a const generic; the source-level field IS the wrapper itself.

Generalized to *all* Sky types (export + non-export), each Sky struct's
stub representation becomes:

```rust
// Today:    pub struct Widget;
// Path 2:   pub struct Widget(SkyOpaqueData<TYPEID_WIDGET>);
```

Where `SkyOpaqueData<const T: u64>` is a pre-declared zero-source-field
wrapper. Rustc's debuginfo walker on `Widget`'s source field sees
`SkyOpaqueData<typeid>` — also opaque-with-Sky-size — and recurses into
*its* source fields. If `SkyOpaqueData`'s definition is `pub struct SkyOpaqueData<const T: u64>;`
(unit struct), the inner walk has zero source fields, no panic.

In other words: Path 2 routes every Sky type through the same wrapper that
already dodges the ICE, instead of having two struct shapes (one
ICE-dodging, one not).

**Cost:** substantial.
  - `stub_gen.rs`: rewrite struct emission to produce wrappers.
  - `layout.rs` (the `layout_of` query override): every Sky-marked ADT lookup
    needs an extra hop through the wrapper to recover the inner typeid.
  - `oracle.rs`: every site that maps a rustc `ty::TyKind::Adt(def, args)`
    to a Sky type name needs to handle the wrapper. The current code does
    a direct DefId-to-name lookup; with the wrapper, it has to decode the
    const-generic typeid and then look up in a Sky-side table.
  - `mir_shims` (drop glue): same.
  - `symbol_name`: same.
  - Migration: every fixture's struct emission changes byte-for-byte; the
    A.5 byte-identical pass-through invariant test handles only pure-Rust
    crates so isn't affected, but the sidecar determinism test compares
    Sky outputs across runs and is sensitive to format drift.
  - Multi-day refactor; estimate **5-10 days** with buffer.

The wrapper machinery already exists in skeleton: the architecture's
§13's typeid table is the right substrate. Sky's locked design lands this
anyway when comptime types arrive (§13.7); accelerating it for Phase E
just pulls forward Sky-aligned work.

**Risk:** the rewrite touches every cross-language type-identity site.
High blast radius. Easy to regress something subtle (e.g., the
"upstream_structs cache for monomorphize_type" mirror from Session 9's
case6 sharpening, which assumes a direct typename match).

## Recommendation

**Do Path 1 first** (file the rustc bug, submit the upstream PR). It's
~1 day of focused work. If it lands or we ship the patch in our fork, the
struct-shape asymmetry can be removed in stub_gen with a 5-LOC change —
Phase E "drops" the asymmetry cleanly without architectural churn.

**Defer Path 2 until** the SkyOpaqueType<typeid> machinery is built for
comptime types anyway (per §13.7's "synthetic items via SkyOpaqueType
wrapper"). At that point the cost amortizes — we're routing comptime
types through the wrapper regardless, and non-export Sky types come along
for free.

**Do NOT** unify the struct shape today by patching just our fork without
upstream effort. The patch would live forever in our private fork; every
nightly bump pays the rebase cost; the upstream Rust ecosystem doesn't
benefit; we'd own a small bit of rustc-internal debt indefinitely.

## What this leaves unfenced

The arch-fence test from Phase F (`tests/architecture_fence.rs`) covers
the discovery+typecheck paths in `callbacks_impl.rs` and `type_resolve.rs`.
It deliberately excludes `stub_gen.rs` — the two stub_gen sites
(struct shape, extern decl) are externally-constrained, not implementation
pragmas. Their `is_generic` branches stay; the inline comments document
why; the plan tracks them as separate phases D (decls, gated on inline
codegen) and E (struct shape, gated on this investigation's outcome).

The CLAUDE.md compiler law is honored in spirit if not letter: every
remaining `is_generic` branch in toylang sits at a site where the
non-generic-is-degenerate-case rule conflicts with an *external*
constraint (Rust syntax for `extern "C"`, rustc's debuginfo walker's
field-count assumption). Phases A/B/C/F closed every site Sky could close
without external dependencies.

## Files touched this investigation

None at the time of the original write-up. The reproduction experiment
was temporary; restored before commit.

## Update (later same session) — patch written

After committing the investigation, Path 1 was started. The
defensive clamp was written against `~/rust` (rustc 1.95.0-dev,
commit `d940e568`) at two sites:

1. `compiler/rustc_codegen_llvm/src/debuginfo/metadata.rs::build_struct_type_di_node`
   — the original ICE site. Clamps the source-field iter to
   `min(variant_def.fields.len(), struct_type_and_layout.fields.count())`.
2. `compiler/rustc_codegen_llvm/src/debuginfo/metadata.rs::build_union_type_di_node`
   — same pattern (source iter → `union_ty_and_layout.field(cx, i)`).
   Sibling clamp.

`build_enum_variant_struct_type_di_node` audited; it iterates the
layout's field count FIRST and then indexes source, so the underreport
case Sky hits doesn't reach it. No clamp needed there.

Verification status: rustc rebuild in progress at write time; will
re-run the `r_t_r_vec_of_ship` reproducer with `pub struct Foo(())` in
stub_gen post-rebuild to confirm the ICE is gone.

PR description draft in `phase-e-rustc-pr-draft.md`.

The patches now add a fourth and fifth modification to our fork's
working tree (alongside the three Approach-A patches). They are NOT
load-bearing for our existing 253/253 baseline — they don't change
behavior on unoverridden layouts — so toylangc with the current
`pub struct Foo;` stub shape continues to work identically. The
patches only become observable when stub_gen is migrated to the
unified shape that triggered the ICE.
