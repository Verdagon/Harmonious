# Phase 5: `toylang.toml` + `toylangc build` (implemented)

> **Status: shipped.** All deliverables landed. 60 unit tests + 110 integration
> tests + 4 standalone tests pass. Manual smoke tests (minimal project + project
> with `toml = "0.8"` as a `[rust-dependencies]` entry) both work end-to-end.
> See `docs/architecture/rust-interop-guide.md` §10.5 and the arcana at
> `docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md` for the final design.

## Context

`toylangc` was only usable as a direct rustc-like compiler
(`toylangc --toylang-input foo.toylang main.rs -o bin`). The user had to
hand-write a Rust `main.rs` and linker glue, and could only depend on crates
already in the sysroot (std/core/alloc). Phase 5 added a `toylangc build`
command that reads a `toylang.toml` manifest, generates a hidden Cargo
project, and invokes `cargo +rustc-fork build` with
`RUSTC_WORKSPACE_WRAPPER=<self>`. Cargo compiles dependency crates with real
rustc and the primary crate through `toylangc`, gated by
`CARGO_PRIMARY_PACKAGE=1`. Wrapper mode rediscovers the toylang source by
re-reading the user's `toylang.toml` — no environment-variable side-channel,
the manifest is single source of truth.

This was pure build orchestration: no changes to the facade, codegen, stub
gen, oracle, parser, or type resolver.

## Approach (as shipped)

`main.rs` dispatches into three modes:

1. **Build mode** — `argv[1] == "build"`. Parse manifest, generate
   `.toylang-build/`, spawn `cargo +rustc-fork build`.
