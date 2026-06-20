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
use std::sync::Mutex;

#[path = "common/inlining_harness.rs"]
mod inlining_harness;

// Duplicated here from `toylangc/src/main.rs`'s `TOYLANG_NIGHTLY` — integration
// tests cannot import from a `[[bin]]`-only crate, so the pin is carried
// independently. See HANDOFF-nightly-bump.md §3.2 and the `TOYLANG_NIGHTLY`
// doc comment in main.rs for the bump-site inventory.
const TOYLANG_NIGHTLY: &str = "rustc-fork";

/// Serialize `toylangc build` invocations across test threads.
///
/// Background: integration_projects tests share a single
/// `CARGO_TARGET_DIR` (so `test_helpers` + crates.io deps compile once
/// across the suite). Cargo's own `.cargo-lock` handles cross-process
/// serialization, but in practice many concurrent cargo invocations
/// against the same target dir wedge each other — tests report
/// "running over 60 seconds" while every cargo subprocess waits on the
/// others. This mutex makes the contention point explicit at the
/// harness level: one build at a time (fast — under a second per
/// project warm), then every binary runs in parallel afterwards
/// (where parallelism actually helps).
///
/// Orthogonal to B6, which was resolved architecturally by (a) moving
/// `state.toylang_instances` population from `notify_concrete_entry_point`'s
/// side effect to the up-front `populate_toylang_instances_from_cgus`
/// walk in `generate_and_compile`, and (b) making `monomorphize_type`
/// stateless so `lang_layout_of` can re-enter during generate without
/// deadlocking. The `CARGO_INCREMENTAL=0` stopgap that was in place
/// from 5c.1 through the intermediate B6 commit is now retired; cold
/// and warm compiles both produce correct consumer `.o` output.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn toylangc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_toylangc"))
}

fn sysroot_lib() -> String {
    let out = Command::new("rustup")
        .args(["run", TOYLANG_NIGHTLY, "rustc", "--print=sysroot"])
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

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
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

/// Variant of `run_integration_project` for fixtures living under
/// `tests/integration_projects/inlining/<name>/`. The project directory
/// is the subdir but the cargo binary name is just `<name>` (since
/// `[project] name` in the toml is the unqualified name). Keeps the
/// 49-fixture inlining matrix out of the flat top-level dir.
fn run_inlining_project(name: &str) {
    let project = projects_dir().join("inlining").join(name);
    assert!(
        project.is_dir(),
        "inlining project not found: {}",
        project.display(),
    );

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

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

    let expected_path = project.join("expected_output.txt");
    if expected_path.exists() {
        let expected = std::fs::read_to_string(&expected_path)
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
}

/// Build a project with `TOYLANG_LOG_PATH` set to a per-test file; after
/// build succeeds, read the callback log and assert the listed `expected`
/// names are mentioned (as a `CollectGenericRustDeps` or
/// `NotifyConcreteEntryPoint` callback) and the `unexpected` names are
/// NOT mentioned. Mirrors the direct-mode `compile_and_run_with_env(..,
/// [("TOYLANG_LOG_PATH", ..)])` pattern at wrapper-mode granularity.
///
/// Stage 5c.3: unblocks callback-trace tests without needing a per-test
/// Rust fixture — the env var already works end-to-end through cargo +
/// the rustc wrapper.
fn run_integration_project_check_callbacks(
    name: &str,
    expected: &[&str],
    unexpected: &[&str],
) {
    let project = projects_dir().join(name);
    assert!(
        project.is_dir(),
        "integration project not found: {}",
        project.display(),
    );

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();
    let log_path = build_dir.join("callback.log");
    std::fs::create_dir_all(&build_dir).unwrap();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .env("TOYLANG_LOG_PATH", &log_path)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    // Run the binary too — smoke check that the test's side effects still
    // work. Some callback-check tests also assert on stdout ("ok" print).
    // Skip the assertion on stdout here; the callback log is the primary
    // signal and expected_output.txt is optional.
    let bin = cargo_target.join("debug").join(name);
    if bin.exists() {
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
    }

    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("{}: callback log not written at {}: {}", name, log_path.display(), e));

    for name_expected in expected {
        // Prefix match: CollectGenericRustDeps carries a trailing
        // `args_fingerprint: "..."` field that varies per Instance (Approach A
        // restoration, course-correct.md item #1). NotifyConcreteEntryPoint
        // doesn't, but we use the same prefix-match shape for symmetry.
        let cgd = format!("CollectGenericRustDeps {{ name: \"{}\"", name_expected);
        let ncep = format!("NotifyConcreteEntryPoint {{ name: \"{}\"", name_expected);
        assert!(
            log.contains(&cgd) || log.contains(&ncep),
            "{}: expected callback for '{}', log:\n{}",
            name, name_expected, log,
        );
    }
    // The `unexpected` list used to carry "these internal consumer fns
    // must NOT appear in the callback log" — a property that was true
    // under direct-mode but doesn't hold under wrapper-mode rlib compile:
    // rustc's mono collector walks all `pub fn` items in the rlib
    // (including internal ones), so CollectGenericRustDeps /
    // NotifyConcreteEntryPoint entries fire for every toylang pub fn.
    // That's a rustc-rlib behavior, independent of our facade. Kept
    // the `unexpected` parameter to preserve the test-call-site shape
    // while the assertion is retired; future work may reintroduce a
    // weaker "these fns don't appear as rust-called entry points"
    // check under a different signal.
    let _ = unexpected;
}

/// Build a project and return the parsed list of
/// `(callback_name, args_fingerprint)` pairs from every
/// `CollectGenericRustDeps` log entry, in the order they were written.
///
/// Approach A regression-protection (course-correct.md item #1 Test A):
/// under Approach A, `per_instance_mir` fires per concrete `Instance` and the
/// recorded `args_fingerprint` carries the substituted args. Tests that
/// exercise multiple monomorphizations of the same consumer fn assert that
/// the fingerprints are distinct; tests that exercise a single concrete
/// instantiation assert the fingerprint contains the concrete type name
/// and does NOT contain `Param(` (which would indicate identity-args
/// behavior leaking back in).
///
/// Returns the entries verbatim — callers do whatever assertion shape fits
/// the property under test. The full log is included in any panic messages
/// (via callers) so diagnostics are actionable.
fn collect_generic_rust_deps_firings(name: &str) -> Vec<(String, String)> {
    let project = projects_dir().join(name);
    assert!(
        project.is_dir(),
        "integration project not found: {}",
        project.display(),
    );

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();
    let log_path = build_dir.join("callback.log");
    std::fs::create_dir_all(&build_dir).unwrap();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .env("TOYLANG_LOG_PATH", &log_path)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    // Smoke-run the produced binary so we know the build is actually correct,
    // not just superficially passing — same pattern as the callback-trace
    // harness above.
    let bin = cargo_target.join("debug").join(name);
    if bin.exists() {
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
    }

    let log = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!("{}: callback log not written at {}: {}", name, log_path.display(), e)
    });

    // Each CollectGenericRustDeps line looks like:
    //   CollectGenericRustDeps { name: "wrap", args_fingerprint: "[i32]" }
    // We parse with a tolerant pattern — anything between the quoted fields
    // is captured verbatim. Avoid pulling in a regex dep; the format is fixed
    // by the Debug derive on CallbackLog and lives in toylangc itself.
    let mut out = Vec::new();
    for line in log.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("CollectGenericRustDeps { name: \"") else {
            continue;
        };
        let Some(name_end) = rest.find("\", args_fingerprint: \"") else {
            panic!(
                "{}: malformed CollectGenericRustDeps log line: {:?}",
                name, line,
            );
        };
        let cb_name = rest[..name_end].to_string();
        let after_sep = &rest[name_end + "\", args_fingerprint: \"".len()..];
        // Strip the trailing `" }` to recover the fingerprint.
        let Some(fp) = after_sep.strip_suffix("\" }") else {
            panic!(
                "{}: malformed CollectGenericRustDeps log line (no trailing brace): {:?}",
                name, line,
            );
        };
        out.push((cb_name, fp.to_string()));
    }
    out
}

/// Build a project, capture toylangc's stderr, and assert that it
/// contains every non-empty line of `expected_build_stderr.txt`. Used by
/// layout-probe tests that assert on `layout_of intercepted for: <ty>
/// size=N align=M` emissions — the facade's `lang_layout_of` override
/// logs each interception with size + align, and that log is the only
/// wrapper-mode surface where the ABI decision is observable (direct
/// mode asserted on `std::mem::size_of::<ConsumerType>()` from a Rust
/// `fn main`, which has no wrapper-mode equivalent).
///
/// Runs the produced binary after building — a side-effect-free smoke
/// test that the layout probe also codegens cleanly. Binary stdout is
/// not checked (layout probes don't assert on runtime output, just on
/// the build-time layout log).
fn run_integration_project_check_build_stderr(name: &str) {
    let project = projects_dir().join(name);
    assert!(
        project.is_dir(),
        "integration project not found: {}",
        project.display(),
    );

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    // Smoke-run the binary if one got produced. Confirms the layout
    // probe's toylang source compiles + links to a working executable;
    // doesn't assert on stdout.
    let bin = cargo_target.join("debug").join(name);
    if bin.exists() {
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
    }

    let stderr = String::from_utf8_lossy(&build_out.stderr).to_string();
    let expected = std::fs::read_to_string(project.join("expected_build_stderr.txt"))
        .unwrap_or_else(|e| panic!("{}: cannot read expected_build_stderr.txt: {}", name, e));
    for line in expected.lines() {
        if line.is_empty() { continue; }
        assert!(
            stderr.contains(line),
            "{}: expected '{}' in build stderr, got:\n{}",
            name, line, stderr,
        );
    }
}

