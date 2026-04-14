# Phase 2: Rust Free Function Calls via `use` Imports — DONE

## Context

Toylang could already call Rust methods (`v.len()`, `Clone::clone(&v)`) and body-less extern
functions. Phase 2 added support for calling Rust free functions imported via `use` (e.g.,
`use std::io::stdout` → `stdout()`), and completed argument type checking uniformly across
all call types.

**Final test counts**: 102 integration tests + 47 unit tests, all passing.

## Principles applied

**Non-generic is the degenerate case of generic.** All oracle functions use
`.instantiate(tcx, args)` with actual type args — never `instantiate_identity()`. Empty
args slice for non-generic functions goes through the same code path.

**Self is just another parameter.** The oracle returns ALL params from `sig.inputs()`
including self. For MethodCall (where syntax separates receiver from args), the type
resolver skips `param_types[0]` (self) because toylang doesn't model autoref — the
receiver type won't match `sig.inputs()[0]` directly. For StaticCall, args already align
with `sig.inputs()` naturally. The checking loop is the same in all cases.

**`rust_param_types` as existence sentinel.** Free functions are detected via
`rust_param_types("", name, type_args)` returning `Some` vs `None`. This eliminates the
Void-as-sentinel ambiguity — void-returning free functions return `Some(vec![...])`, not
`None`.

## Discoveries and fixes

1. **`StructRef` vs `Struct` mismatch**: `rustc_ty_to_resolved_type` produces `StructRef`
   for toylang types, but the type resolver resolves expressions to `Struct` (with field
   types filled in). These are semantically equal but structurally different. Fixed by adding
   `types_match()` that handles this equivalence, used in all arg type checking.

2. **Self-param offset in MethodCall**: `sig.inputs()` includes self as `inputs()[0]`, but
   MethodCall args don't include the receiver. Initially tried prepending the receiver for
   uniform checking, but hit the autoref wall — receiver type is `Vec<I32>` while
   `sig.inputs()[0]` is `&mut Vec<I32>`. Fixed by checking explicit args against
   `param_types[i + 1]`, skipping self.

3. **`instantiate_identity()` misuse in quest.md**: The original plan proposed
   `tcx.fn_sig(def_id).instantiate_identity()` for `rust_free_fn_return_type`. This is a
   no-op unwrap of `EarlyBinder` that leaves `ty::Param` placeholders intact, causing
   `rustc_ty_to_resolved_type` to panic on generic functions. Fixed by always using
   `.instantiate(tcx, args)`. Added comments to all 5 existing `instantiate_identity()`
   call sites explaining why they're structural inspection only.

## Files modified

- `toylangc/src/oracle.rs` — 6 new functions: `find_use_imported_fn_def_id`,
  `instantiate_free_fn_sig`, `rust_free_fn_return_type`, `rust_free_fn_param_types`,
  `rust_method_param_types`, `rust_trait_method_param_types`
- `toylangc/src/toylang/type_resolve.rs` — `ArgTypeMismatch` error variant, `types_match()`
  function, `rust_param_types` callback threaded through `resolve_fn_body`/`resolve_expr`/
  `resolve_stmt`, FnCall free function branch, MethodCall + StaticCall arg checking
- `toylangc/src/toylang/callbacks_impl.rs` — `rust_method_ret` extended for free functions,
  `rust_param_types` closures added at 2 sites, dep collection for use-imported free fns
- `toylangc/src/llvm_gen.rs` — `rust_method_ret` extended, `rust_param_types` closure,
  FnCall dispatch restructured for `is_extern_decl || is_use_import` with real type args
- `toylangc/tests/integration_tests.rs` — 2 new tests: `test_extern_fn_decl_still_works`,
  `test_rust_free_fn_undefined_gives_error`
- `rustc-lang-facade/src/queries/symbol_name.rs` — `instantiate_identity()` comment
- `rustc-lang-facade/src/mir_helpers.rs` — `instantiate_identity()` comment
- `rustc-lang-facade/src/queries/per_instance.rs` — `instantiate_identity()` comments (×2)
- `toylangc/src/llvm_gen.rs` — `instantiate_identity()` comment
- `CLAUDE.md` — compiler law (non-generic = degenerate generic), `instantiate_identity()`
  comment policy
- `quest.md` — Phase 2 section rewritten with final design

## New unit tests (in type_resolve.rs)

| Test | What it verifies |
|------|-----------------|
| `test_arg_type_mismatch_i32_vs_i64` | i64 passed where i32 expected → ArgTypeMismatch |
| `test_arg_type_mismatch_bool_vs_i32` | bool passed where i32 expected → ArgTypeMismatch |
| `test_arg_type_mismatch_generic_fn` | wrong type to generic fn after substitution |
| `test_arg_type_correct_passes` | correct types produce no error |
| `test_arg_type_extra_args_no_crash` | extra args beyond params resolve with Void expected |
| `test_free_fn_not_found_gives_undefined_error` | None from rust_param_types → UndefinedFunction |
| `test_free_fn_void_returning_resolves_correctly` | void-returning free fn not confused with "not found" |
| `test_free_fn_correct_args_pass` | correct args to free fn pass |
| `test_free_fn_with_args_type_checked` | wrong arg type to free fn → ArgTypeMismatch |
| `test_free_fn_return_type_propagates` | free fn return type used in let binding |

## New integration tests

| Test | What it verifies |
|------|-----------------|
| `test_extern_fn_decl_still_works` | body-less extern fn declarations still compile and link |
| `test_rust_free_fn_undefined_gives_error` | calling undefined function fails compilation |
