//! Fence 2 (Sidecar→cache migration, Step 2): cache determinism.
//!
//! Plan-locked invariant: identical input → byte-identical
//! `.sky-cache` output. Same shape as `test_s5_sidecar_determinism`
//! (`integration_projects.rs:1719`), retargeted from `.sky-meta`
//! bytes to `.sky-cache` bytes.
//!
//! Mechanism: build the `arithmetic` fixture twice into per-run
//! target dirs (`target/cache-det-a/`, `.../-b/`). Wipe both target
//! dirs AND each fixture's `.toylang-build/` between runs so the
//! producer-side write runs cold. Locate `lib__lang_stubs-*.sky-cache`
//! under each `debug/deps`, read both, `assert_eq!` byte-by-byte.
//!
//! Mismatch indicates a non-deterministic source has been introduced
//! into the typing pass or cache serialization layer (HashMap
//! iteration order, timestamps, host paths, etc.).
//!
//! Retired at Step 4 if/when the equivalence-with-sidecar gate is
//! gone; replaced by this fence which targets the cache bytes
//! directly.

use std::path::{Path, PathBuf};
use std::process::Command;

fn toylangc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/debug/toylangc")
}

fn build_into(fixture: &Path, target_dir: &Path) {
    let _ = std::fs::remove_dir_all(fixture.join(".toylang-build"));
    let _ = std::fs::remove_dir_all(target_dir);
    let sysroot = String::from_utf8_lossy(
        &Command::new("rustup")
            .args(["run", "rustc-fork", "rustc", "--print=sysroot"])
            .output()
            .expect("rustup")
            .stdout,
    )
    .trim()
    .to_string();
    let status = Command::new(toylangc_binary())
        .arg("build")
        .current_dir(fixture)
        .env("DYLD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("LD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .expect("toylangc build");
    assert!(
        status.success(),
        "toylangc build of fixture {} into {} failed",
        fixture.display(),
        target_dir.display(),
    );
}

fn find_one_cache_file(deps_dir: &Path, stem_prefix: &str) -> PathBuf {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(deps_dir).expect("read deps dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
            if fname.starts_with(stem_prefix) && fname.ends_with(".sky-cache") {
                hits.push(path);
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one `{}*.sky-cache` under {}; found {}: {:?}",
        stem_prefix,
        deps_dir.display(),
        hits.len(),
        hits,
    );
    hits.pop().unwrap()
}

#[test]
fn step_2_fence_2_cache_byte_determinism() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration_projects/arithmetic");
    let target_a = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/cache-det-a");
    let target_b = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/cache-det-b");

    // Run A.
    build_into(&fixture, &target_a);
    let cache_a = find_one_cache_file(&target_a.join("debug/deps"), "__lang_stubs");
    let bytes_a = std::fs::read(&cache_a).expect("read cache a");

    // Run B.
    build_into(&fixture, &target_b);
    let cache_b = find_one_cache_file(&target_b.join("debug/deps"), "__lang_stubs");
    let bytes_b = std::fs::read(&cache_b).expect("read cache b");

    // The two runs MUST produce byte-identical .sky-cache files. On
    // mismatch, panic with the first-differing index so the
    // diagnostic points at the offending byte (matches the sidecar
    // determinism fence's diagnostic style).
    if bytes_a != bytes_b {
        let n = bytes_a.len().min(bytes_b.len());
        let mut first_diff = None;
        for i in 0..n {
            if bytes_a[i] != bytes_b[i] {
                first_diff = Some(i);
                break;
            }
        }
        panic!(
            "cache files diverged between two clean builds:\n  \
             A = {} ({} bytes)\n  \
             B = {} ({} bytes)\n  \
             first differing byte: {:?}\n  \
             Likely cause: a non-deterministic source has been \
             introduced into the typing pass or cache serialization.",
            cache_a.display(),
            bytes_a.len(),
            cache_b.display(),
            bytes_b.len(),
            first_diff,
        );
    }
}
