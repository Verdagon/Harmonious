// Type oracle — resolves Rust generic API signatures by querying TyCtxt directly.

/// The stub wrapper name for toylang's `main` function, avoiding conflict with Rust's `main`.
pub const TOYLANG_MAIN: &str = "__toylang_main";

/// Per @RTMEIZ, returned when `resolved_to_rustc_ty` can't find a Rust
/// type in the `__lang_stubs` registry. The `context` tells the user
/// *why* the type was needed (trait-call Self, generic arg, etc.) so the
/// error message is actionable.
#[derive(Debug, Clone)]
pub struct UnresolvedRustType {
    pub name: String,
    pub context: RustTypeLookupContext,
}

#[derive(Debug, Clone)]
pub enum RustTypeLookupContext {
    TraitCallSelf { trait_name: String, method: String },
    TraitMethodTypeArg { trait_name: String, method: String },
    InherentMethodTypeArg { type_name: String, method: String },
    FreeFunctionTypeArg { function_name: String },
    NestedGenericArg { parent_type: String },
    StructField { struct_name: String },
    Codegen,
}

impl std::fmt::Display for RustTypeLookupContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TraitCallSelf { trait_name, method } =>
                write!(f, "as Self of trait call `{}::{}`", trait_name, method),
            Self::TraitMethodTypeArg { trait_name, method } =>
                write!(f, "as type arg of trait call `{}::{}`", trait_name, method),
            Self::InherentMethodTypeArg { type_name, method } =>
                write!(f, "as type arg of method `{}::{}`", type_name, method),
            Self::FreeFunctionTypeArg { function_name } =>
                write!(f, "as type arg of free function `{}`", function_name),
            Self::NestedGenericArg { parent_type } =>
                write!(f, "as generic arg inside `{}`", parent_type),
            Self::StructField { struct_name } =>
                write!(f, "as field type in struct `{}`", struct_name),
            Self::Codegen =>
                write!(f, "during codegen"),
        }
    }
}

impl std::fmt::Display for UnresolvedRustType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rust type `{}` is not imported (used {}). Add `use <path>::{}` at the top of your source.",
            self.name, self.context, self.name)
    }
}

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

/// Convert a `ResolvedType` to a rustc `Ty<'tcx>`. Used for layout_of queries
/// and dependency resolution. Returns `Err(UnresolvedRustType)` per @RTMEIZ
/// when a Rust type isn't `use`-imported in the toylang source.
pub fn try_resolved_to_rustc_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    resolved: &crate::toylang::typed_ast::ResolvedType,
    context: &RustTypeLookupContext,
) -> Result<ty::Ty<'tcx>, UnresolvedRustType> {
    use crate::toylang::typed_ast::ResolvedType;
    match resolved {
        ResolvedType::I32 => Ok(tcx.types.i32),
        ResolvedType::I64 => Ok(tcx.types.i64),
        ResolvedType::F64 => Ok(tcx.types.f64),
        ResolvedType::Bool => Ok(tcx.types.bool),
        ResolvedType::Usize => Ok(tcx.types.usize),
        ResolvedType::Void => Ok(tcx.types.unit),
        ResolvedType::StructRef { name, type_args }
        | ResolvedType::Struct { name, type_args, .. } => {
            let def_id = find_local_struct_def_id(tcx, name)
                .unwrap_or_else(|| panic!("struct '{}' not found", name));
            let adt_def = tcx.adt_def(def_id);
            let nested = RustTypeLookupContext::NestedGenericArg { parent_type: name.clone() };
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &nested)?)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ty::Ty::new_adt(tcx, adt_def, tcx.mk_args(&args)))
        }
        ResolvedType::RustType { name, type_args } => {
            // Primitive types that appear as type args in Rust generics
            // (e.g., Option<u8>) are mapped to RustType by rustc_ty_to_resolved_type.
            // Convert them back to rustc primitives here.
            match name.as_str() {
                "u8" => return Ok(tcx.types.u8),
                "u16" => return Ok(tcx.types.u16),
                "u32" => return Ok(tcx.types.u32),
                "u64" => return Ok(tcx.types.u64),
                "i8" => return Ok(ty::Ty::new_int(tcx, ty::IntTy::I8)),
                "i16" => return Ok(ty::Ty::new_int(tcx, ty::IntTy::I16)),
                "f32" => return Ok(ty::Ty::new_float(tcx, ty::FloatTy::F32)),
                _ => {}
            }
            // Per @RTMEIZ, this is the critical site: if the type isn't
            // `use`-imported, return a structured error instead of panicking.
            let def_id = find_rust_type_def_id(tcx, name)
                .ok_or_else(|| UnresolvedRustType {
                    name: name.clone(),
                    context: context.clone(),
                })?;
            let adt_def = tcx.adt_def(def_id);
            let nested = RustTypeLookupContext::NestedGenericArg { parent_type: name.clone() };
            let args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
                .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &nested)?)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ty::Ty::new_adt(tcx, adt_def, tcx.mk_args(&args)))
        }
        ResolvedType::Ref { inner } => {
            let inner_ty = try_resolved_to_rustc_ty(tcx, inner, context)?;
            Ok(ty::Ty::new_imm_ref(tcx, tcx.lifetimes.re_erased, inner_ty))
        }
        ResolvedType::Str => panic!("Str type should not need rustc Ty conversion"),
        ResolvedType::ByteSlice => Ok(ty::Ty::new_slice(tcx, tcx.types.u8)),
        ResolvedType::TypeParam(name) => panic!("TypeParam '{}' should be substituted before rustc Ty conversion", name),
    }
}

