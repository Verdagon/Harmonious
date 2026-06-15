//! Sidecar (`.sky-meta`) format — types, header codec.
//!
//! See `docs/architecture/sidecar-format.md` for the full binary-format
//! specification. This module is the Rust-side implementation of that
//! spec.
//!
//! S.1 (this file's initial scope, per the quarter-of-work plan) defines
//! the header struct + error type + read/write of the fixed 64-byte
//! header. S.2 adds payload serialization (bincode + the registry types
//! with serde derives) and exposes `serialize_sidecar` /
//! `deserialize_sidecar`. S.3 / S.4 wire it into the build and load
//! paths respectively. S.5 adds the determinism CI invariant.

#![allow(dead_code)] // S.1 lands the types; subsequent S steps consume them.

use std::fmt;

use crate::toylang::registry::ToylangRegistry;

/// Magic number identifying a Sky sidecar file. ASCII "SKYM".
pub const SIDECAR_MAGIC: [u8; 4] = *b"SKYM";

/// Current sidecar format version. Pre-1.0 policy: strict match required.
/// Bump on any breaking change to the payload's serializable types; see
/// `docs/architecture/sidecar-format.md` "Version policy".
pub const CURRENT_FORMAT_VERSION: u32 = 1;

/// Size of the fixed header in bytes. Payload begins at this offset (64-byte
/// alignment is required by the spec; the natural header end is 48, and the
/// remaining 16 bytes are reserved/padding to reach 64).
pub const HEADER_SIZE: usize = 64;

/// skyc/toylangc major version stamped into the header for diagnostics.
/// Doesn't affect format compatibility — only `format_version` does.
pub const SKYC_VERSION_MAJOR: u32 = 0;

/// skyc/toylangc minor version stamped into the header for diagnostics.
pub const SKYC_VERSION_MINOR: u32 = 1;

/// Default payload offset: immediately after the fixed header. The reader
/// allows any offset >= HEADER_SIZE that's 8-byte aligned; writers always
/// use this constant.
pub const DEFAULT_PAYLOAD_OFFSET: u64 = HEADER_SIZE as u64;

/// Fixed-layout header at the start of every sidecar file. 64 bytes.
///
/// Fields are written little-endian. See
/// `docs/architecture/sidecar-format.md` "Header layout" for byte-by-byte
/// offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarHeader {
    /// skyc/toylangc major version that produced this sidecar.
    /// Diagnostic only — compatibility is determined by `format_version`.
    pub skyc_version_major: u32,
    /// skyc/toylangc minor version. Diagnostic only.
    pub skyc_version_minor: u32,
    /// Sidecar binary format version. Pre-1.0: strict match.
    pub format_version: u32,
    /// Optional capability flags. Bit 0: comptime recipes. Bit 1: async
    /// state machines. Bit 2: sidecar annotations. Toylang Phase 1
    /// sidecars set all bits to zero.
    pub capabilities: u64,
    /// Byte offset of payload start. Must be >= HEADER_SIZE and 8-byte
    /// aligned. Writers use `DEFAULT_PAYLOAD_OFFSET` (64).
    pub payload_offset: u64,
    /// Byte length of payload.
    pub payload_length: u64,
    /// First 8 bytes of BLAKE3(payload). Corruption check only — not
    /// cryptographic integrity.
    pub payload_checksum: u64,
}

impl SidecarHeader {
    /// Construct a header with the project's current skyc + format
    /// version, given payload size + checksum. Convenience for writers.
    pub fn new(payload_length: u64, payload_checksum: u64) -> Self {
        SidecarHeader {
            skyc_version_major: SKYC_VERSION_MAJOR,
            skyc_version_minor: SKYC_VERSION_MINOR,
            format_version: CURRENT_FORMAT_VERSION,
            capabilities: 0,
            payload_offset: DEFAULT_PAYLOAD_OFFSET,
            payload_length,
            payload_checksum,
        }
    }

