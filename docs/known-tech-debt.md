# Known Technical Debt

> Last updated: session 3 (53 tests passing, 0 ignored)

---

## 1. String-Based Type Resolution Duplicated in 6+ Places

### Problem

Converting type name strings to LLVM types or rustc types is implemented
independently in multiple functions, each handling a slightly different subset of
types. Adding a new primitive type (e.g. `u32`, `i16`) requires changes in all of
them. The functions are:

### Locations

**`toylangc/src/llvm_gen.rs`:**

- **`resolved_to_inkwell()`** — converts `ResolvedType` → inkwell `BasicTypeEnum`.
  Handles all resolved types including Struct, Vec (opaque), Ref.

- **`resolve_rust_ty_from_string()`** — converts type name string → rustc `Ty<'tcx>`.
  Handles primitives, local structs, generic `Vec<T>` (with Global allocator lookup).
  Does NOT handle references or pointers.

- **`codegen_internal_function()` param loop** — converts param type string → inkwell
  `BasicMetadataTypeEnum`. Matches: `"i32"`, `"i64"`, `"f64"`, `"bool"`, `"usize"`,
  `"&Vec<..."`, `"&..."`, struct names. This is the only place that handles `&Vec<`
  specially (as pointer type).

- **`parse_coerced_type()`** — converts ABI coercion strings (from `abi_helpers.rs`)
  → inkwell `BasicTypeEnum`. Handles: `"i{N}"`, `"[N x T]"`, `"{ T1, T2 }"`.
  Completely different format from type name strings.

- **`parse_struct_type_str()`** — converts LLVM struct type strings → inkwell
  `StructType`. Handles: `"i32"`, `"i64"`, `"double"`, `"i1"`, `"[N x i8]"`,
  nested `{ ... }`. Only used for ABI coercion struct types.

**`toylangc/src/toylang/type_resolve.rs`:**

- **`parse_type_string()`** — converts type name string → `ResolvedType`. Handles:
  references, pointers, primitives, `Vec<T>`, generic structs, non-generic structs.
  This is the most complete implementation.

- **`resolve_field_type()`** — converts `ToyFieldType` → `ResolvedType`. Handles:
  I32/I64/F64/Bool, TypeParam substitution, ToyStruct, RustGeneric (Vec only).

**`toylangc/src/toylang/callbacks_impl.rs`:**

- **`string_to_rustc_ty()`** — converts type name string → rustc `Ty<'tcx>`.
  Handles: `"i32"`, `"i64"`, `"f64"`, `"bool"`, `"usize"`, local structs.
  Does NOT handle Vec, generics, or references.

- **`resolve_rust_generic_ty()`** — converts generic Rust type name → rustc `Ty<'tcx>`.
  Special-cases Vec (with hidden Global allocator type param). Falls back to local
  struct lookup.

### Impact

Adding a new primitive type requires touching 4-6 functions. Adding a new generic
type (HashMap, BTreeMap) requires touching the Vec special cases in at least 3
files. The functions handle different subsets of types, so bugs can hide where one
function handles a case but another doesn't.

### Fix Options

**Option A: Centralize on `ResolvedType`.**
Make `parse_type_string()` the single source of truth for string → type conversion.
All other functions convert `ResolvedType` to their target representation (inkwell
type, rustc Ty). The param loop in `codegen_internal_function()` would call
`parse_type_string()` then `resolved_to_inkwell()` instead of doing its own string
matching.

**Option B: Build a `TypeRegistry` that caches all conversions.**
A struct that holds `ResolvedType → inkwell type` and `ResolvedType → rustc Ty`
mappings, populated once at the start of codegen. All lookup sites query the
registry instead of doing their own conversion.

**Recommendation:** Option A is simpler and doesn't require a new data structure.
The param type string matching in `codegen_internal_function` is the most
impactful place to fix — it's the last remaining string-based type switch for
params (the extern wrapper already uses ABI-derived types).

---

## 2. Vec-Specific Code (~200 Lines)

### Problem

Vec is the only Rust generic type toylang can use. All Vec support is hardcoded
by name — `"Vec"`, `"new"`, `"push"`, `"len"` — scattered across 4 files and
~200 lines. Supporting a second Rust type (HashMap, String, etc.) would require
duplicating all of this.

### Locations

**Detection (is Vec used?):**
- `llvm_gen.rs`: `body_uses_vec()`, `stmt_uses_vec()`, `expr_uses_vec()` —
  check if body contains `StaticCall { ty == "Vec" }` or `MethodCall { method == "push"|"len" }`

