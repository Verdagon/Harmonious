use std::path::PathBuf;
use std::process::Command;

// Duplicated here from `toylangc/src/main.rs`'s `TOYLANG_NIGHTLY` — integration
// tests cannot import from a `[[bin]]`-only crate, so the pin is carried
// independently. See HANDOFF-nightly-bump.md §3.2 and the `TOYLANG_NIGHTLY`
// doc comment in main.rs for the bump-site inventory.
const TOYLANG_NIGHTLY: &str = "nightly-2026-01-20";

fn toylangc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_toylangc"))
}

fn sysroot_lib() -> String {
    // Zero-fork: standalone tests run against the same vanilla rustup
    // toolchain as the rest of the suite (see `TOYLANG_NIGHTLY` above).
    // The historical `rustc-fork` toolchain (HANDOFF-TL.md §3d) is
    // vestigial and no longer referenced here.
    let out = Command::new("rustup")
        .args(["run", TOYLANG_NIGHTLY, "rustc", "--print=sysroot"])
        .output()
        .expect("failed to run rustup");
    let sysroot = String::from_utf8(out.stdout).unwrap();
    format!("{}/lib", sysroot.trim())
}

fn run_build(project_dir: &std::path::Path) -> std::process::Output {
    Command::new(toylangc_bin())
        .current_dir(project_dir)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .args(["build"])
        .output()
        .expect("failed to run toylangc build")
}

/// Standard harness for Phase 7 standalone smoke tests.
///
/// Each standalone crate lives at `tests/standalone/<project_name>/`
/// with a `toylang.toml` whose `[project].name` is `<project_name>`.
/// The produced binary at `.toylang-build/target/debug/<project_name>`
/// is expected to print `expected` (usually `"<crate> ok"`) and exit
/// zero. The build dir is wiped before each run so tests are hermetic.
fn run_standalone_test(project_name: &str, expected: &str) {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone")
        .join(project_name);

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let build_out = run_build(&project);
    assert!(
        build_out.status.success(),
        "{} toylangc build failed:\nstdout: {}\nstderr: {}",
        project_name,
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let bin = build_dir.join("target/debug").join(project_name);
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .unwrap_or_else(|e| panic!("failed to run {} binary: {}", project_name, e));
    assert!(
        run.status.success(),
        "{} exited non-zero:\nstdout: {}\nstderr: {}",
        project_name,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains(expected),
        "expected '{}' in stdout of {}, got: {}",
        expected,
        project_name,
        stdout,
    );
}

#[test]
fn test_build_minimal_project() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    std::fs::write(
        project.join("toylang.toml"),
        r#"[project]
name = "minimal_app"
source = "main.toylang"
"#,
    )
    .unwrap();
    std::fs::write(project.join("main.toylang"), "fn main() {}\n").unwrap();

    let out = run_build(project);
    assert!(
        out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let bin = project
        .join(".toylang-build")
        .join("target/debug/minimal_app");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "binary exited with error:\nstderr: {}",
        String::from_utf8_lossy(&run.stderr),
    );
}

#[test]
fn test_build_with_rust_dep() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // Use `toml` as the dep since it's already a toylangc build dep —
    // cargo should have it cached so this test doesn't require network.
    std::fs::write(
        project.join("toylang.toml"),
        r#"[project]
name = "with_dep"
source = "main.toylang"

[rust-dependencies]
toml = "0.8"
"#,
    )
    .unwrap();
    std::fs::write(project.join("main.toylang"), "fn main() {}\n").unwrap();

    let out = run_build(project);
    assert!(
        out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let lockfile_path = project.join(".toylang-build/Cargo.lock");
    let lockfile = std::fs::read_to_string(&lockfile_path)
        .expect("Cargo.lock should exist after build");
    assert!(
        lockfile.contains("name = \"toml\""),
        "Cargo.lock should list toml as a dep; contents: {}",
        lockfile
    );
}

#[test]
fn test_build_invalid_manifest_fails() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // Missing required [project] section.
    std::fs::write(
        project.join("toylang.toml"),
        r#"[rust-dependencies]
rand = "0.8"
"#,
    )
    .unwrap();
    std::fs::write(project.join("main.toylang"), "fn main() {}\n").unwrap();

    let out = run_build(project);
    assert!(
        !out.status.success(),
        "toylangc build should fail on manifest missing [project];\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !project.join(".toylang-build").exists(),
        ".toylang-build should not be created when manifest parsing fails"
    );
}

