//! `per_instance_mir` override — Instance-keyed synthetic MIR for consumer functions.
//!
//! Restored from the stage-3 retirement (commit `bf770ae`); see
//! `course-correct.md` item #1 and `rust-interop-architecture.md` §3.1 / §19
//! for the rationale. The query slot is added by 3 rustc fork patches; this
//! file is the facade-side provider that installs into that slot via
//! `Config::override_queries`.
//!
//! Contract: rustc's mono collector calls `per_instance_mir(instance)` before
//! falling through to `instance_mir(instance.def)`. We return `Some(body)`
//! for items the consumer owns (filtered by `is_consumer_codegen_target`),
//! `None` for everything else — non-consumer items fall through to vanilla
//! rustc behavior unchanged.
//!
//! The returned synthetic body terminates with `Unreachable` and is never
//! executed — the consumer's own `.o` supplies the real definition at link
//! time, and the partitioner override in `queries::partition` removes
//! consumer items from rustc's CGU slice so rustc's codegen dispatch never
//! sees them.
//!
//! **Approach A vs B.** This is Approach A (Instance-keyed; the provider sees
//! `instance.args` directly). Under Sky's design, the substitution that
//! produces those concrete args may involve Sky-side comptime evaluation
//! that rustc's collector cannot replicate (arbitrary-typed comptime
//! parameters; see `rust-interop-architecture.md` §3.1, §13.7.5). Toylang
//! does not need comptime, but the facade lives in Approach A so that Sky's
//! future comptime evaluator has a place to hook in — see the SKY-COMPTIME-HOOK
//! marker in `build_dependency_body`.
//!
//! Under Approach A, this query fires `O(consumer_fns × monomorphizations)`
//! times — once per concrete `Instance` rustc's collector encounters — versus
//! `O(consumer_fns)` under the retired Approach B `optimized_mir` override.
//! For Sky's larger projects this is bounded by per-Instance memoization
//! (`rust-interop-architecture.md` §19.5); not implemented in this file yet.

use rustc_middle::mir::*;
use rustc_middle::ty::{self, Instance, Ty, TyCtxt};

pub type PerInstanceMirFn =
    for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> Option<&'tcx Body<'tcx>>;

/// Override provider for `per_instance_mir`. Returns `Some(synthetic_body)`
/// for consumer Instances, `None` for everything else (falls through to
/// `instance_mir` in the rustc collector — patch 2 of the fork).
pub fn lang_per_instance_mir<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
) -> Option<&'tcx Body<'tcx>> {
    let def_id = instance.def_id();

    // Non-consumer items: return None so the collector queries instance_mir.
    if !crate::is_consumer_codegen_target(tcx, def_id) {
        return None;
    }

    // Compute the consumer's callback name. Accessors get "Struct.field";
    // regular functions get their item name. Mirrors the historical logic
    // in the retired `queries/per_instance.rs` at `bf770ae^` (lines 53–65
    // of the reference copy at `docs/historical/approach-a-reference/per_instance.rs`).
    let name = tcx.opt_item_name(def_id)?;
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
            return None;
        }
    } else {
        return None;
    };

    // Ask the consumer for the Rust deps this Instance transitively references.
    // Per Approach A's contract, the consumer substitutes `instance.args`
    // Sky-side and returns fully-concrete `GenericArgsRef`s. The body emitted
    // below contains no `ty::TyKind::Param` placeholders — every cast target
    // is a concrete monomorphization that rustc's collector queues directly.
    // The Sky-side substitution invariant is checked in `build_dependency_body`
    // via debug_assert.
    let rust_deps = crate::call_collect_generic_rust_deps(&callback_name, tcx, instance);

    let body = build_dependency_body(tcx, instance, &rust_deps);
    Some(tcx.arena.alloc(body))
}

