# Handoff: Phase 7 crate #4 — `toml` standalone smoke test

> **Status: LANDED 2026-04-16. Preserved as historical context.**
>
> **Outcome:** First-try green with zero iteration, zero compiler
> changes, ~12 lines of toylang source exactly as the handoff's §3
> starting point predicted. Bumped tests 204 → 205 and Phase 7
> 3/9 → 4/9. Composed all six features the handoff listed in §2:
> Phase 5 build, Phase 2 use-imported free fn, @UTAIRZ `&str` ABI,
> Phase 6 unwrap wrapper on a non-stdlib `Result`, Phase 4 I/O,
> and the new-to-integration-tests shape of
> `name<T>(args)` — a generic free function with an explicit type
> arg on a use-imported name.
>
> **Prediction accuracy:** the 90% path. §6's Class 0 ("generic
> free-fn dispatch gap") was the predicted risk — didn't fire. The
> oracle's `rust_free_fn_return_type` handled the non-empty
> `type_args` slice correctly on its first real call; no
> end-to-end integration exerciser had hit that code path before,
> but the infrastructure worked.
>
> **The surprise came one test later.** `serde_json_test`, planned
> as a "mechanical mirror of toml," surfaced a *different* Class 0
> gap: `serde_json::from_str<'a, T: Deserialize<'a>>` has an
> early-bound lifetime parameter (`'a` in the `where T:
> Deserialize<'a>` bound), and toylang's ten
> `GenericArgs`-building sites across `oracle.rs`,
> `callbacks_impl.rs`, and `llvm_gen.rs` all hand-built the args
> from user type args only — dropping lifetime slots. Fix landed
> as `@ELASZ` (Early-bound Lifetime Args Synthesized). The
> "third instance of latent-until-the-right-crate-shape"
> pattern after `@UTAIRZ` (first `&str`-accepting Rust fn) and
> `@IVTDBTZ` (first inherent static call with args). Documented
> in detail at
> `docs/arcana/EarlyBoundLifetimeArgsSynthesized-ELASZ.md`. The
> same helper that synthesizes lifetime slots also silently
> truncates extras beyond the item's `Type` slots — load-bearing
> for toylang's call-site naming convention, a latent soundness
> hole tracked as tech debt #28 and documented as `@ETASTZ`.
>
> **Lesson carried forward:** when writing the next
> "mechanical mirror" handoff (likely serde_json's or any future
> same-shape test), the `fn from_str<'a, T>` vs
> `fn from_str<T: DeserializeOwned>` distinction deserves a
> dedicated risk-register entry. A one-character difference in
> the Rust signature (the explicit `'a`) turned a predicted
> mechanical test into a compiler fix.
>
> Original handoff body preserved below verbatim for historical
> reference.
>
> ---

**Audience:** Junior engineer picking up Phase 7. Comfortable reading
Rust and willing to read compiler errors carefully. No prior
compiler-internals experience required.

**Estimated effort:** 2–6 hours. Most of that is reading and
iterating on import lines. The code is ~12 lines.

**Prerequisites:** You should have finished the reading list in
`handoff.md` at the repo root, or at minimum read the first 8 entries
there (project-wide `CLAUDE.md`, `quest.md`, the architecture guide
up through Part 4, `docs/usage/writing-main.md`, @MBMRVZ, @RTMEIZ,
@UTAIRZ, @IVTDBTZ). If you haven't done that yet, go do it now —
this doc will be confusing without the background, and the confusion
will cost more time than the reading would.

**Scope:** One crate (`toml`). Three files. **Not six.** This doc
is narrow by design so surprises get focused attention. The general
6-crate handoff at `handoff.md` covers the wider batch; this doc
covers the one you're doing right now.

---

## 1. Where we are

Phase 7 is the "prove toylang can link against arbitrary crates.io
Rust crates" phase. Three smoke tests have landed:

- **`uuid_test`** (2026-04-15, commit `df696c1` + follow-ups) —
  simplest possible shape: zero-arg static method returning a
  trivially-Copy value. Exercised the build orchestration (Phase 5)
  end-to-end and surfaced @MBMRVZ, @RTMEIZ, and the workspace-nesting
  fix.
- **`indexmap_test`** (2026-04-16) — static method with three explicit
  generic type args (`IndexMap::new<i32, i32, RandomState>()`).
  Exercised 3-arg generics and proved S-fixed-impl-block selection
  works when the call site supplies the defaulted type arg
  explicitly.
