# Session Report: Explicit Trait Method Calls + track_caller Fix

## What We Set Out To Do

Add explicit trait method calls to toylang using `Trait::method(receiver, args)`
syntax, reusing the existing `StaticCall` AST node. This is Phase 1 of the
larger quest to let toylang call into arbitrary Rust crates.

## What We Implemented

### Trait method resolution (oracle.rs)

- `find_use_imported_trait_def_id(tcx, name)`: finds a trait's DefId from
  `pub use` re-exports in `__lang_stubs`. Searches `module_children_local`
  for `DefKind::Trait`, parallel to `find_rust_type_def_id` for types.

- `find_trait_method(tcx, trait_def_id, self_ty, method)`: uses
  `tcx.for_each_relevant_impl()` to find a method in the concrete impl
  for a given receiver type.

- `rust_trait_method_return_type(tcx, trait_name, method_name, receiver_ty, type_args)`:
  queries the return type via the trait definition's method DefId (not the
  impl's) with `Self` substituted. Uses `strip_ref()` to unwrap `&T` → `T`
  for the Self type.

- `strip_ref(ty)`: recursively strips `Ref` wrappers from a ResolvedType.

### `__trait::` callback convention (type_resolve.rs, callbacks_impl.rs, llvm_gen.rs)

The `StaticCall` arm in type_resolve.rs was restructured to resolve args first
(so the receiver type is available), then detect trait calls by checking if `ty`
is a known struct. For trait calls, it passes `"__trait::TraitName"` as the
type name to the `rust_method_ret` callback, with the receiver type prepended
to the type_args array. All 3 callback closures (callbacks_impl.rs x2,
llvm_gen.rs x1) detect the `__trait::` prefix and dispatch to
`rust_trait_method_return_type`.

### `&expr` reference expressions (parser, ast, type_resolve, llvm_gen)

Added `Expr::Ref(Box<Expr>)` to the AST and `TypedExprKind::Ref(Box<TypedExpr>)`
to the typed AST. Parser handles `Token::Ampersand` in `parse_primary`.
Type resolution wraps the inner type in `ResolvedType::Ref { inner }`.
Codegen takes a pointer to the inner value via `into_ptr`.

### Trait call codegen (llvm_gen.rs)

`get_or_resolve_rust_method` gained a `receiver_ty: Option<&ResolvedType>`
parameter. When present and the type name resolves as a trait, it uses
the trait definition's method DefId + `[Self + explicit type args]` with
`Instance::expect_resolve` to get the concrete impl Instance. This correctly
maps e.g. `(Clone::clone, [Vec<i32, Global>])` → `alloc::vec::{impl#11}::clone`
with args `[i32, Global]`.

StaticCall codegen detects trait calls and handles the receiver like MethodCall:
for `Ref` receivers, loads the pointer from the alloca to avoid double
indirection.

### Dependency collection (callbacks_impl.rs)