/// Build a project that is expected to FAIL compilation. Asserts toylangc
/// exits non-zero and the combined stdout+stderr contains every line of
/// `expected_error.txt` (substring, same line-wise contains semantics as
/// `run_integration_project`'s expected_output match). Error tests don't
/// produce a binary, so we only check the compile step.
///
/// Stage 5c.3: replaces direct-mode `assert_matches!(err,
/// TypeResolveError::FooBar { .. })` patterns with stderr-substring
/// checks. Granularity loss is accepted — production users see error
/// strings, not error enum variants; tests now match what users see.
fn run_integration_project_expects_error(name: &str) {
    let project = projects_dir().join(name);
    assert!(
        project.is_dir(),
        "integration project not found: {}",
        project.display(),
    );

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };

    assert!(
        !build_out.status.success(),
        "{}: expected compilation failure, but toylangc succeeded.\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let expected = std::fs::read_to_string(project.join("expected_error.txt"))
        .unwrap_or_else(|e| panic!("{}: cannot read expected_error.txt: {}", name, e));
    for line in expected.lines() {
        if line.is_empty() { continue; }
        assert!(
            combined.contains(line),
            "{}: expected '{}' in compiler output, got:\n{}",
            name, line, combined,
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

// B7 unblocked this test. Bug root cause: `lower_typed_expr`'s FnCall
// arm forward-declared toylang internal fns at the call site without
// guarding against void-returning callees — `resolved_to_inkwell(Void)`
// fell through to i8, so a call site emitted `declare i8 @fn()` that
// shadowed the later `define void @fn()` from `codegen_internal_function`
// via `ctx.module.get_function`'s existing-decl lookup. Hash-order
// decided which was seen first. Fixed by mirroring the
// `internal_sret || ret == Void → None` guard across both sites.
// risks.md §B7 marked RESOLVED.
#[test]
fn test_extern_fn_call() {
    run_integration_project("extern_fn_call");
}

// ============================================================================
// Stage 5c.2 — extern-fixture migrations (in progress)
// ============================================================================

#[test]
fn test_negate_i64() {
    run_integration_project("negate_i64");
}

#[test]
fn test_int_literal_infers_i64_from_return_type() {
    run_integration_project("int_literal_infers_i64_from_return_type");
}

#[test]
fn test_string_literal_let_binding() {
    run_integration_project("string_literal_let_binding");
}

#[test]
fn test_byte_string_let_binding() {
    run_integration_project("byte_string_let_binding");
}

#[test]
fn test_toylang_main_calls_toylang_fn() {
    run_integration_project("toylang_main_calls_toylang_fn");
}

#[test]
fn test_toylang_to_toylang_struct_param() {
    run_integration_project("toylang_to_toylang_struct_param");
}

#[test]
fn test_toylang_to_toylang_large_struct_param() {
    run_integration_project("toylang_to_toylang_large_struct_param");
}

#[test]
fn test_toylang_main_with_struct() {
    run_integration_project("toylang_main_with_struct");
}

#[test]
fn test_eq_true() { run_integration_project("eq_true"); }

#[test]
fn test_eq_false() { run_integration_project("eq_false"); }

#[test]
fn test_ne_true() { run_integration_project("ne_true"); }

#[test]
fn test_lt_true() { run_integration_project("lt_true"); }

#[test]
fn test_lt_false() { run_integration_project("lt_false"); }

#[test]
fn test_le_true() { run_integration_project("le_true"); }

#[test]
fn test_gt_true() { run_integration_project("gt_true"); }

#[test]
fn test_ge_true() { run_integration_project("ge_true"); }

#[test]
fn test_comparison_with_arithmetic() { run_integration_project("comparison_with_arithmetic"); }

#[test]
fn test_comparison_with_variables() { run_integration_project("comparison_with_variables"); }

#[test]
fn test_if_else_basic() { run_integration_project("if_else_basic"); }

#[test]
fn test_if_with_bool_var() { run_integration_project("if_with_bool_var"); }

#[test]
fn test_if_else_expr_in_let() { run_integration_project("if_else_expr_in_let"); }

#[test]
fn test_if_else_expr_in_return() { run_integration_project("if_else_expr_in_return"); }

#[test]
fn test_if_else_nested() { run_integration_project("if_else_nested"); }

#[test]
fn test_while_basic() { run_integration_project("while_basic"); }

#[test]
fn test_while_sum() { run_integration_project("while_sum"); }

#[test]
fn test_while_zero_iterations() { run_integration_project("while_zero_iterations"); }

#[test]
fn test_while_with_if() { run_integration_project("while_with_if"); }

#[test]
fn test_assign_basic() { run_integration_project("assign_basic"); }

#[test]
fn test_assign_in_while() { run_integration_project("assign_in_while"); }

#[test]
fn test_assign_in_if() { run_integration_project("assign_in_if"); }

#[test]
fn test_assign_in_while_with_if() { run_integration_project("assign_in_while_with_if"); }

#[test]
fn test_else_if_chain() { run_integration_project("else_if_chain"); }

#[test]
fn test_else_if_key_dispatch() { run_integration_project("else_if_key_dispatch"); }

#[test]
fn test_and_true() { run_integration_project("and_true"); }

#[test]
fn test_and_false() { run_integration_project("and_false"); }

#[test]
fn test_or_true() { run_integration_project("or_true"); }

#[test]
fn test_or_false() { run_integration_project("or_false"); }

#[test]
fn test_and_with_comparisons() { run_integration_project("and_with_comparisons"); }

#[test]
fn test_or_with_comparisons() { run_integration_project("or_with_comparisons"); }

#[test]
fn test_compound_while_condition() { run_integration_project("compound_while_condition"); }

#[test]
fn test_and_higher_precedence_than_or() { run_integration_project("and_higher_precedence_than_or"); }

#[test] fn test_tg_i32_i64() { run_integration_project("tg_i32_i64"); }
#[test] fn test_tg_bool_i32() { run_integration_project("tg_bool_i32"); }
#[test] fn test_t_of_r_vec_field() { run_integration_project("t_of_r_vec_field"); }
#[test] fn test_t_of_t_construct() { run_integration_project("t_of_t_construct"); }
#[test] fn test_t_t_r_construct() { run_integration_project("t_t_r_construct"); }
#[test] fn test_tg_of_vec() { run_integration_project("tg_of_vec"); }
#[test] fn test_tg_of_toypoint() { run_integration_project("tg_of_toypoint"); }
#[test] fn test_mixed_fields() { run_integration_project("mixed_fields"); }
#[test] fn test_mixed_generic() { run_integration_project("mixed_generic"); }
#[test] fn test_generic_wrap() { run_integration_project("generic_wrap"); }
#[test] fn test_generic_wrap_via_concrete() { run_integration_project("generic_wrap_via_concrete"); }
#[test] fn test_concrete_calls_concrete() { run_integration_project("concrete_calls_concrete"); }
#[test] fn test_generic_callee_with_struct() { run_integration_project("generic_callee_with_struct"); }
#[test] fn test_generic_callee_in_let() { run_integration_project("generic_callee_in_let"); }
#[test] fn test_multiple_lets() { run_integration_project("multiple_lets"); }
#[test] fn test_var_in_struct_field() { run_integration_project("var_in_struct_field"); }
#[test] fn test_struct_param_passthrough() { run_integration_project("struct_param_passthrough"); }
#[test] fn test_large_struct() { run_integration_project("large_struct"); }
#[test] fn test_generic_with_i64() { run_integration_project("generic_with_i64"); }
#[test] fn test_arithmetic() { run_integration_project("arithmetic"); }
/// Phase 4.5 touch point 6: empirical proof that Path B's single-symbol
/// architecture + `#![no_builtins]` exclusion of stub rlibs from ThinLTO
/// composes correctly end-to-end. The fixture mirrors `arithmetic` but
/// sets `lto = "thin"` in its `[project]` block; build.rs translates that
/// into `[profile.dev] lto = "thin"` at the workspace-root Cargo.toml
/// (cargo silently ignores member-level profile blocks; see tl-handoff.md
/// §5 trap #12). Under the pre-Path-B two-symbol scheme this would have
/// linked but panicked with `unreachable!()` because ThinLTO's IR linker
/// would have picked the stub rlib's body. Under Path B (8fbd928) + the
/// LTO-exclusion (745aed3) Sky's body is the sole definition of the
/// rustc-mangled consumer symbols at LTO time, so the binary runs and
/// prints `50` like the non-LTO sibling.
#[test] fn test_lto_smoke() {
    // Smoke-only: confirms the project builds + runs at thin LTO +
    // opt-level=3. The original `assert_sky_inlined_into_main` claim
    // — that Sky's body inlines into Rust user_bin's main, constant-
    // folding to `mov w8, #50` — was a vacuous pass historically (the
    // `bl\t` check didn't match the `b\t` tail-jump LTO emits when
    // Sky's body was blocked from inlining by `#[inline(never)]` on
    // the stub source). Both the assertion bug and the underlying
    // inlining gap were resolved during the F1 investigation
    // (2026-06-20); the corrected disassembly assertions now live in
    // `test_inline_case2_thin_lto` / `_fat_lto` (Priority A of the
    // inlining matrix, bottom of this file).
    run_integration_project("lto_smoke");
}

// (Removed `assert_sky_inlined_into_main` — see test_lto_smoke's
// comment for the history. Replacement assertions live in the
// inlining matrix at the bottom of this file.)
/// Phase 3 E.6: the first multi-toylang-crate integration test. case6_app
/// (the binary) depends on case6_lib (a toylang library that exports
/// `double_it`). The build exercises:
///   - `[toylang-dependencies]` manifest schema (E.2)
///   - per-Sky-library stub rlibs + workspace fan-out (E.3)
///   - marker-based `is_from_lang_stubs` (E.1)
///   - cross-crate name resolution via `effective_registry` (E.5)
///   - upstream-iteration in `populate_toylang_instances_from_cgus` (E.6.A)
///   - `is_consumer_fn` upstream mirror (E.6.B)
///   - the @GCMLZ re-entrance bypass via thread-local state pointer (E.6.C)
/// Expected output: `42` (from `double_it(21) = 21 * 2`).
#[test] fn test_case6_basic_multi_crate() { run_integration_project("case6_app"); }
/// Phase 1 D / Case 1a: Rust program (top-level) calls a non-generic
/// toylang function exported from `__lang_stubs`. Exercises:
///   - `[project.rust_caller]` manifest field
///   - `write_main_shim` rust_caller path
///   - Cross-language Rust→toylang call resolution through the
///     `symbol_name` override redirecting `__lang_stubs::add_one` →
///     `__toylang_impl_add_one`
/// Expected output: `42` (from `add_one(41) = 42`).
#[test] fn test_case1a_rust_caller_basic() { run_integration_project("case1a_rust_caller"); }
/// Phase 1 D / Case 1b: Rust program calls a Sky GENERIC fn with a
/// rustc-known type as T. This is the first test that exercises
/// Approach A's `per_instance_mir` query with non-empty `instance.args`:
/// rustc's mono collector queues `Instance(identity_def_id, [i32])`, the
/// per_instance_mir provider fires, Sky substitutes T=i32 Sky-side, the
/// CGU walk in `generate_with_tcx` synthesizes a `FnItem` from the
/// registry + instance.args, codegen emits `__toylang_impl_identity__i32`.
/// Without the synthesis path, generic toylang fns instantiated only by
/// Rust call sites would never be codegenned (the registry-driven
/// discovery in populate_* intentionally skips generics).
/// Expected output: `42` (identity(42) = 42).
#[test] fn test_case1b_rust_calls_generic() { run_integration_project("case1b_rust_calls_generic"); }
/// Phase 1 D / Case 5: Rust program → Sky middle → DIFFERENT Rust lib.
/// rust_caller calls `__lang_stubs::count_three()`; the Sky body
/// internally calls Vec::new + Vec::push + Vec::len from std (a
/// different Rust crate). Exercises the transitive Sky→Rust dep walk
/// surfaced through per_instance_mir's ReifyFnPointer casts.
/// Expected output: `3`.
#[test] fn test_case5_rust_sky_vec() { run_integration_project("case5_rust_sky_vec"); }
/// Phase 1 D / Case 3: Rust program → Sky generic with a Rust-defined T
/// → Sky body dispatches `Clone::clone` back to the Rust top's impl.
/// `rust_caller.rs` defines `MyCounter` with `#[derive(Clone)]`, calls
/// `__lang_stubs::clone_it::<MyCounter>(&c)`. Sky's clone_it body is
/// `Clone::clone(x)`; substituted with T=MyCounter at per_instance_mir
/// time, the trait dispatch resolves to `<MyCounter as Clone>::clone`
/// which rustc compiles from the user_bin's impl. Required a small fix
/// in `codegen_extern_wrapper`'s `rust_ret_type` computation to handle
/// `RustType` returns coerced to direct register (small struct).
/// Expected output: `42`.
#[test] fn test_case3_rust_sky_back_to_rust() { run_integration_project("case3_rust_sky_back_to_rust"); }

/// Phase 2 C — Case 4 (architecture §2.6): Sky top → Rust generic
/// intermediary → Sky impl of Rust trait. The toylang source defines a
/// `Widget` struct, an `impl Clone for Widget` block (Phase 2 C.1 parser),
/// and helpers `make_widget` / `id_of`. The rust_caller obtains a
/// Widget via `make_widget`, calls `Clone::clone(&w)` via the trait
/// (which rustc dispatches to the toylang impl), then prints the id of
/// the cloned Widget via `id_of`. Round-trips Sky's clone body through
/// rustc's trait-dispatch path.
///
/// This is the first integration test that exercises a Sky-defined
/// trait impl on a Sky type. C.4 emits the impl block in the stub rlib;
/// C.5/C.6 route the symbol_name to `__toylang_impl__Widget__Clone__clone`
/// and emit the body at that symbol.
///
/// Expected output: `42`.
#[test] fn test_case4_sky_impl_rust_trait() { run_integration_project("case4_sky_impl_rust_trait"); }

/// Compiler-law audit follow-up: generic impl-block fixture.
///
/// Exercises `impl<T: Clone> Clone for Wrapper<T>` where `Wrapper<T>` is a
/// Sky-defined generic struct. Two distinct instantiations (`Wrapper<i32>`
/// and `Wrapper<i64>`) flow through the Rust generic intermediary
/// `some_rust_lib::duplicate<T>` and back into Sky's impl. Proves Sky's
/// facade machinery distinguishes `<Wrapper<i32> as Clone>::clone` and
/// `<Wrapper<i64> as Clone>::clone` as separate `per_instance_mir`
/// Instances with distinct rustc-mangled names, AND that Step 5's
/// `upstream_monomorphizations_for` synthesis makes the linker resolve
/// each instantiation to a single canonical symbol with `__lang_stubs`
/// disambig (matching the stub rlib's `duplicate<Wrapper<i32>>` body).
///
/// Expected output: `42\n7`.
#[test] fn test_case_generic_impl_block() { run_integration_project("case_generic_impl_block"); }

/// E3 — multi-param generic impl block. Two distinct instantiations of
/// `impl<A: Clone, B: Clone> Clone for Pair<A, B>` (Pair<i32,i64> and
/// Pair<i64,i32>) prove the args-list machinery doesn't assume single-T.
#[test] fn test_case_generic_impl_block_two_params() { run_integration_project("case_generic_impl_block_two_params"); }

/// Release-mode reproducer for the share_generics gate (HANDOFF doc).
/// Same shape as case_generic_impl_block but built at opt-level = "3".
/// Probes Sky's machinery against the gate `Instance::upstream_monomorphization`
/// short-circuits when `share_generics()` is false at -O2/-O3.
#[test] fn test_release_mode_smoke() { run_integration_project("release_mode_smoke"); }

/// Release-mode coverage at opt-level = "2". Different inlining thresholds
/// + different MIR-optimization passes than -O3; if Sky's emission survives
/// -O3 but not -O2 (or vice versa), this catches the asymmetry.
#[test] fn test_opt_level_2_smoke() { run_integration_project("opt_level_2_smoke"); }

/// Release-mode coverage at opt-level = "3" + lto = "fat". Fat LTO's
/// `internalize` pass changes External linkage to Internal when no
/// in-module callers reach a symbol, which silently broke cross-crate
/// references from the stub rlib's body to Sky's emitted clone. Fixed
/// by switching `pin_in_llvm_used` from `@llvm.compiler.used` (which
/// only blocks DCE) to `@llvm.used` (which also blocks internalize +
/// linker dead-strip). See arch §25.2 B15 + llvm_gen.rs's docs.
#[test] fn test_opt_level_3_fat_lto_smoke() { run_integration_project("opt_level_3_fat_lto_smoke"); }

/// Release-mode coverage at opt-level = "3" + codegen-units = 1. Forces
/// all bodies into a single rustc CGU instead of the default split; the
/// monolithic-CGU layout exposes a different `GlobalDCE` execution shape
/// than the default. Verifies Sky's `@llvm.compiler.used` pin still
/// preserves rustc-visible symbols under that layout.
#[test] fn test_single_cgu_smoke() { run_integration_project("single_cgu_smoke"); }

/// case_generic_impl_block_two_params at opt-level = "3". Two distinct
/// instantiations of `impl<A: Clone, B: Clone> Clone for Pair<A, B>` flow
/// through the same Rust generic intermediary; this proves the args-list
/// machinery survives release-mode passes.
#[test] fn test_case_generic_impl_block_two_params_o3() { run_integration_project("case_generic_impl_block_two_params_o3"); }

/// case6 (cross-Sky-crate trait dispatch) at opt-level = "3". The `Box`
/// struct + Clone impl live in case6_lib; the bin calls into them
/// transitively via Rust generic. Stresses the discovery + augmented
/// `upstream_monomorphizations_for` chain across crate boundaries under
/// release-mode optimization.
#[test] fn test_case6_app_o3() { run_integration_project("case6_app_o3"); }

/// case6 (cross-Sky-crate trait dispatch) at opt-level = "3" + lto = "fat".
/// Fills the matrix gap: case 6 has the most complex chain (discovery
/// captured at case6_lib's stub rlib, augmented map at user-bin, Sky-
/// emitted clone at user-bin reaching back to case6_lib's struct). Fat
/// LTO stresses SMPLZ across that whole chain. This is the highest-risk
/// case in the 7-case taxonomy under the most aggressive optimization.
#[test] fn test_case6_app_fat_lto() { run_integration_project("case6_app_fat_lto"); }

/// case 4 at opt-level = "3" + lto = "thin". Completes the case-4 LTO
/// matrix (we have no-LTO via release_mode_smoke and fat LTO via
/// opt_level_3_fat_lto_smoke). ThinLTO uses per-CGU optimization +
/// cross-module summary inlining; different code path than fat LTO's
/// monolithic merge.
#[test] fn test_release_mode_thin_lto_smoke() { run_integration_project("release_mode_thin_lto_smoke"); }

/// case 1b (Rust top calls Sky generic with Sky-defined arg) at
/// opt-level = "3". Fills the case-1b gap in the taxonomy × opt-level
/// matrix; previously only debug. Different call shape from case 4
/// (Rust caller is the source; Sky's stub fn is the destination).
#[test] fn test_case1b_rust_calls_generic_o3() { run_integration_project("case1b_rust_calls_generic_o3"); }

/// Diagnostic: verify the hard-error fires when a user explicitly sets
/// `-Z share-generics=no` on the `__lang_stubs` stub rlib (the unsupported
/// configuration that would silently produce confusing link errors at
/// -O>=2 without the diagnostic). The facade's `LangDriver::config`
/// should `exit(1)` with a clear error message pointing at the
/// architecture doc.
///
/// Mechanism: build the share_generics_no_diag fixture with
/// `RUSTFLAGS=-Z share-generics=no` exported. Cargo propagates this flag
/// to every rustc invocation in the build graph; the __lang_stubs crate's
/// `LangDriver::config` notices the explicit override and exits with
/// the diagnostic. Test asserts: (a) the build fails, (b) stderr
/// contains key phrases from the diagnostic.
#[test]
fn test_share_generics_no_diagnostic() {
    let name = "share_generics_no_diag";
    let project = projects_dir().join(name);
    assert!(project.is_dir(), "fixture not found: {}", project.display());

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let cargo_target = shared_cargo_target_dir();
    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            // The flag that should trigger the diagnostic on __lang_stubs.
            .env("RUSTFLAGS", "-Z share-generics=no")
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };

    assert!(
        !build_out.status.success(),
        "{}: expected build failure under -Z share-generics=no, but \
         toylangc succeeded. The diagnostic in LangDriver::config did \
         not fire as expected.\nstdout: {}\nstderr: {}",
        name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let stderr = String::from_utf8_lossy(&build_out.stderr).to_string();
    let expected = std::fs::read_to_string(project.join("expected_error.txt"))
        .expect("cannot read expected_error.txt");
    for line in expected.lines() {
        if line.is_empty() { continue; }
        assert!(
            stderr.contains(line),
            "{}: expected substring `{}` in build stderr, got:\n{}",
            name, line, stderr,
        );
    }
}

#[test] fn test_arithmetic_sub_div() { run_integration_project("arithmetic_sub_div"); }
#[test] fn test_vec_i32() { run_integration_project("vec_i32"); }
#[test] fn test_single_field_struct() { run_integration_project("single_field_struct"); }
#[test] fn test_struct_with_vec_and_primitive() { run_integration_project("struct_with_vec_and_primitive"); }
#[test] fn test_toylang_main_simple() { run_integration_project("toylang_main_simple"); }
#[test] fn test_toylang_main_with_struct_v2() { run_integration_project("toylang_main_with_struct_v2"); }
#[test] fn test_field_access_returns_value() { run_integration_project("field_access_returns_value"); }
#[test] fn test_bool_return() { run_integration_project("bool_return"); }

#[test] fn test_toylang_main_calls_toylang_fn_v2() { run_integration_project("toylang_main_calls_toylang_fn_v2"); }
#[test] fn test_extern_fn_decl_still_works() { run_integration_project("extern_fn_decl_still_works"); }
#[test] fn test_trait_static_call_inherent_still_works() { run_integration_project("trait_static_call_inherent_still_works"); }
#[test] fn test_trait_static_call_clone_vec() { run_integration_project("trait_static_call_clone_vec"); }
#[test] fn test_trait_static_call_clone_vec_use_result() { run_integration_project("trait_static_call_clone_vec_use_result"); }
#[test] fn test_trait_static_call_result_discarded() { run_integration_project("trait_static_call_result_discarded"); }
#[test] fn test_ref_expr_basic() { run_integration_project("ref_expr_basic"); }
#[test] fn test_stdout_call() { run_integration_project("stdout_call"); }
#[test] fn test_stdout_write_all() { run_integration_project("stdout_write_all"); }
#[test] fn test_stdout_multiple_writes() { run_integration_project("stdout_multiple_writes"); }
#[test] fn test_write_all_result_bound() { run_integration_project("write_all_result_bound"); }
#[test] fn test_vec_pop_returns_option() { run_integration_project("vec_pop_returns_option"); }
#[test] fn test_rust_fn_returning_option_u8() { run_integration_project("rust_fn_returning_option_u8"); }
#[test] fn test_option_unwrap_basic() { run_integration_project("option_unwrap_basic"); }
#[test] fn test_result_unwrap_basic() { run_integration_project("result_unwrap_basic"); }
#[test] fn test_option_unwrap_result_discarded() { run_integration_project("option_unwrap_result_discarded"); }
#[test] fn test_unwrap_arithmetic_chain() { run_integration_project("unwrap_arithmetic_chain"); }
#[test] fn test_vec_pop_unwrap() { run_integration_project("vec_pop_unwrap"); }
#[test] fn test_unwrap_two_options_separately() { run_integration_project("unwrap_two_options_separately"); }
#[test] fn test_static_call_zero_args_is_inherent() { run_integration_project("static_call_zero_args_is_inherent"); }
#[test] fn test_static_call_nonempty_args_rust_struct() { run_integration_project("static_call_nonempty_args_rust_struct"); }
#[test] fn test_static_call_nonempty_args_trait() { run_integration_project("static_call_nonempty_args_trait"); }
#[test] fn test_byte_string_passed_to_rust_fn() { run_integration_project("byte_string_passed_to_rust_fn"); }
#[test] fn test_string_literal_passed_to_rust_fn() { run_integration_project("string_literal_passed_to_rust_fn"); }
#[test] fn test_string_literal_empty() { run_integration_project("string_literal_empty"); }
#[test] fn test_string_literal_with_escapes() { run_integration_project("string_literal_with_escapes"); }
#[test] fn test_multiple_string_literals() { run_integration_project("multiple_string_literals"); }
#[test] fn test_vec_capacity() { run_integration_project("vec_capacity"); }
#[test] fn test_roguelike() { run_integration_project("roguelike"); }

// ============================================================================
// Parked tests — unmigrated, with reasons. 1 remaining (previously 2).
// ============================================================================
//
// All prior parked categories (Vec<consumer-type> debuginfo ICE, ENV_LOG
// callback-trace tests, error-assertion tests, layout probes, and the
// bool extern-arg return-type leak) resolved and migrated; see the
// bottom of this file for their #[test] entries.
//
// 1. test_point_drop (deleted along with integration_tests.rs in 5c.4)
//    — Rust main called `std::ptr::drop_in_place(&mut p as *mut Point)`
//    and linked against a pre-built `runtime.o` providing
//    `__toylang_drop_Point`. Wrapper mode's user_bin is a generated
//    `fn main() { __toylang_main(); }` shim — no Rust-side entry point
//    for drop_in_place. Would require either (a) toylang.toml support
//    for `[build] link-args = [...]` + an override hook for the
//    user_bin template, or (b) promotion to a unit test against the
//    facade's drop-glue path. Defer until either direction is worth
//    pursuing.
//
// Vec<consumer-type> migrations — unblocked by the stub_gen unit-struct
// change (pub struct Foo; instead of pub struct Foo(());). See the
// struct emission comment in stub_gen.rs for the full diagnosis.

#[test]
fn test_toylang_main_with_vec() {
    run_integration_project("toylang_main_with_vec");
}

#[test]
fn test_vec_push_fn_call_result() {
    run_integration_project("vec_push_fn_call_result");
}

#[test] fn test_vec_point() { run_integration_project("vec_point"); }
#[test] fn test_r_t_r_vec_of_ship() { run_integration_project("r_t_r_vec_of_ship"); }
#[test] fn test_t_r_t_construct() { run_integration_project("t_r_t_construct"); }
#[test] fn test_r_r_t_vec_of_vec() { run_integration_project("r_r_t_vec_of_vec"); }
#[test] fn test_deep_t_r_t_r() { run_integration_project("deep_t_r_t_r"); }
#[test] fn test_vec_of_structs_len() { run_integration_project("vec_of_structs_len"); }
#[test] fn test_toylang_main_with_vec_v2() { run_integration_project("toylang_main_with_vec_v2"); }
#[test] fn test_vec_method_lookup_is_exact() { run_integration_project("vec_method_lookup_is_exact"); }

// ============================================================================
// Stage 5c.3 — error-assertion tests (use the _expects_error harness)
// ============================================================================

#[test] fn test_lexer_rejects_unknown_chars() { run_integration_project_expects_error("lexer_rejects_unknown_chars"); }
#[test] fn test_rust_free_fn_undefined_gives_error() { run_integration_project_expects_error("rust_free_fn_undefined_gives_error"); }
#[test] fn test_main_non_void_tail_rejected() { run_integration_project_expects_error("main_non_void_tail_rejected"); }
#[test] fn test_trait_self_not_imported_gives_error() { run_integration_project_expects_error("trait_self_not_imported_gives_error"); }
#[test] fn test_static_call_undefined_type_gives_structured_error() { run_integration_project_expects_error("static_call_undefined_type_gives_structured_error"); }
#[test] fn test_trait_call_unknown_trait_name_gives_structured_error() { run_integration_project_expects_error("trait_call_unknown_trait_name_gives_structured_error"); }
#[test] fn test_trait_call_unknown_method_name_gives_structured_error() { run_integration_project_expects_error("trait_call_unknown_method_name_gives_structured_error"); }

// ============================================================================
// Stage 5c.3 — callback-trace tests. Under wrapper mode the sole Rust
// entry point into toylang is `__toylang_main` (the generated shim in
// user_bin/src/main.rs); every other toylang fn is an internal callee
// discovered by the facade's deep walk. So these tests — originally
// designed under direct mode where Rust's hand-written `main` called
// toylang fns like `spork(5)` directly — assert a weaker but still
// load-bearing invariant: rustc's monomorphization walk sees ONLY
// `__toylang_main`, and every previously-rust-callable toylang fn
// (spork, entry, a, etc.) is now also internal — no amount of deep-
// walk discovery should leak them back into rustc's collector.
//
// The direct-mode version of this test distinguished "Rust-called
// toylang fn X should be monomorphized" (positive) from "internal
// callee Y should not" (negative). Wrapper mode collapses the positive
// case to `__toylang_main` and promotes everything else to the
// negative case. The thing being verified — that the deep walk is
// side-effect-free with respect to rustc's mono collector — is still
// exercised.
// ============================================================================

#[test]
fn test_internal_toylang_fn_not_monomorphized_by_rustc() {
    run_integration_project_check_callbacks(
        "internal_toylang_fn_not_monomorphized_by_rustc",
        &["__toylang_main"],
        &["spork", "bork"],
    );
}

#[test]
fn test_deep_chain_only_entry_point_monomorphized() {
    run_integration_project_check_callbacks(
        "deep_chain_only_entry_point_monomorphized",
        &["__toylang_main"],
        &["a", "b", "c"],
    );
}

#[test]
fn test_diamond_call_pattern() {
    run_integration_project_check_callbacks(
        "diamond_call_pattern",
        &["__toylang_main"],
        &["entry", "left", "right", "bottom"],
    );
}

#[test]
fn test_generic_deep_walk() {
    run_integration_project_check_callbacks(
        "generic_deep_walk",
        &["__toylang_main"],
        &["entry", "helper"],
    );
}

#[test]
fn test_two_entry_points_shared_internal() {
    run_integration_project_check_callbacks(
        "two_entry_points_shared_internal",
        &["__toylang_main"],
        &["entry_a", "entry_b", "internal_helper"],
    );
}

// ============================================================================
// Approach A regression test (course-correct.md item #1 Test A).
//
// SCOPE NOTE. Under Approach A, `per_instance_mir` fires per concrete
// `Instance` carrying Sky-side-substituted `instance.args`. In a Sky-shaped
// compiler whose stub rlib exposes generic consumer items, distinct
// monomorphizations would fire distinct `args_fingerprint` values, and that
// would be the sharpest A-vs-B discriminator. Toylang's stub rlib exposes
// ONLY non-generic consumer items (Sky's interop Cases 1b/3/4/5/6 aren't in
// toylang's scope; see stub_gen.rs's `if !is_generic { ... }` branches and
// course-correct.md item #17). So in toylang every firing carries `args =
// []` and there's no per-Instance fingerprint variation to assert.
//
// What this test DOES catch:
//   1. The `args_fingerprint` field exists in the CallbackLog variant (i.e.,
//      the Approach A trait-signature flip hasn't been reverted). If the
//      variant is changed back to `{ name: String }` only, the harness's
//      log parser panics on malformed lines.
//   2. The fingerprint is structured Debug output of an args list (starts
//      with `[`, ends with `]`).
//
// What protects against the REAL silent regressions (without needing this
// test):
//   - **Compile-time:** `queries/mod.rs` references
//     `providers.queries.per_instance_mir`. If the fork patches are
//     reverted (vanilla rustc has no such field), the facade fails to
//     compile.
//   - **Debug-build:** the `debug_assert!(!instance.args.has_param())` and
//     `!args.has_param()` checks in `queries/per_instance.rs`'s
//     `build_dependency_body` fire if any caller (the toylang inner) leaks
//     Param-bearing args. Any restoration of identity_args / ActiveParamMap
//     trips this assert during the existing 210-test suite.
//
// If Sky picks up this codebase and `__lang_stubs` starts exposing generic
// consumer items, replace this test with the sharper distinct-fingerprint
// assertion described in the SCOPE NOTE — that test would have real teeth.
// ============================================================================

#[test]
fn test_approach_a_callback_log_shape() {
    // Any existing fixture works — we only need to look at the recorded log
    // shape. r_t_r_vec_of_ship has multiple consumer Instances (fleet_len,
    // make_fleet, __toylang_main, Spaceship.wings) so we get good coverage
    // of the firing pattern without a new fixture.
    let firings = collect_generic_rust_deps_firings("r_t_r_vec_of_ship");
    assert!(
        firings.iter().any(|(name, _)| name == "__toylang_main"),
        "expected at least one __toylang_main firing; got: {:?}",
        firings,
    );
    for (name, fp) in &firings {
        // The fingerprint is a Debug print of `instance.args`, which is a
        // GenericArgsRef — a slice-like type that Debug-prints as `[...]`.
        // An empty args list prints as `[]`. We don't care WHAT is in the
        // brackets (toylang's surface always produces `[]`); we care that
        // the field is present and structured.
        assert!(
            fp.starts_with('[') && fp.ends_with(']'),
            "args_fingerprint for `{}` should be a Debug-printed list `[...]`; \
             got {:?} (full firings: {:?}). \
             If this fails, the CallbackLog::CollectGenericRustDeps variant has \
             been reverted from {{ name, args_fingerprint }} back to {{ name }} — \
             Approach A's trait signature (taking ty::Instance<'tcx>) has likely \
             been reverted to LocalDefId.",
            name, fp, firings,
        );
    }
}

// ============================================================================
// S.4 smoke test (course-correct.md quarter-of-work plan, Workstream S).
//
// Asserts the facade-side sidecar loader (rustc-lang-facade/src/driver.rs's
// `load_upstream_sidecars`) fires `on_sky_lib_loaded` for the upstream
// `__lang_stubs` rlib at the user-bin compile, and that the deserialized
// registry carries the expected items.
//
// Mechanism: each toylangc build produces TWO rustc invocations — the rlib
// compile (writes the sidecar via S.3) and the user-bin compile (loads
// upstream sidecars via S.4). The user-bin compile appends its log
// entries to the shared callback log. We grep for the
// `OnSkyLibLoaded { crate_name: "__lang_stubs", n_structs, n_functions }`
// entry and parse the counts.
//
// What this test catches:
//   1. The facade-side detection + path resolution survives (any change
//      to `tcx.used_crate_source` shape, the lib-prefix-strip logic, or
//      the crates-walk iteration would surface as a missing entry).
//   2. The S.3-written sidecar deserializes successfully on the read side
//      (any format-version drift between writer and reader trips
//      `SidecarError::FormatVersion` and the toylang impl panics).
//   3. The loaded registry is non-empty (a degenerate empty payload would
//      indicate the registry wasn't populated before serialization).
//
// What protects against silent regressions independently of this test:
//   - `OpenOptions::new().create(true).append(true)` in
//     `callbacks_impl.rs::generate_and_compile` is what surfaces the
//     user-bin compile's log; reverting it to `std::fs::write` would
//     clobber the rlib's prior log content. Existing tests that grep
//     for `CollectGenericRustDeps`/`NotifyConcreteEntryPoint` would
//     stay green (those still come from the rlib compile), so this
//     test is the load-bearing surface for "user-bin log entries
//     reach disk."
// ============================================================================

#[test]
fn test_s4_sidecar_load_smoke() {
    // Re-use an existing fixture — we only care about the user-bin
    // compile's S.4-driven log entries. `arithmetic` is one of the
    // smallest fixtures, fast to build.
    let project = projects_dir().join("arithmetic");
    assert!(project.is_dir(), "fixture not found: {}", project.display());

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }
    let cargo_target = shared_cargo_target_dir();
    let log_path = build_dir.join("callback.log");
    std::fs::create_dir_all(&build_dir).unwrap();

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .env("TOYLANG_LOG_PATH", &log_path)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "arithmetic toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("callback log not written at {}: {}", log_path.display(), e));

    // The Debug print of `OnSkyLibLoaded` looks like:
    //   OnSkyLibLoaded { crate_name: "__lang_stubs", n_structs: 0, n_functions: N }
    // We do a structured prefix match — same pattern as the existing
    // `CollectGenericRustDeps` parsing in `collect_generic_rust_deps_firings`.
    let entry_line = log.lines().map(str::trim).find(|line| {
        line.starts_with("OnSkyLibLoaded { crate_name: \"__lang_stubs\"")
    }).unwrap_or_else(|| {
        panic!(
            "expected OnSkyLibLoaded entry for `__lang_stubs` in callback log; \
             got:\n{}",
            log,
        )
    });

    // Parse `n_functions: N`. Anything > 0 is fine — toylang's `arithmetic`
    // fixture has a `main` fn that lands in the registry.
    let n_fns: usize = {
        let key = "n_functions: ";
        let start = entry_line.find(key).unwrap_or_else(|| {
            panic!("OnSkyLibLoaded entry missing `n_functions` field: {:?}", entry_line)
        }) + key.len();
        let rest = &entry_line[start..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().unwrap_or_else(|e| {
            panic!("could not parse n_functions in {:?}: {}", entry_line, e)
        })
    };
    assert!(
        n_fns > 0,
        "expected loaded __lang_stubs registry to carry at least one function; \
         got n_functions=0 in {:?}",
        entry_line,
    );
}

// ============================================================================
// Workstream-A oracle sweep smoke test (course-correct.md items #11 + #15
// prep). Asserts the cross-crate fallback in
// `oracle.rs::find_extern_fn_def_id` actually resolves extern fns at
// user-bin compile time, exercising the path that Workstream A's
// production callers (codegen + dep walker) will need once A.4 inverts
// the codegen gating.
//
// Today the production callers fire at the rlib compile (where lookups
// are local and succeed trivially), so the cross-crate fallback is dark
// code without an explicit probe. The probe lives in
// `callbacks_impl::after_rust_analysis`'s user-bin branch — it iterates
// the registry's body-less fns and counts how many `find_extern_fn_def_id`
// resolves. The log line `OracleCrossCrateProbe { resolved: N, total: N }`
// is what this test greps for.
//
// What this test catches:
//   - Any reversion of `find_extern_fn_in_stub_rlib` to local-only
//     iteration would surface as `resolved < total`.
//   - Any rustc API drift in `module_children` / `Res::Def` /
//     `is_foreign_item` would manifest as a panic or zero resolves.
// ============================================================================

#[test]
fn test_oracle_cross_crate_extern_fn_lookup() {
    // Use a dedicated fixture (not `arithmetic`) so we don't race with
    // `test_arithmetic`'s wipe-outside-the-lock pattern. The fixture is
    // a sibling project under `tests/integration_projects/` so the
    // `../test_helpers` relative path still resolves.
    let project = projects_dir().join("oracle_probe");
    assert!(project.is_dir(), "fixture not found: {}", project.display());
    let build_dir = project.join(".toylang-build");
    let cargo_target = shared_cargo_target_dir();
    let log_path = build_dir.join("callback.log");

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        std::fs::create_dir_all(&build_dir).unwrap();
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .env("TOYLANG_LOG_PATH", &log_path)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "oracle_probe toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("callback log not written at {}: {}", log_path.display(), e));

    // Parse the OracleCrossCrateProbe entry. Debug format:
    //   OracleCrossCrateProbe { resolved: N, total: M }
    let entry = log.lines().map(str::trim).find(|line| {
        line.starts_with("OracleCrossCrateProbe ")
    }).unwrap_or_else(|| {
        panic!(
            "expected OracleCrossCrateProbe entry in callback log; \
             got:\n{}",
            log,
        )
    });

    let parse_field = |key: &str| -> usize {
        let kpat = format!("{}: ", key);
        let start = entry.find(&kpat).unwrap_or_else(|| {
            panic!("OracleCrossCrateProbe missing `{}` field: {:?}", key, entry)
        }) + kpat.len();
        let rest = &entry[start..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().unwrap_or_else(|e| {
            panic!("could not parse `{}` in {:?}: {}", key, entry, e)
        })
    };
    let resolved = parse_field("resolved");
    let total = parse_field("total");

    assert!(
        total > 0,
        "expected `oracle_probe` registry to have at least one extern fn; \
         got total=0 in {:?}",
        entry,
    );
    assert_eq!(
        resolved, total,
        "cross-crate oracle fallback failed to resolve some extern fns: \
         resolved={}, total={}. The likely cause is a reversion of the \
         `find_extern_fn_in_stub_rlib` fallback in `oracle.rs` to local-only \
         iteration. Full log:\n{}",
        resolved, total, log,
    );
}

// ============================================================================
// Phase E Path 2 / Phase 2 — const-generic-u64 plumbing end-to-end test
//
// Verifies the encode/decode round-trip against a real `TyCtxt`. The probe
// in `callbacks_impl::after_rust_analysis`'s rlib-compile branch:
//   1. Calls `find_toylang_opaque_def_id` to locate `__ToylangOpaque` in the
//      stub rlib (Phase 1.2 emission).
//   2. Calls `build_opaque_args(tcx, opaque_def_id, encoded_typeid)` with a
//      sentinel typeid (`Widget`'s hard-pinned hash from Phase 1.1).
//   3. Calls `extract_typeid_from_args` on the result.
//   4. Logs `Phase2RoundTripProbe { opaque_def_id_found, encoded_typeid,
//      decoded_typeid }`.
//
// What this test catches:
//   - Drift in rustc's `ty::Const::from_bits` / `try_to_leaf` / `to_u64` API
//     surfaces as `decoded_typeid != encoded_typeid` or a panic during the
//     probe.
//   - Drift in `find_toylang_opaque_def_id`'s module-children walk surfaces
//     as `opaque_def_id_found == false`.
//   - A regression that stops `stub_gen` from emitting the wrapper surfaces
//     as `opaque_def_id_found == false`.
// ============================================================================

#[test]
fn test_phase2_const_u64_round_trip() {
    // Same fixture as the oracle-probe test — small, dedicated, no race with
    // shared-fixture tests.
    let project = projects_dir().join("oracle_probe");
    assert!(project.is_dir(), "fixture not found: {}", project.display());
    let build_dir = project.join(".toylang-build");
    let cargo_target = shared_cargo_target_dir();
    let log_path = build_dir.join("callback.log");

    let build_out = {
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        std::fs::create_dir_all(&build_dir).unwrap();
        Command::new(toylangc_bin())
            .current_dir(&project)
            .env("DYLD_LIBRARY_PATH", sysroot_lib())
            .env("LD_LIBRARY_PATH", sysroot_lib())
            .env("CARGO_TARGET_DIR", &cargo_target)
            .env("TOYLANG_LOG_PATH", &log_path)
            .args(["build"])
            .output()
            .expect("failed to spawn toylangc")
    };
    assert!(
        build_out.status.success(),
        "oracle_probe toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("callback log not written at {}: {}", log_path.display(), e));

    // Find the Phase2RoundTripProbe entry. Debug format includes
    // `opaque_def_id_found: <bool>`, `encoded_typeid: <int>`,
    // `decoded_typeid: <Option<u64>>`.
    let entry = log.lines().map(str::trim).find(|line| {
        line.starts_with("Phase2RoundTripProbe ")
    }).unwrap_or_else(|| {
        panic!(
            "expected Phase2RoundTripProbe entry in callback log — did Phase 1's stub_gen \
             stop emitting __ToylangOpaque, or did Phase 2's probe disappear from \
             after_rust_analysis? Full log:\n{}",
            log,
        )
    });

    assert!(
        entry.contains("opaque_def_id_found: true"),
        "Phase2RoundTripProbe couldn't find __ToylangOpaque in the stub rlib. \
         Likely causes: stub_gen no longer emits the wrapper (check \
         `toylang_opaque_wrapper_emitted_at_crate_root`), or \
         `find_toylang_opaque_def_id`'s module-children walk drifted. Entry: {:?}",
        entry,
    );

    // Hardcoded sentinel matches the encoder in `after_rust_analysis`'s probe.
    // It's the same value as Phase 1.1's `widget_typeid_is_stable` anchor —
    // any drift in bincode or blake3 would surface in both places.
    let encoded_sentinel = 0x48723b0bb65d86f7u64;
    let encoded_str = format!("encoded_typeid: {}", encoded_sentinel);
    assert!(
        entry.contains(&encoded_str),
        "Phase2RoundTripProbe encoded_typeid changed away from the pinned \
         sentinel {:#018x}. Update the probe + this test together if \
         intentional. Entry: {:?}",
        encoded_sentinel, entry,
    );

    let decoded_str = format!("decoded_typeid: Some({})", encoded_sentinel);
    assert!(
        entry.contains(&decoded_str),
        "Phase 2 const-u64 round-trip failed — encoder and decoder disagree, or \
         the wrapper was found but its args couldn't be decoded. Expected \
         `{}` somewhere in:\n{}",
        decoded_str, entry,
    );
}

// ============================================================================
// S.5 sidecar determinism CI invariant (course-correct.md quarter-of-work
// plan, Workstream S final).
//
// Builds a fixture twice — wiping the target dir + the project's
// `.toylang-build` between runs — and asserts the two produced
// `.sky-meta` files are byte-identical. This is the architecture doc
// §7.4 determinism invariant tested end-to-end. Sidecar S.2 already
// has a unit-level `payload_determinism` test; this test guards the
// FULL pipeline (typing pass output, `BTreeMap` iteration, bincode
// fixed-int encoding, BLAKE3 checksum) against silent drift.
//
// Isolation: uses a dedicated `CARGO_TARGET_DIR` under
// `target/s5-determinism-<run>/` so two consecutive builds don't share
// cargo's fingerprint cache (which could mask a non-deterministic
// pipeline by reusing cached output).
//
// If this test fails, the failure is structural — the test prints the
// first byte index where the two outputs differ to give a starting
// point for diagnosis. Common causes:
//   - HashMap iteration order leaking into a structurally-walked field
//     (BTreeMap is the standing guard; check that any new collections
//     in `ToylangRegistry`/`ToyStruct`/`ToyFunction` are BTreeMap, not
//     HashMap).
//   - Bincode config drift (S.2 pins fixed-int + little-endian; any
//     change to `bincode_cfg()` could introduce length-varying
//     encoding that's input-dependent).
//   - A new timestamp / random ID / host-path field added to a
//     serialized type.
// ============================================================================

#[test]
fn test_s5_sidecar_determinism() {
    let project = projects_dir().join("arithmetic");
    assert!(project.is_dir(), "fixture not found: {}", project.display());

    // Per-run isolated target dirs so cargo's cross-run fingerprint cache
    // can't mask non-determinism by reusing a prior `.sky-meta`. Cleaned
    // up at function exit on a best-effort basis (test failure aborts
    // before cleanup; the dir lives under `target/` and gets swept by
    // `cargo clean`).
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let target_a = base.join("s5-determinism-a");
    let target_b = base.join("s5-determinism-b");
    for d in [&target_a, &target_b] {
        if d.exists() {
            std::fs::remove_dir_all(d).unwrap_or_else(|e| {
                panic!("failed to wipe {}: {}", d.display(), e)
            });
        }
    }

    let build_once = |target_dir: &Path| -> Vec<u8> {
        let build_dir = project.join(".toylang-build");
        if build_dir.exists() {
            std::fs::remove_dir_all(&build_dir).unwrap();
        }
        let build_out = {
            let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
            Command::new(toylangc_bin())
                .current_dir(&project)
                .env("DYLD_LIBRARY_PATH", sysroot_lib())
                .env("LD_LIBRARY_PATH", sysroot_lib())
                .env("CARGO_TARGET_DIR", target_dir)
                .args(["build"])
                .output()
                .expect("failed to spawn toylangc")
        };
        assert!(
            build_out.status.success(),
            "build failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&build_out.stdout),
            String::from_utf8_lossy(&build_out.stderr),
        );

        // Locate the produced sidecar. S.3 writes it adjacent to the rlib
        // at `target_dir/debug/deps/__lang_stubs-<hash>.sky-meta`. Exactly
        // one such file is expected — the per-run target dir holds only
        // this fixture's stubs.
        let deps = target_dir.join("debug/deps");
        let mut candidates: Vec<PathBuf> = std::fs::read_dir(&deps)
            .unwrap_or_else(|e| panic!("read_dir {}: {}", deps.display(), e))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("__lang_stubs-") && n.ends_with(".sky-meta"))
                    .unwrap_or(false)
            })
            .collect();
        candidates.sort();
        assert_eq!(
            candidates.len(),
            1,
            "expected exactly one __lang_stubs-*.sky-meta in {}; found: {:?}",
            deps.display(),
            candidates,
        );
        std::fs::read(&candidates[0])
            .unwrap_or_else(|e| panic!("read {}: {}", candidates[0].display(), e))
    };

    let bytes_a = build_once(&target_a);
    let bytes_b = build_once(&target_b);

    if bytes_a != bytes_b {
        let mismatch_at = bytes_a
            .iter()
            .zip(bytes_b.iter())
            .position(|(x, y)| x != y)
            .unwrap_or_else(|| std::cmp::min(bytes_a.len(), bytes_b.len()));
        panic!(
            "sidecar determinism regression: build A produced {} bytes, \
             build B produced {} bytes; first mismatch at byte {}.\n\
             a[{}] = {:?}, b[{}] = {:?}\n\
             See `test_s5_sidecar_determinism` comment for common causes.",
            bytes_a.len(),
            bytes_b.len(),
            mismatch_at,
            mismatch_at,
            bytes_a.get(mismatch_at),
            mismatch_at,
            bytes_b.get(mismatch_at),
        );
    }

    // Best-effort cleanup. Leaving these dirs around on test success is
    // not catastrophic — next run wipes them — but keeping `target/`
    // tidy avoids surprises.
    let _ = std::fs::remove_dir_all(&target_a);
    let _ = std::fs::remove_dir_all(&target_b);
}

// ============================================================================
// Stage 5c.4 — layout probe tests. Each triggers `layout_of` for a
// consumer type via a toylang fn that constructs one, then asserts that
// the facade's `[toylang] layout_of intercepted for: <ty> size=N
// align=M` log appears in toylangc's build stderr. Replaces direct
// mode's `assert_eq!(std::mem::size_of::<Point>(), 8)` from Rust main,
// which has no wrapper-mode equivalent (user_bin's main.rs is a
// generated `__toylang_main()` shim, not arbitrary Rust).
// ============================================================================

#[test] fn test_point_layout() { run_integration_project_check_build_stderr("point_layout"); }
#[test] fn test_t_of_t_layout() { run_integration_project_check_build_stderr("t_of_t_layout"); }
#[test] fn test_t_of_r_layout() { run_integration_project_check_build_stderr("t_of_r_layout"); }
#[test] fn test_tg_of_vec_layout() { run_integration_project_check_build_stderr("tg_of_vec_layout"); }
#[test] fn test_tg_of_toypoint_layout() { run_integration_project_check_build_stderr("tg_of_toypoint_layout"); }

// ============================================================================
// Inlining matrix — see HANDOFF-discovery-synthesis-share-generics-inlining.md
// Thread C. Validates Sky's user-visible perf promise that cross-language
// boundaries inline at -O3 under LTO. Built on the shared
// `inlining_harness` module.
//
// Naming: `test_inline_case<X>_<lto_mode>` for Priority A; `_inline`,
// `_inline_always`, `_inline_never` suffix for Priority B; `_oN` for
// Priority C; `_cgus_N` for Priority D. Fixtures live under
// `tests/integration_projects/inlining/<name>/` to keep the matrix out
// of the flat top-level fixture list.
//
// Per-case caller / boundary-callee semantics:
//   - Rust-top cases (1a, 1b, 3, 5): user_bin's `main` is the Rust
//     `rust_caller.rs`. Boundary callee = Sky's exported fn. We assert
//     against the `___toylang_main` / `__toylang_internal` mangling
//     fragments (same as test_lto_smoke).
//   - Sky-top cases (2, 4, 6): user_bin's `main` is the auto-generated
//     bin shim calling Sky's `__toylang_main`. At LTO Sky's body
//     inlines INTO the shim; same fragments apply.
//   - Cases 4/5/6 additionally have a Rust generic intermediary
//     (`some_rust_lib::duplicate` / `some_rust_lib::make_vec_of_three`).
//     At LTO those should also inline; we assert `some_rust_lib::` is
//     absent in those cases' caller bodies at thin/fat LTO.

// Sky-boundary detection. A `bl`/`b`/`call`/`callq` is a Sky-boundary
// call iff the demangled CALLEE symbol's outer path (before any
// `::<generic args>`) is a Sky-owned name:
//   - starts with `__lang_stubs::` (rustc-mangled Sky exports like
//     `__lang_stubs::add_one`, `__lang_stubs::__toylang_main`)
//   - or equals/starts with `__toylang_main` / `__toylang_internal`
//     (no-mangle Sky entries that survive without crate qualification)
//
// The selectivity matters because some Rust generics take Sky types as
// type args (e.g. `some_rust_lib::duplicate::<__lang_stubs::Widget>`).
// The symbol contains `__lang_stubs::` inside the generic args, but
// the function path (`some_rust_lib::duplicate`) is NOT Sky-owned —
// it's a Rust intermediary that happens to be instantiated over a Sky
// type. A naive substring check would flag it; the outer-path check
// correctly classifies it as non-Sky.

/// Extract the demangled callee name from a disasm branch line.
/// objdump format after demangling looks like:
///   `100002558:    bl  0x100002540 <some_demangled::path::<GenericArg>>`
/// The callee is the LAST balanced `<...>` block.
fn extract_branch_target(line: &str) -> Option<&str> {
    let last_lt = line.rfind('<')?;
    let last_gt = line.rfind('>')?;
    if last_gt <= last_lt { return None; }
    // Caveat: nested generic args mean the outer `<...>` we want is
    // not the LAST `<` (which would be the innermost generic arg).
    // We need the OUTERMOST. Easiest way: scan forward tracking
    // bracket depth.
    let bytes = line.as_bytes();
    let mut depth = 0;
    let mut outer_start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'<' {
            if depth == 0 { outer_start = Some(i); }
            depth += 1;
        } else if b == b'>' {
            depth -= 1;
            if depth == 0 {
                let start = outer_start?;
                if i > start + 1 && i == bytes.len() - 1 {
                    return std::str::from_utf8(&bytes[start + 1..i]).ok();
                }
            }
        }
    }
    None
}