2. **Wrapper mode** — detected by `argv[1]` being a path ending in `rustc`
   (cargo's `RUSTC_WORKSPACE_WRAPPER` protocol). Drop `argv[1]`, then:
   - If `CARGO_PRIMARY_PACKAGE` is set → locate `toylang.toml` by walking up
     from `CARGO_MANIFEST_DIR` (cargo sets this to the primary crate's
     directory, i.e. `.toylang-build/`), re-parse the manifest via
     `manifest::parse`, resolve `source` relative to the manifest's directory,
     read the `.toylang` file, run the existing toylang compilation flow.
   - Otherwise → pass-through via `rustc_driver::RunCompiler` with a no-op
     `Callbacks` impl.
3. **Direct mode** — unchanged existing behavior; all 110 integration tests
   keep working.

Stubs are still virtually injected via `LangFileLoader` — no disk write. The
generated shim `src/main.rs` contains `mod __lang_stubs; use __lang_stubs::*;
fn main() { __toylang_main(); }`. `.toylang-build/` gets its own
`rust-toolchain.toml` pinning `rustc-fork`.

## Files shipped

- **`toylangc/Cargo.toml`** — added `toml = "0.8"` to `[dependencies]`.
- **`toylangc/src/manifest.rs`** (new, ~160 lines) — `Manifest`/`Project`/
  `DepSpec` structs with serde derives, `parse()` using `toml::from_str`,
  6 inline unit tests.
- **`toylangc/src/build.rs`** (new, ~175 lines) — `build_project()` generates
  `.toylang-build/{Cargo.toml, src/main.rs, rust-toolchain.toml}` and spawns
  `cargo +rustc-fork build` with `RUSTC_WORKSPACE_WRAPPER`,
  `DYLD_LIBRARY_PATH`, and `LD_LIBRARY_PATH` env vars set.
- **`toylangc/src/main.rs`** (refactored from 103 → ~200 lines) — three-mode
  dispatch at top of `main()`, extracted existing logic into
  `run_direct_mode`, added `run_wrapper_mode` with `CARGO_PRIMARY_PACKAGE`
  gating, added `NoopCallbacks` + `run_plain_rustc`.
- **`toylangc/tests/standalone_tests.rs`** (new, ~160 lines) — 4 tests:
  `test_build_minimal_project` (empty `fn main() {}`),
  `test_build_with_rust_dep` (`toml = "0.8"` dep, asserts `Cargo.lock`
  resolution), `test_build_invalid_manifest_fails` (missing `[project]`),
  `test_build_missing_source_fails` (source file does not exist).
- **`.gitignore`** — appended `.toylang-build/`.
- **`docs/arcana/ManifestReReadInWrapperMode-MRRIWMZ.md`** (new) — arcana
  documenting the dual-role of `toylang.toml` as both user manifest and
  side-channel between invocations.
- **`docs/architecture/rust-interop-guide.md`** — appended `@MRRIWMZ` to the
  arcana index (§11 "Arcana index").
- **`README.md`** — new "Building a project with `toylang.toml`" section
  explaining the dual role to end users, with a link to the arcana.
- **`@MRRIWMZ` code annotations** added at both manifest read sites
  (`build.rs:build_project` and `main.rs:run_wrapper_mode`).

## High-risk items: investigation outcomes

All three risks were de-risked via reading code before implementation, then
confirmed correct by the first successful end-to-end build.

### Risk A — Pass-through via `RunCompiler` with no-op `Callbacks`

**Outcome: worked as predicted, with one environment caveat.**

- `rustc_driver::RunCompiler::new(&[String], &mut (dyn Callbacks + Send)).run()`
  with a unit-struct `NoopCallbacks` is exactly what the existing
  `rustc-lang-facade/src/driver.rs` uses.
- Default `Callbacks` methods all return `Compilation::Continue` — codegen
  and linking are NOT suppressed.
- Rustc internally skips `argv[0]`, so wrapper mode drops `argv[1]` (the real
  rustc path) leaving `argv = [toylangc, <rustc-args>...]` which rustc
  correctly interprets as `argv[0]=toylangc (skipped), argv[1..]=real args`.
- `NoopCallbacks: Send` is trivially true for a unit struct.

**Caveat that surfaced during smoke testing**: cargo does NOT inherit
`DYLD_LIBRARY_PATH` from the shell when it spawns the wrapper process. Even
the outer `toylangc build` invocation fails without `DYLD_LIBRARY_PATH` set,
because `toylangc` itself dynamically links `librustc_driver-*.dylib`. Build
mode handles this by computing the sysroot via `rustc +rustc-fork --print
sysroot` and setting both `DYLD_LIBRARY_PATH` (macOS) and `LD_LIBRARY_PATH`
(Linux) on the cargo subprocess. The user must still set `DYLD_LIBRARY_PATH`
themselves when invoking `toylangc build` directly (same ergonomic issue that
existed for direct mode — not Phase 5's problem to solve).

### Risk B — Virtual `__lang_stubs` injection inside a Cargo-built crate

**Outcome: worked with zero changes to `file_loader.rs`.**

- `LangFileLoader::is_stubs_path` matches on `path.file_name() == "__lang_stubs.rs"`,
  not on relative vs absolute path.
- When cargo compiles `.toylang-build/src/main.rs`, rustc probes
  `/abs/path/.toylang-build/src/__lang_stubs.rs`; the loader intercepts on
  the filename match and serves virtual stubs.
- The generated shim puts `mod __lang_stubs;` before `use __lang_stubs::*;`
  so rustc tries to resolve the module file first (the loader intercepts)
  before the `use` path needs to resolve.

### Risk C — Toolchain pinning for the child cargo

**Outcome: straightforward, worked first try.** Writing
`[toolchain]\nchannel = "rustc-fork"\n` to `.toylang-build/rust-toolchain.toml`
ensures the child cargo uses the right toolchain regardless of the outer
invocation.

## Lessons learned

1. **The single-source-of-truth design emerged from user feedback, not up-front
   planning.** The original plan used `TOYLANG_INPUT` as an env var to carry
   the source path from build mode to wrapper mode. The user pushed back on
   env vars, asked what alternatives existed, then asked "can't it just read
   `toylang.toml`?" That observation collapsed several layers of complexity:
   no env var, no sidecar file, no copy-into-build-dir. The manifest already
   describes the build — wrapper mode should re-read it, not be handed a
   redundant pointer.

2. **Risk investigation before coding paid off.** Three risks were listed up
   front. Each was investigated via reading `rustc_driver_impl`, `file_loader.rs`,
   and a few rustc-internal types, before writing a line of new code. Every
   risk was confirmed safe in advance; the only surprise at implementation
   time was the `DYLD_LIBRARY_PATH` ergonomic issue, which was cosmetic and
   trivially fixed (one extra `.env()` call on the cargo subprocess).

3. **The dual-role of `toylang.toml` deserved an arcana.** Having the manifest
   read in two different processes at two different times is non-obvious.
   Schema changes, rename of the file, or changes to the
   `CARGO_MANIFEST_DIR/..` walk would break things silently. `@MRRIWMZ` at
   both read sites + a README section for users + an entry in the arcana
   index ensures the dual role is visible to future maintainers.

4. **Direct mode preservation was non-negotiable.** 110 integration tests
   depend on the `--toylang-input` direct-mode flow. The main.rs refactor
   wrapped the existing `main()` body verbatim into `run_direct_mode(argv)`
   and added mode dispatch at the top. This kept the risk contained: if
   wrapper mode broke, direct mode would still work, and the test suite
   would catch any regression immediately.

5. **The `CARGO_PRIMARY_PACKAGE` gate is the lynchpin.** Cargo invokes the
   wrapper for every crate it compiles — dependencies, build scripts,
   proc-macros, the primary crate, and integration tests. Gating toylang
   processing on `CARGO_PRIMARY_PACKAGE` means only the user's crate runs
   through toylangc; everything else pass-throughs to plain rustc via
   `NoopCallbacks`. Without this gate, toylangc would try to parse
   `toylang.toml` while compiling `rand` and fail.

6. **Two bonus tests (negative cases) added after initial review** —
   `test_build_invalid_manifest_fails` and `test_build_missing_source_fails`
   lock in error handling behavior. Both caught that `build_project` returns
   non-zero exit codes correctly and that `.toylang-build/` is not created
   when manifest parsing fails.

## Deferred work (noted for future phases)

- **Phase 6** — `.unwrap()` verification on `Result`/`Option`.
- **Phase 7** — 9 standalone test projects (rand, regex, uuid, clap, glob,
  reqwest, toml, serde_json, indexmap). Before the full push, add one
  bridging test where toylang actually *calls into* a dep crate (candidate:
  `Uuid::new_v4()`); see `quest.md` Step 7.4. Current `test_build_with_rust_dep`
  only verifies cargo *resolves* the dep; it does not prove toylang can call
  it.
- **Phase 8** — test harness running all standalone projects.
- **Pre-existing broken harness files** (`host.rs`, `counter_test.rs`,
  `pair_test.rs` under `toylangc/tests/`) — unrelated to Phase 5, still
  broken, noted in architecture doc.

## Verification (final)

```
cargo +rustc-fork build -p toylangc                          # builds cleanly
cargo +rustc-fork test -p toylangc --bin toylangc            # 60 unit tests
cargo +rustc-fork test -p toylangc --test integration_tests  # 110 tests
cargo +rustc-fork test -p toylangc --test standalone_tests   # 4 tests
```

Manual smoke (requires `DYLD_LIBRARY_PATH` set to sysroot/lib):

```
mkdir -p /tmp/toy-smoke && cd /tmp/toy-smoke
cat > toylang.toml <<EOF
[project]
name = "smoke"
source = "main.toylang"
EOF
echo 'fn main() {}' > main.toylang
DYLD_LIBRARY_PATH=$(rustc +rustc-fork --print sysroot)/lib \
  /path/to/target/debug/toylangc build
./.toylang-build/target/debug/smoke; echo "exit: $?"  # → exit: 0
```
