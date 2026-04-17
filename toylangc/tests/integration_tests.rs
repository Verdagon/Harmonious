//! Integration tests for toylangc.
//!
//! Each test spawns toylangc as a subprocess, compiling inline .toylang + .rs
//! source to a temp binary, runs it, and checks the output.
//!
//! Tests marked #[ignore] are expected to fail — they represent features not yet
//! implemented (TDD north star). Run them with: cargo test -p toylangc -- --ignored

use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Test infrastructure
// ============================================================================

/// Get the rustc sysroot lib path for DYLD_LIBRARY_PATH.
fn sysroot_lib() -> String {
    let out = Command::new("rustup")
        .args(["run", "nightly-2025-01-15", "rustc", "--print=sysroot"])
        .output()
        .expect("failed to run rustup");
    let sysroot = String::from_utf8(out.stdout).unwrap();
    format!("{}/lib", sysroot.trim())
}

/// Path to the toylangc binary built by cargo.
fn toylangc_bin() -> PathBuf {
    // CARGO_BIN_EXE_toylangc is set by cargo for integration tests
    PathBuf::from(env!("CARGO_BIN_EXE_toylangc"))
}

/// Returns true if the callback log shows rustc asked the consumer about
/// `name` via either of the two per-entry-point callbacks. The split between
/// `CollectGenericRustDeps` and `NotifyConcreteEntryPoint` is a
/// facade-internal detail; tests care about "did rustc callback for X" and
/// either variant counts.
fn log_mentions_callback_for(log: &str, name: &str) -> bool {
    log.contains(&format!("CollectGenericRustDeps {{ name: \"{}\" }}", name))
        || log.contains(&format!("NotifyConcreteEntryPoint {{ name: \"{}\" }}", name))
}

/// Compile and run a toylang + rust test. Returns stdout.
/// Panics if compilation or execution fails.
fn run_toylang_test(toylang_src: &str, rust_src: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, toylang_src).unwrap();
    std::fs::write(&rust_path, rust_src).unwrap();

    compile_and_run(
        &toylang_path,
        &rust_path,
        &bin_path,
        &[],
    )
}

/// Same as run_toylang_test but with extra link args (e.g. for runtime.o).
fn run_toylang_test_with_link_args(toylang_src: &str, rust_src: &str, link_args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, toylang_src).unwrap();
    std::fs::write(&rust_path, rust_src).unwrap();

    compile_and_run(
        &toylang_path,
        &rust_path,
        &bin_path,
        link_args,
    )
}

fn compile_and_run(
    toylang_path: &Path,
    rust_path: &Path,
    bin_path: &Path,
    extra_link_args: &[&str],
) -> String {
    let mut args = vec![
        "--edition".to_string(), "2021".to_string(),
        "--toylang-input".to_string(), toylang_path.to_str().unwrap().to_string(),
        rust_path.to_str().unwrap().to_string(),
        "-o".to_string(), bin_path.to_str().unwrap().to_string(),
    ];
    for arg in extra_link_args {
        args.push("-C".to_string());
        args.push(format!("link-arg={}", arg));
    }

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&args)
        .output()
        .expect("failed to run toylangc");

    if !compile.status.success() {
        panic!(
            "Compilation failed (exit {}):\nstdout: {}\nstderr: {}",
            compile.status,
            String::from_utf8_lossy(&compile.stdout),
            String::from_utf8_lossy(&compile.stderr),
        );
    }

    let run = Command::new(bin_path)
        .output()
        .expect("failed to run test binary");

    if !run.status.success() {
        panic!(
            "Test binary failed (exit {}):\nstdout: {}\nstderr: {}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr),
        );
    }

    String::from_utf8(run.stdout).unwrap()
}

/// Compile and run, returning (stdout, compiler_stderr).
fn compile_and_run_with_env(
    toylang_path: &Path,
    rust_path: &Path,
    bin_path: &Path,
    env: &[(&str, &str)],
) -> (String, String) {
    let args = vec![
        "--edition".to_string(), "2021".to_string(),
        "--toylang-input".to_string(), toylang_path.to_str().unwrap().to_string(),
        rust_path.to_str().unwrap().to_string(),
        "-o".to_string(), bin_path.to_str().unwrap().to_string(),
    ];

    let mut cmd = Command::new(toylangc_bin());
    cmd.env("DYLD_LIBRARY_PATH", sysroot_lib()).args(&args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let compile = cmd.output().expect("failed to run toylangc");

    let compiler_stderr = String::from_utf8_lossy(&compile.stderr).to_string();

    if !compile.status.success() {
        panic!(
            "Compilation failed (exit {}):\nstdout: {}\nstderr: {}",
            compile.status,
            String::from_utf8_lossy(&compile.stdout),
            compiler_stderr,
        );
    }

    let run = Command::new(bin_path)
        .output()
        .expect("failed to run test binary");

    if !run.status.success() {
        panic!(
            "Test binary failed (exit {}):\nstdout: {}\nstderr: {}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr),
        );
    }

    (String::from_utf8(run.stdout).unwrap(), compiler_stderr)
}

/// Path to the runtime.o for drop tests.
fn runtime_obj() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{}/tests/runtime.o", manifest_dir)
}

// ============================================================================
// Phase 1: Migrated existing tests (all should PASS)
// ============================================================================

#[test]
fn test_counter_construct() {
    let output = run_toylang_test(
        r#"
struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn wrap_value(x: i32) -> Counter {
    Counter { value: x }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let c = make_counter();
    println!("Counter value: {}", c.value());
    assert_eq!(*c.value(), 42);

    let c2 = wrap_value(99);
    println!("Wrapped value: {}", c2.value());
    assert_eq!(*c2.value(), 99);
}
        "#,
    );
    assert!(output.contains("Counter value: 42"));
    assert!(output.contains("Wrapped value: 99"));
}

#[test]
fn test_pair_construct() {
    let output = run_toylang_test(
        r#"
struct Pair<A, B> {
    first: A,
    second: B,
}

fn make_pair() -> Pair<i32, i32> {
    Pair<i32, i32> { first: 10, second: 20 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_pair();
    println!("Pair: {} {}", p.first(), p.second());
    assert_eq!(*p.first(), 10);
    assert_eq!(*p.second(), 20);
}
        "#,
    );
    assert!(output.contains("Pair: 10 20"));
}

#[test]
fn test_vec_point() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct Point {
    x: i32,
    y: i32,
}

fn make_vec() -> Vec<Point, Global> {
    let v = Vec::new<Point, Global>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v
}

fn vec_len(v: &Vec<Point, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = make_vec();
    let len = vec_len(&v);
    println!("Vec length: {}", len);
    assert_eq!(len, 2);
}
        "#,
    );
    assert!(output.contains("Vec length: 2"));
}

#[test]
fn test_point_layout() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    println!("size  = {}", std::mem::size_of::<Point>());
    println!("align = {}", std::mem::align_of::<Point>());
    assert_eq!(std::mem::size_of::<Point>(), 8);
    assert_eq!(std::mem::align_of::<Point>(), 4);
}
        "#,
    );
    assert!(output.contains("size  = 8"));
    assert!(output.contains("align = 4"));
}

#[test]
fn test_point_drop() {
    let output = run_toylang_test_with_link_args(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn make_point() -> Point {
    Point { x: 1, y: 2 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let mut p = make_point();
    unsafe {
        std::ptr::drop_in_place(&mut p as *mut Point);
        std::mem::forget(p);
    }
    println!("done");
}
        "#,
        &[&runtime_obj()],
    );
    assert!(output.contains("done"));
}

// ============================================================================
// Phase 2: New tests — north star for future features
// Most are #[ignore] because toylang doesn't support the required features yet.
// ============================================================================

// --- Group 2: Generic toylang with different type args ---

#[test]
fn test_tg_i32_i64() {
    let output = run_toylang_test(
        r#"
struct Pair<A, B> {
    first: A,
    second: B,
}

fn make_pair() -> Pair<i32, i64> {
    Pair<i32, i64> { first: 10, second: 2000000000000 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_pair();
    println!("first: {} second: {}", p.first(), p.second());
    assert_eq!(*p.first(), 10);
    assert_eq!(*p.second(), 2000000000000i64);
}
        "#,
    );
    assert!(output.contains("first: 10 second: 2000000000000"));
}

#[test]
fn test_tg_bool_i32() {
    let output = run_toylang_test(
        r#"
struct Pair<A, B> {
    first: A,
    second: B,
}

fn make_pair() -> Pair<bool, i32> {
    Pair<bool, i32> { first: true, second: 99 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_pair();
    println!("first: {} second: {}", p.first(), p.second());
    assert_eq!(*p.first(), true);
    assert_eq!(*p.second(), 99);
}
        "#,
    );
    assert!(output.contains("first: true second: 99"));
}

// --- Group 3: Toylang containing rust type (T(R)) ---

#[test]
fn test_t_of_r_vec_field() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyShip {
    wings: Vec<i32, Global>,
}

fn make_ship() -> ToyShip {
    let v = Vec::new<i32, Global>();
    v.push(1);
    v.push(2);
    ToyShip { wings: v }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let ship = make_ship();
    println!("wings len: {}", ship.wings().len());
    assert_eq!(ship.wings().len(), 2);
}
        "#,
    );
    assert!(output.contains("wings len: 2"));
}

#[test]
fn test_t_of_r_layout() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyShip {
    wings: Vec<i32, Global>,
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    // Vec is 3 pointers = 24 bytes on 64-bit
    let size = std::mem::size_of::<ToyShip>();
    println!("ToyShip size: {}", size);
    assert_eq!(size, 24);
}
        "#,
    );
    assert!(output.contains("ToyShip size: 24"));
}