    /// Decode a header from the first `HEADER_SIZE` bytes of `buf`. Verifies
    /// magic, format_version, and payload_offset structural validity but
    /// does NOT verify the payload checksum (that requires the full payload
    /// — see `verify_payload_checksum`).
    pub fn read(buf: &[u8]) -> Result<SidecarHeader, SidecarError> {
        if buf.len() < HEADER_SIZE {
            return Err(SidecarError::TooShort {
                expected: HEADER_SIZE,
                found: buf.len(),
            });
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&buf[0..4]);
        if magic != SIDECAR_MAGIC {
            return Err(SidecarError::BadMagic { found: magic });
        }
        let header = SidecarHeader {
            skyc_version_major: read_u32_le(&buf[4..8]),
            skyc_version_minor: read_u32_le(&buf[8..12]),
            format_version: read_u32_le(&buf[12..16]),
            capabilities: read_u64_le(&buf[16..24]),
            payload_offset: read_u64_le(&buf[24..32]),
            payload_length: read_u64_le(&buf[32..40]),
            payload_checksum: read_u64_le(&buf[40..48]),
        };
        // Reserved bytes 48..64 are tolerated regardless of content
        // (they're informational padding; we don't fail on nonzero
        // reserved bytes so future writers can extend the header without
        // breaking older readers — until format_version forces it).

        if header.format_version != CURRENT_FORMAT_VERSION {
            return Err(SidecarError::FormatVersion {
                expected: CURRENT_FORMAT_VERSION,
                found: header.format_version,
            });
        }
        if header.payload_offset < HEADER_SIZE as u64 || header.payload_offset % 8 != 0 {
            return Err(SidecarError::BadPayloadOffset {
                offset: header.payload_offset,
            });
        }
        Ok(header)
    }

    /// Encode the header into a 64-byte buffer.
    pub fn write(&self, buf: &mut [u8; HEADER_SIZE]) {
        buf.fill(0); // zero the reserved tail
        buf[0..4].copy_from_slice(&SIDECAR_MAGIC);
        buf[4..8].copy_from_slice(&self.skyc_version_major.to_le_bytes());
        buf[8..12].copy_from_slice(&self.skyc_version_minor.to_le_bytes());
        buf[12..16].copy_from_slice(&self.format_version.to_le_bytes());
        buf[16..24].copy_from_slice(&self.capabilities.to_le_bytes());
        buf[24..32].copy_from_slice(&self.payload_offset.to_le_bytes());
        buf[32..40].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[40..48].copy_from_slice(&self.payload_checksum.to_le_bytes());
    }
}

/// Errors produced when reading or validating a sidecar.
#[derive(Debug, Clone)]
pub enum SidecarError {
    /// Input shorter than the fixed header.
    TooShort {
        expected: usize,
        found: usize,
    },
    /// Magic bytes don't match "SKYM" — not a sidecar file.
    BadMagic {
        found: [u8; 4],
    },
    /// `format_version` in the header doesn't match `CURRENT_FORMAT_VERSION`.
    /// Pre-1.0 strict-match policy: producer and consumer must agree on
    /// the version exactly.
    FormatVersion {
        expected: u32,
        found: u32,
    },
    /// `payload_offset` is invalid (< HEADER_SIZE or not 8-byte aligned).
    BadPayloadOffset {
        offset: u64,
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
    /// File-system I/O error while reading or writing the sidecar.
    Io(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarError::TooShort { expected, found } => {
                write!(
                    f,
                    "sidecar shorter than header: need at least {} bytes, got {}",
                    expected, found,
                )
            }
            SidecarError::BadMagic { found } => {
                write!(
                    f,
                    "sidecar magic mismatch: expected {:?} ({:?}), found {:?}",
                    SIDECAR_MAGIC,
                    std::str::from_utf8(&SIDECAR_MAGIC).unwrap_or("?"),
                    found,
                )
            }
            SidecarError::FormatVersion { expected, found } => {
                write!(
                    f,
                    "sidecar format_version {} is unsupported; this toylangc \
                     expects format_version {}. Pre-1.0 policy is strict match \
                     — rebuild the producing library with a matching toylangc \
                     version.",
                    found, expected,
                )
            }
            SidecarError::BadPayloadOffset { offset } => {
                write!(
                    f,
                    "sidecar payload_offset {} is invalid (must be >= {} and \
                     8-byte aligned)",
                    offset, HEADER_SIZE,
                )
            }
            SidecarError::ChecksumMismatch { expected, computed } => {
                write!(
                    f,
                    "sidecar payload checksum mismatch: header claims {:016x}, \
                     bytes hash to {:016x}. File is corrupt or truncated.",
                    expected, computed,
                )
            }
            SidecarError::BincodeRead(s) => write!(f, "sidecar payload decode failed: {}", s),
            SidecarError::BincodeWrite(s) => write!(f, "sidecar payload encode failed: {}", s),
            SidecarError::Io(s) => write!(f, "sidecar I/O error: {}", s),
        }
    }
}

