# Trait Static Calls: Investigation Report

## Goal

Add explicit trait method calls to toylang: `Clone::clone(&v)` syntax, reusing
the existing `StaticCall` AST node. The user writes `Trait::method(receiver, args)`
and the compiler resolves the trait, finds the concrete impl for the receiver's
type, and emits the call.

## What Was Implemented

### oracle.rs — trait lookup functions

- `find_use_imported_trait_def_id(tcx, name)`: searches `module_children_local`
  for `DefKind::Trait` re-exports from `pub use` in `__lang_stubs`. Parallel to
  the existing `find_rust_type_def_id` for types.

- `find_trait_method(tcx, trait_def_id, self_ty, method)`: uses
  `tcx.for_each_relevant_impl()` to search trait impls matching the receiver's
  concrete type, then finds the method DefId in the impl.

- `rust_trait_method_return_type(tcx, trait_name, method_name, receiver_ty, type_args)`:
  queries the return type of a trait method for a concrete receiver. Looks up
  the trait method DefId in the trait definition (not the impl), builds generic
  args with `Self = receiver_type`, and queries `fn_sig` for the return type.
  Strips `&ref` from the receiver type via `strip_ref()` to get the Self type.

### type_resolve.rs — StaticCall arm

The `StaticCall` arm was restructured to:
1. Resolve args first (before determining return type), so the receiver type
   is available for trait method resolution.
2. Detect trait calls: if `ty` is not a known struct in the registry and the
   call has args, it's likely a trait call.
3. For trait calls, pass `"__trait::TraitName"` as the type_name to the
   `rust_method_ret` callback, with the receiver type prepended to `type_args`.
4. The callback implementations (3 closures in callbacks_impl.rs x2 and
   llvm_gen.rs x1) detect the `__trait::` prefix and call
   `rust_trait_method_return_type` instead of `rust_method_return_type`.

### ast.rs / parser.rs — `&expr` reference expressions

Added `Expr::Ref(Box<Expr>)` to the AST and `TypedExprKind::Ref(Box<TypedExpr>)`
to the typed AST. The parser handles `Token::Ampersand` in `parse_primary` to
produce `Expr::Ref`. Type resolution wraps the inner type in
`ResolvedType::Ref { inner }`. Codegen takes a pointer to the inner value.

This was needed because `Clone::clone(&v)` requires passing a reference to the
receiver.

### callbacks_impl.rs — dependency collection

- `RustMethodDep` gained a `receiver_ty: Option<ResolvedType>` field.
- `walk_typed_expr_for_deps` for `StaticCall` always populates `receiver_ty`
  from the first arg (for both trait and inherent calls; inherent calls fall
  through to the existing lookup path).
- The dep resolution loop checks `receiver_ty.is_some()` and tries trait lookup
  first. For trait deps, it uses the **trait definition's** method DefId (not
  the impl's) with `[Self + explicit type args]` as generic args, so rustc's
  monomorphization collector resolves the concrete impl internally.

### llvm_gen.rs — codegen

- `get_or_resolve_rust_method` gained a `receiver_ty: Option<&ResolvedType>`
  parameter. When present, tries trait lookup first (same approach as deps).
  Uses `Instance::expect_resolve` which correctly maps from trait method +
  `[Self]` to concrete impl method + `[T, A]`.

- `StaticCall` codegen detects trait calls and handles the receiver like
  `MethodCall` codegen: for `Ref` receivers, loads the pointer from the alloca
  to avoid double indirection.

## What Works

- **Trait lookup**: `find_use_imported_trait_def_id` correctly finds traits
  imported via `use std::clone::Clone`.
- **Method resolution**: `find_trait_method` finds the correct impl method.
- **Return type resolution**: `rust_trait_method_return_type` correctly
  determines that `Clone::clone` on `Vec<i32, Global>` returns
  `Vec<i32, Global>`.
- **Instance resolution**: `Instance::expect_resolve` correctly maps
  `(Clone::clone, [Vec<i32, Global>])` to the concrete
  `alloc::vec::{impl#11}::clone` with args `[i32, Global]`.
- **Dependency reporting**: The trait method is reported to rustc's
  monomorphization collector, which compiles the concrete `<Vec as Clone>::clone`.
- **Linking**: The compiled clone function is linked into the binary. No
  undefined symbol errors.
- **All 95 existing tests pass**: No regressions.

## What Fails

The binary crashes at runtime:
```
unsafe precondition(s) violated: slice::from_raw_parts requires
the pointer to be aligned and non-null, and the total size of the
slice not to exceed `isize::MAX`
```

## Root Cause: `#[track_caller]` Hidden Parameters

### Discovery

The LLVM function declaration for `<Vec<i32, Global> as Clone>::clone` has
**3 pointer parameters**, but our codegen only passes **2** (sret + &self).

Debugging output:
```
ABI for clone: ret=Indirect params=[Direct(ptr), Direct(ptr)]
func_params=3 (sret + 2 from coerced_params)
call_args=2  (sret + recv_ptr)
```