// --- Group 4: Toylang containing toylang (T(T)) ---

#[test]
fn test_t_of_t_construct() {
    let output = run_toylang_test(
        r#"
struct ToyInner {
    x: i32,
}

struct ToyOuter {
    inner: ToyInner,
}

fn make_outer() -> ToyOuter {
    ToyOuter { inner: ToyInner { x: 42 } }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let o = make_outer();
    println!("inner.x: {}", o.inner().x());
    assert_eq!(*o.inner().x(), 42);
}
        "#,
    );
    assert!(output.contains("inner.x: 42"), "output was: {}", output);
}

#[test]
fn test_t_of_t_layout() {
    let output = run_toylang_test(
        r#"
struct ToyInner {
    x: i32,
}

struct ToyOuter {
    inner: ToyInner,
    extra: i32,
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let size = std::mem::size_of::<ToyOuter>();
    println!("ToyOuter size: {}", size);
    assert_eq!(size, 8); // ToyInner(4) + i32(4) = 8
}
        "#,
    );
    assert!(output.contains("ToyOuter size: 8"));
}

// --- Group 5: Rust -> toylang -> rust (R(T(R))) ---

#[test]
fn test_r_t_r_vec_of_ship() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyShip {
    wings: Vec<i32, Global>,
}

fn make_fleet() -> Vec<ToyShip, Global> {
    let fleet = Vec::new<ToyShip, Global>();
    let v = Vec::new<i32, Global>();
    v.push(10);
    fleet.push(ToyShip { wings: v });
    fleet
}

fn fleet_len(f: &Vec<ToyShip, Global>) -> usize {
    f.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let fleet = make_fleet();
    let len = fleet_len(&fleet);
    println!("fleet len: {}", len);
    assert_eq!(len, 1);
}
        "#,
    );
    assert!(output.contains("fleet len: 1"));
}

// --- Group 6: Toylang -> rust -> toylang (T(R(T))) ---

#[test]
fn test_t_r_t_construct() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyFleet {
    ships: Vec<ToyPoint, Global>,
}

fn make_fleet() -> ToyFleet {
    let v = Vec::new<ToyPoint, Global>();
    v.push(ToyPoint { x: 1, y: 2 });
    v.push(ToyPoint { x: 3, y: 4 });
    ToyFleet { ships: v }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let fleet = make_fleet();
    println!("ships len: {}", fleet.ships().len());
    assert_eq!(fleet.ships().len(), 2);
}
        "#,
    );
    assert!(output.contains("ships len: 2"));
}

// --- Group 7: Toylang -> toylang -> rust (T(T(R))) ---

#[test]
fn test_t_t_r_construct() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyEngine {
    parts: Vec<i32, Global>,
}

struct ToyShip {
    engine: ToyEngine,
}

fn make_ship() -> ToyShip {
    let v = Vec::new<i32, Global>();
    v.push(100);
    ToyShip { engine: ToyEngine { parts: v } }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let ship = make_ship();
    println!("engine parts: {}", ship.engine().parts().len());
    assert_eq!(ship.engine().parts().len(), 1);
}
        "#,
    );
    assert!(output.contains("engine parts: 1"));
}

// --- Group 8: Rust -> rust -> toylang (R(R(T))) ---

#[test]
fn test_r_r_t_vec_of_vec() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyPoint {
    x: i32,
    y: i32,
}

fn make_nested() -> Vec<Vec<ToyPoint, Global>, Global> {
    let inner = Vec::new<ToyPoint, Global>();
    inner.push(ToyPoint { x: 1, y: 2 });
    let outer = Vec::new<Vec<ToyPoint, Global>, Global>();
    outer.push(inner);
    outer
}

fn outer_len(v: &Vec<Vec<ToyPoint, Global>, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let nested = make_nested();
    let len = outer_len(&nested);
    println!("outer len: {}", len);
    assert_eq!(len, 1);
}
        "#,
    );
    assert!(output.contains("outer len: 1"));
}

// --- Group 9: Generic toylang wrapping rust type (T<G=R>) ---

#[test]
fn test_tg_of_vec() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyWrapper<T> {
    inner: T,
}

fn wrap_vec() -> ToyWrapper<Vec<i32, Global>> {
    let v = Vec::new<i32, Global>();
    v.push(42);
    ToyWrapper<Vec<i32, Global>> { inner: v }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let w = wrap_vec();
    println!("inner len: {}", w.inner().len());
    assert_eq!(w.inner().len(), 1);
}
        "#,
    );
    assert!(output.contains("inner len: 1"));
}

#[test]
fn test_tg_of_vec_layout() {
    let output = run_toylang_test(
        r#"
struct ToyWrapper<T> {
    inner: T,
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let size = std::mem::size_of::<ToyWrapper<Vec<i32>>>();
    println!("ToyWrapper<Vec<i32>> size: {}", size);
    assert_eq!(size, 24); // Vec is 24 bytes on 64-bit
}
        "#,
    );
    assert!(output.contains("ToyWrapper<Vec<i32>> size: 24"));
}

// --- Group 10: Generic toylang wrapping toylang type (T<G=T>) ---

#[test]
fn test_tg_of_toypoint() {
    let output = run_toylang_test(
        r#"
struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyWrapper<T> {
    inner: T,
}

fn wrap_point() -> ToyWrapper<ToyPoint> {
    ToyWrapper<ToyPoint> { inner: ToyPoint { x: 5, y: 6 } }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let w = wrap_point();
    println!("inner: {} {}", w.inner().x(), w.inner().y());
    assert_eq!(*w.inner().x(), 5);
    assert_eq!(*w.inner().y(), 6);
}
        "#,
    );
    assert!(output.contains("inner: 5 6"));
}

#[test]
fn test_tg_of_toypoint_layout() {
    let output = run_toylang_test(
        r#"
struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyWrapper<T> {
    inner: T,
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let size = std::mem::size_of::<ToyWrapper<ToyPoint>>();
    println!("ToyWrapper<ToyPoint> size: {}", size);
    assert_eq!(size, 8); // ToyPoint is 8 bytes
}
        "#,
    );
    assert!(output.contains("ToyWrapper<ToyPoint> size: 8"));
}

// --- Group 11: Deep nesting (4+ levels) ---

#[test]
fn test_deep_t_r_t_r() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyLeaf {
    value: i32,
}

struct ToyBranch {
    leaves: Vec<ToyLeaf, Global>,
}

fn make_tree() -> Vec<ToyBranch, Global> {
    let leaves = Vec::new<ToyLeaf, Global>();
    leaves.push(ToyLeaf { value: 1 });
    leaves.push(ToyLeaf { value: 2 });
    let branch = ToyBranch { leaves: leaves };
    let tree = Vec::new<ToyBranch, Global>();
    tree.push(branch);
    tree
}

fn tree_len(t: &Vec<ToyBranch, Global>) -> usize {
    t.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let tree = make_tree();
    let len = tree_len(&tree);
    println!("tree len: {}", len);
    assert_eq!(len, 1);
}
        "#,
    );
    assert!(output.contains("tree len: 1"));
}

// --- Group 12: Multiple mixed fields ---

#[test]
fn test_mixed_fields() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyMixed {
    a: i32,
    b: Vec<i32, Global>,
    c: ToyPoint,
}

