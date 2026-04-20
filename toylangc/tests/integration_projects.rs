//! Stage 5c integration tests — toylang projects compiled via `toylangc build`
//! (wrapper-mode two-crate path). Each test corresponds to a fixture under
//! `tests/integration_projects/<test_name>/` containing:
//!   - `main.toylang`: the toylang source.
//!   - `toylang.toml`: the project manifest.
//!   - `expected_output.txt`: stdout the produced binary should print.
//!
//! All projects path-depend on the shared `test_helpers` crate at
//! `tests/integration_projects/test_helpers/`, which provides `#[no_mangle]
//! pub extern "C"` definitions for the body-less `fn name(...);` extern
//! declarations toylang sources use (println_int, etc.). cargo dedupes
//! `test_helpers` by canonicalized path → it compiles once and the rlib is
//! reused across the suite via the shared CARGO_TARGET_DIR set in the
//! harness below.

use std::path::{Path, PathBuf};
use std::process::Command;

fn toylangc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_toylangc"))
}

fn sysroot_lib() -> String {
    let out = Command::new("rustup")
        .args(["run", "nightly-2025-01-15", "rustc", "--print=sysroot"])
        .output()
        .expect("failed to run rustup");
    let sysroot = String::from_utf8(out.stdout).unwrap();
    format!("{}/lib", sysroot.trim())
}

/// Shared cargo target dir for every integration project. Setting this once
/// per test (via `Command::env`) makes cargo cache `test_helpers` (same
/// canonicalized path-dep across projects) + every crates.io transitive dep
/// across the whole suite. Without it each test would do a fresh
/// `cargo build` that recompiles `test_helpers` from scratch — minutes of
/// pointless work multiplied by 129 tests. See handoff §6.8 for the
/// failure-mode checklist if a canary runs >15s cold.
fn shared_cargo_target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/integration-projects-cache")
}

fn projects_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/integration_projects")
}

/// Build a project under `tests/integration_projects/<name>/` via
/// `toylangc build`, run the produced binary, and assert its stdout
/// contains every line from `expected_output.txt` (line-wise contains
/// match — ordering preserved, but extra surrounding lines are tolerated).
fn run_integration_project(name: &str) {
    let project = projects_dir().join(name);
    assert!(
        project.is_dir(),
        "integration project not found: {}",
        project.display(),
    );

    // Per-project `.toylang-build/` workspace. Wipe it for a clean cargo
    // resolve; the SHARED cache (CARGO_TARGET_DIR) holds the heavy state
    // so this only forces the toylang-emitted Cargo.toml + lib.rs to be
    // re-templated.
    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();

    let build_out = Command::new(toylangc_bin())
        .current_dir(&project)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .env("CARGO_TARGET_DIR", &cargo_target)
        // Disable rustc's incremental cache. Toylang's overrides
        // (`symbol_name`, `optimized_mir`, etc.) carry side effects that
        // populate consumer-codegen state when the query fires; rustc's
        // incremental cache treats queries as pure and returns cached
        // results on the second run without invoking the provider, leaving
        // our state empty and the emitted toylang `.o` symbol-less. The
        // shared CARGO_TARGET_DIR still caches built rlibs (test_helpers,
        // crates.io deps) so this only forfeits rustc's per-crate
        // fingerprint, not cargo's package-level cache.
        .env("CARGO_INCREMENTAL", "0")
        .args(["build"])
        .output()
        .expect("failed to spawn toylangc");
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    // Binary lives under the SHARED target dir — cargo writes per-package
    // dirs there even though the per-project .toylang-build/ workspace is
    // unique. Project name is the bin name (matches `[[bin]]` config in the
    // generated user_bin Cargo.toml).
    let bin = cargo_target.join("debug").join(name);
    assert!(
        bin.exists(),
        "{} expected binary at {}, found nothing",
        name,
        bin.display(),
    );

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .unwrap_or_else(|e| panic!("{}: failed to spawn binary: {}", name, e));
    assert!(
        run.status.success(),
        "{} binary exited non-zero:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );

    let expected = std::fs::read_to_string(project.join("expected_output.txt"))
        .unwrap_or_else(|e| panic!("{}: cannot read expected_output.txt: {}", name, e));
    let stdout = String::from_utf8_lossy(&run.stdout);
    for line in expected.lines() {
        if line.is_empty() { continue; }
        assert!(
            stdout.contains(line),
            "{}: expected '{}' in stdout, got:\n{}",
            name, line, stdout,
        );
    }
}

// ============================================================================
// Stage 5c.1 canary tests
// ============================================================================

#[test]
fn test_counter_construct() {
    run_integration_project("counter_construct");
}

#[test]
fn test_pair_construct() {
    run_integration_project("pair_construct");
}

// TODO(stage5c.2): test_extern_fn_call hits a toylang codegen bug where a
// fn whose body ends with an extern "C" call returning void gets emitted with
// the WRONG return type — the body's `ret void` mismatches the declared
// return. Repro:
//
//   fn println_bool(x: bool)
//   fn do_print() { println_int(42); println_bool(true); }
//   fn main() { do_print(); }
//
// LLVM IR shows `define i8 @__toylang_internal_do_print()` with `ret void` in
// the body. Specific to extern "C" decls under stage 5c — investigate the
// `coerced_return_type_for_instance` path on bool args. Re-enable canary +
// add the project once fixed.
//
// #[test]
// fn test_extern_fn_call() {
//     run_integration_project("extern_fn_call");
// }