### Cause

Many standard library methods, including `Vec::push`, `Vec::clone`, `Vec::reserve`,
and ~43 other Vec methods, are annotated with `#[track_caller]`. When rustc
computes `fn_abi_of_instance`, it appends a hidden
`&'static core::panic::Location<'static>` parameter to `fn_abi.args`. This is
how Rust provides panic location info ("index out of bounds at src/main.rs:42").

The hidden parameter is added in `compiler/rustc_ty_utils/src/abi.rs`,
function `fn_abi_new_uncached`, lines ~642-651:

```rust
args: inputs
    .iter()
    .copied()
    .chain(extra_args.iter().copied())
    .chain(caller_location)   // <-- appended as final arg
    .enumerate()
    .map(|(i, ty)| arg_of(ty, Some(i)))
    .collect()
```

Where `caller_location` is `Some(tcx.caller_location_ty())` when
`instance.def.requires_caller_location(tcx)` returns true.

### Concrete examples

**`Vec::push(&mut self, value: T)`** — has `#[track_caller]`:
| Index | ABI param | Source-level |
|-------|-----------|-------------|
| 0 | `Direct(ptr)` — 8 bytes, Pointer | `&mut self` |
| 1 | `Direct(i32)` — 4 bytes, Int | `value: T` (T=i32) |
| 2 | `Direct(ptr)` — 8 bytes, Pointer | **hidden**: `&Location` from `#[track_caller]` |

**`<Vec as Clone>::clone(&self)`** — has `#[track_caller]`:
| Index | ABI param | Source-level |
|-------|-----------|-------------|
| 0 | `Direct(ptr)` — 8 bytes, Pointer | `&self` |
| 1 | `Direct(ptr)` — 8 bytes, Pointer | **hidden**: `&Location` from `#[track_caller]` |

Plus `ret = Indirect(24 bytes)` → sret pointer prepended → 3 total LLVM params.

**`Vec::new()`** — does NOT have `#[track_caller]`:
- `ret = Indirect(24 bytes)`, `params = []` → 1 LLVM param (sret only)

**`Vec::len(&self)`** — does NOT have `#[track_caller]`:
- `ret = Direct(i64)`, `params = [Direct(ptr)]` → 1 LLVM param

### This is a pre-existing bug

The `#[track_caller]` issue is **not specific to trait calls**. It affects
existing `MethodCall` codegen too. `v.push(42)` declares a 3-param function
but passes only 2 args. It works by luck because the missing `Location` pointer
is only read in panic paths (allocation failure, bounds checks). Normal
operations like push-with-capacity and len don't trigger panics, so the garbage
third argument is never dereferenced.

For `Clone::clone`, the clone implementation calls `<[T]>::to_vec_in` which
internally calls allocation functions that ARE `#[track_caller]` and DO read
the location on failure. When the garbage location pointer gets passed through,
it causes the crash.

## Options for Fixing (Investigated)

### Option A: Filter out `#[track_caller]` params — REJECTED

Detect `requires_caller_location()` on the Instance and reduce the expected
param count by 1. Don't emit the hidden param in the function declaration.

**Rejected because**: LLVM requires `call` arg count to match the function
declaration's param count exactly. Declaring with N-1 params and calling with
N-1 produces valid IR, but the compiled Rust function reads N params from
registers/stack. The callee reads one more arg than the caller provides — this
is **undefined behavior at the ABI level**. It works on aarch64/x86-64 by luck
(the callee reads garbage from a register that happens to exist) but is not
guaranteed. This is what the existing `v.push()` code does accidentally.

### Option B: Pass a valid Location pointer — VIABLE but complex

Create a static `core::panic::Location` value (file="toylang", line=0, col=0)
and pass it as the last argument for `#[track_caller]` functions. Proper panic
messages, no UB.

Complexity: Need to construct a `Location` struct in LLVM IR. It's
`{ &str, u32, u32 }` which is `{ ptr, usize, u32, u32 }` (fat pointer for
&str + line + col). Or use `Location::caller()` which returns a `&Location`.

### Option C: Pass null pointer — RECOMMENDED

Keep all params from `coerced_param_types_for_instance` (including the
track_caller Location pointer). Declare the function with the full param list.
At call sites, detect `instance.def.requires_caller_location(tcx)` and append
a null `ptr` as the last argument.

This produces valid LLVM IR and valid ABI-level calls. The only downside is
garbled panic messages if the function panics, which is acceptable since
toylang has no meaningful source locations to report. If the function panics,
the null Location will cause the panic handler to abort — which is what
happens anyway for unrecoverable errors in a toylang context.

This also fixes the **pre-existing latent bug** with `v.push()` and every
other `#[track_caller]` method in the existing MethodCall codegen.

### Option D: Use ReifyShim to avoid hidden param — REJECTED

Investigated using `InstanceKind::ReifyShim` or `Instance::resolve_for_fn_ptr`
to get an Instance whose ABI lacks the hidden parameter.

**Rejected because**:

1. **Different symbol name**: Both the v0 and legacy symbol mangling schemes
   append shim-specific suffixes for ReifyShim instances. v0 mangling appends
   `::{{shim:reify#0}}`, legacy appends `{{reify-shim}}`. The discriminant of
   `InstanceKind` is also hashed into the symbol. So `tcx.symbol_name(instance)`
   returns a different symbol for ReifyShim vs Item — our call would reference
   a symbol that rustc never compiled, causing a linker error.

2. **`resolve_for_fn_ptr` has the same problem**: It automatically wraps
   `#[track_caller]` functions in ReifyShim, producing a different symbol.

3. **per_instance_mir interaction**: If the collector processes a ReifyShim
   whose underlying def_id is a `__lang_stubs` function, our `per_instance_mir`
   override would intercept it and return a toylang synthetic body instead of
   the shim's wrapper body.

### No `#[no_track_caller]` attribute exists

There is no way to suppress `#[track_caller]` on a function that has it, other
than wrapping it in a shim (which has the symbol name problem above).

## How `#[track_caller]` Works in rustc (for reference)

The hidden parameter is added in `compiler/rustc_ty_utils/src/abi.rs`,
function `fn_abi_new_uncached`:

```rust
let caller_location =
    instance.def.requires_caller_location(tcx).then(|| tcx.caller_location_ty());

// ...later, building the args list:
args: inputs.iter().copied()
    .chain(extra_args.iter().copied())
    .chain(caller_location)   // <-- appended as final arg
    .enumerate()
    .map(|(i, ty)| arg_of(ty, Some(i)))
    .collect()
```

The type is `&'static core::panic::Location<'static>`, which is a single
pointer at the ABI level.

`requires_caller_location` (in `compiler/rustc_middle/src/ty/instance.rs`)
checks:
```rust
pub fn requires_caller_location(&self, tcx: TyCtxt<'_>) -> bool {
    match *self {
        InstanceKind::Item(def_id) | InstanceKind::Virtual(def_id, _) => {
            tcx.body_codegen_attrs(def_id).flags
                .contains(CodegenFnAttrFlags::TRACK_CALLER)
        }
        InstanceKind::ClosureOnceShim { track_caller, .. } => track_caller,
        _ => false,  // ReifyShim, FnPtrShim, etc. return false
    }
}
```

In `compiler/rustc_codegen_ssa/src/mir/block.rs`, rustc's own codegen handles
the Location param by calling `self.get_caller_location(bx, ...)` and
appending the result as the last LLVM argument.

Standard library functions with `#[track_caller]` include ~43 Vec methods
(push, insert, reserve, clone, etc.) but NOT `Vec::new` or `Vec::len`.

## Files Modified

| File | Changes |
|------|---------|
| `toylangc/src/oracle.rs` | Added `find_use_imported_trait_def_id`, `find_trait_method`, `rust_trait_method_return_type`, `strip_ref` |
| `toylangc/src/toylang/ast.rs` | Added `Expr::Ref(Box<Expr>)` |
| `toylangc/src/toylang/typed_ast.rs` | Added `TypedExprKind::Ref(Box<TypedExpr>)` |
| `toylangc/src/toylang/parser.rs` | Added `&expr` parsing in `parse_primary` |
| `toylangc/src/toylang/type_resolve.rs` | Restructured StaticCall arm (args first), added `Ref` resolution + substitution |
| `toylangc/src/toylang/callbacks_impl.rs` | `__trait::` callback handling, `receiver_ty` field on `RustMethodDep`, trait dep resolution, `Ref` in walk |
| `toylangc/src/llvm_gen.rs` | `receiver_ty` param on `get_or_resolve_rust_method`, trait call codegen, `Ref` codegen + walk |
| `toylangc/tests/integration_tests.rs` | Test cases for trait static calls |
| `rustc-lang-facade/src/abi_helpers.rs` | Debug output (temporary) |

## Debug Output (Temporary)

Several `eprintln!` debug statements were added during investigation and should
be removed before merging:
- `llvm_gen.rs`: Instance resolved, trait static call info, MethodCall info
- `abi_helpers.rs`: ret mode, param details per function

## Recommended Fix (Option C)

In `get_or_resolve_rust_method` (llvm_gen.rs), after resolving the Instance
and building the function declaration:

1. Check `instance.def.requires_caller_location(self.tcx)`.
2. If true, record this fact alongside the `RustMethodInfo` (add a
   `has_track_caller: bool` field).
3. At every call site (StaticCall codegen and MethodCall codegen), after
   building the call args, check `has_track_caller`. If true, append a null
   `ptr` as the last argument.

This fixes both the Clone::clone crash AND the pre-existing latent UB in
`v.push()`, `v.reserve()`, and all other `#[track_caller]` methods.

## Next Steps

1. Implement Option C (`has_track_caller` + null pointer append).
2. Remove debug `eprintln!` statements.
3. Add comprehensive tests for trait static calls.
4. Run full test suite to verify no regressions.
