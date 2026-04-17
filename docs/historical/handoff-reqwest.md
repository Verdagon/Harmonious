# Handoff: Phase 7 crate #8 — `reqwest` standalone smoke test

> **Status: LANDED 2026-04-17. Preserved as historical context.**
>
> **Outcome:** First-try green, zero iteration on imports or syntax,
> zero compiler changes. Three files exactly as §3 starting point
> specified. Isolated test completed in 22s on first run (within the
> budgeted 90s–3min window). Full suite passed on the first run after
> landing and again on the hermeticity second run. Bumped tests
> 208 → 209 and Phase 7 7/9 → 8/9. Commit `bfa7355`.
>
> **Prediction accuracy: the 90% path.** §6's main called-out risk —
> Phase 5's `render_dep()` silently dropping or mangling the
> `features = ["blocking"]` array — did not fire. Post-build
> inspection of `.toylang-build/Cargo.toml` shows `reqwest = {
> version = "0.11", features = ["blocking"] }` verbatim at line 11,
> confirming the positive precedent from uuid's `features = ["v4"]`
> extends cleanly to a feature that gates an entire module. The
> `Client::new()` call site behaved identically to the `Uuid::new_v4()`
> and `thread_rng()` shapes; Drop glue for `Client`'s internal
> `Arc<ClientRef>` ran via rustc's Instance collector without any
> `__toylang_*` or `drop_in_place` linker error.
>
> **The load-bearing question Phase 5 quietly answered.** uuid's
> feature test was cosmetic — v4 activates a constructor, but
> `uuid::Uuid` compiles fine without it. reqwest is the first case
> where the feature gate is load-bearing for the toylang test to
> compile at all (`reqwest::blocking` doesn't exist without
> `blocking`). That transforms the feature-forwarding case from
> "one positive datapoint, passing" to "positive and strictly more
> demanding, passing." Phase 5's detailed-dep infrastructure is now
> proven against the stress case.
>
> **The deep-transitive-dep proof point.** reqwest pulls in ~100
> transitive deps (tokio, hyper, h2, mio, plus their closures).
> Prior Phase 7 crates were all shallow (≤15 deps). 22s full
> resolve + compile on a warm sccache on an M-series Mac; longer
> on a cold cache. No cargo workspace leakage, no DYLD issues, no
> stub-loader confusion. The Phase 5 infrastructure (workspace
> nesting fix, @MRRIWMZ manifest re-read, `RUSTC_WORKSPACE_WRAPPER`
> primary-package gating) held up.
>
> **Deferred: `reqwest::blocking::get(url)`.** The handoff's §2
> explicitly deferred this shape. It would exercise a novel
> `&T`-type-arg generic call (`get<&str>("...")`) that has zero
> precedent in the integration corpus. Worth a follow-up ticket
> once someone has the bandwidth to either verify toylang's parser
> accepts reference types in type-arg position or to document the
> gap as the next Phase 7 arcana. Not blocking for Phase 7
> completion — `Client::new()` proves what needed proving.
>
> **Pattern now: four consecutive first-try Phase 7 tests.** toml
> → glob → rand → reqwest, no compiler changes, no iteration.
> The first three Phase 7 tests (uuid, indexmap, regex) each
> surfaced a gap — @MBMRVZ+@RTMEIZ, the @UTAIRZ `&str` rewire,
> @IVTDBTZ dispatch fix. The fourth (serde_json) surfaced @ELASZ.
> The next four all passed first-try. The "latent until the right
> crate shape surfaces it" pattern is no longer a hot path for
> Phase 7 test-authoring; if clap ever gets the `impl Into<Str>`
> fix, its smoke test should also be mechanical.
>
> **Outcome totals at land time:**
> - 67 unit + 129 integration + 13 standalone = 209
> - 0 failed, 0 ignored
> - Phase 7 at 8/9 (only clap remaining, blocked on `impl Into<Str>`)
>
> Original handoff body preserved below verbatim for historical
> reference.
>
> ---

