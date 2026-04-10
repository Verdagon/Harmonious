//! per_instance_mir query override — per-instantiation MIR for consumer functions.
//!
//! This fires for every concrete instantiation of a consumer function during
//! monomorphization. It returns a MIR body that:
//! 1. References Rust types/functions the consumer needs (driving the collector's
//!    fixpoint loop for dependency discovery)
//! 2. Ends with a panic terminator (the body is never executed — the consumer's
//!    .o provides the real implementation via the symbol_name override)
//!
//! The codegen dispatch (patched in our rustc fork) skips code generation for
//! instances where per_instance_mir returns Some, leaving them as extern
//! declarations. The consumer's .o provides the definition.

#![allow(unused)]

use rustc_middle::mir::*;
use rustc_middle::ty::{self, Instance, Ty, TyCtxt};
use rustc_span::DUMMY_SP;
use std::sync::OnceLock;

/// Per-instance MIR provider. Returns Some for consumer function instances,
/// None for everything else.
pub fn lang_per_instance_mir<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
) -> Option<&'tcx Body<'tcx>> {
    let def_id = instance.def_id();

    // Only intercept consumer functions from __lang_stubs.
    if !crate::is_from_lang_stubs(tcx, def_id) {
        return None;
    }

    let name = tcx.opt_item_name(def_id)?.to_string();

    // Check if it's a registered consumer function
    let is_fn = crate::is_consumer_fn(&name);

    // Check if it's an accessor method on a consumer type
    let is_accessor = if !is_fn {
        is_consumer_accessor(tcx, def_id)
    } else {
        false
    };

    if !is_fn && !is_accessor {
        return None;
    }

    // Build the callback name for the consumer.
    // Regular functions: "make_counter"
    // Accessor methods: "Counter.value"
    let callback_name = if is_accessor {
        let assoc_item = tcx.opt_associated_item(def_id)?;
        let impl_def_id = assoc_item.container_id(tcx);
        let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
        if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
            let struct_name = tcx.item_name(adt_def.did()).to_string();
            format!("{}.{}", struct_name, name)
        } else {
            return None;
        }
    } else {
        name.clone()
    };

    // Call the consumer's monomorphize_fn callback.
    let local_def_id = def_id.as_local()?;
    let result = crate::call_monomorphize_fn(&callback_name, tcx, local_def_id, instance);

    eprintln!("[toylang] per_instance_mir for: {} → symbol='{}', {} rust deps",
        callback_name, result.extern_symbol, result.rust_deps.len());

    // Build a MIR body that references dependencies (for the collector)
    // and ends with a panic (never executed — consumer .o provides real code).
    let body = build_dependency_body(tcx, instance, &result.rust_deps);
    Some(tcx.arena.alloc(body))
}

/// Check if a DefId is an accessor method on a consumer type (public for symbol_name module).
pub(crate) fn is_consumer_accessor_pub(tcx: TyCtxt<'_>, def_id: rustc_span::def_id::DefId) -> bool {
    is_consumer_accessor(tcx, def_id)
}

/// Check if a DefId is an accessor method on a consumer type.
fn is_consumer_accessor(tcx: TyCtxt<'_>, def_id: rustc_span::def_id::DefId) -> bool {
    if let Some(assoc_item) = tcx.opt_associated_item(def_id) {
        let impl_def_id = assoc_item.container_id(tcx);
        let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
        if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
            let struct_name = tcx.item_name(adt_def.did()).to_string();
            return crate::is_consumer_type(&struct_name);
        }
    }
    false
}

/// Build a MIR body that references Rust dependencies (so the monomorphization
/// collector discovers them) and terminates with Abort (never executed).
fn build_dependency_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
    rust_deps: &[(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)],
) -> Body<'tcx> {
    use rustc_index::IndexVec;

    let def_id = instance.def_id();
    let sig = tcx.fn_sig(def_id).instantiate(tcx, instance.args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(),
        sig,
    );

    let span = tcx.def_span(def_id);
    let source_info = SourceInfo::outermost(span);

    // Local declarations: _0 (return), _1.._n (args)
    let mut local_decls: IndexVec<Local, LocalDecl<'tcx>> = IndexVec::new();
    local_decls.push(LocalDecl::new(sig.output(), span)); // _0: return
    for &input_ty in sig.inputs() {
        local_decls.push(LocalDecl::new(input_ty, span)); // args
    }

    let mut blocks: IndexVec<BasicBlock, BasicBlockData<'tcx>> = IndexVec::new();
    let mut stmts = Vec::new();

    // For each type dependency, emit SizeOf to force the collector to discover it
    for &(dep_def_id, dep_args) in rust_deps {
        if tcx.def_kind(dep_def_id).is_fn_like() {
            // Function dependency — emit a ReifyFnPointer cast (same as existing approach)
            let fn_def_ty = Ty::new_fn_def(tcx, dep_def_id, dep_args);
            let fn_sig = tcx.fn_sig(dep_def_id).instantiate(tcx, dep_args);
            let fn_ptr_ty = Ty::new_fn_ptr(tcx, fn_sig);
            let fn_ptr_local = local_decls.push(LocalDecl::new(fn_ptr_ty, span));

            stmts.push(Statement {
                source_info,
                kind: StatementKind::Assign(Box::new((
                    Place::from(fn_ptr_local),
                    Rvalue::Cast(
                        CastKind::PointerCoercion(
                            ty::adjustment::PointerCoercion::ReifyFnPointer,
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
        } else {
            // Type dependency — emit SizeOf
            let size_local = local_decls.push(LocalDecl::new(tcx.types.usize, span));
            let dep_ty = Ty::new_adt(tcx, tcx.adt_def(dep_def_id), dep_args);
            stmts.push(Statement {
                source_info,
                kind: StatementKind::Assign(Box::new((
                    Place::from(size_local),
                    Rvalue::NullaryOp(NullOp::SizeOf, dep_ty),
                ))),
            });
        }
    }

    // Single block: dependency statements + Unreachable terminator.
    // Using Unreachable because the codegen skip means this code is never emitted.
    // The consumer .o provides the real implementation.
    blocks.push(BasicBlockData {
        statements: stmts,
        terminator: Some(Terminator {
            source_info,
            kind: TerminatorKind::Unreachable,
        }),
        is_cleanup: false,
    });

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
        MirSource::item(def_id),
        blocks,
        source_scopes,
        local_decls,
        IndexVec::new(), // user_type_annotations
        sig.inputs().len(),
        vec![],          // var_debug_info
        span,
        None,            // coroutine
        None,            // tainted_by_errors
    );
    // Must set these or the collector panics when accessing required_consts/mentioned_items.
    // Our body has neither — it's a dependency-only stub.
    body.set_required_consts(vec![]);
    body.set_mentioned_items(vec![]);
    body
}
