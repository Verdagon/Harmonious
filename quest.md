# Plan: toylang.toml, Trait Methods, Rust Free Functions, Byte Strings, I/O, and Test Projects

## Context

Toylang currently compiles through rustc but can only use `std` types because there's no Cargo integration. The goal is to let toylang use arbitrary Rust crates (rand, regex, uuid, etc.) by:

1. Adding a `toylang.toml` manifest that declares Rust crate dependencies
2. Building a `toylangc build` command that orchestrates Cargo builds
3. Adding language features needed to call Rust APIs without glue code: trait method resolution, Rust free function calls, byte string literals
4. Proving it works with standalone test projects for 9 crates

This gives toylang control over its own build story — Cargo becomes an implementation detail the user never sees.

**Key discovery**: None of the target crates (rand, regex, clap, toml, serde, etc.) require derive macros. They all have imperative/builder APIs. The main unlock is `.unwrap()` on Result/Option, which is an inherent method already findable by `find_inherent_method`.

---

## Phase 1: Explicit Trait Method Calls — DONE

**Status**: Implemented and tested. 100 integration tests + 37 unit tests passing.

**What was built**: `Trait::method(receiver, args)` syntax for calling trait
methods, reusing the existing `StaticCall` AST node. Example:
`Clone::clone(&v)` where `Clone` is imported via `use std::clone::Clone`.

### What was implemented

- **oracle.rs**: `find_use_imported_trait_def_id`, `find_trait_method`,
  `rust_trait_method_return_type`, `strip_ref`
- **type_resolve.rs**: StaticCall arm restructured (args resolved first),
  `__trait::` prefix convention for trait vs type disambiguation
- **callbacks_impl.rs**: `__trait::` callback handling, `receiver_ty` field on
  `RustMethodDep`, trait dep resolution using trait definition method DefId
  (see @TVIMDGAZ)
- **llvm_gen.rs**: `receiver_ty` param on `get_or_resolve_rust_method`, trait
  call codegen with proper receiver handling, `has_track_caller` on
  `RustMethodInfo` with null pointer append (see @TCHAPZ)
- **ast.rs / parser.rs / typed_ast.rs**: `&expr` reference expressions
  (`Expr::Ref`, `TypedExprKind::Ref`)

### Discoveries and fixes

1. **`#[track_caller]` hidden ABI parameter** (@TCHAPZ): ~43 Vec methods have a
   hidden `&Location` pointer appended by rustc's ABI. This was a pre-existing
   latent UB for all MethodCall sites. Fixed by passing null for the hidden param
   at all 4 call sites. See `docs/arcana/TrackCallerHiddenABIParameter-TCHAPZ.md`.

2. **Trait vs impl method DefId** (@TVIMDGAZ): Trait definition and impl method
   DefIds have different generic parameter structures. Must use the trait
   definition's DefId with `[Self, ...]` args, not the impl's DefId with
   `[T, A, ...]`. `Instance::expect_resolve` maps from trait-level to impl-level
   automatically. See `docs/arcana/TraitVsImplMethodDefIdGenericArgs-TVIMDGAZ.md`.

3. **ReifyShim produces different symbol names**: Investigated using ReifyShim to
   avoid `#[track_caller]` — rejected because both mangling schemes append
   shim-specific suffixes, causing linker errors.

### Tests added

| Test | What it verifies |
|------|-----------------|
| `test_trait_static_call_inherent_still_works` | Vec::new inherent StaticCall still works |
| `test_trait_static_call_clone_vec` | Clone::clone(&v) with sret + track_caller fix |
| `test_trait_static_call_clone_vec_use_result` | Cloned Vec has correct length |
| `test_trait_static_call_result_discarded` | Clone result as ExprStmt (discarded) |
| `test_ref_expr_basic` | &var as argument to trait method |

---

## Phase 2: Rust Free Function Calls from Modules — DONE

**Status**: Implemented and tested. 102 integration tests + 47 unit tests passing.

**What was built**: Use-imported free function calls (`use std::io::stdout` → `stdout()`),
plus uniform argument type checking across all call types (toylang FnCall, free function
FnCall, MethodCall, StaticCall). Added `rust_param_types` callback parallel to
`rust_method_ret` so the type resolver can check args for Rust calls.

### What was implemented

- **oracle.rs**: `find_use_imported_fn_def_id`, `instantiate_free_fn_sig` (private),
  `rust_free_fn_return_type`, `rust_free_fn_param_types`, `rust_method_param_types`,
  `rust_trait_method_param_types`
- **type_resolve.rs**: `ArgTypeMismatch` error variant, `types_match()` for semantic type
  comparison, `rust_param_types` callback threaded through `resolve_fn_body`/`resolve_expr`/
  `resolve_stmt`, FnCall free function branch with existence sentinel, MethodCall and
  StaticCall arg checking
- **callbacks_impl.rs**: `rust_method_ret` closures extended for free functions (×2),
  `rust_param_types` closures added (×2), dep collection for use-imported free fns with
  real type args
- **llvm_gen.rs**: `rust_method_ret`/`rust_param_types` closures, FnCall dispatch
  restructured (`is_extern_decl || is_use_import`), real type args via `mk_args`

### Discoveries and fixes

1. **`StructRef` vs `Struct` type mismatch**: `rustc_ty_to_resolved_type` produces `StructRef`
   for toylang types, but the type resolver produces `Struct` (with field types filled in).
   Equality check fails even though they're semantically identical. Fixed by adding
   `types_match()` that handles this equivalence in all arg type checking.

