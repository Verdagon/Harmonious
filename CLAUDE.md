# erw (Embed Rust Well)

A framework for embedding custom languages into rustc's compilation pipeline via query providers.

## Background

Two-crate workspace:
- **`rustc-lang-facade`** — reusable library that hooks into rustc via 4 query providers (`layout_of`, `optimized_mir`, `symbol_name`, `mir_shims`)
- **`toylangc`** — example consumer compiler for "toylang" that uses the facade

Built against vanilla `nightly-2026-01-20` — zero rustc fork patches. All rustc integration flows through `Config::override_queries`: `optimized_mir` supplies synthetic dep-registering bodies, `collect_and_partition_mono_items` filters consumer items out of rustc's CGU list and forces `(External, Default)` linkage on `__lang_stubs` items directly, `symbol_name` rewrites to toylang-mangled names, `layout_of` / `mir_shims` / `upstream_monomorphizations_for` round out the set. No fork, no hook statics.

Consumer types appear to rustc as opaque stubs with `unreachable!()` bodies. Internal consumer functions are never exposed to rustc — they are discovered via deep monomorphization walk and compiled separately by an Inkwell LLVM backend. A global mutex serializes all consumer code (single-threaded).

`ResolvedType` is the unified type representation used everywhere (no string-based types). Explicit type args required at all call sites (no inference).

**Compiler law: non-generic is the degenerate case of generic.** Never branch on "does this function/type have type parameters?" A non-generic item is simply one with zero type args — it goes through the same instantiation path as a generic one. Code that special-cases the non-generic path creates false distinctions and latent bugs when items gain type params or the code is reused in a more general context. Always write the general path; zero args is a valid input to it.

**Prefer treating `self` as just another parameter.** When possible, avoid separate code paths for self vs non-self parameters. If syntax separates the receiver (e.g., dot notation), reassemble the full parameter list so downstream logic can handle everything uniformly.

**`instantiate_identity()` requires a comment.** `EarlyBinder::instantiate_identity()` is a no-op unwrap — it discards the binder and returns the inner value with all `ty::Param` placeholders intact. It is only correct for structural inspection (e.g., "is this impl for a consumer type?"), never for producing a concrete type at a call site. Every call to `instantiate_identity()` must have a comment explaining why we are intentionally not substituting real values for the generic parameters.

**Never revert unilaterally.** If a change doesn't pan out, stop and ask before undoing anything — including changes you just made. The failing tree carries diagnostic signal, and the user may want partial work kept. Reverting is a pivot.

## Key docs

- [Architecture guide](docs/architecture/rust-interop-guide.md) — full compilation flow, query providers, LLVM backend, ABI handling
- [Known tech debt](docs/architecture/known-tech-debt.md) — tracked debt items
- [Documentation strategy](docs/meta.md) — how docs are organized (categories, conventions, discovery)

## Building & testing

```bash
cargo +nightly-2026-01-20 test          # run all tests (67 unit + 128 integration_projects + 15 standalone = 210)
cargo +nightly-2026-01-20 test --test integration_tests test_name  # run one test
```

## Build & Run Convention

Always pipe `cargo run`, `cargo test`, `cargo build`, `cargo check`, and all `sbt` output into a fixed file in `./tmp/` (use the same file for the entire session/project, e.g. `./tmp/refactor-project.txt`). Come up with a name instead of refactor-project.txt, and then use the same file for the rest of the session.

**Never chain a heavy command with `| tail`, `| head`, `| grep`.** Run the build/test with `>` as one command (redirecting fully to the file) and the inspection as a separate follow-up command. Chaining defeats the purpose: you lose the ability to re-analyze a different part of the output without re-running the expensive build.

DO have them in separate commands:

```bash
cargo run --bin benchmark -- --model openai/gpt-oss-20b > ./tmp/fixing-bug-1047-quest.txt 2>&1
tail -20 ./tmp/fixing-bug-1047-quest.txt
# Later, to see a different part:
head -40 ./tmp/fixing-bug-1047-quest.txt
grep "error" ./tmp/fixing-bug-1047-quest.txt
```

```bash
sbt 'testOnly dev.vale.AfterRegionsIntegrationTests' > ./tmp/fixing-borrowing-test.txt 2>&1
grep "SUCCESS" ./tmp/fixing-borrowing-test.txt
# Later, to see a different part:
tail -30 ./tmp/fixing-borrowing-test.txt
# Later, do some changes to the code, and then same command into same file:
sbt 'testOnly dev.vale.AfterRegionsIntegrationTests' > ./tmp/fixing-borrowing-test.txt 2>&1
grep "SUCCESS" ./tmp/fixing-borrowing-test.txt
```

DON'T chain them together like this:

```bash
# This is bad:
cargo build --lib > ./tmp/build4.txt && grep -B2 "i_env_entry" ./tmp/build4.txt | grep "src/" | head -20
```

Instead, they must be separate entire commands.

DON'T use a different file for each build like this:

```bash
sbt 'testOnly dev.vale.AfterRegionsIntegrationTests' > ./tmp/borrowing-build1.txt 2>&1
grep "SUCCESS" ./tmp/borrowing-build1.txt
# BAD: Don't use a different file
sbt 'testOnly dev.vale.AfterRegionsIntegrationTests' > ./tmp/borrowing-build2.txt 2>&1
grep "SUCCESS" ./tmp/borrowing-build2.txt
```

Instead, use the same file.


## Use Relative Paths For Cargo Commands

In `cargo` commands, don't use `/Volumes/V/...` or `/Users/verdagon/...` etc.

For example, don't do this:

```
cargo check --manifest-path /Volumes/V/Sylvan/FrontendRust/Cargo.toml --tests > /Volumes/V/Sylvan/tmp/slab15-build.txt 2>&1
```

Instead, use relative paths for cargo commands:

```
cargo check --manifest-path ./FrontendRust/Cargo.toml --tests > ./tmp/slab15-build.txt 2>&1
```
