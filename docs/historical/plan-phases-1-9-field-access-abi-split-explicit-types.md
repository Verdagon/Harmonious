# Plan: Phases 1–9 — Field Access, println, Toylang Main, ABI Split, Explicit Type Args

## Status

**All phases complete.** 53 tests pass, 0 failed, 0 ignored. Zero warnings.

Phases completed:
- Phase 1: Field access (`p.x`) — ✅ done
- Phase 2: println built-in — ✅ done
- Phase 3: Toylang-owned main — ✅ done (with extra fixes for `__toylang_main` mapping)
- Phase 4: Internal/extern ABI split — ✅ done
- Phase 5: Bool return ABI test — ✅ done (passed without code fix)
- Phase 6: Struct param ABI tests — ✅ done (passed without code fix)
- Phase 7: Param ABI coercion from fn_abi — ✅ done (fixed `test_generic_callee_with_struct`)
- Phase 8: Forward type inference from args — ✅ done, then reverted (see Phase 9)
- Phase 9: Remove all inference, require explicit type args — ✅ done

Starting state: 40 tests, 5 ignored. Final state: 53 tests, 0 ignored.

## Original Context

Phase 0 was complete (40 tests pass, zero warnings). 5 tests were ignored; 3 were
blocked on missing features: direct field access (`p.x`), a `println` built-in,
and toylang-owned main. The other 2 (`test_generic_callee_in_let`,
`test_generic_callee_with_struct`) had inference/ABI blockers.

---

## Phase 1: Field Access (`p.x`)

### 1a. AST — add FieldAccess variant

**File:** `toylangc/src/toylang/ast.rs` (line 13, Expr enum)

Add after `MethodCall`:
```rust
FieldAccess { receiver: Box<Expr>, field: String },
```

### 1b. Typed AST — add FieldAccess variant

**File:** `toylangc/src/toylang/typed_ast.rs` (line 38, TypedExprKind enum)

Add after `MethodCall`:
```rust
FieldAccess { receiver: Box<TypedExpr>, field: String },
```

### 1c. Parser — disambiguate field access vs method call

**File:** `toylangc/src/toylang/parser.rs`, `parse_postfix` (line 370)

Currently the loop after consuming `.` and the identifier unconditionally expects
`(`. Change to peek for `(` — if present, it's a method call; otherwise field access:

```rust
fn parse_postfix(&mut self) -> Result<Expr, String> {
    let mut expr = self.parse_primary()?;
    loop {
        if self.peek() == &Token::Dot {
            self.consume();
            let ident = self.expect_ident()?;
            if self.peek() == &Token::LParen {
                // Method call: expr.method(args)
                self.consume(); // consume '('
                let args = self.parse_args()?;
                self.expect(Token::RParen)?;
                expr = Expr::MethodCall {
                    receiver: Box::new(expr),
                    method: ident,
                    args,
                };
            } else {
                // Field access: expr.field
                expr = Expr::FieldAccess {
                    receiver: Box::new(expr),
                    field: ident,
                };
            }
        } else {
            break;
        }
    }
    Ok(expr)
}
```

### 1d. Type resolver — resolve FieldAccess

**File:** `toylangc/src/toylang/type_resolve.rs`, `resolve_expr` (after MethodCall arm, ~line 534)

Add new match arm:
```rust
Expr::FieldAccess { receiver, field } => {
    let typed_recv = resolve_expr(receiver, &ResolvedType::Void, scope, registry, vec_inferences);
    let ResolvedType::Struct { name: struct_name, field_types, .. } = &typed_recv.ty else {
        panic!("field access on non-struct type: {:?}", typed_recv.ty);
    };
    let toy_struct = registry.structs.get(struct_name.as_str())
        .expect("struct not found in registry");
    let field_idx = toy_struct.fields.iter()
        .position(|f| f.name == *field)
        .unwrap_or_else(|| panic!("field '{}' not found in '{}'", field, struct_name));
    let field_ty = field_types[field_idx].clone();
    TypedExpr {
        kind: TypedExprKind::FieldAccess {
            receiver: Box::new(typed_recv),
            field: field.clone(),
        },
        ty: field_ty,
    }
}
```

Key: use `field_types[field_idx]` from the already-resolved receiver type. This
correctly handles generic structs (e.g., `Pair<i64, i64>` — field_types already
has [I64, I64] from the type resolution pass). Don't re-resolve from the registry
definition.