fn make_mixed() -> ToyMixed {
    let v = Vec::new<i32, Global>();
    v.push(10);
    ToyMixed { a: 1, b: v, c: ToyPoint { x: 2, y: 3 } }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let m = make_mixed();
    println!("a: {} b_len: {} c.x: {} c.y: {}", m.a(), m.b().len(), m.c().x(), m.c().y());
    assert_eq!(*m.a(), 1);
    assert_eq!(m.b().len(), 1);
    assert_eq!(*m.c().x(), 2);
    assert_eq!(*m.c().y(), 3);
}
        "#,
    );
    assert!(output.contains("a: 1 b_len: 1 c.x: 2 c.y: 3"));
}

#[test]
fn test_mixed_generic() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct ToyGenMixed<T> {
    a: T,
    b: Vec<T, Global>,
    c: i32,
}

fn make_mixed() -> ToyGenMixed<i32> {
    let v = Vec::new<i32, Global>();
    v.push(10);
    v.push(20);
    ToyGenMixed<i32> { a: 42, b: v, c: 99 }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let m = make_mixed();
    println!("a: {} b_len: {} c: {}", m.a(), m.b().len(), m.c());
    assert_eq!(*m.a(), 42);
    assert_eq!(m.b().len(), 2);
    assert_eq!(*m.c(), 99);
}
        "#,
    );
    assert!(output.contains("a: 42 b_len: 2 c: 99"));
}

// ============================================================================
// Group 13: Generic toylang functions
// ============================================================================

#[test]
fn test_generic_wrap() {
    let output = run_toylang_test(
        r#"
struct Wrapper<T> {
    inner: T,
}

fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper<T> { inner: x }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let w = wrap::<i32>(42);
    println!("inner: {}", w.inner());
    assert_eq!(*w.inner(), 42);
}
        "#,
    );
    assert!(output.contains("inner: 42"));
}

#[test]
fn test_generic_wrap_via_concrete() {
    let output = run_toylang_test(
        r#"
struct Wrapper<T> {
    inner: T,
}

fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper<T> { inner: x }
}

fn wrap_i32(x: i32) -> Wrapper<i32> {
    wrap<i32>(x)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let w = wrap_i32(42);
    println!("inner: {}", w.inner());
    assert_eq!(*w.inner(), 42);
}
        "#,
    );
    assert!(output.contains("inner: 42"));
}

#[test]
fn test_concrete_calls_concrete() {
    let output = run_toylang_test(
        r#"
struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn get_counter() -> Counter {
    make_counter()
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let c = get_counter();
    println!("value: {}", c.value());
    assert_eq!(*c.value(), 42);
}
        "#,
    );
    assert!(output.contains("value: 42"));
}

// ============================================================================
#[test]
fn test_generic_callee_with_struct() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn identity<T>(x: T) -> T {
    x
}

fn identity_point(p: Point) -> Point {
    identity<Point>(p)
}

fn make_point() -> Point {
    Point { x: 10, y: 20 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_point();
    let p2 = identity_point(p);
    println!("x: {} y: {}", p2.x(), p2.y());
    assert_eq!(*p2.x(), 10);
    assert_eq!(*p2.y(), 20);
}
        "#,
    );
    assert!(output.contains("x: 10 y: 20"));
}

#[test]
fn test_generic_callee_in_let() {
    let output = run_toylang_test(
        r#"
struct Wrapper<T> {
    inner: T,
}

fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper<T> { inner: x }
}

fn use_wrap() -> i32 {
    let w = wrap<i32>(42);
    42
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let result = use_wrap();
    println!("result: {}", result);
    assert_eq!(result, 42);
}
        "#,
    );
    assert!(output.contains("result: 42"));
}

// ============================================================================
// Group 14: Additional function and struct tests
// ============================================================================

#[test]
fn test_multiple_lets() {
    let output = run_toylang_test(
        r#"
struct Counter {
    value: i32,
}

fn multi_let() -> Counter {
    let a = 42;
    let b = Counter { value: a };
    b
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let c = multi_let();
    println!("value: {}", c.value());
    assert_eq!(*c.value(), 42);
}
        "#,
    );
    assert!(output.contains("value: 42"));
}

#[test]
fn test_var_in_struct_field() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn make_point(x: i32, y: i32) -> Point {
    Point { x: x, y: y }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_point(10, 20);
    println!("x: {} y: {}", p.x(), p.y());
    assert_eq!(*p.x(), 10);
    assert_eq!(*p.y(), 20);
}
        "#,
    );
    assert!(output.contains("x: 10 y: 20"));
}

#[test]
fn test_struct_param_passthrough() {
    let output = run_toylang_test(
        r#"
struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn identity(c: Counter) -> Counter {
    c
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let c = make_counter();
    let c2 = identity(c);
    println!("value: {}", c2.value());
    assert_eq!(*c2.value(), 42);
}
        "#,
    );
    assert!(output.contains("value: 42"));
}

#[test]
fn test_large_struct() {
    let output = run_toylang_test(
        r#"
struct Big {
    a: i32,
    b: i32,
    c: i32,
    d: i32,
    e: i32,
    f: i32,
}

fn make_big() -> Big {
    Big { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let b = make_big();
    println!("a={} f={}", b.a(), b.f());
    assert_eq!(*b.a(), 1);
    assert_eq!(*b.f(), 6);
}
        "#,
    );
    assert!(output.contains("a=1 f=6"));
}

#[test]
fn test_generic_with_i64() {
    let output = run_toylang_test(
        r#"
struct Pair<A, B> {
    first: A,
    second: B,
}

fn make_pair() -> Pair<i64, i64> {
    Pair<i64, i64> { first: 100i64, second: 200i64 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_pair();
    println!("first: {} second: {}", p.first(), p.second());
    assert_eq!(*p.first(), 100i64);
    assert_eq!(*p.second(), 200i64);
}
        "#,
    );
    assert!(output.contains("first: 100 second: 200"));
}

// ============================================================================
// Group 15: Arithmetic expressions
// ============================================================================

#[test]
fn test_arithmetic() {
    let output = run_toylang_test(
        r#"
fn compute() -> i32 {
    let a = 10;
    let b = 20;
    a + b * 2
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let result = compute();
    println!("result: {}", result);
    assert_eq!(result, 50);
}
        "#,
    );
    assert!(output.contains("result: 50"));
}

#[test]
fn test_arithmetic_sub_div() {
    let output = run_toylang_test(
        r#"
fn compute() -> i32 {
    let x = 100;
    let y = 40;
    x - y / 2
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let result = compute();
    println!("result: {}", result);
    assert_eq!(result, 80);
}
        "#,
    );
    assert!(output.contains("result: 80"));
}

// ============================================================================
// Group 16: Vec with primitives
// ============================================================================

#[test]
fn test_vec_i32() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn make_vec() -> Vec<i32, Global> {
    let v = Vec::new<i32, Global>();
    v.push(10);
    v.push(20);
    v.push(30);
    v
}

fn vec_len(v: &Vec<i32, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = make_vec();
    let len = vec_len(&v);
    println!("len: {}", len);
    assert_eq!(len, 3);
}
        "#,
    );
    assert!(output.contains("len: 3"));
}

#[test]
fn test_single_field_struct() {
    let output = run_toylang_test(
        r#"
struct Wrapper {
    value: i32,
}

fn make_wrapper() -> Wrapper {
    Wrapper { value: 99 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let w = make_wrapper();
    println!("value: {}", w.value());
    assert_eq!(*w.value(), 99);
}
        "#,
    );
    assert!(output.contains("value: 99"));
}

#[test]
fn test_struct_with_vec_and_primitive() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct Data {
    count: i32,
    items: Vec<i32, Global>,
}

fn make_data() -> Data {
    let v = Vec::new<i32, Global>();
    v.push(10);
    v.push(20);
    Data { count: 2, items: v }
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let d = make_data();
    println!("count: {} items_len: {}", d.count(), d.items().len());
    assert_eq!(*d.count(), 2);
    assert_eq!(d.items().len(), 2);
}
        "#,
    );
    assert!(output.contains("count: 2 items_len: 2"));
}

#[test]
fn test_vec_of_structs_len() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct Point {
    x: i32,
    y: i32,
}

fn make_points() -> Vec<Point, Global> {
    let v = Vec::new<Point, Global>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v.push(Point { x: 5, y: 6 });
    v
}

fn count_points(v: &Vec<Point, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = make_points();
    let n = count_points(&v);
    println!("count: {}", n);
    assert_eq!(n, 3);
}
        "#,
    );
    assert!(output.contains("count: 3"));
}

// ============================================================================
// Group 16: Toylang owns main
// ============================================================================

