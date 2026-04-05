//! mir_built query override — generate MIR call stubs for consumer functions.
//!
//! When rustc encounters a consumer-defined function, it needs a MIR body.
//! Instead of real code, we generate a thin call stub: an extern call to the
//! consumer's compiled function (`__toylang_impl_make_vec`, etc.) plus phantom
//! `ReifyFnPointer` casts that trigger monomorphization of Rust generic deps.
//!
//! The flow:
//! 1. Rustc's monomorphizer encounters `make_vec()` (a consumer function)
//! 2. This override fires, calls the consumer's `monomorphize_fn` callback
//! 3. Consumer returns `MonomorphizeFnResult { extern_symbol, rust_deps }`
//! 4. We call `mir_helpers::build_extern_call_body` to construct the MIR stub
//! 5. The stub contains:
//!    - ReifyFnPointer casts for each rust_dep (forces monomorphization)
//!    - A call to the extern symbol (the consumer's compiled function)
//! 6. Rustc's monomorphizer sees the casts → stamps out Vec::push<Point>, etc.
//!
//! Why ReifyFnPointer and not `mentioned_items`? See
//! `docs/historical/design-monomorphization-triggers.md` — ReifyFnPointer produces
//! "used" items (guaranteed codegen), while mentioned_items produces "mentioned"
//! items (not guaranteed, semantics can change between nightlies).

#![allow(unused)]

use rustc_data_structures::steal::Steal;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::mir::Body;
use rustc_middle::ty::TyCtxt;
use std::sync::OnceLock;

type MirBuiltFn = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>;

static DEFAULT_MIR_BUILT: OnceLock<MirBuiltFn> = OnceLock::new();

pub fn save_default(f: MirBuiltFn) {
    let _ = DEFAULT_MIR_BUILT.set(f);
}

/// Generate a MIR call stub for a consumer-defined function.
/// Falls through to rustc's default for Rust functions.
pub fn toy_mir_built<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Steal<Body<'tcx>> {
    if let Some(fn_name) = consumer_fn_name(tcx, def_id) {
        eprintln!("[toylang] mir_built intercepted for: {}", fn_name);

        // Ask the consumer to monomorphize this function.
        // Returns the extern symbol + rust deps.
        // mir_built only has the generic definition, not a concrete instance.
        // Use Instance::mono since this body is only for type checking
        // (per_instance_mir provides the real body during monomorphization).
        let instance = rustc_middle::ty::Instance::mono(tcx, def_id.to_def_id());
        let result = crate::call_monomorphize_fn(&fn_name, tcx, def_id, instance);

        eprintln!("[toylang] external codegen for '{}': {} rust deps",
            fn_name, result.rust_deps.len());

        let body = crate::mir_helpers::build_extern_call_body(
            tcx, def_id, &result.extern_symbol, &result.rust_deps,
        );

        return tcx.arena.alloc(Steal::new(body));
    }

    let default = DEFAULT_MIR_BUILT.get().expect("default mir_built not saved");
    default(tcx, def_id)
}

/// If this function is a consumer-defined function (in __lang_stubs), return its name.
/// For accessor methods on non-generic consumer types, returns "StructName.field_name".
fn consumer_fn_name(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<String> {
    if !crate::is_from_lang_stubs(tcx, def_id.to_def_id()) {
        return None;
    }
    let name = tcx.opt_item_name(def_id.to_def_id())?.to_string();
    // Regular consumer function (non-generic only).
    // Generic functions are handled by per_instance_mir at monomorphization time.
    if crate::is_consumer_fn(&name) {
        let generics = tcx.generics_of(def_id.to_def_id());
        if generics.count() == 0 {
            return Some(name);
        }
        // Generic function — let per_instance_mir handle it
        return None;
    }
    // Non-generic accessor method on a consumer type.
    // Generic accessor methods use inline Rust pointer math (STOPGAP).
    if let Some(assoc_item) = tcx.opt_associated_item(def_id.to_def_id()) {
        if let Some(impl_def_id) = assoc_item.container_id(tcx).as_local() {
            let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
            if let rustc_middle::ty::TyKind::Adt(adt_def, args) = self_ty.kind() {
                // Only non-generic (no type params in the impl)
                if args.is_empty() {
                    let struct_name = tcx.item_name(adt_def.did()).to_string();
                    if crate::is_consumer_type(&struct_name) {
                        return Some(format!("{}.{}", struct_name, name));
                    }
                }
            }
        }
    }
    None
}