### 1e. LLVM codegen — lower FieldAccess

**File:** `toylangc/src/llvm_gen.rs`, `lower_typed_expr` (after MethodCall arm, ~line 948)

Add new match arm:
```rust
TypedExprKind::FieldAccess { receiver, field } => {
    let recv_result = lower_typed_expr(ctx, receiver);
    let ResolvedType::Struct { name: struct_name, .. } = &receiver.ty else {
        panic!("field access on non-struct");
    };
    let toy_struct = ctx.registry.structs.get(struct_name.as_str()).unwrap();
    let field_idx = toy_struct.fields.iter()
        .position(|f| f.name == *field)
        .unwrap() as u32;
    let struct_ty = ctx.resolved_to_struct_type(&receiver.ty);
    let struct_ptr = recv_result.into_ptr(&ctx.builder,
        struct_ty.as_basic_type_enum(), "fa_recv");
    let gep = ctx.builder.build_struct_gep(struct_ty, struct_ptr, field_idx, field).unwrap();
    match &expr.ty {
        ResolvedType::Struct { .. } | ResolvedType::Vec { .. } => {
            // Complex types — return pointer to the field
            ExprResult::Ptr(gep, ctx.resolved_to_inkwell(&expr.ty))
        }
        _ => {
            // Primitives — load the value
            let val = ctx.builder.build_load(
                ctx.resolved_to_inkwell(&expr.ty), gep, field).unwrap();
            ExprResult::Value(val)
        }
    }
}
```

This follows the same GEP pattern as StructLit (line 799) and accessor codegen
(line 1282). Primitives are loaded; complex types return a pointer.

### 1f. Dep discovery — add FieldAccess to walk functions

**File:** `toylangc/src/toylang/callbacks_impl.rs`

Update `walk_typed_expr_for_fn_calls` (~line 445) to recurse into FieldAccess:
```rust
TypedExprKind::FieldAccess { receiver, .. } => {
    walk_typed_expr_for_fn_calls(receiver, calls);
}
```

Without this, any FnCall nested inside a FieldAccess expression would be missed
during dependency discovery.

### 1g. Scan functions — add FieldAccess to Vec op scanning

**File:** `toylangc/src/toylang/callbacks_impl.rs`

Update `scan_expr_vec_ops` (~line 237) to recurse into FieldAccess:
```rust
Expr::FieldAccess { receiver, .. } => {
    scan_expr_vec_ops(receiver, new, push, len);
}
```

---

## Phase 2: println Built-in

### 2a. Lexer — add string literal tokenization

**File:** `toylangc/src/toylang/parser.rs`, Token enum (line 11)

Add variant:
```rust
StringLit(String),
```

**File:** `toylangc/src/toylang/parser.rs`, `tokenize` (line 36)

Add string literal handling before the single-char match block (~line 86).
Insert after the digit sequence block (line 84):
```rust
// String literals
if chars[i] == '"' {
    i += 1; // skip opening quote
    let start = i;
    while i < chars.len() && chars[i] != '"' {
        i += 1;
    }
    let s: String = chars[start..i].iter().collect();
    if i < chars.len() { i += 1; } // skip closing quote
    tokens.push(Token::StringLit(s));
    continue;
}
```

No escape sequences needed for now — the tests only use simple strings like
`"Point: {} {}"` and `"Vec length: {}"`. Panic on unterminated strings (missing
closing `"`).

### 2b. AST — add StringLit variant

**File:** `toylangc/src/toylang/ast.rs` (Expr enum)

Add:
```rust
StringLit(String),
```

### 2c. Parser — parse string literals and println

**File:** `toylangc/src/toylang/parser.rs`, `parse_primary` (~line 394)

Add case before the `Token::Ident` match:
```rust
Token::StringLit(s) => {
    let s = s.clone();
    self.consume();
    Ok(Expr::StringLit(s))
}
```

println is parsed as a normal FnCall — `println("fmt", arg1, arg2)` parses as
`Expr::FnCall { name: "println", args: [StringLit("fmt"), arg1, arg2] }`.
No special parser handling needed.

### 2d. Typed AST — add StringLit and Str type

**File:** `toylangc/src/toylang/typed_ast.rs`

Add to `ResolvedType` enum:
```rust
Str,
```