2. **MethodCall self-param offset**: `sig.inputs()` includes self as `inputs()[0]`, but
   MethodCall args don't include the receiver (it's a separate AST field). Initially tried
   prepending receiver for uniform checking, but hit the autoref wall — receiver type is
   `Vec<I32>` while `sig.inputs()[0]` is `&mut Vec<I32>` (toylang doesn't model autoref).
   Fixed by checking explicit args against `param_types[i + 1]`, skipping self. StaticCall
   args already align with `sig.inputs()` naturally (no offset needed).

3. **`instantiate_identity()` is wrong for function signatures**: The original plan used
   `tcx.fn_sig(def_id).instantiate_identity()` — a no-op unwrap of `EarlyBinder` that
   leaves `ty::Param` intact, crashing `rustc_ty_to_resolved_type` on generic functions.
   Fixed by always using `.instantiate(tcx, args)` (non-generic = empty args, same path).
   Compiler law: non-generic is the degenerate case of generic.

4. **`Option`-based existence sentinel**: `rust_param_types` returns `Option<Vec>` — `None`
   means "not found", `Some(vec![])` means "found, takes no args". This cleanly
   distinguishes "function doesn't exist" from "function returns void", avoiding the
   Void-as-sentinel ambiguity from the original plan.

### Tests added

| Test | What it verifies |
|------|-----------------|
| `test_arg_type_mismatch_i32_vs_i64` | i64 passed where i32 expected → ArgTypeMismatch |
| `test_arg_type_mismatch_bool_vs_i32` | bool passed where i32 expected → ArgTypeMismatch |
| `test_arg_type_mismatch_generic_fn` | wrong type after generic substitution |
| `test_arg_type_correct_passes` | correct arg types produce no error |
| `test_arg_type_extra_args_no_crash` | extra args beyond params resolve without error |
| `test_free_fn_not_found_gives_undefined_error` | None from callback → UndefinedFunction |
| `test_free_fn_void_returning_resolves_correctly` | void return not confused with "not found" |
| `test_free_fn_correct_args_pass` | correct args to free fn pass |
| `test_free_fn_with_args_type_checked` | wrong arg type to free fn → ArgTypeMismatch |
| `test_free_fn_return_type_propagates` | free fn return type flows into let binding |
| `test_extern_fn_decl_still_works` | regression: body-less extern fn still compiles |
| `test_rust_free_fn_undefined_gives_error` | compile-fail: undefined function rejected |

---

## Phase 3: Byte String Literals — DONE

**Status**: Implemented and tested. 104 integration tests + 54 unit tests passing.

**What was built**: `b"hello\n"` syntax producing `&[u8]` (fat pointer: ptr + len). Also fixed a latent ScalarPair ABI bug in the facade, added `[u8]` type parsing for extern fn declarations, and unified the FnCall extern/use-import code path with ABI-aware declarations.

### What was implemented

- **parser.rs**: `Token::ByteStringLit(Vec<u8>)`, `Token::LBracket`/`RBracket`, tokenization with escape handling (`\n`, `\t`, `\\`, `\0`, `\"`), `ParseError::UnknownEscape` and `ParseError::UnterminatedString`, `[u8]` type parsing → `ByteSlice`
- **ast.rs**: `Expr::ByteStringLit(Vec<u8>)`
- **typed_ast.rs**: `ResolvedType::ByteSlice` (unsized `[u8]` type), `TypedExprKind::ByteStringLit(Vec<u8>)`
- **type_resolve.rs**: `ByteStringLit` → `Ref { inner: ByteSlice }`, passthrough in `substitute_type_params_in_body`
- **oracle.rs**: `ByteSlice` ↔ `ty::Ty::new_slice(tcx, tcx.types.u8)`, `TyKind::Slice(u8)` → `ByteSlice`, mangled name `"byte_slice"`
- **stub_gen.rs**: `ByteSlice` → `[u8]`
- **callbacks_impl.rs**: `ByteStringLit` in no-deps catch-all
- **llvm_gen.rs**: `Ref { inner: ByteSlice }` → fat pointer struct `{ ptr, i64 }` in `resolved_to_inkwell`, global constant byte array + fat pointer construction in codegen, `push_arg_for_rust_call` helper for ScalarPair arg splitting
- **abi_helpers.rs**: `CoercedParam::Pair(String, String)` variant, all 5 consumers updated

### Discoveries and fixes

1. **ScalarPair ABI bug**: `PassMode::Pair` in rustc means TWO separate LLVM function parameters (confirmed by reading `rustc_codegen_llvm/src/abi.rs:363-369`). But the facade at `abi_helpers.rs` mapped it to one `CoercedParam::Direct("{ ptr, i64 }")`. Fixed by adding `CoercedParam::Pair(String, String)` variant and updating all 5 consumers in `llvm_gen.rs` (method declaration, wrapper param building, wrapper forwarding, call sites, FnCall use-import path). This was latent — no existing code triggered ScalarPair — but would have broken any `&[u8]` interop.

2. **FnCall path lacked ABI awareness**: The FnCall extern/use-import codepath built LLVM function declarations from toylang-internal types, not from rustc's ABI. Both paths now query `coerced_param_types_for_instance` for ABI-correct declarations. Critical distinction: FnCall passes args as direct values (matching the coerced param types), while MethodCall/StaticCall pass args as pointers (matching `get_or_resolve_rust_method`'s Indirect convention). Using `into_ptr` in the FnCall path caused silent data corruption for simple extern C functions.

### Tests added

| Test | What it verifies |
|------|-----------------|
| `test_lex_byte_string` | `b"hello"` tokenizes to correct bytes |
| `test_lex_byte_string_escapes` | `\n`, `\t`, `\0` escape handling |
| `test_lex_byte_string_escaped_quote` | `\"` escape inside byte strings |
| `test_lex_byte_string_empty` | `b""` produces empty vec |
| `test_lex_byte_string_unterminated` | Missing closing quote → error |
| `test_lex_byte_string_unknown_escape` | `\q` → UnknownEscape error |
| `test_resolve_byte_string_lit` | Type resolves to `Ref { inner: ByteSlice }` |
| `test_byte_string_let_binding` | `let x = b"hello"` compiles and runs |
| `test_byte_string_passed_to_rust_fn` | Pass `b"hello"` to Rust `fn(&[u8]) -> i32`, verify len is 5 (ScalarPair ABI test) |

---

## Phase 4: I/O Integration, ABI Return Coercion, Type Broadening — DONE

**Status**: Implemented and tested. 110 integration tests + 54 unit tests passing.

**What was built**: `Write::write_all(&stdout(), b"hello from toylang\n")` works
end-to-end. Also fixed a GLOBALS mutex deadlock, added sret return handling for
use-imported free functions, fixed ABI return type coercion for opaque Rust types,
broadened `rustc_ty_to_resolved_type` to handle more TyKind variants, and added
enum support to `find_reexported_type`.

