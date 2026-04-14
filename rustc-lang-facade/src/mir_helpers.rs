//! MIR body construction utilities.
//!
//! These functions build hand-written MIR `Body` values for the query overrides.
//! Constructing MIR is delicate — rustc's MIR validator checks structural
//! correctness even with borrow checking disabled. Key rules:
//!
//! - `Local(0)` must be the return place with the exact type from `tcx.fn_sig`
//! - Every non-argument local needs `StorageLive` before first use, `StorageDead` after
//!   (except: the current code omits these for simplicity and it works on this nightly.
//!    If a future nightly's validator enforces this, they'll need to be added.)
//! - Every basic block must have a `terminator: Some(...)` — `None` panics in codegen
//! - Use `SourceInfo::outermost(span)` for spans, not `DUMMY_SP` (can trigger ICEs)
//! - mir_shims bodies MUST call `set_required_consts` + `set_mentioned_items` (empty vecs)
//!   because `mir_promoted` doesn't set them for shims. mir_built bodies must NOT call
//!   these — `mir_promoted` sets them and would panic on "already set".
//!
//! See `docs/historical/design-monomorphization-triggers.md` for why we use
//! ReifyFnPointer casts (Approach A) instead of mentioned_items (Approach B).

#![allow(unused)]

use rustc_abi::VariantIdx;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_index::IndexVec;
use rustc_middle::mir::{
    AggregateKind, BasicBlock, BasicBlockData, Body, BorrowKind, CallSource, CastKind,
    ClearCrossCrate, Const, ConstOperand, ConstValue, Local, LocalDecl, MirSource, MutBorrowKind,
    Operand, Place, PlaceElem, Rvalue, SourceInfo, SourceScopeData, START_BLOCK, Statement,
    StatementKind, Terminator, TerminatorKind, UnwindAction,
};
use rustc_middle::mir::CoercionSource;
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::mir::interpret::Scalar;
use rustc_middle::ty::{self, GenericArg, Ty, TyCtxt};
use rustc_span::source_map::Spanned;
use rustc_span::DUMMY_SP;

/// Build a MIR body for drop_in_place::<T> that calls __toylang_drop_T(ptr).
///
/// Signature: fn(*mut T) -> ()
/// MIR:
///   bb0: _0 = __toylang_drop_T(copy _1) -> bb1;
///   bb1: return;
pub fn build_drop_call_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    drop_in_place_def_id: DefId,    // DefId of core::ptr::drop_in_place
    ty: Ty<'tcx>,                   // the Toylang type being dropped
    struct_name: &str,
) -> Body<'tcx> {
    let span = if let ty::TyKind::Adt(adt_def, _) = ty.kind() {
        tcx.def_span(adt_def.did())
    } else {
        DUMMY_SP
    };
    let source_info = SourceInfo::outermost(span);

    // Locals: _0 = (), _1 = *mut T
    let mut local_decls = IndexVec::new();
    local_decls.push(LocalDecl::new(tcx.types.unit, span));               // _0: ()
    local_decls.push(LocalDecl::new(Ty::new_mut_ptr(tcx, ty), span));     // _1: *mut T

    // Find __toylang_drop_{struct_name} in the current crate's extern items
    let drop_fn_name = format!("__toylang_drop_{}", struct_name);
    let drop_fn_def_id = find_extern_fn(tcx, &drop_fn_name);

    // Build the call terminator or fall back to a no-op return
    let (bb0_term, num_blocks) = if let Some(fn_def_id) = drop_fn_def_id {
        // instantiate_identity: structural inspection only — we need the function's
        // zero-sized type to build a Const operand for the MIR call terminator.
        // The drop glue fn is always monomorphic (no type params), so no substitution
        // is needed; we just want the raw type of the definition.
        let fn_ty = tcx.type_of(fn_def_id).instantiate_identity();
        let func = Operand::Constant(Box::new(ConstOperand {
            span,
            user_ty: None,
            const_: Const::zero_sized(fn_ty),
        }));
        let call_term = Terminator {
            source_info,
            kind: TerminatorKind::Call {
                func,
                args: vec![Spanned {
                    node: Operand::Copy(Place::from(Local::from_u32(1))),
                    span,
                }].into_boxed_slice(),
                destination: Place::from(Local::from_u32(0)),
                target: Some(BasicBlock::from_u32(1)),
                unwind: UnwindAction::Continue,
                call_source: CallSource::Misc,
                fn_span: span,
            },
        };
        (call_term, 2usize)
    } else {
        eprintln!("[toylang] WARNING: {} not found, drop body is a no-op", drop_fn_name);
        (Terminator { source_info, kind: TerminatorKind::Return }, 1usize)
    };

    let mut basic_blocks = IndexVec::new();
    basic_blocks.push(BasicBlockData::new(Some(bb0_term), false));
    if num_blocks == 2 {
        basic_blocks.push(BasicBlockData::new(
            Some(Terminator { source_info, kind: TerminatorKind::Return }),
            false,
        ));
    }

    let source_scopes = IndexVec::from_elem_n(
        SourceScopeData {
            span,
            parent_scope: None,
            inlined: None,
            inlined_parent_scope: None,
            local_data: ClearCrossCrate::Clear,
        },
        1,
    );

    let mut body = Body::new(
        MirSource::from_instance(ty::InstanceKind::DropGlue(drop_in_place_def_id, Some(ty))),
        basic_blocks,
        source_scopes,
        local_decls,
        IndexVec::new(), // user_type_annotations
        1,               // arg_count = 1 (*mut T)
        vec![],          // var_debug_info
        span,
        None,            // coroutine
        None,            // tainted_by_errors
    );
    // required_consts and mentioned_items must be set or the monomorphization
    // collector panics. Our synthetic body has neither.
    body.set_required_consts(vec![]);
    body.set_mentioned_items(vec![]);
    body
}

fn find_extern_fn(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    for id in tcx.hir_crate_items(()).foreign_items() {
        let def_id = id.owner_id.def_id.to_def_id();
        if tcx.item_name(def_id).as_str() == name {
            return Some(def_id);
        }
    }
    None
}
