# Rust Types Must Be Explicitly Imported (RTMEIZ)

Every Rust type that flows through toylang's type system — even
transitively, even if the toylang source never names it — must be
`use`-imported in the toylang source. Otherwise `find_rust_type_def_id`
returns `None` and the compiler surfaces either a structured
`RustTypeNotImported` error (from `after_rust_analysis`, the common
case) or a codegen-time panic whose message names the missing type
(rare edge case). The surprise is that the set of types a program
"uses" per toylang's resolver is wider than the set of types its
source mentions by name.

## Where

- `toylangc/src/oracle.rs::try_resolved_to_rustc_ty` — the conversion
  that returns `Result<ty::Ty, UnresolvedRustType>` on missing types.
  Context-aware: reports *why* the type was needed (trait Self, type
  arg, nested generic, etc.) so the error message is actionable.
- `toylangc/src/oracle.rs::resolved_to_rustc_ty` — the panicking
  convenience wrapper for codegen call sites where type resolution
  already passed; panics with the same actionable message if reached.
- `toylangc/src/oracle.rs::find_rust_type_def_id` — the lookup that
  only searches `pub use` re-exports in `__lang_stubs`.
- `toylangc/src/oracle.rs::rust_trait_method_return_type` — one of
  the callers that traffics in types the user never names (the Self
  type of a trait method call). Propagates the `UnresolvedRustType`
  error to `after_rust_analysis`.
- `toylangc/src/toylang/type_resolve.rs::TypeResolveError::RustTypeNotImported`
  — the structured error variant that `after_rust_analysis` surfaces.
  Carries `{ name, context }` where context is the human-readable
  explanation.
- `toylangc/src/stub_gen.rs` — where `pub use` re-exports are emitted
  into `__lang_stubs.rs` based on the toylang source's `use` imports.

## Cross-cutting effect

`find_rust_type_def_id` enumerates `tcx.module_children_local` looking
for `pub use` re-exports in `__lang_stubs`. The stub generator emits
one `pub use <path>;` per toylang `use` statement. That's the entire
type registry — if a type isn't re-exported, it isn't findable.

Types a toylang program uses without naming:

1. **`Self` of a trait method call.** `Write::write_all(&stdout(), ...)`
   — the toylang source names `Write` (the trait), `stdout` (the free
   function), and passes a `&[u8]` literal. It never names `Stdout`
   (the type of `stdout()`'s return). But `rust_trait_method_return_type`
   needs the rustc `ty::Ty` for `Self = Stdout` to instantiate
   `write_all`'s signature — so it calls `resolved_to_rustc_ty(Stdout)`
   → panic if `use std::io::Stdout` was omitted.

2. **Tail-expression return types (even when discarded).** A statement
   `Write::write_all(...);` with trailing `;` has its return type
   computed for consistency. That type is `Result<(), Error>`. Codegen
   for the `ExprStmt` allocates a local sret buffer, which requires
   computing the LLVM type for `Result<(), Error>` — which requires
   `resolved_to_rustc_ty(Result<(), Error>)` — which requires
   `use std::result::Result`. The nested `Error` in turn needs
   `use std::io::Error` because it appears as a generic type arg.

3. **Intermediate types in method chains.** `vec.pop().unwrap()` — if
   `pop()` returns `Option<T>`, the type resolver traffics in `Option`
   between the two calls, even though the source never writes
   `Option`. `use std::option::Option` is required.

4. **Error types inside `Result<_, E>`.** When `E` is itself a Rust
   type (not a primitive), it must be imported. For I/O code this
   means `use std::io::Error` alongside `use std::result::Result`.

The common failure mode is a structured compile error batched with
any other validation errors in `after_rust_analysis`:

```
[toylang] validation failed with 1 error(s):
  - function 'main': RustTypeNotImported { name: "Stdout",
      context: "as Self of trait call `Write::write_all`" }
[toylang] aborting due to validation errors
```

The `context` field names the variant of `RustTypeLookupContext` that
triggered the lookup — `TraitCallSelf`, `TraitMethodTypeArg`,
`InherentMethodTypeArg`, `FreeFunctionTypeArg`, `NestedGenericArg`,
`StructField`, or `Codegen`. Each renders via a `Display` impl into a
human-readable suffix (e.g., `"as Self of trait call \`Write::write_all\`"`).

For codegen-path lookups that slip through validation (rare), the
panicking wrapper reports the same message via `panic!`:

```
thread 'rustc' panicked at .../oracle.rs:...
  Rust type `<name>` is not imported (used during codegen).
  Add `use <path>::<name>` at the top of your source.
```

Either way, the message names the type and the context. The fix is
always in user source: add the missing `use` line.

## Why it exists

Toylang has no type inference and no implicit registration. The type
system works by: (1) toylang source names a Rust type via `use`,
(2) stub_gen emits `pub use` into `__lang_stubs`, (3) rustc's type
checker finds the re-export, (4) the oracle's `find_rust_type_def_id`
resolves name → DefId via `module_children_local`.

The registry is flat and keyed by type name, not by DefPath. There's
no path to "find this type by its full module path without an import"
— toylang only looks at what the user imported.

Auto-registration (caching types encountered during
`rustc_ty_to_resolved_type`) was considered and explicitly rejected:
toylang is an explicit language, and silently registering types by
resolver traversal order would create order-dependent name collisions
and undermine the import story. The structured error approach is the
final fix — it keeps the explicit-import contract while making the
failure mode actionable instead of an ICE.

When a toylang program uses a Rust crate (including `std`), trace
every type that flows through a trait call's `Self`, any tail
expression's return type, any nested generic's args, and
`use`-import each one at the top of the file. The integration
tests in `toylangc/tests/integration_tests.rs` (see
`test_write_all_result_bound`) model the pattern exactly.

## See also

- `docs/usage/writing-main.md` — the practical checklist for toylang
  authors, including which types need importing for common patterns.
- `docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md` — the related
  `fn main()` requirement. A tail expression's return type is subject
  to this rule even when the tail has `;` (discarded but still typed).
- `docs/architecture/known-tech-debt.md` — resolved entry #26 for the
  structured-error conversion; auto-registration was rejected.
- `docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md` — extended
  the `RustTypeLookupContext` enum with `TraitCallName` and
  `TraitMethodName` variants when trait-path lookup failures were
  converted from `panic!` to `UnresolvedRustType`, per the @RTMEIZ
  playbook.
