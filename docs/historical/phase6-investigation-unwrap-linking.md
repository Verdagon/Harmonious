# Investigation: Option::unwrap() Linking Bug

## Symptom
- Test `test_option_unwrap_basic` fails at link time
- Error: `"core::option::Option$LT$T$GT$::unwrap::he7663eaebf9392fa"` not found
- Test `test_vec_pop_unwrap` panics before reaching linker (separate bug)

## Root Causes Identified

### Issue #1: MethodCall Codegen Crashes on Value-Type Receivers (Immediate)
**Location:** `toylangc/src/llvm_gen.rs:1487`

When `Vec::pop()` returns `Option<i32>` as a value and toylang tries to call `.unwrap()` on it:
- The receiver is `ExprResult::Value`, not `ExprResult::Ptr`
- MethodCall codegen panics expecting all receivers to be pointers
- This blocks testing of .unwrap() on value types

**Fix needed:** Handle value-type receivers by allocating space and taking a pointer.

### Issue #2: Inline Functions Don't Produce Global Symbols (Structural)
**Root cause:** `Option::unwrap` is `#[inline(always)]`, which means:
1. Rustc never generates a global callable symbol for direct calls
2. The function is always inlined at call sites
3. When you take a function pointer, rustc generates a `ReifyShim` with **internal linkage only**
4. toylang's LLVM backend emits `declare external @unwrap(...)`, but that symbol doesn't exist anywhere

**Evidence:**
- Normal Rust can call `Option::unwrap` fine - rustc inlines it
- Normal Rust can take function pointers to it - rustc generates a `ReifyShim` with internal linkage
- But neither produces a global symbol that external code can link against
- The collector discovers `Option::unwrap` via ReifyFnPointer but generates a shim with internal linkage
- toylang tries to call it from external LLVM IR → linker can't find it

### Issue #3: #[track_caller] Instance Kind Mismatch (Pre-existing)
- Collector uses `resolve_for_fn_ptr` → creates `ReifyShim(unwrap_def_id)`
- toylangc uses `expect_resolve` → creates `Item(unwrap_def_id)`
- These have different symbols, but would be a secondary issue after fixing inline problem

## The Fundamental Problem

**toylang's architecture of calling Rust functions directly from external LLVM IR doesn't work for:**
- `#[inline]` functions (no global symbol exists)
- `#[track_caller]` functions (hidden parameters, needs shim)
- Generic functions (symbol name issues with crate context)

Normal Rust solves this because rustc controls the entire compilation and can inline these functions during its own codegen. toylang is trying to call them from a separate LLVM-compiled object, which rustc never designed for.

---

## Long-Term General Solution

**Generate non-inline wrapper functions in `__lang_stubs` for all inline stdlib functions.**

Instead of having toylang try to call `Option::unwrap` directly:
1. The stub generator detects when a Rust function is `#[inline]` or `#[inline(always)]`
2. For each such function, it generates a wrapper with `#[inline(never)]` in `__lang_stubs.rs`
3. The wrapper just calls the original function (which gets inlined by rustc into the wrapper)
4. The wrapper has a real, linkable global symbol
5. toylang calls the wrapper instead of the original

**Example:**
```rust
// Original from core:
// #[inline(always)]
// fn unwrap(self) -> T { ... }

// Generated in __lang_stubs:
#[inline(never)]
pub fn __toylang_unwrap_wrapper<T>(opt: Option<T>) -> T {
    opt.unwrap()
}
```

**Benefits:**
- Solves the inline problem (wrapper gets codegen'd with external linkage)
- Works for `#[track_caller]` (wrapper absorbs the hidden parameter)
- Works for generics (wrapper is concrete per monomorphization)
- Systematic: applies to all inline functions automatically
- Minimal changes to toylang's LLVM backend

**Implementation strategy:**
- During stub generation, query `attr::requires_inline()` for each Rust function
- If inline, generate a wrapper with explicit `#[inline(never)]`
- In toylang's oracle/codegen, redirect calls to inline functions to their wrappers

---

## Investigation Complete

The core issue is **architectural mismatch between how rustc handles inline functions (inlining during codegen) and how toylang tries to call them (as external symbols from LLVM IR).** The wrapper solution bridges this gap systematically.