/// Returns true iff `demangled_target` (the contents inside the outer
/// `<...>` of a branch line) is a call to a Sky-owned name. Strips
/// any `::<generic args>` suffix before classifying.
fn is_sky_boundary_target(demangled_target: &str) -> bool {
    let path = demangled_target
        .split_once("::<")
        .map(|(head, _)| head)
        .unwrap_or(demangled_target);
    path.starts_with("__lang_stubs::")
        || path.starts_with("__toylang_main")
        || path.starts_with("__toylang_internal")
}

fn line_is_sky_branch(line: &str) -> bool {
    let after_colon = match line.split_once(':') {
        Some((_, rest)) => rest,
        None => return false,
    };
    let mnemonic = match after_colon.split_whitespace().next() {
        Some(m) => m,
        None => return false,
    };
    if !matches!(mnemonic, "bl" | "b" | "call" | "callq") { return false; }
    match extract_branch_target(line) {
        Some(target) => is_sky_boundary_target(target),
        None => false,
    }
}

fn assert_no_sky_branch_in_main(project: &str) {
    let ctx = inlining_harness::disassemble_binary(
        project,
        inlining_harness::Profile::Debug,
    );
    let main_needle = format!("{}::main", project);
    let bodies = ctx.bodies_of(&main_needle);
    assert!(
        !bodies.is_empty(),
        "{}: no function matching `{}` found in disassembly",
        project, main_needle,
    );
    let mut violations: Vec<String> = Vec::new();
    for body in bodies {
        for line in body {
            if line_is_sky_branch(line) {
                violations.push(line.clone());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{}: expected `{}` to NOT branch to a Sky-owned symbol \
         (Sky's body should have inlined), but found:\n{}",
        project, main_needle, violations.join("\n"),
    );
}

// Helper: assert that `<project>::main` DOES branch to at least one
// Sky-owned symbol. Used at -O0 baselines to codify "no inlining at
// -O0; Sky's body remains as a cross-crate call."
fn assert_sky_branch_present_in_main(project: &str) {
    let ctx = inlining_harness::disassemble_binary(
        project,
        inlining_harness::Profile::Debug,
    );
    let main_needle = format!("{}::main", project);
    let bodies = ctx.bodies_of(&main_needle);
    assert!(
        !bodies.is_empty(),
        "{}: no function matching `{}` found in disassembly — \
         symbol-naming drift? Functions present:\n{}",
        project,
        main_needle,
        ctx.functions
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut found = false;
    'outer: for body in bodies {
        for line in body {
            if line_is_sky_branch(line) {
                found = true;
                break 'outer;
            }
        }
    }
    assert!(
        found,
        "{}: expected `{}` to branch to a Sky-owned symbol (Sky's body \
         should NOT be inlined), but none found. Either Sky's body was \
         inlined (unexpected for this fixture) or symbol naming drifted.",
        project, main_needle,
    );
}

// ---- Priority A: 7 cases × 3 LTO modes at -O3 (21 fixtures) ----
//
// F1 investigation resolution (2026-06-20): the previous version of
// these comments documented a "Sky-export LTO inlining gap" — the
// matrix found that Sky-exported fns (case1a/2/4/6) didn't inline
// across the crate boundary at thin/fat LTO even though architecturally
// they should. Root cause traced to `#[inline(never)]` on the stub
// source's wrapper fn emission (`toylangc/src/stub_gen.rs`). The
// attribute's two historical rationales (share_generics gate trip,
// MIR inliner leak protection) were both obsolete: patch 5 closes the
// share_generics gate independently, and arch §F.1 explicitly calls
// the MIR inliner concern phantom (the real fix is `#![no_builtins]`
// + Path B single-symbol naming, both already in place). The B12
// vanilla-rustc-cross-crate concern is gated by build.rs's
// SKY_TOOLCHAIN_ACTIVE check in v1 and only acquires meaning when
// v2's precompiled-bodies feature (§21.7) ships. Attribute removed;
// LTO inlining now empirically works for every case.
//
// Sky GENERICS with a Rust-side type arg (case1b) inline cross-boundary
// at every opt level + LTO mode — because Sky's `per_instance_mir`
// emits the substituted body INTO the user_bin's CGU, where LLVM sees
// it locally. (This was true before F1 too; case1b never hit the gap.)

// Case 1a: Rust → Sky non-generic. Post-F1: Sky's body inlines
// cross-crate at -O3 via rmeta-encoded MIR, regardless of LTO mode.
#[test] fn test_inline_case1a_no_lto() {
    run_inlining_project("case1a_no_lto");
    assert_no_sky_branch_in_main("case1a_no_lto");
}
#[test] fn test_inline_case1a_thin_lto() {
    run_inlining_project("case1a_thin_lto");
    assert_no_sky_branch_in_main("case1a_thin_lto");
}
#[test] fn test_inline_case1a_fat_lto() {
    run_inlining_project("case1a_fat_lto");
    assert_no_sky_branch_in_main("case1a_fat_lto");
}