### What was implemented

- **rustc-lang-facade/src/lib.rs**: Split `GLOBALS` single mutex into `CONFIG`
  (OnceLock, immutable), `DEFAULT_*` (OnceLock, immutable), and `MUTABLE_STATE`
  (Mutex, mutable). Fixes deadlock where `generate_and_compile` held the mutex
  and tcx queries triggered query providers that tried to re-lock it (see @GCMLZ).
- **toylangc/src/llvm_gen.rs**: FnCall use-import path now queries
  `coerced_return_type_for_instance` for sret handling and uses ABI-coerced return
  types in LLVM declarations instead of toylang types (see @ACRTFDZ). Added
  store-through-pointer reinterpretation when ABI type differs from toylang type.
- **toylangc/src/oracle.rs**: `rustc_ty_to_resolved_type` now handles unsupported
  int/uint/float variants, `Str`, `Never`, `RawPtr`, `Dynamic`, and non-empty
  `Tuple` as opaque `RustType`. `resolved_to_rustc_ty` maps primitive names
  (u8, u16, etc.) back to rustc types. `find_reexported_type` matches
  `DefKind::Enum` (not just `Struct`), fixing Option/Result lookup.

### Discoveries and fixes

1. **GLOBALS mutex deadlock** (@GCMLZ): `call_generate_and_compile` held a single
   mutex guarding both immutable config and mutable state. During LLVM codegen,
   `tcx.symbol_name(stdout)` triggered `lang_symbol_name` which called
   `default_symbol_name()` → tried to re-lock the same mutex → deadlock. Fixed by
   splitting into OnceLock (immutable) + Mutex (mutable only).

2. **ABI return type coercion** (@ACRTFDZ): FnCall use-import path declared LLVM
   functions with `resolved_to_inkwell` return types (e.g., `[8 x i8]` for Stdout),
   but rustc returns `i64` (Direct scalar). LLVM treats aggregate vs scalar returns
   differently — the mismatch caused garbage return values and segfaults. Fixed by
   using `parse_coerced_type` from `CoercedReturn::Direct`.

3. **Enum type lookup**: `find_reexported_type` only matched `DefKind::Struct`.
   `Option` and `Result` are `DefKind::Enum` → not found → panic. Fixed by also
   matching `DefKind::Enum`.

4. **Risk items that were NOT problems**: `&mut self` mutability (toylang's `&`
   works fine — rustc resolves the trait impl regardless). `write_all` as default
   trait method (Instance::expect_resolve handles it). `#[track_caller]` on
   write_all (it has none). ScalarPair for `&[u8]` (already handled by Phase 3).

### Tests added

| Test | What it verifies |
|------|-----------------|
| `test_stdout_call` | `stdout()` free function returning struct via Direct ABI |
| `test_stdout_write_all` | Full `Write::write_all(&out, b"hello\n")` I/O chain |
| `test_stdout_multiple_writes` | Two sequential `Write::write_all` calls |
| `test_write_all_result_bound` | Binding `Result<(), Error>` return to a variable |
| `test_vec_pop_returns_option` | `Vec::pop()` returning `Option<i32>` |
| `test_rust_fn_returning_option_u8` | Extern fn returning `Option<u8>` (exercises u8 type handling) |

---

## Phase 5: toylang.toml and Build Orchestration — DONE

**Status**: Implemented and tested. 60 unit + 110 integration + 4 standalone
tests passing.

**What was built**: `toylangc build` reads `toylang.toml`, generates a hidden
`.toylang-build/` Cargo project, and invokes `cargo +rustc-fork build` with
`RUSTC_WORKSPACE_WRAPPER=<self>`. Cargo compiles deps with real rustc and the
primary crate through toylangc wrapper mode, gated by
`CARGO_PRIMARY_PACKAGE=1`. Wrapper mode rediscovers the toylang source by
re-reading `toylang.toml` from `CARGO_MANIFEST_DIR/..` — no env-var
side-channel, manifest is single source of truth.

### What was implemented

- **toylangc/Cargo.toml**: `toml = "0.8"` dep
- **toylangc/src/manifest.rs** (new): `Manifest`/`Project`/`DepSpec` structs
  with serde derives, `parse()` using `toml::from_str`, 6 inline unit tests
- **toylangc/src/build.rs** (new): `build_project()` generates
  `.toylang-build/{Cargo.toml, src/main.rs, rust-toolchain.toml}` and spawns
  `cargo +rustc-fork build` with `RUSTC_WORKSPACE_WRAPPER`,
  `DYLD_LIBRARY_PATH`, `LD_LIBRARY_PATH` env vars
- **toylangc/src/main.rs**: refactored into three-mode dispatch
  (build/wrapper/direct), added `NoopCallbacks` + `run_plain_rustc` for
  dependency pass-through
- **toylangc/tests/standalone_tests.rs** (new): 4 tests — minimal project,
  project with `toml = "0.8"` dep, invalid manifest, missing source
- **.gitignore**: `.toylang-build/`
- **docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md** (new): documents
  the dual-role of `toylang.toml` as both user manifest and side-channel
  between build-mode and wrapper-mode processes
- **README.md**: new "Building a project with `toylang.toml`" section for
  end users, with a link to the arcana
- **@MRRIWMZ** code annotations at both manifest read sites

### Discoveries and fixes

1. **Dropped `TOYLANG_INPUT` env var in favor of re-reading the manifest**
   (@MRRIWMZ). The original plan passed the `.toylang` source path to
   wrapper mode via `TOYLANG_INPUT=<abs path>` on the cargo subprocess.
   Replaced with: wrapper mode walks up from `CARGO_MANIFEST_DIR` to find
   `toylang.toml`, re-parses it, and resolves `source` itself. Trade-off:
   manifest is parsed twice per build (microseconds). Win: no hidden state,
   the manifest is the single source of truth for both processes.

