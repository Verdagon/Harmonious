// Type oracle — resolves Rust generic API signatures by querying TyCtxt directly.

/// The stub wrapper name for toylang's `main` function, avoiding conflict with Rust's `main`.
pub const TOYLANG_MAIN: &str = "__toylang_main";

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def::DefKind;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::def_id::DefId;

/// Walk local HIR definitions to find a struct named `name`.
/// Also resolves `pub use` re-exports to the original struct DefId.
pub fn find_local_struct_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // First: check local struct definitions
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) == DefKind::Struct {
            if tcx.item_name(def_id).as_str() == name {
                return Some(def_id);
            }
        }
    }
    // Second: check module children for re-exports (pub use)
    find_reexported_type(tcx, name)
}

/// Search local module children for a `pub use` re-export matching `name`.
/// Matches Struct and Enum (e.g., Option, Result are enums, not structs).
fn find_reexported_type(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    use rustc_hir::def::Res;
    fn is_type_def<Id>(res: &Res<Id>) -> Option<DefId> {
        match res {
            Res::Def(DefKind::Struct, id) | Res::Def(DefKind::Enum, id) => Some(*id),
            _ => None,
        }
    }
    // module_children_local works for local modules
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if tcx.def_kind(local_def_id.to_def_id()) != DefKind::Mod { continue; }
        for child in tcx.module_children_local(local_def_id) {
            if child.ident.as_str() == name {
                if let Some(def_id) = is_type_def(&child.res) {
                    return Some(def_id);
                }
            }
        }
    }
    // Also check the crate root
    for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
        if child.ident.as_str() == name {
            if let Some(def_id) = is_type_def(&child.res) {
                return Some(def_id);
            }
        }
    }
    None
}

/// Find a named method in a type's inherent impls.
pub fn find_inherent_method(tcx: TyCtxt<'_>, type_def_id: DefId, method: &str) -> Option<DefId> {
    for &impl_id in tcx.inherent_impls(type_def_id) {
        for &item_id in tcx.associated_item_def_ids(impl_id) {
            if tcx.item_name(item_id).as_str() == method {
                return Some(item_id);
            }
        }
    }
    None
}

/// Look up a Rust type's DefId by name.
/// Checks well-known diagnostic items first, then falls back to local/re-exported items.
/// Convert a `ResolvedType` to a rustc `Ty<'tcx>`. Used for layout_of queries
/// and dependency resolution.
pub fn resolved_to_rustc_ty<'tcx>(tcx: TyCtxt<'tcx>, resolved: &crate::toylang::typed_ast::ResolvedType) -> ty::Ty<'tcx> {
    use crate::toylang::typed_ast::ResolvedType;
    match resolved {
        ResolvedType::I32 => tcx.types.i32,
        ResolvedType::I64 => tcx.types.i64,
        ResolvedType::F64 => tcx.types.f64,
        ResolvedType::Bool => tcx.types.bool,
        ResolvedType::Usize => tcx.types.usize,
        ResolvedType::Void => tcx.types.unit,
        ResolvedType::StructRef { name, type_args }
        | ResolvedType::Struct { name, type_args, .. } => {
            let def_id = find_local_struct_def_id(tcx, name)
                .unwrap_or_else(|| panic!("struct '{}' not found", name));
            let adt_def = tcx.adt_def(def_id);
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
                .collect();
            ty::Ty::new_adt(tcx, adt_def, tcx.mk_args(&args))
        }
        ResolvedType::RustType { name, type_args } => {
            // Primitive types that appear as type args in Rust generics
            // (e.g., Option<u8>) are mapped to RustType by rustc_ty_to_resolved_type.
            // Convert them back to rustc primitives here.
            match name.as_str() {
                "u8" => return tcx.types.u8,
                "u16" => return tcx.types.u16,
                "u32" => return tcx.types.u32,
                "u64" => return tcx.types.u64,
                "i8" => return ty::Ty::new_int(tcx, ty::IntTy::I8),
                "i16" => return ty::Ty::new_int(tcx, ty::IntTy::I16),
                "f32" => return ty::Ty::new_float(tcx, ty::FloatTy::F32),
                _ => {}
            }
            let def_id = find_rust_type_def_id(tcx, name)
                .unwrap_or_else(|| panic!("Rust type '{}' not found", name));
            let adt_def = tcx.adt_def(def_id);
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
                .collect();
            ty::Ty::new_adt(tcx, adt_def, tcx.mk_args(&args))
        }
        ResolvedType::Ref { inner } => {
            let inner_ty = resolved_to_rustc_ty(tcx, inner);
            ty::Ty::new_imm_ref(tcx, tcx.lifetimes.re_erased, inner_ty)
        }
        ResolvedType::Str => panic!("Str type should not need rustc Ty conversion"),
        ResolvedType::ByteSlice => ty::Ty::new_slice(tcx, tcx.types.u8),
        ResolvedType::TypeParam(name) => panic!("TypeParam '{}' should be substituted before rustc Ty conversion", name),
    }
}