Add to `TypedExprKind` enum:
```rust
StringLit(String),
```

### 2e. Type resolver — resolve StringLit and println

**File:** `toylangc/src/toylang/type_resolve.rs`

Add StringLit case in `resolve_expr`:
```rust
Expr::StringLit(s) => TypedExpr {
    kind: TypedExprKind::StringLit(s.clone()),
    ty: ResolvedType::Str,
},
```

Add a new match arm for println with a guard, before the general `Expr::FnCall`
arm (~line 406). The guard ensures only println hits this path; all other FnCalls
fall through to the general arm:
```rust
Expr::FnCall { name, args } if name == "println" => {
    let typed_args: Vec<TypedExpr> = args.iter()
        .map(|a| resolve_expr(a, &ResolvedType::Void, scope, registry, vec_inferences))
        .collect();
    TypedExpr {
        kind: TypedExprKind::FnCall {
            name: "println".into(),
            type_args: vec![],
            args: typed_args,
        },
        ty: ResolvedType::Void,
    }
}
```

### 2f. LLVM codegen — resolved_to_inkwell for Str

**File:** `toylangc/src/llvm_gen.rs`, `resolved_to_inkwell` (~line 94)

Add case:
```rust
ResolvedType::Str => self.context.ptr_type(AddressSpace::default()).into(),
```

### 2g. LLVM codegen — resolved_type_to_rustc_ty for Str

**File:** `toylangc/src/llvm_gen.rs`, `resolved_type_to_rustc_ty` (~line 176)

Add case:
```rust
ResolvedType::Str => panic!("Str type should not need rustc Ty conversion"),
```

Str is only used for format string literals in println — never passed to rustc.

### 2h. LLVM codegen — lower StringLit

**File:** `toylangc/src/llvm_gen.rs`, `lower_typed_expr`

Add case:
```rust
TypedExprKind::StringLit(s) => {
    let ptr = ctx.builder.build_global_string_ptr(s, "str_lit")
        .unwrap()
        .as_pointer_value();
    ExprResult::Value(ptr.into())
}
```

### 2i. LLVM codegen — lower println FnCall

**File:** `toylangc/src/llvm_gen.rs`, `lower_typed_expr`, at top of FnCall arm (~line 822)

Add special case before the general FnCall handling:
```rust
TypedExprKind::FnCall { name, args, .. } if name == "println" => {
    // Build printf format string from the first arg (must be StringLit)
    let TypedExprKind::StringLit(fmt) = &args[0].kind else {
        panic!("println first arg must be string literal");
    };

    // Replace {} with type-appropriate printf specifiers
    let mut printf_fmt = String::new();
    let mut arg_idx = 1usize;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'}') {
            chars.next(); // consume '}'
            let spec = match &args[arg_idx].ty {
                ResolvedType::I32 | ResolvedType::Bool => "%d",
                ResolvedType::I64 => "%ld",
                ResolvedType::Usize => "%zu",
                ResolvedType::F64 => "%f",
                other => panic!("println: unsupported type {:?} for format arg", other),
            };
            printf_fmt.push_str(spec);
            arg_idx += 1;
        } else {
            printf_fmt.push(c);
        }
    }
    printf_fmt.push('\n');

    // Create global string constant for format
    let fmt_ptr = ctx.builder.build_global_string_ptr(&printf_fmt, "println_fmt")
        .unwrap()
        .as_pointer_value();

    // Declare printf (variadic, C linkage)
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let printf_ty = ctx.context.i32_type().fn_type(&[ptr_ty.into()], true);
    let printf_fn = ctx.module.get_function("printf").unwrap_or_else(|| {
        ctx.module.add_function("printf", printf_ty, Some(inkwell::module::Linkage::External))
    });

    // Build call args: format string + each value arg
    let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = vec![fmt_ptr.into()];
    for arg_expr in &args[1..] {
        let val = lower_typed_expr(ctx, arg_expr).into_value(&ctx.builder);
        // Coerce bool (i1) to i32 for printf
        let coerced = if arg_expr.ty == ResolvedType::Bool {
            ctx.builder.build_int_z_extend(
                val.into_int_value(),
                ctx.context.i32_type(), "bool_to_i32").unwrap().into()
        } else {
            val
        };
        call_args.push(coerced.into());
    }

    ctx.builder.build_call(printf_fn, &call_args, "printf_ret").unwrap();
    return ExprResult::Value(ctx.context.i32_type().const_zero().into());
}
```

