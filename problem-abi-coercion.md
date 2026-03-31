# Problem Report: ABI Coercion Mismatch Between rustc and External LLVM Code Generator

## Summary

When a Toylang function compiled by an external LLVM backend returns a struct, rustc
and the external code disagree on the LLVM-level return type. Rustc applies ABI coercion
(e.g., `{ i32, i32 }` → `i64` on aarch64), but the external LLVM IR uses the natural
struct type. This causes the second field of multi-field struct returns to be zeroed or
corrupted at runtime.

## Context

### What we're building

A rustc driver (`toylangc`) that compiles a toy language ("Toylang") alongside Rust.
Toylang types and functions are injected into rustc's compilation pipeline via query
provider overrides (`layout_of`, `mir_built`, `mir_borrowck`, `mir_shims`). The driver
is built on `nightly-2025-01-15` (LLVM 19.1.6, aarch64-apple-darwin).

### The architecture

Toylang function bodies are compiled by an external LLVM backend (not by rustc's MIR
lowering). The `mir_built` override returns a thin call stub that delegates to the
externally-compiled function. The external `.o` file is injected into `CodegenResults`
via a `CodegenBackend` wrapper.

For functions that call Rust generics (e.g., `Vec::push`), the MIR stub also contains
phantom `ReifyFnPointer` casts that trigger monomorphization. Symbol visibility for the
Rust generics is achieved via `-C codegen-units=16` (forces the partitioner to assign
external linkage to cross-CGU symbols).

### What works

- `make_counter() -> Counter` where `Counter { value: i32 }` — single i32 field.
  Rustc coerces this to `i32` return. Our LLVM IR returns `{ i32 }`. LLVM's backend
  handles this coercion transparently (`{ i32 }` ≡ `i32`). ✅

- `wrap_value(x: i32) -> Counter` — parameter + single-field struct return. ✅

- `make_vec() -> Vec<Point>` — Vec is 24 bytes (3 × i64), returned via sret pointer.
  Both sides agree on sret. ✅

- `vec_len(v: &Vec<Point>) -> usize` — pointer in, scalar out. ✅

### What fails

- `make_pair() -> Pair<i32, i32>` where `Pair { first: i32, second: i32 }` — two
  i32 fields, 8 bytes total. Returns `first: 10, second: 0` instead of `10, 20`. ❌

## The problem in detail

### What rustc generates (call site)

When rustc compiles the MIR stub for `make_pair`, it generates this LLVM IR:

```llvm
; The extern function declaration — note the return type is i64, not { i32, i32 }
declare i64 @__toylang_impl_make_pair()

define internal i64 @_ZN9pair_test9make_pair...E() {
start:
  %0 = alloca [8 x i8], align 8
  %_0 = alloca [8 x i8], align 4
  %1 = call i64 @__toylang_impl_make_pair()     ; ← expects i64 return
  store i64 %1, ptr %0, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %_0, ptr align 8 %0, i64 8, i1 false)
  %2 = load i64, ptr %_0, align 4
  ret i64 %2
}
```

Rustc has coerced `Pair<i32, i32>` (8 bytes) to `i64` for the return value. The call
instruction expects `i64` in register `x0`.

### What our LLVM backend generates (callee)

```llvm
define { i32, i32 } @__toylang_impl_make_pair() {
  ret { i32, i32 } { i32 10, i32 20 }
}
```

Our function returns `{ i32, i32 }`. On aarch64, LLVM's code generator places the
first i32 (10) in the lower 32 bits of `x0` and the second i32 (20) in `x1` (or in
the upper 32 bits — the exact behavior depends on LLVM's struct return lowering for
the default calling convention).

### The mismatch

The caller reads ONE register (`x0`) as `i64`. The callee writes TWO values that may
span two registers or be packed differently. The result: `first` reads correctly (from
the lower 32 bits of `x0`), but `second` reads as 0 (the upper 32 bits of `x0` are
zero because the callee put the second value elsewhere).

