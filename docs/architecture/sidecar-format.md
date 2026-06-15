# Sidecar (`.sky-meta`) Format Specification

Implementer-facing reference for the `.sky-meta` sidecar file. The
architecture-level rationale lives in `rust-interop-architecture.md` §7
(sidecar) and §8 (Temputs format); this doc is the binary-format
reference, narrower and more concrete.

The sidecar is a per-library binary blob carrying the typed AST (toylang
calls it the registry; Sky calls it the Temputs) for every item in the
library. It travels alongside the stub rlib and is read by downstream
consumers (binary compiles, other Sky libraries) to reconstitute the
producing library's universe without re-parsing source.

## File location and naming

The sidecar lives adjacent to the stub rlib with extension `.sky-meta`,
named after the library's cargo package name. For `lang_stubs_my_app.rlib`,
the sidecar is `lang_stubs_my_app.sky-meta`. Both files are emitted by the
toylangc build script into `.toylang-build/lang_stubs_crate/`.

When the facade's metadata loader loads an upstream rlib at user_bin
compile time, it checks for an adjacent `<basename>.sky-meta` file:
- Present: load sidecar into the consumer-side universe.
- Absent, but rlib has the `__SKY_STUBS_MARKER` item: error
  ("Sky sidecar missing for `<crate>`; required for compilation").
- Absent and no marker: not a Sky lib; ignore.

## Header layout (64 bytes)

The sidecar starts with a 64-byte fixed header:

| Offset | Length | Field | Encoding | Description |
|---|---|---|---|---|
| 0 | 4 | `magic` | bytes "SKYM" (0x534B594D) | Magic number. Mismatch → format error. |
| 4 | 4 | `skyc_version_major` | u32 LE | Major version of toylangc/skyc that produced the sidecar. Diagnostic only. |
| 8 | 4 | `skyc_version_minor` | u32 LE | Minor version. Diagnostic only. |
| 12 | 4 | `format_version` | u32 LE | Sidecar binary-format version. Pre-1.0: strict match required. See "Version policy" below. |
| 16 | 8 | `capabilities` | u64 LE | Capabilities bitset. See "Capabilities" below. |
| 24 | 8 | `payload_offset` | u64 LE | Byte offset of payload start (must be ≥ 64; 64-byte aligned). |
| 32 | 8 | `payload_length` | u64 LE | Byte length of payload. |
| 40 | 8 | `payload_checksum` | u64 LE | BLAKE3-truncated checksum of payload bytes (first 8 bytes of BLAKE3 digest). |
| 48 | 16 | (reserved) | zero bytes | Reserved for future header fields. Must be zero. |

Total header size: 64 bytes. Payload begins at offset 64 (the natural
header end is 48, with a 16-byte reserved tail to reach 64-byte alignment
— a 64-byte payload start lets the reader mmap the payload with native
cache-line alignment if desired).

### Capabilities bitset

Bit 0 set if the payload contains comptime-synthesis recipes.
Bit 1 set if the payload contains async-state-machine descriptions.
Bit 2 set if the payload contains sidecar-annotation overrides.

Toylang's pre-Phase-2 sidecars set all bits to zero. The bitset lets a
reader skip-verify-load even when it can't interpret some capability:
unknown capability bits → read-only mode (item lookups work, body
materialization fails for affected items).

### Reading the header

```
fn read_header(buf: &[u8]) -> Result<SidecarHeader, SidecarError> {
    if buf.len() < 64 { return Err(SidecarError::TooShort); }
    if &buf[0..4] != b"SKYM" { return Err(SidecarError::BadMagic); }
    let format_version = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    if format_version != CURRENT_FORMAT_VERSION {
        return Err(SidecarError::FormatVersion {
            expected: CURRENT_FORMAT_VERSION, found: format_version
        });
    }
    // ... read remaining fields ...
}
```

## Payload format

The payload is the bincode-serialized `ToylangRegistry` (toylang's
typed-AST collection). The exact bincode shape is derived from the
`#[derive(Serialize, Deserialize)]` annotations on the registry's types;
no separate schema file is maintained.

