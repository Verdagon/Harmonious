# Track Caller Hidden ABI Parameter (TCHAPZ)

Many Rust standard library functions (including ~43 Vec methods like `push`,
`clone`, `reserve`, `insert`) are annotated with `#[track_caller]`. Rustc's ABI
computation appends a hidden `&'static core::panic::Location<'static>` pointer
parameter to these functions' signatures. This parameter is invisible in source
code but present in the compiled function's actual calling convention.

## Where

- `rustc-lang-facade/src/abi_helpers.rs` — `coerced_param_types_for_instance`
  reports the hidden param as an extra `Direct(ptr)` entry
- `toylangc/src/llvm_gen.rs` — `RustMethodInfo.has_track_caller` field, null
  pointer append at all 4 call sites (MethodCall sret/non-sret, trait
  StaticCall sret/non-sret)
- `toylangc/src/llvm_gen.rs` — `get_or_resolve_rust_method` detects it via
  `instance.def.requires_caller_location(tcx)`

## Cross-cutting effect

When toylang calls any Rust method, the LLVM function declaration may have one
more parameter than the source-level signature suggests. If a call site doesn't
pass a value for this hidden parameter, the compiled function reads garbage from
a register/stack slot. This is undefined behavior. For most methods (like `push`
with sufficient capacity), the garbage is never read because the Location is
only used in panic paths. But methods that internally call other
`#[track_caller]` functions (like `clone` calling allocation functions) will
pass the garbage pointer through, causing crashes.

Any new code path that calls Rust methods must account for this: check
`has_track_caller` on the resolved `RustMethodInfo` and append a null `ptr` as
the last argument.

## Why it exists

`#[track_caller]` provides panic location info in error messages like "index
out of bounds at src/main.rs:42". The hidden parameter carries a runtime
`&Location` that changes at every call site, so it cannot be constant-folded or
lowered away. Rustc inserts it in `fn_abi_new_uncached`
(`compiler/rustc_ty_utils/src/abi.rs`). There is no `#[no_track_caller]`
attribute and no way to suppress it. `ReifyShim` wraps the function to hide the
parameter but produces a different symbol name, making it unusable for direct
calls.

We pass null because toylang has no meaningful source locations to report. If a
`#[track_caller]` function panics, the panic handler will see a null Location
and abort, which is acceptable for toylang's error handling model.
