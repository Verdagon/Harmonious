# Handoff: Phase 7 — uuid smoke test project

**Audience:** Junior engineer joining the erw project, no prior compiler or rustc-internals experience required.
**Estimated effort:** 1–2 days if things go smoothly. 3–4 if something breaks in a way we didn't anticipate.
**Prerequisites:** Comfortable with Rust. Willing to read code in other people's crates. Comfortable reading LLVM IR when things break (we'll teach you enough; you don't need to know it coming in).

---

## Read this in order

Before touching any code, read these top to bottom. **Don't skip.** Each one explains context you'll need to make good decisions.

1. **This doc** — sets the stage and gives you the one task.
2. **`CLAUDE.md`** at the project root — the project-wide instructions, including build conventions.
3. **`quest.md`** — project phase history. Skim Phases 1–6 to understand what's built; read Phase 7 carefully (it's your job).
4. **`docs/architecture/rust-interop-guide.md`** Parts 1–4 — how toylang compiles. The full doc is long; stop after "Part 4: The LLVM Backend" for now. Come back to later parts only if you need them for debugging.
5. **`docs/architecture/rust-interop-guide.md`** §10.5 — the `toylang.toml` + `toylangc build` story. This is the mechanism your test will exercise end-to-end.
6. **`docs/usage/rebuilding-rustc-fork.md`** — you probably won't need to rebuild the fork, but if you do, this doc is the difference between a 10-minute detour and a 2-hour rabbit hole. Know it exists.

If you find yourself more than 30 minutes into confusion, come back and reread the section that's relevant. These docs encode a lot of hard-won context.

---

## What you're doing

You are adding **one** standalone test project under `toylangc/tests/standalone/uuid_test/` and **one** test function in `toylangc/tests/standalone_tests.rs` that builds and runs it. The project is a toylang program that calls `uuid::Uuid::new_v4()` and prints "uuid ok\n". It proves toylang can link against and call into a real Rust crate from crates.io that toylangc itself does not depend on.

This test is the bridge from Phase 5 ("cargo resolves deps") to Phase 7 ("toylang calls into deps"). It's deliberately the simplest target: no `.unwrap()`, no iterators, no closures, no trait machinery you haven't already seen. If this works, eight more projects follow the same pattern. If it breaks, you've saved everyone eight rounds of the same debugging.

### Success criteria

1. A new directory `toylangc/tests/standalone/uuid_test/` containing:
   - `toylang.toml` declaring `uuid` as a Rust dependency
   - `main.toylang` with the program below
2. A new `#[test] fn test_standalone_uuid()` in `toylangc/tests/standalone_tests.rs` that:
   - Builds the project via `toylangc build`
   - Runs the resulting binary
   - Asserts stdout contains "uuid ok"
3. `cargo +rustc-fork test -p toylangc --test standalone_tests` passes (all 5 tests: the 4 existing + your new one).
4. No regressions — the full suite (`cargo +rustc-fork test -p toylangc`) still shows 60 unit + 116 integration + 5 standalone = 181 tests, 0 failures, 0 ignored.

### Non-goals

- **Don't** add the other 8 crates yet. Prove the mechanism with uuid first.
- **Don't** refactor anything in `llvm_gen.rs` or the facade. If your test reveals a bug that needs a fix in the compiler, stop and escalate before implementing — we want to know.
- **Don't** add features to toylang (new syntax, new types, new AST nodes). Everything you need is already there.

---

## The program you're writing

(Earlier drafts of this doc omitted: the semicolon after the `let`,
the trailing `;` after `Write::write_all`, and the
`use std::io::Stdout`, `use std::result::Result`, and
`use std::io::Error` imports. All fixed inline; see the commit trail
for context. Three rules learned the hard way here:

1. `fn main()` in toylang must return void, same as Rust. If the final
   expression has a non-void type, toylang's internal main gains an sret
   return, which breaks the Rust-ABI extern wrapper that was generated
   assuming `fn main() -> ()`. The mismatch manifests as a SIGBUS during
   the internal main's final store into a dangling/read-only sret
   buffer. Fix: always terminate the last statement of main with `;` if
   it has a non-void type.
2. Every Rust type that flows through toylang's type system — even as
   the type of a discarded `ExprStmt` — must be `use`-imported so it
   lands in `__lang_stubs`'s `pub use` re-exports and
   `find_rust_type_def_id` can find it. `Result<(), Error>` here needs
   both `use std::result::Result` and `use std::io::Error`.
3. The `Self` type of a trait method call (here `Stdout` on
   `Write::write_all(&stdout(), ...)`) also needs to be `use`-imported
   for the same reason, even though toylang never names it explicitly.)