/// Convert a rustc `Ty<'tcx>` to a `ResolvedType`. Inverse of `resolved_to_rustc_ty`.
pub fn rustc_ty_to_resolved_type<'tcx>(tcx: TyCtxt<'tcx>, ty: ty::Ty<'tcx>) -> crate::toylang::typed_ast::ResolvedType {
    use crate::toylang::typed_ast::ResolvedType;
    use rustc_middle::ty::TyKind;
    match ty.kind() {
        TyKind::Int(int_ty) => match int_ty {
            ty::IntTy::I32 => ResolvedType::I32,
            ty::IntTy::I64 => ResolvedType::I64,
            // Unsupported as toylang primitives but may appear as type args in
            // Rust generic types (e.g., HashMap<i8, ...>). Map to opaque RustType.
            other => ResolvedType::RustType { name: format!("{}", other.name_str()), type_args: vec![] },
        },
        TyKind::Uint(uint_ty) => match uint_ty {
            ty::UintTy::Usize => ResolvedType::Usize,
            other => ResolvedType::RustType { name: format!("{}", other.name_str()), type_args: vec![] },
        },
        TyKind::Float(float_ty) => match float_ty {
            ty::FloatTy::F64 => ResolvedType::F64,
            other => ResolvedType::RustType { name: format!("{}", other.name_str()), type_args: vec![] },
        },
        TyKind::Bool => ResolvedType::Bool,
        TyKind::Tuple(tys) if tys.is_empty() => ResolvedType::Void,
        TyKind::Ref(_, inner, _) => ResolvedType::Ref {
            inner: Box::new(rustc_ty_to_resolved_type(tcx, *inner)),
        },
        TyKind::Adt(adt_def, args) => {
            let name = tcx.item_name(adt_def.did()).to_string();
            let type_args: Vec<ResolvedType> = args.iter()
                .filter_map(|a| match a.unpack() {
                    ty::GenericArgKind::Type(t) => Some(rustc_ty_to_resolved_type(tcx, t)),
                    _ => None,
                })
                .collect();
            // Check if this is a toylang struct (defined in __lang_stubs)
            if rustc_lang_facade::is_from_lang_stubs(tcx, adt_def.did()) {
                ResolvedType::StructRef { name, type_args }
            } else {
                ResolvedType::RustType { name, type_args }
            }
        }
        TyKind::Slice(elem_ty) => {
            if matches!(elem_ty.kind(), TyKind::Uint(ty::UintTy::U8)) {
                ResolvedType::ByteSlice
            } else {
                panic!("rustc_ty_to_resolved_type: unsupported slice element type {:?}", elem_ty)
            }
        }
        // Types that may appear as type args in Rust generic types (e.g., inside
        // Result<(), Error> or HashMap internals). Toylang never inspects these —
        // they pass through as opaque RustType values.
        TyKind::Str => ResolvedType::RustType { name: "str".to_string(), type_args: vec![] },
        TyKind::Never => ResolvedType::RustType { name: "never".to_string(), type_args: vec![] },
        TyKind::RawPtr(inner, _) => ResolvedType::RustType {
            name: "raw_ptr".to_string(),
            type_args: vec![rustc_ty_to_resolved_type(tcx, *inner)],
        },
        TyKind::Dynamic(..) => ResolvedType::RustType { name: "dyn_trait".to_string(), type_args: vec![] },
        TyKind::Tuple(tys) => ResolvedType::RustType {
            name: "tuple".to_string(),
            type_args: tys.iter().map(|t| rustc_ty_to_resolved_type(tcx, t)).collect(),
        },
        _ => panic!("rustc_ty_to_resolved_type: unsupported type {:?}", ty),
    }
}

