//! Pass-through corpus fence: the forked rustc, with Sky's machinery
//! dormant (no `__SKY_STUBS_MARKER` in the crate being compiled), must
//! produce a binary whose runtime behaviour is identical to one built by
//! vanilla `nightly-2026-01-20`.
//!
//! This is a minimal v1 of the arch doc's §25.3.5 "byte-identical
//! pass-through corpus" invariant. Two notable deviations from the doc's
//! framing:
//!
//! 1. **Runtime-behaviour identity, not byte identity.** Empirical probing
//!    after Phase 6 (commit `4854a5a`) shows the two compilers produce
//!    machine-code sections that differ in size — primarily because the
//!    forked rustc was built independently of vanilla's binary release and
//!    ships its own stdlib. Byte-identical output would require building an
//!    unpatched version of the fork from the same source tree as a baseline,
//!    which is more setup than this corpus warrants today.
//!
//!    The runtime-behaviour test still catches the failure modes the arch
//!    doc cares about:
//!    - Patch 4's hook firing on a no-marker crate (would change runtime
//!      output, panic, or change exit code).
//!    - Side effects from Sky's `init` / `provide` leaking into pure-Rust
//!      compiles (would manifest as crashes or behavioural drift).
//!    - The fork's `per_instance_mir` collector dispatch corrupting Rust
//!      generic monomorphisations (would produce wrong runtime output).
//!
//! 2. **Single fixture, not a corpus.** Just the `hello/` package today.
//!    Future expansion (serde-derive consumer, tokio program, generic-heavy,
//!    trait-heavy, sys-crate wrapper per the arch doc) lands as additional
//!    sibling dirs under `tests/passthrough_fixtures/` and additional
//!    `#[test] fn passthrough_<name>` entries here.
//!
//! Required toolchains: both `rustc-fork` and `nightly-2026-01-20` must be
//! installed via rustup. The test skips with a clear diagnostic if either
//! is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned vanilla nightly that the rustc-fork was rebased onto. Must match
/// the `rust-toolchain.toml` baseline + `~/rust/`'s `bootstrap.toml`.
const VANILLA_TOOLCHAIN: &str = "nightly-2026-01-20";
const FORK_TOOLCHAIN: &str = "rustc-fork";

/// Build a fixture crate with the named toolchain into an isolated target
/// directory and return the path to the resulting release binary.
fn build_with_toolchain(fixture_dir: &Path, toolchain: &str, label: &str) -> PathBuf {
    let target_dir = std::env::temp_dir().join(format!("erw-passthrough-{}", label));
    // Wipe between runs so cargo cache poisoning doesn't hide a regression.
    let _ = std::fs::remove_dir_all(&target_dir);
    let status = Command::new("cargo")
        .arg(format!("+{}", toolchain))
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(fixture_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo +{}: {}", toolchain, e));
    assert!(
        status.success(),
        "cargo +{} build failed for fixture at {} (exit {:?})",
        toolchain,
        fixture_dir.display(),
        status.code()
    );

    // Fixture crate name == directory name; binary lives at
    // target/release/<crate-name>.
    let crate_name = fixture_dir
        .file_name()
        .and_then(|s| s.to_str())
        .expect("fixture dir has a name");
    let binary = target_dir
        .join("release")
        .join(format!("passthrough_{}", crate_name));
    assert!(
        binary.exists(),
        "expected built binary at {} (build succeeded but binary missing)",
        binary.display()
    );
    binary
}

/// Run the built binary and capture (exit_code, stdout, stderr).
fn run_binary(binary: &Path) -> (Option<i32>, String, String) {
    let output = Command::new(binary)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {}", binary.display(), e));
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Returns true iff a rustup toolchain by this name is installed.
///
/// `rustup toolchain list` outputs lines like
/// `nightly-2026-01-20-aarch64-apple-darwin` (with the target triple
/// appended) optionally followed by ` (default)` / ` (active)`. A toolchain
/// is considered installed if its name appears as a prefix of any line's
/// first whitespace-separated token. Linked toolchains like `rustc-fork`
/// have no target suffix, so an exact-match check covers them too.
fn toolchain_installed(name: &str) -> bool {
    Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.lines().any(|l| {
                let tok = l.split_whitespace().next().unwrap_or("");
                tok == name || tok.starts_with(&format!("{}-", name))
            })
        })
        .unwrap_or(false)
}

