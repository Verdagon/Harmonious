//! Architectural-property fence test (Phase F of the generic/non-generic
//! uniformity plan).
//!
//! Sky's CLAUDE.md compiler law: "non-generic is the degenerate case of
//! generic. Never branch on 'does this function/type have type parameters?'"
//!
//! Phases A/B/C of the uniformity plan removed the four implementation-pragma
//! sites that branched on `type_params.is_empty()` in discovery and
//! typecheck paths. This test grep-scans the discovery + typecheck files for
//! re-introduction of the asymmetry.
//!
//! The substituted-args helpers (resolve_caller_from_instance,
//! resolve_caller_from_type_args, monomorphize_type's TyKind walk) and the
//! type-arg-arity check at FnCall genuinely need to branch on type-param
//! count — those sites carry a `// arch-fence-allow: <reason>` marker so this
//! test ignores them.
//!
//! After Phase E Path 1 landed (fork patch 4, the debuginfo clamp), the
//! struct-shape ICE workaround was retired and the universal
//! `pub struct Foo<P...>(PhantomData<(P...)>);` shape is used at every N.
//! That eliminated the prior struct-shape divergence from this file's
//! scan list.
//!
//! Session 11 follow-up: the `__toylang_impl_*` and
//! `__toylang_accessor_*` extern "C" decls in stub_gen were investigated
//! and found vestigial — Sky's symbol_name override routes Rust callers
//! to the Sky-emitted symbols without needing forward declarations.
//! Removing both decl sites unified the generic and non-generic emission
//! paths in stub_gen, eliminating the previously-flagged "Phase D" sites.
//! What remains in the `extern "C" { ... }` block is the body-less toylang
//! fn decls (toylang source's "talk directly to existing Rust fn" syntax,
//! e.g., `fn println_int(x: i32);` binding to test_helpers's
//! `#[no_mangle] pub extern "C" fn println_int(...)`). Those decls are
//! orthogonal to toylang's own generics.
//!
//! The fence scans for both `type_params.is_empty()` and
//! `type_args.is_empty()` patterns.
//!
//! Out of scope for this fence:
//!   - oracle.rs / other helper modules — not on the discovery/typecheck
//!     path; their type-param branches are usually well-formedness checks.

use std::fs;

#[test]
fn no_unmarked_type_params_branch_in_discovery() {
    let scan = [
        "src/toylang/callbacks_impl.rs",
        "src/toylang/type_resolve.rs",
        "src/stub_gen.rs",
    ];
    let mut violations = Vec::new();
    for path in scan {
        let src = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));
        let lines: Vec<&str> = src.lines().collect();
        for (lineno, line) in lines.iter().enumerate() {
            let trips = line.contains("type_params.is_empty()")
                || line.contains("type_args.is_empty()");
            if trips {
                let prev = if lineno > 0 { lines[lineno - 1] } else { "" };
                if line.contains("arch-fence-allow") || prev.contains("arch-fence-allow") {
                    continue;
                }
                violations.push(format!("{}:{}: {}", path, lineno + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Phases A/B/C of the generic/non-generic uniformity plan closed the \
         `type_params.is_empty()` discovery/typecheck asymmetry. New \
         occurrences re-introduce it.\n\n\
         Either remove the branch (preferred — it should be uniform), or if \
         it's genuinely on the substituted-args path / a well-formedness arity \
         check, add a `// arch-fence-allow: <reason>` comment on the same \
         line.\n\n\
         Violations:\n  {}",
        violations.join("\n  ")
    );
}

/// F1 protection fence (2026-06-20).
///
/// The F1 investigation removed `#[inline(never)]` from three Sky-item
/// emission sites in `stub_gen.rs`: accessor methods, wrapper functions,
/// and trait-impl methods. The attribute was historically attached to
/// every Sky-item stub but its protections turned out to be obsolete
/// (patch 5 closes the share-generics gate; arch §F.1 says it's the
/// wrong layer for the LTO race fix; B12's vanilla-rustc concern is
/// gated by build.rs in v1). Empirical: with the attribute present,
/// Sky-export bodies didn't inline cross-crate at LTO — visible as
/// `b __lang_stubs::__toylang_main` tail-jumps in user_bin's main.
///
/// This fence asserts that `stub_gen.rs` carries `#[inline(never)]`
/// at exactly the count of known-good sites (today: 2 — the Phase-6
/// stdlib helpers `__toylang_option_unwrap` and
/// `__toylang_result_unwrap`, which retain the attribute for an
/// unrelated reason per §6.6.5: stable-symbol concern).
///
/// If someone re-introduces `#[inline(never)]` on a Sky-item site
/// without flipping the v2 precompiled-bodies trigger, this test
/// fails with the count delta. The expected behavior at the v2
/// trigger: re-introduce the attribute AND update this fence's
/// expected count in the same change, so the discipline is visible
/// in the PR.
#[test]
fn stub_gen_no_inline_never_on_sky_items() {
    let path = "src/stub_gen.rs";
    let src = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));

    // Count `#[inline(never)]` occurrences inside `quote!` / `parse_quote!`
    // bodies (the emission sites) vs in comments. We use a coarse
    // heuristic: any line that contains `#[inline(never)]` and is NOT
    // a comment counts.
    let mut emission_sites: Vec<(usize, &str)> = Vec::new();
    for (lineno, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        // Skip comment lines (// or /// markers).
        if trimmed.starts_with("//") {
            continue;
        }
        if line.contains("#[inline(never)]") {
            emission_sites.push((lineno + 1, line.trim()));
        }
    }

    // The expected count: 2 Phase-6 stdlib helpers
    // (`__toylang_option_unwrap`, `__toylang_result_unwrap`). When v2's
    // opt-in precompiled-bodies feature (arch §21.7) is built, the
    // Sky-item sites may re-introduce the attribute gated on a mode
    // flag. At that time, update the expected count here in the same
    // change as the stub_gen.rs change.
    const F1_EXPECTED_INLINE_NEVER_SITES: usize = 2;

    assert_eq!(
        emission_sites.len(),
        F1_EXPECTED_INLINE_NEVER_SITES,
        "F1 protection: stub_gen.rs has {} `#[inline(never)]` emission \
         site(s) but the F1 investigation locked in {} (Phase-6 stdlib \
         helpers only).\n\n\
         If a Sky-item site (accessor / wrapper / trait-impl method) \
         re-introduced the attribute, REVERT — the attribute blocks \
         LLVM's cross-language inliner from inlining Sky bodies through \
         to Rust callers' main. The original rationales for the attribute \
         on Sky-item stubs are obsolete: patch 5 closes the share-generics \
         gate, arch §F.1 says it's the wrong layer for the LTO race fix, \
         and B12's vanilla-rustc concern is gated by build.rs in v1.\n\n\
         If this is the v2 precompiled-bodies feature (arch §21.7) being \
         introduced, update F1_EXPECTED_INLINE_NEVER_SITES in this test \
         to match the new count.\n\n\
         Sites found:\n  {}",
        emission_sites.len(),
        F1_EXPECTED_INLINE_NEVER_SITES,
        emission_sites
            .iter()
            .map(|(ln, l)| format!("{}:{}: {}", path, ln, l))
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}