- **`regex_test`** (2026-04-16) — static method **with args** plus
  `.unwrap()` on a non-stdlib `Result<T, E>`. Surfaced and fixed two
  latent compiler gaps: the trait-vs-inherent dispatch classifier
  (`type_resolve.rs:~487`) and the inherent StaticCall codegen that
  hardcoded `build_call(func, &[])` (`llvm_gen.rs:~1414`). Documented
  as **@IVTDBTZ** (`InherentVsTraitDispatchByType`) with 8 code-site
  references across oracle / type_resolve / callbacks_impl /
  llvm_gen / the tests. Read it — it's the most recent wider change
  and the one most likely to have re-shaped a code path you'll
  touch.

Current state: **204 tests passing** (67 unit + 129 integration + 8
standalone). Clean tree. Ready for the next smoke test.

## 2. What you're proving

Your job: add a **`toml_test`** standalone smoke test proving toylang
can compile and link a program that:

1. Depends on the `toml` crate from crates.io (Phase 5's manifest
   machinery).
2. Calls `toml::from_str<Value>(&str)` — a **generic free function**
   returning `Result<Value, toml::de::Error>`.
3. Calls `.unwrap()` on the returned Result — second test after regex
   to exercise Phase 6's inline-never unwrap wrapper on a non-stdlib
   Result.
4. Prints `"toml ok\n"` and exits zero.

This single program exercises **six compiler features end-to-end in
composition**: Phase 5 (build), Phase 2 (use-imported free function
call), @UTAIRZ (`&str` ABI via string literal), Phase 6 (unwrap
wrappers on non-stdlib `Result`), Phase 4 (I/O via `Write::write_all`),
and — new for this test — **explicit type args on a free-fn call**.

### Why toml specifically

Other candidates were considered:

- **`serde_json_test`** — same shape as toml (`from_str<T>(&str)`
  returning `Result<T, Error>` where T is typically `Value`). If toml
  works, serde_json should be near-mechanical. toml goes first because
  its `Value` type is simpler (no recursive enum through
  `serde::de::Deserialize` orchestration at the call site).
- **`glob_test`** — takes `&str`, returns an iterator we can't consume
  in toylang. First pass would just link-check. Lower info gain than
  toml — glob doesn't exercise generic type args or `.unwrap()`.
- **`rand_test`** — free fn returning an opaque `ThreadRng`. Simpler
  than toml but doesn't exercise anything toml doesn't; toml's generic
  type arg is the higher-value coverage.
- **`reqwest_test`** — first-pass plan is link-only (no `get()` call);
  exercises feature flags but nothing else.
- **`clap_test`** — still blocked on `impl Into<Str>` synthetic
  generic. Needs its own compiler work.

toml is the pick because it's the first remaining crate that
stress-tests **generic free function dispatch with an explicit type
arg**, a shape no Phase 7 test has exercised yet.

### What's new about this shape

Previous smoke tests called:
- `Uuid::new_v4()` — zero-arg inherent static method, no generics
- `IndexMap::new<K, V, S>()` — zero-arg inherent static method, 3 type args
- `Regex::new("pat")` — inherent static method with args, no generics
- `stdout()` — zero-arg free function, no generics

toml calls `from_str<Value>("")` — a free function (not a static
method on a type) with one explicit type arg. The closest precedent
in integration tests is `test_arg_type_mismatch_generic_fn` — a unit
test for arg checking on generic fns — but no **integration test**
end-to-end exercises this shape. You are the first.

If toylang's parser, type resolver, or codegen has a latent gap on
this shape — similar to how regex_test surfaced @IVTDBTZ — you will
be the one who surfaces it. See §6 for failure classes including
one specific to this shape.

---

## 3. The three files you will create

Mirror `regex_test` exactly. No creativity needed — the pattern is
proven. Look at the existing three (uuid, indexmap, regex) as the
template for structure, file naming, and test-function shape.

### File 1: `toylangc/tests/standalone/toml_test/toylang.toml`

```toml
[project]
name = "toml_test"
source = "main.toylang"

[rust-dependencies]
toml = "0.8"
```

Notes on the toml:
- `toml = "0.8"` pins to the 0.8 line (current: 0.8.19+). This is
  already used in `test_build_with_rust_dep` so we know cargo resolves
  it cleanly.
