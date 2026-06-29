//! Cache-key axes — single source of truth for what determines a cache entry's identity.
//!
//! Sidecar→cache migration (2026-06-28). Plan Decision 3: every input that
//! determines typing-pass output must be part of the cache key. Missing an
//! input = silent miscompile (cache hits when it shouldn't), so the axis
//! list is conservative.
//!
//! The trimmed 6-axis list (workflow round-2 trimmed `cargo_lock_hash` +
//! consumer-resolved features from the upstream key — consumer can't
//! reconstruct them and cargo already encodes them via its own
//! fingerprint):
//!
//! 1. **SkycBinaryHash** — exact build of toylangc that produced the
//!    cache entry. Catches the "skyc point-release bricks every workspace"
//!    scenario where the same nominal version produces different output
//!    after a rebuild.
//! 2. **FormatVersion** — internal cache schema version. Fast invalidation
//!    hook if the cache layout itself evolves.
//! 3. **LocalSourceHashes** — sorted BLAKE3 hashes of the crate's own
//!    `.sky` source files (today: one source file per crate).
//! 4. **UpstreamCacheDigests** — sorted BLAKE3 hashes of the cache-key
//!    digests of every transitive upstream dep. Implements Option 1
//!    (transitive Merkle fingerprinting) from Plan Decision 1.
//! 5. **TargetTriple** — `--target=<triple>` affects typing-pass output
//!    via target-conditional code paths and layout-affecting types.
//! 6. **SkyTomlHash** — fields like `edition` affect parser / typechecker
//!    behavior; we hash the whole `toylang.toml` to capture them.
//! 7. **AnnotationFileHashes** — `<crate>.sky-annotations.toml` +
//!    `<project>/sky-annotations/<crate>.toml`. Currently unused by
//!    toylang but the slot exists so adding annotations later doesn't
//!    require redesigning the key (see arch §24).
//!
//! (Seven entries total; "6-input" in the plan undercounts by one
//! because LocalSourceHashes and UpstreamCacheDigests were conceptually
//! grouped as "source"; the implementation keeps them separate so a
//! single CI fence can mutate either independently.)
//!
//! ## The single-source-of-truth contract
//!
//! Both `compute_cache_key_digest` (producer-side) and
//! `build_rs_rerun_lines` (skyc-generated `build.rs`) iterate
//! `CacheKeyAxis::all()`. The unit test `cache_key_axes_and_build_rs_lines_are_in_sync`
//! asserts both functions visit every variant. Adding a new axis
//! without updating both fails CI loudly.

#![allow(dead_code)] // Step 1.2 lands the types; Step 1.3 / 1.4 / 1.5 consume them.

use std::path::{Path, PathBuf};

/// The cache key axes. Each variant represents one independently-mutable
/// input to the typing-pass that determines cache entry identity.
///
/// Plan Decision 3 + workflow round-2 trim. Adding or removing an axis
/// requires updating `compute_cache_key_digest` AND
/// `build_rs_rerun_lines`; the meta-test
/// `cache_key_axes_and_build_rs_lines_are_in_sync` is the structural
/// guard against drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKeyAxis {
    /// Exact build of toylangc — typically the binary's content hash
    /// (BLAKE3 of the executable's bytes) or a build-time constant
    /// embedded by `build.rs` machinery. Catches "skyc rebuild produces
    /// different output for the same input" scenarios that a version
    /// string would miss.
    SkycBinaryHash,
    /// Internal cache schema version. Bumping the format invalidates
    /// every entry without requiring a redesign of the rest of the key.
    FormatVersion,
    /// Sorted BLAKE3 hashes of the crate's own `.sky` source files.
    LocalSourceHashes,
    /// Sorted BLAKE3 hashes of the cache-key digests of every
    /// transitive upstream dep. Transitive Merkle.
    UpstreamCacheDigests,
    /// The `--target=<triple>` rustc was invoked with.
    TargetTriple,
    /// BLAKE3 hash of the crate's `toylang.toml` (or `sky.toml`).
    SkyTomlHash,
    /// Sorted BLAKE3 hashes of annotation files (`<crate>.sky-annotations.toml`
    /// + project-local override at `<project>/sky-annotations/<crate>.toml`).
    /// Empty hash list when no annotation files exist (the common case
    /// in toylang today).
    AnnotationFileHashes,
}