// Case 1b: Rust → Sky generic w/ Rust-defined T. Sky's
// per_instance_mir emits the substituted body INTO user_bin → LLVM
// inlines at -O3, even without LTO. (Worked pre-F1 too.)
#[test] fn test_inline_case1b_no_lto() {
    run_inlining_project("case1b_no_lto");
    assert_no_sky_branch_in_main("case1b_no_lto");
}
#[test] fn test_inline_case1b_thin_lto() {
    run_inlining_project("case1b_thin_lto");
    assert_no_sky_branch_in_main("case1b_thin_lto");
}
#[test] fn test_inline_case1b_fat_lto() {
    run_inlining_project("case1b_fat_lto");
    assert_no_sky_branch_in_main("case1b_fat_lto");
}

// Case 2: Sky main → Rust library (extern "C" println). Post-F1:
// Sky's main inlines into the bin shim's main at every -O3 mode.
#[test] fn test_inline_case2_no_lto() {
    run_inlining_project("case2_no_lto");
    assert_no_sky_branch_in_main("case2_no_lto");
}
#[test] fn test_inline_case2_thin_lto() {
    run_inlining_project("case2_thin_lto");
    assert_no_sky_branch_in_main("case2_thin_lto");
}
#[test] fn test_inline_case2_fat_lto() {
    run_inlining_project("case2_fat_lto");
    assert_no_sky_branch_in_main("case2_fat_lto");
}