- No `features` block. Default features are what we want.
- `name = "toml_test"` becomes the binary name under
  `.toylang-build/target/debug/`.

### File 2: `toylangc/tests/standalone/toml_test/main.toylang`

**Starting point — iterate from here as errors guide you:**

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

Why each line:

| Line | Rationale |
|---|---|
| `use toml::from_str` | Named at the call site: `from_str<Value>(...)`. Free function — imported as itself, not as `Name::from_str`. |
| `use toml::Value` | Named as the explicit type arg. Implicit per @RTMEIZ: `Value` is the successful Ok type of the Result, flows through the type system. |
| `use toml::de::Error` | Implicit per @RTMEIZ. `from_str<Value>` returns `Result<Value, toml::de::Error>`. The error-type parameter of the Result is round-tripped through the type system when `.unwrap()` resolves, so toylang needs to find it. `toml` also re-exports `Error` at its crate root, so `use toml::Error` may also work — if `toml::de::Error` fails at Phase 5's cargo resolve, try the shorter form. |
| `use std::result::Result` | Implicit per @RTMEIZ. Same reason as in regex_test — `Result` is a named type flowing through. |
| `use std::io::stdout` | Named: the `stdout()` free-fn call. |
| `use std::io::Stdout` | Implicit per @RTMEIZ. The `&stdout()` expression has type `&Stdout`, which is the Self type of `Write::write_all`. Toylang needs the Stdout type imported even though the source never spells it. |
| `use std::io::Write` | Named: the trait in `Write::write_all`. |

**Note on the argument.** `""` is a valid empty TOML document. Parses
to an empty `Value::Table`. We're not testing that the TOML contents
are interesting — just that the parse succeeded and returned an
`Ok(Value::Table(empty))`, which `.unwrap()` extracts without panic.
If for some reason `""` trips an edge case in toml's parser (current
0.8.x accepts it, but version drift happens), fall back to `"x = 1"`
— a single-key document that's trivially valid.

**Why the `let` binding.** We bind the `Value` to a variable we never
use. That's deliberate — same shape as `indexmap_test` and
`regex_test`, keeps the test narrow to "the type-system path
completed and produced a value." We are *not* testing that we can
introspect the Value (no `.get()`, no indexing). Exercising Value's
methods would add more call sites and more failure modes; save that
for a later test.

**Why `;` after `write_all(...)`.** Per @MBMRVZ. `fn main()` with no
declared return type must have a void tail. Forgetting the `;` gives
a clean compile error today, but it's the single most common thing
to forget. Add it.

### File 3: append to `toylangc/tests/standalone_tests.rs`

Mirror `test_standalone_regex` exactly. Copy that function, change
`regex` → `toml` throughout:

```rust
#[test]
fn test_standalone_toml() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/toml_test");

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let build_out = run_build(&project);
    assert!(
        build_out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let bin = build_dir.join("target/debug/toml_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run toml_test binary");
    assert!(
        run.status.success(),
        "toml_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("toml ok"),
        "expected 'toml ok' in stdout, got: {}",
        stdout,
    );
}
```

Insert it after `test_standalone_regex` in the same file. Keep the
ordering alphabetical or chronological — match whatever's there.

---

## 4. Toylang syntax cheatsheet (the things that will trip you up)

Most of this is also in `handoff.md`; duplicated here because toml
touches every pitfall plus one new one.

### Generic type args are not turbofish

| Rust | Toylang |
|---|---|
| `Vec::<i32>::new()` | `Vec::new<i32, Global>()` |
| `toml::from_str::<Value>("")` | `from_str<Value>("")` (after `use toml::from_str`) |
| `.unwrap()` | `.unwrap()` — works as a MethodCall; toylang routes to the Phase 6 wrapper automatically |

Toylang's lexer distinguishes `<` by whitespace: `a < b` is
comparison, `name<T>` is generic-start. If in doubt, squeeze out
the space. For this test specifically:
- `from_str<Value>("")` — **no space between `from_str` and `<`**.
- `from_str <Value>("")` (with space) would lex as `from_str < Value
  > ("")`, which is nonsense and you'd get a parser error.

### **This test is first to exercise generic type args on a bare free-fn call**

No prior Phase 7 test has done `name<T>(args)` where `name` is a
use-imported free function. Precedents:
- `name()` — free fn, no type args (`stdout()`, etc.). Covered.
- `Name::method<T>(args)` — static method with type args
  (`Vec::new<i32, Global>()`, `IndexMap::new<i32, i32, RandomState>()`).
  Covered.
