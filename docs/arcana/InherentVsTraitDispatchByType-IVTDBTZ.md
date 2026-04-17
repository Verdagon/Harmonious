# Inherent Vs Trait Dispatch By Type (IVTDBTZ)

Dispatch between inherent static calls (`RustStruct::method(args)`) and
trait static calls (`Trait::method(receiver, args)`) is **type-kind
based**, not argument-count based. A name is a trait iff
`find_use_imported_trait_def_id(tcx, name).is_some()`. Nothing else
(arg count, receiver presence, whether the name is in the toylang
registry) influences the classification.

## Where

- `toylangc/src/toylang/type_resolve.rs:~487` — `StaticCall` arm
  dispatches on `is_rust_trait(ty)`, a callback predicate.
- `toylangc/src/toylang/callbacks_impl.rs` (Check 5 in
  `after_rust_analysis`) and the `type_resolve_body` helper shared by
  the two walkers — build the `is_rust_trait` closure over
  `find_use_imported_trait_def_id`.
- `toylangc/src/llvm_gen.rs:~682` — third call site for
  `resolve_fn_body`, builds its own `is_rust_trait` closure over
  the same oracle helper.
- `toylangc/src/oracle.rs:~412` — `find_use_imported_trait_def_id`
  is the backing predicate.
- `toylangc/src/oracle.rs:~600` and `~619` — trait-path lookup
  failures return structured `UnresolvedRustType` with new
  `RustTypeLookupContext` variants `TraitCallName` and
  `TraitMethodName` (was two `panic!` sites).
- `toylangc/src/llvm_gen.rs:~1414` — inherent `StaticCall` codegen
  iterates args through `push_arg_for_rust_call` with the cached
  `coerced_params`. Previously this branch hardcoded
  `build_call(func, &[])` and silently discarded every arg.

## Cross-cutting effect

Every static-call shape is classified here. The correctness invariant
is: "would rustc resolve `Name::method(args)` to a trait or an
inherent method?" That's a purely source-level property of `Name`,
decidable from the `use` imports alone. The oracle knows; the type
resolver doesn't, so dispatch needs a callback.

Practically this was first discovered via `Regex::new("\\d+")`, which
misrouted to the trait path and ICEd. Every `RustStruct::method(arg)`
shape — `String::from(x)`, `Vec::with_capacity(n)`, `Box::new(x)`,
`PathBuf::from(s)` — would have tripped the old heuristic the same
way. The only reason Phase 1-6 didn't notice is that every inherent
static test was zero-arg (`Vec::new<T, A>()`, `Uuid::new_v4()`,
`IndexMap::new<K, V, S>()`), which short-circuited the old
`!typed_args.is_empty()` predicate into the inherent path by luck.

The sibling bug in `llvm_gen.rs` inherent StaticCall codegen had the
same root cause: every test with a non-trait, non-toylang-registry
type as the receiver of a static call happened to be zero-arg, so the
hardcoded `&[]` argument vector was invisible. Fixing dispatch
without also fixing codegen produced a clean SIGSEGV at the first
actual-arg call site. They're now both fixed.

## Why it exists

Before @IVTDBTZ:
- `type_resolve.rs` classified `Foo::method(args)` as a trait call iff
  `!typed_args.is_empty() && !registry.structs.contains_key(ty)`.
  The first conjunct was the bug — the classifier was answering "could
  this be a trait?" using arg count as a proxy for "does it have a
  receiver?", but an inherent static method on a Rust struct also has
  non-empty args.
- `oracle.rs:600` and `:619` panicked when the trait classification
  misfired, producing an ICE instead of a source-level error.
- `llvm_gen.rs:~1414` only handled zero-arg inherent static calls,
  silently discarding any args the dispatcher passed through.

The TL accepted a predicate-based classifier (returning `bool`) over
a full `RustTypeKind` enum on the grounds that: (1) the current
branches are two (trait vs inherent); (2) unknown names falling
through the inherent path produce an existing structured error from
`try_resolved_to_rustc_ty`; (3) promoting to an enum later is a
mechanical refactor if we ever grow a third case (e.g. a
language-intrinsic `dyn Trait` dispatch).

## See also

- @RTMEIZ — the structured-error playbook that trait-path lookup
  failures now follow.
- @TVIMDGAZ — trait vs impl method DefId. Once dispatch correctly
  routes to the trait path, this arcana governs which DefId to use.
- @UTAIRZ — `&str` ScalarPair ABI. Regex's crash surfaced when a
  string literal (ScalarPair fat pointer) was passed as the first
  real-arg to an inherent static call — both the dispatch fix and
  the codegen arg-iteration fix had to land for that flow to work.
