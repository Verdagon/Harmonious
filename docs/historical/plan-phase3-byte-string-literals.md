# Phase 3: Byte String Literals — Implementation Record

## Context

Phase 3 of quest.md. Added `b"hello\n"` syntax producing `&[u8]` (fat pointer: ptr + len). Main consumer is Phase 4's `Write::write_all(&stdout(), b"hello\n")`.

## What was done

### 1. ScalarPair ABI fix (pre-requisite)

**Discovery**: `PassMode::Pair` in rustc means TWO separate LLVM function parameters (confirmed in `~/rust/compiler/rustc_codegen_llvm/src/abi.rs:363-369` — two `push()` calls to `llargument_tys`). The facade at `abi_helpers.rs:132-140` mapped it to ONE `CoercedParam::Direct("{ ptr, i64 }")`. This was latent — no existing code triggered ScalarPair — but would break any `&[u8]` interop.

**Fix**: Added `CoercedParam::Pair(String, String)` variant to `abi_helpers.rs`. Updated all 5 consumers in `llvm_gen.rs`:
- `get_or_resolve_rust_method()`: push two param types in declaration
- `generate_extern_wrapper()` param_types: push two param types
- `generate_extern_wrapper()` forwarding: fetch two LLVM params, reassemble into struct via `build_insert_value` for internal function
- MethodCall/StaticCall call sites: new `push_arg_for_rust_call` helper splits `{ ptr, i64 }` struct into two args via `build_extract_value`
- FnCall path: query `coerced_param_types_for_instance`, build ABI-correct declaration

Stored `coerced_params` in `RustMethodInfo` for call-site use.

### 2. FnCall path unified with ABI awareness

**Discovery**: The FnCall extern/use-import path built LLVM declarations from toylang-internal types, not from rustc's ABI. This worked for simple scalars but would break for ScalarPair types.

**Additional discovery during implementation**: Initially merged both extern-decl and use-import paths into one ABI-aware path. This broke extern C functions because `push_arg_for_rust_call` used `into_ptr` (passes pointer), but extern C functions expect `into_value` (passes direct value). The MethodCall/StaticCall paths pass args as pointers because `get_or_resolve_rust_method` declares functions with pointer params (Indirect convention). The FnCall path declares functions with coerced types (Direct convention), so args must be passed as direct values.

**Fix**: FnCall path uses inline arg building with `into_value` + ScalarPair splitting, not the `push_arg_for_rust_call` helper (which uses `into_ptr`).

### 3. Byte string literals across 8 files

Standard "follow StringLit everywhere" pattern:
- **parser.rs**: `Token::ByteStringLit(Vec<u8>)`, `Token::LBracket`/`RBracket`, byte string tokenization with escape handling (`\n`, `\t`, `\\`, `\0`, `\"`), `ParseError::UnknownEscape`/`UnterminatedString`, `[u8]` type parsing → `ByteSlice`
- **ast.rs**: `Expr::ByteStringLit(Vec<u8>)`
- **typed_ast.rs**: `ResolvedType::ByteSlice`, `TypedExprKind::ByteStringLit(Vec<u8>)`
- **type_resolve.rs**: resolves to `Ref { inner: ByteSlice }`, passthrough in `substitute_type_params_in_body`
- **oracle.rs**: `ByteSlice` ↔ `ty::Ty::new_slice(tcx, tcx.types.u8)`, `TyKind::Slice(u8)` → `ByteSlice`, mangled name `"byte_slice"`
- **stub_gen.rs**: `ByteSlice` → `parse_quote!([u8])`
- **callbacks_impl.rs**: no-deps catch-all
- **llvm_gen.rs**: `Ref { inner: ByteSlice }` → fat pointer struct `{ ptr, i64 }` in `resolved_to_inkwell`, global constant byte array + GEP + struct construction in codegen

### 4. `[u8]` type parsing for extern fn declarations

Added `LBracket`/`RBracket` tokens and `[u8]` → `ByteSlice` in `parse_type`, so extern fn declarations like `fn check_bytes(data: &[u8]) -> i32` work. This enabled the critical ScalarPair ABI integration test.

### 5. Defensive assertions

- FnCall: `call_args.len() == param_types.len()` — catches forgotten `is_scalar_pair_type` updates
- Wrapper: `rust_llvm_idx == function.count_params()` — catches Pair/Direct/Indirect index accounting bugs
- Wrapper Pair: reassembled struct type matches `internal_param_types[i]` — catches ABI ↔ internal type drift
- ByteStringLit: `debug_assert` fat pointer struct matches `resolved_to_inkwell` output
- `is_scalar_pair_type`: SYNC comment documenting it must stay in sync with `CoercedParam::Pair`

## Tests added

| Test | Type | What it verifies |
|------|------|-----------------|
| `test_lex_byte_string` | unit | `b"hello"` tokenizes to correct bytes |
| `test_lex_byte_string_escapes` | unit | `\n`, `\t`, `\0` escape handling |
| `test_lex_byte_string_escaped_quote` | unit | `\"` escape inside byte strings |
| `test_lex_byte_string_empty` | unit | `b""` produces empty vec |
| `test_lex_byte_string_unterminated` | unit | Missing closing quote → error |
| `test_lex_byte_string_unknown_escape` | unit | `\q` → UnknownEscape error |
| `test_resolve_byte_string_lit` | unit | Type resolves to `Ref { inner: ByteSlice }` |
| `test_byte_string_let_binding` | integration | `let x = b"hello"` compiles and runs |
| `test_byte_string_passed_to_rust_fn` | integration | Pass `b"hello"` to Rust `fn(&[u8]) -> i32`, verify len is 5 (ScalarPair ABI test) |

## Final test counts

- 54 unit tests (was 47)
- 104 integration tests (was 102)
- 158 total, all passing
