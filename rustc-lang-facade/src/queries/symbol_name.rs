//! symbol_name query override — map consumer function instances to consumer symbol names.
//!
//! When rustc needs the linker symbol for a consumer function instance,
//! we return the consumer's symbol name (e.g., __toylang_impl_make_counter)
//! instead of rustc's default mangled name. This ensures that call sites
//! in other functions emit calls to the consumer's extern symbol, which
//! the consumer's .o provides.
//!
//! Per @GCMLZ, this provider may fire during generate_and_compile. For
//! non-consumer items, it only reads CONFIG and DEFAULT_SYMBOL_NAME (no lock).
//! For consumer items, it calls call_monomorphize_fn (locks MUTABLE_STATE),
//! but those are always cached from inner.codegen_crate.

use rustc_middle::ty::{self, Instance, TyCtxt};

pub type SymbolNameFn = for<'tcx> fn(TyCtxt<'tcx>, Instance<'tcx>) -> ty::SymbolName<'tcx>;

pub fn lang_symbol_name<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
) -> ty::SymbolName<'tcx> {
    let def_id = instance.def_id();

    // Only override consumer functions from __lang_stubs.
    if crate::is_from_lang_stubs(tcx, def_id) {
        let name = tcx.opt_item_name(def_id);
        if let Some(name) = name {
            let name_str = name.to_string();
            let is_fn = crate::is_consumer_fn(&name_str);
            let is_accessor = if !is_fn {
                super::per_instance::is_consumer_accessor_pub(tcx, def_id)
            } else {
                false
            };

            if is_fn || is_accessor {
                // Build callback name (same as per_instance_mir)
                let callback_name = if is_accessor {
                    if let Some(assoc_item) = tcx.opt_associated_item(def_id) {
                        let impl_def_id = assoc_item.container_id(tcx);
                        // instantiate_identity: structural inspection only — we want the
                        // impl's self type with its own params as placeholders so we can
                        // read the ADT name. We are not producing a concrete type here.
                        let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
                        if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
                            let struct_name = tcx.item_name(adt_def.did()).to_string();
                            format!("{}.{}", struct_name, name_str)
                        } else {
                            name_str.clone()
                        }
                    } else {
                        name_str.clone()
                    }
                } else {
                    name_str.clone()
                };

                if let Some(local_def_id) = def_id.as_local() {
                    let result = crate::call_monomorphize_fn(
                        &callback_name, tcx, local_def_id, instance,
                    );
                    return ty::SymbolName::new(tcx, &result.extern_symbol);
                }
            }
        }
    }

    // Per @GCMLZ, default_symbol_name() reads from OnceLock (no mutex lock).
    let default = crate::default_symbol_name();
    default(tcx, instance)
}
