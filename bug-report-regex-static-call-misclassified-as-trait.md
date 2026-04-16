# Bug: `RustStruct::method(args, ...)` mis-dispatches as a trait call

**Status:** Open, blocks Phase 7 `regex_test`. Likely blocks ≥3 other
remaining Phase 7 crates.
**Surfaced by:** Attempting the `regex_test` smoke test described in
`handoff-regex.md`.
**Severity:** High for Phase 7 progress. Any Rust struct's static
constructor that takes arguments is currently broken.
**Reproduction time:** ~30 seconds once toolchain is warm.

---

## 1. Symptom

Toylangc panics during type-resolve (`after_rust_analysis` callback)
with:

```
thread 'rustc' panicked at toylangc/src/oracle.rs:600:28:
trait 'Regex' not found
```

Reported to the user as a cargo ICE. Not a `TypeResolveError` — a raw
`panic!`, so no structured error message.

## 2. Minimal reproducer

Three files (already present on `main`, test is `#[ignore]`d pending this
fix):

- `toylangc/tests/standalone/regex_test/toylang.toml`
- `toylangc/tests/standalone/regex_test/main.toylang`
- `test_standalone_regex` in `toylangc/tests/standalone_tests.rs`

Reproduce:

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_regex -- --ignored 2>&1 | tee /tmp/erw-regex-test.txt
grep -A1 "panicked at" /tmp/erw-regex-test.txt
```

The offending source line in `main.toylang`:

```
let re = Regex::new("\\d+").unwrap();
```

The panic reproduces even if the `.unwrap()` is removed — the minimum
trigger is `Regex::new("\\d+");` (any non-empty arg list).

## 3. Root cause

`toylangc/src/toylang/type_resolve.rs:481-484`, `StaticCall` arm:

```rust
let is_trait_call = !typed_args.is_empty() && {
    // If `ty` is not a known struct/type, it might be a trait
    !registry.structs.contains_key(ty.as_str())
};
```

The heuristic classifies `Foo::method(args)` as a **trait call** iff:

- `typed_args` is non-empty, **AND**
- `Foo` is not in the toylang registry's `structs` map.

Both conjuncts hold for `Regex::new("\\d+")` — `Regex` is a Rust struct
imported via `use regex::Regex`, not a toylang struct. So `is_trait_call`
becomes `true`, and the code builds `__trait::Regex` and calls
`rust_trait_method_return_type`, which in turn calls
`find_use_imported_trait_def_id(tcx, "Regex")`, which returns `None`,
which hits the `unwrap_or_else(|| panic!(...))` at **`oracle.rs:600`**.

The heuristic has no notion of "Rust struct" vs "Rust trait" — it
conflates "not a toylang struct" with "must be a trait".

## 4. Why this wasn't caught earlier

Phase 1 (trait calls) shipped with `Trait::method(recv, args)` as the
motivating case — non-empty args were always trait calls in the test
corpus. Phase 2–6 added static calls on Rust types (`Vec::new<T, A>()`,
`Uuid::new_v4()`, `IndexMap::new<K, V, S>()`) but **every one of them
is zero-arg**. Zero args short-circuits at
`!typed_args.is_empty()` → `is_trait_call = false` → the inherent
static-call path is taken and everything works.

`regex_test` is the first smoke test to exercise
`RustStruct::method(arg)` — non-toylang type, non-empty args — so it's
the first to trip the heuristic. This is precisely the class of gap the
handoff was designed to surface.

## 5. Scope impact

Affects every Rust struct inherent static method that takes arguments.
Concrete Phase 7 crates blocked or affected:

| Crate | Call shape | Affected? |
|---|---|---|
| regex | `Regex::new("\\d+")` | **YES — blocks** |
| toml | `toml::from_str<Value>("...")` | Free fn path, different dispatch — probably unaffected |
| serde_json | `serde_json::from_str<Value>("...")` | Same as toml — probably unaffected |
| glob | `glob::glob("*.txt")` | Free fn — unaffected |
| clap | `Command::new("app")` | **YES — blocks** (orthogonally also blocked on `impl Into<Str>`) |
| reqwest | `reqwest::blocking::get(url)` | Free fn — unaffected |
| rand | `thread_rng()` | Free fn — unaffected |

So at least **regex** and **clap** are blocked on this specifically.
Confirm by manually testing `Command::new("x");` in a minimal program
once a fix candidate lands.

Orthogonally: any future test case using `Vec::from([...])`,
`String::from("x")`, `PathBuf::from("/")`, `HashMap::from_iter(...)`,
`Box::new(x)`, etc. would trip the same heuristic.

## 6. Suggested fix direction (for the TL to own / delegate)

Two changes that should land together:

### 6a. Distinguish Rust struct from Rust trait in the heuristic

`type_resolve.rs:481` needs to consult the oracle for whether `ty` names
a Rust struct/enum (reexported or otherwise) vs a Rust trait vs neither.
`oracle::find_reexported_type` already distinguishes `DefKind::Struct` /
`DefKind::Enum` from `DefKind::Trait` (per Phase 4's
`find_reexported_type` fix — see @RTMEIZ and commit history around enum
support).

Proposed dispatch table:

| `ty` resolves to | Dispatch |
|---|---|
| toylang struct | Inherent static call (existing non-trait path) |
| Rust struct / enum | Inherent static call (existing non-trait path) |
| Rust trait | Trait call (existing `__trait::` path) |
| Unresolved | Structured `TypeResolveError::UndefinedType { name }` |

The `!typed_args.is_empty()` heuristic should be deleted. Trait-vs-
inherent is a property of what `ty` *is*, not of how many args it
takes.

### 6b. Convert the panic at `oracle.rs:600` to a structured error

Per the @RTMEIZ playbook, `find_use_imported_trait_def_id` returning
`None` should produce a `TypeResolveError::RustTypeNotImported { name,
context: RustTypeLookupContext::TraitCallName { .. } }` — add the
`TraitCallName` variant (currently there's `TraitCallSelf` only, at
`oracle.rs:605`). Return `Err` from `rust_trait_method_return_type`
instead of panicking. The panic at line 600 and the sibling panic at
line 619 (`method '{}' not defined on trait '{}'`) both fall under the
same pattern.

Even after fix 6a lands, 6b is belt-and-suspenders — if the user writes
`NonExistentTrait::foo(x)` they should get a readable error, not an ICE.

## 7. Test plan for the fix

Once landed:

1. Remove `#[ignore]` from `test_standalone_regex`; the test should pass.
2. Add a compile-fail unit test: `UndefinedThing::method(42)` →
   `TypeResolveError::UndefinedType` (not a panic).
