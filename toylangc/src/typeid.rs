//! Content-addressed typeids for Sky/Path-2's universal opaque wrapper.
//!
//! Architecture: `rust-interop-architecture.md` §10.6 — a typeid is a stable
//! u64 identity for a Sky-defined type, used as the const-generic argument of
//! `__ToylangOpaque<const T: u64>`. The decoding side (queries/layout.rs,
//! eventually queries/symbol_name.rs) reads the const
//! u64 out of an `Instance`'s args, looks the typeid up in the registry's
//! `typeid_table`, and recovers the original `(name, type_args)` pair.
//!
//! The hash is BLAKE3 truncated to 8 bytes interpreted little-endian as u64 —
//! same reduction `sidecar.rs::compute_checksum` uses for payload integrity.
//! The pre-image is a bincode-serialized `(name, type_args)` tuple under the
//! same fixed-int-LE config the sidecar uses, so two runs with identical input
//! produce byte-identical pre-images and therefore identical typeids.
//!
//! Determinism is load-bearing: cross-compile reproducibility of the binary
//! depends on the typeid for `(Wrapper, [I32])` being the same in every Sky
//! compiler invocation that has access to the type. See §10.8 of the
//! architecture doc for the cross-compile-stability argument.

use crate::toylang::typed_ast::ResolvedType;
use serde::Serialize;

/// Compute the content-addressed typeid for a Sky type identified by name +
/// concrete type arguments.
///
/// For an export struct without type params (e.g., `Widget`), pass an empty
/// slice for `type_args`. For a generic instantiation (e.g., `Wrapper<i32>`),
/// pass the resolved concrete args. The architecture's wrapper-as-field
/// design (§10.4.5 Path 2) uses `compute(name, &[])` at struct-emission time
/// — Wrapper<i32> and Wrapper<i64> share `HASH_FOR_WRAPPER` and disambiguate
/// at the type level via their own generic args slot, not via different
/// wrapper typeids.
///
/// Returns the first 8 bytes of `blake3::hash(bincode(name, type_args))`,
/// interpreted as a little-endian u64. Truncation collision probability is
/// 2^-32 across a 4-billion-type universe — negligible for any realistic Sky
/// project.
pub fn compute(name: &str, type_args: &[ResolvedType]) -> u64 {
    #[derive(Serialize)]
    struct PreImage<'a> {
        name: &'a str,
        type_args: &'a [ResolvedType],
    }
    let preimage = PreImage { name, type_args };
    let bytes = bincode::serde::encode_to_vec(&preimage, bincode_cfg())
        .expect("bincode encode of (name, type_args) is infallible for owned data");
    let digest = blake3::hash(&bytes);
    let d = digest.as_bytes();
    u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

/// Same bincode config sidecar.rs uses. Determinism requires fixed-int +
/// little-endian — varints would make the pre-image size depend on numeric
/// content of type-arg names, indirectly affecting the hash.
fn bincode_cfg() -> bincode::config::Configuration<
    bincode::config::LittleEndian,
    bincode::config::Fixint,
> {
    bincode::config::standard()
        .with_fixed_int_encoding()
        .with_little_endian()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_input_same_output() {
        let a = compute("Widget", &[]);
        let b = compute("Widget", &[]);
        assert_eq!(a, b, "same input must produce same typeid");
    }

    #[test]
    fn different_name_different_typeid() {
        let widget = compute("Widget", &[]);
        let gadget = compute("Gadget", &[]);
        assert_ne!(widget, gadget);
    }

    #[test]
    fn different_args_different_typeid() {
        let w_i32 = compute("Wrapper", &[ResolvedType::I32]);
        let w_i64 = compute("Wrapper", &[ResolvedType::I64]);
        assert_ne!(w_i32, w_i64);
    }

    #[test]
    fn args_order_matters() {
        let ab = compute("Pair", &[ResolvedType::I32, ResolvedType::I64]);
        let ba = compute("Pair", &[ResolvedType::I64, ResolvedType::I32]);
        assert_ne!(ab, ba, "type-arg order must affect typeid");
    }

    #[test]
    fn empty_args_distinct_from_singleton_args() {
        let nullary = compute("X", &[]);
        let unary_i32 = compute("X", &[ResolvedType::I32]);
        assert_ne!(nullary, unary_i32);
    }

    /// Stability anchor — the algorithm is locked. If this assertion ever
    /// changes value, sidecars across Sky compiler versions stop interop'ing.
    /// On change, bump the sidecar format_version and add a migration step.
    ///
    /// The pinned literal was captured by running this test once with
    /// `assert_eq!(widget, 0); eprintln!("widget={:#x}", widget)` and pasting
    /// the printed value below. Future drift in `bincode` or `blake3` will
    /// surface as a test failure rather than silently breaking cross-version
    /// sidecar interop.
    #[test]
    fn widget_typeid_is_stable() {
        // Hard-pinned. Any drift in `bincode`'s fixed-int-LE encoding or in
        // `blake3` would change this value and surface as a test failure.
        // When that happens: bump the sidecar format_version and add a
        // migration step before changing the literal.
        assert_eq!(compute("Widget", &[]), 0x48723b0bb65d86f7);
    }
}