Exact text — save as `toylangc/tests/standalone/uuid_test/main.toylang`:

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

Seven lines. Every line already has precedent in existing integration tests.

### Why each line works

| Line | Mechanism | Prior test that exercises it |
|------|-----------|-------------------------------|
| `use uuid::Uuid` | Phase 4 `use`-import for a struct | many — see tests using `use std::vec::Vec` |
| `use std::io::stdout` | Phase 2 `use`-import for a free fn | `test_stdout_call` |
| `use std::io::Write` | Phase 1 `use`-import for a trait | `test_trait_static_call_clone_vec` |
| `let id = Uuid::new_v4()` | Phase 1 inherent static method returning a struct | `test_trait_static_call_clone_vec` (Clone::clone); `test_stdout_call` (Stdout return) |
| `Write::write_all(&...)` | Phase 1 trait method call with `&expr` ref | `test_stdout_write_all` |
| `b"uuid ok\n"` | Phase 3 byte-string literal | `test_byte_string_passed_to_rust_fn` |
| `stdout()` | Phase 2 use-imported free function | `test_stdout_call` |

If you want to see a working program that does nearly the same thing, grep integration_tests.rs for `test_stdout_write_all` and read it. Your program is that program with `Uuid::new_v4()` inserted above the `Write::write_all` call. The `id` variable isn't used — that's deliberate, keeps the test narrow to "did the call succeed and return something toylang can bind."

### The `toylang.toml`

Save as `toylangc/tests/standalone/uuid_test/toylang.toml`:

```toml
[project]
name = "uuid_test"
source = "main.toylang"

[rust-dependencies]
uuid = { version = "1", features = ["v4"] }
```

The `features = ["v4"]` is required. `new_v4()` lives behind the `v4` feature flag in the uuid crate. Without it the crate compiles but `Uuid::new_v4` doesn't exist, and your build will fail with "method not found" at toylang's type-resolution stage.

### The test function

Add this to `toylangc/tests/standalone_tests.rs`, below the existing tests:

```rust
#[test]
fn test_standalone_uuid() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/uuid_test");

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

    let bin = build_dir.join("target/debug/uuid_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run uuid_test binary");
    assert!(
        run.status.success(),
        "uuid_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("uuid ok"),
        "expected 'uuid ok' in stdout, got: {}",
        stdout,
    );
}
```

Notes on the test:
- It uses `CARGO_MANIFEST_DIR` so the project directory is deterministic — unlike the other 4 tests in this file, which use `tempfile::tempdir()` for hermetic tests with dynamically-generated content. Your project is checked into the repo, so it has a stable path.
- The `remove_dir_all` at the top is important. `toylangc build` caches heavily via cargo; a stale `.toylang-build/` from a previous run can hide regressions. The existing 4 tests avoid this by always starting fresh (tempdir), but yours reuses a stable location.
- `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` must be set on the *run* command, not just the build. The binary links against `libstd-*.dylib` from the rustc-fork sysroot; without these env vars it won't find them at runtime. The existing `run_build` helper already sets them for the build; you must pass them again for the run.
- `stdout().contains("uuid ok")` rather than equality. There may be other output from rustc or toylang on stdout — match loosely.

---

## How to run and what to expect

### Running just your test

```bash
# From /Users/verdagon/erw (the project root):
cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_uuid 2>&1 | tee /tmp/erw-uuid-test.txt
grep "test result:" /tmp/erw-uuid-test.txt
```

**IMPORTANT** — follow the `CLAUDE.md` build convention: always pipe cargo/sbt output to a fixed file in `/tmp/` using `tee` as the last command. Do NOT chain `| grep` or `| head` onto the same command — you'll lose the ability to re-inspect the output without rerunning the expensive build. Run the build and the inspection as two separate commands. The example above shows the correct pattern.

