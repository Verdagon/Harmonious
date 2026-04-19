//! optimized_mir override — DefId-keyed synthetic MIR for consumer functions.
//!
//! Stage-3 migration replaces the forked `per_instance_mir` query (which the
//! facade installed with Instance-keyed, pre-substituted bodies) with an
//! override of rustc's existing `optimized_mir` query. Both hooks fire during
//! monomorphization; the difference is who does the type-arg substitution.
//! Under this override the consumer returns a GENERIC body (with Params
//! intact, pointed to by `ReifyFnPointer` casts on unsubstituted dep items)
//! and rustc's mono collector substitutes per caller during its walk —
//! exactly the machinery it already applies to every generic Rust function.
//!
//! The synthesized body terminates with `Unreachable` and is never executed
//! — the consumer's own `.o` supplies the real definition at link time, and
//! the partitioner override (stage 4a, see `queries/partition.rs`) removes
//! consumer items from rustc's CGU slice so rustc's codegen dispatch never
//! sees them. Non-consumer DefIds delegate to the saved upstream default
//! — no behavior change for ordinary Rust code.

use rustc_middle::mir::*;
use rustc_middle::ty::{self, GenericArgs, Instance, Ty, TyCtxt};
use rustc_span::def_id::LocalDefId;

pub type OptimizedMirFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;

/// Override provider for `optimized_mir`. Synthesizes a dependency-registering
/// body for consumer DefIds; delegates to rustc's saved default for
/// everything else.
pub fn lang_optimized_mir<'tcx>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
) -> &'tcx Body<'tcx> {
    let def_id = local_def_id.to_def_id();

    // Non-consumer items: delegate to rustc's default `optimized_mir`.
    if !crate::is_consumer_codegen_target(tcx, def_id) {
        return (crate::default_optimized_mir())(tcx, local_def_id);
    }

    // Compute the consumer's callback name. Accessors get "Struct.field";
    // regular functions get their item name. Mirrors the naming logic that
    // lived in the retired `per_instance.rs` (lines 53–65 of the old file).
    let Some(name) = tcx.opt_item_name(def_id) else {
        return (crate::default_optimized_mir())(tcx, local_def_id);
    };
    let name_str = name.to_string();
    let callback_name = if crate::is_consumer_fn(&name_str) {
        name_str
    } else if let Some(assoc_item) = tcx.opt_associated_item(def_id) {
        let impl_def_id = assoc_item.container_id(tcx);
        // instantiate_identity: structural inspection only — we want the impl's
        // self type with its own params as placeholders so we can read the ADT
        // name. We are not producing a concrete type here.
        let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
        if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
            let struct_name = tcx.item_name(adt_def.did()).to_string();
            format!("{}.{}", struct_name, name_str)
        } else {
            return (crate::default_optimized_mir())(tcx, local_def_id);
        }
    } else {
        return (crate::default_optimized_mir())(tcx, local_def_id);
    };

    // Ask the consumer for the Rust deps this item transitively references.
    // The returned `GenericArgsRef`s may carry Params — the consumer installs
    // its own Param-aware scope for the walk (see
    // `callbacks_impl::collect_generic_rust_deps_inner` and
    // `oracle::ActiveParamMap`). rustc's mono collector substitutes Params
    // per caller during its own walk of the body we return below.
    let rust_deps = crate::call_collect_generic_rust_deps(&callback_name, tcx, local_def_id);

    // Identity-args Instance: args[i] is the Param at declaration position i
    // (plus `re_erased` for any early-bound lifetime slots — see @ELASZ).
    // `build_dependency_body` consumes this to (a) synthesize a fn signature
    // with Params intact and (b) anchor each ReifyFnPointer cast on the
    // pre-substitution dep refs.
    let identity_args = GenericArgs::identity_for_item(tcx, def_id);
    let identity_instance = Instance::new(def_id, identity_args);

    let body = build_dependency_body(tcx, identity_instance, &rust_deps);
    tcx.arena.alloc(body)
}

/// Build a MIR body that references Rust dependencies (so the monomorphization
/// collector discovers them) and terminates with Unreachable (never executed;
/// the stage-4a partitioner override in `queries::partition` removes consumer
/// items from rustc's CGU slice before codegen sees them, and the consumer's
/// own `.o` provides the real definition at link time).
///
/// Moved verbatim from the retired `queries/per_instance.rs`. Accepts
/// Param-containing sigs safely — `TypingEnv::fully_monomorphized()` is a
/// typing-MODE declaration (`PostAnalysis` + `Reveal::All`), not an input
/// assertion, so the normalizer leaves residual Params alone (verified by
/// POC #1 Surprise 1 on branch `poc/optimized-mir-override`).
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

    for &(dep_def_id, dep_args) in rust_deps {
        if tcx.def_kind(dep_def_id).is_fn_like() {
            // Function dependency — emit a ReifyFnPointer cast. Under the
            // optimized_mir override the dep_args may carry Params; rustc's
            // collector substitutes them per caller when it walks this body.
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
            // Type dependency — emit SizeOf so the collector discovers it.
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

    // Single block: dependency statements + Unreachable terminator. The body
    // is never executed — the consumer's .o provides the real implementation
    // and rustc's codegen is skipped by `CODEGEN_SKIP_HOOK`.
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
        IndexVec::new(),
        sig.inputs().len(),
        vec![],
        span,
        None,
        None,
    );
    // Must set these or the collector panics when accessing
    // required_consts/mentioned_items. Our body has neither.
    body.set_required_consts(vec![]);
    body.set_mentioned_items(vec![]);
    body
}