`RustMethodDep` gained a `receiver_ty: Option<ResolvedType>` field. The dep
resolution loop checks for trait calls and uses the trait definition's method
DefId (not the impl's) with `[Self + type args]` for reporting to rustc's
monomorphization collector.

### `#[track_caller]` fix (llvm_gen.rs)

`RustMethodInfo` gained a `has_track_caller: bool` field, set from
`instance.def.requires_caller_location(tcx)`. At every call site (MethodCall
sret, MethodCall non-sret, trait StaticCall sret, trait StaticCall non-sret),
if `has_track_caller` is true, a null `ptr` is appended as the last argument.

## Surprises and Discoveries

### Surprise 1: `#[track_caller]` hidden parameters

This was the biggest surprise. ~43 Vec methods (push, clone, reserve, insert,
etc.) are annotated with `#[track_caller]` in the standard library. Rustc's
ABI computation (`fn_abi_new_uncached` in `compiler/rustc_ty_utils/src/abi.rs`)
appends a hidden `&'static core::panic::Location<'static>` parameter to the
function's ABI. This parameter is invisible in the source-level signature.

Our `coerced_param_types_for_instance` had been faithfully reporting this extra
param all along, and our function declarations included it, but our call sites
never passed it. For `v.push(42)`, LLVM silently passed garbage for the missing
arg — which worked because the Location pointer is only read on panic.
`Clone::clone` exposed this because clone internally calls allocation code
that reads the Location pointer.

This was a **pre-existing latent bug** in the MethodCall codegen, not specific
to trait calls.

### Surprise 2: Generic args for trait methods vs inherent methods

When resolving a trait method Instance, the generic args are structured
differently than for inherent methods. For inherent methods on `Vec<i32, Global>`,
the args are `[i32, Global]` (the ADT's type params). For trait methods, the
initial instinct was to pass `[Self = Vec<i32, Global>]` as a single arg, but
this caused "type parameter out of range" errors because the trait method's
`fn_sig` references `Self` as param index 0, and `Vec<i32, Global>` itself
contains type params that need separate slots.

The fix: use the **trait definition's** method DefId (not the impl's) with
`[Self]` as the first generic arg. `Instance::expect_resolve` then handles the
mapping from trait-level args to impl-level args automatically.

### Surprise 3: ReifyShim doesn't work for our use case

We investigated using `InstanceKind::ReifyShim` to get an Instance without the
`#[track_caller]` hidden parameter. This doesn't work because ReifyShim
produces a **different symbol name** (both v0 and legacy mangling append
shim-specific suffixes). Our call would reference a symbol that rustc never
compiled, causing linker errors.

### Surprise 4: LLVM tolerates arg count mismatches on external declarations

LLVM doesn't validate that `call` arg counts match declaration param counts
for external function declarations. This is why `v.push(42)` worked despite
passing 2 args to a 3-param function. However, this is undefined behavior at
the ABI level — the callee reads garbage from a register that the caller
didn't set.

### Surprise 5: `&expr` was needed but didn't exist

The parser had `&T` for type syntax but no `&expr` for expression syntax.
Adding `Expr::Ref` was straightforward (parser, AST, type resolver, codegen)
but was an unexpected prerequisite for the `Clone::clone(&v)` syntax.

## Test Results

- **100 integration tests** pass (95 existing + 5 new)
- **37 unit tests** pass
- Zero failures, zero regressions

### New tests added

| Test | What it verifies |
|------|-----------------|
| `test_trait_static_call_inherent_still_works` | Vec::new inherent StaticCall still works |
| `test_trait_static_call_clone_vec` | Clone::clone(&v) compiles and runs (sret + track_caller) |
| `test_trait_static_call_clone_vec_use_result` | Cloned Vec has correct length (3 elements) |
| `test_trait_static_call_result_discarded` | Clone result as ExprStmt (discarded) |
| `test_ref_expr_basic` | &var as argument to trait method, clone result used |

## Files Modified

| File | Changes |
|------|---------|
| `toylangc/src/oracle.rs` | `find_use_imported_trait_def_id`, `find_trait_method`, `rust_trait_method_return_type`, `strip_ref` |
| `toylangc/src/toylang/ast.rs` | `Expr::Ref(Box<Expr>)` |
| `toylangc/src/toylang/typed_ast.rs` | `TypedExprKind::Ref(Box<TypedExpr>)` |
| `toylangc/src/toylang/parser.rs` | `&expr` parsing in `parse_primary` |
| `toylangc/src/toylang/type_resolve.rs` | StaticCall args-first restructure, `__trait::` convention, `Ref` resolution + substitution |
| `toylangc/src/toylang/callbacks_impl.rs` | `__trait::` callback handling, `receiver_ty` on `RustMethodDep`, trait dep resolution, `Ref` in walk |
| `toylangc/src/llvm_gen.rs` | `has_track_caller` on `RustMethodInfo`, `receiver_ty` on `get_or_resolve_rust_method`, trait call codegen, `Ref` codegen + walk, null ptr for track_caller at all 4 call sites |
| `toylangc/tests/integration_tests.rs` | 5 new tests |
| `rustc-lang-facade/src/abi_helpers.rs` | Debug output added then removed |
| `docs/trait-call-investigation.md` | Detailed investigation writeup |
| `docs/rust-interop-architecture-guide.md` | Section 10.4 updated for explicit trait calls, duck typing noted as future |
| `quest.md` | Phase 1 updated, all examples switched to explicit syntax |
