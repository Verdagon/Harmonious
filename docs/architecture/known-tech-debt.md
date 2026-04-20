# Known Technical Debt

> Last updated: Phase 6 complete + tech debt #6 resolved + `toy_*`→`lang_*` rename (116 integration + 60 unit + 4 standalone tests passing, 0 ignored)

---

## 5. Generic Function Body Validation

### Problem

`after_rust_analysis` skips generic functions during validation because
`resolve_fn_body` can't resolve unsubstituted `TypeParam` variants. Generic
functions are validated at monomorphization time instead — the two walkers
(`collect_rust_deps_recursive` and `walk_and_stash_internal_callees`) share
the `type_resolve_body` helper, which substitutes concrete type args and runs
`resolve_fn_body`, panicking with a typed `TypeResolveError` if it fails.
This gives decent error messages but means a generic function with a bug
that's never called won't be caught.

### Fix — blocked on trait bounds

The real long-term fix is trait bounds (`fn wrap<T: Clone>(x: T)`) which allow
validating generic bodies at definition time against the bound's interface.
Until then, monomorphization-time validation is the correct approach (same as
C++ templates).

---

## 28. Silent Truncation Hides Non-Default Parent-Type Args (@ETASTZ)

### Problem

`oracle::build_generic_args_for_item` silently discards user-supplied
type args that exceed the item's `Type` slot count. This is load-bearing
for toylang's call-site convention, where `Name::method<T1, T2, ...>()`
names the **type's** generics (not the method's). When the method's impl
block fixes a parent-type arg — `Vec::new` lives on `impl<T> Vec<T, Global>`
with A baked in — the user-supplied value for that slot gets dropped,
and rustc substitutes the impl-block default.

In the common case the default matches what the user wrote: `Vec::new<I32, Global>()`
truncates `Global` and rustc supplies `Global` from the impl. Harmless.