2. **`DYLD_LIBRARY_PATH` caveat**. Cargo doesn't inherit `DYLD_LIBRARY_PATH`
   from the shell when spawning subprocesses. Even the outer
   `toylangc build` requires it set (toylangc links `librustc_driver` dynamically).
   Fix: build mode computes the sysroot via `rustc +rustc-fork --print sysroot`
   and sets both `DYLD_LIBRARY_PATH` (macOS) and `LD_LIBRARY_PATH` (Linux) on
   the cargo subprocess so wrapper-mode children inherit them.

3. **All three pre-identified risks were de-risked via code reading before
   implementation**: (A) `rustc_driver::RunCompiler` with no-op `Callbacks`
   does NOT suppress codegen/linking — confirmed by reading
   `rustc_driver_impl/src/lib.rs`. (B) `LangFileLoader::is_stubs_path` matches
   on `file_name()`, so absolute paths from cargo work identically to relative
   paths — no facade changes needed. (C) `.toylang-build/rust-toolchain.toml`
   pinning `rustc-fork` ensures the child cargo uses the right toolchain.

### Tests added

| Test | What it verifies |
|------|-----------------|
| `manifest::tests::test_parse_minimal` | name + source only, edition defaults to "2021" |
| `manifest::tests::test_parse_explicit_edition` | explicit edition round-trips |
| `manifest::tests::test_parse_features` | features list parses |
| `manifest::tests::test_parse_simple_dep` | `rand = "0.8"` → `DepSpec::Version` |
| `manifest::tests::test_parse_detailed_dep` | `regex = { version = "1", features = ["unicode"] }` → `DepSpec::Detailed` |
| `manifest::tests::test_parse_missing_project_errors` | missing `[project]` returns `Err` |
| `test_build_minimal_project` | empty `fn main() {}` builds + runs with exit 0 |
| `test_build_with_rust_dep` | `toml = "0.8"` dep resolves; `Cargo.lock` lists `toml` |
| `test_build_invalid_manifest_fails` | missing `[project]` → non-zero exit, no `.toylang-build/` created |
| `test_build_missing_source_fails` | missing source file → non-zero exit |

See `docs/historical/plan-phase5-toylang-toml-build.md` for the full
implementation history and lessons learned.

---

## Phase 6: .unwrap() and Result/Option Handling — DONE

**Status (step 1)**: Implemented and tested. 116 integration + 60 unit + 4 standalone = 180 tests passing.

**What was built**: `#[inline(never)]` wrappers in `__lang_stubs` that take the
receiver by raw pointer and use `core::ptr::read` to consume it before
calling `.unwrap()`. Both `Option::unwrap` and `Result::unwrap` (the latter
with `E: Debug` bound preserved verbatim). Wrapper redirect lives in
`oracle::redirect_to_wrapper`, called from both dep registration
(`callbacks_impl::collect_toylang_fn_deps_inner`) and codegen
(`llvm_gen::get_or_resolve_rust_method`) — see @SMINCZ for why both sites
are required.

External linkage of the wrapper symbol is forced via a 14-line patch in the
forked `rustc_monomorphize/src/partitioning.rs::mono_item_linkage_and_visibility`:
items in `__lang_stubs` get `(External, Default)` uniformly, regardless of
generics. The check uses `tcx.def_path(...).data` walking, NOT
`tcx.def_path_str` — see @DPSFDOZ for the diagnostic-only ICE trap.

Two prior attempts hit different failure modes (mono never happened, then
mono happened but partitioner internalized the symbol). Both writeups are
in `docs/historical/`. The full plan is at
`docs/historical/plan-phase6-unwrap-wrappers-and-partitioner.md`.

Side effect: also fixed a pre-existing latent bug in
`llvm_gen::push_arg_for_rust_call` where non-pair Direct(scalar) args were
passed by pointer instead of value, corrupting every `Vec::push(int)` call
since toylang's inception. The new helper dispatches per-arg on
`CoercedParam` (Direct/Pair/Indirect/Ignore) using the cached
`info.coerced_params`. Test `test_vec_pop_unwrap` exercises this end-to-end.

**Step 2 (DONE)**: the inline `__lang_stubs` partitioner check is now a
`visibility_override` callback on `LangCallbacks`. The rustc fork exposes
`rustc_monomorphize::partitioning::VISIBILITY_OVERRIDE_HOOK` (an
`OnceLock<fn ptr>`); `install_callbacks` registers a bridge fn that
forwards through the standard vtable + trampoline pattern; toylang's impl
walks `tcx.def_path(...).data` for `__lang_stubs` (per @DPSFDOZ — can't
use `is_from_lang_stubs` here because the partitioner runs outside
`generate_and_compile`). The string `__lang_stubs` no longer appears in
the rustc fork.

**Step 3 (DONE — done early via two-family trait split)**: `LangCallbacks`
is now `LangCallbacks: LangPredicates`. State-taking callbacks live on
`LangCallbacks`; pure callbacks (predicates and `visibility_override`)
live on `LangPredicates`. The vtable, trampolines, and bridge fns are
split correspondingly. Predicate trampolines have no `&mut state`
parameter, so predicate bridge fns are structurally lock-free —
`facade_visibility_override` no longer locks `MUTABLE_STATE`. The
"partitioner-time hooks may lock" exception in @GCMLZ is dissolved.

This delivered the family taxonomy (the original Phase 6 step 3 goal)
in the type system rather than as a prose convention. The remaining
sub-goals from the original step 3 plan — `toy_*` → `lang_*` rename
across the 5 query providers, accessor-wrapper audit — are not done
and are not blocking; they can be picked up as small followups when
convenient.

**Phase 6 complete.**

---

### Original step plan (kept for reference; replaced by what's above)

**Goal**: Verify `.unwrap()` works on Result and Option return values. This unlocks regex, clap, glob, reqwest.

`.unwrap()` is an inherent method on both `Result` and `Option` — it should already be found by `find_inherent_method`. The work here is verifying it works end-to-end and fixing any issues.

### Step 6.1: Test .unwrap() on Option

```rust
#[test]
fn test_unwrap_option() {
    // Vec::first() returns Option<&T>
    // or: some method that returns Option, call .unwrap()
}
```

### Step 6.2: Test .unwrap() on Result

```rust
#[test]
fn test_unwrap_result() {
    // str::parse::<i32>() returns Result<i32, ParseIntError>
    // Call .unwrap() on it
}
```

