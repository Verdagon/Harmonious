# Phase 4: I/O Integration (with GLOBALS deadlock fix) — COMPLETED

## Context

Toylang had trait method calls (Phase 1), free function calls (Phase 2), and byte string literals (Phase 3). Phase 4 combined all three to make `Write::write_all(&stdout(), b"hello from toylang\n")` work end-to-end — the first real I/O from toylang without Rust glue code.

## What was done

### Part A: Split GLOBALS into immutable config + mutable state

Discovered a deadlock: `call_generate_and_compile` held a single `Mutex<FacadeGlobals>` while the consumer's LLVM codegen made tcx queries. `tcx.symbol_name(stdout)` triggered `lang_symbol_name` → `default_symbol_name()` → tried to re-lock → deadlock.

Split into:
- `CONFIG: OnceLock<FacadeConfig>` — callbacks + vtable (immutable, no lock needed)
- `DEFAULT_LAYOUT_OF/MIR_SHIMS/SYMBOL_NAME: OnceLock<fn>` — saved query providers (immutable)
- `MUTABLE_STATE: OnceLock<Mutex<FacadeMutableState>>` — consumer_state + lang_obj_path (locked only by callbacks needing `&mut` state)

Documented as @GCMLZ arcana.

### Part B: Removed diagnostic prints

Cleaned up all `eprintln!` debugging from the deadlock investigation across lib.rs, codegen_wrapper.rs, layout.rs, symbol_name.rs, and llvm_gen.rs.

### Part C: sret + ABI return type handling for FnCall use-import path

FnCall path now queries `coerced_return_type_for_instance` and handles all three modes:
- `Indirect` (sret): allocate alloca, pass as first arg, declare void return
- `Direct(s)`: use `parse_coerced_type` for LLVM declaration (NOT `resolved_to_inkwell`)
- `Void`: no return

Also discovered that using `resolved_to_inkwell` (toylang's `[8 x i8]`) for the return type instead of the ABI-coerced type (`i64`) caused segfaults — LLVM treats aggregate vs scalar returns differently. Added store-through-pointer reinterpretation when types differ.

Documented as @ACRTFDZ arcana.

### Part D: Broadened type handling

- `rustc_ty_to_resolved_type`: unsupported int/uint/float variants now produce opaque `RustType` instead of panicking. Added `Str`, `Never`, `RawPtr`, `Dynamic`, non-empty `Tuple`.
- `resolved_to_rustc_ty`: maps primitive names (u8, u16, u32, u64, i8, i16, f32) back to rustc types.
- `find_reexported_type`: now matches `DefKind::Enum` (Option, Result) in addition to `DefKind::Struct`.

### Part E: Integration tests

6 new tests:
- `test_stdout_call` — stdout() free function returning struct
- `test_stdout_write_all` — full Write::write_all I/O chain
- `test_stdout_multiple_writes` — two sequential writes
- `test_write_all_result_bound` — binding Result<(), Error> to a variable
- `test_vec_pop_returns_option` — Vec::pop() returning Option<i32>
- `test_rust_fn_returning_option_u8` — extern fn returning Option<u8>

## Files touched

- `rustc-lang-facade/src/lib.rs` — GLOBALS split into CONFIG + MUTABLE_STATE
- `rustc-lang-facade/src/codegen_wrapper.rs` — use set/get_lang_obj_path helpers
- `rustc-lang-facade/src/queries/layout.rs` — @GCMLZ comments
- `rustc-lang-facade/src/queries/symbol_name.rs` — @GCMLZ comments
- `rustc-lang-facade/src/queries/drop_glue.rs` — @GCMLZ comments
- `rustc-lang-facade/src/abi_helpers.rs` — @ACRTFDZ comments on CoercedReturn
- `toylangc/src/llvm_gen.rs` — sret + ABI return type in FnCall path, @ACRTFDZ comments
- `toylangc/src/oracle.rs` — TyKind broadening, enum in find_reexported_type, primitive round-tripping
- `toylangc/tests/integration_tests.rs` — 6 new tests
- `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` — new arcana
- `docs/arcana/ABICoercedReturnTypeInFunctionDeclarations-ACRTFDZ.md` — new arcana

## Final test count

54 unit tests + 110 integration tests = 164 total, all passing.
