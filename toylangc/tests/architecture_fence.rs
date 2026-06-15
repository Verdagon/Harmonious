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
//! Two remaining stub_gen.rs sites are gated by Rust syntax (Phase D):
//! the two `extern "C" { pub fn ... }` block conditionals — `extern "C"`
//! doesn't permit generic items. These carry
//! `// arch-fence-allow: extern-C-cannot-be-generic` markers.
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
            if line.contains("type_params.is_empty()") {
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