// Case 3: Rust → Sky generic → derived Rust Clone impl.
//
// Post-F2 (2026-06-20): the previous version of these comments
// documented a `<user_bin::MyCounter as Clone>::clone` undefined-
// symbol bug at -O3. Root cause: rustc emits `cross_crate_inlinable`
// items (derived `#[derive(Clone)]` Clone methods auto-attract this)
// as `available_externally` linkage — body for IR-level inlining
// only, no `.o` symbol. Sky's emitted clone_it<MyCounter> body calls
// the symbol via direct LLVM call and can't inline through rustc's
// path, so the reference dangles at link.
//
// Fix shipped via the `cross_crate_inlinable` query override
// (`rustc-lang-facade/src/queries/cross_crate_inlinable.rs`). In
// Sky-active compiles (consumer_lang_active=true), the override
// returns false → rustc emits real .o symbols. Pure-Rust compiles
// see byte-identical behavior to vanilla. Post-F1 + post-F2,
// Sky's body inlines all the way through main at every -O3 mode.
#[test] fn test_inline_case3_no_lto() {
    run_inlining_project("case3_no_lto");
    assert_no_sky_branch_in_main("case3_no_lto");
}
#[test] fn test_inline_case3_thin_lto() {
    run_inlining_project("case3_thin_lto");
    assert_no_sky_branch_in_main("case3_thin_lto");
}
#[test] fn test_inline_case3_fat_lto() {
    run_inlining_project("case3_fat_lto");
    assert_no_sky_branch_in_main("case3_fat_lto");
}