- `name<T>(args)` — free fn with type args. **First integration-test
  exerciser: this test.** Unit-tested indirectly via
  `test_arg_type_mismatch_generic_fn`.

What to watch for if it doesn't parse:
- Parser error at the `<` token — toylang might not accept
  turbofish-free generic free-fn syntax. **If this happens, stop and
  escalate** — it's a real compiler gap needing a parser/type-resolve
  fix, not a source edit.
- Parser error at the call site — maybe the lexer sees `<Value>` as
  a left-angle comparison. Try without whitespace first. If that
  still fails, escalate.

### All generic type params must be supplied explicitly

toml's `from_str` is defined as `from_str<T: DeserializeOwned>(s: &str) -> Result<T, de::Error>`.
That's one type parameter `T`, constrained to `DeserializeOwned`.
toylang doesn't do inference, so you supply `Value` explicitly:
`from_str<Value>("")`.

Toylang's oracle doesn't validate trait bounds itself — rustc does
that during its normal type-check pass on the generated stubs. Since
`Value: DeserializeOwned` is true in the toml crate, you'll get no
complaint. If for some reason toml's version changed and `Value`
lost its `DeserializeOwned` impl, you'd see a rustc trait-bound error
from cargo during Phase 5 build — that would be a cargo/library
issue, not a toylang bug.

### `&mut` doesn't exist in toylang

Not relevant here — `from_str` takes `&str` not `&mut`. But good to
remember.

### `let` is always mutable

Don't write `let mut`. Just `let`. Trivial; worth noting.

### Byte strings vs. regular strings

- `"..."` → `&str` (ScalarPair fat pointer, 2-arg ABI) — per @UTAIRZ
- `b"..."` → `&[u8]` (ScalarPair fat pointer, 2-arg ABI) — per Phase 3

Both use escape sequences: `\n \t \\ \0 \"`. In this test you'll
pass `""` (no content, no escapes) to `from_str`, and `b"toml ok\n"`
(one escape) to `write_all`.

### Implicit imports per @RTMEIZ

Every Rust type that flows through the type system needs `use`,
including:
- The `Self` type of any trait call (here: `Stdout` for `Write::write_all`)
- Every non-primitive generic type arg, including implicit ones from
  nested generics (here: `Result` and `toml::de::Error` from the
  return type of `from_str`)

You'll see the structured error if you miss one. Read the `context`
field — it tells you exactly why each type was needed. Add the `use`
line. Retry.

### Void-return `fn main()` per @MBMRVZ

`fn main()` without `-> Type` must end with `;`-terminated statements
or a void-typed tail expression. Non-void tail expressions give
`TypeResolveError::MainMustReturnVoid` at type-resolve time.

### Per @IVTDBTZ — dispatch is type-kind-based

Since this test's call site `from_str<Value>("")` is a **free function**
(not a `Name::method` shape), it bypasses the trait-vs-inherent
dispatch logic entirely. It goes through
`find_use_imported_fn_def_id` directly. @IVTDBTZ is backdrop
context, not a hot path for this test. But if you end up reading
type_resolve.rs to diagnose a failure, you'll encounter
`is_rust_trait` — know that it's not relevant to your call site.

---

## 5. How to run it

Follow `CLAUDE.md`'s build-redirect convention. Pipe to a fixed file
in `/tmp/` via `tee`; inspect in a separate command. **Don't chain
`| grep` or `| head` onto the same line** — you lose re-inspection.

### Just the new test

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_toml 2>&1 | tee /tmp/erw-toml-test.txt
grep "test result:" /tmp/erw-toml-test.txt
```

### Full suite after the test passes

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-toml-full.txt
grep "test result:" /tmp/erw-toml-full.txt
# Expected after this lands:
# test result: ok. 67 passed  (unit)
# test result: ok. 129 passed (integration)
# test result: ok. 9 passed   (standalone — was 8, +1 for toml)
```

### Timing expectations

- First run: **60–180 seconds** (cargo downloads toml + transitive
  deps: toml_datetime, toml_edit, serde, serde_spanned, winnow,
  indexmap). Network required first time. `toml` has a deeper
  dependency graph than `regex` so expect the longer end.