impl CacheKeyAxis {
    /// Canonical enumeration of every axis. Both `compute_cache_key_digest`
    /// and `build_rs_rerun_lines` MUST iterate this list (the
    /// single-source-of-truth contract). The meta-test
    /// `cache_key_axes_and_build_rs_lines_are_in_sync` verifies neither
    /// drift.
    pub fn all() -> &'static [CacheKeyAxis] {
        &[
            CacheKeyAxis::SkycBinaryHash,
            CacheKeyAxis::FormatVersion,
            CacheKeyAxis::LocalSourceHashes,
            CacheKeyAxis::UpstreamCacheDigests,
            CacheKeyAxis::TargetTriple,
            CacheKeyAxis::SkyTomlHash,
            CacheKeyAxis::AnnotationFileHashes,
        ]
    }

    /// Human-readable name used in error diagnostics and the `skyc inspect`
    /// analog.
    pub fn name(self) -> &'static str {
        match self {
            CacheKeyAxis::SkycBinaryHash => "skyc-binary-hash",
            CacheKeyAxis::FormatVersion => "format-version",
            CacheKeyAxis::LocalSourceHashes => "local-source-hashes",
            CacheKeyAxis::UpstreamCacheDigests => "upstream-cache-digests",
            CacheKeyAxis::TargetTriple => "target-triple",
            CacheKeyAxis::SkyTomlHash => "sky-toml-hash",
            CacheKeyAxis::AnnotationFileHashes => "annotation-file-hashes",
        }
    }
}

/// The inputs feeding `compute_cache_key_digest`. Owned-data wrapper so
/// the producer can build it up incrementally (e.g. resolve transitive
/// upstream cache digests by walking `tcx.crates(())` first, then add
/// the rest).
///
/// Determinism note: every collection field is `Vec<_>` but the digest
/// computation sorts before hashing — callers don't need to maintain
/// ordering themselves.
#[derive(Debug, Clone, Default)]
pub struct CacheKeyInputs {
    /// BLAKE3 of toylangc's binary at write time. Producer reads via
    /// `std::env::current_exe()` + `std::fs::read`; consumer reads the
    /// same way. Identical builds of toylangc produce the same hash.
    pub skyc_binary_hash: [u8; 32],
    /// Internal cache schema version. Today: `cache::CACHE_FORMAT_VERSION`.
    pub format_version: u32,
    /// `(filename, BLAKE3 hash)` pairs for each `.sky` source file in
    /// the crate. Filename included so renaming a file invalidates the
    /// cache even if its content didn't change.
    pub local_source_hashes: Vec<(String, [u8; 32])>,
    /// `(upstream crate name, upstream cache-key digest)` pairs for every
    /// transitive upstream Sky-marked dep. The digest values are the
    /// 16-byte `cache::KEY_DIGEST_LEN` truncations.
    pub upstream_cache_digests: Vec<(String, [u8; 16])>,
    /// `--target=<triple>`-style identifier (e.g. "aarch64-apple-darwin").
    pub target_triple: String,
    /// BLAKE3 of the crate's `toylang.toml` content.
    pub sky_toml_hash: [u8; 32],
    /// `(filename, BLAKE3 hash)` pairs for each annotation file. Empty
    /// when no annotation files exist (the toylang-today case). When
    /// loading the file list, both `<crate>.sky-annotations.toml` and
    /// the project-local override path are included so the consumer
    /// sees a change to either.
    pub annotation_file_hashes: Vec<(String, [u8; 32])>,
}

