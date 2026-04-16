# Phase 6: Inline Functions Linking Problem — Full Investigation Report

**Date:** 2026-04-14 to 2026-04-15  
**Status:** Resolved (Wrapper Functions approach selected)  
**Scope:** Option/Result `.unwrap()` support and broader inline function linking issues

---

## Executive Summary

toylang discovered that calling Rust stdlib methods like `Option::unwrap()` fails at link time with "undefined symbol" errors. Investigation revealed that `#[inline(always)]` functions have no global callable symbols in rustc's generated code — they're only inlined at call sites.

**Root cause:** Normal Rust works because rustc inlines these functions during its own codegen. toylang compiles separately via LLVM, producing external object files that rustc never designed to link against inline functions.

**Solution:** Generate non-inline wrapper functions in `__lang_stubs` that toylang can link to. The wrappers call the original functions, which rustc inlines into the wrapper bodies, producing linkable symbols. This requires **no rustc modifications** — it's a pure toylang solution.

**Scope:** Affects ~43+ Vec methods, Option methods, Result methods, and all `#[track_caller]` functions that should be callable from toylang.

---

## The Problem

### Symptom

Test `test_option_unwrap_basic` fails at link time:

```
Undefined symbols for architecture arm64:
  "core::option::Option$LT$T$GT$::unwrap::he7663eaebf9392fa"
```

toylang's LLVM-compiled `.o` file declares this symbol as external (`declare external @symbol`) but the linker cannot find it anywhere.

### Why This Happens

**Normal Rust compilation:**
```rust
// User calls unwrap():
let x = Some(5).unwrap();
```
Rustc inlines `Option::unwrap()` directly at the call site during codegen, producing machine code inline. No symbol is generated because no call exists — it's all inlined.

**toylang's separate compilation:**
```toylang
// toylang code calls unwrap():
let x = opt.unwrap();
```

1. toylang generates LLVM IR with `call @unwrap(...)`
2. toylang emits this to a separate `.o` file via LLVM backend
3. Linker tries to resolve `@unwrap` symbol
4. **rustc's `.rlib` has no `unwrap` symbol** — it was inlined everywhere
5. Linker fails: symbol not found

### The Fundamental Mismatch

| Context | How inline functions work |
|---------|---|
| **Normal Rust** | rustc controls all codegen → can inline everywhere → no symbol needed |
| **toylang** | rustc compiles stdlib, toylang compiles separately → each needs callable symbols from the other |

toylang is trying to call functions that rustc **intentionally didn't generate symbols for**.

---

## Investigation Phases

### Phase 1: Root Cause Analysis

**Finding:** `Option::unwrap` is decorated with `#[inline(always)]`, forcing rustc to inline it everywhere.

```rust
impl<T> Option<T> {
    #[inline(always)]
    pub fn unwrap(self) -> T { ... }
}
```

**Why:** The attribute tells rustc "always inline this, never generate a callable symbol." This is an optimization — the function is small and benefits from inlining into each call site.

**Implication:** When toylang tries to call it as a normal function, there's no symbol to call.

### Phase 2: Function Pointer Mechanism (ReifyShim)

When Rust code takes a function pointer to an `#[inline(always)]` function, rustc has to generate *something* callable. It creates a `ReifyShim` — a thin wrapper that provides a function pointer address.

**The twist:** ReifyShim has **internal linkage only**.

From rustc's `rustc_monomorphize/src/partitioning.rs`:
```rust
// These are all compiler glue and such, never exported, always hidden.
InstanceKind::ReifyShim(..)
| InstanceKind::FnPtrShim(..)
| ... => return Visibility::Hidden,

// Later:
linkage: Linkage::Internal,  // Only visible within this object file
```

**Why internal?** ReifyShim is per-codegen-unit (per `.o` file), and each can have its own copy with internal linkage. The linker never needs to see them.

**Problem for toylang:** Internal symbols are invisible across object file boundaries. toylang's LLVM-generated `.o` can't link against them.

### Phase 3: Three Proposed Solutions

We investigated three approaches for fixing this:

#### **Option A: Make ReifyShim WeakODR Linkage**

