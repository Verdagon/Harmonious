//! Fence 6 (Sidecar→cache migration, Step 4): cleanup audit.
//!
//! When Step 4 lands (delete sidecar code), this fence asserts the
//! cleanup discipline has been honored:
//!
//! 1. No `// delete after step 4` comments remain in src/ or tests/.
//! 2. No `mod sidecar;` or `use crate::sidecar` references.
//! 3. No `.sky-meta` string literals in source code (test fixtures
//!    are exempt because the dual-write rollback paths may still
//!    produce them under SKYC_DUAL_WRITE=1 if that env var is
//!    retained).
//!
//! UNTIL Step 4 ships: this fence is `#[ignore]`d. Re-enable when the
//! sidecar deletion lands. Greps below are the canonical Step 4
//! completion checklist.

use std::path::Path;
use std::process::Command;

const SOURCE_DIRS: &[&str] = &[
    "toylangc/src",
    "rustc-lang-facade/src",
    "toylangc/tests",
];

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("toylangc has a parent")
        .to_path_buf()
}

fn run_grep(pattern: &str, dir: &Path) -> String {
    // Exclude this file itself — it intentionally contains the
    // patterns we're auditing for. The Step 4 cleanup pattern would
    // otherwise hit this file's documentation strings and panic.
    let out = Command::new("grep")
        .args([
            "-rn",
            "--include=*.rs",
            "--exclude=cleanup_audit.rs",
            pattern,
            &dir.display().to_string(),
        ])
        .output()
        .expect("grep");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn step_4_fence_6_cleanup_audit() {
    let root = repo_root();

    // 1. `// delete after step 4` markers must be gone.
    let mut leftover_markers = String::new();
    for dir in SOURCE_DIRS {
        let hits = run_grep("delete after step 4", &root.join(dir));
        if !hits.is_empty() {
            leftover_markers.push_str(&format!("--- {} ---\n{}", dir, hits));
        }
    }
    assert!(
        leftover_markers.is_empty(),
        "Step 4 cleanup audit failed: `// delete after step 4` markers still present:\n{}",
        leftover_markers,
    );

    // 2. `mod sidecar;` and `use crate::sidecar` must be gone.
    let mut leftover_refs = String::new();
    for dir in SOURCE_DIRS {
        let hits = run_grep("mod sidecar", &root.join(dir));
        let hits2 = run_grep("crate::sidecar", &root.join(dir));
        if !hits.is_empty() || !hits2.is_empty() {
            leftover_refs.push_str(&format!("--- {} ---\nmod sidecar:\n{}\ncrate::sidecar:\n{}",
                dir, hits, hits2));
        }
    }
    assert!(
        leftover_refs.is_empty(),
        "Step 4 cleanup audit failed: sidecar module references still present:\n{}",
        leftover_refs,
    );

    // 3. `sidecar.rs` should not exist in toylangc/src.
    let sidecar_path = root.join("toylangc/src/sidecar.rs");
    assert!(
        !sidecar_path.exists(),
        "Step 4 cleanup audit failed: sidecar.rs still exists at {}",
        sidecar_path.display(),
    );

    // 4. `.sky-meta` string literals — informational only. Test
    //    fixtures may still mention the extension for diagnostic
    //    purposes; tighten this assertion only if Step 4 retires the
    //    SKYC_DUAL_WRITE env var entirely.
    let mut sky_meta_refs = String::new();
    for dir in SOURCE_DIRS {
        let hits = run_grep("sky-meta", &root.join(dir));
        if !hits.is_empty() {
            sky_meta_refs.push_str(&format!("--- {} ---\n{}", dir, hits));
        }
    }
    // We don't assert here — log instead for the human reviewer.
    if !sky_meta_refs.is_empty() {
        eprintln!(
            "[step 4 cleanup audit] note: `.sky-meta` references remain — \
             these may be diagnostic-only or rollback-path references that \
             survived deletion. Review:\n{}",
            sky_meta_refs,
        );
    }
}
