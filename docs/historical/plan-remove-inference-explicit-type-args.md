# Plan: Remove all type inference, require explicit type args

## Context

The toylang compiler has accumulated inference machinery that's fragile and
caused bugs (generic type inference breaking let bindings, Vec element type
heuristics failing on non-struct-literal push args). Since this is a proof of
concept, simplicity wins. Remove all inference and require explicit type args.

Also fixes several bugs from the audit (lexer skipping unknown chars, silent
empty-deps, Vec method lookup by substring, empty registry on missing input).

## Current state

4 tests are broken (regressions from the inference fixes attempted earlier):
- `test_generic_wrap`, `test_generic_wrap_via_concrete`,
  `test_generic_callee_in_let`, `test_generic_callee_with_struct`

4 bug-exposing tests exist as `#[ignore]`:
- `test_vec_method_lookup_is_exact`, `test_vec_push_fn_call_result`,
  `test_lexer_rejects_unknown_chars`, `test_int_literal_infers_i64_from_return_type`

The Vec method lookup fix (#1) and lexer fix (#5) and empty registry fix (#6)
are already implemented. The `infer_let_types_from_return` addition is the cause
of the 4 regressions and needs to be reverted.

## Changes

### 1. Parser — add type args to FnCall and StaticCall

**File:** `toylangc/src/toylang/parser.rs`

In `parse_primary`, after consuming an identifier and seeing `<` (before `(`),
parse type args. Currently `IDENT(args)` → FnCall. Change to:

- `IDENT<T1, T2>(args)` → FnCall with type_args
- `IDENT(args)` → FnCall with empty type_args (concrete call)

**Ambiguity:** `IDENT<` could be a FnCall with type args or a BinaryOp with
less-than. For now, always treat `IDENT<` as type args when followed by
identifiers and `>`. This works because toylang doesn't have comparison operators
yet. When comparisons are added, we'll need lookahead disambiguation.

Similarly for StaticCall: `Ty::method<T1>(args)`.

Also add `type_args` parsing to `Vec::new<Point>()` syntax — currently
StaticCall has no type args field.

### 2. AST — add type_args to FnCall and StaticCall

**File:** `toylangc/src/toylang/ast.rs`

```rust
FnCall { name: String, type_args: Vec<String>, args: Vec<Expr> },
StaticCall { ty: String, method: String, type_args: Vec<String>, args: Vec<Expr> },
```

### 3. Type resolver — remove all inference, use explicit type args

**File:** `toylangc/src/toylang/type_resolve.rs`

**Remove entirely:**
- `infer_type_args_from_expected()` 
- `infer_type_params_from_arg()`
- `infer_let_types_from_return()`
- `infer_vec_types()` and all helper functions (`infer_from_push`,
  `infer_from_struct_field`, `infer_from_struct_lit`, `infer_expr_type`,
  `is_vec_new`)
- `vec_inferences` and `let_type_hints` parameter threading through
  `resolve_fn_body`, `resolve_stmt`, `resolve_expr`

**Modify the generic FnCall arm:**
- Instead of inferring type_args, read them from `expr.type_args`
- Build substitution map directly: `func.type_params[i] → type_args[i]`
- Panic if `type_args.len() != func.type_params.len()` for generic functions

**Modify the StaticCall arm (Vec::new):**
- Read element type from `expr.type_args[0]` instead of from `vec_inferences`
- `Vec::new<Point>()` → type_args = ["Point"] → elem type = Point

**Simplify `resolve_stmt` for Let:**
- No `vec_inferences` or `let_type_hints` — just resolve with `Void` expected
- The expression's own type_args provide all needed type information

### 4. Update callbacks_impl.rs

**File:** `toylangc/src/toylang/callbacks_impl.rs`

- `scan_expr_vec_ops`: add arm for new `FnCall` with type_args field
- `collect_rust_deps`: remove the Vec inference fallbacks. Instead, scan the
  AST for `StaticCall { ty: "Vec", type_args: [elem], .. }` to find the element
  type directly from the explicit type arg.
- Remove the `find_vec_elem_from_typed_body` call and the silent empty-deps
  fallback — with explicit type args, Vec element type is always known.

### 5. Update llvm_gen.rs

**File:** `toylangc/src/llvm_gen.rs`

- `codegen_internal_function` Vec symbol resolution: get elem type from the
  AST's `StaticCall` type_args instead of heuristic scanning
- Remove `find_vec_elem_from_body` and `find_vec_elem_from_typed_body`
- Remove `body_uses_vec`, `find_vec_elem_name`, `find_vec_elem_from_params`
- The `StaticCall` codegen arm for `Vec::new` uses `type_args[0]` to resolve
  the element type

### 6. Update tests

**File:** `toylangc/tests/integration_tests.rs`

All generic function calls need explicit type args:

| Test | Before | After |
|------|--------|-------|
| `test_generic_wrap` | `wrap(42i32)` | `wrap<i32>(42)` |
| `test_generic_wrap_via_concrete` | `wrap(x)` in `wrap_i32` | `wrap<i32>(x)` |
| `test_generic_callee_with_struct` | `identity(p)` | `identity<Point>(p)` |
| `test_generic_callee_in_let` | `wrap(42)` | `wrap<i32>(42)` |

All Vec::new calls need element type:

| Pattern | Before | After |
|---------|--------|-------|
| Vec of structs | `Vec::new()` then `v.push(Point{...})` | `Vec::new<Point>()` |
| Vec of primitives | `Vec::new()` then `v.push(42)` | `Vec::new<i32>()` |

Update the bug-exposing tests:
- `test_vec_method_lookup_is_exact` — already fixed (#1), remove `#[ignore]`,
  add explicit `Vec::new<Point>()`
- `test_vec_push_fn_call_result` — remove `#[ignore]`, add `Vec::new<Point>()`
- `test_lexer_rejects_unknown_chars` — already fixed (#5), remove `#[ignore]`
- `test_int_literal_infers_i64_from_return_type` — change to use explicit type
  annotation or restructure. Since we don't have let-type-annotations, the
  literal `3000000000` would need to be in a context with an expected type
  (struct field, return position, or function arg). Convert to a direct return:
  `fn big() -> i64 { 3000000000 }`

### 7. Fixes already done (keep)

These are already implemented and just need to stay:
- #1: Vec method lookup by exact key (`vec_fns` map) — done
- #5: Lexer panics on unknown chars — done
- #6: Empty registry panics — done

## Verification

```bash
cargo +rustc-fork test -p toylangc --test integration_tests
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve
```

All tests pass, 0 ignored, zero warnings.

## Critical files

| File | Change |
|------|--------|
| `toylangc/src/toylang/ast.rs` | Add `type_args` to FnCall and StaticCall |
| `toylangc/src/toylang/parser.rs` | Parse `name<T>(args)` and `Ty::method<T>(args)` |
| `toylangc/src/toylang/type_resolve.rs` | Remove all inference functions, use explicit type_args |
| `toylangc/src/toylang/callbacks_impl.rs` | Remove Vec inference fallbacks, use explicit type_args |
| `toylangc/src/llvm_gen.rs` | Remove heuristic Vec elem discovery, use explicit type_args |
| `toylangc/tests/integration_tests.rs` | Add explicit type args to all generic/Vec calls |