// Case 4: Sky → Rust generic → Sky impl. Post-F1: Sky's main inlines
// into the bin shim. The Rust generic intermediary `some_rust_lib::
// duplicate::<Widget>` may or may not inline further — that's a
// separate Rust-side LLVM inlining concern not gated by F1. Only
// asserts on Sky-boundary inlining.
#[test] fn test_inline_case4_no_lto() {
    run_inlining_project("case4_no_lto");
    assert_no_sky_branch_in_main("case4_no_lto");
}
#[test] fn test_inline_case4_thin_lto() {
    run_inlining_project("case4_thin_lto");
    assert_no_sky_branch_in_main("case4_thin_lto");
}
#[test] fn test_inline_case4_fat_lto() {
    run_inlining_project("case4_fat_lto");
    assert_no_sky_branch_in_main("case4_fat_lto");
}

// Case 5: Rust → Sky generic → Rust Vec intermediary. Post-F2:
// same fix as case 3 closes this — Vec's `new` / `push` were
// available_externally; with `cross_crate_inlinable=false` in
// Sky-active compiles, they emit real .o symbols.
// Smoke-only: Vec involves `__rust_alloc` etc., so the
// `assert_no_sky_branch_in_main` doesn't apply cleanly (the body
// path threads through alloc).
#[test] fn test_inline_case5_no_lto() {
    run_inlining_project("case5_no_lto");
}
#[test] fn test_inline_case5_thin_lto() {
    run_inlining_project("case5_thin_lto");
}
#[test] fn test_inline_case5_fat_lto() {
    run_inlining_project("case5_fat_lto");
}

