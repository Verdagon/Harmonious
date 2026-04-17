# Handoff: Phase 7 crates #6 and #7 — `glob` and `rand` standalone smoke tests

> **Status: LANDED 2026-04-17. Preserved as historical context.**
>
> **Outcome:** Both tests first-try green with zero compiler changes
> and zero iteration on imports. Three files each, ~12 lines of
> toylang combined, committed one-per-test in the recommended order
> (glob first, rand second). Bumped tests 206 → 207 → 208 and
> Phase 7 5/9 → 7/9. Full suite passed on the first run after each
> landing; second run (hermeticity check) also passed both times.
> Realistic expectation from the handoff's §2 ("Realistic
> expectation: both pass on first try or near-first try with only
> `use` line adjustments") held exactly — no adjustments were
> required. The `use rand::rngs::ThreadRng` canonical path resolved
> on the first attempt; no fallback to the crate-root re-export
> needed.
>
> **Prediction accuracy:** the 90% path the handoff predicted. §7's
> eight failure classes were all prepared for; none fired.
> Specifically: Class 0 ("Novel gap — unlikely but possible") was
> the main monitored risk — the handoff flagged `Paths` Drop glue
> (glob) and `ThreadRng` Drop glue (rand) as "weakly possible"
> gaps, and explicitly called out Failure Class 5 ("missing
> `__toylang_*` symbol or `drop_in_place::<ThreadRng>`") as the
> escalation path if the Drop-dep walk had a hole. The walk
> didn't. Both Drops compile and run normally via rustc's Instance
> collector, confirming the same code path that already worked for
> `Stdout` (Phase 4) extends to opaque cargo-resolved crate types
> held as unused let-bindings.
>
> **Two sequential first-try passes in the same session — no
> @UTAIRZ / @IVTDBTZ / @ELASZ moment.** This is the first
> multi-test Phase 7 handoff where no latent compiler gap
> surfaced. The pattern the last three Phase 7 tests established
> (`regex` surfaced @IVTDBTZ, `serde_json` surfaced @ELASZ, `toml`
> was first-try) now has two more "mechanical completion" data
> points. Strongly suggests `reqwest_test` — the last remaining
> unblocked crate — will also be mechanical.
>
> **Lesson carried forward:** the handoff's §7 "classify first,
> then match section" flow was the critical self-discipline. It
> made the 90% path explicit and kept the escalation path clear
> for the 10%. The §6 build-redirect convention (`/tmp/erw-glob-rand.txt`,
> never chain `| grep`) was followed throughout; re-reading the
> full test output post-hoc via a separate `grep "test result:"`
> command confirmed the 67 + 129 + 12 = 208 final totals without
> re-running the suite.
>
> **Outcome totals at land time:**
> - glob commit: 67 unit + 129 integration + 11 standalone = 207
> - rand commit: 67 unit + 129 integration + 12 standalone = 208
> - 0 failed, 0 ignored across both
>
> **What's left after this:** 2 Phase 7 crates (`clap` still
> blocked on orthogonal `impl Into<Str>` synthetic generic;
> `reqwest` unblocked, likely mechanical per the two-data-point
> inference above). Phase 7 target of 8/9 ignoring clap is now
> one commit away.
>
> Original handoff body preserved below verbatim for historical
> reference.
>
> ---

**Audience:** Junior engineer picking up Phase 7. Comfortable reading
Rust and willing to read compiler errors carefully. No prior
compiler-internals experience required.

**Estimated effort:** 1–3 hours total for both tests. Most of that is
the reading prerequisites below; the tests themselves are ~10 lines
of toylang each and should pass first-try or near-first-try.

**Prerequisites.** You should have read, at minimum:

1. **Project root `CLAUDE.md`** — build-redirect convention. When you
   run `cargo test`, pipe via `tee` to a fixed file in `/tmp` (pick
   a name like `/tmp/erw-glob-rand.txt` for this session). Never
   chain `| grep` / `| head` / `| tail` onto the same line as the
   `tee` — that defeats the purpose. Run the build as one command,
   inspect with a second command.
2. **`quest.md`** — the Phase 7 section and the "What landed" entries
   for `toml_test` and `serde_json_test`. Skim the whole doc first
   if you haven't; it's the single source of truth for what each
   phase produced.
