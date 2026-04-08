# Plan: Generalize Vec-Specific Code to Support Arbitrary Rust Types

> **Status: COMPLETE.** All 53 integration tests + 10 unit tests passing.
> Zero compiler warnings.

## Context

Vec was the only Rust generic type toylang supported. All Vec support was
hardcoded by name ("Vec", "new", "push", "len") across ~200 lines in 4 files.
This refactor generalized that code so adding a second Rust type (HashMap,
String, etc.) requires zero compiler changes — just `use` it and call methods.

## Design Decisions

**D1: Explicit type args, no default filling.** Users provide all type args:
`Vec::new<Point, Global>()`. Eliminated `extract_global_ty`.

**D2: Rust method return types via callback.** `resolve_fn_body` takes a
`rust_method_ret: &dyn Fn(&str, &str, &[ResolvedType]) -> ResolvedType`
callback. Production closures are hardcoded for now (push→Void, len→Usize,
new→RustType). Unit tests supply their own closures.

**D3: Unified codegen for Rust methods.** StaticCall and MethodCall both use
`rust_method_info` keyed by mangled symbol. `fn_abi`-based sret detection
planned but not yet implemented (currently uses method-name heuristic).

**D4: Merged dep discovery.** `collect_rust_deps` deleted entirely.
`collect_toylang_fn_deps` handles both toylang and Rust deps from one typed
AST walk.

**D5: `use` imports.** Toylang `use std::alloc::Global` → stub generator emits
`pub use std::alloc::Global;` → rustc compiles it → `find_local_struct_def_id`
resolves it. No hardcoded type lookups for imported types.

## What Was Implemented

### Step 1: Mechanical rename `ResolvedType::Vec` → `RustType` ✅

Straightforward. `replace_all` for match arms, update unit test assertions.
No surprises.

### Step 2: Add `type_args` to `TypedExprKind::StaticCall` ✅

Added field, updated pattern matches. No surprises.

### Step 3: Generalize oracle.rs ✅

Renamed `find_vec_method` → `find_inherent_method` (takes `type_def_id` param).
Added `find_rust_type_def_id`. Kept `find_vec_method` as convenience wrapper
temporarily (deleted in step 8). `extract_global_ty` kept temporarily (deleted
in step 8).

### Step 4: Add `rust_method_ret` callback to type resolver ✅

Threaded callback through `resolve_fn_body` → `resolve_stmt` → `resolve_expr`
(~10 call sites to update).

**Surprise:** MethodCall on `&Vec<T>` (Ref-wrapped receiver) wasn't handled.
The new general MethodCall arm matched on `RustType` but not `Ref { inner:
RustType }`. Fixed by extracting type info from both direct and ref-wrapped
receivers.

**Decision:** Production closures in callbacks_impl.rs and llvm_gen.rs are
hardcoded tables for now (push→Void, len→Usize, new→RustType). The callback
architecture is in place for future `tcx.fn_sig()` queries.

### Step 5: Merge dep discovery ✅

Replaced `walk_typed_body_for_fn_calls` with `walk_typed_body_for_deps` that
collects both `fn_calls` and `rust_method_deps`. Added `RustMethodDep` struct
and `resolved_to_rustc_ty` helper in callbacks_impl.rs.

**Surprise:** After removing Global auto-fill, `Vec::push` and `Vec::len` deps
failed with "type parameter A/#1 out of range when instantiating, args=[Point]".
The dep resolution built generic args from `RustType.type_args` which only had
1 arg (element type) — push/len need 2 (element + allocator). Temporarily
re-added Vec-specific Global fill-in for dep resolution (removed in step 8).

Deleted ~150 lines of Vec-specific helpers from callbacks_impl.rs.

### Step 6: Generalize codegen method resolution ✅

Replaced `vec_fns: HashMap<String, FunctionValue>` with
`rust_method_info: HashMap<String, RustMethodInfo>` keyed by mangled symbol.
Added `get_or_resolve_rust_method` with lazy caching. Added
`resolve_rust_methods_from_typed_body` to walk typed AST for method discovery.

Updated StaticCall and MethodCall arms to use `rust_method_info` instead of
`vec_fns`. Deleted ~200 lines of Vec-specific helpers from llvm_gen.rs.

**Surprise:** `parse_generic_type` (kept for extern wrapper) was still needed
by `struct_type_for_ret`. Accidentally deleted it with the Vec helpers, had to
re-add it.

**Note:** `resolve_rust_ty_from_string` (string-based type resolver) became
unused after `resolve_vec_symbols` deletion — deleted it too.

### Step 7: Unified `call_rust_function` codegen ✅

Done inline in step 6 — StaticCall and MethodCall arms both use
`rust_method_info` directly. Decided not to extract a separate
`call_rust_method` function since the inline code is clear enough and the
two arms have different receiver handling.

### Step 8: `use` imports + explicit Global ✅

This step had the most surprises.

**8a-8d: Import infrastructure** — straightforward. Parser recognizes `use`
keyword, stores path in `registry.imports`, stub generator emits `pub use`.
No new lexer tokens needed (`use` is just an Ident, `::` already existed as
DoubleColon).