#[test]
fn test_build_missing_source_fails() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    std::fs::write(
        project.join("toylang.toml"),
        r#"[project]
name = "ghost"
source = "does_not_exist.toylang"
"#,
    )
    .unwrap();
    // Intentionally do not create the source file.

    let out = run_build(project);
    assert!(
        !out.status.success(),
        "toylangc build should fail when source file is missing;\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn test_build_inside_another_workspace() {
    // Regression test: ensure the generated .toylang-build/Cargo.toml
    // declares itself as its own workspace root, so cargo doesn't walk up
    // and latch onto a parent [workspace] table. Without the `[workspace]`
    // line in write_cargo_toml, this test fails with:
    //   error: current package believes it's in a workspace when it's not
    // The 4 existing tempdir tests silently pass because a bare tempdir
    // has no parent workspace; this one synthesizes one on purpose.
    let outer = tempfile::tempdir().unwrap();
    let outer_path = outer.path();

    // Parent workspace manifest. Empty members list; we don't want to make
    // the project dir a member — the whole point is to prove that cargo
    // would otherwise auto-detect the nested Cargo.toml as belonging here.
    std::fs::write(
        outer_path.join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();

    let project = outer_path.join("inner_project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("toylang.toml"),
        r#"[project]
name = "inner_project"
source = "main.toylang"
"#,
    )
    .unwrap();
    std::fs::write(project.join("main.toylang"), "fn main() {}\n").unwrap();

    let out = run_build(&project);
    assert!(
        out.status.success(),
        "toylangc build should succeed when nested under a parent workspace;\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// Phase 7 crate #1: uuid — smoke test bridging Phase 5 (cargo
// resolves deps) to Phase 7 (toylang calls into deps). Shipped with
// @MBMRVZ (main must return void) and @RTMEIZ (every Rust type
// flowing through the type system must be use-imported).
#[test]
fn test_standalone_uuid() {
    run_standalone_test("uuid_test", "uuid ok");
}

// Phase 7 crate #2: indexmap — first 3-arg generic method call
// (IndexMap::new<i32, i32, RandomState>) via an S-fixed impl block.
#[test]
fn test_standalone_indexmap() {
    run_standalone_test("indexmap_test", "indexmap ok");
}

// Phase 7 crate #3: regex — exercises four features in composition:
// Phase 5 build, @UTAIRZ &str ABI, Phase 6 .unwrap() wrappers (first
// non-stdlib Result<T, E>), Phase 4 I/O. First Phase 7 smoke test
// whose RustStruct::method(args) shape tripped both the dispatch
// classifier and the inherent StaticCall codegen; fixed as @IVTDBTZ.
#[test]
fn test_standalone_regex() {
    run_standalone_test("regex_test", "regex ok");
}

// Phase 7 crate #5: serde_json — first integration test of a Rust
// free fn with an early-bound lifetime parameter. `from_str<'a, T:
// Deserialize<'a>>(s: &'a str)` ICEd rustc until @ELASZ centralized
// GenericArgs construction in `oracle::build_generic_args_for_item`
// with `ty::GenericArgs::for_item` synthesizing `re_erased` for
// lifetime slots.
#[test]
fn test_standalone_serde_json() {
    run_standalone_test("serde_json_test", "serde_json ok");
}

// Phase 7 crate #4: toml — first integration test of the `name<T>(args)`
// shape (use-imported generic free fn with an explicit type arg).
// Composes Phase 5 (build), Phase 2 (use-imported free fn), @UTAIRZ
// (&str via string literal), Phase 6 (unwrap on non-stdlib Result),
// and Phase 4 (Write::write_all).
#[test]
fn test_standalone_toml() {
    run_standalone_test("toml_test", "toml ok");
}

// Phase 7 crate #6: glob — free fn taking &str, returning Result
// without unwrap. First Phase 7 test to bind a Result and NOT call
// .unwrap() on it. Exercises @UTAIRZ &str ABI via string literal
// and Phase 2 free-fn dispatch.
#[test]
fn test_standalone_glob() {
    run_standalone_test("glob_test", "glob ok");
}

// Phase 7 crate #7: rand — zero-arg free fn returning an opaque
// ThreadRng. First Phase 7 test to return a non-Copy non-Result
// Rust type from a free fn and let Drop glue run naturally at
// end-of-main.
#[test]
fn test_standalone_rand() {
    run_standalone_test("rand_test", "rand ok");
}

// Phase 7 crate #8: reqwest — first standalone test to exercise
// Phase 5's detailed-dep path end-to-end (features = ["blocking"]
// gates an entire module, unlike uuid's cosmetic ["v4"]). Uses
// `Client::new()` rather than `blocking::get(url)` to avoid a
// novel generic-with-reference-type-arg shape and a network call;
// shape-identical to Uuid::new_v4() and thread_rng().
#[test]
fn test_standalone_reqwest() {
    run_standalone_test("reqwest_test", "reqwest ok");
}

// Phase 7 crate #9: clap — disproved the multi-week "blocked on
// impl Into<Str> synthetic generic" assumption. Command::new takes
// `impl Into<Str>`, which desugars to a synthetic type param that
// rustc exposes in `generics_of` alongside named ones. The call
// site names it explicitly — `Command::new<&str>("app")` — matching
// turbofish order. See @ELASZ's "Synthetic `impl Trait` slots"
// section for why the uniform-slot treatment in
// `build_generic_args_for_item` makes this work without special-
// casing.
#[test]
fn test_standalone_clap() {
    run_standalone_test("clap_test", "clap ok");
}

// Phase 7 follow-up probe: reqwest::blocking::get<T: IntoUrl>(url).
// Retires the "novel &T-type-arg shape deferred as follow-up" note
// from reqwest_test's commit bfa7355. `get` has an explicit named
// T: IntoUrl (not synthetic, unlike clap's `impl Into<Str>`), so the
// call site writes `get<&str>("")` as any other generic free fn —
// strictly simpler than clap's synthetic-slot case. Uses an empty
// string URL so IntoUrl's `Url::parse` fails synchronously with
// RelativeUrlWithoutBase before any network activity; the Result
// is bound but not unwrapped, matching glob's scope discipline.
#[test]
fn test_standalone_reqwest_get() {
    run_standalone_test("reqwest_get_test", "reqwest_get ok");
}
