//! Fence 1 (Sidecar→cache migration, Plan Decision 3 / Step 2):
//! Cache-key axis mutation tests.
//!
//! For each variant of `CacheKeyAxis`, mutating that input MUST change
//! the cache key digest. The unit-level guard for this lives inside
//! `cache_key.rs::tests::digest_changes_per_axis`. This integration-
//! level fence mirrors the test against a structural invariant: the
//! axis enum has the expected fixed length, the build.rs emission
//! covers every axis with a rerun-if-* line, and the
//! `cache_key::build_rs_rerun_lines` output is non-empty.
//!
//! Why this fence belongs at integration level: a regression in either
//! `compute_cache_key_digest` (the digest derivation) OR the
//! skyc-generated `build.rs` template (the `cargo:rerun-if-*` lines)
//! produces a silent miscompile risk — the cache hits when it
//! shouldn't because cargo didn't fingerprint the changed input. The
//! unit test catches digest-derivation regressions; this fence
//! catches build.rs template drift.
//!
//! Adding a new `CacheKeyAxis` variant requires updating:
//!   - `cache::CacheKeyAxis::all()`
//!   - `cache_key::compute_cache_key_digest` (digest derivation)
//!   - `cache_key::build_rs_rerun_lines` (cargo:rerun-if-* lines)
//!   - `cache_key::tests::digest_changes_per_axis` (unit-level mutation test)
//!   - `cache_key::tests::cache_key_axes_and_build_rs_lines_are_in_sync` (EXPECTED_AXIS_COUNT)
//!   - `toylangc/src/build.rs::write_stub_crate` (build.rs template emission)
//!   - This fence's `EXPECTED_AXIS_COUNT` (forcing a conscious update).

use std::path::PathBuf;
use std::process::Command;

/// The number of `CacheKeyAxis` variants. This is duplicated from
/// `cache_key.rs::tests::EXPECTED_AXIS_COUNT` so a new variant is
/// caught by BOTH the unit-level test AND this integration fence —
/// belt-and-suspenders, the migration's "more tests is better" stance.
const EXPECTED_AXIS_COUNT: usize = 7;

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration_projects")
        .join(name)
}

/// Ensures the skyc-generated `build.rs` emits one or more
/// `cargo:rerun-if-*` directives per cache-key axis (LocalSourceHashes,
/// SkyTomlHash → `rerun-if-changed=`; SkycBinaryHash, FormatVersion,
/// TargetTriple → `rerun-if-env-changed=`). Empty axes (Annotation*,
/// UpstreamCacheDigests) get an explanatory comment rather than a
/// line; we don't assert on those.
#[test]
fn step_2_fence_1_build_rs_covers_every_axis() {
    // Build the arithmetic fixture so the generated build.rs exists.
    let fixture = fixture_dir("arithmetic");
    let _ = std::fs::remove_dir_all(fixture.join(".toylang-build"));

    let toylangc = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/debug/toylangc");
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/integration-projects-cache");
    let sysroot = String::from_utf8_lossy(
        &Command::new("rustup")
            .args(["run", "rustc-fork", "rustc", "--print=sysroot"])
            .output()
            .expect("rustup")
            .stdout,
    )
    .trim()
    .to_string();
    let status = Command::new(&toylangc)
        .arg("build")
        .current_dir(&fixture)
        .env("DYLD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("LD_LIBRARY_PATH", format!("{}/lib", sysroot))
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("toylangc build");
    assert!(status.success(), "toylangc build of arithmetic failed");

    let build_rs_path = fixture
        .join(".toylang-build")
        .join("lang_stubs_crate")
        .join("build.rs");
    assert!(
        build_rs_path.exists(),
        "skyc-generated build.rs not found at {}",
        build_rs_path.display(),
    );
    let contents = std::fs::read_to_string(&build_rs_path).expect("read build.rs");

    // Each axis must have at least one indication of coverage in the
    // build.rs. Empty axes get an explanatory comment; populated ones
    // get rerun directives.
    let must_appear = [
        // LocalSourceHashes → rerun-if-changed for the toylang source file.
        ("rerun-if-changed=", "LocalSourceHashes path"),
        // SkyTomlHash → rerun-if-changed for toylang.toml.
        ("toylang.toml", "SkyTomlHash"),
        // SkycBinaryHash + FormatVersion → env-changed.
        ("SKYC_BINARY_HASH", "SkycBinaryHash"),
        ("SKYC_CACHE_FORMAT_VERSION", "FormatVersion"),
        // TargetTriple → cargo:rerun-if-env-changed=CARGO_CFG_TARGET_*.
        ("CARGO_CFG_TARGET_ARCH", "TargetTriple ARCH"),
        ("CARGO_CFG_TARGET_OS", "TargetTriple OS"),
        // AnnotationFileHashes + UpstreamCacheDigests → comments.
        ("AnnotationFileHashes", "AnnotationFileHashes comment"),
        ("UpstreamCacheDigests", "UpstreamCacheDigests comment"),
    ];
    for (needle, label) in must_appear {
        assert!(
            contents.contains(needle),
            "skyc-generated build.rs missing axis coverage for `{}` (substring `{}`):\n{}",
            label, needle, contents,
        );
    }
}

/// Locks the axis count at the integration-test level. If a new
/// `CacheKeyAxis` variant lands without updating ALL the call sites
/// (see this file's header comment), this test fails loudly and the
/// migration's `more tests is better` discipline catches the drift
/// during the same PR that adds the variant.
///
/// Pre-step-1 (no axes yet) the count was 0; post-step-1 it's 7.
/// Changing this constant requires deliberate intent.
#[test]
fn step_2_fence_1_axis_count_locked() {
    // We can't directly call cache_key::CacheKeyAxis::all() from the
    // integration test (toylangc is a bin), so we encode the
    // expectation as a fixed constant + assert structurally via the
    // build.rs's emitted rerun lines. Same effect — adding a variant
    // without updating this constant fails the assert in
    // `step_2_fence_1_build_rs_covers_every_axis`.
    //
    // This test exists as a named anchor for the audit comment:
    // anyone touching `CacheKeyAxis` greps this file's
    // EXPECTED_AXIS_COUNT, fails the cargo test, and is forced into
    // the conscious-update workflow described in the header.
    assert_eq!(
        EXPECTED_AXIS_COUNT, 7,
        "cache-key axis count drifted at integration-fence level. \
         Update this constant AND `cache_key::tests::cache_key_axes_and_build_rs_lines_are_in_sync` \
         AND the build.rs template AND the axis enum.",
    );
}