### This is NOT specific to `extern "C"`

We initially thought the issue was the C ABI. We changed the stub declaration to
`extern "Rust"`:

```rust
extern "Rust" {
    fn __toylang_impl_make_pair() -> Pair<i32, i32>;
}
```

The result is identical: rustc still declares `i64` as the LLVM return type. Both the
C and Rust ABIs on aarch64 coerce small structs to scalars. The coercion is applied by
**rustc's codegen** (in `rustc_codegen_llvm`), not by LLVM's backend.

### Confirmation via Clang

Clang exhibits the same behavior for C structs:

```c
struct Pair { int first; int second; };
struct Pair make_pair(void) { ... }
```

Clang generates:
```llvm
define i64 @make_pair() {
  %retval = alloca %struct.Pair, align 4
  ...
  %0 = load i64, ptr %retval, align 4
  ret i64 %0
}
```

Clang also coerces `{ int, int }` → `i64` in the frontend. This confirms: any external
code generator must match the ABI coercion rules, or use an alternative mechanism.

### The coercion rules (aarch64-apple-darwin)

From the AAPCS64 (ARM Architecture Procedure Call Standard) and Apple's variant:

| Struct size | Return mechanism | LLVM return type |
|-------------|------------------|------------------|
| ≤ 4 bytes   | Register (w0)    | i32              |
| 5-8 bytes   | Register (x0)    | i64              |
| 9-16 bytes  | Registers (x0+x1)| [2 x i64]        |
| > 16 bytes  | Indirect (sret)  | void + sret ptr  |

For `{ i32, i32 }` (8 bytes): the coerced LLVM return type is `i64`. The struct is
loaded as a single i64 from memory (packing both fields) and returned in register `x0`.

These rules are **platform-specific**. x86_64 has different rules (some structs are
returned in `rax`+`rdx`, some via sret, depending on field types and counts).

## Approaches considered

### 1. Match the ABI coercion in our LLVM backend

Generate `define i64 @__toylang_impl_make_pair()` and pack the fields:

```llvm
define i64 @__toylang_impl_make_pair() {
  %tmp = alloca { i32, i32 }, align 4
  ; store fields
  %f0 = getelementptr inbounds { i32, i32 }, ptr %tmp, i32 0, i32 0
  store i32 10, ptr %f0
  %f1 = getelementptr inbounds { i32, i32 }, ptr %tmp, i32 0, i32 1
  store i32 20, ptr %f1
  ; load as i64 and return
  %result = load i64, ptr %tmp
  ret i64 %result
}
```

**Pros:** Correct. Matches what Clang does.

**Cons:** Requires implementing platform-specific ABI coercion rules in our LLVM
backend. Different rules for aarch64, x86_64, etc. The rules are complex (depend on
field types, alignment, number of fields, whether fields are floating-point, etc.).
This is essentially reimplementing Clang's `TargetCodeGenInfo`.

### 2. Use sret (out-pointer) for all struct returns

Change the extern declaration to take a pointer parameter:

```rust
extern "C" {
    fn __toylang_impl_make_pair(out: *mut Pair<i32, i32>);
}
```

The MIR stub passes a pointer to the return place. Our LLVM IR writes through it:

```llvm
define void @__toylang_impl_make_pair(ptr %out) {
  %f0 = getelementptr inbounds { i32, i32 }, ptr %out, i32 0, i32 0
  store i32 10, ptr %f0
  %f1 = getelementptr inbounds { i32, i32 }, ptr %out, i32 0, i32 1
  store i32 20, ptr %f1
  ret void
}
```

**Pros:** No ABI coercion. Portable across all platforms. Simple LLVM IR.

**Cons:** Extra indirection for small structs (the caller allocates stack space and
passes a pointer, rather than using registers). The MIR stub becomes more complex
(must allocate a temporary, pass its address, then copy to the return place). For
large structs (> 16 bytes), sret is already used, so this only adds overhead for
small structs.