3. **`docs/architecture/rust-interop-guide.md`** — at least Part 4
   (how the consumer/facade split works, why `ResolvedType` is
   unified, why all type args are explicit). §10.7 gives you the
   Phase 7 remaining-crate table.
4. **`docs/usage/writing-main.md`** — the practical cheatsheet for
   authoring toylang source that calls Rust code. Import rules,
   syntax pitfalls, void-tail requirement.
5. **Arcana you'll encounter in the error messages:**
   - `@MBMRVZ` — `fn main()` must return void. Forget the trailing
     `;` and you'll see `MainMustReturnVoid`.
   - `@RTMEIZ` — every Rust type flowing through the type system
     must be `use`-imported, even implicitly (e.g., the `Err` type
     of a `Result` even if you never name it). Structured error
     message tells you which type and why.
   - `@UTAIRZ` — `&str` and `&[u8]` are fat pointers. Relevant to
     glob (takes `&str`); not relevant to rand (zero args).
   - `@IVTDBTZ` — trait-vs-inherent dispatch; not hit by either
     test but backdrop context if you end up reading `type_resolve.rs`.
   - `@ELASZ` / `@ETASTZ` — the two arcana landed with
     `serde_json_test`. `@ELASZ` handles early-bound lifetime
     params (synthesized as `re_erased`); `@ETASTZ` handles
     user-supplied extras beyond the method's slot count (silently
     truncated — load-bearing for Vec-style call-site naming).
     Unlikely to be relevant to glob/rand, but good context.
6. **The three prior handoffs** in `docs/historical/` — especially
   `handoff-toml.md`, which is the template this doc mirrors. Its
   "Retrospective" block at the top documents what actually
   happened vs what was predicted; the "prediction vs reality"
   framing is the single most useful meta-lesson for this work.

**Scope.** Two crates — `glob` and `rand`. Each is three files
(toylang.toml + main.toylang + a test function appended to
standalone_tests.rs). **One commit per test** is the recommended
cadence: land glob first, fully green the whole suite, then do
rand. Each lands independently; they don't share code.

---

## 1. Where we are

Phase 7 is at 5/9 after `serde_json_test` landed along with two
arcana (`@ELASZ`, `@ETASTZ`). Current totals: 67 unit + 129
integration + 10 standalone = **206 tests, 0 failed, 0 ignored.**

Done so far: `uuid_test`, `indexmap_test`, `regex_test`,
`toml_test`, `serde_json_test`.

Remaining after this handoff: `clap_test` only. That's its own
mini-phase because `Command::new(impl Into<Str>)` has a synthetic
`impl Trait` generic that toylang can't currently express. Someone
else will plan and execute it separately.

**After you finish** both tests land: Phase 7 will be at **8/9 with
208 tests** (or 209 if an unforeseen compiler gap requires adding
a regression unit test, same pattern as @ELASZ).

## 2. What you're proving

### `glob_test` (target: 5th crate to use `&str`)

A toylang program that:

1. Depends on `glob = "0.3"` from crates.io (Phase 5 manifest).
2. Calls `glob("*.rs")` — a **use-imported free function taking
   `&str` and returning `Result<Paths, PatternError>`**.
3. Binds the result to a variable but **does not** call `.unwrap()`
   on it — first-pass scope discipline. Exercising the `Paths`
   iterator is out of scope.
4. Prints `"glob ok\n"` and exits zero.

This exercises `@UTAIRZ` (`&str` ABI via string literal), Phase 2
(use-imported free fn), Phase 5 (build), Phase 4 (I/O). No
generics, no `.unwrap()`. Simplest remaining shape.

### `rand_test` (target: first zero-arg free fn returning an opaque type)

A toylang program that:

1. Depends on `rand = "0.8"` from crates.io.
2. Calls `rand::thread_rng()` — a **zero-arg free function returning
   `ThreadRng`**.
3. Binds the returned `ThreadRng` to a variable; does **not** call
   any methods on it (no `.gen()`, no `Rng` trait usage). Drop glue
   runs at end of `main()` via rustc's normal codegen.
4. Prints `"rand ok\n"` and exits zero.

This exercises Phase 2 (free fn with return type that has a
non-trivial Drop), plus the full I/O chain. No generics, no `.unwrap()`.

### Why these two together