Why bincode (per `rust-interop-architecture.md` §7.3):
- Deterministic given deterministic input.
- Self-describing via the derived schema (no separate `.proto` etc.).
- Compact, fast read/write.
- Mature in the Rust ecosystem.

### Bincode configuration

Use bincode's *fixed-int* + *little-endian* + *no-length-limit*
configuration to maximize cross-platform determinism:

```rust
let cfg = bincode::config::standard()
    .with_fixed_int_encoding()
    .with_little_endian();
```

(`fixed_int_encoding` makes `u64` write 8 bytes regardless of value;
`varint` is the default and would make sidecar size dependent on
content. Determinism — not size — is what we optimize for.)

### Serializable types

The complete set of types that must derive `Serialize, Deserialize`:

- `toylang::registry::{ToylangRegistry, ToyStruct, ToyField, ToyFunction, ToyParam}`
- `toylang::ast::{Block, Stmt, Expr, BinOp}`
- `toylang::typed_ast::ResolvedType`

`ToylangRegistry`'s `structs` and `functions` fields are `HashMap<String,
_>` today. **They must be promoted to `BTreeMap<String, _>` for the
sidecar** so iteration order is deterministic (HashMap iteration is
randomized per-run by default). Either:
- Promote at the registry type itself (`pub structs: BTreeMap<...>`),
  affecting all of toylang's runtime code; or
- Serialize via a custom `Serialize` impl that sorts keys before emit.

Phase 1S picks promotion (cleanest; sorted iteration anywhere in the
codebase is generally desirable).

## Determinism requirements

The sidecar is byte-deterministic given identical source input. CI
verifies this by building the same project twice (with cache wipes
between) and byte-comparing. Concrete rules:

- No timestamps anywhere in the payload.
- All map/set iteration uses BTreeMap / BTreeSet.
- No random IDs.
- No host-system-dependent paths. Source positions reference filenames
  relative to the cargo package root.
- Float NaN handling: payload contains no floats today. If/when floats
  appear (numeric literals), use bit-pattern serialization (`f64::to_bits`)
  so NaN payload is canonicalized.

## Missing-sidecar diagnostics

When the facade loader detects a marker-bearing rlib without an adjacent
sidecar:

```
error: Sky sidecar missing for crate `my_utils`
  expected at: target/deps/my_utils-abc123.sky-meta
  crate marker present: yes
  hint: this rlib was built without the corresponding sidecar
  hint: rebuild `my_utils` with the Sky toolchain
```

This is a hard error. Falling back to "treat as plain Rust" is wrong
because the rlib's `unreachable!()` bodies would panic at runtime.

## Version policy

**Pre-1.0 (current):** strict match. The sidecar's `format_version` must
equal the consumer's expected version exactly; mismatch is a hard error
with a "rebuild upstream" diagnostic. Toylang/Sky users keep their
toolchain consistent; mixing versions isn't supported.

**1.0+:** range match. Consumer reads format versions in
`[expected_min, expected_max]` and applies migration code for older
versions. Migration is read-only (the on-disk bytes don't change; the
in-memory representation matches current). For sidecars newer than
consumer's max, hard error ("future format; please upgrade").

`format_version` bumps:
- Bump on any breaking change to the payload's serializable types
  (field added/removed/reordered, enum variant added/reordered).
- Don't bump for diagnostic header field changes (e.g., adding fields to
  the reserved 16-byte tail).
- Bumps land as discrete commits with a CHANGELOG entry in this doc's
  "History" section below.

## History

| `format_version` | Commit | Change |
|---|---|---|
| 1 | (initial) | Initial format. See "Payload format" above for the type list. |

## See also

- `rust-interop-architecture.md` §7 — sidecar architecture and design
  rationale.
- `rust-interop-architecture.md` §8 — Temputs payload content.
- `rust-interop-architecture.md` §4.4 — byte-identical pass-through
  invariant (the sidecar's existence must NOT affect pure-Rust compile
  output).
- `toylangc/src/sidecar.rs` — implementation.
- `toylangc/src/toylang/{registry,ast,typed_ast}.rs` — the serializable
  types.