**Audience:** Engineer picking up the last unblocked Phase 7 crate.
This is a tight one-test handoff; the pattern is extremely well-trodden
after three consecutive first-try landings (toml, glob, rand).

**Estimated effort:** 30–60 minutes. First cargo resolve of the reqwest
dep tree is the slowest step (reqwest pulls ~100 transitive deps —
tokio, hyper, rustls or native-tls, etc., most of which are gated off
by disabling default features).

**Prerequisites.** You should have already read:

1. `handoff-glob-rand.md` at `docs/historical/` — the template this
   doc mirrors, with the full failure-class taxonomy (§7) and
   scope-discipline rules (§8) that apply here verbatim. **Don't
   re-read those sections below; this doc cross-refs them.**
2. Project root `CLAUDE.md` — build-redirect convention. Use
   `/tmp/erw-reqwest.txt` as the fixed output file this session;
   never chain `| grep` onto the `tee`.
3. `docs/architecture/rust-interop-guide.md` §10.7 — shows 7/9 Phase
   7 complete at the time this handoff was written.

**Scope.** One crate. Three files. One commit to land the test, one
retrospective commit to move this handoff to `docs/historical/`.

---

## 1. Where we are

Phase 7 is at 7/9 after `glob_test` and `rand_test` landed
2026-04-17 (both first-try, zero compiler changes). Current totals:
67 unit + 129 integration + 12 standalone = **208 tests, 0 failed, 0
ignored.**

Done so far: `uuid_test`, `indexmap_test`, `regex_test`, `toml_test`,
`serde_json_test`, `glob_test`, `rand_test`.

Remaining after this handoff: `clap_test` only. It's blocked on an
orthogonal `impl Into<Str>` synthetic-generic gap and is a separate
mini-phase requiring compiler work, not a test-authoring task.

**After you finish** Phase 7 will be at **8/9 with 209 tests** (or
210 if a novel gap requires adding a regression unit test, same
pattern as @ELASZ — unlikely per the analysis below).

## 2. What you're proving

### `reqwest_test` (target: first detailed-dep usage with a feature flag)

A toylang program that:

1. Depends on `reqwest = { version = "0.11", features = ["blocking"] }`
   — the **first standalone test to exercise Phase 5's detailed-dep
   path end-to-end**. The simple form `reqwest = "0.11"` would not
   enable the `blocking` module (`reqwest::blocking` is feature-gated),
   so the test can't be reduced to a simple-dep case.