Both are "mechanical completion" crates. Neither exercises a new
compiler feature — all the infrastructure landed in earlier
phases. If either surfaces a latent gap, it'll be analogous to
@ELASZ / @IVTDBTZ (a shape of Rust API toylang hasn't yet
encountered). But the probability is low. Realistic expectation:
both pass on first try or near-first try with only `use` line
adjustments.

---

## 3. `glob_test` — the three files

### File 1: `toylangc/tests/standalone/glob_test/toylang.toml`

```toml
[project]
name = "glob_test"
source = "main.toylang"

[rust-dependencies]
glob = "0.3"
```

`glob = "0.3"` is the current stable line. No features block
needed — defaults are fine.

### File 2: `toylangc/tests/standalone/glob_test/main.toylang`

**Starting point:**

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

Why each import (per `@RTMEIZ`):

| Line | Reason |
|---|---|
| `use glob::glob` | Named at the call site as a bare free function. |
| `use glob::Paths` | Ok type of `Result<Paths, PatternError>`. @RTMEIZ wants it findable even if source never spells the name. |
| `use glob::PatternError` | Err type of the Result. Same @RTMEIZ reason. |
| `use std::result::Result` | The `Result` type itself. |
| `use std::io::stdout` / `Stdout` / `Write` | Standard I/O imports, same pattern as every other Phase 7 test. |

**Notes on the program:**

- **`"*.rs"` is a valid glob pattern.** Any POSIX-glob pattern works.
  The test doesn't care what matches; `glob("invalid[")` would
  return an `Err(PatternError)` but that's fine — we don't
  `.unwrap()`, so an error result binds to `result` without panic.
  Use `"*.rs"` because it's obviously correct and won't tempt you
  to introspect the match count.
- **No `.unwrap()`.** Deliberate. `Paths` is an iterator of
  `Result<PathBuf, GlobError>` — consuming it would require `for`
  loops or `Iterator::next`, neither of which is in scope for a
  smoke test. We're just proving the call links.
- **`let result = ...` is load-bearing.** You need the binding so
  the Result value has a place to live. Don't try
  `glob("*.rs");` as a bare ExprStmt — the tail non-void-return
  rule (@MBMRVZ) kicks in and it might fail, and more importantly,
  the Result gets dropped at end-of-statement with no opportunity
  for rustc to run the Drop impl via its normal codegen path. The
  `let` keeps the value alive to end-of-function and matches what
  uuid/indexmap/regex/toml/serde_json all do.
- **Trailing `;` after `write_all(...)`.** Required per @MBMRVZ.

### File 3: append to `toylangc/tests/standalone_tests.rs`

Mirror `test_standalone_serde_json` exactly (that's the most
recent template). Insert between `test_standalone_regex` and
`test_standalone_serde_json` to maintain alphabetical ordering
(uuid → indexmap → regex → glob → serde_json → toml). Or, if
alphabetical ordering has decayed (check the current state —
probably still alphabetical by crate name), put it where it fits.

```rust
// Phase 7 crate #6: glob — free fn taking &str, returning Result
// without unwrap. First Phase 7 test to bind a Result and NOT call
// .unwrap() on it. Exercises @UTAIRZ `&str` ABI via string literal
// and Phase 2 free-fn dispatch.
#[test]
fn test_standalone_glob() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/glob_test");

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

    let bin = build_dir.join("target/debug/glob_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run glob_test binary");
    assert!(
        run.status.success(),
        "glob_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("glob ok"),
        "expected 'glob ok' in stdout, got: {}",
        stdout,
    );
}
```

Reuses the existing helpers `run_build()` and `sysroot_lib()` at
the top of the file.

---

## 4. `rand_test` — the three files

### File 1: `toylangc/tests/standalone/rand_test/toylang.toml`

```toml
[project]
name = "rand_test"
source = "main.toylang"

[rust-dependencies]
rand = "0.8"
```

**Pin to 0.8, not 0.9.** rand 0.9 renamed `thread_rng` to `rng` and
reorganized modules; the handoff's starting code uses the 0.8 API.
If you're feeling adventurous you can try 0.9, but then read the
upstream release notes and adjust the imports accordingly.

### File 2: `toylangc/tests/standalone/rand_test/main.toylang`

**Starting point:**

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

Why each import (per `@RTMEIZ`):