### 2j. Dep discovery — add FieldAccess and StringLit to walk

**File:** `toylangc/src/toylang/callbacks_impl.rs`, `walk_typed_expr_for_fn_calls`

Add case for StringLit (leaf node, no children):
```rust
TypedExprKind::StringLit(_) => {}
```

This is already handled by the `_ => {}` fallthrough, but explicit is better
if the compiler warns about non-exhaustive matches.

---

## Phase 3: Toylang-Owned Main

The three ignored tests have toylang source with `fn main() { ... }` and Rust
source with `fn main() { unreachable!() }`. The stub wrapper for a toylang
function named `main` would conflict with Rust's `main`.

**Approach:** Rename the stub wrapper to `__toylang_main`. Rust's main calls it
directly. This uses the existing stub/MonoItems/per_instance_mir pipeline — no
extern "C" hacks or force-monomorphization tricks. The test Rust source becomes
`fn main() { __toylang_main(); }`, same pattern as the working v2 tests.

### 3a. stub_gen.rs — rename main wrapper

**File:** `toylangc/src/stub_gen.rs` (~line 100, function stub generation)

When generating the wrapper function, if the function name is `main`, use
`__toylang_main` as the wrapper name:

```rust
let wrapper_name = if _name == "main" { "__toylang_main" } else { _name };
let wrapper_ident = format_ident!("{}", wrapper_name);
```

The extern declaration uses `__toylang_impl_main` (normal symbol), unchanged.

### 3b. codegen_function — handle void return

**File:** `toylangc/src/llvm_gen.rs`, `codegen_function` (~line 553)

Line 559: `let ret_ty_name = func.return_ty.as_ref().unwrap();` panics for
`fn main()` which has no return type. Change to:
```rust
let ret_ty_name_owned;
let ret_ty_name: &str = match func.return_ty.as_deref() {
    Some(s) => { ret_ty_name_owned = s.to_string(); &ret_ty_name_owned }
    None => "void",
};
```

Trace through the downstream uses of `ret_ty_name` in `codegen_function`:

1. **`ret_complex_ty`** (~line 570): checks `starts_with("Vec<")` and
   `struct_type_for_ret`. "void" matches neither → `None`. Correct.

2. **`coerced_return_type_for_instance`** (~line 567): queries ABI. A void
   function returns `CoercedReturn::Void`. `is_sret` = false. Correct.

3. **`llvm_ret_type`** (~line 624): `is_sret` is false, "void" doesn't match
   any primitive or Vec check → falls to struct branch → `coerced` is Void →
   we need a void case. Add before the primitive check:
   ```rust
   } else if ret_ty_name == "void" {
       None
   ```

4. **Return path** (~line 686): `typed_body.ret` is None for a void function →
   falls through to the `else { build_return(None) }` at line 725. Correct.

The type resolver already handles `return_ty: None` → `ResolvedType::Void`
(type_resolve.rs line 22-24), so no change needed there.

### 3c. Update ignored tests

**File:** `toylangc/tests/integration_tests.rs`

Update the three ignored tests' Rust source from:
```rust
mod __lang_stubs;
fn main() { unreachable!() }
```
to:
```rust
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
```

Remove `#[ignore]` from all three tests.

### 3d. Type resolver — already handles void

`resolve_fn_body` (type_resolve.rs:22-24) already does:
```rust
let ret_ty = func.return_ty.as_deref()
    .map(|s| parse_type_string(s, registry))
    .unwrap_or(ResolvedType::Void);
```
No change needed.

### 2k. Verify dep discovery handles println and new nodes

No code changes expected — verify these at test time:

- `collect_toylang_fn_deps` skips `println` because it's not in the registry
  (line ~410: `if !registry.functions.contains_key(callee_name) { continue; }`).
- `collect_rust_deps` / `scan_expr_vec_ops` ignores `FnCall` and `StringLit`
  (only matches `StaticCall` and `MethodCall`). If the compiler warns about
  non-exhaustive patterns on `Expr::StringLit` or `Expr::FieldAccess`, add
  explicit `_ => {}` or named arms to `scan_expr_vec_ops`.

---

## Critical Files

