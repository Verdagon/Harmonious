# ABI Coerced Return Type In Function Declarations (ACRTFDZ)

When declaring an external Rust function in LLVM IR, the return type must use
rustc's ABI-coerced type (e.g., `i64` for an 8-byte struct), not the toylang
representation (e.g., `[8 x i8]`). After the call, if the ABI type differs from
the toylang type, the value must be stored through a pointer and loaded as the
toylang type — a type-punning bitcast via memory.

## Where

- `toylangc/src/llvm_gen.rs` — FnCall use-import/extern path: return type
  built from `coerced_return_type_for_instance`, not `resolved_to_inkwell`
- `toylangc/src/llvm_gen.rs` — `get_or_resolve_rust_method`: same pattern
  for MethodCall/StaticCall return type declarations
- `rustc-lang-facade/src/abi_helpers.rs` — `coerced_return_type_for_instance`
  returns `Direct("i64")`, `Indirect`, or `Void`

## Cross-cutting effect

LLVM treats aggregate returns (like `[8 x i8]`) differently from scalar returns
(like `i64`). On aarch64, a scalar `i64` is returned in register `x0`, but an
aggregate `[8 x i8]` may be returned via a hidden sret pointer or loaded from
memory. If the LLVM function declaration says `[8 x i8]` but the actual compiled
function returns `i64` in `x0`, LLVM reads the return value from the wrong
location — producing garbage. This garbage silently propagates until something
dereferences it (e.g., passing a corrupt `Stdout` handle to `write_all`),
causing a segfault.

Any code path that declares an LLVM function for a Rust callee must use
`parse_coerced_type` on the ABI-coerced return string, not `resolved_to_inkwell`
on the toylang type. The store-through-pointer reinterpretation pattern handles
the type mismatch between what the call returns (ABI type) and what toylang
expects (toylang type).

## Why it exists

Rustc's ABI computation (`fn_abi_of_instance`) may coerce return types to
scalars for efficiency. An 8-byte struct like `Stdout` becomes `i64` — returned
in a register instead of memory. This is target-specific (aarch64 vs x86_64
have different thresholds). Toylang represents all opaque Rust types as
`[N x i8]` byte arrays, which are LLVM aggregates. The mismatch between rustc's
scalar coercion and toylang's aggregate representation is fundamental — the
store-through-pointer pattern bridges the gap.

For primitive types (bool, i32), the ABI type may also differ (bool is `i8` in
ABI but `i1` in toylang). The same reinterpretation pattern handles this, though
for primitives the mismatch is less dangerous (both are scalars, just different
widths).