| Line | Reason |
|---|---|
| `use rand::thread_rng` | Named at the call site as a bare free function. |
| `use rand::rngs::ThreadRng` | Return type of `thread_rng()`. Canonical path in rand 0.8 is `rand::rngs::ThreadRng`. If the build fails at cargo resolve, `use rand::ThreadRng` (re-export at crate root) is the fallback. Try the longer path first — it's definitely there. |
| `use std::io::stdout` / `Stdout` / `Write` | Standard I/O imports. |

**Notes:**

- **No `Result` imports** — `thread_rng()` returns `ThreadRng`
  directly, not a Result. No `.unwrap()`. No error-type to
  import.
- **The `let rng = ...` binding** is load-bearing for the same
  reason as glob's `let result`: it gives the value a scope to
  live in so Drop runs at end-of-`main`. `ThreadRng`'s Drop
  decrements a thread-local refcount; rustc handles this via
  normal codegen.
- **No methods on `rng`.** Don't try `rng.gen::<i32>()` or similar
  — that would pull in the `Rng` trait and its associated method
  call, an entire new shape. First-pass scope is proving the
  `thread_rng()` call links.

### File 3: append to `toylangc/tests/standalone_tests.rs`

Mirror the glob test function verbatim with `glob` → `rand`.
Alphabetically `rand` sits between `indexmap` and `regex`. Place
accordingly.

```rust
// Phase 7 crate #7: rand — zero-arg free fn returning an opaque
// ThreadRng. First Phase 7 test to return a non-Copy non-Result
// Rust type from a free fn and let Drop glue run naturally at
// end-of-main.
#[test]
fn test_standalone_rand() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/rand_test");

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

    let bin = build_dir.join("target/debug/rand_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run rand_test binary");
    assert!(
        run.status.success(),
        "rand_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("rand ok"),
        "expected 'rand ok' in stdout, got: {}",
        stdout,
    );
}
```

---

## 5. Toylang syntax cheatsheet — the pitfalls specific to these tests

Most of this is in `docs/usage/writing-main.md`. Duplicated here
because these two tests touch every pitfall plus one rand-specific
one.

### Generic type args are NOT turbofish

Not relevant to either test — both call non-generic free fns. But
if you end up adjusting syntax, remember: toylang is `name<T>(args)`
(no `::`), not `name::<T>(args)`.

### `let` is always mutable

Not relevant here but worth noting. Don't write `let mut rng = ...`;
just `let rng = ...`.

### Byte strings vs regular strings

- `"..."` → `&str` (fat pointer, per `@UTAIRZ`)
- `b"..."` → `&[u8]` (fat pointer, per Phase 3)

Both support the same escape sequences: `\n \t \\ \0 \"`.

In glob_test you'll use `"*.rs"` (no escapes needed); in rand_test
the byte-string `b"rand ok\n"` goes to `write_all`. Don't mix them
up — `write_all` wants `&[u8]`.

### Trailing `;` on `write_all(...)` is mandatory

Per `@MBMRVZ`. `fn main()` without a return type must end with a
void-typed tail or semicolon-terminated statement. Forget the `;`
and you get `TypeResolveError::MainMustReturnVoid`. Fix: add the
`;`. Simple, but the single most common first-submit error.

### Implicit imports per `@RTMEIZ`

Every Rust type that flows through the type system needs `use` —
including types the source never names. The error message's
`context` field tells you exactly why. Read it; add the line;
retry.

For glob: `Paths`, `PatternError`, `Result` are all flowing types.

For rand: just `ThreadRng` (the return type).

### No generic free fns to worry about

Unlike `toml_test` and `serde_json_test`, neither of these calls a
generic free function. `glob(&str)` and `thread_rng()` are both
non-generic. So you won't exercise the `@ELASZ` code path. (If
you'd chosen rand 0.9, `rng()` might be different — one more
reason to stick with 0.8.)

### `ThreadRng` Drop glue — new territory

This might be the first Phase 7 test where a binding's type has a
non-trivial Drop impl that rustc generates at end-of-scope. Uuid
is `Copy` (no Drop). IndexMap is held through RandomState (simple).
Regex has Drop but it's used via a methodCall chain. Paths (glob)
has Drop but we never actually consume it.