| File | Phase | Change |
|------|-------|--------|
| `toylangc/src/toylang/ast.rs` | 1, 2 | Add `FieldAccess`, `StringLit` to Expr |
| `toylangc/src/toylang/typed_ast.rs` | 1, 2 | Add `FieldAccess`, `StringLit` to TypedExprKind; `Str` to ResolvedType |
| `toylangc/src/toylang/parser.rs` | 1, 2 | Disambiguate field vs method in `parse_postfix`; string literal lexing; `StringLit` in `parse_primary` |
| `toylangc/src/toylang/type_resolve.rs` | 1, 2 | `FieldAccess` resolution; `StringLit` resolution; `println` special case in FnCall |
| `toylangc/src/llvm_gen.rs` | 1, 2, 3 | `FieldAccess` GEP+load; `StringLit` global string; `println→printf`; `Str` in `resolved_to_inkwell`; void return handling |
| `toylangc/src/toylang/callbacks_impl.rs` | 1 | `FieldAccess` in walk and scan functions |
| `toylangc/src/stub_gen.rs` | 3 | Rename `main` wrapper to `__toylang_main` |
| `toylangc/tests/integration_tests.rs` | 3 | Remove `#[ignore]`, update Rust source for 3 tests |

## Verification

After each phase, run both test suites:
```bash
cargo +rustc-fork test -p toylangc --test integration_tests
cargo +rustc-fork test -p toylangc --bin toylangc -- type_resolve
```

After Phase 1 (field access): all 40 existing tests still pass. No new tests yet —
field access isn't exercised until the Phase 3 tests are enabled.

After Phase 2 (println): same — println isn't exercised until Phase 3 tests.

After Phase 1: 41 passed (40 + `test_field_access_returns_value`).

After Phase 2: 41 passed (println not exercised until Phase 3).

After Phase 3 (toylang main): 44 passed, 0 failed, 2 ignored.

After Phase 4 (ABI split): 44 passed, behavior-preserving refactor.

After Phase 5 (bool test): 45 passed (44 + `test_bool_return`).

### Phase 3 — Unanticipated fixes

The plan's steps 3a-3c were correct, but Phase 3 surfaced 5 additional issues:

1. **`fn_names()` didn't include `__toylang_main`** — the facade's `is_consumer_fn`
   check uses the Rust item name, but fn_names had only `"main"`. Fix: add
   `"__toylang_main"` to fn_names when `"main"` is present.

2. **`generate_with_tcx` couldn't find `main` in registry** — the Rust item name
   is `__toylang_main` but registry key is `"main"`. Fix: map
   `__toylang_main` → `"main"` in registry lookup, monomorphize_fn, and
   compute_fn_symbol.

3. **Vec symbols not resolved for void functions** — `collect_rust_deps` and
   `find_vec_elem_name` only inspected return type & params. Fix: added
   `find_vec_elem_from_body` that scans push args for struct literal names.

4. **`scan_expr_vec_ops` missed Vec ops inside FnCall args** — e.g.
   `println("...", v.len())`. Fix: added `Expr::FnCall` arm to recurse into args.

5. **LLVM symbol deduplication** — when callee is codegenned after caller, the
   caller's declaration has the wrong ABI type. `module.add_function` appends
   `.1` suffix. Temporarily fixed with topological sort; properly fixed in Phase 4.

### Phase 5 — Outcome

Bool return ABI (i1 vs i8) was already handled by `coerce_int_to_type` in the
extern wrapper's direct return path. No code fix needed, just the test.

---

## Phase 4: Internal/Extern ABI Split

### Context

Phase 3 revealed that toylang-to-toylang function calls have an ABI mismatch:
`codegen_function` generates functions with Rust's coerced ABI (e.g. `{ i32 }` for
a single-field struct), but the FnCall arm declares the callee with the naive
resolved type (e.g. `i32`). This was worked around with a fragile topological sort
that codegens callees before callers.

The real fix: separate internal calling convention from Rust-facing ABI.

### Design

Each toylang function gets **two** LLVM functions:

1. **Internal** (`__toylang_internal_{name}`) — simple, predictable ABI:
   - Primitives (i32, i64, f64, bool, usize): returned directly
   - Void: void return
   - Structs/Vec: always sret (ptr first param, void return)