/// Convert a ResolvedType to a string suitable for symbol mangling.
pub fn resolved_type_to_mangled_name(ty: &crate::toylang::typed_ast::ResolvedType) -> String {
    use crate::toylang::typed_ast::ResolvedType;
    match ty {
        ResolvedType::I32 => "i32".to_string(),
        ResolvedType::I64 => "i64".to_string(),
        ResolvedType::F64 => "f64".to_string(),
        ResolvedType::Bool => "bool".to_string(),
        ResolvedType::Usize => "usize".to_string(),
        ResolvedType::Void => "void".to_string(),
        ResolvedType::StructRef { name, type_args }
        | ResolvedType::Struct { name, type_args, .. }
        | ResolvedType::RustType { name, type_args } => {
            if type_args.is_empty() {
                name.clone()
            } else {
                let args: Vec<String> = type_args.iter().map(resolved_type_to_mangled_name).collect();
                format!("{}_LT_{}_GT_", name, args.join("_"))
            }
        }
        ResolvedType::Ref { inner } => format!("ref_{}", resolved_type_to_mangled_name(inner)),
        ResolvedType::TypeParam(name) => panic!("resolved_type_to_mangled_name: unresolved TypeParam '{}' during mangling", name),
        ResolvedType::Str => "str".to_string(),
        ResolvedType::ByteSlice => "byte_slice".to_string(),
    }
}

/// Query rustc for a Rust method's return type, converting to ResolvedType.
pub fn rust_method_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> crate::toylang::typed_ast::ResolvedType {
    let type_def_id = find_rust_type_def_id(tcx, type_name)
        .unwrap_or_else(|| panic!("Rust type '{}' not found", type_name));
    let method_def_id = find_inherent_method(tcx, type_def_id, method_name)
        .unwrap_or_else(|| panic!("method '{}' not found on '{}'", method_name, type_name));

    // Build generic args from type_args
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
        .collect();
    let expected_count = tcx.generics_of(method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);

    // Query fn_sig and extract return type
    let sig = tcx.fn_sig(method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    rustc_ty_to_resolved_type(tcx, sig.output())
}

/// Find an extern (non-toylang) function by name among local definitions.
/// Excludes functions in __lang_stubs (those are toylang wrappers).
pub fn find_extern_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) != DefKind::Fn { continue; }
        if tcx.item_name(def_id).as_str() != name { continue; }
        if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
    }
    None
}

pub fn find_rust_type_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // All Rust types must be imported via `use` in toylang source.
    // The stub generator emits `pub use` re-exports, and
    // find_local_struct_def_id finds them via module_children_local.
    find_local_struct_def_id(tcx, name)
}

/// Find a trait DefId by name among `pub use` re-exports in __lang_stubs.
pub fn find_use_imported_trait_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    use rustc_hir::def::Res;
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if tcx.def_kind(local_def_id.to_def_id()) != DefKind::Mod { continue; }
        for child in tcx.module_children_local(local_def_id) {
            if child.ident.as_str() == name {
                if let Res::Def(DefKind::Trait, target_def_id) = child.res {
                    return Some(target_def_id);
                }
            }
        }
    }
    for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
        if child.ident.as_str() == name {
            if let Res::Def(DefKind::Trait, target_def_id) = child.res {
                return Some(target_def_id);
            }
        }
    }
    None
}

/// Find a use-imported free function by name among `pub use` re-exports in __lang_stubs.
pub fn find_use_imported_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    use rustc_hir::def::Res;
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if tcx.def_kind(local_def_id.to_def_id()) != DefKind::Mod { continue; }
        for child in tcx.module_children_local(local_def_id) {
            if child.ident.as_str() == name {
                if let Res::Def(DefKind::Fn, def_id) = child.res {
                    return Some(def_id);
                }
            }
        }
    }
    for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
        if child.ident.as_str() == name {
            if let Res::Def(DefKind::Fn, def_id) = child.res {
                return Some(def_id);
            }
        }
    }
    None
}

/// Instantiate a free function's fn_sig with given type args.
/// Non-generic functions pass an empty slice — same code path, no special-casing.
fn instantiate_free_fn_sig<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> ty::FnSig<'tcx> {
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
        .collect();
    let expected_count = tcx.generics_of(def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(def_id).instantiate(tcx, args);
    tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig)
}

/// Return type of a use-imported free function. None if not found.
pub fn rust_free_fn_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<crate::toylang::typed_ast::ResolvedType> {
    let def_id = find_use_imported_fn_def_id(tcx, name)?;
    let sig = instantiate_free_fn_sig(tcx, def_id, type_args);
    Some(rustc_ty_to_resolved_type(tcx, sig.output()))
}