#[test]
fn test_toylang_main_simple() {
    let output = run_toylang_test(
        r#"
fn toylang_main() -> i32 {
    42
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let code = toylang_main();
    println!("exit: {}", code);
    assert_eq!(code, 42);
}
        "#,
    );
    assert!(output.contains("exit: 42"));
}

#[test]
fn test_toylang_main_with_struct_v2() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn make_point() -> Point {
    Point { x: 10, y: 20 }
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let p = make_point();
    println!("Point: {} {}", p.x(), p.y());
    assert_eq!(*p.x(), 10);
    assert_eq!(*p.y(), 20);
}
        "#,
    );
    assert!(output.contains("Point: 10 20"));
}

#[test]
fn test_field_access_returns_value() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn get_x() -> i32 {
    let p = Point { x: 10, y: 20 };
    p.x
}

fn get_y() -> i32 {
    let p = Point { x: 10, y: 20 };
    p.y
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let x = get_x();
    let y = get_y();
    println!("x={} y={}", x, y);
    assert_eq!(x, 10);
    assert_eq!(y, 20);
}
        "#,
    );
    assert!(output.contains("x=10 y=20"));
}

#[test]
fn test_bool_return() {
    let output = run_toylang_test(
        r#"
fn always_true() -> bool {
    true
}

fn always_false() -> bool {
    false
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    assert!(always_true());
    assert!(!always_false());
    println!("bool: {} {}", always_true(), always_false());
}
        "#,
    );
    assert!(output.contains("bool: true false"));
}

