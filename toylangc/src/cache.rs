//! Local sibling-cache (`.sky-cache`) format — types, header codec.
//!
//! Sidecar→cache migration (2026-06-28, design doc:
//! `tmp/claude-plan-2026-06-28-bd1a7f89.md`). Replaces `.sky-meta` sidecar
//! files. Same payload (bincode-serialized `ToylangRegistry`), different
//! header (carries a Merkle key digest for consumer-side verification),
//! and different storage location (`target/<triple>/<profile>/deps/`
//! adjacent to cargo's `.rlib`/`.rmeta`, NEVER shipped on crates.io).
//!
//! The on-disk file shape:
//!
//! ```text
//! offset  size   field
//! ------  ----   -----
//!   0      4     magic "SKYC"
//!   4      4     cache_format_version (u32 LE)
//!   8     16     cache_key_digest (BLAKE3-truncated to 16 bytes)
//!  24      8     payload_offset (u64 LE) = 64
//!  32      8     payload_length (u64 LE)
//!  40      8     payload_checksum (BLAKE3-trunc to 8 bytes, LE u64)
//!  48     16     reserved (zeroed; tolerated nonzero on read)
//!  64     N      payload (bincode-encoded ToylangRegistry)
//! ```
//!
//! All multi-byte integers little-endian; payload offset 8-byte aligned.
//! Header layout intentionally mirrors `sidecar::SidecarHeader` shape (so
//! a reader of one understands the other) but the magic bytes and the
//! key-digest field differ.
//!
//! Plan-locked decisions implemented here:
//! - **Decision 6a/6b**: cache file carries content, not a pointer; version
//!   identifier in the header's magic bytes path (not Cargo.toml metadata).
//! - **Decision 3 (cache key)**: the 16-byte `cache_key_digest` field
//!   stores the BLAKE3-truncated Merkle digest computed by
//!   `cache_key::compute_cache_key_digest`. Consumer compares the
//!   on-disk digest against its own freshly-computed expected digest
//!   before deserializing; mismatch produces a clear diagnostic, not a
//!   silent stale-data read.

#![allow(dead_code)] // Step 1.1 lands the types; Step 1.3 / 1.4 / 2 consume them.

use std::fmt;

use crate::toylang::registry::ToylangRegistry;

/// Magic number identifying a Sky cache file. ASCII "SKYC".
///
/// Distinct from the sidecar magic "SKYM" so an accidental cross-read
/// fails fast with a clear `BadMagic` error rather than silently
/// deserializing the wrong shape.
pub const CACHE_MAGIC: [u8; 4] = *b"SKYC";

/// Current cache format version. Pre-1.0 policy: strict match required.
/// Bump on any breaking change to the payload's serializable types or
/// the header layout. Under the cache model — unlike sidecars — bumping
/// this is cheap: cache-version-keyed entries become unreachable on the
/// next skyc invocation and get rebuilt on miss. No migration story
/// needs to exist.
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// Size of the fixed header in bytes. Payload begins at this offset
/// (64-byte alignment is required by the spec; the natural header end
/// is 48, and the remaining 16 bytes are reserved/padding to reach 64).
pub const HEADER_SIZE: usize = 64;

/// Byte length of the BLAKE3-truncated cache-key digest stored in the
/// header. 16 bytes = 128 bits; collision resistance well within Sky's
/// universe scale (§29.A.u128-typeids reasoning carries over).
pub const KEY_DIGEST_LEN: usize = 16;

/// Default payload offset: immediately after the fixed header.
pub const DEFAULT_PAYLOAD_OFFSET: u64 = HEADER_SIZE as u64;

/// Fixed-layout header at the start of every cache file. 64 bytes.
///
/// Fields are written little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheHeader {
    /// Cache binary format version. Pre-1.0: strict match.
    pub format_version: u32,
    /// BLAKE3-truncated Merkle digest of the cache-key axes inputs that
    /// produced this cache entry. Consumer compares against its own
    /// freshly-computed expected digest before deserializing. Mismatch
    /// indicates the cache file was produced under different inputs
    /// than the consumer expects (typically a missed cargo-fingerprint
    /// input — see `cache_key.rs` for the axis list).
    pub cache_key_digest: [u8; KEY_DIGEST_LEN],
    /// Byte offset of payload start. Must be >= HEADER_SIZE and 8-byte
    /// aligned. Writers use `DEFAULT_PAYLOAD_OFFSET` (64).
    pub payload_offset: u64,
    /// Byte length of payload.
    pub payload_length: u64,
    /// First 8 bytes of BLAKE3(payload). Corruption check only — not
    /// cryptographic integrity.
    pub payload_checksum: u64,
}

