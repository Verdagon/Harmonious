# Rebuilding the rustc fork

When you change anything under `~/rust/compiler/`, you must rebuild **and
reinstall** the fork before `cargo +rustc-fork` will pick up the change.
The full workflow is non-obvious and the obvious shortcut does not work.

## The full workflow

```bash
cd ~/rust

# 1. Build + package the rustc-dev distribution.
python3 x.py dist rustc-dev 2>&1 | tee /tmp/erw-rustc-build.txt

# 2. Untar and install into the stage2 sysroot.
#    NOTE: replace the version string with whatever `ls ~/rust/build/dist/`
#    reports (e.g. `1.95.0-dev` as of 2026-06-24, `1.86.0-dev` historically).
cd /tmp
rm -rf rustc-dev-1.95.0-dev-aarch64-apple-darwin
tar xzf ~/rust/build/dist/rustc-dev-1.95.0-dev-aarch64-apple-darwin.tar.gz
cd rustc-dev-1.95.0-dev-aarch64-apple-darwin
bash install.sh --prefix=$HOME/rust/build/host/stage2

# 3. Strip the rustc-src dump (otherwise stage2 lib/rustlib/rustc-src
#    gets giant and confuses some tooling).
rm -rf $HOME/rust/build/host/stage2/lib/rustlib/rustc-src

# 4. Build the stage2 standard library + rustdoc.
#    Building rustdoc here (rather than as a separate step) keeps it
#    out of the "step 4 wipes step 2's work" trap — `x.py build
#    library` and `x.py build src/tools/rustdoc` are independent
#    targets, but both need to be built together at stage 2 so the
#    sysroot ends up with both stdlib AND rustdoc installed. Without
#    rustdoc, `cargo test --workspace` exits with non-zero when it
#    reaches the doctest step ("'rustdoc' is not installed for the
#    custom toolchain 'rustc-fork'") even though all real tests pass.
cd ~/rust
python3 x.py build --stage 2 library src/tools/rustdoc

# 5. REINSTALL rustc-dev. The library build in step 4 wipes
#    lib/rustlib/<target>/lib/librustc_*.rmeta. Without this second
#    install, cargo +rustc-fork build fails with 50+ "can't find crate
#    for `rustc_abi`" errors.
cd /tmp/rustc-dev-1.95.0-dev-aarch64-apple-darwin
bash install.sh --prefix=$HOME/rust/build/host/stage2
```

After all five steps, `cargo +rustc-fork build -p toylangc` works AND
`cargo test --workspace` exits 0 (with the rustdoc-based doctest step
finding 0 tests to run — the `///` comments in `rustc-lang-facade`
are illustrative blocks, not runnable examples).

## Things that look like shortcuts but aren't

- **`python3 x.py build --stage 2 compiler/rustc` alone is NOT enough.**
  It builds `librustc_driver.dylib` and the `lib/rustlib/.../lib/`
  metadata, but it does not populate `host/stage2/bin/rustc`. Without
  that binary, `cargo +rustc-fork` fails with `'rustc' is not installed
  for the custom toolchain 'rustc-fork'`. Use `x.py dist rustc-dev`
  instead — it does both jobs.

- **Skipping step 5 (the reinstall) is a guaranteed regression.**
  `x.py build library --stage 2` performs an "Uplift library
  (stage1 → stage2)" step that recreates `lib/rustlib/<target>/lib/`
  from scratch with only the stdlib `.rlib` files. Every `librustc_*`
  metadata file installed by step 2 is gone afterwards. The error
  surface (`can't find crate for rustc_abi`) makes it look like the
  facade is missing a dep; the actual cause is that the sysroot was
  silently wiped.

- **Don't `rustup component add` anything.** The toolchain link points
  at a build directory, not a rustup-managed install — `rustup
  component add` rejects it with `cannot use rustup component add`.

## Editing rules inside the fork

The fork is built with `-D warnings`, including `unreachable_pub`. If
you add a `pub item` inside a `mod foo;` (private module) declaration,
the compiler errors out immediately:

```
error: unreachable `pub` item
   --> compiler/rustc_monomorphize/src/partitioning.rs:137:1
help: consider restricting its visibility: `pub(crate)`
```

If the item really needs to be reachable from outside the crate (for
example, a `pub static` registration hook the facade installs into),
flip the module declaration in the crate's `lib.rs` to `pub mod foo;`
in the same patch. Don't reach for `#[allow(unreachable_pub)]` — it
hides the real visibility constraint and surprises the next reader.

## Verifying the install worked

```bash
ls $HOME/rust/build/host/stage2/bin/rustc                              # rustc binary
ls $HOME/rust/build/host/stage2/bin/rustdoc                            # rustdoc binary (added 2026-06-24)
ls $HOME/rust/build/host/stage2/lib/rustlib/aarch64-apple-darwin/lib/ \
   | grep rustc_abi                                                    # rustc-dev libs
rustup run rustc-fork rustdoc --version                                # smoke-test rustdoc
```

All four must succeed. If the rustc binary is missing, repeat step 1.
If `librustc_abi*` is missing from rustlib, repeat step 5. If rustdoc
is missing, repeat step 4 (with `src/tools/rustdoc` in the build
target list).

## Time budget

Plan for **8–12 minutes** end-to-end on incremental changes:
- Step 1 (`x.py dist rustc-dev`): 3–5 min for cargo to recompile the
  affected crates and re-package.
- Step 4 (`x.py build library --stage 2`): 3–5 min, mostly re-linking
  `rustc_driver`.
- Steps 2, 3, 5: a few seconds each.

A clean rebuild (after `rm -rf build/`) is 30–60 minutes — avoid this
unless you've changed `config.toml` or you suspect stale incremental
state.

## See also

- `../arcana/GenerateCompileMutexLock-GCMLZ.md` — when modifying the
  fork to add new query providers or hooks, read this for the locking
  rules they must follow.
- `../architecture/rust-interop-guide.md` §10.6.4 — example of a
  cross-fork registration hook (`VISIBILITY_OVERRIDE_HOOK`) and the
  facade bridge fn that fills it.
