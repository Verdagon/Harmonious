# Monomorphization Problem: Generic Wrapper Functions Not Generated

**Date:** 2026-04-15  
**Status:** Blocker — Generic wrapper functions for inline stdlib methods are not being monomorphized by rustc  
**Scope:** Phase 6 — Option/Result `.unwrap()` support in toylang

---

## Executive Summary

toylang has implemented wrapper functions for Rust's `#[inline(always)]` stdlib methods (like `Option::unwrap()`) so that toylang's separate LLVM compilation can call them. The wrappers are defined as generic Rust functions in `__lang_stubs.rs`:

```rust
#[inline(never)]
pub fn __toylang_option_unwrap<T>(opt: Option<T>) -> T {
    opt.unwrap()
}
```

**The problem:** When toylang's LLVM IR references this wrapper with a specific type (e.g., `Option<i32>`), it computes the correct monomorphized symbol name (`_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE`), but **rustc never generates the code for that monomorphization**. The linker therefore cannot find the symbol.

**Root cause:** rustc only monomorphizes generic functions when Rust code calls them. Since toylang's LLVM IR calls are invisible to rustc (LLVM IR is external bytecode, not Rust source), rustc doesn't know the wrapper needs to be monomorphized.

**Current state:** Compilation succeeds, but linking fails with "undefined symbol" errors.

---

## The Symptom

When running `test_option_unwrap_basic`:

```
[DEBUG llvm_gen MethodCall] Using wrapper for Option.unwrap: _ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE

error: linking with `cc` failed: exit status: 1
  ...
  Undefined symbols for architecture arm64:
    "_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE", referenced from:
        ___toylang_internal_main in toylang_output_73641.o
  ld: symbol(s) not found for architecture arm64
```