impl CacheHeader {
    /// Construct a header with the project's current format version,
    /// given the cache-key digest, payload size, and payload checksum.
    /// Convenience for writers.
    pub fn new(cache_key_digest: [u8; KEY_DIGEST_LEN], payload_length: u64, payload_checksum: u64) -> Self {
        CacheHeader {
            format_version: CACHE_FORMAT_VERSION,
            cache_key_digest,
            payload_offset: DEFAULT_PAYLOAD_OFFSET,
            payload_length,
            payload_checksum,
        }
    }

    /// Decode a header from the first `HEADER_SIZE` bytes of `buf`.
    /// Verifies magic, format_version, and payload_offset structural
    /// validity but does NOT verify the payload checksum (that requires
    /// the full payload — see `verify_payload_checksum`) and does NOT
    /// verify the cache_key_digest (that requires a caller-supplied
    /// expected digest — see `deserialize_cache`).
    pub fn read(buf: &[u8]) -> Result<CacheHeader, CacheError> {
        if buf.len() < HEADER_SIZE {
            return Err(CacheError::TooShort {
                expected: HEADER_SIZE,
                found: buf.len(),
            });
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&buf[0..4]);
        if magic != CACHE_MAGIC {
            return Err(CacheError::BadMagic { found: magic });
        }
        let format_version = read_u32_le(&buf[4..8]);
        let mut cache_key_digest = [0u8; KEY_DIGEST_LEN];
        cache_key_digest.copy_from_slice(&buf[8..24]);
        let header = CacheHeader {
            format_version,
            cache_key_digest,
            payload_offset: read_u64_le(&buf[24..32]),
            payload_length: read_u64_le(&buf[32..40]),
            payload_checksum: read_u64_le(&buf[40..48]),
        };
        // Reserved bytes 48..64 tolerated regardless of content.

        if header.format_version != CACHE_FORMAT_VERSION {
            return Err(CacheError::FormatVersion {
                expected: CACHE_FORMAT_VERSION,
                found: header.format_version,
            });
        }
        if header.payload_offset < HEADER_SIZE as u64 || header.payload_offset % 8 != 0 {
            return Err(CacheError::BadPayloadOffset {
                offset: header.payload_offset,
            });
        }
        Ok(header)
    }

    /// Encode the header into a 64-byte buffer.
    pub fn write(&self, buf: &mut [u8; HEADER_SIZE]) {
        buf.fill(0); // zero the reserved tail
        buf[0..4].copy_from_slice(&CACHE_MAGIC);
        buf[4..8].copy_from_slice(&self.format_version.to_le_bytes());
        buf[8..24].copy_from_slice(&self.cache_key_digest);
        buf[24..32].copy_from_slice(&self.payload_offset.to_le_bytes());
        buf[32..40].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[40..48].copy_from_slice(&self.payload_checksum.to_le_bytes());
    }
}

