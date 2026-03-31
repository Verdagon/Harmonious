#![allow(unused)]

extern crate rustc_abi;
extern crate rustc_index;
extern crate rustc_middle;

use rustc_abi::{
    AbiAndPrefAlign, Align, BackendRepr, FieldIdx, FieldsShape, LayoutData, Size, VariantIdx,
    Variants,
};
use rustc_middle::ty::layout::{LayoutError, TyAndLayout};
use rustc_middle::ty::{PseudoCanonicalInput, Ty, TyCtxt, TypingEnv, TyKind};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use crate::toylang::registry::{ToylangRegistry, ToyFieldType, ToyStruct};

// The provider function type for layout_of on nightly-2025-01-15.
type LayoutOfFn = for<'tcx> fn(
    TyCtxt<'tcx>,
    PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>>;

// Both are global statics so queries executing on Rayon worker threads
// can read them (thread-locals would only be set on the main rustc thread).
static REGISTRY: OnceLock<Arc<ToylangRegistry>> = OnceLock::new();
static DEFAULT_LAYOUT_OF: OnceLock<LayoutOfFn> = OnceLock::new();

pub fn install_registry(r: Arc<ToylangRegistry>) {
    // Ignore the error if already set (idempotent for test runs).
    let _ = REGISTRY.set(r);
}

pub fn save_default(f: LayoutOfFn) {
    let _ = DEFAULT_LAYOUT_OF.set(f);
}

pub fn toy_layout_of<'tcx>(
    tcx: TyCtxt<'tcx>,
    query: PseudoCanonicalInput<'tcx, Ty<'tcx>>,
) -> Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>> {
    let ty = query.value;

    // Detect Toylang types by matching against ADT names only.
    // Restrict to TyKind::Adt so we don't accidentally intercept
    // *mut Point, &mut Point, FnDef(..., [Point]), etc.
    let struct_name = REGISTRY.get().and_then(|reg| {
        if let rustc_middle::ty::TyKind::Adt(adt_def, _) = ty.kind() {
            let name = tcx.item_name(adt_def.did()).to_string();
            reg.structs.keys().find(|k| k.as_str() == name).cloned()
        } else {
            None
        }
    });

    if let Some(name) = struct_name {
        eprintln!("[toylang] layout_of intercepted for: {:?}", ty);
        let reg = REGISTRY.get().expect("registry set above");
        return Ok(build_layout(tcx, ty, &reg.structs[&name], query.typing_env));
    }

    // Fall through to rustc's default provider.
    let default = DEFAULT_LAYOUT_OF.get().expect("default layout_of not saved");
    default(tcx, query)
}

fn build_layout<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
    toy: &ToyStruct,
    typing_env: TypingEnv<'tcx>,
) -> TyAndLayout<'tcx> {
    use rustc_index::IndexVec;

    // Build type-param substitution from the adt's generic args.
    let subst: HashMap<&str, Ty<'tcx>> = if !toy.type_params.is_empty() {
        if let TyKind::Adt(_, args) = ty.kind() {
            toy.type_params.iter()
                .enumerate()
                .map(|(i, name)| (name.as_str(), args[i].expect_ty()))
                .collect()
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // Compute field offsets dynamically.
    let mut offset = 0u64;
    let mut field_offsets_vec: Vec<u64> = Vec::new();
    let mut max_align = 1u64;
    for field in &toy.fields {
        let (fsz, falign) = field_size_align(tcx, &field.rust_type, &subst, typing_env);
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
    let memory_index: IndexVec<FieldIdx, u32> = (0..toy.fields.len() as u32).collect();

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

fn field_size_align<'tcx>(
    tcx: TyCtxt<'tcx>,
    field_ty: &ToyFieldType,
    subst: &HashMap<&str, Ty<'tcx>>,
    typing_env: TypingEnv<'tcx>,
) -> (u64, u64) {
    match field_ty {
        ToyFieldType::I32  => (4, 4),
        ToyFieldType::I64  => (8, 8),
        ToyFieldType::F64  => (8, 8),
        ToyFieldType::Bool => (1, 1),
        ToyFieldType::TypeParam(name) => {
            let concrete = *subst.get(name.as_str()).expect("type param not in subst");
            let layout = tcx.layout_of(PseudoCanonicalInput {
                value: concrete,
                typing_env: TypingEnv::fully_monomorphized(),
            }).expect("layout of type param field");
            (layout.size.bytes(), layout.align.abi.bytes())
        }
    }
}

fn align_up(offset: u64, align: u64) -> u64 {
    (offset + align - 1) & !(align - 1)
}
