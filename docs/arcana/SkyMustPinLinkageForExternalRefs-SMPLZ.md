# Sky Must Pin Linkage For External Refs (SMPLZ)

Sky's consumer-language emitter (toylangc today, Sky proper later) produces
LLVM function definitions for rustc-visible symbols whose only callers live
in OTHER compile units — typically the stub rlib's machine code calling
into Sky's emitted bodies via a rustc-mangled name. Within Sky's CGU module
there is no in-module caller, so three independent LLVM passes will try to
remove or rewrite these symbols unless we pin them:

1. **`GlobalOpt` + `GlobalDCE`** at -O>=2 (non-LTO and LTO pre-merge): mark
   the function as dead (no in-module use) and delete it.
2. **LTO `internalize`** at `lto = "fat"` (and to a lesser degree
   `"thin"`): change linkage from `External` to `Internal` because no
   callers in the merged module reach it. The function survives in the
   final `.o` as a *local* definition (objdump: `l F` instead of `g F`),
   silently breaking cross-crate references at link time.
3. **Linker dead-strip** (`-Wl,-dead_strip` on macOS, `--gc-sections` on
   ELF): remove the symbol entirely from the final binary because nothing
   in the final-link input set references it.

The defense is `@llvm.used`. It is the only LLVM directive that defeats
all three passes. The weaker `@llvm.compiler.used` variant defeats only
(1); fixtures running `lto = "fat"` will silently fail at link with errors
identical to the share-generics-gate disambig mismatch (B14), which sends
the investigation down the wrong trail.

The rule: **any Sky-emitted symbol whose intended caller is in another
compile unit's machine code MUST be pinned in `@llvm.used`.** The check is
trivial — Sky already knows which emissions are "rustc-visible" (they
carry a `stub_def_id`). Whenever Sky adds a new emission path that
produces a rustc-visible symbol, the corresponding entries must flow
through `pin_in_llvm_used`.

## Where

- **Pin site:** `toylangc/src/llvm_gen.rs::pin_in_llvm_used`. Called from
  `fill_module` after `apply_rust_compat_attributes`. Walks `fn_items`
  filtered by `instance.is_some()` (the proxy for "rustc-visible") and
  appends every extern symbol to `@llvm.used`.
- **Add sites:** every call to `ctx.module.add_function(extern_symbol, ...)`
  inside `codegen_extern_wrapper`. Each one produces a rustc-visible
  symbol that needs pinning. The current implementation funnels all of
  these through the `fn_items` collection, so the pin happens once at
  the end. Future emission paths that produce rustc-visible symbols
  outside `codegen_extern_wrapper` (drop glue, vtables, async state
  machines) MUST either route through the same collection or call
  `pin_in_llvm_used` themselves.
- **Fixture proving the discipline:**
  `toylangc/tests/integration_projects/opt_level_3_fat_lto_smoke/` +
  `test_opt_level_3_fat_lto_smoke`. Builds the standard
  `case_generic_impl_block` shape at `opt-level = "3" + lto = "fat"`
  and runs the resulting binary. Reverting the pin to
  `@llvm.compiler.used` reproduces the bug; switching back to
  `@llvm.used` clears it.

## Cross-cutting effect

The failure mode is silent and the resulting linker error is
*byte-identical* to the share-generics-gate disambig mismatch (B14):

```
Undefined symbols for architecture arm64:
  "__RNvXs_Cs..._12___lang_stubsINtB4_7WrapperlENtNtCs..._4core5clone5Clone5cloneB4_",
    referenced from:
      __RINv...duplicate... in lib__lang_stubs-XXX.rlib[N](...rcgu.o)
```

The disambig in the symbol name is correct (so the share-generics fixes
are intact). The trap is that the surface error looks identical to B14,
so the natural first move is to inspect the share-generics gate — when
in fact the gate is firing correctly and the problem is the linkage of
Sky's emitted symbol got demoted during LTO `internalize`.

The disambiguating check is `llvm-objdump -t` on the user_bin's post-LTO
`.o` file:

- `g     F __TEXT,__text _<symbol>` — the symbol is a global definition.
  Working. The B14 chain is at fault (gate or augmented-map issue).
- `l     F __TEXT,__text _<symbol>` — the symbol is a local definition.
  SMPLZ at fault: the `@llvm.used` pin is missing or wrong (e.g.,
  `@llvm.compiler.used` instead). The cross-crate reference can't
  resolve because the linker doesn't see the symbol as exportable.

The cross-cutting effect propagates forward: every NEW Sky emission path
that produces a rustc-visible symbol will silently break under fat LTO
unless its symbols are flowed into `pin_in_llvm_used`. Future Sky work
that's at risk:

- Drop glue for Sky-defined types (when Sky proper implements its drop
  model).
- vtable shims for trait objects.
- Async state machine `poll` impls (when Sky's async lands).
- Closure `Fn`/`FnMut`/`FnOnce` impls.
- Comptime-produced types' associated functions.

Each of these emission paths needs the same pinning discipline.

## Why it exists

`@llvm.compiler.used` and `@llvm.used` are two intentionally-different
LLVM directives:

- `@llvm.compiler.used`: "the compiler must not remove this symbol via
  IR-level passes (DCE), but the linker is free to dead-strip it." Used
  by rustc for its own symbols where the linker has full visibility
  into uses.
- `@llvm.used`: "this symbol must survive to the output object AND keep
  its external linkage AND survive linker dead-strip." Used for cases
  where the linker cannot see all uses — e.g., symbols referenced from
  hand-written assembly or from a different compile unit's already-
  compiled object code.

Sky's case is the second one: the user_bin's compile cannot see that the
stub rlib's pre-compiled `.rcgu.o` references the symbol, because the
stub rlib's `.o` is linker-fed as opaque machine code, not as bitcode
participating in LTO's IR-level analysis. From the IR linker's perspective,
Sky's emitted symbol has zero users; without `@llvm.used`, internalize
demotes it. With `@llvm.used`, internalize is forced to leave it as
External and the linker sees it as exportable.

The cost of `@llvm.used` over `@llvm.compiler.used` is that the linker no
longer dead-strips these symbols even when truly dead. For Sky's specific
case, every pinned symbol is by construction reachable from another crate
(otherwise we wouldn't be emitting it under a rustc-mangled name), so the
linker would have kept it anyway. The trade is essentially free.

A natural worry — "won't this bloat binaries with dead Sky exports?" —
doesn't apply because Sky only emits bodies for items that rustc's mono
collector reached at SOME compile in the build graph. Anything genuinely
unreachable across the whole build graph never gets emitted in the first
place.

## See also

- `rust-interop-architecture.md` §25.2 B15 — the closed-risk entry with
  fix history.
- `rust-interop-architecture.md` §25.2 B14 — the previous closed risk
  with byte-identical error symptoms.
- `docs/arcana/SymbolManglingIsNotCodegen-SMINCZ.md` — sister discipline:
  computing a symbol name doesn't drive codegen. SMPLZ is the next layer
  down: even after codegen runs, the symbol's *linkage* must survive
  LTO. Both layers must be correct or the link fails.