2. **Extern wrapper** (`__toylang_impl_{name}`) — thin wrapper matching Rust ABI:
   - Calls the internal function
   - Adapts the return value to match `coerced_return_type_for_instance`

Toylang-to-toylang `FnCall` uses `__toylang_internal_` symbols directly. Rust
calls the `__toylang_impl_` wrapper (unchanged from today).

### 4a. Rename `codegen_function` → `codegen_internal_function`

**File:** `toylangc/src/llvm_gen.rs`

- Change symbol from `__toylang_impl_{name}` to `__toylang_internal_{name}`
- Remove `instance` parameter — no longer needed for ABI queries
- Remove `coerced_return_type_for_instance` call
- Simplify return type logic:
  - `Struct`/`Vec` → always sret (void return, sret pointer first param)
  - Primitives → return directly via `resolved_to_inkwell`
  - Void → void return
- Simplify return codegen: sret case does memcpy + `ret void`, primitive does
  `ret <value>`, no coercion logic
- Keep Vec symbol resolution (`resolve_vec_symbols`) here since the body is lowered here

### 4b. Change FnCall arm to use internal ABI

**File:** `toylangc/src/llvm_gen.rs`, `lower_typed_expr`, FnCall arm

Change symbol prefix from `__toylang_impl_` to `__toylang_internal_`.

The internal ABI is predictable from `ResolvedType`, so the caller can always
construct the correct declaration without seeing the callee's definition:

- If callee return type is `Struct`/`Vec`:
  - Declare callee with sret pointer first param, void return
  - Allocate alloca for result
  - Call with sret ptr + args
  - Return `ExprResult::Ptr(alloca, type)`
- If primitive:
  - Declare callee with direct return type from `resolved_to_inkwell`
  - Call normally
  - Return `ExprResult::Value`
- If void:
  - Declare with void return
  - Call
  - Return dummy value

### 4c. Add `codegen_extern_wrapper`

**File:** `toylangc/src/llvm_gen.rs`

New function that generates the Rust ABI wrapper. Takes `instance` (for
`coerced_return_type_for_instance`), the extern symbol, and the internal symbol.

Logic:
1. Query `coerced_return_type_for_instance(tcx, instance)` for Rust ABI return
2. Build extern function signature matching Rust ABI (same as current
   `codegen_function` signature logic)
3. Call the internal function:
   - If internal uses sret (struct/vec return):
     - If Rust ABI is `Indirect` (also sret): pass Rust's sret pointer directly
       to the internal function — no extra alloca
     - If Rust ABI is `Direct(coerced)`: alloca tmp, call internal with sret ptr,
       load from alloca as coerced type, `ret` it
   - If primitive return: call internal, return result (with `coerce_int_to_type`
     if Rust ABI expects a different width)
   - If void: call internal, `ret void`

### 4d. Update `generate_with_tcx`

**File:** `toylangc/src/llvm_gen.rs`

- Remove the topological sort and `has_toylang_fn_calls` helper
- Remove `walk_typed_body_for_fn_calls` public export (make private again in
  `callbacks_impl.rs`)
- Two passes over `fn_items`:
  1. Call `codegen_internal_function` for each
  2. Call `codegen_extern_wrapper` for each
- Compute internal symbol by replacing `__toylang_impl_` with `__toylang_internal_`
  in the extern symbol string

### 4e. What stays the same

- `callbacks_impl.rs` `monomorphize_fn` still returns `__toylang_impl_` as extern_symbol
- `symbol_name.rs` still maps Rust stubs to `__toylang_impl_`
- `stub_gen.rs` still declares `extern "C" { fn __toylang_impl_... }`
- Accessor functions (`codegen_accessor_inline`) are Rust-facing only, unchanged
- `abi_helpers.rs` still used, but only by `codegen_extern_wrapper`

### Verification — ✅ done

All 44 tests passed. Behavior-preserving refactor — same LLVM IR semantics,
split across two functions per toylang function. No surprises.

### Implementation notes

- `codegen_internal_function` takes no `instance` param — uses `ResolvedType`
  from typed AST for all ABI decisions
- `codegen_extern_wrapper` takes `instance` for `coerced_return_type_for_instance`
- `resolve_return_type` added to `type_resolve.rs` — lightweight helper that
  resolves just the return type without running the full body
- `is_internal_sret(ty)` helper — `matches!(ty, Struct{..} | Vec{..})`
- FnCall arm returns `ExprResult::Ptr` for struct/vec returns (alloca + sret call)

