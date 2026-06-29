//! Fence 4 (Sidecar→cache migration, Step 2): trait-impl-on-Sky-type
//! reaches body correctly via the cache load path.
//!
//! The migration's load-bearing case 6 scenario (per arch §2.6 /
//! §8.9.5): a Sky-defined type `Box` in an upstream lib impls a Rust
//! trait `Clone`; downstream code calls `duplicate<Box>(&b)` which
//! reaches `<Box as Clone>::clone` via rustc's cascade discovery.
//! Under the cache model, the consumer must read the upstream's
//! trait-impl method metadata from the cache file (not the sidecar)
//! and produce a correct universe so `monomorphize_type` resolves
//! `Box` and `consumer_fill_modules` emits the impl-method body.
//!
//! The `case6_app` fixture exercises exactly this path. The
//! integration suite (`integration_projects.rs`) runs it under the
//! default dual-path / cache-primary configuration; this fence is a
//! named anchor that makes the case6 → cache-path relationship
//! grep-discoverable for future readers.
//!
//! On regression: cache-load misses trait-impl methods → user-bin
//! compile can't resolve `<Box as Clone>::clone` → link error or
//! runtime panic at the `unreachable!()` stub body. The expected
//! output is `42`; any other value (or non-zero exit) indicates the
//! cache scope assumption is silently wrong.

use std::path::PathBuf;
use std::process::Command;

fn toylangc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/debug/toylangc")
}

#[test]
fn step_2_fence_4_case6_trait_impl_via_cache_path() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration_projects/case6_app");
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/cache-fence4");
    let _ = std::fs::remove_dir_all(fixture.join(".toylang-build"));
    let _ = std::fs::remove_dir_all(&target);

    let sysroot = String::from_utf8_lossy(
        &Command::new("rustup")
            .args(["run", "rustc-fork", "rustc", "--print=sysroot"])
            .output()
            .expect("rustup")
            .stdout,
    )
    .trim()
    .to_string();

    // Build under Step 2 default (cache-primary, no sidecar fallback
    // unless explicitly forced). The cache path is the only path
    // populating the universe.
    let status = Command::new(toylangc_binary())
        .arg("build")
        .current_dir(&fixture)
        .env("DYLD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("LD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("CARGO_TARGET_DIR", &target)
        .status()
        .expect("toylangc build");
    assert!(
        status.success(),
        "case6_app build under cache-primary failed — \
         trait-impl-on-Sky-type cache load may be broken",
    );

    let bin = target.join("debug/case6_app");
    let out = Command::new(&bin)
        .env("DYLD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .output()
        .expect("run case6_app binary");
    assert!(
        out.status.success(),
        "case6_app binary failed to run; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "42",
        "case6_app expected `42`, got `{}` — Sky trait-impl method body \
         was not emitted correctly via the cache load path",
        stdout.trim(),
    );
}
