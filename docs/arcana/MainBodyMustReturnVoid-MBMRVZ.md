# Main Body Must Return Void (MBMRVZ)

Toylang's `fn main()` body must have a void-typed tail expression.
Violating this rule compiles and links silently, runs, executes all
side effects, then SIGBUSes during teardown. The cross-cutting effect
comes from an ABI mismatch between the two forms of main that toylang
generates.

## Where

- `toylangc/src/llvm_gen.rs::codegen_internal_function` — where the
  internal main's return type is inferred from its body.
- `toylangc/src/llvm_gen.rs::codegen_extern_wrapper` — where the
  Rust-ABI extern wrapper (`__toylang_impl_main` / `__toylang_main`)
  is built with a fixed signature.
- `toylangc/src/stub_gen.rs` — where the `__toylang_main` stub is
  emitted, declaring `fn __toylang_main()` with a void return.
- `toylangc/src/toylang/parser.rs::parse_fn_body` — the parser branch
  that permits an implicit unit tail when a block ends with `;` before
  `}`.

## Cross-cutting effect

Every entry-point toylang function generates two LLVM functions: an
internal ABI form (`__toylang_internal_main`) and an extern Rust-ABI
wrapper (`__toylang_impl_main`). The internal form's return type is
inferred from the body's tail expression. The extern wrapper for main
is always declared `fn __toylang_main()` (void) because the
auto-generated Rust shim does `fn main() { __toylang_main(); }` and
expects no return value.

When main's tail expression is non-void (e.g., `Write::write_all(...)`
which returns `Result<(), Error>`), the internal form grows an sret
return: its first parameter becomes a `*mut Result<(), Error>` buffer
that it will fill before returning. But the extern wrapper doesn't
know about this — it calls `__toylang_internal_main()` with no sret
argument, leaving the sret register (x8 on aarch64, rdi on x86-64)
pointing wherever it was previously. Depending on caller-saved
register state, this often ends up pointing into the binary's text or
const segment.

The internal body runs, executes its side effects (I/O, heap
allocations, visible output), then at the end does its final
`str xN, [xM]` to write the Result value into the sret buffer. The
store targets a read-only page. SIGBUS (macOS/aarch64) or SIGSEGV
(Linux) at `address=<low-address-in-text-segment>` with
`code=2 (write protection)`.

Symptom profile:

- Binary prints expected output (side effects complete).
- Exit status is 128+signal (138 for SIGBUS on macOS, 139 for SIGSEGV
  on Linux).
- `stderr` is empty — no panic, no diagnostic, no Rust backtrace.
- Crash site is inside `__toylang_internal_main`, instruction is a
  store to a low-memory (text segment) address.

The fix is always in toylang source, not the compiler: terminate the
last statement with `;` so main's tail is implicit unit, or use an
explicit void-typed tail expression.

```
fn main() {
    let id = Uuid::new_v4();
    Write::write_all(&stdout(), b"hi\n");   // <-- trailing ; makes this void
}
```

`toylangc/src/toylang/parser.rs::parse_fn_body` permits this via the
`ret: None` branch (lines 442-444): a block that ends with `}`
immediately after a `;`-terminated statement has no tail expression
and the block's type is unit.

## Why it exists

Toylang infers function return types from body tails — it has no
explicit return annotations. The extern wrapper for main, by contrast,
has a fixed Rust-compatible signature because it's called from a
hand-written Rust shim (`src/main.rs`) that predates any user code.
The compiler could in principle reject non-void main tails at
type-resolve time (see known-tech-debt for that entry), but today the
burden is on the author.

Every other entry-point function is fine: its extern wrapper's
signature is derived from the same body whose type the internal form
uses, so the two stay in sync. Main is special because its external
ABI is pinned by Rust's `fn main() -> ()` convention.

The `fn main()` rule is the same as Rust's: if the last statement has
a non-unit type, terminate it with `;` to discard. Rust's compiler
catches this at typecheck time ("expected `()`, found `...`"); toylang
doesn't.

## See also

- `docs/usage/writing-main.md` — how to write a correct `fn main()`
  in toylang.
- `docs/arcana/ABICoercedReturnTypeInFunctionDeclarations-ACRTFDZ.md`
  — the related concern that Rust-facing function declarations must
  use ABI-coerced return types, not toylang types.
- `docs/architecture/known-tech-debt.md` — pending compiler-level
  check to reject non-void main tails at type-resolve time.