**Idea:** Change rustc to give ReifyShim external weak linkage instead of internal.

**Changes needed:** ~50-100 lines in rustc's `partitioning.rs`

**Why it doesn't work on your machine:**
- Weak linkage requires COMDAT section support in the linker
- macOS (Darwin) **does not support COMDAT**
- Would break on macOS, AIX, and similar platforms
- **You're on macOS** ❌

**Also doesn't solve:** `#[track_caller]` functions (different mechanism, still internal linkage)

**Verdict:** Viable for Linux/Windows, but not cross-platform.

#### **Option B: Generate Wrapper Functions** ✅ **SELECTED**

**Idea:** toylang detects which functions are `#[inline]` and generates non-inline wrapper functions in `__lang_stubs.rs`.

```rust
// Original: #[inline(always)]
#[inline(always)]
fn unwrap(self) -> T { ... }

// Generated wrapper: #[inline(never)]
#[inline(never)]
pub fn __toylang_unwrap_wrapper<T>(opt: Option<T>) -> T {
    opt.unwrap()  // rustc inlines this
}
```

**How it works:**
1. toylang detects that `unwrap` is inline
2. Generates wrapper Rust code
3. rustc compiles wrapper normally (no special handling)
4. Wrapper gets a normal external linkage symbol
5. rustc inlines original into wrapper (optimization isn't lost)
6. toylang calls wrapper symbol instead

**Changes needed:** ~150 lines in toylang, **zero changes to rustc**

**Why this works on macOS:** Wrappers are normal functions → normal linkage → works everywhere

**Also solves:** `#[track_caller]` functions automatically (wrapper absorbs hidden parameter)

**Advantages:**
- ✅ Works on all platforms (macOS, Linux, Windows)
- ✅ No rustc modifications
- ✅ Solves both inline and `#[track_caller]` issues
- ✅ Per-function approach means adding new wrappers is easy
- ✅ Follows existing patterns in codebase (accessor wrappers already exist)

**Disadvantages:**
- Requires detecting which functions are inline (doable via existing rustc APIs)
- Requires generating wrapper code (straightforward)

**Verdict:** Best approach for Phase 6. Pragmatic, works everywhere, no rustc risk.

#### **Option C: Add Wrapper InstanceKind to rustc**

**Idea:** Add a new `Wrapper` variant to `InstanceKind` enum in rustc, telling rustc to generate non-inline wrappers explicitly.

```rust
pub enum InstanceKind {
    Item(DefId),
    ReifyShim(DefId),
    Virtual(usize),
    // ... existing variants ...
    Wrapper(DefId, GenericArgsRef<'tcx>),  // NEW
}
```

**Changes needed:** ~500-800 lines across ~7 rustc files

**How it works:**
1. toylang detects inline functions
2. Tells rustc to create `Wrapper` instances (via per_instance_mir)
3. rustc handles everything: codegen, symbol naming, monomorphization
4. toylang calls the wrapper symbols

**Advantages:**
- ✅ Most systematic, elegant solution
- ✅ Works everywhere
- ✅ Solves multiple issues at once (inline, track_caller, const fn, sealed traits)
- ✅ Addresses ~43+ Vec methods and other stdlib functions comprehensively
- ✅ Integrates cleanly with rustc's machinery

**Disadvantages:**
- Much more complex (500+ lines vs 150 lines)
- Higher risk (modifying rustc internals)
- Overkill for just Phase 6

**Verdict:** Save for Phase 7+ if wrapper functions accumulate. Better to keep Phase 6 simple and focused.

---

## Detailed Investigation Findings

### Finding 1: Scope of the Problem

The inline function issue isn't unique to `Option::unwrap`. It affects:

- **Option methods:** `unwrap`, `expect`, `unwrap_or`, `map`, `and_then`, `or_else`, `zip`, `filter`, `flatten`, `take`, `replace`, `insert`, ...
- **Result methods:** `unwrap`, `expect`, `unwrap_err`, `map`, `and_then`, `or_else`, ...
- **Vec methods:** ~43+ methods decorated with `#[inline]` or `#[inline]` (some always, some conditional), including `push`, `pop`, `len`, `is_empty`, `iter`, `drain`, `retain`, ...
- **Other:** String methods, slice methods, Iterator adaptors, and all functions marked with `#[track_caller]`

This is **not a one-off edge case** — it's a systematic architectural issue affecting ~100+ stdlib functions.

### Finding 2: #[track_caller] Interaction

Functions with `#[track_caller]` have a hidden parameter injected by rustc:

```rust
#[track_caller]
fn some_fn() { ... }

// Rustc transforms to:
fn some_fn(caller_location: &'static core::panic::Location) { ... }
```

The `Location` is injected at the call site, not explicitly passed. When you try to call this from external LLVM code, there's no mechanism to inject the hidden parameter.

**Wrapper solution:** The wrapper absorbs the hidden parameter:
```rust
#[inline(never)]
pub fn __toylang_some_fn_wrapper() {
    some_fn()  // rustc injects Location here
}
```

The wrapper calls the original from within rustc's compiled code, so rustc injects the location. toylang calls the wrapper, no issues.

**ReifyShim solution:** Doesn't help — ReifyShim still has internal linkage.

### Finding 3: Per-CGU Duplication Design

ReifyShim's internal linkage and per-CGU design is intentional:

```
CGU 1: {
    __internal_reify_unwrap_1: Linkage::Internal
}

CGU 2: {
    __internal_reify_unwrap_2: Linkage::Internal
}

Linker: Deduplicates via COMDAT or ignores (they're internal anyway)
```

This is a valid design for rustc's use case (intra-crate symbol resolution). But it breaks for inter-module calling across separately-compiled object files.

### Finding 4: Symbol Naming

ReifyShim symbols use mangling that includes a `reify` discriminant:

```
Original: _ZN4core6option6Option6unwrap17he7663eaebf9392faE
ReifyShim: _ZN4core6option6Option6unwrap...{{shim:reify#0}}...
```

Symbol names are deterministic but complex, making it hard for toylang to predict them without rustc queries. This is another reason coordination is difficult.

### Finding 5: Rustc's Architecture Assumption

The root issue is architectural: rustc assumes all code it needs to call is either:
1. Inlined (no symbol needed), or
2. In the same compilation context (same rlib, same CGU)

toylang violates both assumptions — it's external code calling stdlib functions that were designed to be inlined.

---

## Why Each Solution (or Doesn't) Work

### Why WeakODR doesn't fully solve it:

1. **Platform incompatibility:** macOS doesn't support COMDAT → WeakODR breaks
2. **`#[track_caller]` incompatibility:** Still doesn't solve the hidden parameter problem
3. **Requires rustc modification:** Adds complexity and risk

### Why ReifyShim coordination doesn't work:

Even if toylang requested ReifyShim instances, they'd still have internal linkage by rustc's design. External LLVM code still can't link to them. And there's still no mechanism for `#[track_caller]` parameters.

### Why Wrapper Functions work:

1. **Platform universal:** Works on any platform (no COMDAT needed)
2. **Normal Rust code:** Wrappers are compiled by rustc like any other code → normal linkage rules apply
3. **Solves both issues:** Inline functions have a callable symbol, `#[track_caller]` parameters are injected by rustc
4. **Zero rustc changes:** No risk of breaking rustc
5. **Per-function scaling:** Easy to add wrappers for new functions as needed
6. **Existing patterns:** Accessor wrappers already in codebase (struct field accessors use same pattern)

---

## Implementation Plan: Wrapper Functions

### Phase 1: Detect inline functions
- **File:** `rustc-lang-facade/src/callbacks_impl.rs`
- **Work:** Query `attr::requires_inline()` for each rust_dep during monomorphization
- **Output:** OnceLock<HashSet<DefId>> of functions needing wrappers
- **Lines:** ~30-40

### Phase 2: Generate wrapper code
- **File:** `toylangc/src/stub_gen.rs`
- **Work:** For each DefId in wrapper set, generate Rust code:
  ```rust
  #[inline(never)]
  pub fn __toylang_<fn_name>_wrapper<T1, T2, ...>(args...) -> RetTy {
      original_function(args...)
  }
  ```
- **Output:** Added to `__lang_stubs.rs` during stub generation
- **Lines:** ~60-80

### Phase 3: Redirect calls
- **File:** `toylangc/src/oracle.rs`
- **Work:** When resolving a function symbol, check if it's in the wrapper set
- **Change:** Return wrapper symbol instead of original symbol
- **Lines:** ~30-40

**Total:** ~150 lines of code, no rustc modifications

---

## Implications Going Forward

### Phase 6 (Immediate)
- Implement wrapper functions for Phase 6 tests
- Unblock `Option::unwrap` and `Result::unwrap`
- Verify 5 integration tests pass
- ~6-8 hours work

### Phase 7+ (Medium term)
- As toylang gains more functionality, more stdlib methods will be needed
- Each new wrapper is a simple addition (~5-10 lines per function)
- If wrapper count becomes excessive (50+), consider Option C (Wrapper InstanceKind)
- But for now, Wrapper Functions is the right pragmatic approach

### Alternative Futures

**If wrapper count grows to 50+:** Could revisit Wrapper InstanceKind as a comprehensive refactor, making wrapper detection and generation automatic and systematic.

**If macOS support becomes critical for ReifyShim:** Would need to add platform-specific fallback logic (use External linkage on macOS, WeakODR elsewhere), adding complexity.

**If other ABIs emerge:** Wrapper pattern scales well — just generate more wrappers.

---

## Technical Deep Dives

### Why Inlining Isn't Lost

One concern: "Won't removing `#[inline(always)]` slow things down?"

**No.** The wrapper function itself is marked `#[inline(never)]`, but it calls the original function:

```rust
#[inline(never)]
pub fn __toylang_unwrap_wrapper<T>(opt: Option<T>) -> T {
    opt.unwrap()  // THIS is still #[inline(always)]
}
```

rustc sees the call to `unwrap()` inside the wrapper and **inlines it during codegen** of the wrapper. From the caller's perspective, they're calling the wrapper, which is not inlined (hence the symbol exists), but the actual unwrap code is inlined into the wrapper's body.

**Result:** You get the best of both worlds — a callable symbol AND the inlining optimization.

### Why Generics Work Fine

"Won't generic wrappers be monomorphized with mangled symbols?"

**Yes, and that's fine.** Each monomorphization (e.g., `__toylang_unwrap_wrapper<i32>`) gets its own concrete wrapper function with its own symbol. toylang can:
1. Detect which instantiation it needs
2. Compute the mangled symbol name using rustc's mangling rules
3. Call the specific monomorphization

This is exactly how toylang already calls generic functions elsewhere in the codebase.

### Why This Doesn't Require per_instance_mir Changes

toylang doesn't need to intercept these at the rustc level. The wrappers are:
1. Generated as normal Rust code
2. Compiled by rustc during normal `__lang_stubs.rs` compilation
3. Treated as normal functions (Item, not special InstanceKind)
4. rustc generates symbols as usual
5. toylang links to them as external symbols

No query provider modifications needed. Pure toylang solution.

---

## References

- **Original investigation:** `investigations/phase6-unwrap-linking-bug.md`
- **Plan:** `plans/jolly-sauteeing-pascal.md`
- **Related docs:** `docs/architecture/rust-interop-guide.md` (§10.6)
- **Existing pattern:** Accessor wrappers in `toylangc/src/stub_gen.rs` (struct field accessors)

---

## Conclusion

The inline function problem is a fundamental architectural mismatch: rustc assumes all code is inlined during its own compilation, but toylang compiles separately. Rather than fight rustc's design (ReifyShim WeakODR) or add significant complexity (Wrapper InstanceKind), the Wrapper Functions approach embraces the separation of concerns:

- Rustc compiles stdlib and wrapper code normally
- toylang compiles its code separately
- Both produce object files with external symbols
- Linker resolves them cleanly

This is a pragmatic, universal solution that requires no rustc modifications and scales well. It unblocks Phase 6 and sets a pattern for future inline function calls.