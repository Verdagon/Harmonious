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

/// Build a MIR body that calls an extern "C" function and returns the result.
///
/// This is the primary MIR construction function — used by the `mir_built` override
/// for every consumer-defined function.
///
/// **Phantom deps mechanism:** If `rust_deps` is non-empty, emits `ReifyFnPointer`
/// casts that force rustc's monomorphizer to stamp out those Rust generic
/// instantiations (e.g. `Vec::push<Point>`). Each cast takes a zero-sized FnDef
/// constant, coerces it to a function pointer, then transmutes to `*const ()`.
/// The final `*const ()` is passed as an extra argument to the extern function.
/// The consumer's compiled function ignores this argument at runtime.
///
/// If `rust_deps` is empty, passes a null `*const ()` instead.
///
/// **Why ReifyFnPointer?** The monomorphization collector scans reachable MIR for
/// function type references. A `Const::zero_sized(FnDef(...))` in a `ReifyFnPointer`
/// cast is treated as a "used" item — guaranteed to be codegen'd. This is stronger
/// than `mentioned_items` which only produces "mentioned" items with no codegen
/// guarantee. See `docs/historical/design-monomorphization-triggers.md`.
///
/// **Future: type deps.** Currently only handles function deps. When we need to
/// monomorphize Rust types discovered inside consumer structs (e.g. `Vec<RustWing>`
/// as a field), we'll need to emit phantom type locals here too. See
/// `docs/historical/struct-opacity-and-type-deps.md`.
pub fn build_extern_call_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    extern_symbol: &str,
    _rust_deps: &[(DefId, ty::GenericArgsRef<'tcx>)],
) -> Body<'tcx> {
    // With per_instance_mir handling dependency discovery and codegen skip,
    // this body is only used for type checking. It calls the extern function
    // with the real args (no _deps phantom parameter).
    let span = tcx.def_span(def_id);
    let source_info = SourceInfo::outermost(span);

    let fn_sig = tcx.fn_sig(def_id).instantiate_identity().skip_binder();
    let ret_ty = fn_sig.output();
    let real_arg_count = fn_sig.inputs().len();

    let mut local_decls = IndexVec::new();
    local_decls.push(LocalDecl::new(ret_ty, span)); // _0
    for &input_ty in fn_sig.inputs() {
        local_decls.push(LocalDecl::new(input_ty, span));
    }

    // Find the extern function
    let extern_def_id = find_extern_fn(tcx, extern_symbol)
        .unwrap_or_else(|| panic!("[toylang] extern function '{}' not found", extern_symbol));
    let extern_fn_ty = tcx.type_of(extern_def_id).instantiate_identity();

    let func = Operand::Constant(Box::new(ConstOperand {
        span,
        user_ty: None,
        const_: Const::zero_sized(extern_fn_ty),
    }));

    // Build args: just the real args
    let args: Vec<Spanned<Operand<'tcx>>> = (0..real_arg_count)
        .map(|i| Spanned {
            node: Operand::Move(Place::from(Local::from_u32((i + 1) as u32))),
            span,
        })
        .collect();

    let bb0 = BasicBlockData {
        statements: vec![],
        terminator: Some(Terminator {
            source_info,
            kind: TerminatorKind::Call {
                func,
                args: args.into_boxed_slice(),
                destination: Place::from(Local::ZERO),
                target: Some(BasicBlock::from_u32(1)),
                unwind: UnwindAction::Continue,
                call_source: CallSource::Misc,
                fn_span: span,
            },
        }),
        is_cleanup: false,
    };

    let bb1 = BasicBlockData {
        statements: vec![],
        terminator: Some(Terminator { source_info, kind: TerminatorKind::Return }),
        is_cleanup: false,
    };

    let mut basic_blocks = IndexVec::new();
    basic_blocks.push(bb0);
    basic_blocks.push(bb1);

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

    Body::new(
        MirSource::item(def_id.to_def_id()),
        basic_blocks,
        source_scopes,
        local_decls,
        IndexVec::new(),
        real_arg_count,
        vec![],
        span,
        None,
        None,
    )
}