impl std::error::Error for SidecarError {}

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

/// Bincode configuration used by both serialize and deserialize sides.
/// Fixed-int + little-endian per the format spec — varint encoding would
/// make sidecar size depend on numeric content, breaking the byte-equality
/// CI invariant we care about.
fn bincode_cfg() -> bincode::config::Configuration<
    bincode::config::LittleEndian,
    bincode::config::Fixint,
> {
    bincode::config::standard()
        .with_fixed_int_encoding()
        .with_little_endian()
}

/// BLAKE3-truncated payload checksum (first 8 bytes, little-endian u64).
fn compute_checksum(payload: &[u8]) -> u64 {
    let digest = blake3::hash(payload);
    let bytes = digest.as_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Serialize a registry into a full sidecar byte stream (header + payload).
///
/// Output layout matches `docs/architecture/sidecar-format.md`: 64-byte header,
/// then bincode-encoded payload. Total length =
/// `HEADER_SIZE + serialized_payload.len()`.
///
/// Determinism: identical input → identical output. Verified by S.5's CI
/// invariant.
pub fn serialize_sidecar(registry: &ToylangRegistry) -> Result<Vec<u8>, SidecarError> {
    let payload = bincode::serde::encode_to_vec(registry, bincode_cfg())
        .map_err(|e| SidecarError::BincodeWrite(e.to_string()))?;
    let checksum = compute_checksum(&payload);
    let header = SidecarHeader::new(payload.len() as u64, checksum);

    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    let mut header_buf = [0u8; HEADER_SIZE];
    header.write(&mut header_buf);
    out.extend_from_slice(&header_buf);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Deserialize a sidecar byte stream back into a registry. Validates the
/// header, payload length, and payload checksum before bincode-decoding.
///
/// Returns `SidecarError::ChecksumMismatch` if the payload's computed BLAKE3
/// truncated hash doesn't match the header's stored checksum. This catches
/// corruption + truncation; it is NOT cryptographic integrity.
pub fn deserialize_sidecar(buf: &[u8]) -> Result<ToylangRegistry, SidecarError> {
    let header = SidecarHeader::read(buf)?;
    let payload_start = header.payload_offset as usize;
    let payload_end = payload_start
        .checked_add(header.payload_length as usize)
        .ok_or_else(|| SidecarError::Io("payload length overflow".to_string()))?;
    if buf.len() < payload_end {
        return Err(SidecarError::TooShort {
            expected: payload_end,
            found: buf.len(),
        });
    }
    let payload = &buf[payload_start..payload_end];

    let computed = compute_checksum(payload);
    if computed != header.payload_checksum {
        return Err(SidecarError::ChecksumMismatch {
            expected: header.payload_checksum,
            computed,
        });
    }

    let (registry, _consumed) = bincode::serde::decode_from_slice::<ToylangRegistry, _>(
        payload, bincode_cfg(),
    )
    .map_err(|e| SidecarError::BincodeRead(e.to_string()))?;
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = SidecarHeader::new(1234, 0xDEADBEEF_CAFEBABE);
        let mut buf = [0u8; HEADER_SIZE];
        h.write(&mut buf);
        let h2 = SidecarHeader::read(&buf).expect("header round-trip");
        assert_eq!(h, h2);
    }

    #[test]
    fn header_magic_check() {
        let mut buf = [0u8; HEADER_SIZE];
        SidecarHeader::new(0, 0).write(&mut buf);
        buf[0] = b'X'; // corrupt magic
        match SidecarHeader::read(&buf) {
            Err(SidecarError::BadMagic { found }) => assert_eq!(found[0], b'X'),
            other => panic!("expected BadMagic, got {:?}", other),
        }
    }

    #[test]
    fn header_format_version_strict() {
        let mut buf = [0u8; HEADER_SIZE];
        let h = SidecarHeader {
            format_version: CURRENT_FORMAT_VERSION + 1,
            ..SidecarHeader::new(0, 0)
        };
        h.write(&mut buf);
        match SidecarHeader::read(&buf) {
            Err(SidecarError::FormatVersion { expected, found }) => {
                assert_eq!(expected, CURRENT_FORMAT_VERSION);
                assert_eq!(found, CURRENT_FORMAT_VERSION + 1);
            }
            other => panic!("expected FormatVersion error, got {:?}", other),
        }
    }

    #[test]
    fn header_too_short() {
        let buf = [0u8; HEADER_SIZE - 1];
        match SidecarHeader::read(&buf) {
            Err(SidecarError::TooShort { expected, found }) => {
                assert_eq!(expected, HEADER_SIZE);
                assert_eq!(found, HEADER_SIZE - 1);
            }
            other => panic!("expected TooShort, got {:?}", other),
        }
    }

    #[test]
    fn header_bad_payload_offset() {
        let mut buf = [0u8; HEADER_SIZE];
        let h = SidecarHeader {
            payload_offset: 1, // < HEADER_SIZE
            ..SidecarHeader::new(0, 0)
        };
        h.write(&mut buf);
        match SidecarHeader::read(&buf) {
            Err(SidecarError::BadPayloadOffset { offset }) => assert_eq!(offset, 1),
            other => panic!("expected BadPayloadOffset, got {:?}", other),
        }
    }

    #[test]
    fn header_payload_offset_unaligned() {
        let mut buf = [0u8; HEADER_SIZE];
        let h = SidecarHeader {
            payload_offset: 65, // >= HEADER_SIZE but not 8-byte aligned
            ..SidecarHeader::new(0, 0)
        };
        h.write(&mut buf);
        match SidecarHeader::read(&buf) {
            Err(SidecarError::BadPayloadOffset { offset }) => assert_eq!(offset, 65),
            other => panic!("expected BadPayloadOffset for unaligned offset, got {:?}", other),
        }
    }

    #[test]
    fn header_endianness_is_little() {
        let h = SidecarHeader::new(0x0102030405060708, 0xCAFEBABEDEADBEEF);
        let mut buf = [0u8; HEADER_SIZE];
        h.write(&mut buf);
        // payload_length at offset 32, little-endian
        assert_eq!(&buf[32..40], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        // payload_checksum at offset 40
        assert_eq!(&buf[40..48], &[0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xFE, 0xCA]);
    }

    #[test]
    fn header_reserved_tail_is_zero() {
        let h = SidecarHeader::new(0, 0);
        let mut buf = [0xFFu8; HEADER_SIZE]; // pre-fill with garbage
        h.write(&mut buf);
        assert_eq!(&buf[48..64], &[0u8; 16]);
    }

    #[test]
    fn header_tolerates_nonzero_reserved_tail_on_read() {
        // A future writer might use the reserved bytes; the current reader
        // should not fail (until format_version forces breaking).
        let mut buf = [0u8; HEADER_SIZE];
        SidecarHeader::new(0, 0).write(&mut buf);
        buf[48..64].copy_from_slice(&[0xAA; 16]);
        SidecarHeader::read(&buf).expect("nonzero reserved tail should not fail read");
    }

    // ============================================================================
    // S.2: payload round-trip via bincode + checksum.
    // ============================================================================

    use crate::toylang::ast::{BinOp, Block, Expr, Stmt};
    use crate::toylang::registry::{ToyField, ToyFunction, ToyParam, ToyStruct, ToylangRegistry};
    use crate::toylang::typed_ast::ResolvedType;

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
                    rust_type: ResolvedType::I32,
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
                        rust_type: ResolvedType::TypeParam("A".to_string()),
                    },
                    ToyField {
                        name: "second".to_string(),
                        rust_type: ResolvedType::TypeParam("B".to_string()),
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
                    ty: ResolvedType::TypeParam("T".to_string()),
                }],
                return_ty: Some(ResolvedType::TypeParam("T".to_string())),
                body: Some(Block {
                    stmts: vec![Stmt::Let {
                        name: "y".to_string(),
                        expr: Expr::BinaryOp {
                            op: BinOp::Add,
                            left: Box::new(Expr::Var("x".to_string())),
                            right: Box::new(Expr::IntLit(1, ResolvedType::I32)),
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
        let bytes = serialize_sidecar(&original).expect("serialize");
        let recovered = deserialize_sidecar(&bytes).expect("deserialize");
        // Spot-check: structural equality at the level the registry exposes.
        // Bincode is a fixed-int derivation, so we trust serde derives to
        // round-trip the full tree; we just verify the top-level shape.
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
        // Same input → byte-identical output. The Determinism CI invariant
        // (S.5) verifies this end-to-end on a real project; this test
        // guards the property at the unit level.
        let bytes_a = serialize_sidecar(&sample_registry()).expect("serialize a");
        let bytes_b = serialize_sidecar(&sample_registry()).expect("serialize b");
        assert_eq!(bytes_a, bytes_b);
    }

    #[test]
    fn payload_checksum_mismatch_detected() {
        let original = sample_registry();
        let mut bytes = serialize_sidecar(&original).expect("serialize");
        // Corrupt one byte of the payload (past the header).
        bytes[HEADER_SIZE + 5] ^= 0xFF;
        match deserialize_sidecar(&bytes) {
            Err(SidecarError::ChecksumMismatch { .. }) => {}
            other => panic!("expected ChecksumMismatch, got {:?}", other),
        }
    }

    #[test]
    fn payload_truncation_detected() {
        let original = sample_registry();
        let bytes = serialize_sidecar(&original).expect("serialize");
        let truncated = &bytes[..bytes.len() - 5];
        match deserialize_sidecar(truncated) {
            Err(SidecarError::TooShort { .. }) => {}
            other => panic!("expected TooShort, got {:?}", other),
        }
    }

    #[test]
    fn payload_empty_registry() {
        let original = ToylangRegistry::default();
        let bytes = serialize_sidecar(&original).expect("serialize empty");
        let recovered = deserialize_sidecar(&bytes).expect("deserialize empty");
        assert!(recovered.structs.is_empty());
        assert!(recovered.functions.is_empty());
        assert!(recovered.imports.is_empty());
        assert!(recovered.typeid_table.is_empty());
    }

    /// Phase E Path 2 / Phase 1.3 — `populate_typeid_table` hashes every
    /// struct in the registry into the typeid_table. Verifies population is
    /// idempotent + survives a sidecar round-trip (the architecture-§10.8
    /// "downstream compiles decode upstream typeids" property).
    #[test]
    fn typeid_table_populates_and_round_trips() {
        let mut original = sample_registry();
        original.populate_typeid_table();
        assert_eq!(
            original.typeid_table.len(),
            original.structs.len(),
            "one entry per struct",
        );

        // Each table entry's name matches the struct name; args are empty per
        // the Path 2 "per-struct identity" interpretation.
        for (typeid, (name, args)) in &original.typeid_table {
            assert!(original.structs.contains_key(name));
            assert!(args.is_empty());
            assert_eq!(*typeid, crate::typeid::compute(name, &[]));
        }

        // Round-trip preserves the table.
        let bytes = serialize_sidecar(&original).expect("serialize");
        let recovered = deserialize_sidecar(&bytes).expect("deserialize");
        assert_eq!(recovered.typeid_table, original.typeid_table);

        // Idempotency: re-running populate on the recovered registry produces
        // the same table.
        let mut recovered2 = recovered.clone();
        recovered2.populate_typeid_table();
        assert_eq!(recovered2.typeid_table, original.typeid_table);
    }
}