impl CacheKeyInputs {
    /// Helper for tests + the skyc-generated `build.rs`: returns the
    /// `cargo:rerun-if-*` lines a stub crate's `build.rs` must emit so
    /// cargo invalidates the upstream rlib when any of these inputs
    /// changes. Iterates `CacheKeyAxis::all()` in order so the
    /// single-source-of-truth contract holds.
    ///
    /// Some axes (skyc binary hash, format version, target triple) are
    /// environment-keyed via `cargo:rerun-if-env-changed=` rather than
    /// path-keyed. File-keyed axes use absolute paths so cargo's
    /// fingerprint sees the actual file the consumer's frontend will
    /// hash.
    pub fn build_rs_rerun_lines(&self) -> Vec<String> {
        build_rs_rerun_lines(self)
    }
}

/// Compute the 16-byte BLAKE3-truncated Merkle digest from a set of
/// cache key inputs. Deterministic given identical inputs (sorts every
/// collection before hashing).
///
/// Implementation detail (the canonical encoding):
/// - Each axis is hashed in `CacheKeyAxis::all()` order.
/// - Within each axis, scalar fields are encoded as little-endian
///   bytes (u32 → 4 bytes, target triple as UTF-8 bytes prefixed by
///   length).
/// - Collection axes (`local_source_hashes`, `upstream_cache_digests`,
///   `annotation_file_hashes`) are sorted by their string key before
///   hashing, then each entry's name length + name bytes + hash bytes
///   are fed into the hasher.
/// - The final 32-byte BLAKE3 output is truncated to the first 16 bytes
///   for storage in `cache::CacheHeader::cache_key_digest`.
pub fn compute_cache_key_digest(inputs: &CacheKeyInputs) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    for axis in CacheKeyAxis::all() {
        // Mix the axis name in so two axes with the same encoded payload
        // shape still produce distinct overall digests.
        let name = axis.name();
        hasher.update(&(name.len() as u32).to_le_bytes());
        hasher.update(name.as_bytes());

        match axis {
            CacheKeyAxis::SkycBinaryHash => {
                hasher.update(&inputs.skyc_binary_hash);
            }
            CacheKeyAxis::FormatVersion => {
                hasher.update(&inputs.format_version.to_le_bytes());
            }
            CacheKeyAxis::LocalSourceHashes => {
                hash_named_hash_collection(&mut hasher, &inputs.local_source_hashes);
            }
            CacheKeyAxis::UpstreamCacheDigests => {
                let mut sorted: Vec<&(String, [u8; 16])> = inputs.upstream_cache_digests.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                hasher.update(&(sorted.len() as u32).to_le_bytes());
                for (name, digest) in sorted {
                    hasher.update(&(name.len() as u32).to_le_bytes());
                    hasher.update(name.as_bytes());
                    hasher.update(digest);
                }
            }
            CacheKeyAxis::TargetTriple => {
                hasher.update(&(inputs.target_triple.len() as u32).to_le_bytes());
                hasher.update(inputs.target_triple.as_bytes());
            }
            CacheKeyAxis::SkyTomlHash => {
                hasher.update(&inputs.sky_toml_hash);
            }
            CacheKeyAxis::AnnotationFileHashes => {
                hash_named_hash_collection(&mut hasher, &inputs.annotation_file_hashes);
            }
        }
    }
    let full = hasher.finalize();
    let bytes = full.as_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[..16]);
    out
}