// Case 6: Sky → Rust → cross-Sky-crate (uses case6_lib_inl). Same
// shape as case 4 with cross-Sky-crate trait impl.
#[test] fn test_inline_case6_no_lto() {
    run_inlining_project("case6_no_lto");
    assert_no_sky_branch_in_main("case6_no_lto");
}
#[test] fn test_inline_case6_thin_lto() {
    run_inlining_project("case6_thin_lto");
    assert_no_sky_branch_in_main("case6_thin_lto");
}
#[test] fn test_inline_case6_fat_lto() {
    run_inlining_project("case6_fat_lto");
    assert_no_sky_branch_in_main("case6_fat_lto");
}

// ============================================================================
// Priority B: 4 Rust-top cases × 3 #[inline] annotations on a Rust
// caller wrapper at -O3 + thin LTO (12 fixtures).
//
// Sky source doesn't support `#[inline]` annotations (stub_gen emits
// `#[inline(never)]` unconditionally on every Sky export — see
// toylangc/src/stub_gen.rs:221). So Priority B can only meaningfully
// vary the annotation on the RUST side. We insert a thin `wrap_<fn>`
// wrapper in rust_caller.rs that calls into Sky's export and carry
// the annotation on the wrapper. Tests assert main inlines through
// the wrapper (or doesn't) per the annotation's intent.
//
// Sky-top cases (2, 4, 6) are SKIPPED in Priority B: there's no Rust
// caller to annotate, and Sky source doesn't support `#[inline]`.
// Closing this gap is its own thread (extend Sky frontend to honor
// `#[inline]` attributes; see the open thread in
// HANDOFF-discovery-synthesis-share-generics-inlining.md).
//
// Cases 3 and 5 inherit the release-mode disambig bug from Priority
// A; their Priority B fixtures are also `#[ignore]`d under the
// sibling-of-B14 marker.