#[test]
fn passthrough_hello_runtime_identical() {
    if !toolchain_installed(VANILLA_TOOLCHAIN) {
        eprintln!(
            "SKIP: vanilla toolchain {} not installed — pass-through fence \
             cannot run without a baseline. Install via\n  \
             rustup toolchain install {}",
            VANILLA_TOOLCHAIN, VANILLA_TOOLCHAIN
        );
        return;
    }
    if !toolchain_installed(FORK_TOOLCHAIN) {
        eprintln!(
            "SKIP: fork toolchain {} not installed — see \
             docs/historical/rebuilding-rustc-fork.md.",
            FORK_TOOLCHAIN
        );
        return;
    }

    let fixture = Path::new("tests/passthrough_fixtures/hello");
    assert!(
        fixture.exists(),
        "fence must run from the toylangc crate root (looking for ./{})",
        fixture.display()
    );

    let vanilla_bin = build_with_toolchain(fixture, VANILLA_TOOLCHAIN, "hello-vanilla");
    let fork_bin = build_with_toolchain(fixture, FORK_TOOLCHAIN, "hello-fork");

    let (vanilla_code, vanilla_stdout, vanilla_stderr) = run_binary(&vanilla_bin);
    let (fork_code, fork_stdout, fork_stderr) = run_binary(&fork_bin);

    if vanilla_code != fork_code
        || vanilla_stdout != fork_stdout
        || vanilla_stderr != fork_stderr
    {
        panic!(
            "Pass-through fence: the rustc-fork, with no Sky marker present, \
             produced a binary whose runtime behaviour differs from vanilla \
             {}. The arch doc §25.3.5 invariant is that Sky's machinery being \
             present in the toolchain does NOT change behaviour on pure-Rust \
             crates.\n\n\
             vanilla exit: {:?}\n  fork exit:    {:?}\n\
             vanilla stdout ({} bytes):\n{}\n\
             fork stdout    ({} bytes):\n{}\n\
             vanilla stderr ({} bytes):\n{}\n\
             fork stderr    ({} bytes):\n{}",
            VANILLA_TOOLCHAIN,
            vanilla_code,
            fork_code,
            vanilla_stdout.len(),
            vanilla_stdout,
            fork_stdout.len(),
            fork_stdout,
            vanilla_stderr.len(),
            vanilla_stderr,
            fork_stderr.len(),
            fork_stderr,
        );
    }
}

// Category 2 pass-through corpus expansion (test expansion plan in
// handoff.md). Each fixture exercises a different pure-Rust codegen
// pattern that Sky's machinery could plausibly disturb if its query
// overrides leaked into pure-Rust compiles. Runtime-output identity
// (not byte identity) per the deviation noted in the file header.

/// Shared body for an additional pass-through fixture. Wraps the
/// toolchain-installed check + build + run + behaviour-diff.
fn passthrough_runtime_identical(fixture_subdir: &str, label_prefix: &str) {
    if !toolchain_installed(VANILLA_TOOLCHAIN) {
        eprintln!(
            "SKIP: vanilla toolchain {} not installed",
            VANILLA_TOOLCHAIN
        );
        return;
    }
    if !toolchain_installed(FORK_TOOLCHAIN) {
        eprintln!(
            "SKIP: fork toolchain {} not installed",
            FORK_TOOLCHAIN
        );
        return;
    }

    let fixture = Path::new("tests/passthrough_fixtures").join(fixture_subdir);
    assert!(
        fixture.exists(),
        "fence must run from the toylangc crate root (looking for ./{})",
        fixture.display()
    );

    let vanilla_bin = build_with_toolchain(
        &fixture, VANILLA_TOOLCHAIN, &format!("{}-vanilla", label_prefix));
    let fork_bin = build_with_toolchain(
        &fixture, FORK_TOOLCHAIN, &format!("{}-fork", label_prefix));

    let (vanilla_code, vanilla_stdout, vanilla_stderr) = run_binary(&vanilla_bin);
    let (fork_code, fork_stdout, fork_stderr) = run_binary(&fork_bin);

    if vanilla_code != fork_code
        || vanilla_stdout != fork_stdout
        || vanilla_stderr != fork_stderr
    {
        panic!(
            "Pass-through fence ({}): rustc-fork output differs from vanilla.\n\
             vanilla exit: {:?}\n  fork exit:    {:?}\n\
             vanilla stdout: {}\n  fork stdout:    {}\n\
             vanilla stderr: {}\n  fork stderr:    {}",
            fixture_subdir,
            vanilla_code,
            fork_code,
            vanilla_stdout,
            fork_stdout,
            vanilla_stderr,
            fork_stderr,
        );
    }
}

#[test]
fn passthrough_generics_heavy_runtime_identical() {
    passthrough_runtime_identical("generics_heavy", "generics-heavy");
}

#[test]
fn passthrough_trait_dispatch_runtime_identical() {
    passthrough_runtime_identical("trait_dispatch", "trait-dispatch");
}

#[test]
fn passthrough_closures_iters_runtime_identical() {
    passthrough_runtime_identical("closures_iters", "closures-iters");
}
