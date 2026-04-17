# Handoff: Phase 7 crate #3 — `regex` standalone smoke test

**Audience:** Junior engineer picking up Phase 7. Comfortable reading
Rust and willing to read compiler errors carefully. No prior
compiler-internals experience required.

**Estimated effort:** 2–6 hours. Most of that is reading and
iterating on import lines. The code is 12 lines.

**Prerequisites:** You should have finished the reading list in
`handoff.md` at the repo root, or at minimum read the first 6 entries
there (project-wide `CLAUDE.md`, `quest.md`, the architecture guide
up through Part 4, `docs/usage/writing-main.md`, @MBMRVZ, @RTMEIZ).
If you haven't done that yet, go do it now — this doc will be
confusing without the background, and the confusion will cost more
time than the reading would.

**Scope:** One crate (`regex`). Three files. **Not eight.** This
doc is narrow by design so surprises get focused attention. The
general 7-crate handoff at `handoff.md` covers the wider batch; this
doc covers the one you're doing right now.

---

## 1. Where we are

Phase 7 is the "prove toylang can link against arbitrary crates.io
Rust crates" phase. Two smoke tests have landed:

- **`uuid_test`** (2026-04-15, commit `df696c1` + follow-ups) —
  simplest possible shape: zero-arg static method returning a
  trivially-Copy value. Exercised the build orchestration (Phase 5)
  end-to-end and surfaced three latent issues that became arcana
  @MBMRVZ, @RTMEIZ, and the workspace-nesting fix.
- **`indexmap_test`** (2026-04-16, commit `df597bb`) — static method
  with three explicit generic type args
  (`IndexMap::new<i32, i32, RandomState>()`). Exercised 3-arg
  generics and proved S-fixed-impl-block selection works when the
  call site supplies the defaulted type arg explicitly.

Immediately after indexmap, planning the next crate (clap) surfaced
a compiler-feature gap: toylang's `"..."` string literals could not
be passed to any Rust function taking `&str`. That gap was fixed
the same day (commit `17841a3`) and is documented in the arcana
**@UTAIRZ** (`UnsizedTypesAppearInsideRef`). Read it — it explains
the six-touchpoint wiring pattern that both `Str` and `ByteSlice`
now follow.

Current state: **197 tests passing** (67 unit + 123 integration + 7
standalone). Clean tree. Ready for the next smoke test.

## 2. What you're proving

Your job: add a **`regex_test`** standalone smoke test proving toylang
can compile and link a program that:

1. Depends on the `regex` crate from crates.io (Phase 5's manifest
   machinery).
2. Constructs a `Regex` value via `Regex::new(&str)` — exercises the
   string-literal `&str` ABI path (Phase 7.5 / @UTAIRZ work).
3. Calls `.unwrap()` on the returned `Result<Regex, regex::Error>` —
   exercises Phase 6's inline-never unwrap wrapper on a non-stdlib
   Result for the first time.
4. Prints `"regex ok\n"` and exits zero.

This single program exercises **four compiler features end-to-end in
composition**: Phase 5 (build), Phase 6 (unwrap wrappers),
Phase 4 (I/O via `Write::write_all`), and the Phase 7.5 `&str` ABI.
If it passes on the first try, Phase 7's remaining four crates
(glob, toml, serde_json, reqwest) are almost certainly mechanical —
they share these primitives and add nothing structurally new. If it
fails, the failure mode tells us something specific we didn't know.

### Why regex specifically

Other candidates were considered:

- **rand** — too easy; mirrors uuid exactly. Passes almost trivially.
  Doesn't exercise `.unwrap()` or `&str`. Low signal.
- **clap** — has a synthetic `impl Into<Str>` generic on
  `Command::new` that toylang cannot supply. Still blocked; needs
  its own compiler work.
- **toml / serde_json** — same shape as regex (`&str` + generic
  `from_str<T>(&str)`), but don't exercise `.unwrap()`. Regex
  exercises one more path.
- **glob** — the "glob result iterator" can't be consumed in toylang
  (no iterator support). First pass would skip the `.unwrap()` path
  and just link-check. Lower info-gain than regex.
- **reqwest** — first-pass plan is link-only (no `get()` call);
  tests feature flags but nothing else.

Regex is the pick because it's the only remaining crate that stress-
tests **both** today's wins simultaneously without mixing in a third
unknown.

---

## 3. The three files you will create

Mirror `uuid_test` / `indexmap_test` exactly. No creativity needed —
the pattern is proven. Look at the existing two as the template for
structure, file naming, and test-function shape.

### File 1: `toylangc/tests/standalone/regex_test/toylang.toml`

```toml
[project]
name = "regex_test"
source = "main.toylang"

[rust-dependencies]
regex = "1"
```

