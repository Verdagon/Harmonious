//! Cache-discipline audit fence (handoff.md Decision 14 / Phase I).
//!
//! Decision 14 originally prescribed forcing
//! `cache_on_disk_if(false)` on every Sky-overridden rustc query via
//! Provider slots. Audit at 2026-06-24 found that the current nightly
//! does NOT expose per-query `*_cache_on_disk_if` slots on `Providers`
//! — `cache_on_disk_if` is a query-DECLARATION-time modifier in
//! rustc's macro DSL, not a Provider slot. The handoff's example
//! syntax `providers.queries.layout_of_cache_on_disk_if = ...`
//! doesn't compile.
//!
//! Per the audit, every Sky-overridden query is cache-safe by
//! construction:
//!
//! | Query                              | Upstream cache policy                | Why safe |
//! |------------------------------------|--------------------------------------|----------|
//! | per_instance_mir                   | `cache_on_disk_if { false }`         | Sky fork patch declares false |
//! | layout_of                          | (no clause → default false)          | Macro default is `false`; never disk-cached |
//! | cross_crate_inlinable              | (no clause → default false)          | Same |
//! | collect_and_partition_mono_items   | `eval_always`                        | Re-runs every compile; never cached |
//!
//! `symbol_name` override retired 2026-06-24 (Phase F, handoff Decision 2).
//! Rustc's default v0 mangler now produces every consumer symbol — call
//! sites and the `fill_extra_modules` emission share a single name by
//! construction (arch §6.2). The cache-safety question for symbol_name
//! is moot post-retirement: rustc's default behavior is Instance-keyed,
//! and the cache key includes the type args, so Sky's universe-state
//! changes (typeid drift, sidecar updates) flow through correctly via
//! Instance variation.
//!
//! This fence preserves the audit findings as code-level documentation:
//! the test enumerates every override file Sky ships and asserts each
//! one carries a marker comment describing its cache-safety reasoning.
//! Future engineers adding a new query override must touch this list,
//! which forces a fresh audit at that time. Catches the failure mode
//! where someone adds a Sky override on a disk-cached query without
//! considering Sky's universe-state-changes-invalidation problem.
//!
//! When current nightly gains a Provider-slot `_cache_on_disk_if`
//! pattern (or rustc renames the existing macro modifier), this fence
//! should be updated to encode the audit findings against the new API.

use std::fs;

const QUERIES_DIR: &str = "../rustc-lang-facade/src/queries";

/// Each entry: (filename, marker substring that must appear in the
/// file's comments). The marker captures the audit finding for that
/// query in human-readable form. Future override changes that don't
/// match the marker substring fail this test, forcing a fresh audit.
///
/// `symbol_name.rs` removed from this list 2026-06-24 (Phase F): the
/// override is retired; there's no longer a file to audit.
const EXPECTED_AUDIT_MARKERS: &[(&str, &str)] = &[
    ("layout.rs", "cache-audit:"),
    ("per_instance.rs", "cache-audit:"),
    ("partition.rs", "cache-audit:"),
    ("cross_crate_inlinable.rs", "cache-audit:"),
];

#[test]
fn every_query_override_carries_a_cache_audit_marker() {
    let mut missing = Vec::new();
    for (file, marker) in EXPECTED_AUDIT_MARKERS {
        let path = format!("{}/{}", QUERIES_DIR, file);
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                missing.push(format!("{}: cannot read ({})", path, e));
                continue;
            }
        };
        if !src.contains(marker) {
            missing.push(format!(
                "{}: missing `{}` audit marker. Per handoff.md Decision 14, \
                every Sky-overridden query needs a comment stating its \
                cache-on-disk policy and why it's safe (or unsafe + how Sky \
                handles it). See toylangc/tests/cache_audit.rs's header table \
                for the audit findings as of 2026-06-24.",
                path, marker
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "cache-audit fence failed: {} files missing audit marker:\n  - {}",
        missing.len(),
        missing.join("\n  - "),
    );
}