/// Param types of a use-imported free function. None if not found.
pub fn rust_free_fn_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<Vec<crate::toylang::typed_ast::ResolvedType>> {
    let def_id = find_use_imported_fn_def_id(tcx, name)?;
    let sig = instantiate_free_fn_sig(tcx, def_id, type_args);
    Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect())
}

/// Param types of a Rust inherent method. None if type or method not found.
pub fn rust_method_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<Vec<crate::toylang::typed_ast::ResolvedType>> {
    let type_def_id = find_rust_type_def_id(tcx, type_name)?;
    let method_def_id = find_inherent_method(tcx, type_def_id, method_name)?;
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
        .collect();
    let expected_count = tcx.generics_of(method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig);
    Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect())
}

/// Param types of a trait method. None if not found.
pub fn rust_trait_method_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    trait_name: &str,
    method_name: &str,
    receiver_ty: &crate::toylang::typed_ast::ResolvedType,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<Vec<crate::toylang::typed_ast::ResolvedType>> {
    let trait_def_id = find_use_imported_trait_def_id(tcx, trait_name)?;
    let self_resolved = strip_ref(receiver_ty);
    let self_ty = resolved_to_rustc_ty(tcx, self_resolved);
    let trait_method_def_id = tcx.associated_item_def_ids(trait_def_id)
        .iter()
        .find(|&&id| tcx.item_name(id).as_str() == method_name)
        .copied()?;
    // Per @TVIMDGAZ, args are [Self, ...explicit] for the trait definition's method DefId
    let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
    for ta in type_args {
        all_ty_args.push(ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)));
    }
    let expected_count = tcx.generics_of(trait_method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig);
    Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect())
}

/// Find a trait method's DefId given a trait and the receiver's concrete type.
/// Searches all impls of the trait that match `self_ty`.
pub fn find_trait_method<'tcx>(
    tcx: TyCtxt<'tcx>,
    trait_def_id: DefId,
    self_ty: ty::Ty<'tcx>,
    method: &str,
) -> Option<DefId> {
    let mut result = None;
    tcx.for_each_relevant_impl(trait_def_id, self_ty, |impl_def_id| {
        if result.is_some() { return; }
        for &item_id in tcx.associated_item_def_ids(impl_def_id) {
            if tcx.item_name(item_id).as_str() == method {
                result = Some(item_id);
            }
        }
    });
    result
}

/// Strip `Ref` wrappers from a ResolvedType to get the underlying type.
/// `&Vec<i32>` → `Vec<i32>`, `Vec<i32>` → `Vec<i32>`.
pub fn strip_ref(ty: &crate::toylang::typed_ast::ResolvedType) -> &crate::toylang::typed_ast::ResolvedType {
    use crate::toylang::typed_ast::ResolvedType;
    match ty {
        ResolvedType::Ref { inner } => strip_ref(inner),
        other => other,
    }
}

/// Query rustc for a trait method's return type.
/// `trait_name` is the trait (e.g. "Write"), `receiver_ty` is the concrete receiver
/// type (e.g. &Stdout or Stdout), `method_name` is the method (e.g. "write_all").
/// The `Ref` wrapper is stripped from `receiver_ty` to get the Self type for impl lookup.
pub fn rust_trait_method_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    trait_name: &str,
    method_name: &str,
    receiver_ty: &crate::toylang::typed_ast::ResolvedType,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> crate::toylang::typed_ast::ResolvedType {
    let trait_def_id = find_use_imported_trait_def_id(tcx, trait_name)
        .unwrap_or_else(|| panic!("trait '{}' not found", trait_name));
    // Per @TVIMDGAZ, strip &ref to get Self and use the trait definition's method DefId
    let self_resolved = strip_ref(receiver_ty);
    let self_ty = resolved_to_rustc_ty(tcx, self_resolved);

    let trait_method_def_id = {
        let mut found = None;
        for &item_id in tcx.associated_item_def_ids(trait_def_id) {
            if tcx.item_name(item_id).as_str() == method_name {
                found = Some(item_id);
                break;
            }
        }
        found.unwrap_or_else(|| panic!("method '{}' not defined on trait '{}'", method_name, trait_name))
    };

    // Per @TVIMDGAZ, args are [Self, ...explicit] for the trait definition's method DefId
    let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
    for ta in type_args {
        all_ty_args.push(ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)));
    }
    let expected_count = tcx.generics_of(trait_method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);

    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    rustc_ty_to_resolved_type(tcx, sig.output())
}