/// Errors produced when reading or validating a cache file.
#[derive(Debug, Clone)]
pub enum CacheError {
    /// Input shorter than the fixed header.
    TooShort {
        expected: usize,
        found: usize,
    },
    /// Magic bytes don't match "SKYC" — not a Sky cache file (or possibly
    /// stale `.sky-meta` content if discovery picked up the wrong file).
    BadMagic {
        found: [u8; 4],
    },
    /// `format_version` in the header doesn't match `CACHE_FORMAT_VERSION`.
    /// Pre-1.0 strict-match policy: producer and consumer must agree on
    /// the version exactly. Under the cache model this just means
    /// "cache miss, rebuild" — no migration is ever attempted.
    FormatVersion {
        expected: u32,
        found: u32,
    },
    /// `payload_offset` is invalid (< HEADER_SIZE or not 8-byte aligned).
    BadPayloadOffset {
        offset: u64,
    },
    /// Header's cache_key_digest doesn't match the caller-supplied
    /// expected digest. Indicates the cache was produced under
    /// different inputs than the consumer's current view — typically
    /// a missed cargo-fingerprint axis. Should be treated as a hard
    /// error (per Decision 2 / §7.6) rather than silently re-deriving.
    KeyDigestMismatch {
        expected: [u8; KEY_DIGEST_LEN],
        found: [u8; KEY_DIGEST_LEN],
    },
    /// Computed checksum of payload bytes doesn't match the header's
    /// `payload_checksum`. Indicates corruption or tampering.
    ChecksumMismatch {
        expected: u64,
        computed: u64,
    },
    /// Bincode deserialization of payload failed.
    BincodeRead(String),
    /// Bincode serialization of payload failed (writer-side error).
    BincodeWrite(String),
    /// File-system I/O error while reading or writing the cache file.
    Io(String),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheError::TooShort { expected, found } => {
                write!(
                    f,
                    "cache file shorter than header: need at least {} bytes, got {}",
                    expected, found,
                )
            }
            CacheError::BadMagic { found } => {
                write!(
                    f,
                    "cache magic mismatch: expected {:?} ({:?}), found {:?}",
                    CACHE_MAGIC,
                    std::str::from_utf8(&CACHE_MAGIC).unwrap_or("?"),
                    found,
                )
            }
            CacheError::FormatVersion { expected, found } => {
                write!(
                    f,
                    "cache format_version {} is unsupported; this toylangc \
                     expects format_version {}. Cache miss; rebuild the \
                     producing library with the current toylangc.",
                    found, expected,
                )
            }
            CacheError::BadPayloadOffset { offset } => {
                write!(
                    f,
                    "cache payload_offset {} is invalid (must be >= {} and \
                     8-byte aligned)",
                    offset, HEADER_SIZE,
                )
            }
            CacheError::KeyDigestMismatch { expected, found } => {
                write!(
                    f,
                    "cache key digest mismatch: header carries {}, \
                     consumer expects {}. Cache entry was produced under \
                     different inputs (likely a missed cargo-fingerprint axis); \
                     rebuild the producing library.",
                    hex16(found),
                    hex16(expected),
                )
            }
            CacheError::ChecksumMismatch { expected, computed } => {
                write!(
                    f,
                    "cache payload checksum mismatch: header claims {:016x}, \
                     bytes hash to {:016x}. File is corrupt or truncated.",
                    expected, computed,
                )
            }
            CacheError::BincodeRead(s) => write!(f, "cache payload decode failed: {}", s),
            CacheError::BincodeWrite(s) => write!(f, "cache payload encode failed: {}", s),
            CacheError::Io(s) => write!(f, "cache I/O error: {}", s),
        }
    }
}

impl std::error::Error for CacheError {}

fn read_u32_le(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[..4]);
    u32::from_le_bytes(a)
}

