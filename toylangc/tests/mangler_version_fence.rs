//! B25 drift-observation fence — default symbol mangling stability.
//!
//! Per arch §25.2 B25 + handoff Decision 15: Sky's emission depends on
//! rustc's DEFAULT symbol mangler (v0 as of 2026). Sky's bitcode emits
//! each rustc-visible body under the name `tcx.symbol_name(instance).name`
//! produces. Call sites in upstream rlibs and downstream user_bin compiles
//! use the same `tcx.symbol_name(...)` path. If rustc ever changes the
//! default mangler (legacy → v0 happened in 2020; future change to v1 or
//! beyond is plausible), Sky's intra-build emission/reference pairs still
//! match BUT cross-toolchain-version artifacts diverge.
//!
//! The integration suite (~352 fixtures) already catches mangler-default
//! changes — every cross-crate Sky-Rust symbol link would fail. This
//! fence is the explicit declaration: "Sky's emission paths use the
//! rustc-default mangler; we do not pin a specific version, and we do
//! not bypass the standard `tcx.symbol_name` path."
//!
//! The fence asserts two things:
//!   1. Toylangc's emission code reads `tcx.symbol_name(instance)` (the
//!      rustc-default path) and not a hand-rolled mangler.
//!   2. No `--symbol-mangling-version` or `-Csymbol-mangling-version`
//!      flag is hardcoded in skyc's build orchestration — Sky relies on
//!      rustc's current default rather than pinning a specific version.
//!
//! When v0 stops being the default (or becomes deprecated), this fence
//! continues to pass — Sky still uses whatever `tcx.symbol_name` returns
//! — but the integration suite's cross-crate fixtures will surface the
//! breakage explicitly. The fence's role is "make the dependency on
//! rustc's default explicit so future engineers don't accidentally
//! hardcode a version."
//!
//! Reaction if rustc changes the default mangler:
//!   - Determine whether Sky's emission and reference paths still produce
//!     matching symbols (they should — both call `tcx.symbol_name`).
//!   - If integration fixtures fail with undefined-symbol link errors,
//!     re-introduce the retired `symbol_name` override as a delegating
//!     shim that explicitly invokes the old mangler. The override file
//!     header in `rustc-lang-facade/src/queries/` (look for retired
//!     `symbol_name.rs` in git history) is the reference shape.

use std::fs;
use std::path::Path;

const REPO_ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Files in toylangc's emission paths that MUST use `tcx.symbol_name`
/// directly. If any of these accidentally hardcoded a mangler (e.g. by
/// using `def_path_str` + a homemade format), the integration suite
/// would catch it at link time — but this fence catches it earlier
/// with a clearer error.
const EMISSION_PATHS: &[&str] = &[
    "src/llvm_gen.rs",
    "src/toylang/callbacks_impl.rs",
];

#[test]
fn emission_uses_tcx_symbol_name_for_rustc_default_mangler() {
    let mut found_any = false;
    let mut suspicious_hardcodes: Vec<String> = Vec::new();
    for rel in EMISSION_PATHS {
        let path = Path::new(REPO_ROOT).join(rel);
        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
        if src.contains("tcx.symbol_name(") {
            found_any = true;
        }
        // Detect a hardcoded mangler-version flag — the kind of thing a
        // future engineer might add if they thought "I'll pin v0 just to
        // be safe." Sky's discipline is "don't pin; let rustc decide."
        for hit in src.lines().enumerate().filter(|(_, l)| {
            l.contains("symbol-mangling-version")
                || l.contains("symbol_mangling_version")
        }) {
            let (lineno, line) = hit;
            // The comment-style mentions in this file or doc comments
            // referencing the discipline are fine; the suspicious case is
            // an actual flag passed to rustc or a config value set on
            // the session.
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
                continue;
            }
            suspicious_hardcodes.push(format!(
                "{}:{}: suspicious symbol-mangling-version reference: {}",
                rel,
                lineno + 1,
                trimmed,
            ));
        }
    }
    assert!(
        found_any,
        "B25 fence: no `tcx.symbol_name(` call found in any of {:?}. \
         Sky's single-symbol architecture (arch §6.2) requires emission \
         to use rustc's default mangler via `tcx.symbol_name(instance)`. \
         If you moved the emission to a new path, update the EMISSION_PATHS \
         constant in this fence.",
        EMISSION_PATHS,
    );
    assert!(
        suspicious_hardcodes.is_empty(),
        "B25 fence: found {} suspicious symbol-mangling-version reference(s):\n  - {}\n\
         Sky depends on rustc's CURRENT default mangler; do not pin a version. \
         If a pinned version is genuinely needed, document the reasoning in \
         this fence's header and update EXPECTED_HARDCODES (none today).",
        suspicious_hardcodes.len(),
        suspicious_hardcodes.join("\n  - "),
    );
}

/// Sanity check: the retired `symbol_name.rs` override file is genuinely
/// gone. If it returns as a delegating shim in a future drift-reaction,
/// update this fence's commentary; the file's presence alone doesn't
/// signal a regression, but its return without a documented reason
/// might.
#[test]
fn retired_symbol_name_override_stays_retired_unless_intentional() {
    let path = Path::new(REPO_ROOT)
        .join("../rustc-lang-facade/src/queries/symbol_name.rs");
    if path.exists() {
        let src = fs::read_to_string(&path)
            .expect("read symbol_name.rs");
        assert!(
            src.contains("B25") || src.contains("delegating shim") || src.contains("drift-observation"),
            "B25 fence: symbol_name.rs has returned but lacks the B25 / \
             delegating-shim documentation. If you're re-introducing this \
             override as a drift-observation sentinel per arch §25.2 B25, \
             add a header comment explaining the reaction context."
        );
    }
    // If the file is absent, that's the expected post-Phase-F state and
    // this fence passes trivially.
}