#[test]
fn test_toylang_to_toylang_struct_param() {
    let output = run_toylang_test(
        r#"
fn println_int(x: i32)

struct Point {
    x: i32,
    y: i32,
}

fn get_x(p: Point) -> i32 {
    p.x
}

fn main() {
    let p = Point { x: 42, y: 99 };
    println_int(get_x(p));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_int(x: i32) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_toylang_to_toylang_large_struct_param() {
    let output = run_toylang_test(
        r#"
fn println_int(x: i32)

struct Quad {
    a: i32,
    b: i32,
    c: i32,
    d: i32,
}

fn sum_quad(q: Quad) -> i32 {
    q.a + q.b + q.c + q.d
}

fn main() {
    let q = Quad { a: 10, b: 20, c: 30, d: 40 };
    println_int(sum_quad(q));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_int(x: i32) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("100"));
}

#[test]
fn test_toylang_main_with_struct() {
    let output = run_toylang_test(
        r#"
fn print_int(x: i32)
fn println_int(x: i32)

struct Point {
    x: i32,
    y: i32,
}

fn main() {
    let p = Point { x: 10, y: 20 };
    print_int(p.x);
    print_int(p.y);
    println_int(0);
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

pub fn print_int(x: i32) { print!("{} ", x); }
pub fn println_int(x: i32) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("10"));
    assert!(output.contains("20"));
}

#[test]
fn test_toylang_main_with_vec_v2() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct Point {
    x: i32,
    y: i32,
}

fn make_vec() -> Vec<Point, Global> {
    let v = Vec::new<Point, Global>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v
}

fn vec_len(v: &Vec<Point, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = make_vec();
    let len = vec_len(&v);
    println!("Vec length: {}", len);
    assert_eq!(len, 2);
}
        "#,
    );
    assert!(output.contains("Vec length: 2"));
}

#[test]
fn test_toylang_main_with_vec() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn println_usize(x: usize)

struct Point {
    x: i32,
    y: i32,
}

fn main() {
    let v = Vec::new<Point, Global>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    println_usize(v.len());
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_usize(x: usize) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("2"));
}

#[test]
fn test_toylang_main_calls_toylang_fn_v2() {
    let output = run_toylang_test(
        r#"
struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn get_counter() -> Counter {
    make_counter()
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let c = get_counter();
    println!("Counter: {}", c.value());
    assert_eq!(*c.value(), 42);
}
        "#,
    );
    assert!(output.contains("Counter: 42"));
}

#[test]
fn test_toylang_main_calls_toylang_fn() {
    let output = run_toylang_test(
        r#"
fn println_int(x: i32)

struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn main() {
    let c = make_counter();
    println_int(c.value);
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_int(x: i32) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("42"));
}

// ============================================================================
// Bug-exposing tests — these test known fragile patterns
// ============================================================================

#[test]
fn test_vec_method_lookup_is_exact() {
    // This test verifies that Vec::new/push/len are found by exact symbol matching,
    // not substring matching. The function "renew" contains "new" in its name.
    // With contains()-based matching, if "renew" is found first in the HashMap,
    // it would be used instead of Vec::new, causing a type mismatch or crash.
    // HashMap ordering is nondeterministic, so this may pass intermittently.
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

struct Point {
    x: i32,
    y: i32,
}

fn renew() -> Point {
    Point { x: 99, y: 88 }
}

fn new_point() -> Point {
    Point { x: 1, y: 2 }
}

fn make_vec() -> Vec<Point, Global> {
    let fresh = renew();
    let also_new = new_point();
    let v = Vec::new<Point, Global>();
    v.push(fresh);
    v.push(also_new);
    v
}

fn vec_len(v: &Vec<Point, Global>) -> usize {
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = make_vec();
    let n = vec_len(&v);
    println!("len: {}", n);
    assert_eq!(n, 2);
}
        "#,
    );
    assert!(output.contains("len: 2"));
}

#[test]
fn test_vec_push_fn_call_result() {
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn println_usize(x: usize)

struct Point {
    x: i32,
    y: i32,
}

fn make_point() -> Point {
    Point { x: 5, y: 6 }
}

fn main() {
    let v = Vec::new<Point, Global>();
    v.push(make_point());
    println_usize(v.len());
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_usize(x: usize) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("1"));
}

#[test]
fn test_lexer_rejects_unknown_chars() {
    // The @ should cause a compilation error.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
fn foo() -> i32 {
    @42
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { println!("{}", foo()); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed due to unknown character @, but succeeded");
}

#[test]
fn test_int_literal_infers_i64_from_return_type() {
    // The value 3000000000 exceeds i32 max (2147483647) but fits in i64.
    // Integer literals get their type from the expected context (here: return type).
    let output = run_toylang_test(
        r#"
fn big() -> i64 {
    3000000000
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    let v = big();
    println!("big: {}", v);
    assert_eq!(v, 3000000000i64);
}
        "#,
    );
    assert!(output.contains("big: 3000000000"));
}

#[test]
fn test_extern_fn_call() {
    let output = run_toylang_test(
        r#"
fn println_int(x: i32)
fn println_bool(x: bool)

fn do_print() {
    println_int(42);
    println_bool(true);
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_int(x: i32) { println!("{}", x); }
pub fn println_bool(x: bool) { println!("{}", x); }

fn main() {
    do_print();
}
        "#,
    );
    assert!(output.contains("42"));
    assert!(output.contains("true"));
}

#[test]
fn test_vec_capacity() {
    // capacity() was previously impossible — it required a new hardcoded match arm.
    // Now it works automatically via fn_abi_of_instance.
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(1);
    v.push(2);
    v.push(3);
    println_usize(v.capacity());
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;

pub fn println_usize(x: usize) { println!("{}", x); }

fn main() {
    __toylang_main();
}
        "#,
    );
    // Vec capacity after 3 pushes is implementation-defined but >= 3
    let cap: usize = output.trim().parse().expect("output should be a number");
    assert!(cap >= 3, "capacity should be >= 3, got {}", cap);
}

// ============================================================================
// Group: Comparison operators
// ============================================================================

#[test]
fn test_eq_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 == 5 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_eq_false() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 == 3 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), false); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_ne_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 != 3 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_lt_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 3 < 5 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_lt_false() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 < 3 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), false); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_le_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 <= 5 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_gt_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 > 3 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_ge_true() {
    let output = run_toylang_test(
        "fn check() -> bool { 5 >= 5 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_comparison_with_arithmetic() {
    let output = run_toylang_test(
        "fn check() -> bool { 2 + 3 == 5 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_comparison_with_variables() {
    let output = run_toylang_test(
        r#"
fn cmp(a: i32, b: i32) -> bool {
    a == b
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(cmp(3, 3), true);
    assert_eq!(cmp(3, 5), false);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Group: if/else
// ============================================================================

#[test]
fn test_if_else_basic() {
    let output = run_toylang_test(
        r#"
fn pick(x: i32) -> i32 {
    if x > 0 { 1 } else { 0 }
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(pick(5), 1);
    assert_eq!(pick(-1), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_if_with_bool_var() {
    let output = run_toylang_test(
        r#"
fn check(flag: bool) -> i32 {
    if flag { 42 } else { 0 }
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(check(true), 42);
    assert_eq!(check(false), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_if_else_expr_in_let() {
    let output = run_toylang_test(
        r#"
fn max(a: i32, b: i32) -> i32 {
    let result = if a > b { a } else { b };
    result
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(max(3, 5), 5);
    assert_eq!(max(7, 2), 7);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_if_else_expr_in_return() {
    let output = run_toylang_test(
        r#"
fn abs(x: i32) -> i32 {
    if x > 0 { x } else { 0 - x }
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(abs(5), 5);
    assert_eq!(abs(-3), 3);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_if_else_nested() {
    let output = run_toylang_test(
        r#"
fn classify(x: i32) -> i32 {
    if x > 0 { 1 } else { if x < 0 { 2 } else { 0 } }
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(classify(5), 1);
    assert_eq!(classify(-3), 2);
    assert_eq!(classify(0), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Group: while loops
// ============================================================================

#[test]
fn test_while_basic() {
    let output = run_toylang_test(
        r#"
fn count_to(n: i32) -> i32 {
    let i = 0;
    while i < n {
        let i = i + 1;
    }
    i
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(count_to(10), 10);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_while_sum() {
    let output = run_toylang_test(
        r#"
fn sum_to(n: i32) -> i32 {
    let i = 0;
    let sum = 0;
    while i < n {
        let i = i + 1;
        let sum = sum + i;
    }
    sum
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(sum_to(10), 55);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_while_zero_iterations() {
    let output = run_toylang_test(
        r#"
fn noop() -> i32 {
    let i = 10;
    while i < 5 {
        let i = i + 1;
    }
    i
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(noop(), 10);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_while_with_if() {
    let output = run_toylang_test(
        r#"
fn count_big(n: i32) -> i32 {
    let i = 0;
    let count = 0;
    while i < n {
        let count = count + if i > 2 { 1 } else { 0 };
        let i = i + 1;
    }
    count
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(count_big(5), 2);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Phase 1: Mutable Assignment
// ============================================================================

#[test]
fn test_assign_basic() {
    let output = run_toylang_test(
        r#"
fn compute() -> i32 {
    let x = 0;
    x = 5;
    x
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(compute(), 5); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_assign_in_while() {
    let output = run_toylang_test(
        r#"
fn count_to(n: i32) -> i32 {
    let i = 0;
    while i < n {
        i = i + 1;
    }
    i
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(count_to(10), 10); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_assign_in_if() {
    let output = run_toylang_test(
        r#"
fn pick(x: i32) -> i32 {
    let result = 0;
    if x > 0 {
        result = 1;
    }
    result
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(pick(5), 1);
    assert_eq!(pick(-1), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_assign_in_while_with_if() {
    let output = run_toylang_test(
        r#"
fn count_big_assign(n: i32) -> i32 {
    let i = 0;
    let count = 0;
    while i < n {
        if i > 2 {
            count = count + 1;
        }
        i = i + 1;
    }
    count
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(count_big_assign(5), 2); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Phase 2: else if
// ============================================================================

#[test]
fn test_else_if_chain() {
    let output = run_toylang_test(
        r#"
fn classify(x: i32) -> i32 {
    if x > 0 {
        1
    } else if x < 0 {
        2
    } else {
        0
    }
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(classify(5), 1);
    assert_eq!(classify(-3), 2);
    assert_eq!(classify(0), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_else_if_key_dispatch() {
    let output = run_toylang_test(
        r#"
fn handle_key(key: i32) -> i32 {
    let result = 0;
    if key == 1 {
        result = 10;
    } else if key == 2 {
        result = 20;
    } else if key == 3 {
        result = 30;
    }
    result
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(handle_key(1), 10);
    assert_eq!(handle_key(2), 20);
    assert_eq!(handle_key(3), 30);
    assert_eq!(handle_key(99), 0);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Phase 3: Boolean Operators (&&, ||)
// ============================================================================

#[test]
fn test_and_true() {
    let output = run_toylang_test(
        "fn check() -> bool { true && true }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_and_false() {
    let output = run_toylang_test(
        "fn check() -> bool { true && false }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), false); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_or_true() {
    let output = run_toylang_test(
        "fn check() -> bool { false || true }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), true); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_or_false() {
    let output = run_toylang_test(
        "fn check() -> bool { false || false }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(check(), false); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_and_with_comparisons() {
    let output = run_toylang_test(
        r#"
fn in_range(x: i32) -> bool {
    x > 0 && x < 10
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(in_range(5), true);
    assert_eq!(in_range(-1), false);
    assert_eq!(in_range(15), false);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_or_with_comparisons() {
    let output = run_toylang_test(
        r#"
fn out_of_range(x: i32) -> bool {
    x < 0 || x > 10
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    assert_eq!(out_of_range(-5), true);
    assert_eq!(out_of_range(15), true);
    assert_eq!(out_of_range(5), false);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_compound_while_condition() {
    let output = run_toylang_test(
        r#"
fn search(n: i32) -> i32 {
    let i = 0;
    let found = false;
    while i < n && found == false {
        if i == 5 {
            found = true;
        }
        i = i + 1;
    }
    i
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(search(10), 6); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Phase 4: Roguelike Integration Test
// ============================================================================

#[test]
fn test_roguelike() {
    let output = run_toylang_test(
        r#"
fn board_is_wall(row: i32, col: i32) -> bool
fn get_move(i: i32) -> i32
fn num_moves() -> i32
fn print_int(x: i32)

fn game() -> i32 {
    let pr = 4;
    let pc = 3;
    let g1r = 1;
    let g1c = 7;
    let g2r = 3;
    let g2c = 3;
    let g3r = 6;
    let g3c = 5;
    let alive = 3;
    let i = 0;
    let nm = num_moves();
    while i < nm {
        let key = get_move(i);
        let nr = pr;
        let nc = pc;
        if key == 0 {
            nr = nr - 1;
        } else if key == 1 {
            nr = nr + 1;
        } else if key == 2 {
            nc = nc - 1;
        } else if key == 3 {
            nc = nc + 1;
        }
        if board_is_wall(nr, nc) == false {
            pr = nr;
            pc = nc;
        }
        if pr == g1r && pc == g1c {
            g1r = -1;
            g1c = -1;
            alive = alive - 1;
        }
        if pr == g2r && pc == g2c {
            g2r = -1;
            g2c = -1;
            alive = alive - 1;
        }
        if pr == g3r && pc == g3c {
            g3r = -1;
            g3c = -1;
            alive = alive - 1;
        }
        i = i + 1;
    }
    print_int(pr);
    print_int(pc);
    print_int(alive);
    alive
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;

pub fn board_is_wall(row: i32, col: i32) -> bool {
    row <= 0 || row >= 9 || col <= 0 || col >= 9
}

static MOVES: &[i32] = &[
    0, 0, 0,
    3, 3, 3, 3,
    1, 1,
];

pub fn get_move(i: i32) -> i32 { MOVES[i as usize] }
pub fn num_moves() -> i32 { MOVES.len() as i32 }
pub fn print_int(x: i32) { println!("{}", x); }

fn main() {
    let alive = game();
    assert_eq!(alive, 1);
    println!("roguelike ok");
}"#,
    );
    assert!(output.contains("roguelike ok"));
}

// ============================================================================
// Tech debt fixes: precedence, assign type check, unary neg
// ============================================================================

#[test]
fn test_and_higher_precedence_than_or() {
    let output = run_toylang_test(
        r#"
fn check(a: bool, b: bool, c: bool) -> bool {
    a || b && c
}
        "#,
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() {
    // a || (b && c): true || (false && false) == true
    assert_eq!(check(true, false, false), true);
    // a || (b && c): false || (true && true) == true
    assert_eq!(check(false, true, true), true);
    // a || (b && c): false || (true && false) == false
    assert_eq!(check(false, true, false), false);
    println!("ok");
}"#,
    );
    assert!(output.contains("ok"));
}

#[test]
fn test_negate_i64() {
    let output = run_toylang_test(
        "fn neg() -> i64 { -1i64 }",
        r#"mod __lang_stubs; use __lang_stubs::*;
fn main() { assert_eq!(neg(), -1i64); println!("ok"); }"#,
    );
    assert!(output.contains("ok"));
}

// ============================================================================
// Deep monomorphization: internal toylang fns should not be exposed to rustc
// ============================================================================

/// Rust calls spork() which calls bork() (toylang-internal) which calls a Rust
/// extern fn. Rustc should never see bork — the deep walk in per_instance_mir
/// should discover the transitive Rust dep directly.
///
/// Currently FAILS because collect_toylang_fn_deps reports bork to rustc
/// instead of recursively walking into it.
#[test]
fn test_internal_toylang_fn_not_monomorphized_by_rustc() {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");
    let log_path = dir.path().join("callback.log");

    // bork is an internal toylang fn — only called by spork, never by Rust.
    // Rustc should never need to monomorphize it.
    std::fs::write(&toylang_path, r#"
fn add_ten(x: i32) -> i32

fn bork(x: i32) -> i32 {
    add_ten(x)
}

fn spork(x: i32) -> i32 {
    bork(x)
}
    "#).unwrap();

    std::fs::write(&rust_path, r#"
mod __lang_stubs; use __lang_stubs::*;
pub fn add_ten(x: i32) -> i32 { x + 10 }
fn main() {
    assert_eq!(spork(5), 15);
    println!("ok");
}
    "#).unwrap();

    let (output, _stderr) = compile_and_run_with_env(
        &toylang_path,
        &rust_path,
        &bin_path,
        &[("TOYLANG_LOG_PATH", log_path.to_str().unwrap())],
    );
    assert!(output.contains("ok"));

    // Read the callback log and verify rustc never asked us to monomorphize bork.
    let log = std::fs::read_to_string(&log_path).expect("callback log not written");
    assert!(log_mentions_callback_for(&log, "spork"),
        "expected spork to be monomorphized, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "bork"),
        "bork should NOT be monomorphized by rustc — it's internal to toylang, log:\n{}", log);
}

/// Three-deep chain: Rust → a → b → c → Rust extern.
/// Only `a` should be monomorphized by rustc. `b` and `c` are internal.
#[test]
fn test_deep_chain_only_entry_point_monomorphized() {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");
    let log_path = dir.path().join("callback.log");

    std::fs::write(&toylang_path, r#"
fn add_one(x: i32) -> i32

fn c(x: i32) -> i32 {
    add_one(x)
}

fn b(x: i32) -> i32 {
    c(x)
}

fn a(x: i32) -> i32 {
    b(x)
}
    "#).unwrap();

    std::fs::write(&rust_path, r#"
mod __lang_stubs; use __lang_stubs::*;
pub fn add_one(x: i32) -> i32 { x + 1 }
fn main() {
    assert_eq!(a(10), 11);
    println!("ok");
}
    "#).unwrap();

    let (output, _stderr) = compile_and_run_with_env(
        &toylang_path, &rust_path, &bin_path,
        &[("TOYLANG_LOG_PATH", log_path.to_str().unwrap())],
    );
    assert!(output.contains("ok"));

    let log = std::fs::read_to_string(&log_path).expect("callback log not written");
    assert!(log_mentions_callback_for(&log, "a"),
        "expected a to be monomorphized, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "b"),
        "b should NOT be monomorphized by rustc, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "c"),
        "c should NOT be monomorphized by rustc, log:\n{}", log);
}

/// Diamond call pattern: entry → left → bottom, entry → right → bottom.
/// Only `entry` should be monomorphized by rustc. left, right, bottom are internal.
#[test]
fn test_diamond_call_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");
    let log_path = dir.path().join("callback.log");

    std::fs::write(&toylang_path, r#"
fn add(x: i32, y: i32) -> i32

fn bottom(x: i32) -> i32 {
    add(x, 1)
}

fn left(x: i32) -> i32 {
    bottom(x)
}

fn right(x: i32) -> i32 {
    bottom(x)
}

fn entry(x: i32) -> i32 {
    add(left(x), right(x))
}
    "#).unwrap();

    std::fs::write(&rust_path, r#"
mod __lang_stubs; use __lang_stubs::*;
pub fn add(x: i32, y: i32) -> i32 { x + y }
fn main() {
    assert_eq!(entry(5), 12);
    println!("ok");
}
    "#).unwrap();

    let (output, _stderr) = compile_and_run_with_env(
        &toylang_path, &rust_path, &bin_path,
        &[("TOYLANG_LOG_PATH", log_path.to_str().unwrap())],
    );
    assert!(output.contains("ok"));

    let log = std::fs::read_to_string(&log_path).expect("callback log not written");
    assert!(log_mentions_callback_for(&log, "entry"),
        "expected entry to be monomorphized, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "left"),
        "left should NOT be monomorphized by rustc, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "right"),
        "right should NOT be monomorphized by rustc, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "bottom"),
        "bottom should NOT be monomorphized by rustc, log:\n{}", log);
}

/// Generic deep walk: entry<i32> → helper<i32> → Rust extern.
/// Only entry should be monomorphized by rustc. helper is internal.
#[test]
fn test_generic_deep_walk() {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");
    let log_path = dir.path().join("callback.log");

    std::fs::write(&toylang_path, r#"
fn identity(x: i32) -> i32

fn helper<T>(x: T) -> T {
    x
}

fn entry(x: i32) -> i32 {
    let y = helper<i32>(x);
    identity(y)
}
    "#).unwrap();

    std::fs::write(&rust_path, r#"
mod __lang_stubs; use __lang_stubs::*;
pub fn identity(x: i32) -> i32 { x }
fn main() {
    assert_eq!(entry(42), 42);
    println!("ok");
}
    "#).unwrap();

    let (output, _stderr) = compile_and_run_with_env(
        &toylang_path, &rust_path, &bin_path,
        &[("TOYLANG_LOG_PATH", log_path.to_str().unwrap())],
    );
    assert!(output.contains("ok"));

    let log = std::fs::read_to_string(&log_path).expect("callback log not written");
    assert!(log_mentions_callback_for(&log, "entry"),
        "expected entry to be monomorphized, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "helper"),
        "helper should NOT be monomorphized by rustc, log:\n{}", log);
}

/// Two Rust entry points calling the same internal function.
/// Both entry points should be monomorphized, but the shared internal fn should not.
#[test]
fn test_two_entry_points_shared_internal() {
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");
    let log_path = dir.path().join("callback.log");

    std::fs::write(&toylang_path, r#"
fn double(x: i32) -> i32

fn internal_helper(x: i32) -> i32 {
    double(x)
}

fn entry_a(x: i32) -> i32 {
    internal_helper(x)
}

fn entry_b(x: i32) -> i32 {
    internal_helper(x)
}
    "#).unwrap();

    std::fs::write(&rust_path, r#"
mod __lang_stubs; use __lang_stubs::*;
pub fn double(x: i32) -> i32 { x * 2 }
fn main() {
    assert_eq!(entry_a(5), 10);
    assert_eq!(entry_b(7), 14);
    println!("ok");
}
    "#).unwrap();

    let (output, _stderr) = compile_and_run_with_env(
        &toylang_path, &rust_path, &bin_path,
        &[("TOYLANG_LOG_PATH", log_path.to_str().unwrap())],
    );
    assert!(output.contains("ok"));

    let log = std::fs::read_to_string(&log_path).expect("callback log not written");
    assert!(log_mentions_callback_for(&log, "entry_a"),
        "expected entry_a to be monomorphized, log:\n{}", log);
    assert!(log_mentions_callback_for(&log, "entry_b"),
        "expected entry_b to be monomorphized, log:\n{}", log);
    assert!(!log_mentions_callback_for(&log, "internal_helper"),
        "internal_helper should NOT be monomorphized by rustc, log:\n{}", log);
}

// ============================================================================
// Group: Trait static calls (Trait::method(receiver, args))
// ============================================================================

#[test]
fn test_trait_static_call_inherent_still_works() {
    // Verify that inherent StaticCall (Vec::new) still works after trait support was added
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn get_len() -> usize {
    let v = Vec::new<i32, Global>();
    v.push(42);
    v.len()
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
fn main() { println!("{}", get_len()); }
        "#,
    );
    assert!(output.contains("1"));
}

#[test]
fn test_trait_static_call_clone_vec() {
    // Call Clone::clone(&v) on a Vec — trait method via explicit qualification.
    // Tests sret return + @TCHAPZ null pointer fix for #[track_caller].
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::clone::Clone

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(10);
    v.push(20);
    Clone::clone(&v);
    println_usize(v.len())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("2"));
}

#[test]
fn test_trait_static_call_clone_vec_use_result() {
    // Clone a Vec and verify the cloned Vec has the correct length
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::clone::Clone

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(10);
    v.push(20);
    v.push(30);
    let v2 = Clone::clone(&v);
    println_usize(v2.len())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("3"));
}

#[test]
fn test_trait_static_call_result_discarded() {
    // Clone::clone as ExprStmt — result is discarded
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::clone::Clone

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(1);
    Clone::clone(&v);
    println_usize(v.len())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("1"));
}

#[test]
fn test_ref_expr_basic() {
    // &var produces a reference — used as argument to trait method
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::clone::Clone

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(42);
    let r = &v;
    let v2 = Clone::clone(r);
    println_usize(v2.len())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("1"));
}

// ==========================================================================
// Phase 2: Free function calls + arg type checking
// ==========================================================================

#[test]
fn test_extern_fn_decl_still_works() {
    // Regression: body-less extern fn declarations still compile and link
    let output = run_toylang_test(
        r#"
fn print_i32(x: i32)

fn main() {
    print_i32(42i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle] pub extern "C" fn print_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_rust_free_fn_undefined_gives_error() {
    // Compile-fail: calling a function that doesn't exist anywhere
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
fn main() {
    completely_undefined_xyz()
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for undefined function, but succeeded");
}

#[test]
fn test_main_non_void_tail_rejected() {
    // Per @MBMRVZ, `fn main()` must have a void-typed tail. Non-void
    // tails used to silently compile and SIGBUS at runtime during
    // teardown; the type resolver now rejects them with a clean error.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    // Main's tail is `1i32` — non-void. Should be rejected.
    std::fs::write(&toylang_path, r#"
fn main() {
    1i32
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for non-void main tail, but succeeded");
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(stderr.contains("MainMustReturnVoid"),
        "stderr should mention MainMustReturnVoid; got: {}", stderr);
}

#[test]
fn test_trait_self_not_imported_gives_error() {
    // Per @RTMEIZ, the Self type of a trait call must be use-imported.
    // Missing `use std::io::Stdout` → structured error, not a panic/ICE.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
use std::io::stdout
use std::io::Write

fn main() {
    let out = stdout();
    Write::write_all(&out, b"hello\n");
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for missing Stdout import, but succeeded");
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(stderr.contains("Stdout"),
        "stderr should mention 'Stdout'; got: {}", stderr);
    assert!(stderr.contains("RustTypeNotImported"),
        "stderr should mention 'RustTypeNotImported'; got: {}", stderr);
}


#[test]
fn test_byte_string_let_binding() {
    // Verify b"hello" compiles and the program runs without crashing.
    let output = run_toylang_test(
        r#"
fn main() -> i32 {
    let x = b"hello";
    42i32
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "42");
}

#[test]
fn test_byte_string_passed_to_rust_fn() {
    // Critical ScalarPair ABI test: pass b"hello" (a fat pointer { ptr, len })
    // to a Rust function taking &[u8]. If the ABI is wrong (struct vs two scalars),
    // this will segfault or return garbage.
    let output = run_toylang_test(
        r#"
fn check_bytes(data: &[u8]) -> i32

fn main() -> i32 {
    check_bytes(b"hello")
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

#[no_mangle]
pub fn check_bytes(data: &[u8]) -> i32 {
    data.len() as i32
}

fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "5");
}

#[test]
fn test_string_literal_let_binding() {
    // Verify "hello" compiles and the program runs without crashing.
    let output = run_toylang_test(
        r#"
fn main() -> i32 {
    let x = "hello";
    42i32
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "42");
}

#[test]
fn test_string_literal_passed_to_rust_fn() {
    // Mirrors test_byte_string_passed_to_rust_fn but for regular string literals.
    // Pass "hello" (should be &str: ScalarPair { ptr, len }) to a Rust function
    // taking &str. Exercises the &str ABI path that will unblock clap/regex/
    // toml/serde_json smoke tests in Phase 7.
    let output = run_toylang_test(
        r#"
fn check_str(data: &str) -> i32

fn main() -> i32 {
    check_str("hello")
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

#[no_mangle]
pub fn check_str(data: &str) -> i32 {
    data.len() as i32
}

fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "5");
}

#[test]
fn test_string_literal_empty() {
    // Edge case: "" has len 0. Verifies the len field is computed per-literal.
    let output = run_toylang_test(
        r#"
fn check_str(data: &str) -> i32

fn main() -> i32 {
    check_str("")
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

#[no_mangle]
pub fn check_str(data: &str) -> i32 {
    data.len() as i32
}

fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_string_literal_with_escapes() {
    // Proves the lexer interprets \n, \t inside regular strings (not
    // literal backslashes). "hello\nworld" has len 11 after escape
    // expansion.
    let output = run_toylang_test(
        r#"
fn check_str(data: &str) -> i32

fn main() -> i32 {
    check_str("hello\nworld")
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

#[no_mangle]
pub fn check_str(data: &str) -> i32 {
    data.len() as i32
}

fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "11");
}

#[test]
fn test_multiple_string_literals() {
    // Two distinct literals in the same fn must get two distinct global
    // byte arrays. Guards against codegen reusing a shared global.
    let output = run_toylang_test(
        r#"
fn check_str(data: &str) -> i32

fn main() -> i32 {
    check_str("abc") + check_str("de")
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

#[no_mangle]
pub fn check_str(data: &str) -> i32 {
    data.len() as i32
}

fn main() { let code = __toylang_main(); println!("{}", code); }
        "#,
    );
    assert_eq!(output.trim(), "5");
}

// ── Phase 4: I/O integration ────────────────────────────────────────

#[test]
fn test_stdout_call() {
    // Verify stdout() alone compiles — isolates free-function-returning-struct
    // from trait call machinery.
    let output = run_toylang_test(
        r#"
use std::io::stdout
use std::io::Stdout

fn println_i32(x: i32)

fn main() {
    let out = stdout();
    println_i32(42i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_stdout_write_all() {
    // Full I/O: Write::write_all(&stdout(), b"hello from toylang\n")
    let output = run_toylang_test(
        r#"
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn println_i32(x: i32)

fn main() {
    let out = stdout();
    Write::write_all(&out, b"hello from toylang\n");
    println_i32(0i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("hello from toylang"));
}

#[test]
fn test_stdout_multiple_writes() {
    // Multiple Write::write_all calls
    let output = run_toylang_test(
        r#"
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn println_i32(x: i32)

fn main() {
    let out = stdout();
    Write::write_all(&out, b"aaa\n");
    Write::write_all(&out, b"bbb\n");
    println_i32(0i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("aaa"));
    assert!(output.contains("bbb"));
}

#[test]
fn test_write_all_result_bound() {
    // Bind write_all's Result<(), Error> return value to a variable.
    // Exercises rustc_ty_to_resolved_type on Result and its nested types.
    let output = run_toylang_test(
        r#"
use std::io::stdout
use std::io::Stdout
use std::io::Write
use std::result::Result
use std::io::Error

fn println_i32(x: i32)

fn main() {
    let out = stdout();
    let r = Write::write_all(&out, b"hello\n");
    println_i32(42i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("hello"));
    assert!(output.contains("42"));
}

#[test]
fn test_vec_pop_returns_option() {
    // Vec::pop() returns Option<T> — exercises rustc_ty_to_resolved_type on
    // Option (Adt with type arg). Binding the result exercises the full
    // type conversion chain including the i32 type arg inside Option.
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::option::Option

fn println_i32(x: i32)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(99);
    let popped = v.pop();
    println_i32(42i32)
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_rust_fn_returning_option_u8() {
    // A Rust function returning Option<u8> exercises rustc_ty_to_resolved_type
    // on TyKind::Uint(U8) inside the Option's type args.
    let output = run_toylang_test(
        r#"
use std::option::Option

fn get_byte() -> Option<u8>
fn println_i32(x: i32)

fn main() {
    let b = get_byte();
    println_i32(42i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn get_byte() -> Option<u8> { Some(7) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

// ---- Phase 6: .unwrap() on Option / Result ----

#[test]
fn test_option_unwrap_basic() {
    // Option::unwrap is #[inline(always)] — direct call would produce no
    // external symbol. Phase 6 redirects to __toylang_option_unwrap wrapper
    // in __lang_stubs (forced External by partitioning.rs patch).
    let output = run_toylang_test(
        r#"
use std::option::Option

fn make_some_i32(x: i32) -> Option<i32>
fn println_i32(x: i32)

fn main() {
    let o = make_some_i32(42i32);
    let v = o.unwrap();
    println_i32(v)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_result_unwrap_basic() {
    // Result::unwrap with E: Debug. Wrapper preserves the bound verbatim.
    let output = run_toylang_test(
        r#"
use std::result::Result

fn make_ok_i32(x: i32) -> Result<i32, i32>
fn println_i32(x: i32)

fn main() {
    let r = make_ok_i32(7i32);
    let v = r.unwrap();
    println_i32(v)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_ok_i32(x: i32) -> Result<i32, i32> { Ok(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("7"));
}

#[test]
fn test_option_unwrap_result_discarded() {
    // Calling unwrap as ExprStmt — return value discarded. Exercises wrapper
    // when it's not bound to a variable.
    let output = run_toylang_test(
        r#"
use std::option::Option

fn make_some_i32(x: i32) -> Option<i32>
fn println_i32(x: i32)

fn main() {
    let o = make_some_i32(123i32);
    o.unwrap();
    println_i32(456i32)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("456"));
}

#[test]
fn test_unwrap_arithmetic_chain() {
    // unwrap() result used in an arithmetic expression — exercises the
    // result-typed expression context.
    let output = run_toylang_test(
        r#"
use std::option::Option

fn make_some_i32(x: i32) -> Option<i32>
fn println_i32(x: i32)

fn main() {
    let o = make_some_i32(40i32);
    let v = o.unwrap() + 2i32;
    println_i32(v)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

#[test]
fn test_vec_pop_unwrap() {
    // Vec::pop returns Option<T>, then unwrap. Nested MethodCall resolution.
    // Also exercises push_arg_for_rust_call's per-arg ABI coercion: Vec::push
    // takes value: i32 by Direct(i32), so the call site must pass the i32
    // value (not a pointer to it).
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::option::Option

fn println_i32(x: i32)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(99i32);
    let popped = v.pop();
    let inner = popped.unwrap();
    println_i32(inner)
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("99"));
}

#[test]
fn test_unwrap_two_options_separately() {
    // Two unwrap call sites — exercises wrapper symbol caching and the
    // single-monomorphization path (both call __toylang_option_unwrap::<i32>).
    let output = run_toylang_test(
        r#"
use std::option::Option

fn make_some_i32(x: i32) -> Option<i32>
fn println_i32(x: i32)

fn main() {
    let a = make_some_i32(10i32);
    let b = make_some_i32(32i32);
    let av = a.unwrap();
    let bv = b.unwrap();
    println_i32(av + bv)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}

// ============================================================================
// Per @IVTDBTZ — inherent-vs-trait dispatch is type-kind based, not arg-count
// based. The following tests are regression guards for the dispatch fix and
// its sibling codegen fix (inherent StaticCall iterates args). See
// docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md.
// ============================================================================

#[test]
fn test_static_call_zero_args_is_inherent() {
    // Regression guard. Before @IVTDBTZ the classifier short-circuited
    // zero-arg static calls to the inherent path via `!typed_args.is_empty()`.
    // After the fix dispatch is pure `is_rust_trait(ty)`, so zero-arg calls
    // must continue routing to inherent purely because `Vec` isn't a trait.
    // Fails loudly if someone re-introduces an arg-count check as a
    // "perf optimization".
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    println_usize(v.capacity())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_static_call_nonempty_args_rust_struct() {
    // Positive test for @IVTDBTZ. `Vec::with_capacity<T, A>(n: usize)` is
    // an inherent static method on a Rust struct with a non-zero-arg list.
    // Before the fix, dispatch misrouted this to the trait path (panicking
    // at oracle.rs:600 because `Vec` isn't a trait). Even after fixing
    // dispatch, the inherent StaticCall codegen used to hardcode
    // `build_call(func, &[])` and discard the arg — making this test
    // double-duty: it verifies dispatch routes correctly AND that the arg
    // actually flows through to the Rust function (observable via
    // .capacity() returning the value we passed).
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec

fn println_usize(x: usize)

fn main() {
    let v = Vec::with_capacity<i32, Global>(5usize);
    println_usize(v.capacity())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert_eq!(output.trim(), "5");
}

#[test]
fn test_static_call_nonempty_args_trait() {
    // Regression guard: the trait-call path must still work after the
    // dispatch change. `Clone::clone(&v)` routes via is_rust_trait == true,
    // __trait::Clone prefix, rust_trait_method_return_type. Mirrors
    // test_trait_static_call_clone_vec but named to make its post-@IVTDBTZ
    // intent explicit.
    let output = run_toylang_test(
        r#"
use std::alloc::Global
use std::vec::Vec
use std::clone::Clone

fn println_usize(x: usize)

fn main() {
    let v = Vec::new<i32, Global>();
    v.push(7i32);
    let v2 = Clone::clone(&v);
    println_usize(v2.len())
}
        "#,
        r#"
#![feature(allocator_api)]
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn println_usize(x: usize) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert_eq!(output.trim(), "1");
}

#[test]
fn test_static_call_undefined_type_gives_structured_error() {
    // Per @IVTDBTZ: an unknown `Name::method(args)` where `Name` is neither
    // a use-imported trait nor a use-imported Rust struct/enum must yield
    // a structured error, not an ICE. Dispatch falls through to the
    // inherent path since `is_rust_trait("UndefinedThing") == false`;
    // try_resolved_to_rustc_ty then emits RustTypeNotImported with a
    // structured context.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
fn main() {
    UndefinedThing::method(42i32)
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for UndefinedThing::method, but succeeded");
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(stderr.contains("RustTypeNotImported"),
        "stderr should mention structured 'RustTypeNotImported'; got: {}", stderr);
    assert!(stderr.contains("UndefinedThing"),
        "stderr should mention 'UndefinedThing'; got: {}", stderr);
    // The pre-@IVTDBTZ panic was `panic!("trait '{}' not found", ...)` — that
    // exact phrasing must not appear post-fix.
    assert!(!stderr.contains("trait 'UndefinedThing' not found"),
        "regression: pre-@IVTDBTZ panic string surfaced. stderr: {}", stderr);
}

#[test]
fn test_trait_call_unknown_trait_name_gives_structured_error() {
    // Per @IVTDBTZ / @RTMEIZ: trait dispatch is via is_rust_trait predicate.
    // A name not imported as a trait (typo or missing use) returns false
    // from the predicate → dispatch goes inherent → inherent lookup also
    // fails → structured RustTypeNotImported. This covers the dispatch-
    // falls-through-cleanly path; the next test covers the path where the
    // trait IS imported but the method name is typo'd.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let out = stdout();
    Writ::write_all(&out, b"hello\n");
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for typo'd trait name, but succeeded");
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(stderr.contains("RustTypeNotImported"),
        "stderr should mention structured 'RustTypeNotImported'; got: {}", stderr);
    assert!(stderr.contains("Writ"),
        "stderr should mention 'Writ'; got: {}", stderr);
    assert!(!stderr.contains("trait 'Writ' not found"),
        "regression: pre-@IVTDBTZ panic string surfaced. stderr: {}", stderr);
}

#[test]
fn test_trait_call_unknown_method_name_gives_structured_error() {
    // Per @IVTDBTZ part 6b: trait imported correctly, but method name typo.
    // is_rust_trait("Write") == true → dispatch goes trait → oracle finds
    // trait DefId but can't find `writ_all` as an associated item → the
    // formerly-panicking path at oracle.rs:619 now returns
    // UnresolvedRustType with a TraitMethodName context. Structured error,
    // no ICE.
    let dir = tempfile::tempdir().unwrap();
    let toylang_path = dir.path().join("input.toylang");
    let rust_path = dir.path().join("test.rs");
    let bin_path = dir.path().join("test_bin");

    std::fs::write(&toylang_path, r#"
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn main() {
    let out = stdout();
    Write::writ_all(&out, b"hello\n");
}
    "#).unwrap();
    std::fs::write(&rust_path, r#"
mod __lang_stubs;
use __lang_stubs::*;
fn main() { __toylang_main(); }
    "#).unwrap();

    let compile = Command::new(toylangc_bin())
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .args(&[
            "--edition", "2021",
            "--toylang-input", toylang_path.to_str().unwrap(),
            rust_path.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run toylangc");

    assert!(!compile.status.success(),
        "compilation should have failed for typo'd method name, but succeeded");
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(stderr.contains("RustTypeNotImported"),
        "stderr should mention structured 'RustTypeNotImported'; got: {}", stderr);
    assert!(stderr.contains("writ_all"),
        "stderr should mention 'writ_all'; got: {}", stderr);
    // The TraitMethodName variant's Display produces "as method name on
    // trait `Write`" — confirms we routed through the new 6b error path
    // rather than the inherent fallback.
    assert!(stderr.contains("as method name on trait"),
        "stderr should reference the TraitMethodName Display output; got: {}", stderr);
    assert!(!stderr.contains("method 'writ_all' not defined on trait"),
        "regression: pre-@IVTDBTZ panic string surfaced. stderr: {}", stderr);
}