/// Convenience wrapper that panics on error — for call sites where context
/// is unavailable or errors have already been validated away (e.g., codegen
/// after `after_rust_analysis` passed).
pub fn resolved_to_rustc_ty<'tcx>(tcx: TyCtxt<'tcx>, resolved: &crate::toylang::typed_ast::ResolvedType) -> ty::Ty<'tcx> {
    try_resolved_to_rustc_ty(tcx, resolved, &RustTypeLookupContext::Codegen)
        .unwrap_or_else(|e| panic!("{}", e))
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

/// Phase 6: Stdlib methods that can't be called directly because they are
/// `#[inline(always)]` (no external symbol) or `#[track_caller]` (hidden ABI
/// param). For each, stub_gen emits a `pub fn __toylang_*` wrapper in
/// __lang_stubs that takes the receiver by raw pointer (so calling convention
/// matches toylang's existing recv_ptr passing). Both dep registration and
/// codegen redirect to the wrapper via `redirect_to_wrapper`.
const WRAPPERS: &[(&str, &str, &str)] = &[
    ("Option", "unwrap", "__toylang_option_unwrap"),
    ("Result", "unwrap", "__toylang_result_unwrap"),
];

pub fn wrapper_fn_name(type_name: &str, method_name: &str) -> Option<&'static str> {
    WRAPPERS.iter()
        .find(|(t, m, _)| *t == type_name && *m == method_name)
        .map(|(_, _, w)| *w)
}

pub fn all_wrappers() -> &'static [(&'static str, &'static str, &'static str)] {
    WRAPPERS
}

/// Find a wrapper function in __lang_stubs by name.
fn find_wrapper_fn_def_id(tcx: TyCtxt<'_>, wrapper_name: &str) -> Option<DefId> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) != DefKind::Fn { continue; }
        if tcx.item_name(def_id).as_str() != wrapper_name { continue; }
        if rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
    }
    None
}

/// If (type_name, method_name) is in the wrapper table, build an Instance for
/// the wrapper. The wrapper's generic shape mirrors the original method's, so
/// type_args pass through unchanged.
///
/// Both `collect_toylang_fn_deps_inner` and `get_or_resolve_rust_method` MUST
/// call this helper so that rust_deps registration and codegen agree on the
/// wrapper symbol.
pub fn redirect_to_wrapper<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<(DefId, ty::GenericArgsRef<'tcx>)> {
    let wrapper_name = wrapper_fn_name(type_name, method_name)?;
    let wrapper_def_id = find_wrapper_fn_def_id(tcx, wrapper_name)?;
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
        .collect();
    let expected_count = tcx.generics_of(wrapper_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    Some((wrapper_def_id, args))
}

/// Query rustc for a Rust method's return type, converting to ResolvedType.
pub fn rust_method_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<crate::toylang::typed_ast::ResolvedType, UnresolvedRustType> {
    let type_def_id = find_rust_type_def_id(tcx, type_name)
        .ok_or_else(|| UnresolvedRustType {
            name: type_name.to_string(),
            context: RustTypeLookupContext::InherentMethodTypeArg {
                type_name: type_name.to_string(), method: method_name.to_string(),
            },
        })?;
    let method_def_id = find_inherent_method(tcx, type_def_id, method_name)
        .unwrap_or_else(|| panic!("method '{}' not found on '{}'", method_name, type_name));

    // Build generic args from type_args
    let arg_ctx = RustTypeLookupContext::InherentMethodTypeArg {
        type_name: type_name.to_string(), method: method_name.to_string(),
    };
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx)?)))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_count = tcx.generics_of(method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);

    // Query fn_sig and extract return type
    let sig = tcx.fn_sig(method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    Ok(rustc_ty_to_resolved_type(tcx, sig.output()))
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
    // Per @RTMEIZ, all Rust types must be `use`-imported in the
    // toylang source — including types the source never names
    // explicitly (trait-call Self types, tail return types, nested
    // generic args). The stub generator emits `pub use` re-exports
    // from the source's `use` statements, and find_local_struct_def_id
    // finds them via module_children_local. Types not `use`-imported
    // return None here and the caller panics.
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
fn try_instantiate_free_fn_sig<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    fn_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<ty::FnSig<'tcx>, UnresolvedRustType> {
    let arg_ctx = RustTypeLookupContext::FreeFunctionTypeArg {
        function_name: fn_name.to_string(),
    };
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx)?)))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_count = tcx.generics_of(def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(def_id).instantiate(tcx, args);
    Ok(tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig))
}

