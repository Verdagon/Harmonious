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
    Pair { first: 10, second: 20 }
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
struct Point {
    x: i32,
    y: i32,
}

fn make_vec() -> Vec<Point> {
    let v = Vec::new<Point>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v
}

fn vec_len(v: &Vec<Point>) -> usize {
    v.len()
}
        "#,
        r#"
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
    Pair { first: 10, second: 2000000000000 }
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
    Pair { first: true, second: 99 }
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
struct ToyShip {
    wings: Vec<i32>,
}

fn make_ship() -> ToyShip {
    let v = Vec::new<i32>();
    v.push(1);
    v.push(2);
    ToyShip { wings: v }
}
        "#,
        r#"
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
struct ToyShip {
    wings: Vec<i32>,
}
        "#,
        r#"
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
struct ToyShip {
    wings: Vec<i32>,
}

fn make_fleet() -> Vec<ToyShip> {
    let fleet = Vec::new<ToyShip>();
    let v = Vec::new<i32>();
    v.push(10);
    fleet.push(ToyShip { wings: v });
    fleet
}

fn fleet_len(f: &Vec<ToyShip>) -> usize {
    f.len()
}
        "#,
        r#"
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
struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyFleet {
    ships: Vec<ToyPoint>,
}

fn make_fleet() -> ToyFleet {
    let v = Vec::new<ToyPoint>();
    v.push(ToyPoint { x: 1, y: 2 });
    v.push(ToyPoint { x: 3, y: 4 });
    ToyFleet { ships: v }
}
        "#,
        r#"
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
struct ToyEngine {
    parts: Vec<i32>,
}

struct ToyShip {
    engine: ToyEngine,
}

fn make_ship() -> ToyShip {
    let v = Vec::new<i32>();
    v.push(100);
    ToyShip { engine: ToyEngine { parts: v } }
}
        "#,
        r#"
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
struct ToyPoint {
    x: i32,
    y: i32,
}

fn make_nested() -> Vec<Vec<ToyPoint>> {
    let inner = Vec::new<ToyPoint>();
    inner.push(ToyPoint { x: 1, y: 2 });
    let outer = Vec::new<Vec<ToyPoint>>();
    outer.push(inner);
    outer
}

fn outer_len(v: &Vec<Vec<ToyPoint>>) -> usize {
    v.len()
}
        "#,
        r#"
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
struct ToyWrapper<T> {
    inner: T,
}

fn wrap_vec() -> ToyWrapper<Vec<i32>> {
    let v = Vec::new<i32>();
    v.push(42);
    ToyWrapper { inner: v }
}
        "#,
        r#"
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
    ToyWrapper { inner: ToyPoint { x: 5, y: 6 } }
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
struct ToyLeaf {
    value: i32,
}

struct ToyBranch {
    leaves: Vec<ToyLeaf>,
}

fn make_tree() -> Vec<ToyBranch> {
    let leaves = Vec::new<ToyLeaf>();
    leaves.push(ToyLeaf { value: 1 });
    leaves.push(ToyLeaf { value: 2 });
    let branch = ToyBranch { leaves: leaves };
    let tree = Vec::new<ToyBranch>();
    tree.push(branch);
    tree
}

fn tree_len(t: &Vec<ToyBranch>) -> usize {
    t.len()
}
        "#,
        r#"
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
struct ToyPoint {
    x: i32,
    y: i32,
}

struct ToyMixed {
    a: i32,
    b: Vec<i32>,
    c: ToyPoint,
}

fn make_mixed() -> ToyMixed {
    let v = Vec::new<i32>();
    v.push(10);
    ToyMixed { a: 1, b: v, c: ToyPoint { x: 2, y: 3 } }
}
        "#,
        r#"
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
struct ToyGenMixed<T> {
    a: T,
    b: Vec<T>,
    c: i32,
}

fn make_mixed() -> ToyGenMixed<i32> {
    let v = Vec::new<i32>();
    v.push(10);
    v.push(20);
    ToyGenMixed { a: 42, b: v, c: 99 }
}
        "#,
        r#"
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
    Wrapper { inner: x }
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
    Wrapper { inner: x }
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
    Wrapper { inner: x }
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
    Pair { first: 100, second: 200 }
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
fn make_vec() -> Vec<i32> {
    let v = Vec::new<i32>();
    v.push(10);
    v.push(20);
    v.push(30);
    v
}

fn vec_len(v: &Vec<i32>) -> usize {
    v.len()
}
        "#,
        r#"
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
struct Data {
    count: i32,
    items: Vec<i32>,
}

fn make_data() -> Data {
    let v = Vec::new<i32>();
    v.push(10);
    v.push(20);
    Data { count: 2, items: v }
}
        "#,
        r#"
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
struct Point {
    x: i32,
    y: i32,
}

fn make_points() -> Vec<Point> {
    let v = Vec::new<Point>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v.push(Point { x: 5, y: 6 });
    v
}

fn count_points(v: &Vec<Point>) -> usize {
    v.len()
}
        "#,
        r#"
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
struct Point {
    x: i32,
    y: i32,
}

fn get_x(p: Point) -> i32 {
    p.x
}

fn main() {
    let p = Point { x: 42, y: 99 };
    println("{}", get_x(p));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

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
    println("{}", sum_quad(q));
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

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
struct Point {
    x: i32,
    y: i32,
}

fn main() {
    let p = Point { x: 10, y: 20 };
    println("Point: {} {}", p.x, p.y);
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("Point: 10 20"));
}

#[test]
fn test_toylang_main_with_vec_v2() {
    let output = run_toylang_test(
        r#"
struct Point {
    x: i32,
    y: i32,
}

fn make_vec() -> Vec<Point> {
    let v = Vec::new<Point>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    v
}

fn vec_len(v: &Vec<Point>) -> usize {
    v.len()
}
        "#,
        r#"
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
struct Point {
    x: i32,
    y: i32,
}

fn main() {
    let v = Vec::new<Point>();
    v.push(Point { x: 1, y: 2 });
    v.push(Point { x: 3, y: 4 });
    println("Vec length: {}", v.len());
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("Vec length: 2"));
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
struct Counter {
    value: i32,
}

fn make_counter() -> Counter {
    Counter { value: 42 }
}

fn main() {
    let c = make_counter();
    println("Counter: {}", c.value);
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("Counter: 42"));
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

fn make_vec() -> Vec<Point> {
    let fresh = renew();
    let also_new = new_point();
    let v = Vec::new<Point>();
    v.push(fresh);
    v.push(also_new);
    v
}

fn vec_len(v: &Vec<Point>) -> usize {
    v.len()
}
        "#,
        r#"
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
struct Point {
    x: i32,
    y: i32,
}

fn make_point() -> Point {
    Point { x: 5, y: 6 }
}

fn main() {
    let v = Vec::new<Point>();
    v.push(make_point());
    println("len: {}", v.len());
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;

fn main() {
    __toylang_main();
}
        "#,
    );
    assert!(output.contains("len: 1"));
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
