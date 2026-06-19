//! Glob-reexport marker-trap fence (arch doc §4.5 / Session 11 historical
//! regression).
//!
//! Every Sky stub rlib emits `pub const __SKY_STUBS_MARKER: () = ();` at its
//! crate root, and the facade's `is_from_lang_stubs(tcx, def_id)` predicate
//! walks `module_children` to detect it. Toylang's user-bin shim — and any
//! downstream Sky crate that does `use sky_lib::*;` — re-exports the
//! upstream stub rlib's marker into its own crate root. A naive marker
//! check that only looked at the symbol *name* would see the re-export and
//! incorrectly classify the downstream as a stub rlib. The partitioner
//! override would then filter `fn main` out of codegen and the link would
//! fail with a missing `_main`.
//!
//! The fix Session 11 shipped is a one-line parentage check inside
//! `crate_has_sky_marker`:
//!
//! ```ignore
//! match c.res.expect_non_local::<DefId>() {
//!     Res::Def(_, def_id) if def_id.krate == cnum => true,
//!     _ => false,
//! }
//! ```
//!
//! The check is implicitly exercised by every integration project (toylang's
//! `stub_gen` emits `use __lang_stubs::*;` at the user-bin crate root, so
//! every passing test depends on the parentage check rejecting the glob
//! re-export). This fence is a *named* regression guard: it locates the
//! parentage check in source and asserts the discriminating clause is still
//! present. A refactor that silently removes the `def_id.krate == cnum`
//! clause — preserving the surrounding code shape but breaking semantics —
//! would still pass every integration test today only if the project
//! happened not to hit a downstream-glob path, which is exactly the kind
//! of latent regression this fence catches *before* an integration project
//! eventually surfaces it.

use std::fs;
use std::path::Path;

/// Strip Rust line + block comments. Same shape as
/// `toylangc/tests/decoupling_fence.rs::strip_comments`.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut in_block = false;
    while i < bytes.len() {
        if in_block {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
            } else {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_block = true;
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

#[test]
fn crate_has_sky_marker_keeps_parentage_check() {
    let path = Path::new("src/lib.rs");
    assert!(
        path.exists(),
        "fence must run from the rustc-lang-facade crate root (looking for ./{})",
        path.display()
    );
    let src = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
    let code = strip_comments(&src);

    // Locate the `crate_has_sky_marker` function body and slice out only its
    // span. Scanning the whole file would false-positive on doc-comment
    // examples elsewhere (which the comment strip already handles) but the
    // function-scoped check is tighter — it asserts the parentage clause
    // lives inside the right function.
    let needle = "fn crate_has_sky_marker";
    let start = code
        .find(needle)
        .unwrap_or_else(|| panic!(
            "could not locate `{}` in src/lib.rs — the marker-detection \
             function has been renamed or removed. If so, update this fence \
             accordingly; do not delete the parentage check itself.",
            needle,
        ));
    // The function body extends to the first balanced closing brace at depth 0.
    let after_signature = &code[start..];
    let body_start = after_signature
        .find('{')
        .expect("malformed function: no opening brace after signature");
    let mut depth = 0i32;
    let mut end_off = body_start;
    for (i, ch) in after_signature.char_indices().skip(body_start) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end_off = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        end_off > body_start,
        "could not find the end of `crate_has_sky_marker`'s body"
    );
    let body = &after_signature[body_start..end_off];

    // The parentage check must be present in some recognisable form. We
    // accept any of the historical shapes — what matters is that a
    // discriminator binds a `DefId` and tests its `.krate` field against the
    // crate being walked (`cnum`). New rewrites that change the variable
    // names should update this fence to match.
    let candidates: &[&str] = &[
        "def_id.krate == cnum",
        "def_id.krate == c_num",
        ".krate == cnum",
    ];
    let parentage_ok = candidates.iter().any(|sig| body.contains(sig));

    assert!(
        parentage_ok,
        "Marker-parentage fence: `crate_has_sky_marker` no longer contains a \
         recognisable parentage check (looked for any of {:?} in its body).\n\n\
         Without the parentage check, every downstream Sky crate that does \
         `use sky_lib::*;` (toylang's user-bin shim is the canonical case) \
         gets its imported `__SKY_STUBS_MARKER` re-export mistaken for a \
         local one. The partitioner override then filters that crate's \
         `fn main` out of codegen and the link fails with a missing `_main`.\n\n\
         Arch doc reference: §4.5. Historical regression: toylang Session 11.\n\n\
         If you've genuinely refactored the predicate (e.g., renamed `cnum` \
         to something else), update the candidate list above to match — do \
         NOT silently drop the check.",
        candidates,
    );
}