/// Return type of a use-imported free function. None if not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_free_fn_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<Option<crate::toylang::typed_ast::ResolvedType>, UnresolvedRustType> {
    let Some(def_id) = find_use_imported_fn_def_id(tcx, name) else { return Ok(None) };
    let sig = try_instantiate_free_fn_sig(tcx, def_id, name, type_args)?;
    Ok(Some(rustc_ty_to_resolved_type(tcx, sig.output())))
}

/// Param types of a use-imported free function. None if not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_free_fn_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(def_id) = find_use_imported_fn_def_id(tcx, name) else { return Ok(None) };
    let sig = try_instantiate_free_fn_sig(tcx, def_id, name, type_args)?;
    Ok(Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect()))
}

/// Param types of a Rust inherent method. None if type or method not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_method_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(type_def_id) = find_rust_type_def_id(tcx, type_name) else { return Ok(None) };
    let Some(method_def_id) = find_inherent_method(tcx, type_def_id, method_name) else { return Ok(None) };
    let arg_ctx = RustTypeLookupContext::InherentMethodTypeArg {
        type_name: type_name.to_string(), method: method_name.to_string(),
    };
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx)?)))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_count = tcx.generics_of(method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig);
    Ok(Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect()))
}

/// Param types of a trait method. None if trait/method not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_trait_method_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    trait_name: &str,
    method_name: &str,
    receiver_ty: &crate::toylang::typed_ast::ResolvedType,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(trait_def_id) = find_use_imported_trait_def_id(tcx, trait_name) else { return Ok(None) };
    let self_resolved = strip_ref(receiver_ty);
    let self_ctx = RustTypeLookupContext::TraitCallSelf {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let self_ty = try_resolved_to_rustc_ty(tcx, self_resolved, &self_ctx)?;
    let Some(trait_method_def_id) = tcx.associated_item_def_ids(trait_def_id)
        .iter()
        .find(|&&id| tcx.item_name(id).as_str() == method_name)
        .copied() else { return Ok(None) };
    // Per @TVIMDGAZ, args are [Self, ...explicit] for the trait definition's method DefId
    let arg_ctx = RustTypeLookupContext::TraitMethodTypeArg {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
    for ta in type_args {
        all_ty_args.push(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx)?));
    }
    let expected_count = tcx.generics_of(trait_method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig);
    Ok(Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect()))
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
) -> Result<crate::toylang::typed_ast::ResolvedType, UnresolvedRustType> {
    let trait_def_id = find_use_imported_trait_def_id(tcx, trait_name)
        .unwrap_or_else(|| panic!("trait '{}' not found", trait_name));
    // Per @TVIMDGAZ, strip &ref to get Self and use the trait definition's method DefId.
    // Per @RTMEIZ, the Self type of a trait call must be `use`-imported in the toylang
    // source even though the source never names it — try_resolved_to_rustc_ty needs it
    // findable in the type registry.
    let self_resolved = strip_ref(receiver_ty);
    let self_ctx = RustTypeLookupContext::TraitCallSelf {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let self_ty = try_resolved_to_rustc_ty(tcx, self_resolved, &self_ctx)?;

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
    let arg_ctx = RustTypeLookupContext::TraitMethodTypeArg {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
    for ta in type_args {
        all_ty_args.push(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx)?));
    }
    let expected_count = tcx.generics_of(trait_method_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);

    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    Ok(rustc_ty_to_resolved_type(tcx, sig.output()))
}