ThreadRng's Drop is the only one in the Phase 7 corpus where the
binding is a non-Copy, non-Result value that sits unused to
end-of-scope. Rustc's collector should handle it — Stdout (used in
`&stdout()`) has Drop too and that works — but if rand_test fails
in a way that looks like "symbol not found: ... drop_in_place ..."
or "missing __toylang_..." at link time, that's the Drop-dep path.
See Failure Class 5.

---

## 6. How to run

Follow `CLAUDE.md`'s build-redirect convention. Pipe to `/tmp`
via `tee`; inspect in a separate command. **Don't chain** `| grep`
or `| head` on the same line.

### glob, isolated

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_glob 2>&1 | tee /tmp/erw-glob-rand.txt
grep "test result:" /tmp/erw-glob-rand.txt
```

### rand, isolated

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_rand 2>&1 | tee /tmp/erw-glob-rand.txt
grep "test result:" /tmp/erw-glob-rand.txt
```

### Full suite (after either test passes)

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-glob-rand.txt
grep "test result:" /tmp/erw-glob-rand.txt
```

Expected after both land:

```
test result: ok. 67 passed     (unit)
test result: ok. 129 passed    (integration)
test result: ok. 12 passed     (standalone — was 10, +2 for glob + rand)
```

Run the full suite **twice in a row** after each test to confirm
hermeticity. Second run should be faster. If the second fails when
the first passed, you have a test-cleanup bug.

### Timing expectations

- First run of either test: **30–60 seconds**. Both crates have
  shallow dep graphs (glob = no transitive deps; rand 0.8 pulls
  in rand_core, rand_chacha, getrandom, a few more). First run
  downloads via cargo.
- Subsequent runs: **5–15 seconds** (deps cached).
- Full suite, warm cache: 35–50 seconds.

If a run sits at 0% CPU for >2 minutes, possible deadlock — see
Failure Class 7.

---

## 7. Failure classes — how to diagnose

In rough order of likelihood for these two tests specifically.
Classify first, read the matching section.

### Class 1 — `RustTypeNotImported { name: "X", context: "Y" }`

**Symptom:**

```
[toylang] validation failed with 1 error(s):
  - function 'main': RustTypeNotImported { name: "PatternError",
      context: "as generic arg inside `Result`" }
```

**What it means:** A Rust type the type system needed isn't
`use`-imported. The `context` field tells you exactly why.

**Fix:** Add the `use` line. Rerun.

For glob, likely misses if you copy from a toml-style header
without adapting:
- `PatternError` — Err type of `Result<Paths, PatternError>`
- `Paths` — Ok type
- `Result` — the Result type itself

For rand:
- `ThreadRng` — return type; path is `rand::rngs::ThreadRng`

If the canonical path fails, toml / regex / serde_json all had
alternate re-export paths (`toml::Error` and `toml::de::Error`,
etc.). For rand, try `use rand::ThreadRng` as a fallback.

### Class 2 — `MainMustReturnVoid { got: <some type> }`

**Symptom:** You forgot the `;` after `Write::write_all(...)`.

**Fix:** Add `;`. Rerun.

### Class 3 — Parser errors

Most likely for these tests:
- Missing `;` after a `let`.
- Whitespace around `"` in string literals — shouldn't matter,
  but visually check for smart quotes or stray characters.
- Typo in a type name — `PatternError` vs `patternerror`,
  `ThreadRng` vs `ThreadRNG`.

**Fix:** Your source code. Not a compiler bug.

### Class 4 — Linker error on a rustc-mangled symbol

**Symptom:** Toylang compilation succeeds, `cargo build` fails at
link time with a missing `_ZN4glob...` or `_ZN4rand...` symbol.

**What it means:** An ABI mismatch. Toylang declared an external
Rust function with wrong param/return ABI vs what rustc compiled.
**Escalate** — not a junior fix. Document the missing symbol and
the call site and post in team chat, following the
`docs/historical/bug-report-regex-static-call-misclassified-as-trait.md`
template (or its successor — whichever is the most recent
compiler-gap writeup in the repo).

### Class 5 — Linker error on a `__toylang_*` symbol or drop glue

**Symptom:** Missing symbol starts with `__toylang_` or
`core::ptr::drop_in_place`.

**What it means:** Toylang's codegen and the partitioner disagreed
about which symbols to emit. For rand specifically: if the error
mentions `drop_in_place::<ThreadRng>` or similar, the Drop-dep
path for unused let-bindings has a gap.

