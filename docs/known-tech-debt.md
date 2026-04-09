# Known Technical Debt

> Last updated: session 7 (74 integration tests + 31 unit tests passing, 0 ignored)

---

## 1. String-Based Struct/Type Lookups

### Problem

Structs and types are looked up by string name throughout the codebase:
`registry.structs.get(name)` (~8 sites across type_resolve.rs, callbacks_impl.rs,
llvm_gen.rs) and `find_rust_type_def_id(tcx, name)` (~7 sites). All live in a
flat namespace with no module qualification. Risk of name collisions if a struct
and function share the same name, or if types from different modules clash.

### Fix Options

**Option A: Resolve to indices during parsing.** Parser assigns each struct a
numeric ID. Registry becomes `Vec<ToyStruct>` indexed by ID. All references
carry the ID instead of the name string.

**Option B: Resolve to DefId during type resolution.** After stub generation,
each toylang struct has a `DefId`. The type resolver stores `DefId`s alongside
names in `ResolvedType::Struct` / `ResolvedType::RustType`. Downstream code
uses the `DefId` directly.

---

## 2. Hand-Rolled Symbol Mangling

### Problem

Two independent mangling functions build symbols by hand:

- `compute_fn_symbol` + `mangle_ty_for_symbol` in `callbacks_impl.rs:461-501`
  (39 lines) — builds `__toylang_impl_wrap__i32` from rustc `Ty<'tcx>` types
- `resolved_type_to_mangled_name` in `llvm_gen.rs:1191-1213` (22 lines) —
  builds mangled names from `ResolvedType` for internal symbols

No unified mangling scheme. Doesn't use `tcx.symbol_name()`. Could produce
collisions or non-demanglable names for complex types (nested generics, tuples).

### Fix Options

**Option A: Use `tcx.symbol_name()` for toylang functions.** Build a proper
`Instance` for each toylang function and use rustc's mangling. Requires toylang
functions to have valid `DefId`s (they already do via stubs).

**Option B: Keep hand-rolled but unify.** Merge the two mangling functions into
one that operates on `ResolvedType` (already covers all variants exhaustively).
Accept that the scheme diverges from rustc's mangling.

---

## 3. `find_rust_type_def_id` Hardcoded Diagnostic Items

### Problem

In `oracle.rs:192-202`, only `"Vec"` is mapped to a rustc diagnostic item via
`sym::Vec`. All other Rust type names fall through to `find_local_struct_def_id`,
which scans local definitions by name string. This won't find std library types
like `HashMap`, `String`, `Box` unless they're re-exported via `pub use` in
`__lang_stubs.rs`.

### Fix Options

**Option A: Expand the diagnostic item map.** Add entries for commonly used Rust
types: `String` → `sym::String`, `HashMap` → `sym::HashMap`,
`Box` → `sym::Box`, `Option` → `sym::Option`, etc.

**Option B: Require `use` imports for all Rust types.** The `use` import
mechanism already works (`use std::alloc::Global`). Require all Rust types
to be imported, then look them up via `find_local_struct_def_id` which finds
re-exported types through `module_children_local`. No hardcoded diagnostic items
needed.

---

## 4. Redundant `monomorphize_fn` Calls

### Problem

`monomorphize_fn` is called 3 times per consumer function instance:
1. From `per_instance_mir` (facade, dep discovery + symbol)
2. From `symbol_name` (facade, symbol only)
3. From `generate_with_tcx` (llvm_gen.rs, symbol + type resolver for codegen)

Each call recomputes the extern symbol. Calls 1 and 3 also run the full type
resolver on the body. Correct but wasteful.

### Fix Options

**Option A: Cache in `ToylangCallbacks`.** Use
`Mutex<HashMap<String, CachedMonoResult>>` keyed by extern symbol (which is
lifetime-free and unique per Instance). `per_instance_mir` populates on first
call. `symbol_name` reads cached symbol. `generate_and_compile` reads cached
TypedFnBody. Can't key by `Instance<'tcx>` directly (has lifetime).

**Option B: Accept the redundancy.** `resolve_fn_body` is cheap (no LLVM, just
scope tracking). The redundancy is ~3x a fast operation. Only optimize if
profiling shows it matters.

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

## 6. `FnBody` Misnamed

### Problem

`ast::FnBody` (`{ stmts: Vec<Stmt>, ret: Option<Expr> }`) is used for function
bodies, if/else branches, and while loop bodies. The name `FnBody` is misleading
since it's really a general "block" construct. Same for `typed_ast::TypedFnBody`.

### Fix

Rename to `Block` / `TypedBlock` (or `CodeBlock`). Mechanical rename — ~20
references across ast.rs, typed_ast.rs, parser.rs, type_resolve.rs, llvm_gen.rs,
callbacks_impl.rs.

---

## Resolved Items (sessions 1–7)

| # | Item | Session | Resolution |
|---|------|---------|------------|
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
