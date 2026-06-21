# Stub Body Must Not Be Inlined (SBMNBIZ)

Sky's `stub_gen` emits `pub fn foo() -> T { unreachable!() }` for every
consumer export. The Option 4 `codegen_fn_attrs` override marks each
such item with `Linkage::AvailableExternally`, which means LLVM keeps
the stub's body in IR **for inlining purposes** but emits no `.o`
symbol — the real `.o` symbol comes from Sky's `fill_extra_modules`
contribution (rustc fork patch 4).

The hazard: **if LLVM ever inlines the stub's `unreachable!()` body
into a real caller, the result is undefined behavior**. The caller's
continuation after the inlined call becomes unreachable; the optimizer
removes downstream code (including legitimate work like the next
`println_int`); and if the caller is ever invoked, the program has UB.
Worse, this failure is silent — there's no link error, no test crash
from the build itself, just wrong runtime behavior.

The discipline: at every compile session where rustc emits an
`AvailableExternally` stub body for a consumer item, EITHER

(a) **Sky's `fill_extra_modules` also emits the real body with
    `External` linkage in the same compile session**, so the IR
    linker (intra-CGU + thin-local-LTO import) picks the real body
    over the AvailableExternally stub, OR

(b) **No callers of the symbol exist in that compile session's IR**,
    so no inlining can occur (rustc's mono collector either gates the
    item out via @F.13 — `is_reachable_non_generic` returns true for
    upstream non-generic items at downstream compiles — or no local
    Sky body references the symbol).

When the IR linker has both an External and an AvailableExternally
definition for the same symbol, External wins. The unreachable body
is effectively discarded for both `.o` emission AND inlining. When
only the AvailableExternally body exists AND a caller does, the
inliner sees the stub body as the inlinable definition → UB risk.

## Where

The discipline is upheld by the coordination of three subsystems:

- **`rustc-lang-facade/src/queries/codegen_fn_attrs.rs`** —
  `lang_codegen_fn_attrs` + `lang_extern_codegen_fn_attrs` set
  `Linkage::AvailableExternally` on every consumer item (per Option 4
  / arch §F.14.1). This is what creates the unreachable-bodied
  AvailableExternally LLVM function definitions that the invariant
  protects against.
- **`rustc-lang-facade/src/queries/per_instance.rs`** —
  `lang_per_instance_mir` returns a synthetic body with ReifyFnPointer
  casts and an `Unreachable` MIR terminator. When rustc lowers this
  to LLVM IR (at compile sessions where the collector calls
  per_instance_mir — see @F.13 for which sessions do), the result is
  another AvailableExternally-with-unreachable body in the IR pool.
  Same hazard, same defense.
- **`toylangc/src/llvm_gen.rs::fill_module`** — Sky's emitter. At
  every compile session where consumer bodies need to be the
  inlinable definition, this is what emits them with `External`
  linkage so they win the IR linker pick over the AvailableExternally
  stubs above.
- **`toylangc/src/toylang/callbacks_impl.rs::consumer_fill_modules`** —
  the gate that decides whether `fill_module` runs at a given compile
  session. Under §5.5 Step 2 narrower revision, fill_module runs at
  BOTH stub_rlib (for owned non-generic items) AND user_bin compiles
  (for generic instantiations). Both sessions must emit Sky's body
  for any item whose stub is also being emitted.

The fixtures that empirically prove the discipline:

- **The inlining matrix** at `toylangc/tests/integration_projects/inlining/`.
  Every fixture's `main` is `unsafe { __toylang_main(); }` (or
  `println(sky_fn())`). If the unreachable stub body got inlined into
  the bin's `main`, the binary would either trap (`udf`/`brk`) on
  invocation or produce wrong stdout (the `expected_output.txt` check
  would fail at test time). 40+ matrix fixtures running this shape
  through every LTO mode + opt-level + codegen-units variant fence
  the invariant.
- **The 8 `_no_lto` fixtures at -O>=1** specifically codify the
  Step-2-era behavior: at `lto = false`, cross-Sky/Rust inlining is
  lost (because thin-local LTO can't span rustc invocations), so the
  bin's `main` tail-jumps to `__toylang_main` — `b __lang_stubs::__toylang_main`.
  These fixtures assert the tail-jump IS present, which incidentally
  also proves SBMNBIZ holds: if the unreachable stub got inlined, the
  jump would be replaced by undefined behavior (or constant-folded to
  nothing), not a clean tail-jump to the real symbol.

## Cross-cutting effect

Adding any new emission path that produces an unreachable-terminated
AvailableExternally body for a Sky-defined symbol creates UB risk
unless the corresponding real body is also emitted (or no caller
exists). Currently the only such emission paths are:

1. Rustc's compilation of `stub_gen`'s emitted source (`unreachable!()`
   body, `AvailableExternally` via codegen_fn_attrs override).
2. The `per_instance_mir` provider's synthetic body (ReifyFnPointer
   casts + `Unreachable` terminator, `AvailableExternally` linkage in
   the lowered LLVM IR).