2. Calls `Client::new()` — a **zero-arg inherent static method on a
   use-imported struct**, shape-identical to `Uuid::new_v4()` (Phase 7
   #1) and `thread_rng()` (rand_test, just landed). Returns a `Client`
   (non-Copy, has Drop).
3. Binds the `Client` to a variable; does **not** call any methods on
   it (no `.get()`, no HTTP). Drop glue runs at end of `main()` via
   rustc's normal codegen.
4. Prints `"reqwest ok\n"` and exits zero. **No network call.**

This exercises Phase 2 (use-imported inherent static method), Phase 5's
detailed-dep Cargo.toml emission (verified working per uuid_test's
`features = ["v4"]` round-trip at `toylangc/src/build.rs:98-112`), and
Phase 4 (I/O). Zero new features; pure mechanical composition.

### Why `Client::new()` and not `blocking::get`

The obvious shape is `reqwest::blocking::get("http://...")` — but that
requires (a) a live HTTP call or a URL that fails fast, and (b) a
toylang generic-call site with a reference-type type arg (`get<&str>`).
Neither is tested in the integration corpus (grep for `<&` in
`integration_tests.rs` returns zero matches). `blocking::get` would
likely surface a novel shape — the same "latent until the right crate
shape surfaces it" pattern that produced @UTAIRZ / @IVTDBTZ / @ELASZ.

`Client::new()` dissolves both risks. It's the same shape toylang has
now compiled seven times. First-pass scope is proving the `blocking`
feature gate resolves and toylang links against reqwest's Client type
— a harder-to-pull-in API surface than the ones landed so far. Testing
`blocking::get` is a valuable follow-up but belongs in its own ticket
once the `&T`-type-arg shape is either verified to work or documented
as the next Phase 7 arcana.

---

## 3. `reqwest_test` — the three files

### File 1: `toylangc/tests/standalone/reqwest_test/toylang.toml`

```toml
[project]
name = "reqwest_test"
source = "main.toylang"

[rust-dependencies]
reqwest = { version = "0.11", features = ["blocking"] }
```

**Pin to 0.11, not 0.12.** reqwest 0.12 reorganized some modules and
changed a handful of types; 0.11's `blocking::Client::new()` signature
is stable and well-understood. If 0.12 ships during the life of this
handoff, stick with 0.11 anyway — the shape-identicality argument only
holds if the signature matches the starting code below.

**Default features are intentionally left enabled.** The smallest
possible feature set that includes `blocking` works, but disabling
defaults pulls in complexity (JSON support toggles, TLS backend
selection — rustls vs native-tls) that could surface a reqwest build
error unrelated to toylang. Keep it simple first pass; tighten later if
the first run takes forever due to transitive compilation.

### File 2: `toylangc/tests/standalone/reqwest_test/main.toylang`

**Starting point:**

```
use reqwest::blocking::Client
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let client = Client::new();
    Write::write_all(&stdout(), b"reqwest ok\n");
}
```

Why each import (per `@RTMEIZ`):

| Line | Reason |
|---|---|
| `use reqwest::blocking::Client` | Named at the call site as `Client::new()`. Canonical path in reqwest 0.11 is `reqwest::blocking::Client`. |
| `use std::io::stdout` / `Stdout` / `Write` | Standard I/O imports, same pattern as every other Phase 7 test. |

**Notes on the program:**

- **No error-type or Result imports.** `Client::new()` is infallible in
  reqwest 0.11's blocking API (it returns `Client` directly, not
  `Result<Client, Error>`). This is one reason it's strictly simpler
  than the `.unwrap()`-using tests (regex/toml/serde_json).
- **`let client = ...` is load-bearing**, same reason as glob's
  `let result` and rand's `let rng`: keeps the value alive to
  end-of-`main` so Drop runs via rustc's normal codegen.
- **No methods on `client`.** Don't call `.get(...)`, `.post(...)`,
  `.builder()`, nothing. Matches the scope discipline from glob/rand.
- **Trailing `;` after `write_all(...)`.** Required per @MBMRVZ.

### File 3: append to `toylangc/tests/standalone_tests.rs`

Mirror `test_standalone_rand` exactly (most recent template) with
`rand` → `reqwest`. Append at the end of the file — the file's
ordering has decayed from alphabetical to roughly chronological; just
add at the bottom.

```rust
// Phase 7 crate #8: reqwest — first standalone test to exercise
// Phase 5's detailed-dep path end-to-end (features = ["blocking"]).
// Uses `Client::new()` rather than `blocking::get(url)` to avoid a
// novel generic-with-reference-type-arg shape and a network call;
// shape-identical to Uuid::new_v4() and thread_rng().
#[test]
fn test_standalone_reqwest() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/reqwest_test");

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

    let bin = build_dir.join("target/debug/reqwest_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run reqwest_test binary");
    assert!(
        run.status.success(),
        "reqwest_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("reqwest ok"),
        "expected 'reqwest ok' in stdout, got: {}",
        stdout,
    );
}
```

Reuses the existing helpers `run_build()` and `sysroot_lib()` at the
top of the file.

---

## 4. Doc updates (4 locations)

After the test passes, bump four spots in two files:

- `quest.md` — Phase 7 heading `(7/9 done)` → `(8/9 done)`; test
  totals `12 standalone = 208` → `13 standalone = 209`; add a "What
  landed" block following the rand_test pattern; drop `reqwest_test`
  from the Remaining table (leaves only `clap_test`); update
  "Currently 208" → "Currently 209".
- `docs/architecture/rust-interop-guide.md` front-matter line 3 —
  `12 standalone tests` → `13 standalone tests`.
- Same file line 55 and line 139 — `7/9 done` → `8/9 done`;
  `Remaining: 2` → `Remaining: 1`.