3. Add a passing unit test with a `RustStruct::method(arg)` shape that
   doesn't require a crates.io dep — e.g. `String::from("x")` or a
   synthetic struct in an existing test fixture.
4. Add one for the trait case to prove that path still works:
   `Clone::clone(&v)` (already in `test_trait_static_call_clone_vec`).

## 8. Files touched (expected)

- `toylangc/src/toylang/type_resolve.rs` — heuristic at line 481
  (primary fix)
- `toylangc/src/oracle.rs` — lines 600 and 619 (panic → structured
  error), possibly an added variant on `RustTypeLookupContext`
- `toylangc/src/toylang/callbacks_impl.rs:222-232` — may need to handle
  a new error variant if one is added
- `toylangc/tests/integration_tests.rs` — new unit tests from §7
- `toylangc/tests/standalone_tests.rs` — remove `#[ignore]` on
  `test_standalone_regex`

Estimated effort: **4–8 hours** (including the write-up of a new
arcana file since this is a cross-cutting dispatch issue worth
documenting alongside @RTMEIZ, @TVIMDGAZ, @UTAIRZ).

## 9. Full stack trace (for reference)

```
thread 'rustc' panicked at toylangc/src/oracle.rs:600:28:
trait 'Regex' not found
stack backtrace:
   0: _rust_begin_unwind
   1: core::panicking::panic_fmt
   2: toylangc::oracle::rust_trait_method_return_type::{{closure}}
        at toylangc/src/oracle.rs:600:28
   3: core::option::Option::unwrap_or_else
   4: toylangc::oracle::rust_trait_method_return_type
        at toylangc/src/oracle.rs:599:24
   5: <ToylangCallbacks as LangCallbacks>::after_rust_analysis::{{closure}}
        at toylangc/src/toylang/callbacks_impl.rs:229:21
   6: toylangc::toylang::type_resolve::resolve_expr
        at toylangc/src/toylang/type_resolve.rs:494:21   # <-- is_trait_call=true path
   7: toylangc::toylang::type_resolve::resolve_expr
        at toylangc/src/toylang/type_resolve.rs:549:30   # MethodCall recursing into .unwrap()'s receiver
   8: toylangc::toylang::type_resolve::resolve_stmt
        at toylangc/src/toylang/type_resolve.rs:690:30
   9: toylangc::toylang::type_resolve::resolve_fn_body
  ...
```

Full trace in `/tmp/erw-regex-test.txt` (regenerate on demand).

## 10. Handoff pointers

- Reproducer checked in at `toylangc/tests/standalone/regex_test/`
  (`#[ignore]`'d test — un-ignore once fixed).
- Handoff doc: `handoff-regex.md` at repo root — describes the intended
  shape of the test and the failure classes it *was* expected to hit
  (this class wasn't among them; §6 of that doc can be amended once
  this fix lands).
- Relevant arcana: `@RTMEIZ` (structured errors for missing types),
  `@TVIMDGAZ` (trait vs impl method DefId — context for why the
  trait path exists at all).
- Quest plan: `quest.md` — Phase 7 §"What's remaining" expects regex
  unblocked; this bug is the blocker.