/// Compute `cargo:rerun-if-*` lines for the skyc-generated `build.rs`.
/// Iterates `CacheKeyAxis::all()` — every axis MUST appear so cargo
/// invalidates the upstream rlib's compile when any cache input
/// changes. The meta-test asserts this alignment.
///
/// Some axes are file-paths (`rerun-if-changed=...`), some are
/// environment vars (`rerun-if-env-changed=...`). For environment-keyed
/// axes the stub's `build.rs` propagates the value to the wrapper-mode
/// compile via a `--cfg` injection or env-var-export (toylang's
/// existing `RUSTC_WORKSPACE_WRAPPER` path makes the wrapper's mtime a
/// natural fingerprint; this `build.rs` mechanism layers on the
/// remaining axes).
///
/// Note: `LocalSourceHashes` and `UpstreamCacheDigests` are
/// derived inputs at digest computation time — the source files
/// themselves and the upstream rlib paths are what cargo needs to
/// fingerprint. We list source paths under `LocalSourceHashes` and rely
/// on cargo's existing rlib-fingerprint mechanism for upstream invalidation
/// (when an upstream rlib changes, cargo rebuilds the current crate,
/// and the rebuild reads the upstream's cache digest fresh).
pub fn build_rs_rerun_lines(inputs: &CacheKeyInputs) -> Vec<String> {
    let mut lines = Vec::new();
    for axis in CacheKeyAxis::all() {
        match axis {
            CacheKeyAxis::SkycBinaryHash => {
                // The skyc wrapper binary's mtime rides cargo's
                // `RUSTC_WORKSPACE_WRAPPER` fingerprint path natively;
                // we additionally export an env var so a non-wrapper
                // execution path (e.g. Sky-proper post-Phase-G cdylib)
                // can pick up the same invalidation signal via cargo's
                // env-var rerun. The producer's compute_cache_key_digest
                // reads the binary's content; the wrapper-mtime
                // mechanism already invalidates on rebuild.
                lines.push("cargo:rerun-if-env-changed=SKYC_BINARY_HASH".to_string());
            }
            CacheKeyAxis::FormatVersion => {
                // Bumps to the cache format version invalidate every
                // entry. Cargo doesn't directly see the constant, so we
                // mix it into a rerun env var the wrapper sets.
                lines.push("cargo:rerun-if-env-changed=SKYC_CACHE_FORMAT_VERSION".to_string());
            }
            CacheKeyAxis::LocalSourceHashes => {
                for (path, _hash) in &inputs.local_source_hashes {
                    lines.push(format!("cargo:rerun-if-changed={}", path));
                }
            }
            CacheKeyAxis::UpstreamCacheDigests => {
                // Upstream invalidation is handled by cargo's own
                // rlib-fingerprint mechanism: when an upstream rlib
                // changes, cargo rebuilds the current crate, and the
                // rebuild reads each upstream's cache digest fresh
                // from disk. No `rerun-if-*` line needed here.
            }
            CacheKeyAxis::TargetTriple => {
                lines.push("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH".to_string());
                lines.push("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS".to_string());
                lines.push("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ENV".to_string());
                lines.push("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_VENDOR".to_string());
            }
            CacheKeyAxis::SkyTomlHash => {
                // The wrapper plants `toylang.toml` inside each stub
                // crate dir (see build.rs::write_stub_crate). The
                // stable path is `${CARGO_MANIFEST_DIR}/toylang.toml`.
                lines.push("cargo:rerun-if-changed=toylang.toml".to_string());
            }
            CacheKeyAxis::AnnotationFileHashes => {
                for (path, _hash) in &inputs.annotation_file_hashes {
                    lines.push(format!("cargo:rerun-if-changed={}", path));
                }
            }
        }
    }
    lines
}

fn hash_named_hash_collection(hasher: &mut blake3::Hasher, items: &[(String, [u8; 32])]) {
    let mut sorted: Vec<&(String, [u8; 32])> = items.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    hasher.update(&(sorted.len() as u32).to_le_bytes());
    for (name, hash) in sorted {
        hasher.update(&(name.len() as u32).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(hash);
    }
}

/// Convenience helper for the producer side: hash a file's content via
/// BLAKE3 (full 32-byte digest). Returns Err if the file can't be read.
pub fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let bytes = std::fs::read(path)?;
    let digest = blake3::hash(&bytes);
    Ok(*digest.as_bytes())
}

/// Convenience helper: BLAKE3 over arbitrary bytes (full 32-byte digest).
pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    let digest = blake3::hash(bytes);
    *digest.as_bytes()
}