Notes on the toml:
- `regex = "1"` pins to the 1.x major (current: 1.11+). We take
  whatever minor/patch cargo resolves. 1.x has been the stable line
  since 2018; no breakage expected in the smoke-test surface.
- No `features` block. Default features include `std`, `unicode-perl`,
  etc. which is what `\d` requires. If you later discover `\d` needs
  a feature that isn't default, add it here.
- `name = "regex_test"` becomes the binary name under
  `.toylang-build/target/debug/`.

### File 2: `toylangc/tests/standalone/regex_test/main.toylang`

**Starting point — iterate from here as errors guide you:**

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

Why each line:

| Line | Rationale |
|---|---|
| `use regex::Regex` | Named at the call site: `Regex::new<...>(...)`. Obvious. |
| `use regex::Error` | Implicit per @RTMEIZ. `Regex::new` returns `Result<Regex, regex::Error>`. The error-type parameter of the Result is round-tripped through the type system when `.unwrap()` resolves, so toylang needs to find it. Without this line you'll see `RustTypeNotImported { name: "Error", context: "as generic arg inside Result" }` or similar. |
| `use std::result::Result` | Implicit per @RTMEIZ. Same reason — `Result` is a named type flowing through. |
| `use std::io::stdout` | Named: the `stdout()` free-fn call. |
| `use std::io::Stdout` | Implicit per @RTMEIZ. The `&stdout()` expression has type `&Stdout`, which is the Self type of `Write::write_all`. Toylang needs the Stdout type imported even though the source never spells it. |
| `use std::io::Write` | Named: the trait in `Write::write_all`. |

**Note on the regex literal.** `"\\d+"` in toylang means a string
containing four characters: backslash, `d`, `+`, and... wait, three
characters. `\\` is the escape for a single backslash, then `d`, then
`+`. The compiled regex matches one or more digits. We're not
actually matching anything here — the test just proves `Regex::new`
returned `Ok(...)` without error, which for a syntactically valid
pattern it will. If you prefer a simpler pattern with no escapes,
`"a"` works too (matches the literal letter `a`).

**Why the `let` binding.** We bind the Regex to a variable we never
use. That's deliberate — same shape as `indexmap_test`, keeps the
test narrow to "the type-system path completed and produced a value."
We are *not* testing regex matching. Exercising `is_match(...)` would
add another call site and another failure mode; save that for later.

**Why `;` after `write_all(...)`.** Per @MBMRVZ. `fn main()` with no
declared return type must have a void tail. Forgetting the `;` gives
a clean compile error today, but it's the single most common thing
to forget. Add it.

### File 3: append to `toylangc/tests/standalone_tests.rs`

Mirror `test_standalone_indexmap` exactly. Copy that function, change
`indexmap` → `regex` throughout:

```rust
#[test]
fn test_standalone_regex() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/regex_test");

    // Clean any previous build output so the test is hermetic.
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

    let bin = build_dir.join("target/debug/regex_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run regex_test binary");
    assert!(
        run.status.success(),
        "regex_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("regex ok"),
        "expected 'regex ok' in stdout, got: {}",
        stdout,
    );
}
```