**Escalate.**

### Class 6 — Runtime segfault / SIGBUS

**Symptom:** Build succeeds, binary runs, prints nothing (or
partial) and exits with a signal.

**Debug:**

```bash
BIN=toylangc/tests/standalone/glob_test/.toylang-build/target/debug/glob_test
SYSROOT=$(rustup run rustc-fork rustc --print=sysroot)
DYLD_LIBRARY_PATH=$SYSROOT/lib lldb -batch \
    --one-line-on-crash 'bt 30' -o 'run' $BIN
```

Capture the backtrace. If it crashes inside `glob::glob` with bogus
`&str` data, suspect `@UTAIRZ`. If it crashes in Drop glue
(`drop_in_place`), suspect an ABI mismatch on the Drop call.

**Escalate.**

### Class 7 — Build hangs at 0% CPU

**Symptom:** `cargo +rustc-fork test` sits idle for >2 minutes.

**What it means:** Possible mutex deadlock (`@GCMLZ`). Should be
prevented by Phase 6.3's two-family trait split, but regressions
happen.

**Debug:** Ctrl+C. Run `sample <pid>` (macOS) or `lldb -p <pid>`
to capture a stack trace of the blocked thread.

**Escalate.**

### Class 8 — Missing rustc-dev crates

**Symptom:** Cargo errors about missing `rustc_abi`, `rustc_middle`,
etc.

**Fix:** Rebuild the `rustc-fork` toolchain per
`docs/usage/rebuilding-rustc-fork.md`. Shouldn't happen unless you
built it yourself.

### Class 0 — Novel gap (unlikely but possible)

Three prior Phase 7 tests surfaced a latent compiler gap:
- `regex_test` → `@IVTDBTZ`
- (non-crate test) `@UTAIRZ` for string literal ABI
- `serde_json_test` → `@ELASZ` + `@ETASTZ`

The pattern: an API shape no previous test exercised triggers a
code path with an untested assumption. For glob/rand the risks
are:

- **glob: `Paths` has Drop glue involving an iterator.** Unused
  Result-binding with non-trivial inner type might surface a new
  edge case in dep registration.
- **rand: ThreadRng Drop.** First unused-binding with a non-Copy,
  non-Result type. Might surface Drop-dep-walk gaps.

Both risks are "weakly possible" — neither is predicted to fire
based on reading the current compiler code, but neither has a
positive test covering the specific shape yet.

**If you hit a novel ICE or panic:** stop. Write up the exact
error, minimal reproducer (your three files), best-guess root
cause, and post to team chat. Use the most recent bug-report file
in `docs/historical/` as a template — the serde_json escalation
(filed as pre-@ELASZ) is the most recent successful escalation.

---

## 8. Scope discipline — what NOT to do

- **Don't introspect the result.** No `paths.count()` on glob, no
  `rng.gen()` on rand. Smoke tests only.
- **Don't use `.unwrap()` on either.** glob explicitly requires no
  unwrap first-pass; rand doesn't return a Result so there's
  nothing to unwrap.
- **Don't change anything in `toylangc/src/`,
  `rustc-lang-facade/`, or `~/rust/`.** Per the established rule —
  if you feel tempted, that's an escalation path, not a source
  edit. @ELASZ was a team-tier fix; you're writing tests.
- **Don't do both tests in one commit.** One commit per test.
  glob first (simpler), rand second. Each commit should:
  1. Add the three files for that test.
  2. Bump doc totals (206 → 207 after glob; 207 → 208 after rand).
  3. Bump Phase 7 progress (5/9 → 6/9; 6/9 → 7/9).
- **Don't update `handoff-glob-rand.md` (this file).** When both
  tests land, the person who ships them (probably you) should
  mirror the `handoff-toml.md` retrospective pattern: add a "LANDED"
  block at the top documenting prediction-vs-reality, then move
  to `docs/historical/handoff-glob-rand.md`. See §10 below.
- **Don't parse non-trivial glob patterns.** `"*.rs"` is enough.
- **Don't touch rand 0.9.** Stick with 0.8.
- **Don't try to seed the RNG or call any method on it.** Binding
  and letting Drop run is the entire test.

## 9. Pre-submission checklist

For each test (do this twice — once after glob, once after rand):