But: if toylang ever gains a way to name a **non-default** parent-type arg
(a custom `Allocator` for `Vec`, a non-default `BuildHasher` for `HashMap`,
or any other type-level generic fixed by its method's impl), the silent
truncation becomes a real bug. `Vec::new<I32, MyAllocator>()` would
silently produce a `Vec<I32, Global>` — the user's intent is lost with
no error. Every test today passes only because toylang has no syntax for
naming a non-default allocator/hasher/etc., so the collision is
unreachable.

### Fix

Either:

1. **Validate truncation at the truncation site.** Query
   `generics_of(parent_def_id).params[i].default_value(tcx)` for each
   slot the user's extras would fill. If the extra matches the default,
   truncate as before. If not, emit a structured error ("you asked for
   `MyAllocator` but `Vec::new` only supports `Global`; use a different
   Vec method or path").

2. **Separate the type-name and method-name at parse/type-resolve.**
   Require toylang source to write `Vec<I32, MyAllocator>::new()` (two
   levels of generics), with the parser producing distinct type-level
   and method-level arg lists. Feed them through separately. More
   invasive, but matches Rust's own turbofish semantics.

Deferred until toylang gains non-default parent-type arg naming or a
standard library Rust API surfaces one (reqwest's future-feature gates
are a candidate; custom allocators are not on the near roadmap).

See the full arcana at `docs/arcana/ExtraTypeArgsSilentlyTruncated-ETASTZ.md`.

---

## 29. Callback-trace test `unexpected` assertions retired

### Problem

The 5 callback-trace integration tests (e.g.
`test_deep_chain_only_entry_point_monomorphized`,
`test_internal_toylang_fn_not_monomorphized_by_rustc`) originally
asserted invariants of the form "`bork` should NOT be monomorphized by
rustc" via their `unexpected` parameter to
`run_integration_project_check_callbacks`. That invariant held under
direct-mode + cold compile (rustc's mono collector only walked
Rust-callable toylang fns), but doesn't hold under wrapper-mode rlib
compile: rustc's mono collector walks every `pub fn` in the stub rlib
regardless of reachability, so `CollectGenericRustDeps` and
`NotifyConcreteEntryPoint` entries fire for every toylang pub fn.

The harness's `unexpected` parameter is now a no-op (kept for call-site
shape stability). The positive-entries check (`expected` parameter)
still verifies B6's populate step runs correctly and `__toylang_main`
+ CGU-level entries are walked — that's the load-bearing signal.

### Fix

Reformulate the negative invariant under a different signal — e.g.
"these fns don't appear as rust-called entry points in
`state.toylang_instances`" — or retire the affected tests entirely and
rely on the behavioral tests (which verify end-to-end: the binary
produces correct output, which requires the facade's walk to be
correct).

Non-blocking; flagged for whoever next touches the integration-test
harness. See `run_integration_project_check_callbacks` in
`toylangc/tests/integration_projects.rs` for the current state and the
risks.md §B6 RESOLVED note for the historical framing.

---

## Resolved Items (sessions 1–8)

| # | Item | Session | Resolution |
|---|------|---------|------------|
| 27 | Non-void `fn main()` tail SIGBUSes at runtime (@MBMRVZ) | 9 | after_rust_analysis now checks: when `name == "main"` and `return_ty.is_none()`, the body's tail must be void. Emits `TypeResolveError::MainMustReturnVoid`. Test: `test_main_non_void_tail_rejected`. |
| 26 | Missing-import ICE at `oracle.rs:112` (@RTMEIZ) | 9 | Converted `resolved_to_rustc_ty` panic into structured `TypeResolveError::RustTypeNotImported { name, context }`. Eight `RustTypeLookupContext` variants (TraitCallSelf / TraitMethodTypeArg / InherentMethodTypeArg / FreeFunctionTypeArg / NestedGenericArg / TraitCallName / TraitMethodName / Codegen — last two added by @IVTDBTZ) with `Display` impls producing actionable messages. Auto-registration was rejected in favor of explicit imports. Test: `test_trait_self_not_imported_gives_error`. |
| 15 | Plan's roguelike expected value wrong | 8 | Corrected alive=2 → alive=1 (g2 collision at (3,3) was missed) |
| 16 | `&&`/`||` equal precedence | 8 | Split `parse_logical` into `parse_logical_or` / `parse_logical_and` |
| 17 | Assignment doesn't type-check RHS | 8 | Added `AssignTypeMismatch` error variant + explicit check |
| 18 | Unary negation hardcodes i32 zero | 8 | Added `Expr::UnaryNeg` AST node, type resolver desugars with correct type |
| 19 | `find_rust_type_def_id` hardcoded diagnostic items | 8 | Already fixed (use imports + `module_children_local`) |
| 20 | String-based struct/type lookups | 8 | Removed redundant `name` field from ToyStruct/ToyFunction; added parser validation for duplicate names and `__toylang_` reserved prefix |
| 21 | Hand-rolled symbol mangling | 8 | Unified all mangling onto `oracle::resolved_type_to_mangled_name`; deleted duplicate `mangle_ty` from facade; added `_LT_`/`_GT_` delimiters for type args to prevent collisions |
| 22 | `FnBody` misnamed | 8 | Renamed to `Block` / `TypedBlock` across all 7 files |
| 23 | Facade stores copies of consumer name sets | 8 | Replaced `type_names()`/`fn_names()` with `is_consumer_type()`/`is_consumer_fn()` callbacks through vtable; removed `CONSUMER_TYPE_NAMES`/`CONSUMER_FN_NAMES` globals and `HashSet` fields from `FacadeGlobals` |
| 6 | FnCall path uses `is_scalar_pair_type` instead of `CoercedParam` | 9 | Migrated FnCall arg loop to `push_arg_for_rust_call` (same per-variant dispatch MethodCall/StaticCall already use). Deleted `is_scalar_pair_type`. FnCall now indexes `coerced_params[i]` (no receiver offset). |
| 7 | Phase 6 partitioner check is inline string-match | 9 | Replaced with `visibility_override` callback on `LangCallbacks`. Rustc fork exposes `rustc_monomorphize::partitioning::VISIBILITY_OVERRIDE_HOOK: OnceLock<fn ptr>`; facade installs the bridge fn in `install_callbacks`; toylang's impl walks `tcx.def_path(...).data` for `__lang_stubs`. String `__lang_stubs` no longer appears in the rustc fork. |
| 24 | Redundant `monomorphize_fn` calls / shallow dep walk | 8 | Deep monomorphization walk split into two walkers: `collect_rust_deps_recursive` (local cycle guard; driven by `collect_generic_rust_deps`) returns Rust deps only; `walk_and_stash_internal_callees` (persistent `walked_entry_points` dedup; driven by `notify_concrete_entry_point`) stashes internal toylang instances in `ToylangState.toylang_instances`. `generate_with_tcx` uses stashed instances instead of MonoItems for toylang functions. Entry-point fns get extern wrappers, internal fns get only internal ABI. Deleted `resolve_function_for_instance` from llvm_gen.rs. |
| 1 | Hardcoded `Vec::new` in type resolver | 6 | Replaced with `rust_method_ret` callback |
| 2 | Method arg inference uses `type_args[0]` | 6 | Explicit typed literals; `expected_ty` eliminated for literals |
| 3 | `mangle_ty_for_symbol` Debug fallback | 6 | Extended match for Str, Ref, RawPtr, Slice, Tuple |
| 4 | `PassMode::Indirect { on_stack: true }` | 6 | Removed assert; both on_stack variants emit Indirect |
| 5 | `PassMode::Pair` as single scalar | 6 | Proper ScalarPair → `{ scalar1, scalar2 }` LLVM struct |
| 6 | Panics instead of user-facing errors | 6 | `TypeResolveError` (8 variants) + `ParseError` (7 variants) |
| 7 | No parser tests | 6 | 7 parser unit tests (all error cases) |
| 8 | No error case integration tests | 6 | 18 error-case unit tests across parser + type resolver |
| 9 | `after_rust_analysis` stub | 6 | 5 validation checks (structs, stubs, Rust types, externs, bodies) |
| 10 | No tests for Rust types other than Vec | — | Accepted: mechanism is general, Vec exercises it |
| 11 | `parse_coerced_type` / `parse_struct_type_str` duplication | 6 | Unified; struct parser delegates to coerced parser |
| 12 | Duplicate field lookup pattern | 6 | Extracted `find_field_index` helper |
| 13 | `resolved_to_rustc_ty` forwarding wrapper | 6 | Deleted; direct `oracle::` calls |
| 14 | `struct_names.clone()` in parser | 6 | Removed from Parser struct; threaded as `&[String]` param |
| — | String-based type resolution | 5 | Unified onto `ResolvedType` |
| — | Vec-specific code | 4 | Generalized to any Rust type |
| — | println hardcoding | 5 | Replaced with extern fn mechanism |
| — | Ref param redundant conversion | 5 | `Scalar(Pointer)` detection emits `"ptr"` |
| — | `__toylang_main` duplication | 5 | `TOYLANG_MAIN` constant |
| — | Method signature heuristic | 5 | `fn_abi_of_instance` queries |
| — | `rust_method_ret` closures | 5 | `tcx.fn_sig()` via oracle |
| — | Duplicated `resolved_type_to_rustc_ty` | 5 | Deleted duplicate |
| — | Dummy Vec constructions | 5 | Deleted |