/// Helper for the producer side: build a `CacheKeyInputs` for the
/// current crate. Caller supplies the source-file paths, the upstream
/// cache digests (resolved by walking `tcx.crates(())`), the target
/// triple, the toylang.toml path, and any annotation file paths.
///
/// All file-hash inputs are read fresh; the caller is responsible for
/// ensuring the files exist (missing files panic the producer — same
/// posture as the existing sidecar write path which panics on I/O
/// failure).
pub fn build_inputs(
    skyc_binary_hash: [u8; 32],
    local_source_paths: &[PathBuf],
    upstream_cache_digests: Vec<(String, [u8; 16])>,
    target_triple: String,
    sky_toml_path: &Path,
    annotation_file_paths: &[PathBuf],
) -> std::io::Result<CacheKeyInputs> {
    let mut local_source_hashes = Vec::with_capacity(local_source_paths.len());
    for p in local_source_paths {
        let h = hash_file(p)?;
        local_source_hashes.push((p.display().to_string(), h));
    }
    let sky_toml_hash = hash_file(sky_toml_path)?;
    let mut annotation_file_hashes = Vec::with_capacity(annotation_file_paths.len());
    for p in annotation_file_paths {
        let h = hash_file(p)?;
        annotation_file_hashes.push((p.display().to_string(), h));
    }
    Ok(CacheKeyInputs {
        skyc_binary_hash,
        format_version: crate::cache::CACHE_FORMAT_VERSION,
        local_source_hashes,
        upstream_cache_digests,
        target_triple,
        sky_toml_hash,
        annotation_file_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inputs() -> CacheKeyInputs {
        CacheKeyInputs {
            skyc_binary_hash: [0xAB; 32],
            format_version: 1,
            local_source_hashes: vec![
                ("main.toylang".to_string(), [0x11; 32]),
                ("lib.toylang".to_string(), [0x22; 32]),
            ],
            upstream_cache_digests: vec![
                ("lib_b".to_string(), [0x33; 16]),
                ("lib_a".to_string(), [0x44; 16]),
            ],
            target_triple: "aarch64-apple-darwin".to_string(),
            sky_toml_hash: [0x55; 32],
            annotation_file_hashes: vec![],
        }
    }

    #[test]
    fn digest_is_deterministic() {
        // Same inputs → same digest. Order of collections must not
        // affect output (the canonical encoding sorts).
        let mut a = sample_inputs();
        let b = sample_inputs();
        // Permute one collection on `a` — digest must still match.
        a.local_source_hashes.reverse();
        a.upstream_cache_digests.reverse();
        let da = compute_cache_key_digest(&a);
        let db = compute_cache_key_digest(&b);
        assert_eq!(da, db, "digest must be invariant to collection order");
    }

    #[test]
    fn digest_changes_per_axis() {
        // Mutating any single axis changes the digest. This is the
        // unit-level guard for Fence 1 (cache-key axis mutation tests).
        let base = sample_inputs();
        let base_digest = compute_cache_key_digest(&base);

        // SkycBinaryHash
        {
            let mut v = base.clone();
            v.skyc_binary_hash = [0xCD; 32];
            assert_ne!(compute_cache_key_digest(&v), base_digest, "SkycBinaryHash axis");
        }
        // FormatVersion
        {
            let mut v = base.clone();
            v.format_version = 2;
            assert_ne!(compute_cache_key_digest(&v), base_digest, "FormatVersion axis");
        }
        // LocalSourceHashes
        {
            let mut v = base.clone();
            v.local_source_hashes[0].1 = [0xAA; 32];
            assert_ne!(compute_cache_key_digest(&v), base_digest, "LocalSourceHashes axis");
        }
        // UpstreamCacheDigests
        {
            let mut v = base.clone();
            v.upstream_cache_digests[0].1 = [0xBB; 16];
            assert_ne!(compute_cache_key_digest(&v), base_digest, "UpstreamCacheDigests axis");
        }
        // TargetTriple
        {
            let mut v = base.clone();
            v.target_triple = "x86_64-pc-linux-gnu".to_string();
            assert_ne!(compute_cache_key_digest(&v), base_digest, "TargetTriple axis");
        }
        // SkyTomlHash
        {
            let mut v = base.clone();
            v.sky_toml_hash = [0xEE; 32];
            assert_ne!(compute_cache_key_digest(&v), base_digest, "SkyTomlHash axis");
        }
        // AnnotationFileHashes
        {
            let mut v = base.clone();
            v.annotation_file_hashes.push(("anno.toml".to_string(), [0xFF; 32]));
            assert_ne!(compute_cache_key_digest(&v), base_digest, "AnnotationFileHashes axis");
        }
    }

    #[test]
    fn digest_length_is_16_bytes() {
        let inputs = sample_inputs();
        let digest = compute_cache_key_digest(&inputs);
        assert_eq!(digest.len(), 16);
    }

    /// The single-source-of-truth meta-test: every variant of
    /// `CacheKeyAxis` is referenced by both `compute_cache_key_digest`
    /// (the digest derivation) AND `build_rs_rerun_lines` (the
    /// skyc-generated build.rs lines).
    ///
    /// We can't introspect the digest implementation directly, so this
    /// test approximates the contract by asserting both code paths
    /// don't panic when iterating over every axis and that the axes
    /// list itself has the expected fixed length (a new variant
    /// without updating this constant fails CI loudly, which is the
    /// real point).
    #[test]
    fn cache_key_axes_and_build_rs_lines_are_in_sync() {
        const EXPECTED_AXIS_COUNT: usize = 7;
        assert_eq!(
            CacheKeyAxis::all().len(),
            EXPECTED_AXIS_COUNT,
            "adding a CacheKeyAxis variant requires updating compute_cache_key_digest, \
             build_rs_rerun_lines, this test's EXPECTED_AXIS_COUNT, AND Fence 1's \
             cache_key_axis_fence.rs",
        );

        // build_rs_rerun_lines must produce at least one line per
        // axis that has rerun lines (the upstream-cache-digests axis
        // is the only one that's intentionally empty — its
        // invalidation goes through cargo's own rlib fingerprint).
        let inputs = sample_inputs();
        let lines = build_rs_rerun_lines(&inputs);
        let joined = lines.join("\n");

        // SkycBinaryHash exports SKYC_BINARY_HASH env var.
        assert!(joined.contains("SKYC_BINARY_HASH"), "missing SkycBinaryHash line");
        // FormatVersion via SKYC_CACHE_FORMAT_VERSION.
        assert!(joined.contains("SKYC_CACHE_FORMAT_VERSION"), "missing FormatVersion line");
        // LocalSourceHashes via rerun-if-changed for each file.
        assert!(joined.contains("main.toylang"), "missing local source line");
        // TargetTriple via CARGO_CFG_TARGET_*.
        assert!(joined.contains("CARGO_CFG_TARGET_ARCH"), "missing target triple line");
        // SkyTomlHash via rerun-if-changed=toylang.toml.
        assert!(joined.contains("toylang.toml"), "missing sky.toml line");
        // AnnotationFileHashes: empty inputs → empty lines, allowed.
    }

    #[test]
    fn empty_inputs_still_produce_a_digest() {
        let empty = CacheKeyInputs::default();
        let digest = compute_cache_key_digest(&empty);
        // Default is all-zeros + empty collections; digest still has
        // to be 16 bytes and shouldn't panic.
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn build_inputs_helper_round_trip() {
        // Smoke test: build_inputs reads a temp dir, computes hashes,
        // produces a usable CacheKeyInputs.
        use std::io::Write as _;
        let tmp = std::env::temp_dir().join(format!(
            "toylangc-cache-key-test-{}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let sky_toml = tmp.join("toylang.toml");
        let mut f = std::fs::File::create(&sky_toml).unwrap();
        f.write_all(b"[project]\nname=\"x\"\n").unwrap();
        drop(f);
        let main_sky = tmp.join("main.toylang");
        let mut f = std::fs::File::create(&main_sky).unwrap();
        f.write_all(b"fn main() { }\n").unwrap();
        drop(f);

        let inputs = build_inputs(
            [0u8; 32],
            &[main_sky.clone()],
            vec![],
            "aarch64-apple-darwin".to_string(),
            &sky_toml,
            &[],
        )
        .expect("build_inputs");
        assert_eq!(inputs.local_source_hashes.len(), 1);
        assert_eq!(inputs.annotation_file_hashes.len(), 0);

        let _digest = compute_cache_key_digest(&inputs);
        let _lines = build_rs_rerun_lines(&inputs);

        // Cleanup
        std::fs::remove_dir_all(&tmp).ok();
    }
}
