# Plan: Tech Debt Resolution — Session 6

> **Status: COMPLETE.** All 14 tech debt items resolved.
> 55 integration tests + 28 unit tests passing. Zero compiler warnings.

## Context

Session 6 resolved all remaining tech debt items from the fresh codebase scan
conducted at the end of session 5. Work proceeded in order of increasing
difficulty, with each change verified against the full test suite.

## What Was Implemented

### Quick wins (items #13, #12, #3, #11)

- **#13 — `resolved_to_rustc_ty` wrapper**: Deleted 3-line forwarding function,
  replaced 2 call sites with direct `oracle::resolved_to_rustc_ty`.
- **#12 — Duplicate field lookup**: Extracted `find_field_index` helper in
  type_resolve.rs, replacing 2 inline `.fields.iter().position()` patterns.
- **#3 — `mangle_ty_for_symbol` Debug fallback**: Extended match to cover
  Str, Ref, RawPtr, Slice, Tuple. Unknown types now panic instead of producing
  nondeterministic `{:?}` symbols.
- **#11 — `parse_coerced_type`/`parse_struct_type_str` duplication**: Unified
  by making `parse_struct_type_str` delegate to `parse_coerced_type` for each
  field. Added `double` handling. Function went from 35 lines to 7.

### Generalize static calls and explicit literals (#1, #2)

- **#1 — Hardcoded `Vec::new`**: Replaced with call to `rust_method_ret`
  callback (already queries `tcx.fn_sig` via `oracle::rust_method_return_type`).
- **#2 — Method arg inference uses `type_args[0]`**: Root cause eliminated by
  making all literals self-typed:
  - `Expr::IntLit(i64)` → `Expr::IntLit(i64, ResolvedType)` with suffix support
  - `Expr::StructLit` now carries `type_args: Vec<ResolvedType>`
  - Parser supports `42i64`, `Pair<i32, i64> { ... }` syntax
  - `expected_ty` propagation eliminated for literals
  - MethodCall args use `Void` instead of `type_args[0]`

**Surprise**: Generic function bodies containing `Wrapper<T> { inner: x }` had
unsubstituted `TypeParam("T")` reaching codegen. `resolve_function_for_instance`
cloned the body without substituting type params in expressions. Fix: added
`substitute_type_params_in_body` that walks the AST and substitutes type params
in StructLit/FnCall/StaticCall type_args.

### Parser cleanup (#14)

- **#14 — `struct_names.clone()` in parser**: Removed `struct_names` from Parser
  struct. Now accumulated in `parse_program` and threaded as `&[String]` through
  all expression-parsing functions. Also threaded `type_params: &[String]` so
  generic function bodies correctly parse `T` as `TypeParam`. Eliminated all 4
  `.clone()` calls.

### `after_rust_analysis` validation (#9)

Implemented 5 validation checks that run after rustc type-checks:
1. Every toylang struct is visible to rustc
2. Every toylang function with a body has a stub
3. Rust types in struct fields exist
4. Extern functions exist in Rust
5. Non-generic function bodies type-resolve successfully

Errors collected into `Vec<String>`, reported all at once before aborting.

### Result-based error handling (#6, #7, #8)

**TypeResolveError enum** (8 variants): UndefinedVariable, UndefinedStruct,
UndefinedFunction, FieldNotFound, FieldAccessOnNonStruct,
MethodCallOnUnsupportedType, WrongTypeArgCount, NonStructLitType.

**ParseError enum** (7 variants): UnknownIntSuffix, UnexpectedCharacter,
UnexpectedToken, UnexpectedTopLevelToken, ExpectedExpression, ExpectedType,
ExpectedPointerQualifier.

Converted 7 function signatures in type_resolve.rs from `-> T` to
`-> Result<T, TypeResolveError>`. Converted all ~20 parser functions from
`Result<_, String>` to `Result<_, ParseError>`. `after_rust_analysis` uses
direct `match` instead of `catch_unwind`.

**28 unit tests** (21 type_resolve + 7 parser) including 18 error-case tests
that destructure error variants and check fields — no string matching.

### ABI edge cases (#4, #5)

- **#4 — `PassMode::Indirect { on_stack: true }`**: Removed the assert. Both
  `on_stack: true` (byval, 32-bit x86) and `on_stack: false` now emit
  `CoercedParam::Indirect`.
- **#5 — `PassMode::Pair` as single scalar**: Fixed in both return and param
  functions. Now extracts two scalars from `BackendRepr::ScalarPair` and emits
  `"{ scalar1, scalar2 }"` LLVM struct type. Added `primitive_to_llvm_str` helper.
  Existing `parse_coerced_type` in llvm_gen.rs already handles struct types.

## Functions added

| Function | File |
|----------|------|
| `find_field_index` | type_resolve.rs |
| `substitute_type_params_in_body` | type_resolve.rs |
| `parse_struct_lit_fields` | parser.rs |
| `collect_rust_type_names` | callbacks_impl.rs |
| `primitive_to_llvm_str` | abi_helpers.rs |

## Verification

```bash
cargo +rustc-fork check -p toylangc                              # 0 warnings
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve # 21 passed
cargo +rustc-fork test -p toylangc --bin toylangc -- parser       # 7 passed
cargo +rustc-fork test -p toylangc --test integration_tests       # 55 passed
```
