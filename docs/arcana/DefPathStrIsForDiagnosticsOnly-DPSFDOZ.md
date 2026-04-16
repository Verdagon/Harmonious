# DefPathStr Is For Diagnostics Only (DPSFDOZ)

`tcx.def_path_str(def_id)` is implemented in terms of `trimmed_def_paths`,
which ICEs with `'trimmed_def_paths' called, diagnostics were expected but
none were emitted. Use 'with_no_trimmed_paths' for debugging.` if invoked
outside a diagnostic-permissive scope. "Diagnostic-permissive" effectively
means "during error reporting" — rustc internally tracks whether any
diagnostic has been emitted in the current session and panics on
`def_path_str` calls that imply diagnostics machinery without one being
in flight.

For non-diagnostic code (query providers, partitioner, codegen hot paths,
build-time path matching), use `tcx.def_path(def_id).data` and walk the
`DefPathData` components directly. Match on
`DefPathData::TypeNs(sym)` for module/type components,
`DefPathData::ValueNs(sym)` for fn/const components, etc.

## Where

- **The unsafe call:** `rustc-lang-facade/src/lib.rs:218-221` —
  `is_from_lang_stubs` uses `tcx.def_path_str(def_id).starts_with("__lang_stubs::")`.
  This is currently safe only because every caller invokes it from inside
  `generate_and_compile`, where consumer codegen runs and rustc's diagnostic
  machinery happens to be permissive. Any future caller from a different
  context (a query provider, a hook outside generate_and_compile, the
  forked rustc itself) will ICE.
- **The safe pattern:** `rust/compiler/rustc_monomorphize/src/partitioning.rs`
  in `mono_item_linkage_and_visibility` — checks for `__lang_stubs` by
  walking `tcx.def_path(def_id).data` for a `DefPathData::TypeNs` named
  `__lang_stubs`. This runs during partitioning (deep inside normal
  compilation, no diagnostics) and must not use `def_path_str`.

## Cross-cutting effect

The footgun is asymmetric: `def_path_str` works in MOST places, fails in a
few. The few are exactly the places where you might want to use it for
matching (query providers, hooks). A grep for `def_path_str` in the facade
will find one occurrence that looks innocent; expanding its caller set is
the dangerous move, not introducing a new use.

The first time this was hit (2026-04-15, while patching the rustc fork's
partitioner), the ICE message blamed `compiler_builtins` and pointed at
diagnostic-trimming code that has nothing to do with the actual call. Several
minutes were spent suspecting the partitioner patch logic before realizing
the API itself was unsuitable for that context.

## Why it exists

Rustc maintains two views of an item's path: the canonical one
(`def_path(def_id)`) and the trimmed/diagnostic one (`def_path_str`). The
trimmed one elides crate prefixes and uses module re-export shortest-paths
for human readability in error messages. Computing it is expensive and
context-dependent (which crates are visible, what aliases the user has),
so it's gated behind the assertion that diagnostics are actually being
emitted. Outside diagnostics, the assertion fires and aborts compilation.

There is no soft mode that returns "no, can't do this here" — it's
ICE-or-succeed. So the rule is binary: never use `def_path_str` from code
that runs during normal compilation (query providers, mono collection,
partitioning, codegen passes, link-time decisions). For matching, use
`def_path(def_id).data` and inspect the `DefPathData` enum.

## See also

- `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` — covers another implicit
  scope constraint on facade APIs (which mutex they're allowed to lock from
  where).