### Step 6.3: Likely issues

- `Result<T, E>` has 2 type args. When `rust_method_return_type` queries `fn_sig` for `unwrap`, it needs to instantiate with the correct generic args so the return type resolves to `T` not `E`.
- `Option<T>` has 1 type arg. Same concern.
- The method lookup needs to handle the type args correctly — `find_inherent_method` finds the DefId, then `get_or_resolve_rust_method` builds the Instance with the right args.

### Step 6.4: Tests

```rust
// Basic unwrap tests:
#[test] fn test_option_unwrap_returns_inner_type() { ... }
#[test] fn test_result_unwrap_returns_ok_type() { ... }
#[test] fn test_nested_unwrap() { /* foo().unwrap().bar().unwrap() */ }

// Method chaining with unwrap:
#[test] fn test_method_chain_with_unwrap() {
    // Regex::new("pattern").unwrap().is_match("text")
}

// Discarding vs using unwrap result:
#[test] fn test_unwrap_result_used() { /* let x = foo().unwrap(); use x */ }
#[test] fn test_unwrap_result_discarded() { /* foo().unwrap(); as ExprStmt */ }
```

---

## Phase 7: Standalone Test Projects — IN PROGRESS (7/9 done)

**Status**: uuid, indexmap, regex, toml, serde_json, glob, and rand
smoke tests landed and green. 2 crates remaining. Junior-engineer
handoff at `/Users/verdagon/erw/handoff.md` covers the batch
end-to-end.

**Goal**: Create test projects under
`toylangc/tests/standalone/<crate>_test/` proving toylang links
against and calls into arbitrary Rust crates from crates.io via
`toylangc build`.

### What landed (2026-04-15)

**`uuid_test`** — the smoke test bridging Phase 5 (cargo resolves
deps) to Phase 7 (toylang calls into deps). Program:

```
use uuid::Uuid
use std::io::stdout
use std::io::Stdout
use std::io::Write
use std::result::Result
use std::io::Error

fn main() {
    let id = Uuid::new_v4();
    Write::write_all(&stdout(), b"uuid ok\n");
}
```

Shipped in commit `df696c1` + follow-ups. Surfaced three latent
issues that all landed this week:

1. **Workspace nesting** — the generated `.toylang-build/Cargo.toml`
   now emits `[workspace]` so checked-in projects under cargo
   workspaces compile. Regression test:
   `test_build_inside_another_workspace`. See arch guide §10.5.
2. **@MBMRVZ — `fn main()` must return void.** Non-void tail
   expressions used to silently compile and SIGBUS at runtime;
   now a `TypeResolveError::MainMustReturnVoid` at type-resolve
   time. See `docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md`.
3. **@RTMEIZ — Rust types must be `use`-imported, even implicitly.**
   Trait-Self types, nested generics, etc. Previously panicked at
   `oracle.rs:112`; now a
   `TypeResolveError::RustTypeNotImported { name, context }` with a
   7-variant `RustTypeLookupContext` telling the user exactly which
   kind of usage triggered the lookup. See
   `docs/arcana/RustTypesMustBeExplicitlyImported-RTMEIZ.md`.

Both improvements shipped as tech-debt #26 and #27 in commit
`0b1432e`. `docs/usage/writing-main.md` has the practical checklist
for toylang authors.

### What landed (2026-04-16)

**`indexmap_test`** — second smoke test, chosen to exercise a
different shape from uuid: generic API with three explicit type args
(`IndexMap::new<i32, i32, RandomState>()`) at the construction site.
Program:

```
use indexmap::IndexMap
use std::collections::hash_map::RandomState
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let m = IndexMap::new<i32, i32, RandomState>();
    Write::write_all(&stdout(), b"indexmap ok\n");
}
```

Passed on first attempt — no changes required to `toylangc/src/`,
`rustc-lang-facade/`, or the rustc fork. The pre-execution risk
(indexmap's `new()` lives on an S-fixed impl block, not the open
`impl<K, V, S>`) dissolved under toylang's explicit-args philosophy:
supplying `RandomState` at the call site matched rustc's elided
default, and `Instance::expect_resolve` handled impl-block selection
with no toylang-side logic. 3-arg generic parsing also worked
first-try despite no prior test exercising it (Vec was the only 2-arg
precedent). Confirms Phase 7 is a mechanical expansion from here.

### What landed (2026-04-16, later) — string-literal `&str` ABI fix

Planning clap as the next Phase 7 smoke test surfaced a latent
compiler gap: toylang's `"..."` string literals could not be passed
to any Rust function taking `&str`. A minimal reproducer
(`test_string_literal_passed_to_rust_fn`) failed at type-resolve with
`ArgTypeMismatch { expected: Ref { inner: RustType "str" }, got: Str }`.

Root cause: `ResolvedType::Str` was a half-wired type predating Phase
3's `ByteSlice` template. Six compiler stages each made a different
assumption about what `Str` meant (unsized `str`, or `&str`, or
C-string `*const i8`), never aligned with each other or with rustc's
`TyKind::Str`.

Fix mirrored ByteSlice at every touchpoint. Documented in detail in
`docs/arcana/UnsizedTypesAppearInsideRef-UTAIRZ.md` with `@UTAIRZ`
references at 10 code sites across `typed_ast.rs`, `parser.rs`,
`type_resolve.rs`, `oracle.rs`, `stub_gen.rs`, and `llvm_gen.rs`.

Tests added: 6 lexer unit tests (`test_lex_string*`), 1 type-resolve
unit test (`test_resolve_string_lit`), 5 integration tests
(`test_string_literal_*`) including the reproducer. Also lexer
support for escape sequences (`\n \t \\ \0 \"`) in regular strings;
previously only byte strings supported them.

Unblocks 3 of 7 remaining Phase 7 smoke tests (regex, toml,
serde_json — all take `&str` directly). Does NOT unblock clap —
its `Command::new(impl Into<Str>)` has an orthogonal synthetic-generic
gap.

### What landed (2026-04-16, later still) — `@IVTDBTZ` dispatch + codegen fix

