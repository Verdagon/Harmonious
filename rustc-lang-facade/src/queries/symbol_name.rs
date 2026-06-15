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
//! For consumer items, it calls `call_notify_concrete_entry_point`, which
//! locks `MUTABLE_STATE`. Before the B6 fix this was the side-effect that
//! populated `state.toylang_instances`; post-B6 the populate step moved to
//! an up-front CGU walk in `generate_and_compile` and
//! `notify_concrete_entry_point_inner` is pure (aside from the log push).
//! See `docs/architecture/risks.md` §B6 and `@GCMLZ` for the full story,
//! including the `_inner` bypass used during codegen to avoid re-locking.
//!
//! @SyMINCZ — computing a symbol name here does NOT force rustc to codegen
//! the `Instance`. It is a pure read. Codegen for consumer-referenced Rust
//! generics is driven exclusively by the `ReifyFnPointer` casts synthesized
//! in the `per_instance_mir` override's `build_dependency_body`. A reader who
//! sees this file as "the place where the consumer's symbol enters LLVM IR"
//! and assumes it also drives codegen is in the trap the @SyMINCZ arcana
//! documents.

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
            // Phase 2 C.6: classify the three consumer-owned symbol shapes.
            // Trait-impl methods are checked BEFORE the accessor path so the
            // shared `is_consumer_accessor_safe` excludes them (also enforced
            // by the predicate's own `impl_trait_ref` check, but order makes
            // the intent obvious here).
            let trait_impl = if !is_fn {
                crate::is_consumer_trait_impl_method(tcx, def_id)
            } else {
                None
            };
            let is_accessor = if !is_fn && trait_impl.is_none() {
                crate::is_consumer_accessor_safe(tcx, def_id)
            } else {
                false
            };

            if is_fn || is_accessor || trait_impl.is_some() {
                // Build callback name (must match the one consumers key on
                // in their `notify_concrete_entry_point_inner` switch).
                let callback_name = if let Some((self_n, trait_n, method_n)) = &trait_impl {
                    // Phase 2 C.6 — trait-impl method shape:
                    //   `__impl_method__<Self>__<Trait>__<method>`
                    // distinct from the accessor pattern (`<Self>.<m>`) so
                    // consumers can route them to a separate mangler.
                    format!("__impl_method__{}__{}__{}", self_n, trait_n, method_n)
                } else if is_accessor {
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

                // Stage 5b: rewrite consumer symbols regardless of local/extern.
                // In the rlib compile consumer fns are local. In the user-bin
                // compile they're extern (in the `__lang_stubs` rlib). Rustc
                // re-queries `symbol_name` locally for cross-crate references
                // rather than reading from metadata, so the user-bin compile
                // sees these DefIds here too and must rewrite so its call sites
                // target the consumer-chosen symbol (`__toylang_impl_*`) that
                // the rlib's `.o` defines.
                let symbol = crate::call_notify_concrete_entry_point(
                    &callback_name, tcx, instance,
                );
                return ty::SymbolName::new(tcx, &symbol);
            }
        }
    }

    // Per @GCMLZ, default_symbol_name() reads from OnceLock (no mutex lock).
    let default = crate::default_symbol_name();
    default(tcx, instance)
}
