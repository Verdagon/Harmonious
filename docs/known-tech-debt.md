# Known Technical Debt

> Last updated: session 5 (55 tests passing, 0 ignored)
>
> Items 1–9 from the original list are resolved. Item 10 (panics) expanded
> into a broader set of issues discovered during a fresh codebase scan.

---

## 1. Hardcoded `Vec::new` in Type Resolver

### Problem

Only `("Vec", "new")` is handled for static calls in `type_resolve.rs:302-320`.
Any other static method panics with "unsupported static call". This prevents
`HashMap::new`, `Option::Some`, user-defined constructors, etc.

### Fix Options

**Option A: Query `tcx.fn_sig()` for static calls** (same as method calls).
The infrastructure exists — `rust_method_return_type` in oracle.rs already
queries fn_sig for inherent methods. Extend to handle static calls the same
way: find the method DefId, query its signature, determine return type.

**Option B: General static call resolution in type_resolve.** Instead of
matching `("Vec", "new")`, look up the type name in the registry or as a
RustType, find the method, and query its return type via the `rust_method_ret`
callback (which already delegates to `oracle::rust_method_return_type`).

---

## 2. Method Call Arg Type Inference Uses Only `type_args[0]`

### Problem

In `type_resolve.rs:357-368`, method call arguments all get `type_args[0]` as
their expected type. Works for `Vec<T>::push(T)` but breaks for multi-parameter
methods like `HashMap<K, V>::insert(k, v)` where k and v have different types.

### Fix Options

**Option A: Query method signature for param types.** Use `tcx.fn_sig()` on
the method DefId (via the `rust_method_ret` callback or a new
`rust_method_param_types` callback) to get the actual parameter types. Map
each arg to its corresponding param type.

**Option B: Extend `rust_method_ret` to return full signature.** Change the
callback to return param types too (or add a parallel callback). The type
resolver uses the full signature for arg type inference.

---

## 3. `mangle_ty_for_symbol` Fallback to Debug Format

### Problem

In `callbacks_impl.rs:418`, unknown types get `format!("{:?}", ty)` as their
symbol mangling. This produces nondeterministic, non-demanglable symbols and
could cause collisions.

### Fix Options

**Option A: Extend the match to cover all TyKind variants** that can appear
in generic args (slices, references, raw pointers, tuples). Panic on truly
unexpected types.

**Option B: Use rustc's own symbol mangling.** Call `tcx.symbol_name()` on a
dummy Instance to get the correctly mangled form, or delegate to
`resolved_type_to_mangled_name` (which already handles ResolvedType).

---

## 4. `PassMode::Indirect { on_stack: true }` Unimplemented

### Problem

In `abi_helpers.rs:127-132`, byval parameters (used on 32-bit x86) hit an
assert and panic. The code only handles `on_stack: false` (pointer indirect).

### Fix Options

**Option A: Emit byval attribute.** When `on_stack: true`, emit
`CoercedParam::Indirect` but with a note to add the LLVM `byval` attribute.
The extern wrapper passes the value on the stack instead of by pointer.

**Option B: Emit as Direct with the full type size.** Treat byval as a large
Direct parameter. LLVM handles the stack copy.

---

## 5. `PassMode::Pair` Treated as Single Scalar

### Problem

In `abi_helpers.rs:66-84`, ScalarPair returns are treated as a single integer
of the total size. The comment says "rare for extern functions" but this isn't
validated. Could silently produce wrong register assignments.

### Fix Options

**Option A: Emit as LLVM struct return `{ scalar1, scalar2 }`.** Parse the
two scalars from the Pair and emit a two-field struct type string. The caller
unpacks the two values.

**Option B: Validate that Pair doesn't occur for extern functions.** Add a
diagnostic check: if Pair appears for a consumer function's extern wrapper,
emit an error explaining the limitation.

---

## 6. Panics Instead of User-Facing Errors (~30 sites)

### Problem

User code errors cause compiler panics instead of proper error messages.
Scattered across type_resolve.rs (~12 sites), llvm_gen.rs (~15 sites),
callbacks_impl.rs (~5 sites), oracle.rs (~3 sites).

### Fix Options

**Option A: Thread `Result` through the type resolver first.** The type
resolver is the first place user errors surface (unknown types, wrong arg
counts, etc.). Convert `resolve_fn_body` and `resolve_expr` to return
`Result<T, Vec<Diagnostic>>`. Codegen panics are compiler bugs (acceptable).