---

## Phase 5: Bool Return ABI Test — ✅ done

### Context

LLVM uses `i1` for booleans. Rust ABI expects `i8`. Investigated whether the
extern wrapper needed explicit `zext i1 to i8` coercion.

### 5a. Added `test_bool_return` test

Tests `always_true() -> bool` and `always_false() -> bool` called from Rust with
`assert!` and `println!`.

### Outcome

Test passed without code changes. The `coerce_int_to_type` call in the extern
wrapper's direct return path already handles i1→i8 extension.

---

## Phase 6: Struct Param ABI Smoke Test

### Context

Rust ABI may coerce struct *parameters* (not just returns) — small structs can be
passed in registers differently than our naive LLVM StructType representation.

Currently, `codegen_function` passes struct params as raw LLVM StructTypes, and
this works for the existing tests (`test_struct_param_passthrough` passes — Rust
calls `identity(c: Counter) -> Counter` which takes and returns a struct).

The internal/extern split (Phase 4) doesn't change param handling — both internal
and extern use the same param types. But we should add a test specifically for
toylang-to-toylang struct param passing to catch any issues.

### 6a. Add toylang-to-toylang struct param test

**File:** `toylangc/tests/integration_tests.rs`

```rust
#[test]
fn test_toylang_to_toylang_struct_param() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn get_x(p: Point) -> i32 {
    p.x
}

fn main() {
    let p = Point { x: 42, y: 99 };
    println("{}", get_x(p));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("42"));
}
```

This tests the full chain: toylang `main` creates a struct, passes it by value
to another toylang function `get_x`, which accesses a field and returns it via
println. Both the struct param passing and the field access happen entirely within
toylang's internal ABI.

### 6b. Add larger struct param test (if 6a passes)

```rust
#[test]
fn test_toylang_to_toylang_large_struct_param() {
    let output = run_toylang_test(
        r#"
struct Quad {
    a: i32,
    b: i32,
    c: i32,
    d: i32,
}

fn sum_quad(q: Quad) -> i32 {
    q.a + q.b + q.c + q.d
}

fn main() {
    let q = Quad { a: 10, b: 20, c: 30, d: 40 };
    println("{}", sum_quad(q));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("100"));
}
```

A 4-field struct (16 bytes) may cross the threshold where aarch64 switches from
register passing to indirect. This test catches that edge case.

### 6c. Fix struct param ABI if tests fail

If the tests fail, the extern wrapper needs param adaptation:
- Query `fn_abi.args[i].mode` for each param
- If `PassMode::Cast` or `PassMode::Indirect`, adapt the param in the wrapper
  before calling the internal function

If the tests pass (likely, since `test_struct_param_passthrough` already passes
for Rust→toylang struct params), no code changes needed — just the tests.

### Verification — ✅ done

Both tests passed without code changes. 47 tests total.

---

## Phase 7: Param ABI Coercion from fn_abi — ✅ done

### Context

`test_generic_callee_with_struct` produced `x: 10 y: <garbage>`. Investigation
of the generated LLVM IR showed that the extern wrapper declared Point params as
`{ i32, i32 }` but Rust passed them as `i64` (coerced on aarch64). The return
side was correctly ABI-derived but the param side used string-based type matching.

### What was done

- Added `CoercedParam` enum and `coerced_param_types_for_instance` to
  `abi_helpers.rs` — mirrors the existing return ABI query
- Replaced string-based param type building in `codegen_extern_wrapper` with
  ABI-derived types from `fn_abi.args`
- Added param conversion: when Rust ABI type differs from internal type,
  bitcast via memory (alloca/store/load)
- Added `on_stack` assertion for future byval detection

### Empirical ABI findings on aarch64

- `Point { i32, i32 }` → `PassMode::Cast` to `i64`
- `Counter { i32 }` → `PassMode::Cast` to `i32`
- `&Vec<T>` → `PassMode::Direct` with `Scalar(Pointer)`, size=64
- Primitives → `PassMode::Direct` with matching scalar size

### Known limitation

For ref params (`&Vec<T>`), `fn_abi` reports `Direct("i64")` but internal
expects `ptr`. The bitcast-via-memory fires unnecessarily. LLVM's mem2reg
eliminates it. Future optimization: detect `Scalar(Pointer)` in `backend_repr`.

