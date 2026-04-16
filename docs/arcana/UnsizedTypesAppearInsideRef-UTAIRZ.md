# Unsized Types Appear Inside Ref (UTAIRZ)

`ResolvedType::Str` and `ResolvedType::ByteSlice` represent the unsized
Rust primitives `str` and `[u8]`. They are never valid bare — they only
ever appear as the inner of `ResolvedType::Ref`. A bare form at any
stage past the parser is a bug, and the two variants must be wired
identically through six compiler stages. Adding a third unsized type
(e.g., `CStr`, `dyn Trait`, `[T]` for arbitrary `T`) requires replicating
the same six-touchpoint pattern — missing one site produces the
half-wired state documented in the 2026-04-16 `test_string_literal_passed_to_rust_fn`
reproducer.

## Where

The pattern spans six sites. Str and ByteSlice are mirror images at
each one:

| Stage | `Str` | `ByteSlice` |
|---|---|---|
| Variant declaration | `toylangc/src/toylang/typed_ast.rs` — `ResolvedType::Str` | same file — `ResolvedType::ByteSlice` |
| Parser type syntax | `parser.rs::parse_type` — `str` keyword arm | same fn — `[u8]` bracket arm |
| Literal typing | `type_resolve.rs` — `"..." → Ref { Str }` | same file — `b"..." → Ref { ByteSlice }` |
| Oracle forward map | `oracle.rs::try_resolved_to_rustc_ty` — `Str → tcx.types.str_` | same fn — `ByteSlice → new_slice(u8)` |
| Oracle reverse map | `oracle.rs::rustc_ty_to_resolved_type` — `TyKind::Str → Str` | same fn — `TyKind::Slice(u8) → ByteSlice` |
| Stub rendering | `stub_gen.rs::resolved_type_to_syn` — `Str → str` | same fn — `ByteSlice → [u8]` |
| LLVM codegen | `llvm_gen.rs::resolved_to_inkwell` — bare `Str` panics; `Ref { Str }` → `{ ptr, i64 }`. Literal codegen in same file emits global byte array + fat pointer. | same fn — identical pattern for `ByteSlice` / `Ref { ByteSlice }` |

Additional derived sites (mangled names, equality matching) fall out
of the above automatically via the existing `Ref` and `RustType` arms
and do not need per-type wiring.

## Cross-cutting effect

If any of the six stages disagrees with the others, the type-system
pipeline produces silent mismatches rather than diagnosable errors:

- **Literal typed as bare Str + parser produces Ref { RustType "str" }**:
  `types_match` falls to `_ => a == b` → `ArgTypeMismatch` at
  validation. This was the state before 2026-04-16 — test
  `test_string_literal_passed_to_rust_fn` surfaced it.
- **Oracle forward panics on Str**: any code path that round-trips a
  string literal through rustc's ABI machinery crashes during codegen.
- **LLVM codegen emits a single pointer for bare Str**: `push_arg_for_rust_call`'s
  `CoercedParam::Pair` arm calls `into_struct_value()` on the value,
  which panics because a pointer isn't a struct. This path only fires
  when the earlier type-resolve stage also lets the bare form through —
  the half-wired state compounds.
- **`build_global_string_ptr` (null-terminated C string) used for
  string literals**: The pointer is valid, but the length encoded in
  the fat pointer at the call site is wrong (reads from uninitialized
  memory or off-by-one). Rust code receiving the `&str` sees a
  truncated or over-long slice. Corruption is silent until bytes are
  read.

The inverse failure mode — bare `Ref { Str }` reaching codegen without
being wrapped — is caught by an explicit `panic!("bare Str should not
appear without Ref wrapper")` in `resolved_to_inkwell`. That panic is
belt-and-suspenders: all producers type literals as `Ref { Str }`, so
the bare form cannot reach codegen through normal paths.

## Why it exists

Rust's `str` and `[u8]` are `?Sized` — their size is not known at
compile time, so they cannot live as bare values in memory or registers.
Rust's ABI handles them exclusively as ScalarPair references:
`&str` is `{ ptr, usize }`, `&[u8]` is `{ ptr, usize }`. The LLVM
representation is a two-field struct; the `CoercedParam::Pair` ABI
variant splits it into two LLVM function params at call sites.

Toylang's type system mirrors this: the bare unsized type has no LLVM
representation (no size, no register class), so codegen asserts it's
always wrapped. The sized form is `Ref { Unsized }`, which has a
concrete `{ ptr, i64 }` layout. This keeps toylang's ResolvedType in
one-to-one correspondence with rustc's ABI — no lookahead, no implicit
conversion, no "maybe a pointer, maybe a pair" ambiguity.

The alternative — representing `&str` as a single pointer and carrying
the length out-of-band — was the pre-Phase 3 state for byte strings
and caused the latent ScalarPair ABI bug that Phase 3 fixed. The bug
was invisible for as long as no code exercised `&[u8]` arguments; the
same trap applied to `&str` until 2026-04-16.

## Adding a new unsized type (checklist)

To introduce `Foo` as a new unsized type (e.g., a new primitive or
`[T]`-style slice), replicate at exactly these six sites, using
`ByteSlice` as the template:

1. **`typed_ast.rs`** — add variant with a doc comment stating
   "always appears inside `Ref` as `&Foo`".
2. **`parser.rs::parse_type`** — add syntax recognition (keyword arm
   or bracket-form arm).
3. **`type_resolve.rs`** — type the corresponding literal (if any) as
   `Ref { Foo }`, not bare `Foo`.
4. **`oracle.rs`** — both directions: `Foo → tcx rustc type` and
   `rustc TyKind → Foo`.
5. **`stub_gen.rs`** — render as bare Rust type (e.g., `Foo`). The
   `Ref` wrapper at the caller adds the `&`.
6. **`llvm_gen.rs`** — bare variant panics; extend the `matches!`
   guard on the fat-pointer arm to include `ResolvedType::Foo`. If
   `Foo`'s fat pointer has a different shape than `{ ptr, i64 }`,
   write a new arm instead of extending.

Then add: lexer tests for any literal syntax, a `test_resolve_foo_lit`
unit test asserting `Ref { Foo }`, and an integration test passing a
literal to a Rust fn taking `&Foo` (mirror
`test_string_literal_passed_to_rust_fn` / `test_byte_string_passed_to_rust_fn`).
