# Symbol Mangling Is Not Codegen (SMINCZ)

Computing a symbol name from a rustc query does not commit rustc to actually
emitting that symbol. `tcx.symbol_name(instance)` is a pure read query that
returns the v0 mangled name an Instance *would* have if codegen happened.
`ty::Instance::expect_resolve(...)` similarly returns a typed handle with no
codegen side-effect. Neither one drives monomorphization. Treating them as
if they did has cost two implementers a working linker step.

To force rustc to emit a generic `Instance`, the Instance must appear as a
`Rvalue::Cast(CastKind::PointerCoercion(ReifyFnPointer))` inside a MIR body
that rustc's mono collector walks. The facade's `per_instance_mir` query
provider already does this for every entry in a toylang function's
`rust_deps` list (see `rustc-lang-facade/src/queries/per_instance.rs:106-173`).
The mono collector at `rustc_monomorphize/src/collector.rs:709-717` promotes
ReifyFnPointer targets into `used_items` (authoritative codegen, not
hint-level), which forces emission. This is the only mechanism in the codebase
that drives codegen of a generic Instance that toylang references.

## Where

- **Symptom site (where it looks like it should work):** every
  `tcx.symbol_name(instance)` call in `toylangc/src/llvm_gen.rs` and
  `toylangc/src/oracle.rs`. Also every `ty::Instance::expect_resolve(...)`
  call in those files. None of these drive codegen on their own.
- **Fix site (where codegen actually gets driven):** the `rust_deps`
  vector populated in
  `toylangc/src/toylang/callbacks_impl.rs::collect_toylang_fn_deps_inner`,
  which feeds into `per_instance_mir`'s synthesized MIR body via
  `rustc-lang-facade/src/queries/per_instance.rs::build_dependency_body`.
- **Anti-pattern that masks the bug:** non-generic items get codegen'd
  unconditionally if they exist in the crate (no mono gate). Per-type
  monomorphic shims like `__toylang_option_unwrap_i32` "work" without
  hitting `rust_deps` registration — but only because the shim is non-generic,
  not because the redirect plumbing is correct.

## Cross-cutting effect

If a wrapper, redirect, or replacement function lives in a file that ONLY
calls `tcx.symbol_name` (or `expect_resolve`) and emits an extern declaration
in LLVM IR, the symbol name will be correct, the LLVM module will compile,
and the linker will fail with `undefined symbol`. Three independent failures
have looked exactly like this:

1. **Generic wrapper never instantiated** (no `rust_deps` entry → no
   ReifyFnPointer → mono collector skips it).
2. **Generic wrapper instantiated but partitioner internalizes the symbol**
   (separate concern; see the `__lang_stubs` linkage override in
   `partitioning.rs`).
3. **Symbol exists and visible but caller passes args by wrong ABI**
   (the body runs but reads garbage; not a linker error but the same
   investigation funnel).

The first failure mode is what SMINCZ is about. The other two are downstream
of getting past it.

Workarounds that look attractive but are wrong:
- `#[used]` — preserves emitted symbols from DCE; doesn't drive mono
  for unemitted ones.
- Synthetic `static _: &dyn Fn(...) = &wrapper::<T>` — should drive
  ReifyShim in principle, but ICE'd inside `per_instance_mir` because the
  hook didn't expect a synthetic static. Even if it didn't ICE, it'd
  internalize via the same partitioner path.
- `#[no_mangle]` — changes the symbol name, not the codegen decision.

## Why it exists

Rustc's design assumes generic instantiations are driven by Rust call sites:
the mono collector walks Rust MIR for ReifyFnPointer / function-call terminators
and emits whatever it finds. External LLVM IR is invisible to this walk by
construction — it's a byproduct, not an input.

The fork's `per_instance_mir` query is the bridge: for each toylang function
that gets compiled, we synthesize a MIR body whose only purpose is to mention
every Rust dep as a ReifyFnPointer. Rustc thinks these are real calls; the
mono collector walks them and codegens the deps. The synthesized body itself
is unreachable (terminator is `Unreachable`), so the references are
"mention-only" from a runtime perspective but "use" from the collector's
perspective — exactly what the partitioner needs to commit to emission.

The trap is that the codegen-driving mechanism (`rust_deps` registration) is
in a different file from the symbol-string consumer (`llvm_gen.rs`). Both
sites must compute the same Instance. The symbol-string consumer LOOKS like
it's the place where the dependency is created, because it's the place where
the symbol enters the LLVM IR. It isn't. The dep was created upstream — or
should have been — and silently nothing happens if it wasn't.

**Rule:** if the only line that changes when adding a new Rust callee is a
`tcx.symbol_name` call site, you're in the wrong file. The change must also
land in the dep-registration path that feeds `per_instance_mir`.

## See also

- `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` — separate concern in the
  same area (mutex layout for query providers).
- `docs/historical/phase6-attempt1-mono-not-generated.md` — first attempt's
  writeup. Hit failure mode 1 directly.
- `docs/historical/phase6-attempt2-linkage-visibility.md` — second attempt's
  writeup. Got past failure mode 1 by registering deps correctly, then hit
  failure mode 2 (partitioner internalization).
