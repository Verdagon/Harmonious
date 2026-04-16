# Known Technical Debt

> Last updated: Phase 6 complete + tech debt #6 resolved + `toy_*`→`lang_*` rename (116 integration + 60 unit + 4 standalone tests passing, 0 ignored)

---

## 26. Auto-register Rust types encountered during conversion (@RTMEIZ)

### Problem

Every Rust type that flows through toylang's type system must be
`use`-imported in the toylang source, even types the source never
names explicitly: the `Self` of a trait-method call, the return type
of a discarded tail expression, the generic args of a nested type.
Missing an import surfaces as a runtime panic inside toylangc
(`Rust type '<name>' not found` at `oracle.rs:112`) instead of a
clean compile error.

See `docs/arcana/RustTypesMustBeExplicitlyImported-RTMEIZ.md` for the
full mechanism and `docs/usage/writing-main.md` for the practical
rule.

### Fix

`rustc_ty_to_resolved_type` at `toylangc/src/oracle.rs` already has
the `DefId` of every Rust type it encounters at the moment it builds
a `ResolvedType::RustType { name, ... }`. Cache the `(name, DefId)`
pair in a `LazyLock<Mutex<HashMap<String, DefId>>>` at that site.
Update `find_rust_type_def_id` to consult the cache before falling
back to the `pub use` walk. This removes the "did you remember to
`use` it?" hazard entirely.

Risks: name collisions between distinct types sharing a base name
(e.g., `std::io::Error` vs `std::fmt::Error`). Mitigation: store a
`Vec<DefId>` per name and make the resolver require disambiguation
when more than one is registered, matching today's `pub use` behavior
(which already errors on duplicate re-exports).

### Better-UX alternative

Even without auto-registration, reject the panic at type-resolve time
with a structured error ("Rust type `Stdout` used implicitly as Self
of `Write::write_all` but not `use`-imported; add `use std::io::Stdout`").
One-line check in `type_resolve.rs` that wraps the `resolved_to_rustc_ty`
call in a pre-flight `find_rust_type_def_id` and converts `None` into
a typed error variant. Cheap, fully compatible with the auto-register
fix above, and transforms silent SIGBUSes / rustc ICEs into clear
compile errors.

---

## 27. Reject non-void `fn main()` tail expressions (@MBMRVZ)

### Problem

Toylang's `fn main()` body must have a void-typed tail expression. If
it doesn't, the internal form of main grows an sret return, while the
extern wrapper `__toylang_impl_main` is fixed at `fn __toylang_main()`
(void) because the auto-generated Rust shim does
`fn main() { __toylang_main(); }`. The mismatch produces a SIGBUS
during the internal body's final store into a dangling sret buffer,
*after* all side effects complete and output has been printed.

See `docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md` for the full
cross-cutting explanation and `docs/usage/writing-main.md` for the
authorial rule.

### Fix

At type-resolve time (`toylangc/src/toylang/type_resolve.rs`), when
resolving a function named `"main"`, require the block's inferred
return type to be `ResolvedType::Void`. Emit a new
`TypeResolveError::MainMustReturnVoid` variant with a message that
tells the user exactly what to do:

> `fn main()` must return `()`. The last statement's type is
> `Result<(), Error>` — add `;` to discard it, or end main with a
> void-typed expression.

Transforms a silent SIGBUS-at-runtime into an actionable compile
error.

---

## 5. Generic Function Body Validation

### Problem

`after_rust_analysis` skips generic functions during validation because
`resolve_fn_body` can't resolve unsubstituted `TypeParam` variants. Generic
functions are validated at monomorphization time instead — `collect_toylang_fn_deps`
substitutes concrete type args and runs `resolve_fn_body`, panicking with a
typed `TypeResolveError` if it fails. This gives decent error messages but means
a generic function with a bug that's never called won't be caught.

### Fix — blocked on trait bounds

The real long-term fix is trait bounds (`fn wrap<T: Clone>(x: T)`) which allow
validating generic bodies at definition time against the bound's interface.
Until then, monomorphization-time validation is the correct approach (same as
C++ templates).

---

## Resolved Items (sessions 1–8)

| # | Item | Session | Resolution |
|---|------|---------|------------|
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
| 24 | Redundant `monomorphize_fn` calls / shallow dep walk | 8 | Deep monomorphization walk: `collect_toylang_fn_deps_inner` recursively walks toylang callees, only returns Rust deps to rustc. Internal toylang instances stashed in `ToylangState.toylang_instances`. `generate_with_tcx` uses stashed instances instead of MonoItems for toylang functions. Entry-point fns get extern wrappers, internal fns get only internal ABI. Deleted `resolve_function_for_instance` from llvm_gen.rs. |
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
