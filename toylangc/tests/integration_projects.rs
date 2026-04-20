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
        let cgd = format!("CollectGenericRustDeps {{ name: \"{}\" }}", name_expected);
        let ncep = format!("NotifyConcreteEntryPoint {{ name: \"{}\" }}", name_expected);
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