/// Build a MIR body that references Rust dependencies (so the monomorphization
/// collector discovers them) and terminates with `Unreachable`.
///
/// The body is never executed — the consumer's `.o` provides the real
/// implementation at link time, and the partitioner override in
/// `queries::partition` filters consumer items out of rustc's CGU slice
/// before codegen dispatch sees them.
///
/// This is the designated "fix site" per @SyMINCZ — the only mechanism in the
/// codebase that forces rustc's mono collector to codegen a generic Rust
/// `Instance` the consumer references. `tcx.symbol_name` and
/// `Instance::expect_resolve` elsewhere are pure reads; the `ReifyFnPointer`
/// casts synthesized here are what put dep Instances into the collector's
/// `used_items` queue.
///
/// Body construction mechanics (locals, ReifyFnPointer casts, Unreachable
/// terminator, `set_required_consts(vec![])` / `set_mentioned_items(vec![])`)
/// are inherited byte-for-byte from the retired Approach B `optimized_mir.rs`
/// (which had inherited them from the original Approach A `per_instance.rs`
/// at `bf770ae^` via the stage-3 migration). The only thing that changes
/// between the two approaches is the args feeding it: under B, identity args
/// with Params; under A, the concrete `instance.args`.
fn build_dependency_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
    rust_deps: &[(rustc_span::def_id::DefId, ty::GenericArgsRef<'tcx>)],
) -> Body<'tcx> {
    use rustc_index::IndexVec;
    use rustc_middle::ty::TypeVisitableExt;

    let def_id = instance.def_id();

    // Approach A invariant (rust-interop-architecture.md §3.1, §19.1):
    // `instance.args` and every entry in `rust_deps` must be fully concrete in
    // the consumer's universe — Sky-side substitution has replaced every
    // `ty::TyKind::Param` before this function is reached. Param-bearing args
    // here are a substitution bug, not a routine case. The check is debug-only
    // because the per_instance_mir query fires O(consumer_fns ×
    // monomorphizations) times and we don't want the predicate walk in release.
    debug_assert!(
        !instance.args.has_param(),
        "build_dependency_body: Param-bearing instance.args reached the facade — \
         consumer should have substituted Sky-side before returning. instance = {:?}",
        instance,
    );
    debug_assert!(
        rust_deps.iter().all(|(_, args)| !args.has_param()),
        "build_dependency_body: Param-bearing dep args reached the facade — \
         consumer's collect_generic_rust_deps should have substituted Sky-side. \
         instance = {:?}, deps = {:?}",
        instance, rust_deps,
    );

    // SKY-COMPTIME-HOOK: under Sky's architecture (rust-interop-architecture.md
    // §13.7.5, §19.1), `instance.args` may contain entries that are Sky-typed
    // comptime values represented as slab pointers (u64 offsets). Sky's
    // comptime evaluator would materialize those values *before* the
    // substitution below, so the resulting sig is concrete in Sky's universe.
    // Toylang has no comptime, so this hook is a no-op for now. The plug-in
    // point is here, at the boundary between Instance receipt and MIR
    // construction.
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
        // Every dep the consumer returns is fn-like by contract (functions,
        // methods, trait methods, Phase-6 wrapper shims) — toylangc's
        // `collect_rust_deps_recursive` only pushes function/method DefIds.
        // Non-fn deps should route through the `layout_of` query path instead
        // (which is what toylangc does via `monomorphize_type`).
        assert!(
            tcx.def_kind(dep_def_id).is_fn_like(),
            "consumer returned non-fn dep {:?} from collect_generic_rust_deps; \
             use the layout_of / monomorphize_type path for type discovery",
            dep_def_id,
        );

        // Function dependency — emit a ReifyFnPointer cast. Per Approach A
        // (debug_asserted at the top of this function), dep_args are fully
        // concrete; the cast directly references the concrete monomorphization
        // that rustc's collector queues.
        let fn_def_ty = Ty::new_fn_def(tcx, dep_def_id, dep_args);
        let fn_sig = tcx.fn_sig(dep_def_id).instantiate(tcx, dep_args);
        let fn_ptr_ty = Ty::new_fn_ptr(tcx, fn_sig);
        let fn_ptr_local = local_decls.push(LocalDecl::new(fn_ptr_ty, span));

        stmts.push(Statement::new(
            source_info,
            StatementKind::Assign(Box::new((
                Place::from(fn_ptr_local),
                Rvalue::Cast(
                    CastKind::PointerCoercion(
                        ty::adjustment::PointerCoercion::ReifyFnPointer(rustc_hir::Safety::Safe),
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
        ));
    }

    blocks.push(BasicBlockData::new_stmts(
        stmts,
        Some(Terminator {
            source_info,
            kind: TerminatorKind::Unreachable,
        }),
        false,
    ));

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