- Subsequent runs: **5–15 seconds** (deps cached in
  `~/.cargo/registry` and the test's own `.toylang-build/target`).
- Full suite after cache is warm: 35–50 seconds.

If a single run sits at 0% CPU for more than 2 minutes, you may have
hit a deadlock. See failure class 7 below — rare, but it happened
pre-Phase 6.3.

---

## 6. Failure classes (how to diagnose what went wrong)

These are the classes you're most likely to hit, in rough order of
likelihood for this specific test. Classify first, then read the
matching section. **Don't try to fix compiler internals** — if you
hit anything beyond classes 1–3, escalate.

### Class 0 — New: parser / type-resolve doesn't accept `name<T>(args)`

**Symptom:** Parser error at the `<` in `from_str<Value>("")`, or a
type-resolve error like `UndefinedFunction { name: "from_str" }` even
though `use toml::from_str` is imported, or a brand-new panic
somewhere in `type_resolve.rs` / `oracle.rs` about generic free-fn
resolution.

**What it means:** toylang's end-to-end plumbing for "generic free
function call with explicit type args" has a latent gap. It works at
the unit-test level (`test_arg_type_mismatch_generic_fn`) but not
end-to-end.

**Why this is possible:** Zero Phase 1–7 integration tests exercise
the `name<T>(args)` shape on a use-imported free function. The oracle
has `rust_free_fn_return_type(_, _, type_args: &[ResolvedType])` that
accepts type_args, but it may never have been called with non-empty
type_args in a real compile.

**Fix:** **Escalate.** Not a junior fix. Write up the exact parser
or type-resolve error and post in team chat, citing this class in
particular. Use the template from
`bug-report-regex-static-call-misclassified-as-trait.md` at the repo
root — it's the reference writeup for a "latent compiler gap
surfaced by a Phase 7 smoke test" and got a clean TL-sign-off
turnaround. Mirror its structure:

1. Symptom (the exact error)
2. Minimal reproducer (the three files you made, already a complete
   reproducer)
3. Root cause (your best guess after reading type_resolve.rs around
   the FnCall arm and oracle.rs::rust_free_fn_return_type)
4. Why this wasn't caught earlier (same argument as above)
5. Scope impact (toml, serde_json, any future generic free-fn call)
6. Suggested fix direction (one paragraph — your best guess; the TL
   will refine)

### Class 1 — `RustTypeNotImported { name: "X", context: "Y" }`

**Symptom:**

```
[toylang] validation failed with 1 error(s):
  - function 'main': RustTypeNotImported { name: "Error",
      context: "as generic arg inside `Result`" }
```

**What it means:** A Rust type the type system needed isn't
`use`-imported. The `context` field tells you exactly why.

**Fix:** Add the `use` line. Rerun.

For toml specifically, the likely candidates you might miss from the
proposed program are:
- `toml::de::Error` — the Err type parameter of `Result<Value, Error>`.
  If `use toml::de::Error` fails at Phase 5 build (e.g., toml version
  drift moved it), fall back to `use toml::Error`.
- `std::result::Result` — the Result type itself
- `std::io::Stdout` — Self of the `Write::write_all` trait call
- `toml::Value` — the Ok type parameter; also the explicit type arg

**Do not modify the compiler.** This class is a source-code problem.

### Class 2 — `MainMustReturnVoid { got: <some type> }`

**Symptom:** You forgot the `;` after `Write::write_all(...)` or
after the unwrap call.

**Fix:** Add `;`. Rerun.

### Class 3 — Parser errors

Most likely causes for this test:
- Missing `;` after a `let`.
- Using Rust turbofish (`::<T>`) instead of toylang `name<T>()`.
- Whitespace bug around `<` in `from_str<Value>`.
- Typo in a type name (`Value` vs `value` — capital V).

**Fix:** Your source code. Not a compiler bug.

### Class 4 — Linker error on a rustc-mangled symbol `_ZN4toml...`

**Symptom:** Toylang compilation succeeds, `cargo build` fails at
link time with a missing symbol that looks like
`_ZN4toml2de4from_str...` or similar.

**What it means:** Toylang declared an external Rust function with
the wrong ABI (return type or param type mismatch vs. what rustc
compiled). This is an ABI gap in `llvm_gen.rs` or the facade's
`abi_helpers.rs`.

**Escalate.** Not a junior fix. Document what the missing symbol
was, which toylang call site is suspect, and post in team chat.

### Class 5 — Linker error on a `__toylang_*` symbol

**Symptom:** Missing symbol prefix `__toylang_`.

**What it means:** Toylang's codegen and the CGU partitioner
disagreed on which symbols to emit. Almost certainly a Phase 6
regression around the unwrap wrapper.

**Escalate.** Has been fixed twice; a new break deserves careful
review.

### Class 6 — Runtime segfault / SIGBUS

**Symptom:** Build succeeds, the binary runs, prints (or doesn't),
exits with a signal — 138 = SIGBUS (macOS), 139 = SIGSEGV (Linux),
EXC_BAD_ACCESS on macOS arm64.

**What it means:** ABI mismatch at a call site. Toylang and rustc
disagree about where a value lives.

**Debug tip:** Run under lldb:
```bash
BIN=toylangc/tests/standalone/toml_test/.toylang-build/target/debug/toml_test
SYSROOT=$(rustup run rustc-fork rustc --print=sysroot)
DYLD_LIBRARY_PATH=$SYSROOT/lib lldb -batch --one-line-on-crash 'bt 30' -o 'run' $BIN
```
Capture the backtrace. If the crash is in `__toylang_result_unwrap`
or immediately after it, the unwrap wrapper's ABI is suspect. If the
crash is inside `toml::de::from_str` with bogus `&str` data, the
string-literal ABI (@UTAIRZ) or the free-fn call ABI
(`push_arg_for_rust_call` under the FnCall path) is suspect.

**Escalate.** Don't try to patch this alone. The regex_test
escalation turned into the @IVTDBTZ fix — this could be a similar
shape for free-fn arg passing. Reference bug-report-regex-static-*
as your writeup template.

### Class 7 — Build hangs at 0% CPU

**Symptom:** `cargo +rustc-fork test` sits at 0% CPU for >2 minutes.

**What it means:** Possible mutex deadlock (@GCMLZ). Should be
prevented structurally by Phase 6.3's two-family trait split, but a
regression would surface here.

**Debug:** Ctrl+C. Then `sample <pid>` on macOS or `lldb -p <pid>`
to get the blocked thread's stack. Post the trace.

**Escalate.**

### Class 8 — Missing rustc-dev crates

**Symptom:** Cargo errors about missing `rustc_abi`, `rustc_middle`,
etc. when building `rustc-lang-facade`.

**What it means:** The `rustc-fork` toolchain's sysroot is missing
the rustc-dev crates.

**Fix:** Rebuild the fork per `docs/usage/rebuilding-rustc-fork.md`.
Shouldn't happen unless you rebuilt the fork yourself.

---

## 7. Scope discipline — what NOT to do

- **Don't** actually use the parsed `Value`. Our job is link-check
  plus the type-system path for unwrap. Exercising `val.as_table()`
  or indexing would add method calls, another failure mode each;
  save it for later.
- **Don't** change anything in `toylangc/src/`, `rustc-lang-facade/`,
  or `~/rust/`. If you think you need to, stop — the test has
  surfaced a real compiler gap that needs escalation, not a source
  edit. Specifically: if you feel tempted to change type_resolve.rs
  or oracle.rs to "make generic free-fn calls work", that's the
  escalation path. Write it up.
- **Don't** add additional standalone tests in the same PR. One
  crate, one test, one commit.
- **Don't** parse non-trivial TOML. `""` (or `"x = 1"` if `""` fails)
  is the smallest thing that's definitely valid. Exotic TOML content
  adds nothing to the test's signal and might surface toml-crate
  edge cases that have nothing to do with toylang.
- **Don't** ignore warnings that come from `cargo build` of the
  generated wrapper crate. Unused-import warnings are expected
  (toylang's load-bearing `use` lines look unused to rustc). Anything
  else — read it.

The one-job rule: if a change isn't required to make
`test_standalone_toml` pass, don't make it. Note unrelated
observations in team chat or as separate PRs.

---

## 8. Pre-submission checklist

Before calling it done:

- [ ] `toylangc/tests/standalone/toml_test/toylang.toml` has
      `[project]` and `[rust-dependencies]` with `toml = "0.8"`.
- [ ] `toylangc/tests/standalone/toml_test/main.toylang` has the
      required imports and body. Exactly one `fn main()`.
- [ ] `toylangc/tests/standalone_tests.rs` has a matching
      `test_standalone_toml` function mirroring
      `test_standalone_regex`.
- [ ] `.toylang-build/` directories are NOT committed (root
      `.gitignore` excludes them — verify `git status` is clean of
      them).
- [ ] Isolated test passes:
      `cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_toml`
- [ ] Full suite passes with no regressions (204 → 205 tests).
- [ ] Full suite passes **twice in a row**. Second should be faster
      (cache warm). If it fails on the second but passed the first,
      you have a hermeticity bug — fix the test cleanup logic.
- [ ] `git status` shows only files under
      `tests/standalone/toml_test/` and the edit to
      `standalone_tests.rs`. No compiler source changes.
- [ ] Commit message style matches prior ones — dense, one
      paragraph, describes what and why. See
      `git log --oneline -5` for examples. Suffix
      `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
      only if you want to credit the assistant.
- [ ] Update `quest.md` (gitignored, stays local) to reflect Phase 7
      at 4/9 with a one-line note on what toml proved. Also bump
      the test-total line to 205.
- [ ] Update `docs/architecture/rust-interop-guide.md` front-matter:
      test totals 204 → 205, Phase 7 status `3/9` → `4/9`, brief
      mention of toml under §10.7 (promote to "Done" list).

---

## 9. What passing toml unlocks

Direct unblocks:
- **serde_json** — the same shape (`from_str<T>(&str) -> Result<T, Error>`)
  with `serde_json::Value` replacing `toml::Value`. Should be
  mechanical. Once toml passes, writing the serde_json handoff is
  copy-and-modify.

Also validated (not unblocked, but proven):
- **glob** — takes `&str` but via `glob::glob(pattern)` — not
  generic. Would likely have worked without toml_test too, but if
  toml surfaces a generic-free-fn gap and the fix also touches
  non-generic free-fn dispatch, glob benefits.

Still orthogonally blocked:
- **clap** — `Command::new(impl Into<Str>)`. Synthetic `impl Trait`
  generic is a separate compiler gap.

Not affected by this test:
- **rand** — free fn, no `&str`, no generics. Would have passed
  before this test.
- **reqwest** — first-pass plan is link-only, same story.

So after toml lands, the remaining batch is: clap (own mini-phase,
needs compiler work), 1 mechanical crate that mirrors toml
(serde_json), and 3 simpler mechanical crates (rand, glob, reqwest).

---

## 10. If you get stuck — what to escalate with

Post in team chat with:

1. Which failure class from §6 (or "something else").
2. The exact command you ran.
3. The tail of `/tmp/erw-toml-test.txt` (last 60–100 lines).
4. What you've already tried.
5. The current contents of your `main.toylang` (copy-paste inline).
6. The `git diff` of your changes (or just the file list — whatever's
   cleaner).

The goal is to make it easy for the reviewer to either:
(a) spot the obvious thing you missed, or
(b) understand the real compiler gap you've surfaced without having
    to re-run everything locally.

**Special case — Class 0 (generic free-fn dispatch gap).** This is
the most interesting-but-possible failure mode for this specific
test. If you hit it, your writeup should follow the
bug-report-regex-static-call-misclassified-as-trait.md template
exactly. That report became the @IVTDBTZ arcana and a clean TL
sign-off in under 24 hours. Good escalations get fast turnarounds
because they do the diagnostic work for the reviewer.

**Do not attempt ABI fixes, linker fixes, or fork patches.** Those
affect everything downstream. Your job is to prove the test works
— or to surface the blocker clearly. Both outcomes are valuable.

---

## 11. Summary

Three files. Eleven lines of toylang source (plus the Rust test
function which is just `test_standalone_regex` with names swapped).
Passing `test_standalone_toml` bumps Phase 7 to 4/9, tests to 205,
and confirms Phase 7's remaining mechanical batch (serde_json, glob,
rand, reqwest) is indeed mechanical.

Most of this doc is prep for the 10% chance something goes wrong in
an informative way — specifically the Class 0 "generic free-fn
dispatch" gap that would be this test's equivalent of what regex_test
surfaced (@IVTDBTZ). The 90% expected case is: add the three files,
run the test, see "toml ok" in stdout, commit, ship.

If you've read this far and the program in §3 looks mysterious,
that's the signal to go back to the reading list in §0 and work
through the arcana. The arcana exist because each one is an
expensive lesson someone already paid for. You get the cheap
version.

Good luck.