**Element type discovery (what's in the Vec?):**
- `llvm_gen.rs`: `find_vec_elem_from_explicit_ast()` — scans for `Vec::new<T>()` type args
- `llvm_gen.rs`: `find_vec_elem_name()` — extracts element from return type string `"Vec<Point>"`
- `llvm_gen.rs`: `find_vec_elem_from_params()` — scans params for `"Vec<T>"` pattern
- `callbacks_impl.rs`: `find_vec_elem_from_explicit_type_args()` — same as llvm_gen version but returns rustc `Ty`
- `callbacks_impl.rs`: `find_vec_elem_ty()` — extracts from function signature
- `callbacks_impl.rs`: `find_vec_in_fields_recursive()` — searches struct fields for Vec

**Vec operation scanning:**
- `callbacks_impl.rs`: `scan_body_vec_ops()`, `scan_expr_vec_ops()` — flag which
  Vec methods (new/push/len) are used in the body

**Symbol resolution:**
- `llvm_gen.rs`: `resolve_vec_symbols()` — queries oracle for Vec::new/push/len
  DefIds, resolves mangled symbols, declares LLVM functions, populates `vec_fns` map
- `oracle.rs`: `find_vec_method()` — iterates inherent impls of Vec to find method by name
- `oracle.rs`: `extract_global_ty()` — extracts Vec's hidden Global allocator type

**Codegen:**
- `llvm_gen.rs`: `StaticCall` arm matches `("Vec", "new")` — allocs opaque type, calls sret
- `llvm_gen.rs`: `MethodCall` arm matches `"push"` — builds push call with arg pointer
- `llvm_gen.rs`: `MethodCall` arm matches `"len"` — builds len call, returns usize

**Type resolution:**
- `type_resolve.rs`: `parse_type_string()` — `if base == "Vec"` special case
- `type_resolve.rs`: `resolve_field_type()` — `"Vec"` match in RustGeneric arm
- `type_resolve.rs`: `StaticCall` arm — `("Vec", "new")` with explicit type args
- `type_resolve.rs`: `MethodCall` arm — `"push"` and `"len"` with hardcoded return types

**Dependency discovery:**
- `callbacks_impl.rs`: `collect_rust_deps()` — assembles Vec method dependencies
  with correct generic args (elem type + Global allocator)
- `callbacks_impl.rs`: `resolve_rust_generic_ty()` — `"Vec"` special case for
  hidden allocator type param

### Impact

Adding Vec::pop, Vec::get, or Vec::insert requires changes in codegen (new match
arm), type resolver (new method arm), callbacks (new dependency), and possibly
scan functions. Adding HashMap would require duplicating the entire pipeline.

### Fix Options (Part 10.4 Roadmap)

The architecture guide documents a 4-move roadmap:

**Move 1 (done):** Opaque Rust types via `layout_of`.

**Move 2: General inherent method resolution.** Replace `find_vec_method` +
`resolve_vec_symbols` with `find_inherent_method(tcx, adt_def_id, method_name)`.
Use `fn_abi_of_instance` for calling conventions. On-demand resolution at call
sites instead of upfront scanning.

**Move 3: Merge dep discovery.** Merge `collect_rust_deps` (untyped AST scan for
Vec ops) into `collect_toylang_fn_deps` (typed AST walk). One walk, all deps.
Eliminates `scan_body_vec_ops`, `body_uses_vec`, and the multi-tier element type
discovery chain.

**Move 4: General method codegen.** Replace hardcoded `push`/`len`/`new` match
arms with a general "call Rust method" path using ABI info from `fn_abi_of_instance`.

**Recommendation:** Start with Move 2, which unblocks non-Vec Rust types. Moves 3
and 4 can follow incrementally.

---

## 3. println Hardcoding

### Problem

`println` is the only built-in function. It's special-cased by name string in two
places with no abstraction — adding a second built-in (print, eprintln, assert)
would require duplicating the pattern.

### Locations

**Type resolver** (`type_resolve.rs`):
```rust
Expr::FnCall { name, type_args: _, args } if name == "println" => {
    // resolves args with Void expected, returns Void
}
```
This guard must appear BEFORE the general `Expr::FnCall` arm to prevent the
resolver from looking up "println" in the registry (where it doesn't exist).

**Codegen** (`llvm_gen.rs`):
```rust
TypedExprKind::FnCall { name, args, .. } if name == "println" => {
    // Builds printf format string from first arg (must be StringLit)
    // Replaces {} with type-specific printf specifiers (%d, %ld, %zu, %f)
    // Appends \n, declares printf, builds call
}
```
This is ~50 lines of format string manipulation and printf interop.

**Dep discovery** (`callbacks_impl.rs`):
`println` is implicitly skipped — `collect_toylang_fn_deps` looks up callee names
in `registry.functions`, and "println" isn't there, so it's silently ignored.
This works but is accidental, not intentional.

### Impact

Fine for one built-in. Adding more built-ins (print without newline, eprintln,
assert, debug formatting) would require:
- Another guarded match arm in the type resolver for each
- Another ~50 line codegen block for each
- Continued reliance on the "not in registry" skip in dep discovery

### Fix Options

**Option A: Keep as-is.** println is the only planned built-in for now. The
hardcoding is simple and readable. Revisit when a second built-in is needed.

**Option B: Built-in function registry.** Create a `BuiltinFn` enum or trait
with methods for type resolution and codegen. Register built-ins at startup.
The type resolver and codegen check built-ins before the registry lookup.
Overkill for one function but clean for N functions.

**Option C: Built-in as a codegen-only concept.** Keep the type resolver guard
(`name == "println"` → resolve as Void), but extract the codegen into a function
`codegen_builtin_println(ctx, args) -> ExprResult`. This localizes the printf
logic without adding registry infrastructure.

**Recommendation:** Option A for now. Option C when a second built-in is added.

---

## 4. Ref Param Redundant Conversion in Extern Wrapper

### Problem

When the extern wrapper calls the internal function, reference params (`&Vec<T>`,
`&T`) get an unnecessary alloca/store/load bitcast. This happens because
`fn_abi_of_instance` reports pointer params as `PassMode::Direct` with size 64
bits (on aarch64), producing `CoercedParam::Direct("i64")`. The internal function
expects `ptr`. Since `i64 != ptr` in LLVM type comparison, the conversion fires:

```llvm
; Generated (unnecessary):
%param_coerce = alloca i64
store i64 %0, ptr %param_coerce
%converted = load ptr, ptr %param_coerce

; Optimal:
; just pass %0 directly (same bits)
```

LLVM's mem2reg pass eliminates this, so there's no runtime cost. But it adds
unnecessary IR and obscures the generated code.

### Location

`llvm_gen.rs`, `codegen_extern_wrapper()`, in the param forwarding loop:
```rust
CoercedParam::Direct(ty_str) => {
    let rust_abi_ty = parse_coerced_type(ctx, ty_str);
    let internal_ty = internal_param_types[i];
    if rust_abi_ty == internal_ty {
        call_args.push(param.into());  // same type, pass through
    } else {
        // Type mismatch — bitcast via memory
        let alloca = ctx.builder.build_alloca(rust_abi_ty, "param_coerce").unwrap();
        ctx.builder.build_store(alloca, param).unwrap();
        let converted = ctx.builder.build_load(internal_ty, alloca, "converted").unwrap();
        call_args.push(converted.into());
    }
}
```

The `else` branch fires for every pointer param because `i64 != ptr`.

### Empirical data

From `fn_abi_of_instance` on aarch64:
- `&Vec<Point>` → `PassMode::Direct`, size=64, `abi=Scalar(Pointer(AddressSpace(0)))`
- The `Scalar(Pointer)` info is available in `arg.layout.backend_repr` but we
  don't check it — we just emit `"i64"` from the size.

### Fix Options

**Option A: Detect `Scalar(Pointer)` in `coerced_param_types_for_instance`.**
Check `arg.layout.backend_repr` for `Scalar(Initialized { value: Pointer(...) })`.
If it's a pointer, return a new `CoercedParam::Pointer` variant (or just format
the type string as `"ptr"` instead of `"i64"`). The wrapper would then get `ptr`
as the Rust ABI type, which matches the internal `ptr`, and no conversion fires.

**Option B: Check if both types have the same bit width before converting.**
Instead of `rust_abi_ty == internal_ty`, check if they're the same size. If both
are 64 bits, skip the conversion. This is less precise but catches the pointer
case.

**Option C: Leave it.** LLVM eliminates the redundant alloca. The IR is slightly
ugly but functionally correct. Fix when it matters for debugging or IR readability.

**Recommendation:** Option A is clean and precise. Option C is fine for now.

---

## 5. Duplicated `__toylang_main` Mapping

### Problem

When toylang defines `fn main()`, the stub wrapper is renamed to `__toylang_main`
to avoid conflicting with Rust's `main`. This mapping is hardcoded in 4 separate
locations with no single source of truth. Adding a second renamed function or
changing the naming convention requires updating all 4.

### Locations

**`callbacks_impl.rs`, `fn_names()` method:**
```rust
if names.contains("main") {
    names.insert("__toylang_main".to_string());
}
```

**`callbacks_impl.rs`, `monomorphize_fn()` method:**
```rust
let registry_name = if name == "__toylang_main" { "main" } else { name };
```

**`llvm_gen.rs`, `generate_with_tcx()` function:**
```rust
let registry_name = if name == "__toylang_main" { "main".to_string() } else { name.clone() };
```

**`stub_gen.rs`, wrapper generation:**
```rust
let wrapper_name = if _name == "main" { "__toylang_main" } else { _name.as_str() };
```

### Impact

Low — there's only one renamed function and the pattern is unlikely to change.
But it's a maintainability smell: if someone changes one location and misses
another, the compiler silently breaks for toylang programs with `fn main()`.

### Fix Options

**Option A: Centralize the mapping.** Add a function (e.g. in the registry module)
like `fn wrapper_name(registry_name: &str) -> String` and
`fn registry_name(wrapper_name: &str) -> String`. All 4 locations call these
instead of hardcoding the string comparison.

**Option B: Store the mapping in the registry.** When `fn main()` is parsed, the
registry records `main → __toylang_main` in a `name_overrides: HashMap<String, String>`.
All lookups consult this map.

**Option C: Leave it.** It's 4 lines of trivial code. The risk of divergence is
low since all 4 are in the same codebase and tested by the same integration tests.

**Recommendation:** Option A is a 10-minute cleanup. Option C is fine if there's
no plan to add more renamed functions.
