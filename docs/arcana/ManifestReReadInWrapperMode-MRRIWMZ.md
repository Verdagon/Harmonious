# Manifest Re-read In Wrapper Mode (MRRIWMZ)

`toylang.toml` is read from two different processes at two different times:

1. The user-invoked `toylangc build` parses it to orchestrate cargo.
2. The wrapper-mode `toylangc` (spawned by cargo for the primary crate via
   `RUSTC_WORKSPACE_WRAPPER`) parses it again to locate the `.toylang`
   source file — because cargo owns argv and has no way to carry out-of-band
   data from the build orchestrator to the wrapper process without an env
   var side-channel.

The manifest is thus both a user-facing UX artifact AND a side-channel between
two invocations of the same binary. Changes to the manifest schema, location,
or parser must keep both read sites in sync.

## Where

- `toylangc/src/manifest.rs` — `parse()` is the single parse implementation
  used by both read sites
- `toylangc/src/build.rs` — `build_project()` is read site 1 (user-invoked
  `toylangc build`). Resolves `source` relative to the manifest dir,
  generates `.toylang-build/` Cargo project, spawns
  `cargo +rustc-fork build` with `RUSTC_WORKSPACE_WRAPPER=<self>`.
- `toylangc/src/main.rs` — `run_wrapper_mode()` is read site 2 (cargo-invoked
  wrapper). Walks up from `CARGO_MANIFEST_DIR` (= `.toylang-build/`) to find
  `toylang.toml`, re-parses, resolves `source`.

## Cross-cutting effect

Anything that would break the second read breaks the build silently from the
user's perspective. Examples:

- Renaming `toylang.toml` → must update both read sites.
- Changing the `[project].source` field semantics (e.g. making it absolute) →
  both sites compute paths relative to the manifest's parent directory.
- Moving `.toylang-build/` to a non-sibling location → wrapper mode's
  `CARGO_MANIFEST_DIR/..` walk breaks.
- Failing to set `RUSTC_WORKSPACE_WRAPPER` to an absolute path
  (`current_exe()`) → cargo won't find the wrapper.
- Requiring new env vars in wrapper mode → build mode must also set them on
  the cargo subprocess (like `DYLD_LIBRARY_PATH`/`LD_LIBRARY_PATH` for the
  dynamic loader to find `librustc_driver`).

## Why it exists

Cargo's `RUSTC_WORKSPACE_WRAPPER` protocol gives the wrapper argv and the
environment, but no way for the orchestrator to attach arbitrary data to a
specific crate's compilation. The obvious alternative is an environment
variable like `TOYLANG_INPUT=<abs path>` set on the cargo subprocess. This
was rejected because:

- Env vars couple the build orchestrator and wrapper in an invisible way.
- The manifest already fully describes the build — any env var would be
  redundant information that can drift from the manifest.
- Using the manifest as the single source of truth means a developer can
  inspect `toylang.toml` and understand the full build; there's no hidden
  state.

Trade-off: the manifest is parsed twice per build (once in the outer
`toylangc build` process, once in the wrapper-mode child process for the
primary crate). Parsing cost is trivially small (microseconds) compared to
the rest of compilation.
