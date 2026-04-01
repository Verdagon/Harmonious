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

/// Build a trivial MIR body for a zero-argument function that returns a
/// constant i32. Used to verify the mir_built override fires correctly.
///
/// MIR structure:
///   bb0:
///     _0 = const VALUE_i32;
///     return;
pub fn build_const_i32_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    value: i32,
) -> Body<'tcx> {
    let span = tcx.def_span(def_id);
    let source_info = SourceInfo::outermost(span);

    // Local(0) = return place of type i32
    let mut local_decls = IndexVec::new();
    local_decls.push(LocalDecl::new(tcx.types.i32, span));

    // Assign constant to return place
    let assign_stmt = Statement {
        source_info,
        kind: StatementKind::Assign(Box::new((
            Place::from(Local::from_u32(0)), // RETURN_PLACE
            Rvalue::Use(Operand::Constant(Box::new(ConstOperand {
                span,
                user_ty: None,
                const_: Const::Val(
                    ConstValue::Scalar(Scalar::from_i32(value)),
                    tcx.types.i32,
                ),
            }))),
        ))),
    };

    let terminator = Terminator {
        source_info,
        kind: TerminatorKind::Return,
    };

    let mut basic_blocks = IndexVec::new();
    basic_blocks.push(BasicBlockData::new(Some(terminator), false));
    // Append statement to block (BasicBlockData::new sets statements: vec![])
    basic_blocks[START_BLOCK].statements.push(assign_stmt);

    // One source scope is required (OUTERMOST_SOURCE_SCOPE = index 0)
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
        IndexVec::new(), // user_type_annotations
        0,               // arg_count (get_x takes Point arg but we ignore it for PoC)
        vec![],          // var_debug_info
        span,
        None,            // coroutine
        None,            // tainted_by_errors
    )
}

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
/// The extern function always has one extra `*const ()` parameter at the end.
///
/// If `rust_deps` is non-empty, emits ReifyFnPointer casts that trigger
/// monomorphization of the specified Rust generic functions. All casts write
/// to a single `deps_local` whose final value is passed as the extra arg.
/// If `rust_deps` is empty, passes a null `*const ()`.
///
/// The Toylang LLVM implementation ignores the extra arg at runtime.
pub fn build_extern_call_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    extern_symbol: &str,
    rust_deps: &[(DefId, ty::GenericArgsRef<'tcx>)],
) -> Body<'tcx> {
    let span = tcx.def_span(def_id);
    let source_info = SourceInfo::outermost(span);

    let fn_sig = tcx.fn_sig(def_id).instantiate_identity().skip_binder();
    let ret_ty = fn_sig.output();
    let real_arg_count = fn_sig.inputs().len();

    // Locals: _0=return, _1.._N=real args
    let mut local_decls = IndexVec::new();
    local_decls.push(LocalDecl::new(ret_ty, span)); // _0
    for &input_ty in fn_sig.inputs() {
        local_decls.push(LocalDecl::new(input_ty, span));
    }

    let raw_ptr_ty = Ty::new_imm_ptr(tcx, tcx.types.unit); // *const ()

    // Allocate a single local for the phantom deps argument
    let deps_local = local_decls.push(LocalDecl::new(raw_ptr_ty, span));

    let mut stmts = Vec::new();

    // For each Rust dep: ReifyFnPointer into a unique fn_ptr_local,
    // then Transmute into the shared deps_local (overwriting each time).
    // All fn_ptr_locals stay live because their Transmutes read them.
    // The monomorphizer sees the Const::zero_sized(fn_def_ty) in each cast.
    for &(dep_def_id, dep_args) in rust_deps {
        let fn_def_ty = Ty::new_fn_def(tcx, dep_def_id, dep_args);
        let fn_sig_of_dep = tcx.fn_sig(dep_def_id).instantiate(tcx, dep_args);
        let fn_ptr_ty = Ty::new_fn_ptr(tcx, fn_sig_of_dep);
        let fn_ptr_local = local_decls.push(LocalDecl::new(fn_ptr_ty, span));

        // _N = const <FnDef> as fn_ptr_ty (ReifyFnPointer)
        stmts.push(Statement {
            source_info,
            kind: StatementKind::Assign(Box::new((
                Place::from(fn_ptr_local),
                Rvalue::Cast(
                    CastKind::PointerCoercion(
                        PointerCoercion::ReifyFnPointer,
                        CoercionSource::Implicit,
                    ),
                    Operand::Constant(Box::new(ConstOperand {
                        span,
                        user_ty: None,
                        const_: Const::zero_sized(fn_def_ty),
                    })),
                    fn_ptr_ty,
                ),
            ))),
        });

        // _deps = move _N as *const () (Transmute) — overwrites each time
        stmts.push(Statement {
            source_info,
            kind: StatementKind::Assign(Box::new((
                Place::from(deps_local),
                Rvalue::Cast(
                    CastKind::Transmute,
                    Operand::Move(Place::from(fn_ptr_local)),
                    raw_ptr_ty,
                ),
            ))),
        });
    }

    // If no deps, initialize deps_local to null
    if rust_deps.is_empty() {
        stmts.push(Statement {
            source_info,
            kind: StatementKind::Assign(Box::new((
                Place::from(deps_local),
                Rvalue::Use(Operand::Constant(Box::new(ConstOperand {
                    span,
                    user_ty: None,
                    const_: Const::Val(
                        ConstValue::Scalar(Scalar::from_target_usize(0, &tcx.data_layout)),
                        raw_ptr_ty,
                    ),
                }))),
            ))),
        });
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

    // Build args: real args first, then the single phantom deps arg
    let mut args: Vec<Spanned<Operand<'tcx>>> = Vec::new();
    for i in 0..real_arg_count {
        args.push(Spanned {
            node: Operand::Move(Place::from(Local::from_u32((i + 1) as u32))),
            span,
        });
    }
    args.push(Spanned {
        node: Operand::Move(Place::from(deps_local)),
        span,
    });

    // bb0: phantom stmts + call
    let bb0 = BasicBlockData {
        statements: stmts,
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