`regex_test` was the next Phase 7 smoke test attempted. Program:

```
use regex::Regex
use regex::Error
use std::result::Result
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let re = Regex::new("\\d+").unwrap();
    Write::write_all(&stdout(), b"regex ok\n");
}
```

Surfaced **two latent compiler gaps** that both had to land before the
test could pass:

1. **Dispatch gap (`type_resolve.rs:~487`):** the classifier for
   `Name::method(args)` used `!typed_args.is_empty() && !registry.structs.contains_key(ty)`
   as a proxy for "is this a trait call?". This misrouted every
   `RustStruct::method(arg)` with non-empty args to the trait path,
   ICE-ing at `oracle.rs:600` with `"trait 'Regex' not found"`. Every
   future `String::from(x)`, `Vec::with_capacity(n)`, `Box::new(x)`
   would have tripped it identically. Phase 1–6 didn't notice because
   every inherent static test was zero-arg (`Vec::new`, `Uuid::new_v4`,
   `IndexMap::new<K,V,S>`), short-circuiting the predicate.
2. **Codegen gap (`llvm_gen.rs:~1414`):** the inherent StaticCall
   branch hardcoded `build_call(func, &[])` — `// Inherent static
   call (e.g., Vec::new) — no args`. Every arg the upstream dispatcher
   passed was silently discarded. Same zero-arg short-circuit meant
   it lived undetected.

Fix shipped as `@IVTDBTZ` (Inherent Vs Trait Dispatch By Type). TL's
accepted approach:

- **6a (simplified):** Replace the arg-count heuristic with a new
  predicate callback `is_rust_trait(&str) -> bool` threaded through
  `resolve_fn_body` / `resolve_expr` / `resolve_stmt` alongside the
  existing `rust_method_ret` / `rust_param_types` callbacks. Backed
  by `oracle::find_use_imported_trait_def_id`. Three call sites build
  the closure (both in `callbacks_impl.rs`, one in `llvm_gen.rs`).
- **6b:** Convert `oracle.rs:600` and `:619` panics to
  `Err(UnresolvedRustType)` with two new `RustTypeLookupContext`
  variants — `TraitCallName { method }` (for trait name missing)
  and `TraitMethodName { trait_name }` (for method name typo on
  imported trait).
- **Sibling codegen fix:** rewrote the inherent StaticCall branch to
  mirror the trait-call branch above it — iterate `args` via
  `push_arg_for_rust_call` with `coerced_params[i]`, handle `sret`,
  `has_track_caller`, and `debug_assert_eq` on arg count.

Tests added: 6 new integration tests — `test_static_call_zero_args_is_inherent`
(regression guard for dispatch), `test_static_call_nonempty_args_rust_struct`
(positive test via `Vec::with_capacity(5usize)`, doubles as codegen
regression guard), `test_static_call_nonempty_args_trait` (trait
path regression via `Clone::clone(&v)`), and three compile-fail tests
covering structured errors: undefined type, typo'd trait name,
typo'd method name (the last exercises `TraitMethodName` directly).
Plus `test_standalone_regex` un-ignored.

Documented in detail at
`docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md` with `@IVTDBTZ`
markers at 8 files: `oracle.rs` (variants + panic sites),
`type_resolve.rs` (dispatch line), `callbacks_impl.rs` (both closure
sites), `llvm_gen.rs` (third call site + codegen fix),
`integration_tests.rs` + `standalone_tests.rs` (test headers). Cross-
referenced from `@RTMEIZ` and `@TVIMDGAZ`. Arch guide front-matter,
§10.7, and §11 arcana index updated. Bug report at
`bug-report-regex-static-call-misclassified-as-trait.md` preserved
as history.

Unblocks: `regex_test` (done), plus eases `clap_test` — its dispatch
would now work for `Command::new("x")` if the `impl Into<Str>`
synthetic-generic gap were resolved (orthogonal, still blocked).
Mechanical for `toml_test`, `serde_json_test`, `glob_test` (all
free-fn) and `rand_test`.

### What landed (2026-04-16, later still) — `toml_test`

**`toml_test`** — fourth Phase 7 smoke test. Program:

```
use toml::from_str
use toml::Value
use toml::de::Error
use std::result::Result
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let val = from_str<Value>("").unwrap();
    Write::write_all(&stdout(), b"toml ok\n");
}
```

Passed on first attempt — no changes required to `toylangc/src/`,
`rustc-lang-facade/`, or the rustc fork. First integration test of a
use-imported **generic free function with an explicit type arg**
(`name<T>(args)` shape). Prior tests covered `name()` (free fn, no
generics — `stdout()`) and `Name::method<T>(args)` (static method
with generics — `Vec::new<i32, Global>()`, `IndexMap::new<i32, i32,
RandomState>()`), but not the combination. Unit-tested indirectly via
`test_arg_type_mismatch_generic_fn`; no integration test had
exercised it end-to-end until now.

Composed six features in one 12-line program: Phase 5 (build),
Phase 2 (use-imported free fn), @UTAIRZ (`&str` ABI via string
literal), Phase 6 (unwrap wrapper on non-stdlib `Result<Value,
toml::de::Error>` — second `.unwrap()` test after regex to exercise
the non-stdlib Result path), Phase 4 (I/O via `Write::write_all`),
plus the new generic-free-fn shape.

First-try success confirms Phase 7's remaining crates
(`serde_json_test` especially — same shape with `serde_json::Value`
replacing `toml::Value`) should be mechanical.

### What landed (2026-04-16, later still) — `serde_json_test` + `@ELASZ`

**`serde_json_test`** — fifth Phase 7 smoke test. Program:

```
use serde_json::from_str
use serde_json::Value
use serde_json::Error
use std::result::Result
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let val = from_str<Value>("null").unwrap();
    Write::write_all(&stdout(), b"serde_json ok\n");
}
```

The "mechanical mirror of toml" prediction was wrong. serde_json's
`from_str` is `fn from_str<'a, T: Deserialize<'a>>(s: &'a str) -> Result<T>`
— `'a` is **early-bound** (appears in the `where T: Deserialize<'a>`
bound, so it lands in `generics_of`). toml's is
`fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, Error>` —
no lifetime parameter at all. The one-character difference from
toml's signature surfaced a latent compiler gap.

