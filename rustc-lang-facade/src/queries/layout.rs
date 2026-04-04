//! layout_of query override.
//!
//! When rustc needs the size/alignment of a consumer-defined type, this override
//! intercepts the query and asks the consumer for the field types via
//! `monomorphize_type`. The consumer returns concrete `Ty<'tcx>` values for each
//! field, and we compute the C-like struct layout (field offsets with padding).
//!
//! IMPORTANT: layout_of is called for EVERY type rustc encounters — including
//! `*mut Point`, `&Point`, `Option<Point>`, `FnDef(..., [Point])`, etc. We MUST
//! filter to only `TyKind::Adt` types whose name matches a consumer-registered type.
//! Intercepting derived types (pointers, references, etc.) corrupts their layouts
//! and causes ICEs in codegen. This was a hard-won lesson from the early PoC.
//!
//! The layout returned here uses `BackendRepr::Memory { sized: true }` which tells
//! rustc this is an opaque memory blob. Rustc doesn't try to decompose it into
//! scalar pairs or niches. This is intentional — the consumer controls the layout.
//! Niche optimization (`largest_niche: None`) is disabled; add it later if needed.
//!
//! Target portability: all sizes and alignments come from `tcx.layout_of()` on the
//! consumer-provided field types. No hardcoded sizes. This was fixed in the
//! "replace hardcoded aarch64 values" change — see git history.

#![allow(unused)]

use rustc_abi::{
    AbiAndPrefAlign, Align, BackendRepr, FieldIdx, FieldsShape, LayoutData, Size, VariantIdx,
    Variants,
};
use rustc_middle::ty::layout::{LayoutError, TyAndLayout};
use rustc_middle::ty::{PseudoCanonicalInput, Ty, TyCtxt, TypingEnv, TyKind};
use std::sync::OnceLock;

// The provider function type. This must match rustc's Providers::layout_of signature
// exactly — it changes between nightlies. On nightly-2025-01-15 it uses
// PseudoCanonicalInput. On other nightlies it may use ParamEnvAnd.
type LayoutOfFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>>;

/// Saved default provider. Used to fall through for non-consumer types.
/// Stored in OnceLock because query providers are function pointers — they
/// can't capture the original provider as a closure variable.
static DEFAULT_LAYOUT_OF: OnceLock<LayoutOfFn> = OnceLock::new();

pub fn save_default(f: LayoutOfFn) {
    let _ = DEFAULT_LAYOUT_OF.set(f);
}

/// The layout_of override. Intercepts consumer-defined types, falls through
/// to rustc's default for everything else.
pub fn toy_layout_of<'tcx>(
    tcx: TyCtxt<'tcx>,
    query: PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>> {
    let ty = query.value;

    // Only intercept ADT types from __lang_stubs whose name matches a consumer type.
    // Checking the module prevents collisions with user-defined types sharing a name.
    if let TyKind::Adt(adt_def, _) = ty.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        if crate::is_consumer_type(&name) && crate::is_from_lang_stubs(tcx, adt_def.did()) {
            eprintln!("[toylang] layout_of intercepted for: {:?}", ty);

            // Ask the consumer to monomorphize this type — returns concrete field types.
            // For non-generic types, this just returns the field types directly.
            // For generic types like Pair<i32, i32>, the consumer substitutes
            // type params with concrete args and returns [tcx.types.i32, tcx.types.i32].
            let result = crate::call_monomorphize_type(&name, tcx, ty);

            return Ok(build_layout(tcx, ty, &result.field_types, query.typing_env));
        }
    }

    // Fall through to rustc's default provider.
    let default = DEFAULT_LAYOUT_OF.get().expect("default layout_of not saved");
    default(tcx, query)
}

fn build_layout<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
    field_types: &[Ty<'tcx>],
    typing_env: TypingEnv<'tcx>,
) -> TyAndLayout<'tcx> {
    use rustc_index::IndexVec;

    // Compute field offsets from the consumer-provided field types.
    let mut offset = 0u64;
    let mut field_offsets_vec: Vec<u64> = Vec::new();
    let mut max_align = 1u64;
    for &field_ty in field_types {
        let layout = tcx.layout_of(PseudoCanonicalInput {
            value: field_ty,
            typing_env: TypingEnv::fully_monomorphized(),
        }).expect("layout of field type");
        let fsz = layout.size.bytes();
        let falign = layout.align.abi.bytes();
        max_align = max_align.max(falign);
        offset = align_up(offset, falign);
        field_offsets_vec.push(offset);
        offset += fsz;
    }
    let total_size = align_up(offset, max_align);

    let align = Align::from_bytes(max_align).unwrap();
    let abi_align = AbiAndPrefAlign::new(align);

    // Report 0 fields to rustc — the struct is fully opaque. Rustc only needs
    // the total size and alignment for ABI decisions with BackendRepr::Memory.
    // Exposing per-field info caused crashes when rustc's ABI code tried to
    // index into the ADT's fields (which are dummy stubs, not real fields).
    let layout_data = LayoutData {
        fields: FieldsShape::Arbitrary {
            offsets: IndexVec::new(),
            memory_index: IndexVec::new(),
        },
        variants: Variants::Single { index: VariantIdx::from_u32(0) },
        backend_repr: BackendRepr::Memory { sized: true },
        largest_niche: None,
        align: abi_align,
        size: Size::from_bytes(total_size),
        max_repr_align: None,
        unadjusted_abi_align: align,
        randomization_seed: 0,
    };

    TyAndLayout {
        ty,
        layout: tcx.mk_layout(layout_data),
    }
}

fn align_up(offset: u64, align: u64) -> u64 {
    (offset + align - 1) & !(align - 1)
}