Insert it after `test_standalone_indexmap` in the same file. Keep
the ordering (alphabetical, or chronological — either works; match
whatever's there).

---

## 4. Toylang syntax cheatsheet (the things that will trip you up)

Most of this is also in `handoff.md`; duplicated here because regex
touches every pitfall.

### Generic type args are not turbofish

| Rust | Toylang |
|---|---|
| `Vec::<i32>::new()` | `Vec::new<i32, Global>()` |
| `Regex::new("\\d+")` | `Regex::new("\\d+")` (no type args — Regex::new takes `&str` and returns `Result<Regex, Error>`, no generic params) |
| `.unwrap()` | `.unwrap()` — works as a MethodCall; toylang routes to the Phase 6 wrapper automatically |

Toylang's lexer distinguishes `<` by whitespace: `a < b` is
comparison, `Name<T>` is generic-start. If in doubt, squeeze out
the space.

### All generic type params must be supplied explicitly

For `Regex::new("\\d+")` this is moot — `Regex::new` has zero
generic parameters (it's a plain inherent method on `Regex`, takes
an `impl Into<...>` in newer versions, but regex 1.x's public
signature is `new(re: &str) -> Result<Regex, Error>` per docs.rs).
So no turbofish is needed. **Verify this.** If you look at regex
docs and `Regex::new` has turned into `new<S: Into<String>>(...)` or
similar, you've hit a synthetic-generic problem like clap's — stop
and escalate.

### `&mut` doesn't exist in toylang

Not relevant for regex's smoke test — `Regex::new` takes `&str` not
`&mut`. But good to remember.

### `let` is always mutable

Don't write `let mut`. Just `let`. Trivial; worth noting.

### Byte strings vs. regular strings

- `"..."` → `&str` (ScalarPair fat pointer, 2-arg ABI)
- `b"..."` → `&[u8]` (ScalarPair fat pointer, 2-arg ABI)

Both use escape sequences: `\n \t \\ \0 \"`. In the regex test, the
pattern `"\\d+"` uses `\\` (escaped backslash) so the resulting
string contains one backslash character followed by `d+` — i.e. the
regex `\d+`. This is the same rule as Rust.

### Implicit imports per @RTMEIZ

Every Rust type that flows through the type system needs `use`,
including:
- The `Self` type of any trait call (here: `Stdout` for `Write::write_all`)
- Every non-primitive generic type arg, including implicit ones from
  nested generics (here: `Result` and `regex::Error` from the return
  type of `Regex::new`)

You'll see the structured error if you miss one. Read the `context`
field — it tells you exactly why each type was needed. Add the `use`
line. Retry.

### Void-return `fn main()` per @MBMRVZ

`fn main()` without `-> Type` must end with `;`-terminated statements
or a void-typed tail expression. Non-void tail expressions give
`TypeResolveError::MainMustReturnVoid` at type-resolve time.

---

## 5. How to run it

Follow `CLAUDE.md`'s build-redirect convention. Pipe to a fixed file
in `/tmp/` via `tee`; inspect in a separate command. **Don't chain
`| grep` or `| head` onto the same line** — you lose re-inspection.

### Just the new test

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_regex 2>&1 | tee /tmp/erw-regex-test.txt
grep "test result:" /tmp/erw-regex-test.txt
```

### Full suite after the test passes

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-regex-full.txt
grep "test result:" /tmp/erw-regex-full.txt
# Expected after this lands:
# test result: ok. 67 passed  (unit)
# test result: ok. 123 passed (integration)
# test result: ok. 8 passed   (standalone — was 7, +1 for regex)
```

### Timing expectations

- First run: **30–120 seconds** (cargo downloads regex + transitive
  deps: regex-syntax, aho-corasick, memchr). Network required first
  time.
- Subsequent runs: **5–15 seconds** (deps cached in
  `~/.cargo/registry` and the test's own `.toylang-build/target`).
- Full suite after cache is warm: 30–45 seconds.

If a single run sits at 0% CPU for more than 2 minutes, you may have
hit a deadlock. See failure class 7 below — rare, but it happened
pre-Phase 6.3.

---

## 6. Failure classes (how to diagnose what went wrong)

These are the classes you're most likely to hit, in rough order of
likelihood for this specific test. Classify first, then read the
matching section. **Don't try to fix compiler internals** — if you
hit anything beyond classes 1–3, escalate.

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

For regex specifically, the likely candidates you might miss from
the proposed program are:
- `regex::Error` — nested inside `Result<Regex, Error>`
- `std::result::Result` — the Result type itself
- `std::io::Stdout` — Self of the `Write::write_all` trait call

**Do not modify the compiler.** This class is a source-code problem.

### Class 2 — `MainMustReturnVoid { got: <some type> }`

**Symptom:** You forgot the `;` after `Write::write_all(...)` or
after the unwrap call.

**Fix:** Add `;`. Rerun.

### Class 3 — Parser errors

Most likely causes:
- Missing `;` after a `let`.
- Using Rust turbofish (`::<T>`) instead of toylang `::method<T>()`.
- Unknown escape in a byte or regular string. Supported escapes:
  `\n`, `\t`, `\\`, `\0`, `\"`. Nothing else.
- Typo in a type name.

**Fix:** Your source code. Not a compiler bug.

### Class 4 — Linker error on a rustc-mangled symbol `_ZN7regex...`

**Symptom:** Toylang compilation succeeds, `cargo build` fails at
link time with a missing symbol that looks like
`_ZN5regex4re_unicode...` or similar.

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
exits with a signal — 138 = SIGBUS (macOS), 139 = SIGSEGV (Linux).

**What it means:** ABI mismatch at a call site. Toylang and rustc
disagree about where a value lives.

**Debug tip:** Run under lldb:
```bash
lldb -batch -o run -o bt \
    toylangc/tests/standalone/regex_test/.toylang-build/target/debug/regex_test
```
Capture the backtrace. If the crash is in `__toylang_result_unwrap`
or immediately after it, the unwrap wrapper's ABI is suspect.

**Escalate.** Don't try to patch this alone.

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

- **Don't** actually call `.is_match(...)`. Our job is link-check
  plus the type-system path for unwrap. Real regex matching is later
  work; it'd add a method call, another string-literal param, and
  another failure mode. Keep the surface narrow.
- **Don't** change anything in `toylangc/src/`, `rustc-lang-facade/`,
  or `~/rust/`. If you think you need to, stop — the test has
  surfaced a real compiler gap that needs escalation, not a source
  edit.
- **Don't** add additional standalone tests in the same PR. One
  crate, one test, one commit.
- **Don't** try to handle iterators, mutation, or `.unwrap()` on
  anything other than the return of `Regex::new`. Those are
  different tests.
- **Don't** ignore warnings that come from `cargo build` of the
  generated wrapper crate. Unused-import warnings are expected
  (toylang's load-bearing `use` lines look unused to rustc).
  Anything else — read it.

The one-job rule: if a change isn't required to make
`test_standalone_regex` pass, don't make it. Note unrelated
observations in team chat or as separate PRs.

---

## 8. Pre-submission checklist

Before calling it done:

- [ ] `toylangc/tests/standalone/regex_test/toylang.toml` has
      `[project]` and `[rust-dependencies]` with `regex = "1"`.
- [ ] `toylangc/tests/standalone/regex_test/main.toylang` has the
      required imports and body. Exactly one `fn main()`.
- [ ] `toylangc/tests/standalone_tests.rs` has a matching
      `test_standalone_regex` function mirroring
      `test_standalone_indexmap`.
- [ ] `.toylang-build/` directories are NOT committed (root
      `.gitignore` excludes them — verify `git status` is clean of
      them).
- [ ] Isolated test passes:
      `cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_regex`
- [ ] Full suite passes with no regressions (197 → 198 tests).
- [ ] Full suite passes **twice in a row**. Second should be faster
      (cache warm). If it fails on the second but passed the first,
      you have a hermeticity bug — fix the test cleanup logic.
- [ ] `git status` shows only files under
      `tests/standalone/regex_test/` and the edit to
      `standalone_tests.rs`. No compiler source changes.
- [ ] Commit message style matches prior ones — dense, one
      paragraph, describes what and why. See
      `git log --oneline -5` for examples. Suffix
      `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
      only if you want to credit the assistant.
- [ ] Update `quest.md` (gitignored, stays local) to reflect Phase 7
      at 3/9 with a one-line note on what regex proved. Also bump
      the test-total line.
- [ ] Update `docs/architecture/rust-interop-guide.md` front-matter:
      test totals 197 → 198, Phase 7 status `2/9` → `3/9`, brief
      mention of regex under §10.7.

---

## 9. What passing regex unlocks

Direct unblocks:
- **toml** — the same shape (`from_str<T>(&str)`) with `Value`
  replacing `Regex`. Should be mechanical.
- **serde_json** — same shape as toml.
- **glob** — takes `&str`, first-pass plan omits the `.unwrap()`, so
  strictly easier than regex.

Still orthogonally blocked:
- **clap** — `Command::new(impl Into<Str>)`. Synthetic `impl Trait`
  generic is a separate compiler gap.

Not affected by this test:
- **rand** — free fn, no `&str`. Would have passed before this test.
- **reqwest** — first-pass plan is link-only, same story.

So after regex lands, the remaining batch is: clap (own mini-phase,
needs compiler work), and 4 mechanical crates (rand, glob, toml,
serde_json, reqwest) that should go fast.

---

## 10. If you get stuck — what to escalate with

Post in team chat with:

1. Which failure class from §6 (or "something else").
2. The exact command you ran.
3. The tail of `/tmp/erw-regex-test.txt` (last 60–100 lines).
4. What you've already tried.
5. The current contents of your `main.toylang` (copy-paste inline).
6. The `git diff` of your changes (or just the file list — whatever's
   cleaner).

The goal is to make it easy for the reviewer to either:
(a) spot the obvious thing you missed, or
(b) understand the real compiler gap you've surfaced without having
    to re-run everything locally.

**Do not attempt ABI fixes, linker fixes, or fork patches.** Those
affect everything downstream. Your job is to prove the test works
— or to surface the blocker clearly. Both outcomes are valuable.

---

## 11. Summary

Three files. Twelve lines of toylang source (plus the Rust test
function which is just `test_standalone_indexmap` with names
swapped). Passing `test_standalone_regex` bumps Phase 7 to 3/9,
tests to 198, and confirms Phase 7's remaining mechanical batch
(glob, toml, serde_json) is indeed mechanical.

Most of this doc is prep for the 10% chance something goes wrong in
an informative way. The 90% expected case is: add the three files,
run the test, see "regex ok" in stdout, commit, ship.

If you've read this far and the program in §3 looks mysterious,
that's the signal to go back to the reading list in §0 and work
through the arcana. The arcana exist because each one is an
expensive lesson someone already paid for. You get the cheap
version.

Good luck.