### Running the full suite (you MUST do this before declaring victory)

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-uuid-test.txt
grep "test result:" /tmp/erw-uuid-test.txt
```

Expected three lines:
```
test result: ok. 60 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 116 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in ~25s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in ~10s
```

The integration test count (116) and unit test count (60) must not change. Only the standalone count goes from 4 to 5.

### Timing expectations

- First run of `test_standalone_uuid`: 30–90 seconds. Cargo downloads uuid and its 1–2 transitive dependencies from crates.io (requires network the first time), compiles them, then runs toylangc on your .toylang source.
- Subsequent runs: 10–20 seconds. Dependencies are cached; only your code recompiles.
- If a run takes more than 3 minutes without progress, something is wrong — see "If things break" below.

---

## If things break

Start with the build output. `toylangc build` is chatty; the failure mode usually names itself. Here are the failure classes, in rough order of likelihood:

### 1. "method `new_v4` not found" at toylang type-resolution

**Symptom:** toylangc exits with a TypeResolveError mentioning `new_v4` or `Uuid`. The error will be printed before any LLVM codegen happens.

**Most likely cause:** you forgot `features = ["v4"]` in `toylang.toml`, so the `new_v4` associated function was cfg'd out of the uuid crate.

**Fix:** add the feature and re-run. If already present, double-check the TOML syntax (the feature array is strings, not identifiers).

**Less likely:** you got the `use` path wrong. The `uuid` crate re-exports `Uuid` at the crate root (`uuid::Uuid`), not at e.g. `uuid::core::Uuid`. If you wrote something else in the `use` line, fix it.

### 2. Linker error: undefined symbol `_ZN...uuid...new_v4...`

**Symptom:** `toylangc build` completes the toylang compilation stage but `cargo build` (which runs after) fails during linking.

**Most likely cause:** toylang's LLVM backend declared `Uuid::new_v4` with the wrong ABI — e.g. as returning a direct scalar when rustc actually returns it via sret. This is the class of bug documented in @ACRTFDZ. It would mean we have an ABI coverage gap for 16-byte struct returns.

**Debug steps:**
- Look at the linker error. If the missing symbol is a toylang-mangled name (`__toylang_*`), the problem is different (see case 4). If it's a rustc-mangled name (`_ZN...`), it's an ABI coercion issue.
- Check what ABI rustc actually uses for `Uuid` returns by reading rustc's output on a trivial Rust program: `rustc -Zunpretty=normal` won't help; instead write a 3-line Rust program, build with `--emit=llvm-ir`, and read the `.ll`. See how `declare` lines list the return type.
- Compare against what toylang declared. You can find toylang's emitted LLVM IR in `.toylang-build/target/debug/build/*/out/*.ll` after a failed build.

**Escalate:** this is not a junior-engineer fix. Stop and ask. The fix likely goes in `llvm_gen.rs` around the FnCall use-import path and may need a new `CoercedReturn` variant or an ABI query we haven't used before.

### 3. Segfault at runtime

**Symptom:** build succeeds, binary exists, but running it exits non-zero with no output or a crash.

**Most likely cause:** another ABI issue — same family as case 2, but the mismatch manifests at runtime instead of link time. For example, toylang stores the return of `new_v4()` into an alloca of the wrong size.

**Debug steps:**
- Run the binary under `lldb`: `lldb .toylang-build/target/debug/uuid_test`, then `run`, then `bt` after it crashes. The backtrace will show whether the crash is inside uuid code (ABI mismatch on the return path) or inside the toylang-emitted code (alloca / bit-copy bug).
- If the crash is inside uuid code dereferencing something: toylang probably stored the 16-byte return value into an 8-byte alloca or similar.
- If you see `Write::write_all` in the backtrace reading garbage: the `stdout()` result was corrupted, but we know that works from Phase 4. More likely the uuid result corrupted something adjacent on the stack.

**Escalate** same as case 2. Don't try to fix ABI bugs solo.

### 4. Linker error: undefined symbol `__toylang_*`

**Symptom:** the missing symbol starts with `__toylang_`.

**Most likely cause:** the partitioner didn't emit one of toylang's wrapper functions, or toylang declared one it didn't generate. This is the class of bug Phase 6 step 1 was written to fix.

**Debug steps:**
- Identify which symbol is missing. `__toylang_accessor_*`, `__toylang_option_unwrap`, `__toylang_result_unwrap` are all wrappers we already have. If it's one of those, the visibility-override hook isn't firing.
- `__toylang_impl_main` missing means main wasn't emitted — maybe an issue with `fn main()` recognition.

**Escalate.** This class of failure is unusual and would indicate a regression in Phase 6 work.

### 5. Build hangs forever (0% CPU, no progress)

**Symptom:** `cargo +rustc-fork test` sits at 0% CPU for >2 minutes without output progress.

**Most likely cause:** the mutex deadlock from @GCMLZ, reintroduced somehow. Should not be possible after the Phase 6 step 3 two-family trait split.

**Debug:** Ctrl+C, then `sample <pid>` at the point it hangs (or use `lldb -p <pid>`) to see what thread is blocked and where.

**Escalate.** This would be a regression we need to know about immediately.

### 6. "Can't find crate `rustc_abi`" at toylangc build time

**Symptom:** cargo errors about missing rustc-internal crates.

**Cause:** rustc-fork toolchain sysroot is missing rustc-dev libs. This is the exact failure mode `docs/usage/rebuilding-rustc-fork.md` step 5 covers.

**Fix:** run the install.sh reinstall step from the doc. Shouldn't happen unless you rebuilt the fork yourself.

### What "escalate" means

Message the team chat with:
1. Which failure class (1–6)
2. The exact command you ran
3. The tail of `/tmp/erw-uuid-test.txt` (last 50 lines is usually enough)
4. What you've already tried

Do NOT try to fix ABI bugs, linker bugs, or deadlock bugs on your own as your first Phase 7 task. Those fixes land in the compiler and are reviewed carefully. Your job is to prove the test works or surface the blocker; both outcomes are valuable.

---

## Things that are not your problem

During this task, you may notice things that look wrong. Don't fix them:

- **Warnings in `cargo build`.** There are 5 pre-existing dead-code warnings in `toylangc/src/toylang/callbacks_impl.rs` (e.g., `field 'name' is never read`). They've been there since Phase 6. Leave them.
- **Comments mentioning `is_scalar_pair_type`.** Shouldn't exist anymore after tech debt #6 cleanup, but if you grep and find one in `docs/historical/` — that's historical record, not a live comment. Leave it.
- **`handoff.md` at the project root.** That's a previous handoff doc for Phase 6 steps 2/3. Superseded. If you want to cross-reference it to see how prior junior engineers were onboarded, fine — but it describes tasks already done.
- **Any reference to `toy_layout_of` or `toy_mir_shims` in docs/historical/.** Renamed to `lang_*` in Phase 6 step 3 cleanup, but historical docs keep their old names. Leave them.

The one-job rule: if a change isn't required to make `test_standalone_uuid` pass, don't make it. Create a separate PR (or flag it in the team chat) for anything else you notice.

---

## Pre-submission checklist

Before you say "done":

- [ ] `toylangc/tests/standalone/uuid_test/toylang.toml` exists, has `[project]` and `[rust-dependencies]` sections, includes `features = ["v4"]`.
- [ ] `toylangc/tests/standalone/uuid_test/main.toylang` exists with the 7-line program above (spelled exactly).
- [ ] `toylangc/tests/standalone_tests.rs` has a `test_standalone_uuid` function below the existing tests.
- [ ] `.toylang-build/` is **not** committed — verify with `git status` that it doesn't appear as an added directory. The root `.gitignore` already excludes it.
- [ ] `cargo +rustc-fork test -p toylangc --test standalone_tests test_standalone_uuid` passes in isolation.
- [ ] `cargo +rustc-fork test -p toylangc` passes with `60 + 116 + 5 = 181` tests, 0 failures, 0 ignored.
- [ ] You've run the test suite **twice in a row** — second run should be faster (cache warm). If the second run fails but the first passed, you have a caching/cleanup bug in the test.
- [ ] No changes to `llvm_gen.rs`, `rustc-lang-facade/`, or the rustc fork (`~/rust/`). If any of those files changed, you went off-plan — escalate.
- [ ] You've updated `quest.md` with a one-line note under Phase 7 marking the uuid smoke test as done, but not claiming the rest of Phase 7 is done.

Commit message: `Add uuid standalone test project (Phase 7 smoke test)`. Body should mention the test was added to validate that toylang can call into crates.io deps, note it exercises use-imports, byte strings, and I/O from Phase 1–4, and reference this handoff doc.

---

## If everything works on the first try

That's the best outcome — it means Phase 5's build orchestration and Phase 1–4's call machinery compose cleanly for a realistic third-party crate. Report back and we'll sketch out the remaining 8 projects as a follow-up batch.

## If it works after one or two minor fixes

Write up what you had to change and why in the PR description. That feedback shapes how we write the rest of Phase 7.

## If you hit a real bug in toylang or the facade

You will have done something valuable: found the first real-world gap in toylang's Rust interop that all the synthetic integration tests missed. Write up the failure mode, attach the `.ll` file and the error log, and hand it back. Don't try to fix it — fixing compiler ABI bugs requires context this doc can't give you, and silent near-misses in ABI handling are exactly the class of bug that breaks two years later in production.

Good luck. Ask questions early and often — a 5-minute clarification saves a half-day rabbit hole.