Future additions to watch:
- **Drop glue** for Sky-defined types (when Sky proper implements its
  drop model). If rustc emits a placeholder drop glue with an
  unreachable body, Sky's drop glue must shadow it.
- **vtable shims** for Sky trait objects.
- **Async state machine `poll` impls** when Sky's async lands.
- **Closure `Fn`/`FnMut`/`FnOnce` impls** lifted to named structs in
  the stub rlib.
- **Comptime-produced types' associated functions** in Sky proper.

Each of these emission paths, if it touches the AvailableExternally +
unreachable pattern, must be cross-referenced with Sky's
fill_extra_modules contribution to ensure the real body is emitted
wherever a caller could exist.

The cross-cutting trap: **the failure is silent**. There's no link
error (the AvailableExternally body provides the inlining target; the
External symbol either exists elsewhere or LLVM doesn't need an
external symbol at all once it inlined). There's no compile error.
The binary builds, runs, and either traps mysteriously at runtime or
produces wrong output. Standard CI test runs only catch this if a
fixture's runtime behavior is sensitive enough to detect the inlined
unreachable.

## Why it exists

The AvailableExternally + Sky-real-body coexistence pattern is
load-bearing for Option 4 (arch §F.14.1). Option 4 retired the A.3
partition filter (which removed consumer items from rustc's CGU list
before LLVM saw them) in favor of letting rustc emit consumer items
into the LLVM IR pool with explicit `AvailableExternally` linkage.
The benefit is architectural: per-item linkage attributes are simpler
than censoring rustc's CGU list. The cost is that rustc's IR pool now
contains the unreachable stub body alongside Sky's real body, and the
IR linker must pick correctly.

LLVM's IR linker rule (External over AvailableExternally for
same-symbol conflicts) is the mechanism that makes this safe — but
only when the External version is actually present. The two
sufficient conditions documented above (real body emitted OR no
caller) cover every compile session shape Sky uses today.

Under the pre-§5.5-Step-2 architecture (all Sky bodies emitted at
user_bin only), the discipline held by construction: Sky's
`fill_extra_modules` emitted the real body at the SAME compile
session where the stub's AvailableExternally body lived. The
single-symbol architecture (arch §F.2) made the names match, and the
IR linker pick gave Sky's real body the inlining slot.

Under §5.5 Step 2 (non-generic Sky bodies emitted at owning crate's
stub_rlib compile), Sky's body moved to a DIFFERENT compile session
from where the bin's main lives. At the bin's user_bin compile, the
stub's AvailableExternally body could exist (from rustc's per_instance_mir
codegen) without Sky's real body. The safety condition switches from
(a) "real body shadows" to (b) "no caller exists in this IR pool" —
arch @F.13 (the `is_reachable_non_generic` collector gate) ensures
per_instance_mir doesn't even fire for non-generic upstream items at
user_bin compile, so no AvailableExternally body is emitted there
either. Empty IR pool, no risk.

The invariant must be maintained whenever future architectural
changes alter:
- WHICH compile sessions emit consumer items' stub bodies (rustc's
  side) — currently driven by Option 4's `codegen_fn_attrs` override
  and the per_instance_mir provider's gate.
- WHICH compile sessions emit consumer items' real bodies (Sky's
  side) — currently driven by `consumer_fill_modules` and Step 2's
  is_user_bin_compile / def_id.is_local() discrimination.

If those two sets of sessions ever diverge such that rustc emits an
AvailableExternally stub at session X but Sky doesn't emit the real
body at session X AND a caller of the symbol does exist at session X,
SBMNBIZ is violated → UB.

## See also

- `rust-interop-architecture.md` §F.14.1 — Option 4 / A.3 retirement,
  the change that introduced the AvailableExternally pattern.
- `rust-interop-architecture.md` §F.13 — the `is_reachable_non_generic`
  collector gate that gives us condition (b) at user_bin compile for
  upstream non-generic items under §5.5 Step 2.
- `rust-interop-architecture.md` §F.2 — single-symbol architecture
  (Path B). SBMNBIZ depends on the stub and Sky's real body sharing
  the same mangled name so the IR linker sees them as candidates for
  the same definition.
- `docs/arcana/SkyMustPinLinkageForExternalRefs-SMPLZ.md` — sibling
  discipline: even after SBMNBIZ ensures the real body wins the
  inlining slot, that body's `.o` symbol must survive LTO/DCE to be
  callable cross-crate. Both invariants must hold.
- `docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md` — earlier in the
  layering: computing a symbol name doesn't drive codegen. SBMNBIZ is
  about what happens to the body AFTER codegen has emitted it.