Every site in toylangc that built a `GenericArgs` from user type
args only — 10 sites across `oracle.rs`, `callbacks_impl.rs`, and
`llvm_gen.rs` — was hand-rolling the args, passing exactly the
type args the user supplied and silently truncating to
`generics_of(def_id).count()`. With only type parameters in the
wild, this happened to produce correct args. With an early-bound
lifetime in the generics list, `EarlyBinder::instantiate` expected
a region in slot 0 and found a type, so rustc ICEd:

```
expected region for `'a/#0` ('a/#0/0) but found Type(serde_json::Value)
when instantiating args=[serde_json::Value]
```

**Fix shipped as `@ELASZ`** (Early-bound Lifetime Args are
Synthesized):

- New helper `oracle::build_generic_args_for_item` using
  `ty::GenericArgs::for_item(tcx, def_id, |param, _| ...)`. Rustc
  drives the per-param walk; the callback fills `Lifetime` slots
  with `tcx.lifetimes.re_erased` (the post-borrowck placeholder
  used by rustc internally during monomorphization), consumes
  user-supplied types for `Type` slots in declaration order, and
  panics for `Const` slots (not yet needed).
- All 10 sites swapped from the hand-rolled `mk_args(&all_ty_args[..count.min(len)])`
  pattern to a single line calling the helper.
- `@ELASZ` markers at all 10 call sites plus the helper itself.
- Documented in detail at
  `docs/arcana/EarlyBoundLifetimeArgsSynthesized-ELASZ.md`.

Tests: no new unit test — unit tests mock the oracle callbacks, so
they can't exercise `build_generic_args_for_item` directly.
`test_standalone_serde_json` is the regression guard; it exercises
the full oracle → callbacks → llvm_gen → rustc pipeline end-to-end
with an early-bound lifetime.

Third instance of the "latent until the right crate shape surfaces
it" pattern after @UTAIRZ (first `&str`-accepting Rust fn) and
@IVTDBTZ (first inherent static call with args). Unblocks any
future Rust API with an early-bound lifetime — `serde_json::from_slice`,
`Visitor<'de>` impls, and anything with the shape
`fn foo<'a, T: SomeTrait<'a>>(...)`.

### What landed (2026-04-17) — `glob_test`

**`glob_test`** — sixth Phase 7 smoke test. Program:

```
use glob::glob
use glob::Paths
use glob::PatternError
use std::result::Result
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let result = glob("*.rs");
    Write::write_all(&stdout(), b"glob ok\n");
}
```

Passed on first attempt — no changes required to `toylangc/src/`,
`rustc-lang-facade/`, or the rustc fork. First Phase 7 test to bind
a `Result` without calling `.unwrap()` on it (the `Paths` iterator
is intentionally left unconsumed — first-pass scope discipline).
Composes four features in one 12-line program: Phase 5 (build),
Phase 2 (use-imported free fn `glob::glob`), @UTAIRZ (`&str` ABI via
string literal `"*.rs"`), and Phase 4 (I/O via `Write::write_all`).

Confirms the mechanical-completion prediction for the remaining
Phase 7 crates — the handoff's 90% case (three files, first-try
pass) held. Tests: 67 unit + 129 integration + 11 standalone = 207.

### What landed (2026-04-17, later) — `rand_test`

**`rand_test`** — seventh Phase 7 smoke test. Program:

```
use rand::thread_rng
use rand::rngs::ThreadRng
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let rng = thread_rng();
    Write::write_all(&stdout(), b"rand ok\n");
}
```

Passed on first attempt — no changes required to `toylangc/src/`,
`rustc-lang-facade/`, or the rustc fork. First Phase 7 test to bind
a non-Copy, non-Result Rust type (`ThreadRng`) as an unused
let-binding and let rustc's normal Drop-glue codegen run at
end-of-`main`. Pinned to `rand = "0.8"` (0.9 renamed `thread_rng` to
`rng`). Exercises the zero-arg use-imported free fn (`thread_rng()`)
returning an opaque Rust struct held by value — the Drop-dep risk
flagged in the handoff's Failure Class 5 did not fire, confirming
rustc's collector handles `drop_in_place::<ThreadRng>` via the same
code path that already worked for `Stdout` (used in `&stdout()`) in
earlier Phase 7 tests.

Two sequential first-try passes (glob then rand) in the same session
strongly suggest `reqwest_test` is similarly mechanical. Test totals:
67 unit + 129 integration + 12 standalone = 208, 0 failed, 0 ignored.
Phase 7 at 7/9; 2 crates remaining (clap still blocked on `impl
Into<Str>`, reqwest unblocked).

### What's remaining

2 crates, handed off to a junior engineer via `handoff.md`:

| Crate | Complexity | Notes |
|---|---|---|
| `clap_test` | Builder w/ `impl Into<Str>` | Still blocked on synthetic generics (orthogonal to IVTDBTZ) |
| `reqwest_test` | Free fn, needs `blocking` feature | No network call on smoke test |

Each project is three files (`toylang.toml`, `main.toylang`, plus one
test function appended to `toylangc/tests/standalone_tests.rs`). The
pattern is proven: match one of the seven landed smoke tests'
structure (uuid, indexmap, regex, toml, serde_json, glob, rand), let
the structured errors guide missing imports and syntax fixes.

Full Phase 7 completion target: 67 unit + 134 integration + 14
standalone = 215 tests, 0 failed, 0 ignored. (Currently 208: 67 unit
+ 129 integration + 12 standalone.)

### Original plan (historical)

The original Phase 7 plan split crates into Step 7.1 (I/O only) and
Step 7.2 (need `.unwrap()`), and pre-sketched sample programs for each
before syntax rules (`;` terminators, `Stdout` / `Result` / `Error`
implicit-import rules, non-turbofish generic syntax) were fully
worked out. Those samples are now superseded by the uuid example
above and the per-crate starting points in `handoff.md`. Original
plan preserved in git history (pre-commit `0b1432e`) if needed.

---

## Phase 8: Test Harness — PARTIALLY DONE (file exists, harness helper sketch deferred)

