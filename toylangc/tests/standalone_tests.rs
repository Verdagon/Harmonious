use std::path::PathBuf;
use std::process::Command;

fn toylangc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_toylangc"))
}

fn sysroot_lib() -> String {
    let out = Command::new("rustup")
        .args(["run", "rustc-fork", "rustc", "--print=sysroot"])
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

#[test]
fn test_standalone_uuid() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/uuid_test");

    // Clean any previous build output so the test is hermetic.
    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let build_out = run_build(&project);
    assert!(
        build_out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let bin = build_dir.join("target/debug/uuid_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run uuid_test binary");
    assert!(
        run.status.success(),
        "uuid_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("uuid ok"),
        "expected 'uuid ok' in stdout, got: {}",
        stdout,
    );
}

#[test]
fn test_standalone_indexmap() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/indexmap_test");

    // Clean any previous build output so the test is hermetic.
    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let build_out = run_build(&project);
    assert!(
        build_out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let bin = build_dir.join("target/debug/indexmap_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run indexmap_test binary");
    assert!(
        run.status.success(),
        "indexmap_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("indexmap ok"),
        "expected 'indexmap ok' in stdout, got: {}",
        stdout,
    );
}

// Per @IVTDBTZ — exercises four features in composition: Phase 5 build,
// @UTAIRZ &str ABI, Phase 6 .unwrap() wrappers (first non-stdlib
// Result<T, E>), Phase 4 I/O. First Phase 7 smoke test whose
// RustStruct::method(args) shape tripped both the dispatch classifier
// and the inherent StaticCall codegen; see
// docs/arcana/InherentVsTraitDispatchByType-IVTDBTZ.md.
#[test]
fn test_standalone_regex() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/standalone/regex_test");

    let build_dir = project.join(".toylang-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir).unwrap();
    }

    let build_out = run_build(&project);
    assert!(
        build_out.status.success(),
        "toylangc build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    let bin = build_dir.join("target/debug/regex_test");
    assert!(bin.exists(), "expected binary at {}", bin.display());

    let run = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", sysroot_lib())
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("failed to run regex_test binary");
    assert!(
        run.status.success(),
        "regex_test exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("regex ok"),
        "expected 'regex ok' in stdout, got: {}",
        stdout,
    );
}
