#![allow(unused)]


use rustc_abi::{
    AbiAndPrefAlign, Align, BackendRepr, FieldIdx, FieldsShape, LayoutData, Size, VariantIdx,
    Variants,
};
use rustc_middle::ty::layout::{LayoutError, TyAndLayout};
use rustc_middle::ty::{PseudoCanonicalInput, Ty, TyCtxt, TypingEnv, TyKind};
use std::sync::OnceLock;

// The provider function type for layout_of on nightly-2025-01-15.
type LayoutOfFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>>;

static DEFAULT_LAYOUT_OF: OnceLock<LayoutOfFn> = OnceLock::new();

pub fn save_default(f: LayoutOfFn) {
    let _ = DEFAULT_LAYOUT_OF.set(f);
}

pub fn toy_layout_of<'tcx>(
    tcx: TyCtxt<'tcx>,
    query: PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>> {
    let ty = query.value;

    // Only intercept ADT types whose name matches a consumer-registered type.
    if let TyKind::Adt(adt_def, _) = ty.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        if crate::is_consumer_type(&name) {
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

    let offsets: IndexVec<FieldIdx, Size> = field_offsets_vec.iter()
        .map(|&o| Size::from_bytes(o))
        .collect();
    let memory_index: IndexVec<FieldIdx, u32> = (0..field_types.len() as u32).collect();

    let layout_data = LayoutData {
        fields: FieldsShape::Arbitrary { offsets, memory_index },
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