**Status**: The file `toylangc/tests/standalone_tests.rs` already
exists with 10 tests (4 build-mechanism + `test_build_inside_another_workspace`
+ `test_standalone_uuid` + `test_standalone_indexmap` +
`test_standalone_regex` + `test_standalone_toml` +
`test_standalone_serde_json`). Remaining harness polish (a
`run_standalone_test(name, expected)` helper so each crate is
one-liner `#[test] fn test_standalone_<crate>()`) is a nice-to-have
but deferred until the 6 remaining Phase 7 crates land — the helper
is easier to design once we have 14 concrete test functions to
deduplicate.

**Goal**: `cargo +rustc-fork test` builds and verifies all standalone test projects.

### Step 8.1: New test file

**File**: `toylangc/tests/standalone_tests.rs` (exists)

One test function per project. Pattern:
```rust
fn run_standalone_test(project_name: &str, expected_output: &str) {
    let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone").join(project_name);

    // Clean previous build
    let build_dir = project_dir.join(".toylang-build");
    if build_dir.exists() { std::fs::remove_dir_all(&build_dir).unwrap(); }

    // Build
    let build_output = Command::new(toylangc_bin())
        .arg("build")
        .current_dir(&project_dir)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run toylangc build");
    assert!(build_output.status.success(),
        "toylangc build failed for {}:\nstdout: {}\nstderr: {}",
        project_name,
        String::from_utf8_lossy(&build_output.stdout),
        String::from_utf8_lossy(&build_output.stderr));

    // Run
    let binary = build_dir.join("target/debug").join(project_name);
    let run_output = Command::new(&binary)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run binary");
    assert!(run_output.status.success(),
        "{} exited with error:\nstderr: {}",
        project_name, String::from_utf8_lossy(&run_output.stderr));
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(stdout.contains(expected_output),
        "expected '{}' in output of {}, got: {}", expected_output, project_name, stdout);
}

#[test] fn test_standalone_rand() { run_standalone_test("rand_test", "rand ok"); }
#[test] fn test_standalone_uuid() { run_standalone_test("uuid_test", "uuid ok"); }
#[test] fn test_standalone_indexmap() { run_standalone_test("indexmap_test", "indexmap ok"); }
#[test] fn test_standalone_regex() { run_standalone_test("regex_test", "regex ok"); }
#[test] fn test_standalone_clap() { run_standalone_test("clap_test", "clap ok"); }
#[test] fn test_standalone_glob() { run_standalone_test("glob_test", "glob ok"); }
#[test] fn test_standalone_reqwest() { run_standalone_test("reqwest_test", "reqwest ok"); }
#[test] fn test_standalone_toml() { run_standalone_test("toml_test", "toml ok"); }
#[test] fn test_standalone_serde_json() { run_standalone_test("serde_json_test", "serde_json ok"); }
```

### Step 8.2: .gitignore

Add `.toylang-build/` to root `.gitignore`.

---

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| `&[u8]` fat pointer ScalarPair ABI | Phase 3 codegen produces wrong args | Dedicated tests (Step 3.9). Check `coerced_param_types_for_instance` output. May need to split fat ptr into two args. |
| `tcx.all_traits()` API name | Phase 1 won't compile | **RESOLVED**: not needed; trait lookup uses `find_use_imported_trait_def_id`. |
| `Result<T, E>` in `rustc_ty_to_resolved_type` | Panics on complex nested types | Handle more `TyKind` variants. Map unknown ADTs to `RustType`. |
| `RUSTC_WORKSPACE_WRAPPER` args format | Phase 5 breaks | Test with `cargo build -v` to inspect exact args. |
| Generic type args on external types | Phase 7 tests fail on wrong arg count | Query `tcx.generics_of()` for required vs defaulted params. |
| Trait method finds wrong blanket impl | Phase 1 resolves wrong method | **RESOLVED**: `for_each_relevant_impl` takes first match; working in all tests. |
| `from_str` is a trait method (FromStr) | Phase 7 toml/serde tests need trait resolution | **RESOLVED**: `toml::from_str` is a free function. Works via Phase 2 `find_use_imported_fn_def_id`. |
| `StructRef` vs `Struct` in arg checking | Arg type checking false positives | **RESOLVED** (Phase 2): Added `types_match()` for semantic type equivalence. |
| MethodCall autoref for self param | Self arg type check fails (receiver lacks `&mut`) | **RESOLVED** (Phase 2): Skip self param in MethodCall checking (`param_types[i+1]`); toylang doesn't model autoref. |

---

## Verification

Follow the `CLAUDE.md` build-redirect convention: pipe to a fixed
file in `/tmp/` via `tee`, inspect as a separate command. Don't
chain `| grep` onto the same line.

```bash
# Full test suite:
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-quest.txt
grep "test result:" /tmp/erw-quest.txt
# Current expected: 67 unit + 129 integration + 12 standalone = 208 tests, 0 failed, 0 ignored
# Phase 7 target: 67 + 134 + 14 = 215 (2 more standalone tests to land)

# Just the standalone suite:
cargo +rustc-fork test -p toylangc --test standalone_tests 2>&1 | tee /tmp/erw-quest.txt
grep "test result:" /tmp/erw-quest.txt

# Manual verification of a built standalone project (uuid already done):
cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_uuid 2>&1 | tee /tmp/erw-quest.txt
# Binary lives at:
#   toylangc/tests/standalone/uuid_test/.toylang-build/target/debug/uuid_test
```

For junior engineers picking up the 6 remaining Phase 7 crates, the
authoritative guide is `/Users/verdagon/erw/handoff.md`.

## Key docs reference

- **Architecture**: `docs/architecture/rust-interop-guide.md`
- **Handoff (live)**: `handoff.md` — Phase 7 remaining 6 crates
- **Handoff (historical)**: `handoff-phase7-uuid.md` — the uuid
  smoke test as originally scoped; superseded but kept for context
  on what surfaced.
- **Writing toylang that uses Rust deps**: `docs/usage/writing-main.md`
- **Known tech debt**: `docs/architecture/known-tech-debt.md`
- **Arcana index**: end of `docs/architecture/rust-interop-guide.md` §11