**Option B: Incremental — add `Result` at the outermost boundary.** Wrap
`resolve_fn_body` in a `catch_unwind`, collect the panic message, and report
it as a user error. Hacky but fast to implement.

---

## 7. No Parser Tests

### Problem

Zero tests for the parser. Type resolution has 10 unit tests, but parser
syntax handling (struct parsing, function signatures, expressions, type
parsing, error recovery) is completely untested.

### Fix Options

**Option A: Unit tests for `parse_type`.** Test each ResolvedType variant:
primitives, refs, generics, nested generics, TypeParam, StructRef, RustType.

**Option B: Round-trip tests.** Parse toylang source → check registry contents
(struct names, field types, function signatures).

---

## 8. No Error Case Integration Tests

### Problem

All 55 integration tests are positive cases. No tests verify that the compiler
reports errors (instead of panicking) for: wrong types, missing structs, wrong
arg counts, undefined methods, etc.

### Fix Options

Depends on #6 (error handling). Once errors are `Result`-based, add tests that
assert compilation fails with a specific error message instead of a panic.

---

## 9. `after_rust_analysis` Is a TODO Stub

### Problem

`callbacks_impl.rs:43-44`: `fn after_rust_analysis` does nothing. Toylang
types are never validated against Rust types. A toylang function claiming to
return `Point` but actually returning `Counter` won't be caught.

### Fix Options

**Option A: Type-check toylang function bodies against Rust signatures.** In
`after_rust_analysis`, run the type resolver and compare resolved types against
what rustc expects (from the stub signatures).

**Option B: Defer to codegen.** Mismatches will surface as LLVM type errors
or ABI mismatches at codegen time. Less user-friendly but catches the same
bugs.

---

## 10. No Tests for Rust Types Other Than Vec

### Problem

The mechanism is general (fn_abi, fn_sig queries), but only Vec is exercised
in tests. HashMap, String, Option, etc. are untested. Subtle differences in
their ABI or method signatures could break silently.

### Fix Options

Add integration tests for at least one more Rust type (e.g., `String` with
`push_str`/`len`, or a simple wrapper type) to validate the general mechanism.

---

## 11. `parse_coerced_type` / `parse_struct_type_str` Duplication

### Problem

Two LLVM type string parsers in `llvm_gen.rs` with overlapping primitive
handling (`"i32"`, `"i64"`, `"double"`, etc.). Both parse strings from
`rustc_lang_facade` ABI helpers.

### Fix Options

**Option A: Unify into one parser.** `parse_coerced_type` already delegates
to `parse_struct_type_str` for struct types. Merge the remaining primitives.

**Option B: Change ABI helpers to return structured types** instead of strings.
`CoercedReturn::Direct` and `CoercedParam::Direct` would carry an enum
(like `AbiType::Int(bits)`, `AbiType::Float`, `AbiType::Ptr`, etc.) instead
of a string. Eliminates string parsing entirely.

---

## 12. Duplicate Field Lookup Pattern

### Problem

`type_resolve.rs` has `fields.iter().position(|f| f.name == ...)` repeated
2+ times with near-identical panic messages.

### Fix Options

Extract `fn find_field_index(struct: &ToyStruct, name: &str) -> Result<usize>`.

---

## 13. `resolved_to_rustc_ty` Forwarding Wrapper

### Problem

`callbacks_impl.rs:362-364` is a trivial one-line wrapper around
`oracle::resolved_to_rustc_ty`. Used 3 times in the same file.

### Fix Options

Delete the wrapper. Call `crate::oracle::resolved_to_rustc_ty` directly at
each call site.

---

## 14. `struct_names.clone()` in Parser (4 clones)

### Problem

`parser.rs` clones `self.struct_names` 4 times during parsing to work around
borrow checker constraints. O(n²) copies for n structs.

### Fix — acceptable for POC

The clone count is bounded by the number of top-level definitions, which is
small. Could refactor with indices or split borrows if performance matters.

---

## Resolved Items (sessions 1–5)

| # | Item | Session |
|---|------|---------|
| — | String-based type resolution | 5 |
| — | Vec-specific code | 4 |
| — | println hardcoding | 5 |
| — | Ref param redundant conversion | 5 |
| — | `__toylang_main` duplication | 5 |
| — | Method signature heuristic | 5 |
| — | `rust_method_ret` closures | 5 |
| — | Duplicated `resolved_type_to_rustc_ty` | 5 |
| — | Dummy Vec constructions | 5 |