- Same file §10.7 — add `reqwest_test` bullet to Done list (following
  `rand_test`'s format); shrink the Remaining table from 2 rows to 1
  (just clap).

**Commit style.** Dense one-paragraph message, same shape as the glob
and rand commits (see `git log --oneline -3`). End with
`Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

---

## 5. How to run

Per CLAUDE.md: pipe to `/tmp/erw-reqwest.txt` via `tee`, inspect with a
separate command. **Never chain** `| grep` / `| head` / `| tail` onto
the same line as the `tee`.

### Isolated

```bash
cargo +rustc-fork test -p toylangc --test standalone_tests \
    test_standalone_reqwest 2>&1 | tee /tmp/erw-reqwest.txt
grep "test result:" /tmp/erw-reqwest.txt
```

### Full suite (run twice for hermeticity)

```bash
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-reqwest.txt
grep "test result:" /tmp/erw-reqwest.txt
# second run — should still pass
cargo +rustc-fork test -p toylangc 2>&1 | tee /tmp/erw-reqwest.txt
grep "test result:" /tmp/erw-reqwest.txt
```

Expected after landing:

```
test result: ok. 67 passed     (unit)
test result: ok. 129 passed    (integration)
test result: ok. 13 passed     (standalone — was 12, +1 for reqwest)
```

### Timing expectations

- **First run: 90 seconds – 3 minutes.** reqwest has by far the deepest
  dep tree of any Phase 7 crate so far — tokio, hyper, and their
  transitive deps take real time to compile even with only `blocking`
  enabled. glob and rand ran in ~10s; budget ~10x for reqwest.
- **Subsequent runs: 8–15s** (deps cached).
- If the first run sits at 0% CPU for >5 min — Class 7 (possible
  deadlock); escalate per handoff-glob-rand §7.

---

## 6. Failure classes

Same 8-class taxonomy as `handoff-glob-rand.md` §7, with one
reqwest-specific risk worth calling out:

**Most likely novel gap — Class 0 risk: Phase 5 feature forwarding.**

Phase 5's `DepSpec::Detailed` path is tested at unit level
(`manifest::tests::test_parse_detailed_dep`) and works end-to-end for
uuid's `features = ["v4"]` — verified by reading
`.toylang-build/Cargo.toml` after a successful uuid_test run (contains
the features array unchanged). That's a positive precedent, but it's
the only positive precedent. If reqwest's specific feature combination
produces an unexpected Cargo.toml emission bug — e.g., the `blocking`
string gets mangled by `render_dep()` at `build.rs:98-112` — you'll
see either:

- cargo error: `feature "blocking" not found for reqwest` → features
  array isn't making it through
- cargo error: `could not find 'blocking' in 'reqwest'` → feature made
  it but reqwest compilation failed for an unrelated reason

**Diagnosis:** read the generated `.toylang-build/Cargo.toml` after
the failed build:

```bash
cat toylangc/tests/standalone/reqwest_test/.toylang-build/Cargo.toml
```

If `reqwest = { version = "0.11", features = ["blocking"] }` is
verbatim there, the emission is correct — the error is downstream
(reqwest build fault). If the features array is missing or malformed,
that's a Phase 5 gap and should be documented as arcana before fixing.

**All other failure classes** — Class 1–7 — apply unchanged from
handoff-glob-rand §7. Most likely for this test specifically: **none**
(the shape is strictly simpler than rand's; no `Result`, no
`.unwrap()`, no generic args).

---

## 7. Scope discipline

Same rules as prior handoffs:

- **Don't call `blocking::get(url)`** or any HTTP method. Client::new
  only. The `get` shape is a separate testing goal with its own risks
  (novel `&T` type arg).
- **Don't `.unwrap()` anything.** `Client::new()` returns `Client`
  directly, not `Result`. Nothing to unwrap.
- **Don't touch compiler source.** Same rule as every Phase 7 handoff.
  If a novel gap surfaces (Class 0 above or anything else), stop and
  escalate per the bug-report template at
  `docs/historical/bug-report-regex-static-call-misclassified-as-trait.md`.
- **Don't disable default features** unless first pass fails due to
  extreme compile time. If you do disable, document why in the commit
  message.
- **Don't pin to 0.12.** Stick with 0.11.
- **Don't update this handoff file.** When the test lands, add a
  `LANDED` retrospective block at the top (following the
  `docs/historical/handoff-glob-rand.md` template) and `mv` to
  `docs/historical/`. Separate retrospective commit, same cadence as
  glob-rand.

---

## 8. Pre-submission checklist

- [ ] `toylangc/tests/standalone/reqwest_test/toylang.toml` has
      `[project]` and `[rust-dependencies]` with `reqwest = { version
      = "0.11", features = ["blocking"] }`.
- [ ] `toylangc/tests/standalone/reqwest_test/main.toylang` has the
      4 imports and the 2-line body. Exactly one `fn main()`.
- [ ] `toylangc/tests/standalone_tests.rs` has a matching
      `test_standalone_reqwest` function.
- [ ] `.toylang-build/` directories are NOT committed
      (root `.gitignore:7`).
- [ ] Isolated test passes.
- [ ] Full suite passes.
- [ ] Full suite passes **twice in a row**. Second should be faster.
- [ ] `git status` shows only files under
      `tests/standalone/reqwest_test/`, the edit to
      `standalone_tests.rs`, and the four doc bumps. **No compiler
      source changes.**
- [ ] Commit message matches recent style (`git log --oneline -3`).

---

## 9. What passing unlocks

- **Phase 7 down to one remaining crate** (`clap_test`). clap requires
  `impl Into<Str>` synthetic-generic handling, a separate mini-phase
  with compiler work. After reqwest, the "mechanical expansion" story
  is complete.
- **First positive precedent for a deep-transitive-dep crate.** uuid,
  indexmap, regex, toml, serde_json, glob, rand are all shallow
  (≤15 deps). reqwest has 100+. If cargo resolution, download, and
  compilation all work cleanly, the Phase 5 infrastructure is proven
  robust against any crates.io dep tree toylang might reasonably hit
  in the future.
- **Scope of Phase 8 polish becomes clearer.** After reqwest, there
  are 8 near-identical `test_standalone_*` functions — enough corpus
  to confidently design a `run_standalone_test(name, expected)`
  helper.

---

## 10. Retrospective step (after landing)

Same pattern as `handoff-glob-rand.md`. Add at the very top of this
file:

```markdown
> **Status: LANDED YYYY-MM-DD. Preserved as historical context.**
>
> **Outcome:** <one-paragraph summary>
>
> **Prediction accuracy:** <comparison to §6 failure classes>
>
> <any surprises>
>
> Original handoff body preserved below verbatim.
>
> ---
```

Then `mv handoff-reqwest.md docs/historical/`. Commit separately
(the retrospective commit), titled something like
`handoff-reqwest: mark landed, move to historical`.

---

## 11. If stuck

Escalate per `handoff-glob-rand.md` §12 — same rule. Post in team chat
with failure class, exact command, tail of `/tmp/erw-reqwest.txt`,
what you tried, the contents of `main.toylang`, and `git diff`.

**Don't attempt ABI fixes, linker fixes, Phase 5 feature-emission
fixes, or fork patches.** If Phase 5 feature forwarding is broken, a
real fix belongs to the Phase 5 owner — not the smoke-test author.
Write up the gap clearly and hand off.

---

## 12. Summary

Three files. ~10 lines of toylang. One commit to land. One commit to
retrospect and move to historical. Bumps Phase 7 from 7/9 to 8/9,
tests 208 → 209.

Most of the risk is at cargo-resolve time (detailed-dep path); once
toylang actually starts compiling the user program, the code path is
the same one Client-style zero-arg inherent statics have walked seven
times now.

If this one also lands first-try, that's **four consecutive
first-try Phase 7 tests** (toml → glob → rand → reqwest). At that
point the "mechanical completion" classification is conclusively
earned and Phase 7's only remaining work is clap's compiler gap.

Good luck.