fn read_u64_le(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

fn hex16(bytes: &[u8; KEY_DIGEST_LEN]) -> String {
    let mut out = String::with_capacity(KEY_DIGEST_LEN * 2);
    for b in bytes.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Bincode configuration used by both serialize and deserialize sides.
/// Fixed-int + little-endian per the format spec — varint encoding would
/// make cache size depend on numeric content, breaking the byte-equality
/// CI invariant we care about (Fence 2 / `cache_determinism`).
fn bincode_cfg() -> bincode::config::Configuration<
    bincode::config::LittleEndian,
    bincode::config::Fixint,
> {
    bincode::config::standard()
        .with_fixed_int_encoding()
        .with_little_endian()
}

/// BLAKE3-truncated payload checksum (first 8 bytes, little-endian u64).
fn compute_payload_checksum(payload: &[u8]) -> u64 {
    let digest = blake3::hash(payload);
    let bytes = digest.as_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Serialize a registry into a full cache byte stream (header + payload).
///
/// `cache_key_digest` is the 16-byte BLAKE3-truncated digest of the
/// producer's cache-key axes inputs, computed by
/// `cache_key::compute_cache_key_digest`. It is stored in the header so
/// the consumer can detect cases where the file's inputs no longer
/// match the consumer's expected inputs (e.g. annotation file changed
/// but cargo didn't see it as a fingerprint input).
///
/// Determinism: identical input → identical output. Verified by Fence 2.
pub fn serialize_cache(
    registry: &ToylangRegistry,
    cache_key_digest: [u8; KEY_DIGEST_LEN],
) -> Result<Vec<u8>, CacheError> {
    let payload = bincode::serde::encode_to_vec(registry, bincode_cfg())
        .map_err(|e| CacheError::BincodeWrite(e.to_string()))?;
    let payload_checksum = compute_payload_checksum(&payload);
    let header = CacheHeader::new(cache_key_digest, payload.len() as u64, payload_checksum);

    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    let mut header_buf = [0u8; HEADER_SIZE];
    header.write(&mut header_buf);
    out.extend_from_slice(&header_buf);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Deserialize a cache byte stream back into a registry. Validates the
/// header, key digest (against caller-supplied expected), payload length,
/// and payload checksum before bincode-decoding.
///
/// Returns `CacheError::KeyDigestMismatch` if the header's stored digest
/// doesn't match the caller's expected digest. This is the load-bearing
/// safety check — without it, a stale cache file could silently produce
/// the wrong typed AST in the consumer's universe.
pub fn deserialize_cache(
    buf: &[u8],
    expected_key_digest: [u8; KEY_DIGEST_LEN],
) -> Result<ToylangRegistry, CacheError> {
    let header = CacheHeader::read(buf)?;
    if header.cache_key_digest != expected_key_digest {
        return Err(CacheError::KeyDigestMismatch {
            expected: expected_key_digest,
            found: header.cache_key_digest,
        });
    }
    let payload_start = header.payload_offset as usize;
    let payload_end = payload_start
        .checked_add(header.payload_length as usize)
        .ok_or_else(|| CacheError::Io("payload length overflow".to_string()))?;
    if buf.len() < payload_end {
        return Err(CacheError::TooShort {
            expected: payload_end,
            found: buf.len(),
        });
    }
    let payload = &buf[payload_start..payload_end];

    let computed = compute_payload_checksum(payload);
    if computed != header.payload_checksum {
        return Err(CacheError::ChecksumMismatch {
            expected: header.payload_checksum,
            computed,
        });
    }

    let (registry, _consumed) = bincode::serde::decode_from_slice::<ToylangRegistry, _>(
        payload, bincode_cfg(),
    )
    .map_err(|e| CacheError::BincodeRead(e.to_string()))?;
    Ok(registry)
}

/// Read just the cache-key digest from a cache file without decoding the
/// payload. Useful for diagnostic tools (`skyc inspect` analog) and
/// shadow-mode verification (Fence 3) where the consumer wants to know
/// "what digest does this file claim?" without paying the bincode decode.
pub fn read_cache_key_digest(buf: &[u8]) -> Result<[u8; KEY_DIGEST_LEN], CacheError> {
    let header = CacheHeader::read(buf)?;
    Ok(header.cache_key_digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_digest() -> [u8; KEY_DIGEST_LEN] {
        let mut d = [0u8; KEY_DIGEST_LEN];
        for (i, b) in d.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17);
        }
        d
    }

    fn other_digest() -> [u8; KEY_DIGEST_LEN] {
        let mut d = sample_digest();
        d[0] ^= 0xFF;
        d
    }

    // ============================================================================
    // Header round-trip + invariants.
    // ============================================================================

    #[test]
    fn header_round_trip() {
        let h = CacheHeader::new(sample_digest(), 1234, 0xDEADBEEF_CAFEBABE);
        let mut buf = [0u8; HEADER_SIZE];
        h.write(&mut buf);
        let h2 = CacheHeader::read(&buf).expect("header round-trip");
        assert_eq!(h, h2);
    }

    #[test]
    fn header_magic_check() {
        let mut buf = [0u8; HEADER_SIZE];
        CacheHeader::new(sample_digest(), 0, 0).write(&mut buf);
        buf[0] = b'X'; // corrupt magic
        match CacheHeader::read(&buf) {
            Err(CacheError::BadMagic { found }) => assert_eq!(found[0], b'X'),
            other => panic!("expected BadMagic, got {:?}", other),
        }
    }

    #[test]
    fn header_format_version_strict() {
        let mut buf = [0u8; HEADER_SIZE];
        let h = CacheHeader {
            format_version: CACHE_FORMAT_VERSION + 1,
            ..CacheHeader::new(sample_digest(), 0, 0)
        };
        h.write(&mut buf);
        match CacheHeader::read(&buf) {
            Err(CacheError::FormatVersion { expected, found }) => {
                assert_eq!(expected, CACHE_FORMAT_VERSION);
                assert_eq!(found, CACHE_FORMAT_VERSION + 1);
            }
            other => panic!("expected FormatVersion error, got {:?}", other),
        }
    }

    #[test]
    fn header_too_short() {
        let buf = [0u8; HEADER_SIZE - 1];
        match CacheHeader::read(&buf) {
            Err(CacheError::TooShort { expected, found }) => {
                assert_eq!(expected, HEADER_SIZE);
                assert_eq!(found, HEADER_SIZE - 1);
            }
            other => panic!("expected TooShort, got {:?}", other),
        }
    }

    #[test]
    fn header_bad_payload_offset() {
        let mut buf = [0u8; HEADER_SIZE];
        let h = CacheHeader {
            payload_offset: 1, // < HEADER_SIZE
            ..CacheHeader::new(sample_digest(), 0, 0)
        };
        h.write(&mut buf);
        match CacheHeader::read(&buf) {
            Err(CacheError::BadPayloadOffset { offset }) => assert_eq!(offset, 1),
            other => panic!("expected BadPayloadOffset, got {:?}", other),
        }
    }

    #[test]
    fn header_endianness_is_little() {
        let h = CacheHeader::new(sample_digest(), 0x0102030405060708, 0xCAFEBABEDEADBEEF);
        let mut buf = [0u8; HEADER_SIZE];
        h.write(&mut buf);
        // payload_length at offset 32, little-endian
        assert_eq!(&buf[32..40], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        // payload_checksum at offset 40
        assert_eq!(&buf[40..48], &[0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xFE, 0xCA]);
    }

    #[test]
    fn header_reserved_tail_is_zero() {
        let h = CacheHeader::new(sample_digest(), 0, 0);
        let mut buf = [0xFFu8; HEADER_SIZE]; // pre-fill with garbage
        h.write(&mut buf);
        assert_eq!(&buf[48..64], &[0u8; 16]);
    }

    #[test]
    fn header_tolerates_nonzero_reserved_tail_on_read() {
        let mut buf = [0u8; HEADER_SIZE];
        CacheHeader::new(sample_digest(), 0, 0).write(&mut buf);
        buf[48..64].copy_from_slice(&[0xAA; 16]);
        CacheHeader::read(&buf).expect("nonzero reserved tail should not fail read");
    }

    // ============================================================================
    // Payload round-trip via bincode + checksum + key digest gate.
    // ============================================================================

    use crate::toylang::ast::{BinOp, Block, Expr, Stmt};
    use crate::toylang::registry::{ToyField, ToyFunction, ToyParam, ToyStruct, ToylangRegistry};
    use crate::toylang::typed_ast::SourceType;

    fn sample_registry() -> ToylangRegistry {
        let mut registry = ToylangRegistry::default();
        registry.imports.push("std::alloc::Global".to_string());
        registry.imports.push("std::vec::Vec".to_string());
        registry.structs.insert(
            "Widget".to_string(),
            ToyStruct {
                type_params: vec![],
                fields: vec![ToyField {
                    name: "id".to_string(),
                    rust_type: SourceType::I32,
                }],
            },
        );
        registry.structs.insert(
            "Pair".to_string(),
            ToyStruct {
                type_params: vec!["A".to_string(), "B".to_string()],
                fields: vec![
                    ToyField {
                        name: "first".to_string(),
                        rust_type: SourceType::TypeParam("A".to_string()),
                    },
                    ToyField {
                        name: "second".to_string(),
                        rust_type: SourceType::TypeParam("B".to_string()),
                    },
                ],
            },
        );
        registry.functions.insert(
            "wrap".to_string(),
            ToyFunction {
                type_params: vec!["T".to_string()],
                is_export: false,
                params: vec![ToyParam {
                    name: "x".to_string(),
                    ty: SourceType::TypeParam("T".to_string()),
                }],
                return_ty: Some(SourceType::TypeParam("T".to_string())),
                body: Some(Block {
                    stmts: vec![Stmt::Let {
                        name: "y".to_string(),
                        expr: Expr::BinaryOp {
                            op: BinOp::Add,
                            left: Box::new(Expr::Var("x".to_string())),
                            right: Box::new(Expr::IntLit(1, SourceType::I32)),
                        },
                    }],
                    ret: Some(Expr::Var("y".to_string())),
                }),
            },
        );
        registry
    }

    #[test]
    fn payload_round_trip() {
        let original = sample_registry();
        let digest = sample_digest();
        let bytes = serialize_cache(&original, digest).expect("serialize");
        let recovered = deserialize_cache(&bytes, digest).expect("deserialize");
        // Spot-check: structural equality at the level the registry exposes.
        assert_eq!(recovered.imports, original.imports);
        assert_eq!(recovered.structs.len(), original.structs.len());
        assert_eq!(recovered.functions.len(), original.functions.len());
        assert!(recovered.structs.contains_key("Widget"));
        assert!(recovered.structs.contains_key("Pair"));
        assert!(recovered.functions.contains_key("wrap"));
        let wrap = recovered.functions.get("wrap").unwrap();
        assert_eq!(wrap.type_params, vec!["T".to_string()]);
        assert_eq!(wrap.params.len(), 1);
        assert!(wrap.body.is_some());
    }

    #[test]
    fn payload_determinism() {
        // Same input → byte-identical output. Fence 2 (cache_determinism)
        // verifies the property end-to-end on a real project; this test
        // guards the property at the unit level. Identical digest argument
        // is mandatory — different digests produce different bytes.
        let digest = sample_digest();
        let bytes_a = serialize_cache(&sample_registry(), digest).expect("serialize a");
        let bytes_b = serialize_cache(&sample_registry(), digest).expect("serialize b");
        assert_eq!(bytes_a, bytes_b);
    }

    #[test]
    fn digest_mismatch_returns_error() {
        // Load-bearing safety test (Plan Decision 3 / §7.6 hard-error
        // policy): a cache file produced with digest A must NOT
        // deserialize successfully when the consumer expects digest B.
        let bytes = serialize_cache(&sample_registry(), sample_digest()).expect("serialize");
        match deserialize_cache(&bytes, other_digest()) {
            Err(CacheError::KeyDigestMismatch { expected, found }) => {
                assert_eq!(expected, other_digest());
                assert_eq!(found, sample_digest());
            }
            other => panic!("expected KeyDigestMismatch, got {:?}", other),
        }
    }

    #[test]
    fn payload_checksum_mismatch_detected() {
        let digest = sample_digest();
        let mut bytes = serialize_cache(&sample_registry(), digest).expect("serialize");
        // Corrupt one byte of the payload (past the header).
        bytes[HEADER_SIZE + 5] ^= 0xFF;
        match deserialize_cache(&bytes, digest) {
            Err(CacheError::ChecksumMismatch { .. }) => {}
            other => panic!("expected ChecksumMismatch, got {:?}", other),
        }
    }

    #[test]
    fn payload_truncation_detected() {
        let digest = sample_digest();
        let bytes = serialize_cache(&sample_registry(), digest).expect("serialize");
        let truncated = &bytes[..bytes.len() - 5];
        match deserialize_cache(truncated, digest) {
            Err(CacheError::TooShort { .. }) => {}
            other => panic!("expected TooShort, got {:?}", other),
        }
    }

    #[test]
    fn payload_empty_registry() {
        let original = ToylangRegistry::default();
        let digest = sample_digest();
        let bytes = serialize_cache(&original, digest).expect("serialize empty");
        let recovered = deserialize_cache(&bytes, digest).expect("deserialize empty");
        assert!(recovered.structs.is_empty());
        assert!(recovered.functions.is_empty());
        assert!(recovered.imports.is_empty());
        assert!(recovered.typeid_table.is_empty());
    }

    #[test]
    fn read_key_digest_without_payload() {
        let digest = sample_digest();
        let bytes = serialize_cache(&sample_registry(), digest).expect("serialize");
        let recovered = read_cache_key_digest(&bytes).expect("read digest");
        assert_eq!(recovered, digest);
    }

    /// Differs from sidecar.rs: cache files cross the "different magic" gate
    /// when an old `.sky-meta` file ends up at the cache location somehow.
    #[test]
    fn rejects_sidecar_magic() {
        let mut buf = [0u8; HEADER_SIZE];
        // Pretend someone wrote a sidecar file at the cache path.
        let sidecar_magic: [u8; 4] = *b"SKYM";
        buf[0..4].copy_from_slice(&sidecar_magic);
        match CacheHeader::read(&buf) {
            Err(CacheError::BadMagic { found }) => assert_eq!(found, sidecar_magic),
            other => panic!("expected BadMagic, got {:?}", other),
        }
    }
}