The symbol name is correct (rustc's v0 mangled name for `test::__lang_stubs::__toylang_option_unwrap::<i32>`), but it doesn't exist in any compiled object file.

---

## Technical Details

### How the wrapper is called

1. **toylang codegen** encounters a method call: `opt.unwrap()` where `opt: Option<i32>`
2. **llvm_gen.rs** detects this needs a wrapper and:
   - Calls `find_wrapper_fn_def_id(tcx, "__toylang_option_unwrap")` → finds the wrapper's DefId
   - Constructs an `Instance` with the wrapper DefId and type args `[i32]`
   - Calls `tcx.symbol_name(wrapper_instance)` → gets `_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE`
   - Emits LLVM IR: `declare external @_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE`
3. **LLVM backend** compiles this to an `.o` file with an unresolved external reference
4. **Linker** tries to find the symbol in compiled object files
5. **Linker fails** — the symbol doesn't exist anywhere

### Why the symbol doesn't exist

When rustc compiles `__lang_stubs.rs`, it sees:

```rust
#[inline(never)]
pub fn __toylang_option_unwrap<T>(opt: Option<T>) -> T {
    opt.unwrap()
}
```

rustc's monomorphization pass asks: "What instantiations of `__toylang_option_unwrap` are actually used by Rust code?"

The answer: **None.** No Rust code calls `__toylang_option_unwrap`. Only toylang's LLVM IR references it, and rustc doesn't see LLVM IR (it's compiled separately, outside rustc's knowledge).

Therefore, rustc's monomorphization pass never generates code for `__toylang_option_unwrap<i32>`, `__toylang_option_unwrap<i64>`, or any other instantiation. The generic function stays as a template but is never instantiated.

### Symbol name calculation is correct

toylang correctly queries `tcx.symbol_name()` for the wrapper, which uses rustc's v0 symbol mangling. The computed symbol is the correct name for that monomorphization — **if it existed**.

The problem is not the symbol name calculation; it's that the symbol was never generated because the monomorphization never happened.

---

## Why This Is An Architectural Mismatch

Rust's monomorphization design assumes:

1. **All code that's compiled together knows about each other** — rustc compiles the entire crate at once and can see all Rust code
2. **Generic instantiations are driven by Rust call sites** — when Rust code calls `foo::<T>()`, monomorphization happens
3. **External code (FFI) uses specific concrete functions** — FFI calls into non-generic `extern "C"` functions

toylang violates assumption 2:

- **toylang code** is compiled to LLVM IR in a separate phase (outside rustc)
- **toylang's LLVM IR** references generic Rust functions with specific type arguments
- **rustc never sees toylang code** — so it never triggers monomorphization

---

## Previous Attempts and Why They Failed

### Attempt 1: Mark wrappers with `#[used]`

**Idea:** `#[used]` tells the compiler not to optimize away unused items.

**Result:** Failed. `#[used]` prevents dead code elimination, but doesn't trigger monomorphization. The wrapper function still exists (unmonomorphized) in the compiled code.

### Attempt 2: Synthetic static fn pointers

**Idea:** Add static items that take function pointers to the wrapper, forcing rustc to monomorphize:

```rust
static _: &dyn Fn(Option<i32>) -> i32 = &__toylang_option_unwrap::<i32>;
```

**Result:** Caused rustc ICE (internal compiler error). Per_instance_mir or some other hook may not handle this pattern correctly.

### Attempt 3: Concrete monomorphic wrappers (workaround, not solution)

**Idea:** Generate non-generic wrappers for known types:

```rust
#[inline(never)]
#[no_mangle]
pub fn __toylang_option_unwrap_i32(opt: Option<i32>) -> i32 {
    opt.unwrap()
}
```

**Result:** Worked! But violates the architectural goal of having a single generic wrapper that toylang instantiates. This is a **workaround**, not a proper solution.

---

## What Would Solve This

### Option A: Have rustc monomorphize the wrapper

**Mechanism:** toylang tells rustc to monomorphize specific wrapper instances.

**Possible approaches:**
1. Register wrapper instances as dependencies that must be monomorphized (via callbacks or per_instance_mir)
2. Have per_instance_mir intercept references to wrapper functions and force monomorphization
3. Have a build script extract needed monomorphizations from toylang and pass them to rustc
4. Modify per_instance_mir to track which wrapper instances toylang's LLVM IR uses, then ensure they're compiled

**Advantage:** Architecturally clean, single generic wrapper, scales to arbitrary types  
**Challenge:** Requires coordination between toylang's LLVM codegen and rustc's monomorphization

### Option B: Make wrapper calls visible to rustc as Rust code

**Mechanism:** Generate Rust code that calls the wrappers, creating Rust call sites that trigger monomorphization.

**Possible approaches:**
1. Generate synthetic Rust functions that call the wrapper (likely to be dead-code-eliminated)
2. Register wrapper instances in a static that's referenced from Rust code
3. Use a build script to generate explicit Rust calls for needed instantiations

**Advantage:** Uses rustc's existing monomorphization machinery  
**Challenge:** Creating Rust call sites that can't be optimized away

### Option C: Monomorphize upfront in stub_gen (current workaround)

**Mechanism:** Before linking, scan toylang's code to find all Option/Result types used, generate concrete wrappers for each.

**Advantage:** Simple, works immediately  
**Disadvantage:** Not scalable, requires knowing all types upfront, doesn't generalize to arbitrary wrappers

---

## Information Needed

To solve this, we need to understand:

1. **When does per_instance_mir get invoked?** Does it handle wrapper functions at all, or only toylang functions?
2. **Can per_instance_mir register dependencies for monomorphization?** Is there a mechanism to tell rustc "I need this generic function monomorphized with these type args"?
3. **What's the relationship between toylang's LLVM IR generation and per_instance_mir's MIR interception?** When toylang references a Rust function, does that trigger per_instance_mir at the right time?
4. **How do other tools/frameworks that do separate compilation handle this?** (e.g., procedural macros, build scripts, FFI frameworks)

---

## Impact

This blocks:

- Phase 6: `.unwrap()` support for Option/Result
- Any other inline functions that need wrappers (Vec methods, String methods, etc.)
- Any generic Rust function that's only called from external LLVM code

The workaround (concrete monomorphic wrappers) works for Phase 6, but is not a general solution.

---

## Key Files

- **`toylangc/src/stub_gen.rs`** — Generates `__toylang_option_unwrap<T>` and other wrappers
- **`toylangc/src/llvm_gen.rs`** — Resolves wrapper symbols and emits LLVM IR references (lines 382-403)
- **`toylangc/src/oracle.rs`** — Finds wrapper DefIds and queries their symbols (find_wrapper_fn_def_id, ~line 329)
- **`toylangc/src/toylang/callbacks_impl.rs`** — per_instance_mir hooks (if applicable)
- **`rustc-lang-facade/src/callbacks_impl.rs`** — rustc query hooks (if applicable)

---

## Code Changes Made

### 1. Wrapper Function Generation (toylangc/src/stub_gen.rs, lines ~179-207)

Generic wrapper functions are generated in `__lang_stubs.rs`:

```rust
// Phase B: Generate wrappers for inline stdlib functions that toylang needs to call.
// These wrappers are marked #[inline(never)] so rustc generates external symbols.
// The wrappers simply call the original (inline) functions, which get inlined by rustc.
//
// We generate generic wrappers here. When toylang encounters a call to unwrap() with
// a specific type (e.g., Option<i32>::unwrap), it queries rustc for the symbol name
// of the instantiated wrapper function, just like it does for other generic functions.
//
// We generate these for known stdlib functions that are problematic:
// - Option::unwrap: #[inline(always)]
// - Result::unwrap: #[inline(always)]
// (More can be added as needed for other inline functions)
let stdlib_wrappers: Vec<syn::Item> = vec![
    // Option::unwrap<T> generic wrapper
    parse_quote! {
        #[inline(never)]
        pub fn __toylang_option_unwrap<T>(opt: core::option::Option<T>) -> T {
            opt.unwrap()
        }
    },
    // Result::unwrap<T, E> generic wrapper (E must implement Debug for unwrap to work)
    parse_quote! {
        #[inline(never)]
        pub fn __toylang_result_unwrap<T, E: core::fmt::Debug>(res: core::result::Result<T, E>) -> T {
            res.unwrap()
        }
    },
];
items.extend(stdlib_wrappers);
```

These functions are added to the generated `__lang_stubs` module which is compiled by rustc as part of the test.

### 2. Wrapper Function Discovery (toylangc/src/oracle.rs, lines ~329-364)

Added `find_wrapper_fn_def_id` to locate wrapper function DefIds in the compiled `__lang_stubs` module:

```rust
/// Find a wrapper function in __lang_stubs by name.
/// Wrappers are generated with names like __toylang_option_unwrap, __toylang_result_unwrap, etc.
pub fn find_wrapper_fn_def_id(tcx: TyCtxt<'_>, wrapper_name: &str) -> Option<DefId> {
    use rustc_hir::def::Res;

    eprintln!("[DEBUG find_wrapper_fn_def_id] looking for wrapper: {}", wrapper_name);

    // First, find the __lang_stubs module
    let mut lang_stubs_def_id = None;
    for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
        if child.ident.as_str() == "__lang_stubs" {
            if let Res::Def(DefKind::Mod, def_id) = child.res {
                eprintln!("[DEBUG find_wrapper_fn_def_id] found __lang_stubs module: {:?}", def_id);
                lang_stubs_def_id = Some(def_id);
                break;
            }
        }
    }

    // If found, search for the wrapper function in the module
    if let Some(module_def_id) = lang_stubs_def_id {
        if let Some(local_def_id) = module_def_id.as_local() {
            for child in tcx.module_children_local(local_def_id) {
                eprintln!("[DEBUG find_wrapper_fn_def_id]   child: {}", child.ident.as_str());
                if child.ident.as_str() == wrapper_name {
                    if let Res::Def(DefKind::Fn, def_id) = child.res {
                        eprintln!("[DEBUG find_wrapper_fn_def_id] FOUND: {:?}", def_id);
                        return Some(def_id);
                    }
                }
            }
        } else {
            eprintln!("[DEBUG find_wrapper_fn_def_id] __lang_stubs module is not local");
        }
    } else {
        eprintln!("[DEBUG find_wrapper_fn_def_id] __lang_stubs module not found");
    }

    eprintln!("[DEBUG find_wrapper_fn_def_id] NOT FOUND: {}", wrapper_name);
    None
}
```

This function:
1. Searches the crate root for the `__lang_stubs` module
2. If found, searches that module's children for the wrapper function by name
3. Returns the DefId if found, which can then be used with rustc's `symbol_name` query

### 3. Wrapper Detection and Resolution (toylangc/src/llvm_gen.rs, lines ~382-403)

In `get_or_resolve_rust_method`, added logic to detect when a wrapper is needed and redirect to it:

```rust
// Phase C: Check if this method needs a wrapper (inline or track_caller).
// If so, redirect to the wrapper function instead of the original.
let symbol = if should_use_wrapper(type_name, method_name) {
    // Wrapper is needed. Find its DefId and query its symbol through rustc.
    let wrapper_base_name = compute_wrapper_name(type_name, method_name);
    if let Some(wrapper_def_id) = crate::oracle::find_wrapper_fn_def_id(self.tcx, &wrapper_base_name) {
        // Instantiate the wrapper with the given type arguments
        let wrapper_instance = ty::Instance::expect_resolve(
            self.tcx,
            ty::TypingEnv::fully_monomorphized(),
            wrapper_def_id,
            &args,
            rustc_span::DUMMY_SP,
        );
        let wrapper_symbol = self.tcx.symbol_name(wrapper_instance).name.to_string();
        eprintln!("[DEBUG llvm_gen MethodCall] Using wrapper for {}.{}: {}", type_name, method_name, wrapper_symbol);
        wrapper_symbol
    } else {
        eprintln!("[WARNING llvm_gen] Could not find wrapper function {}", wrapper_base_name);
        symbol  // Fall back to original if wrapper not found
    }
} else {
    symbol
};
```

The key steps:
1. Check if the method needs a wrapper (`should_use_wrapper`)
2. Compute the base wrapper name (`__toylang_option_unwrap`)
3. Find the DefId using `find_wrapper_fn_def_id`
4. Create an `Instance` with the type arguments (`&args`)
5. Query rustc for the symbol name of that instance
6. Use the computed symbol in LLVM IR

### 4. Helper Functions (toylangc/src/llvm_gen.rs, lines ~1911-1922)

```rust
/// Check if a method needs a wrapper (inline or #[track_caller]).
/// Phase 6 handles Option::unwrap and Result::unwrap.
/// More can be added as needed for other inline functions.
fn should_use_wrapper(type_name: &str, method_name: &str) -> bool {
    match (type_name, method_name) {
        ("Option", "unwrap") => true,
        ("Result", "unwrap") => true,
        _ => false,
    }
}

/// Compute the generic wrapper function name.
/// This returns the Rust function name like __toylang_option_unwrap (without type parameters).
/// The actual monomorphized symbol will be computed by querying rustc with type instantiation.
fn compute_wrapper_name(type_name: &str, method_name: &str) -> String {
    match (type_name, method_name) {
        ("Option", "unwrap") => "__toylang_option_unwrap".to_string(),
        ("Result", "unwrap") => "__toylang_result_unwrap".to_string(),
        _ => format!("__toylang_{}_{}", type_name.to_lowercase(), method_name.to_lowercase()),
    }
}
```

---

## Execution Flow

Here's the complete flow when toylang encounters `opt.unwrap()` where `opt: Option<i32>`:

1. **Parse and type-check** — toylang's parser identifies this as a method call on `Option<i32>`
2. **llvm_gen encounters it** — `get_or_resolve_rust_method("Option", "unwrap", [i32])`
3. **Check for wrapper** — `should_use_wrapper("Option", "unwrap")` returns `true`
4. **Compute wrapper name** — `compute_wrapper_name("Option", "unwrap")` returns `"__toylang_option_unwrap"`
5. **Find wrapper DefId** — `find_wrapper_fn_def_id(tcx, "__toylang_option_unwrap")` searches `__lang_stubs` module and finds DefId
6. **Create instance** — `Instance::expect_resolve(..., wrapper_def_id, [i32], ...)` — this is where monomorphization SHOULD happen
7. **Query symbol** — `tcx.symbol_name(wrapper_instance)` returns `_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE`
8. **Emit LLVM** — Generate `declare external @_ZN4test12__lang_stubs23__toylang_option_unwrap17hf0150b200c563b5eE` and call it
9. **Link fails** — Symbol doesn't exist because step 6 didn't actually trigger code generation

---

## References

- [Phase 6 Investigation Doc](docs/historical/phase6-inline-functions-investigation.md) — Full technical analysis of inline function problem and wrapper solution
- [Architecture Guide](docs/architecture/rust-interop-guide.md) — How toylang integrates with rustc
- [Plan](/.claude/plans/jolly-sauteeing-pascal.md) — Original Phase 6 implementation plan