- [ ] `toylangc/tests/standalone/<crate>_test/toylang.toml` has
      `[project]` and `[rust-dependencies]` with correct pin.
- [ ] `toylangc/tests/standalone/<crate>_test/main.toylang` has
      the required imports and body. Exactly one `fn main()`.
- [ ] `toylangc/tests/standalone_tests.rs` has a matching
      `test_standalone_<crate>` function.
- [ ] `.toylang-build/` directories are NOT committed (root
      `.gitignore` excludes them — verify `git status` is clean).
- [ ] Isolated test passes:
      `cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_<crate>`
- [ ] Full suite passes with no regressions.
- [ ] Full suite passes **twice in a row**. Second should be
      faster (cache warm). Failure on second after first passed =
      hermeticity bug; fix cleanup logic.
- [ ] `git status` shows only files under `tests/standalone/<crate>_test/`
      and the edit to `standalone_tests.rs` and the four doc bumps.
      **No compiler source changes.**
- [ ] Commit message style matches recent ones — dense, one
      paragraph, describes what and why. `git log --oneline -5`
      for examples.
- [ ] Update `quest.md` Phase 7 heading (`5/9` → `6/9` → `7/9`),
      the "landed and green" list, and the test totals (206 →
      207 → 208).
- [ ] Update `docs/architecture/rust-interop-guide.md`
      front-matter: test totals, `Phase 7 in progress (5/9)` →
      `(6/9)` → `(7/9)`, §10.7 "Done" list and "Remaining" table.

## 10. What passing unlocks

Direct unblocks:

- **`reqwest_test`** — the last mechanical crate, feature-gated on
  `blocking`. First-pass: link-only (no network call). After
  glob/rand confirm the no-generics + use-imported-free-fn shape
  is solid, reqwest should be mechanical; the only new wrinkle is
  the `features = ["blocking"]` entry in `toylang.toml`
  (exercises Phase 5's detailed-dep path end-to-end for the first
  time via standalone test).
- **`clap_test`** — still blocked on `impl Into<Str>` synthetic
  generic. A separate mini-phase, someone else's plan.

Also validated (not unblocked, but proven):
- Phase 7's "mechanical" classification for the remaining crates.
  Three tests in a row passing first-try-ish would strongly
  suggest reqwest is similar.

## 11. Retrospective step (do after both tests land)

Mirror the `handoff-toml.md` pattern. Add a block at the very top
of this file:

```markdown
> **Status: LANDED 2026-04-??. Preserved as historical context.**
>
> **Outcome:** <one-paragraph summary>
>
> **Prediction accuracy:** <one-paragraph comparison to §7 failure classes>
>
> <any surprises>
>
> Original handoff body preserved below verbatim.
>
> ---
```

Then `mv handoff-glob-rand.md docs/historical/`. Commit as a
separate commit titled something like `handoff-glob-rand: mark
landed, move to historical`.

## 12. If you get stuck — what to escalate with

Post in team chat with:

1. Which failure class from §7 (or "something else").
2. The exact command you ran.
3. The tail of `/tmp/erw-glob-rand.txt` (last 60–100 lines).
4. What you've already tried.
5. The current contents of your `main.toylang` (copy-paste inline).
6. The `git diff` of your changes (or just the file list).

Goal: make it easy for the reviewer to either spot the obvious
missed thing or understand the real compiler gap without re-running
locally.

**Do not attempt ABI fixes, linker fixes, or fork patches.** The
rule holds from prior handoffs. Your job is to prove the tests
work — or surface the blocker cleanly. Both outcomes are valuable.

---

## 13. Summary

Six files (three per test). Under twenty lines of toylang source
combined. Two commits, small and self-contained. Bumps Phase 7
from 5/9 to 7/9, from 206 tests to 208. Confirms the remaining
crate (`reqwest_test`) is likely mechanical.

Most of this doc is preparation for the 10% chance either test
surfaces an unpredicted gap — that's the same structure every
Phase 7 handoff has taken. The 90% case is: three files each, run
the test, see "glob ok" / "rand ok", commit, ship.

If you're reading this far and the starting programs in §3 and §4
look mysterious, go back to the reading list at the top and work
through the arcana. Each arcana exists because it's a lesson
someone already paid for. You get the cheap version.

Good luck.