### Verification — ✅ done

`test_generic_callee_with_struct` passes. 49 tests, 0 ignored.

---

## Phase 8: Forward Type Inference from Args — ✅ done, then reverted

### What was attempted

Added `infer_type_params_from_arg` to infer generic type params from argument
types (e.g. `wrap(42)` → arg is `i32` → `T = i32`). Fixed `test_generic_callee_in_let`.

Also added `infer_let_types_from_return` to propagate return types backward to
let bindings (e.g. `let x = 10` in fn returning i64 → x is i64).

### What went wrong

`infer_let_types_from_return` broke generic functions. For `fn wrap<T>(x: T) -> Wrapper<T>`,
the return type contains unresolved `T`. The hint tried to parse `Wrapper<T>` as
a concrete type and panicked. The inference ran before type param substitution.

### Decision

Rather than fix the inference to handle generic functions, decided to remove ALL
inference and require explicit type args. This is a proof-of-concept compiler;
simplicity wins.

---

## Phase 9: Remove All Inference, Require Explicit Type Args — ✅ done

### Design

- Generic function calls: `wrap<i32>(42)` instead of `wrap(42)`
- Vec creation: `Vec::new<Point>()` instead of `Vec::new()` with heuristic inference
- No backward type propagation to let bindings
- Integer literals still default to i32 unless context provides expected type
  (return position, struct field, function param)

### What was removed

- `infer_type_args_from_expected` — inferred type params from expected return type
- `infer_type_params_from_arg` — inferred type params from argument types
- `infer_let_types_from_return` — propagated return type to let bindings
- `infer_vec_types` and all helpers (`infer_from_push`, `infer_from_struct_field`,
  `infer_from_struct_lit`, `infer_expr_type`, `is_vec_new`)
- `resolved_type_to_string` — only used by inference
- `vec_inferences` and `let_type_hints` parameter threading
- `find_vec_elem_from_body` and `find_vec_elem_from_typed_body` in llvm_gen.rs

### What was added

- `type_args: Vec<String>` field on `Expr::FnCall` and `Expr::StaticCall`
- `parse_type_arg_list()` in parser — parses `<T1, T2>` syntax
- `find_vec_elem_from_explicit_ast` in llvm_gen.rs — reads Vec element type
  from `StaticCall` type_args directly
- `find_vec_elem_from_explicit_type_args` in callbacks_impl.rs — same for
  dependency discovery

### Bug fixes included

- **Vec method lookup** — replaced `name.contains("new")` substring matching
  with exact `vec_fns` HashMap keyed by method name
- **Lexer** — panics on unknown characters instead of silently skipping
- **Empty registry** — panics on missing `--toylang-input` instead of silently
  producing empty program

### Test changes

- All `Vec::new()` → `Vec::new<ElemType>()` (20 instances)
- All generic calls → explicit type args (`wrap<i32>(42)`, `identity<Point>(p)`)
- 4 new tests: `test_vec_method_lookup_is_exact`, `test_vec_push_fn_call_result`,
  `test_lexer_rejects_unknown_chars`, `test_int_literal_infers_i64_from_return_type`

### Verification — ✅ done

53 tests, 0 ignored, zero warnings.

---

## Final Critical Files

| File | Phase | Change |
|------|-------|--------|
| `toylangc/src/llvm_gen.rs` | 4, 7, 9 | Internal/extern ABI split; param ABI coercion; removed heuristic Vec elem discovery |
| `toylangc/src/toylang/type_resolve.rs` | 9 | Removed all inference; explicit type args on FnCall/StaticCall |
| `toylangc/src/toylang/ast.rs` | 9 | Added `type_args` to FnCall and StaticCall |
| `toylangc/src/toylang/parser.rs` | 9 | `parse_type_arg_list()`; lexer panics on unknown chars |
| `toylangc/src/toylang/callbacks_impl.rs` | 7, 9 | Param ABI coercion; explicit Vec elem type discovery |
| `rustc-lang-facade/src/abi_helpers.rs` | 7 | `CoercedParam` enum + `coerced_param_types_for_instance` |
| `toylangc/src/main.rs` | 9 | Panic on missing `--toylang-input` |
| `toylangc/tests/integration_tests.rs` | all | 53 tests, explicit type args everywhere |
