# Building and Testing

Practical commands for building erw and running tests. As of the stage-4 zero-fork landing, erw builds against vanilla `nightly-2025-01-15` installed via rustup.

## Install the toolchain

```bash
rustup toolchain install nightly-2025-01-15
```

No forked toolchain needed. The historical fork-rebuild workflow — useful only if you ever need to temporarily patch rustc for an investigation — is preserved at `docs/historical/rebuilding-rustc-fork.md`.

## Run the test suite

```bash
# Full suite (67 unit + 127 integration_projects + 15 standalone = 209; 0 failed, 0 ignored)
cargo +nightly-2025-01-15 test -p toylangc

# Just integration projects (toylangc build each project under
# toylangc/tests/integration_projects/<name>/, run binary, check stdout)
cargo +nightly-2025-01-15 test -p toylangc --test integration_projects

# Just unit tests (embedded in the toylangc binary)
cargo +nightly-2025-01-15 test -p toylangc --bin toylangc

# Just standalone tests (larger projects that depend on real crates.io crates)
cargo +nightly-2025-01-15 test -p toylangc --test standalone_tests

# Run a specific test
cargo +nightly-2025-01-15 test -p toylangc --test integration_projects test_name
```

## Check for warnings

```bash
cargo +nightly-2025-01-15 check -p toylangc
```

Expected: zero warnings. If a rustc bump introduces warnings from rustc-internal API churn, fix them before merging (don't accumulate). If the warnings indicate drift in `rustc_private` APIs, that's the category-B drift described in `docs/architecture/risks.md` — budget a bump-repair session rather than suppressing.

## Build-output convention (per project `CLAUDE.md`)

For long-running commands where you want to inspect output repeatedly without re-running:

```bash
# Redirect fully to a fixed file, then inspect separately:
cargo +nightly-2025-01-15 test -p toylangc 2>&1 > /tmp/erw.txt
grep "test result:" /tmp/erw.txt
# Later, without re-running:
tail -30 /tmp/erw.txt
grep "FAILED" /tmp/erw.txt
```

Don't chain `| grep` / `| tail` / `| head` on the initial run — you lose the ability to re-analyze a different part of the output without re-running. Reuse the same `/tmp/<session>.txt` throughout a debugging session rather than creating new files per run.

## Running `toylangc build` on a standalone project

Any of the 15 standalone test projects at `toylangc/tests/standalone/<crate>_test/` is a self-contained example. To build and run one manually:

```bash
cd toylangc/tests/standalone/uuid_test
rm -rf .toylang-build      # clean slate
cargo +nightly-2025-01-15 run --bin toylangc \
    --manifest-path /Users/verdagon/erw/Cargo.toml -- build
./.toylang-build/target/debug/uuid_test
# Expected output: uuid ok
```

The standalone projects are also the canonical example of how a user would structure their own toylang project: `toylang.toml` + `main.toylang` + any declared `[rust-dependencies]`.

## Bumping the rustc nightly pin

The `nightly-2025-01-15` pin is intentional. When bumping:

1. Pick a new nightly (usually ~3 months old — gives the ecosystem time to stabilize around rustc-internals changes; don't chase latest).
2. Update `rust-toolchain.toml` at repo root.
3. Run the full test suite. Expect API drift in `rustc_middle::mir` / `abi_helpers.rs` / query signatures; this is category-B risk (`docs/architecture/risks.md` §3). Budget ~1 week.
4. Batch the drift repair into a dedicated commit/PR cycle; don't mix with feature work. Makes bisect easier if something breaks later.

See `docs/architecture/risks.md` §5 "Nightly-pin strategy" for the longer discussion.

## See also

- `docs/architecture/rust-interop-guide.md` — the current architecture being tested.
- `docs/architecture/risks.md` — what to expect when a rustc bump breaks something.
- `docs/usage/writing-main.md` — practical rules for writing toylang programs (for when you're debugging test behavior, not just running the suite).
- `docs/historical/rebuilding-rustc-fork.md` — the preserved fork-rebuild workflow (only useful for one-off rustc patching experiments).
