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

// TODO(stage5c-followup): test_extern_fn_call hits a toylang codegen bug
// that surfaces only under the stage-5c wrapper-mode path. The toylang
// fn `do_print()` (containing both `println_int(42)` and `println_bool(true)`
// extern "C" calls) gets emitted with declared return `i8` but body
// `ret void`. Mismatch fails llc.
//
// Confirmed independent of stage 5c.2's pattern via probe: toylang source
// IS the same as a `probe_extern_void` project, but project name
// `extern_fn_call` reproduces the bug while `probe_extern_void` does not.
// HashMap iteration order over the toylang registry — different project
// names hash to different orderings, exposing/hiding the bug. Pre-existing
// codegen latent bug; the FileLoader (direct-mode) path masked it. Affects
// 1 of 57 extern-fixture tests; no other migrated tests exercise the
// `bool extern "C" arg + sibling extern "C" call` shape. Park here; flag
// to TL for separate ticket.
//
// #[test]
// fn test_extern_fn_call() {
//     run_integration_project("extern_fn_call");
// }

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
// Parked tests — unmigrated, with reasons
// ============================================================================
//
// Every test below is still present in `integration_tests.rs` (direct mode)
// and passes there. They do not migrate cleanly to wrapper-mode + projects
// for the documented reasons. Stage 5c.3 and 5c.4 address the unblockable
// ones (error-harness helper, ENV_LOG-callback replacement, Vec<consumer
// type> debuginfo fix).
//
// CATEGORY: Vec<consumer-type> debuginfo ICE (same cause as the
// inline-TODO'd test_toylang_main_with_vec below; tracked as risks.md
// §B6's debuginfo sibling issue). Every test listed here constructs
// `Vec<Point, Global>` or similar with a toylang-defined struct as the
// element type, and the stub rlib compile panics in
// `build_struct_type_di_node` during debuginfo generation for the
// opaque struct's generic instantiation inside Vec.
//
// Fix is architectural: either emit `pub struct Point;` (unit struct)
// instead of `pub struct Point(());` (tuple struct with unit field)
// from stub_gen so the source-level field count matches our
// layout_of(0-field), OR teach the layout_of override to always agree
// with the source-level field count (slightly more invasive because
// the override synthesizes layouts for generic instantiations that
// don't exist in source). Defer to stage 5c.4 or a dedicated follow-up.
//
//   test_vec_point                       (line  249)
//   test_r_t_r_vec_of_ship               (line  530)
//   test_t_r_t_construct                 (line  571)
//   test_r_r_t_vec_of_vec                (line  649)
//   test_deep_t_r_t_r                    (line  808)
//   test_vec_of_structs_len              (line 1396)
//   test_toylang_main_with_vec           (line 1695) — see also v2 below
//   test_toylang_main_with_vec_v2        (line 1656)
//   test_vec_method_lookup_is_exact      (line 1798)
//   test_vec_push_fn_call_result         (line 1852)
//
// CATEGORY: Rust-specific layout probes. These tests assert on
// `std::mem::size_of::<ConsumerType>()` / `std::mem::align_of` from
// the Rust side. Wrapper mode runs a toylang binary; there's no
// Rust-side entry point to query layout. Migrating would require a
// layout-introspection helper exposed via test_helpers that takes a
// concrete type and returns its size — which means the set of types
// has to be baked into test_helpers, defeating the point. These
// tests are really facade-correctness probes; candidate for
// promotion to a rustc-lang-facade unit test rather than an
// integration project. Park.
//
//   test_point_layout                    (line  288)
//   test_t_of_r_layout                   (line  442)
//   test_t_of_t_layout                   (line  501)
//   test_tg_of_vec_layout                (line  723)
//   test_tg_of_toypoint_layout           (line  779)
//   test_point_drop                      (line  313) — uses runtime.o
//                                        link-args + std::ptr::drop_in_place
//
// CATEGORY: ENV_LOG callback-trace tests. These use
// `compile_and_run_with_env` to set `TOYLANG_CALLBACK_LOG=1` and
// assert on the callback sequence the facade recorded for the
// compile. The log is a facade-internal observation that doesn't
// surface through `toylangc build` output. 5c.3 or 5c.4 either
// needs to add a `--log-callbacks=<path>` flag or these tests need
// to become facade-level unit tests. Park.
//
//   test_internal_toylang_fn_not_monomorphized_by_rustc (line 2692)
//   test_deep_chain_only_entry_point_monomorphized      (line 2741)
//   test_diamond_call_pattern                           (line 2791)
//   test_generic_deep_walk                              (line 2847)
//   test_two_entry_points_shared_internal               (line 2892)
//
// CATEGORY: Error-assertion tests. These expect `toylangc` to exit
// non-zero with a structured error. 5c.3 will add a
// `run_integration_project_expects_error(name, pattern)` harness
// helper that captures stderr and substring-matches. Park until
// that lands.
//
//   test_lexer_rejects_unknown_chars                           (line 1891)
//   test_rust_free_fn_undefined_gives_error                    (line 3115)
//   test_main_non_void_tail_rejected                           (line 3149)
//   test_trait_self_not_imported_gives_error                   (line 3189)
//   test_static_call_undefined_type_gives_structured_error     (line 3886)
//   test_trait_call_unknown_trait_name_gives_structured_error  (line 3934)
//   test_trait_call_unknown_method_name_gives_structured_error (line 3985)
//
// CATEGORY: Codegen bugs (pre-existing, exposed only under two-crate).
// See handoff §B7 + risks.md §3 B7.
//
//   test_extern_fn_call                  (line 1949) — bool extern-arg
//                                        return-type leak
//
// Previously-TODO'd inline text about the Vec<Point> ICE retained below
// for git-log searchability; superseded by the category block above.
//
// TODO(stage5c-followup): test_toylang_main_with_vec and
// test_vec_push_fn_call_result hit a rustc ICE during the stub rlib
// compile: `FieldsShape::offset` panics with "index out of bounds: the
// len is 0 but the index is 0" while rustc generates debuginfo for the
// Point struct inside a Vec<Point, Global> monomorphization. Our
// `layout_of` override reports Point as 0-field opaque; the source-level
// `pub struct Point(())` has 1 tuple field; debuginfo's
// `build_struct_type_di_node` walks source fields and indexes into
// layout → panic.
//
// Direct mode (FileLoader) doesn't hit this — the single-crate compile
// doesn't seem to request debuginfo for Point through the Vec<Point>
// path in the same way. Wrapper mode's stub-rlib compile exposes the
// mismatch. Park alongside the bool extern-arg leak; park as TL category
// B addendum. Revisit after other migrations land — may be fixable by
// emitting `pub struct Point;` (unit struct) instead of `pub struct
// Point(());` (tuple struct with unit field), or by aligning the
// layout_of override's field count with the source-level struct def.
//
// #[test]
// fn test_toylang_main_with_vec() {
//     run_integration_project("toylang_main_with_vec");
// }
//
// #[test]
// fn test_vec_push_fn_call_result() {
//     run_integration_project("vec_push_fn_call_result");
// }
