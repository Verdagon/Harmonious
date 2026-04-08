# Plan: Unify Type Representation — One Structured Type Enum Everywhere

> **Status: COMPLETE.** All 55 integration tests + 10 unit tests passing.
> Zero compiler warnings.

## Context

Types were represented three different ways: `ToyFieldType` (struct fields),
`String` (params, returns, call type args), and `ResolvedType` (typed AST).
This refactor unified everything onto `ResolvedType`, with the parser producing
structured types directly instead of strings.

## What Was Implemented

### Phase 1: Type unification

- **`TypeParam(String)` added** to `ResolvedType` for unresolved type params
- **`ToyFieldType` deleted** — struct fields use `ResolvedType`
- **`ToyParam.ty`** changed from `String` to `ResolvedType`
- **`ToyFunction.return_ty`** changed from `Option<String>` to `Option<ResolvedType>`
- **`FnCall/StaticCall type_args`** changed from `Vec<String>` to `Vec<ResolvedType>`
- **Parser rewritten**: `parse_type()` returns `ResolvedType` directly
- **`resolved_type_to_syn`** replaces `field_type_to_syn` + `syn::parse_str`
- **`substitute_type_params`** walks `ResolvedType` tree (replaces string substitution)

### Phase 2: StructRef/Struct split

**Surprise:** Parser produced `Struct { field_types: [] }` — ambiguous between
"zero fields" and "not yet resolved." Caused 7 test failures (silent zero-field
structs in codegen).

**Fix:** Split into two variants:
- `StructRef { name, type_args }` — parser/registry, field layout unknown
- `Struct { name, type_args, field_types }` — type resolver/codegen, fully resolved

`resolve_struct_fields` converts `StructRef` → `Struct` by looking up fields.
`resolved_to_inkwell` panics on `StructRef` — makes the bug impossible.

### Phase 3: monomorphize_type round-trip fix

**Surprise:** `monomorphize_type` round-tripped through `ResolvedType`
(rustc Ty → ResolvedType → substitute → rustc Ty). Failed for types like
`Global` that aren't locally imported.

**Fix:** `resolved_to_rustc_ty_with_subst` keeps substitution at the rustc
`Ty` level — `HashMap<&str, Ty<'tcx>>` maps type params directly to rustc
types, avoiding the lossy round-trip.

## Functions Deleted (~21)

| Function | File |
|----------|------|
| `ToyFieldType` enum | registry.rs |
| `parse_type_str()` | parser.rs |
| `parse_field_type()` | parser.rs |
| `field_type_to_syn()` | stub_gen.rs |
| `parse_type_string()` | type_resolve.rs |
| `split_type_args()` | type_resolve.rs |
| `resolve_field_type()` | type_resolve.rs |
| `substitute_type_params()` (string ver) | type_resolve.rs |
| `resolve_type()` | llvm_gen.rs |
| `resolve_field_with_args()` | llvm_gen.rs |
| `type_from_string()` | llvm_gen.rs |
| `resolve_field_type_with_subst()` | llvm_gen.rs |
| `struct_type_for_ret()` | llvm_gen.rs |
| `parse_generic_type()` | llvm_gen.rs |
| `substitute_type_params_str[_pub]()` | llvm_gen.rs |
| `string_to_rustc_ty()` | callbacks_impl.rs |
| `rustc_ty_to_type_string()` | callbacks_impl.rs |
| `resolve_field_ty()` | callbacks_impl.rs |
| `resolve_rust_generic_ty()` | callbacks_impl.rs |
| `find_local_struct_ty()` | oracle.rs |
| `dummy_opaque_vec()` | typed_ast.rs |

## Functions Added (~7)

| Function | File |
|----------|------|
| `parse_type()` → `ResolvedType` | parser.rs |
| `resolved_type_to_syn()` | stub_gen.rs |
| `substitute_type_params()` (ResolvedType ver) | type_resolve.rs |
| `resolve_struct_fields()` (StructRef → Struct) | type_resolve.rs |
| `resolved_type_to_mangled_name()` | llvm_gen.rs |
| `resolved_to_rustc_ty_with_subst()` | callbacks_impl.rs |

## Verification

```bash
cargo +rustc-fork check -p toylangc                              # 0 warnings
cargo +rustc-fork test -p toylangc --test integration_tests      # 55 passed
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve # 10 passed
```
