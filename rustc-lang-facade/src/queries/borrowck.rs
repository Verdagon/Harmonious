//! Borrow check override — skip for consumer-defined functions.
//!
//! The MIR bodies we inject (extern call stubs from `mir_helpers`) are not valid
//! from rustc's borrow checker perspective — they use `Move` and `Copy` in ways
//! that violate Rust's ownership rules. This is by design: the consumer's language
//! has its own safety model (which could include linear types, deferred borrows,
//! automatic refcounting, etc.), and the consumer's own type checker verifies
//! safety. Rustc's borrow checker should not re-check our injected MIR.
//!
//! We return an empty `BorrowCheckResult` for consumer items — "nothing to report."
//! Normal Rust functions still go through full borrow checking.
//!
//! Consumer items are detected by:
//! 1. File extension (.toylang) — for future source file loading
//! 2. Name lookup against registered consumer function names — current approach

#![allow(unused)]

use rustc_hir::def_id::LocalDefId;
use rustc_middle::mir::BorrowCheckResult;
use rustc_middle::ty::TyCtxt;
use std::sync::OnceLock;

type BorrowckFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx BorrowCheckResult<'tcx>;

static DEFAULT_MIR_BORROWCK: OnceLock<BorrowckFn> = OnceLock::new();

pub fn save_default(f: BorrowckFn) {
    let _ = DEFAULT_MIR_BORROWCK.set(f);
}

/// Skip borrow checking for consumer-defined functions. Returns an empty
/// `BorrowCheckResult` (no errors, no region info). Falls through to
/// rustc's default for Rust functions.
pub fn toy_mir_borrowck<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx BorrowCheckResult<'tcx> {
    if is_consumer_item(tcx, def_id) {
        // Skip borrow checking — hand-built MIR bodies won't pass it.
        tcx.arena.alloc(BorrowCheckResult {
            concrete_opaque_types: Default::default(),
            closure_requirements: None,
            used_mut_upvars: Default::default(),
            tainted_by_errors: None,
        })
    } else {
        let default = DEFAULT_MIR_BORROWCK.get().expect("default mir_borrowck not saved");
        default(tcx, def_id)
    }
}

fn is_consumer_item(tcx: TyCtxt<'_>, def_id: LocalDefId) -> bool {
    if !crate::is_from_lang_stubs(tcx, def_id.to_def_id()) {
        return false;
    }
    if let Some(name) = tcx.opt_item_name(def_id.to_def_id()) {
        if crate::is_consumer_fn(name.as_str()) {
            return true;
        }
    }
    // Non-generic accessor methods on consumer types
    if let Some(assoc_item) = tcx.opt_associated_item(def_id.to_def_id()) {
        if let Some(impl_def_id) = assoc_item.container_id(tcx).as_local() {
            let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
            if let rustc_middle::ty::TyKind::Adt(adt_def, args) = self_ty.kind() {
                if args.is_empty() {
                    let struct_name = tcx.item_name(adt_def.did()).to_string();
                    if crate::is_consumer_type(&struct_name) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