### 3. Inject into the same LLVM module (avoid the ABI boundary)

If our LLVM IR is in the same LLVM module as rustc's code, there's no ABI boundary.
LLVM sees both the definition and the call, and handles any type mismatches during
intra-module optimization. The function wouldn't need a stable ABI at all.

This requires injecting our LLVM bitcode into a rustc codegen unit BEFORE the LTO/
optimization pass. The injection point is `compile_codegen_unit` in the
`ExtraBackendMethods` trait.

**Pros:** No ABI issues at all. Maximum optimization (LLVM can inline across the
boundary). The "correct" long-term solution.

**Cons:** `ExtraBackendMethods` requires implementing `WriteBackendMethods`, which
has ~15 methods and 6 associated types. The associated types (`ModuleLlvm`,
`OwnedTargetMachine`, `ModuleBuffer`, etc.) are `pub(crate)` in `rustc_codegen_llvm`
and cannot be named from an external driver. This effectively blocks implementation
without either:
- An upstream rustc change to make these types public
- Unsafe type-erasure or transmute hacks
- Duplicating the type definitions (extremely fragile across nightlies)

### 4. Query rustc for the exact LLVM signature

In `after_analysis`, use rustc's internals to determine what LLVM types it will use
for the function signature. Generate our LLVM IR to match exactly.

**Pros:** Always correct, no manual ABI rules.

**Cons:** Rustc's ABI lowering is deep in `rustc_codegen_llvm` (specifically
`abi.rs` and the `FnAbi` type). Accessing it from `after_analysis` may not be
straightforward — the `FnAbi` is computed during codegen, not during analysis.
May require calling `fn_abi_of_instance` which is a codegen-time query.

### 5. Use `#[repr(C)]` on the Toylang struct stubs

If the struct is `#[repr(C)]`, rustc's ABI lowering follows C rules exactly.
Our LLVM backend can also follow C rules (using the alloca+load pattern from
Approach 1). Both sides agree because they both implement the C ABI.

**Pros:** Well-defined, documented ABI. Clang can serve as a reference implementation.

**Cons:** The user explicitly wants to avoid `#[repr(C)]` on Toylang structs.
Also still requires implementing C ABI coercion in the LLVM backend (same as
Approach 1, just with better-documented rules).

## Resolution: Approach 4 (query `fn_abi_of_instance`)

**Implemented and working.** The `after_analysis` callback calls
`tcx.fn_abi_of_instance(...)` for each externally-compiled function, extracts the
`PassMode` from `fn_abi.ret.mode`, and maps `CastTarget` → LLVM type string (~40
lines in `src/abi_helpers.rs`). The LLVM IR generator then uses the coerced type
in the `define` line and builds the struct in memory via alloca+GEP+store, then
loads as the coerced scalar and returns it.

For `Pair<i32, i32>` on aarch64: `PassMode::Cast { rest: Uniform { unit: Reg::i64() } }`
→ coerced type is `i64` → LLVM IR:
```llvm
define i64 @__toylang_impl_make_pair() {
  %retval = alloca { i32, i32 }, align 4
  %retval_f0 = getelementptr inbounds { i32, i32 }, ptr %retval, i32 0, i32 0
  store i32 10, ptr %retval_f0
  %retval_f1 = getelementptr inbounds { i32, i32 }, ptr %retval, i32 0, i32 1
  store i32 20, ptr %retval_f1
  %result = load i64, ptr %retval, align 4
  ret i64 %result
}
```

This matches exactly what Clang generates for C struct returns.

**Key files:**
- `src/abi_helpers.rs` — `coerced_return_type()`, `cast_target_to_llvm_str()`, `reg_to_llvm_str()`
- `src/llvm_gen.rs` — `lower_ret_coerced()` for the alloca+load pattern, `find_fn_def_id()`
- `src/stub_gen.rs` — extern declarations use `extern "C"` (both ABIs produce the same coercion)

All four tests pass: `counter_test`, `pair_test`, `host`, `layout_test`.