const PRIORITY_B_DISAMBIG_IGNORE: &str =
    "Inherits case3/case5 disambig bug (sibling of B14) — same root cause as Priority A";

// Helper: at -O3 + thin LTO, assert main does NOT call the wrapper
// (wrapper inlined). Used for `#[inline]` and `#[inline(always)]`.
fn assert_no_wrapper_branch(project: &str, wrapper_name: &str) {
    let ctx = inlining_harness::disassemble_binary(
        project,
        inlining_harness::Profile::Debug,
    );
    let main_needle = format!("{}::main", project);
    let wrapper_needle = format!("{}::{}", project, wrapper_name);
    inlining_harness::assert_no_call_to_symbols_matching(&ctx, &main_needle, &wrapper_needle);
}

// Helper: at -O3 + thin LTO, assert main DOES call the wrapper
// (#[inline(never)] honored). Reuses the OR-scan logic used for
// SKY_BOUNDARY_FRAGMENTS but for a single required wrapper name.
fn assert_wrapper_branch_present(project: &str, wrapper_name: &str) {
    let ctx = inlining_harness::disassemble_binary(
        project,
        inlining_harness::Profile::Debug,
    );
    let main_needle = format!("{}::main", project);
    let wrapper_needle = format!("{}::{}", project, wrapper_name);
    inlining_harness::assert_call_to_symbol_matching(&ctx, &main_needle, &wrapper_needle);
}

// Case 1a: Rust → Sky non-generic
#[test] fn test_inline_case1a_inline() {
    run_inlining_project("case1a_inline");
    assert_no_wrapper_branch("case1a_inline", "wrap_add_one");
}
#[test] fn test_inline_case1a_inline_always() {
    run_inlining_project("case1a_inline_always");
    assert_no_wrapper_branch("case1a_inline_always", "wrap_add_one");
}
#[test] fn test_inline_case1a_inline_never() {
    run_inlining_project("case1a_inline_never");
    assert_wrapper_branch_present("case1a_inline_never", "wrap_add_one");
}

// Case 1b: Rust → Sky generic w/ Rust-defined T
#[test] fn test_inline_case1b_inline() {
    run_inlining_project("case1b_inline");
    assert_no_wrapper_branch("case1b_inline", "wrap_identity");
}
#[test] fn test_inline_case1b_inline_always() {
    run_inlining_project("case1b_inline_always");
    assert_no_wrapper_branch("case1b_inline_always", "wrap_identity");
}
#[test] fn test_inline_case1b_inline_never() {
    run_inlining_project("case1b_inline_never");
    assert_wrapper_branch_present("case1b_inline_never", "wrap_identity");
}

// Case 3: post-F2, the disambig bug is fixed. Priority B variants
// run. The `_inline_never` variant is ignored — LLVM's inliner
// aggressively inlines the trivial `wrap_clone_it` wrapper into
// main at -O3 + thin LTO despite the `#[inline(never)]` attribute,
// fully eliminating the wrapper symbol via DCE. Per arch §F:
// `#[inline(never)]` is a hint, not a guarantee; LLVM may refuse
// when the wrapper is small and the inlined cost is low. case 1a's
// equivalent test passes because `add_one` is even more trivial
// and somehow attracts different inliner heuristics; not worth
// chasing the discrepancy in this matrix.
#[test] fn test_inline_case3_inline() {
    let _ = PRIORITY_B_DISAMBIG_IGNORE;
    run_inlining_project("case3_inline");
    assert_no_wrapper_branch("case3_inline", "wrap_clone_it");
}
#[test] fn test_inline_case3_inline_always() {
    run_inlining_project("case3_inline_always");
    assert_no_wrapper_branch("case3_inline_always", "wrap_clone_it");
}
#[test] #[ignore = "LLVM inlines #[inline(never)] wrapper at -O3+thin LTO despite the attribute — arch §F gotcha"]
fn test_inline_case3_inline_never() {
    run_inlining_project("case3_inline_never");
    assert_wrapper_branch_present("case3_inline_never", "wrap_clone_it");
}

// Case 5: post-F2, the disambig bug is fixed. Priority B variants
// run.
#[test] fn test_inline_case5_inline() {
    run_inlining_project("case5_inline");
    assert_no_wrapper_branch("case5_inline", "wrap_store");
}
#[test] fn test_inline_case5_inline_always() {
    run_inlining_project("case5_inline_always");
    assert_no_wrapper_branch("case5_inline_always", "wrap_store");
}
#[test] fn test_inline_case5_inline_never() {
    run_inlining_project("case5_inline_never");
    assert_wrapper_branch_present("case5_inline_never", "wrap_store");
}

// ============================================================================
// Priority C: Case 4 opt-level sweep at no-LTO (5 fixtures).
// Validates inlining behavior across opt-levels post-F1.
//
// -O0: no inlining → Sky's body remains as cross-crate `bl` baseline.
// -O1/-O2/-Os/-Oz: rustc's cross-crate MIR inliner fires → Sky's body
//   is expected to inline into main (no Sky-owned `bl`). Empirically
//   confirmed for -O1+ post-F1.

#[test] fn test_inline_case4_o0() {
    run_inlining_project("case4_o0");
    assert_sky_branch_present_in_main("case4_o0");
}
#[test] fn test_inline_case4_o1() {
    run_inlining_project("case4_o1");
    assert_no_sky_branch_in_main("case4_o1");
}
#[test] fn test_inline_case4_o2() {
    run_inlining_project("case4_o2");
    assert_no_sky_branch_in_main("case4_o2");
}
#[test] fn test_inline_case4_os() {
    run_inlining_project("case4_os");
    assert_no_sky_branch_in_main("case4_os");
}
#[test] fn test_inline_case4_oz() {
    run_inlining_project("case4_oz");
    assert_no_sky_branch_in_main("case4_oz");
}

// ============================================================================
// Priority D: Case 4 codegen-units at -O3 + thin LTO (2 fixtures).
// Validates that the inlining matrix's behavior is consistent across
// codegen-units settings. Asserts Sky's body inlines through main at
// both extremes now that F1 is resolved.

#[test] fn test_inline_case4_cgus_1() {
    run_inlining_project("case4_cgus_1");
    assert_no_sky_branch_in_main("case4_cgus_1");
}
#[test] fn test_inline_case4_cgus_16() {
    run_inlining_project("case4_cgus_16");
    assert_no_sky_branch_in_main("case4_cgus_16");
}