**Surprise 1: `#![feature(allocator_api)]`** — `std::alloc::Global` is behind
an unstable feature gate. The `pub use` in `__lang_stubs.rs` compiled fine
syntactically, but rustc rejected it at the type level. `#![feature(...)]`
must be at the crate root, not in a submodule. Fix: each integration test's
Rust source (the crate root) adds `#![feature(allocator_api)]`.

**Surprise 2: `find_local_struct_def_id` didn't find re-exports** — The
function walked `hir_crate_items().definitions()` checking `DefKind::Struct`.
`pub use` items have `DefKind::Use`, not `Struct`. Calling `item_name()` on
`Use` items at the crate root ICE'd rustc. Fix: added `find_reexported_type`
that uses `tcx.module_children_local()` to walk module children and find
re-exported structs. Had to use `module_children_local` (not `module_children`)
because the latter only works for external crates.

**Surprise 3: Three dummy Vec constructions with incomplete type args** — Three
places in llvm_gen.rs created `RustType { name: "Vec", type_args: [I32] }` as
a dummy type to query Vec's opaque layout size ("all Vec<T> have identical
layout"). With explicit Global, these produced `Vec<i32>` with 1 arg instead
of 2. Fix: added `Global` as second type arg to all three.

**Surprise 4: `parse_field_type` didn't handle unknown names** — The struct
field parser only recognized primitives, type params, known struct names, and
generic types (with `<`). `Global` as a non-generic unknown name hit the error
path. Fix: treat unknown names as `RustGeneric(name, vec![])` (opaque Rust
types).

**Surprise 5: `resolve_field_type` only handled Vec** — The `RustGeneric` arm
in type_resolve.rs only matched `"Vec"` and panicked on anything else. Fix:
generalized to handle any `RustGeneric` name.

**Surprise 6: `parse_generic_type` naive comma splitting** — The string-based
`parse_generic_type("ToyWrapper<Vec<i32, Global>>")` used `split(',')` which
broke nested generics into `["Vec<i32", "Global>"]`. Fix: rewrote to track
angle bracket depth when splitting.

## Functions Deleted

| Function | File |
|----------|------|
| `extract_global_ty` | oracle.rs |
| `find_vec_method` | oracle.rs |
| `collect_rust_deps` | callbacks_impl.rs |
| `find_vec_elem_from_explicit_type_args` | callbacks_impl.rs |
| `scan_body_vec_ops` / `scan_expr_vec_ops` | callbacks_impl.rs |
| `find_vec_in_fields_recursive` | callbacks_impl.rs |
| `find_vec_elem_ty` | callbacks_impl.rs |
| `resolve_vec_symbols` | llvm_gen.rs |
| `resolve_rust_ty_from_string` | llvm_gen.rs |
| `body_uses_vec` / `stmt_uses_vec` / `expr_uses_vec` | llvm_gen.rs |
| `find_vec_elem_from_explicit_ast` | llvm_gen.rs |
| `find_vec_elem_name` / `find_vec_in_struct_fields` | llvm_gen.rs |
| `find_vec_elem_from_params` / `resolve_field_type_name` | llvm_gen.rs |

## Functions Added

| Function | File |
|----------|------|
| `find_inherent_method(tcx, type_def_id, method)` | oracle.rs |
| `find_rust_type_def_id(tcx, name)` (diagnostic items + local fallback) | oracle.rs |
| `find_reexported_type(tcx, name)` (module_children_local) | oracle.rs |
| `get_or_resolve_rust_method(type_name, method, type_args)` | llvm_gen.rs |
| `RustMethodInfo` struct + `rust_method_info` field | llvm_gen.rs |
| `resolve_rust_methods_from_typed_body` | llvm_gen.rs |
| `resolved_to_rustc_ty` | callbacks_impl.rs |
| `RustMethodDep` + `walk_typed_body_for_deps` | callbacks_impl.rs |
| `use` parsing in `parse_program` | parser.rs |
| `pub use` emission in `generate` | stub_gen.rs |
| `imports: Vec<String>` field | registry.rs |
| `test_rust_method_ret` callback | type_resolve.rs (tests) |

## Remaining Vec-Specific Code (acceptable)

- **`get_or_resolve_rust_method` method signature heuristic:** Uses method name
  ("new" → sret, "push" → ptr+ptr+void, "len" → ptr+usize) to determine LLVM
  function signature. Should be replaced with `fn_abi_of_instance` queries.
- **Production `rust_method_ret` closures:** Hardcoded push→Void, len→Usize.
  Should query `tcx.fn_sig()` for actual return types.
- **Three dummy `Vec<I32, Global>` for layout queries:** Used to get opaque Vec
  size. Harmless but not general.
- **`find_rust_type_def_id` diagnostic item match for "Vec":** One line. Could
  be removed if Vec is always imported via `use`, but diagnostic items are
  faster than module_children_local search.

## Verification

```bash
cargo +rustc-fork test -p toylangc --test integration_tests  # 53 passed
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve  # 10 passed
cargo +rustc-fork check -p toylangc  # 0 warnings
```
