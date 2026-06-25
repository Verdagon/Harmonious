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
    Codegen,
    // Per @IVTDBTZ — when dispatch classifies a `Name::method(args)` call as
    // a trait call (because `is_rust_trait(Name)` returned true), but the
    // trait name itself isn't findable via `find_use_imported_trait_def_id`
    // at resolve time. Typically a missing `use` line or typo. `name` on
    // the surrounding UnresolvedRustType holds the trait name.
    TraitCallName { method: String },
    // Per @IVTDBTZ — trait was found, but the method name isn't an
    // associated item on the trait. Typically a typo at the call site.
    // `name` on the surrounding UnresolvedRustType holds the method name.
    TraitMethodName { trait_name: String },
    // Workstream B / Phase B — A query input was still a TypeParam at
    // registry-build time. The per-Instance substituted pass will redo the
    // query with concrete args; callers should treat this as a "defer" signal,
    // not a user-facing error. See `UnresolvedRustType::is_deferred`.
    //
    // `query` is a human-readable description of which query was deferred,
    // e.g. "trait call `Clone::clone`", "inherent method `Vec::push`",
    // "free function `wrap`", "method `.clone()` on receiver containing
    // TypeParam". Used in the Display impl for diagnostics if this ever leaks
    // past `is_deferred()`.
    DeferredTypeParam { query: String },
}

impl UnresolvedRustType {
    /// True iff this error should be silently skipped at the eager-typecheck
    /// pass because resolution depends on concrete args only known at the
    /// per-Instance substituted pass.
    pub fn is_deferred(&self) -> bool {
        matches!(self.context, RustTypeLookupContext::DeferredTypeParam { .. })
    }
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
            Self::Codegen =>
                write!(f, "during codegen"),
            Self::TraitCallName { method } =>
                write!(f, "as trait name in trait call `::{}`", method),
            Self::TraitMethodName { trait_name } =>
                write!(f, "as method name on trait `{}`", trait_name),
            Self::DeferredTypeParam { query } =>
                write!(f, "as TypeParam in {} (deferred to substituted pass)", query),
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

// Sunny-karp refactor (2026-06-25): TypeParam-bearing oracle queries.
// Previously every oracle query short-circuited with `DeferredTypeParam` the
// moment it saw a `ResolvedType::TypeParam` in its args; `after_rust_analysis`
// swallowed the deferred error and the typed AST was discarded, then re-run
// from scratch at every per-Instance mono. The shortcut was implementation
// laziness from the Approach A migration — `tcx.fn_sig(...).instantiate(...)`
// accepts Param-bearing args and `rustc_ty_to_resolved_type` already converts
// `TyKind::Param` → `ResolvedType::TypeParam`. The current path threads the
// caller's `type_params: &[String]` through `try_resolved_to_rustc_ty`; the
// `TypeParam` arm rebuilds `ty::Ty::new_param(tcx, idx, name)` using the
// index from the caller's param list. The early-returns are gone; generic
// bodies type-resolve fully once at `after_rust_analysis`, cached on
// `ToyFunction.typed_body`, and at mono we substitute the typed AST directly.

/// Cross-crate-aware DefId resolver. Searches for an item named `name` whose
/// `Res::Def` kind passes `kind_filter`. Search order:
///
///   1. Children of every local module (including the crate root). Catches
///      both locally-defined items (`pub struct Foo`) and `pub use`
///      re-exports — a `pub use` shows up in the parent module's
///      `module_children_local` exactly like a local definition.
///   2. Children of the extern `__lang_stubs` rlib's crate root. Under the
///      two-crate architecture (stage 5b) the stub rlib is always an extern
///      crate from the user bin's perspective, so this walk finds its
///      re-exports and stub items. (Pre-5c.4 FileLoader-single-crate
///      setups had no extern `__lang_stubs`, making this step inert; that
///      path is retired.)
///
/// The two walks are intentionally symmetric — same matcher, same DefKind
/// filter — so that the cross-crate path produces semantically identical
/// DefIds to the local path.
fn resolve_rust_path(
    tcx: TyCtxt<'_>,
    name: &str,
    kind_filter: fn(DefKind) -> bool,
) -> Option<DefId> {
    // Local modules + crate root.
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if tcx.def_kind(local_def_id.to_def_id()) != DefKind::Mod { continue; }
        if let Some(def_id) = match_module_child_local(tcx, local_def_id, name, kind_filter) {
            return Some(def_id);
        }
    }
    if let Some(def_id) = match_module_child_local(
        tcx, rustc_hir::def_id::CRATE_DEF_ID, name, kind_filter,
    ) {
        return Some(def_id);
    }
    // Extern Sky stub rlib(s). Under the two-crate architecture this is
    // the user-bin side lookup path that resolves re-exports carried by
    // the stub rlib (e.g. `pub use std::io::Stdout;`). Phase 3 E.1 swapped
    // the crate-name match for marker-based detection: any extern crate
    // carrying `__SKY_STUBS_MARKER` (the facade's `is_from_lang_stubs`
    // predicate) is a candidate. There is currently always exactly one
    // such crate; multi-crate (E.3) will iterate.
    for c in tcx.crates(()).iter().copied() {
        if !rustc_lang_facade::is_from_lang_stubs(tcx, c.as_def_id()) {
            continue;
        }
        if let Some(def_id) = match_module_child_extern(tcx, c.as_def_id(), name, kind_filter) {
            return Some(def_id);
        }
    }
    None
}

fn match_module_child_local(
    tcx: TyCtxt<'_>,
    module: rustc_hir::def_id::LocalDefId,
    name: &str,
    kind_filter: fn(DefKind) -> bool,
) -> Option<DefId> {
    use rustc_hir::def::Res;
    for child in tcx.module_children_local(module) {
        if child.ident.as_str() != name { continue; }
        if let Res::Def(kind, def_id) = child.res {
            if kind_filter(kind) { return Some(def_id); }
        }
    }
    None
}

fn match_module_child_extern(
    tcx: TyCtxt<'_>,
    module: DefId,
    name: &str,
    kind_filter: fn(DefKind) -> bool,
) -> Option<DefId> {
    use rustc_hir::def::Res;
    for child in tcx.module_children(module) {
        if child.ident.as_str() != name { continue; }
        if let Res::Def(kind, def_id) = child.res {
            if kind_filter(kind) { return Some(def_id); }
        }
    }
    None
}

fn is_type_kind(kind: DefKind) -> bool {
    matches!(kind, DefKind::Struct | DefKind::Enum)
}

fn is_trait_kind(kind: DefKind) -> bool {
    matches!(kind, DefKind::Trait)
}

fn is_fn_kind(kind: DefKind) -> bool {
    matches!(kind, DefKind::Fn)
}

/// Walk local + extern stub-rlib HIR to find a struct/enum named `name`.
/// Also resolves `pub use` re-exports to the original struct/enum DefId.
pub fn find_local_struct_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    resolve_rust_path(tcx, name, is_type_kind)
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
///
/// `caller_type_params` is the list of in-scope type-param names (from the
/// surrounding consumer fn / impl block). When the input contains
/// `ResolvedType::TypeParam(name)`, this list determines the `ParamTy` index
/// passed to `ty::Ty::new_param`. Pass `&[]` at non-generic / post-mono call
/// sites (most codegen + layout sites) — hitting a TypeParam with empty
/// param_names is a substitution bug and panics.
pub fn try_resolved_to_rustc_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    resolved: &crate::toylang::typed_ast::ResolvedType,
    context: &RustTypeLookupContext,
    caller_type_params: &[String],
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
                .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &nested, caller_type_params)?)))
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
                .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &nested, caller_type_params)?)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ty::Ty::new_adt(tcx, adt_def, tcx.mk_args(&args)))
        }
        ResolvedType::Ref { inner } => {
            let inner_ty = try_resolved_to_rustc_ty(tcx, inner, context, caller_type_params)?;
            Ok(ty::Ty::new_imm_ref(tcx, tcx.lifetimes.re_erased, inner_ty))
        }
        // Per @UTAIRZ, Str and ByteSlice round-trip through rustc identically;
        // reverse maps at `rustc_ty_to_resolved_type` produce the matching variants.
        ResolvedType::Str => Ok(tcx.types.str_),
        ResolvedType::ByteSlice => Ok(ty::Ty::new_slice(tcx, tcx.types.u8)),
        ResolvedType::TypeParam(name) => {
            // Sunny-karp: rebuild rustc's `ParamTy` from the caller fn's
            // type-param list. The index is the param's position in the
            // surrounding fn's generics; `rustc_ty_to_resolved_type`
            // round-trips this back to `TypeParam(name)` via the param's
            // name field. When `caller_type_params` is empty (post-mono /
            // codegen call sites), a TypeParam reaching here is a
            // substitution bug.
            let idx = caller_type_params.iter().position(|p| p == name)
                .unwrap_or_else(|| panic!(
                    "TypeParam '{}' not found in caller_type_params {:?} — \
                     either the caller fn's type-param list was not threaded \
                     through, or substitution missed this Param",
                    name, caller_type_params,
                ));
            Ok(ty::Ty::new_param(tcx, idx as u32, rustc_span::Symbol::intern(name)))
        }
    }
}

/// Convenience wrapper that panics on error — for call sites where context
/// is unavailable or errors have already been validated away (e.g., codegen
/// after `after_rust_analysis` passed). Post-mono / codegen path; passes an
/// empty `caller_type_params` to `try_resolved_to_rustc_ty`.
pub fn resolved_to_rustc_ty<'tcx>(tcx: TyCtxt<'tcx>, resolved: &crate::toylang::typed_ast::ResolvedType) -> ty::Ty<'tcx> {
    try_resolved_to_rustc_ty(tcx, resolved, &RustTypeLookupContext::Codegen, &[])
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
                .filter_map(|a| match a.kind() {
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
        // Per @UTAIRZ, TyKind::Slice(u8) reverse-maps to ByteSlice, matching the
        // forward map in `try_resolved_to_rustc_ty`.
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
        // Per @UTAIRZ, TyKind::Str reverse-maps to Str — NOT to RustType "str"
        // (which would break type equality with the Ref-wrapped literal).
        TyKind::Str => ResolvedType::Str,
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
        // Type parameters surface here under the `optimized_mir` override path
        // (stage-3 migration): rustc hands us a generic consumer body whose
        // Params are placeholders the collector substitutes per caller. We
        // represent them as `TypeParam(name)` and round-trip via the
        // thread-local param_map installed in `collect_generic_rust_deps_inner`.
        TyKind::Param(p) => ResolvedType::TypeParam(p.name.to_string()),
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
        // Under the `optimized_mir` override path, Params flow through the
        // deps walker inside generic consumer bodies; the cycle-guard seed
        // for `collect_rust_deps_recursive` mangles the caller's identity
        // args, which may include Params. Produce a stable, collision-safe
        // string; the mangling is only used as a dedup key inside the
        // walker — the produced string is never emitted as a real symbol
        // (the consumer backend always sees concrete args during codegen).
        ResolvedType::TypeParam(name) => format!("P_{}", name),
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

/// Find a wrapper function in __lang_stubs by name. The Phase-6 wrappers
/// (`__toylang_option_unwrap<T>` etc.) are emitted by `stub_gen` as ordinary
/// `pub fn` items at the `__lang_stubs` crate root, so this lookup matches
/// the crate-root `Fn` pattern (not the `extern "C" { pub fn ... }` foreign
/// pattern that `find_extern_fn_def_id` handles).
fn find_wrapper_fn_def_id(tcx: TyCtxt<'_>, wrapper_name: &str) -> Option<DefId> {
    // Local walk: works at the rlib compile where __lang_stubs IS the
    // local crate.
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) != DefKind::Fn { continue; }
        if tcx.item_name(def_id).as_str() != wrapper_name { continue; }
        if rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
    }
    // Cross-crate fallback (stage-5a oracle sweep completion, like
    // `find_extern_fn_def_id`): under the two-crate architecture the
    // user-bin compile sees `__lang_stubs` as extern. `redirect_to_wrapper`
    // is called from the codegen path (`llvm_gen.rs:360`) and from the
    // recursive dep walker (`callbacks_impl.rs:937`); both surfaces are
    // exercised at user-bin time once Workstream A moves codegen there.
    // Phase 3 E.6: iterate ALL marker-bearing crates (see
    // `find_extern_fn_in_stub_rlib`).
    use rustc_hir::def::Res;
    for c in tcx.crates(()).iter().copied() {
        if !rustc_lang_facade::is_from_lang_stubs(tcx, c.as_def_id()) {
            continue;
        }
        for child in tcx.module_children(c.as_def_id()) {
            if child.ident.as_str() != wrapper_name { continue; }
            if let Res::Def(DefKind::Fn, def_id) = child.res {
                return Some(def_id);
            }
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
    // @ELASZ
    let args = build_generic_args_for_item(tcx, wrapper_def_id, &all_ty_args);
    Some((wrapper_def_id, args))
}

/// Query rustc for a Rust method's return type, converting to ResolvedType.
///
/// `caller_type_params` carries the in-scope type-param names so
/// `try_resolved_to_rustc_ty` can rebuild `ParamTy` for `ResolvedType::TypeParam`
/// args (sunny-karp). Pass `&[]` from post-mono / non-generic call sites.
pub fn rust_method_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
    caller_type_params: &[String],
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
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx, caller_type_params)?)))
        .collect::<Result<Vec<_>, _>>()?;
    // @ELASZ
    let args = build_generic_args_for_item(tcx, method_def_id, &all_ty_args);

    // Query fn_sig and extract return type. Param-bearing args flow through
    // `instantiate` cleanly (Params are normalization terminals); the output
    // sig carries our Params and `rustc_ty_to_resolved_type` converts them
    // back to `ResolvedType::TypeParam`.
    let sig = tcx.fn_sig(method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    Ok(rustc_ty_to_resolved_type(tcx, sig.output()))
}

/// Find an extern (non-toylang) function by name among local definitions.
///
/// Under the current two-crate architecture the primary shape is:
///   - A foreign declaration `extern "C" { pub fn <name>(…); }` inside
///     `__lang_stubs`. `stub_gen` emits these for body-less toylang fns;
///     an external cargo dep (e.g. the integration tests' `test_helpers`)
///     provides the matching `#[no_mangle] pub extern "C" fn <name>` at
///     final link time.
///
/// A vestigial second shape — a local Rust `pub fn <name> { … }` outside
/// `__lang_stubs` — is also accepted. It was exercised by the retired
/// FileLoader-single-crate (direct mode) path (stage 5c.4) where the
/// user's `.rs` fixture defined `pub fn println_int` etc. at the user
/// crate root. The branch is preserved because it's harmless under the
/// current architecture (the user-bin's HIR contains only the tiny
/// `fn main() { __toylang_main(); }` shim, so no name collision is
/// possible) and because removing it would be a no-op refactor.
///
/// Both shapes return a usable `DefId` for `tcx.fn_sig` / `symbol_name` /
/// `coerced_*_for_instance` queries; toylang's codegen treats them the
/// same. Where both might match the same name (consumer fn shadowing
/// extern decl), the local-defined fn wins by iteration order — the
/// `!is_from_lang_stubs` filter below preserves that historical bias.
pub fn find_extern_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    let mut foreign_match: Option<DefId> = None;
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) != DefKind::Fn { continue; }
        if tcx.item_name(def_id).as_str() != name { continue; }
        if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
        // Stub-rlib foreign decl. Remember; only return if no non-stub
        // match shows up in the rest of the iteration.
        if tcx.is_foreign_item(def_id) {
            foreign_match = Some(def_id);
        }
    }
    if let Some(found) = foreign_match {
        return Some(found);
    }
    // Cross-crate fallback (completes the stage-5a oracle sweep that
    // landed `resolve_rust_path` for the `use`-imported lookups but
    // never extended `find_extern_fn_def_id` to extern crates). Under
    // the two-crate architecture the user-bin compile's local HIR holds
    // only the `fn main() { __toylang_main(); }` shim — every
    // `extern "C" { pub fn println_i32; }` declaration lives in the
    // upstream `__lang_stubs` rlib. Without this fallback any user-bin
    // codegen path that resolves an extern fn name (e.g. the call-site
    // resolution at `llvm_gen.rs:1173`) would panic, which is the
    // blocker that surfaced when Workstream A's prototype tried to
    // move codegen to user-bin time.
    find_extern_fn_in_stub_rlib(tcx, name)
}

/// Find a `pub fn <name>` defined at the `__lang_stubs` rlib's crate root
/// (NOT inside an `extern "C" {}` block). `stub_gen` emits these for every
/// consumer fn with a body — they're the `unreachable!()`-bodied Rust
/// shells that match a toylang fn's signature so rustc has a DefId for
/// the call site. The user-bin compile, post-Workstream-A, needs these
/// DefIds to construct rustc `Instance`s for ABI-aware extern-wrapper
/// codegen (`__toylang_impl_*` emission). Returns None if no match.
pub fn find_stub_fn_in_stub_rlib(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // Phase 3 E.6: iterate ALL marker-bearing crates (see
    // `find_extern_fn_in_stub_rlib` for the why).
    use rustc_hir::def::Res;
    for c in tcx.crates(()).iter().copied() {
        if !rustc_lang_facade::is_from_lang_stubs(tcx, c.as_def_id()) {
            continue;
        }
        for child in tcx.module_children(c.as_def_id()) {
            if child.ident.as_str() != name { continue; }
            if let Res::Def(DefKind::Fn, def_id) = child.res {
                if !tcx.is_foreign_item(def_id) {
                    return Some(def_id);
                }
            }
        }
    }
    None
}

/// Walk the `__lang_stubs` extern crate root + each `extern "C"` foreign-mod
/// child, looking for a foreign fn named `name`. Returns the foreign-item's
/// DefId. Symmetric with the local walk in `find_extern_fn_def_id`'s loop
/// (matches the `extern "C" { pub fn ...; }` shape `stub_gen` emits).
fn find_extern_fn_in_stub_rlib(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    // Phase 3 E.1/E.6: iterate ALL marker-bearing crates, not just the first.
    // Under multi-toylang-project builds the dep's stub rlib may come up in
    // `tcx.crates(())` before the root's, so a `.find()`-then-search shape
    // would search the wrong crate and miss legitimate extern fns.
    use rustc_hir::def::Res;
    for c in tcx.crates(()).iter().copied() {
        if !rustc_lang_facade::is_from_lang_stubs(tcx, c.as_def_id()) {
            continue;
        }
        let stubs_root = c.as_def_id();
        // The crate root's direct children include the `extern "C" {}`
        // block as a ForeignMod. Foreign fns are children of that ForeignMod,
        // not of the crate root, so we recurse one level into each ForeignMod.
        for child in tcx.module_children(stubs_root) {
            if let Res::Def(kind, mid) = child.res {
                if kind == DefKind::ForeignMod {
                    for fchild in tcx.module_children(mid) {
                        if fchild.ident.as_str() != name { continue; }
                        if let Res::Def(fkind, fdid) = fchild.res {
                            if fkind == DefKind::Fn && tcx.is_foreign_item(fdid) {
                                return Some(fdid);
                            }
                        }
                    }
                } else if kind == DefKind::Fn && child.ident.as_str() == name {
                    return Some(mid);
                }
            }
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
    resolve_rust_path(tcx, name, is_trait_kind)
}

/// Find a use-imported free function by name among `pub use` re-exports in __lang_stubs.
pub fn find_use_imported_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<DefId> {
    resolve_rust_path(tcx, name, is_fn_kind)
}

/// Phase 2 C.5: find the impl-method DefId for
/// `impl <Trait> for <ConsumerStruct> { fn <method>(...) ... }` across the
/// crate graph. Walks `tcx.all_impls(trait_def_id)` for every impl whose
/// self type's ADT name matches `self_type_name`, then finds the
/// associated item with the matching method name.
///
/// Returns None if the trait isn't `use`-imported, no matching impl
/// exists, or the method name isn't on that impl. Cross-crate-safe (the
/// stub rlib's impls show up in `all_impls` because rustc indexes them
/// by trait DefId during metadata load).
pub fn find_trait_impl_method_def_id(
    tcx: TyCtxt<'_>,
    trait_name: &str,
    self_type_name: &str,
    method_name: &str,
) -> Option<DefId> {
    let trait_def_id = find_use_imported_trait_def_id(tcx, trait_name)?;
    for impl_def_id in tcx.all_impls(trait_def_id) {
        // instantiate_identity: structural inspection only — we read the
        // self type's ADT name; we are not producing a concrete type here.
        let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
        let ty::TyKind::Adt(adt_def, _) = self_ty.kind() else { continue; };
        if tcx.item_name(adt_def.did()).as_str() != self_type_name { continue; }
        // Self-type name alone is ambiguous: `Box` matches both
        // `case6_lib::Box` (Sky-defined) and `alloc::boxed::Box` /
        // `std::ffi::os_str::Box<OsStr>` (Rust-stdlib-defined). Restrict to
        // consumer-marker-bearing crates so we land on the Sky impl.
        // Path B exposed this — pre-Path B the synthesized
        // `__toylang_impl__<Self>__<Trait>__<m>` name didn't care which
        // DefId we returned; Path B uses the DefId's rustc-default mangling
        // directly and demands the right impl.
        if !rustc_lang_facade::is_from_lang_stubs(tcx, adt_def.did()) {
            continue;
        }
        for &assoc_id in tcx.associated_item_def_ids(impl_def_id) {
            if tcx.item_name(assoc_id).as_str() == method_name {
                return Some(assoc_id);
            }
        }
    }
    None
}

/// @ELASZ — Build a `GenericArgs` for `def_id` by letting rustc drive the
/// per-param walk. `resolved_types` supplies the `Type` slots in
/// declaration order (for trait methods the caller prepends `Self`;
/// for free fns and inherent methods it's just user-supplied type args
/// pre-resolved via `try_resolved_to_rustc_ty`). Lifetime slots are
/// filled with `re_erased` (post-borrowck placeholder — borrow-checking
/// already happened during stub typecheck, so lifetimes are semantically
/// irrelevant at our phase). Const slots panic (not yet supported; no
/// test in the corpus exercises them). Replaces five hand-rolled
/// `mk_args` sites that silently dropped lifetime slots and ICEd rustc
/// whenever a Rust item had an early-bound lifetime parameter (e.g.,
/// `serde_json::from_str<'a, T: Deserialize<'a>>`).
///
/// Synthetic `impl Trait` params (from `fn foo(x: impl Trait)` desugar)
/// are handled by the same `Type` arm — rustc exposes them in
/// `generics_of` with `synthetic: true`, and we consume them from
/// `resolved_types` identically to named params. Named params first,
/// synthetic params second in declaration order — matching turbofish.
/// Don't add a "synthetic-only" branch; that breaks clap and every
/// `impl Into<T>` / `impl AsRef<T>` / `impl Fn(...)` call site.
pub fn build_generic_args_for_item<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    resolved_types: &[ty::GenericArg<'tcx>],
) -> ty::GenericArgsRef<'tcx> {
    let mut types_iter = resolved_types.iter().copied();
    let args = ty::GenericArgs::for_item(tcx, def_id, |param, _| {
        match param.kind {
            ty::GenericParamDefKind::Lifetime => tcx.lifetimes.re_erased.into(),
            ty::GenericParamDefKind::Type { .. } => {
                types_iter.next().unwrap_or_else(|| {
                    panic!(
                        "oracle: insufficient type args for {:?} (param {:?}) — \
                         type_resolve should have caught this upstream",
                        def_id, param.name,
                    )
                })
            }
            ty::GenericParamDefKind::Const { .. } => {
                panic!(
                    "oracle: const generic params not yet supported ({:?} on {:?})",
                    param.name, def_id,
                )
            }
        }
    });
    // @ETASTZ — extras beyond the item's `Type` slots are silently
    // truncated. Toylang's call-site syntax names the type's
    // generics (`Vec::new<I32, Global>()` names A=Global even
    // though `Vec::new`'s own generics are just `[T]` — A is
    // fixed by the impl block). In the common case rustc's
    // impl-block defaults match the user-supplied extra, so the
    // truncation is correct. If toylang ever names a non-default
    // parent-type arg (a custom allocator, hasher, etc.) that
    // would be silently dropped — latent soundness hole tracked
    // as tech debt.
    args
}

/// Instantiate a free function's fn_sig with given type args.
/// Non-generic functions pass an empty slice — same code path, no special-casing.
fn try_instantiate_free_fn_sig<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    fn_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
    caller_type_params: &[String],
) -> Result<ty::FnSig<'tcx>, UnresolvedRustType> {
    let arg_ctx = RustTypeLookupContext::FreeFunctionTypeArg {
        function_name: fn_name.to_string(),
    };
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx, caller_type_params)?)))
        .collect::<Result<Vec<_>, _>>()?;
    // @ELASZ
    let args = build_generic_args_for_item(tcx, def_id, &all_ty_args);
    let sig = tcx.fn_sig(def_id).instantiate(tcx, args);
    Ok(tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig))
}

/// Return type of a use-imported free function. None if not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_free_fn_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
    caller_type_params: &[String],
) -> Result<Option<crate::toylang::typed_ast::ResolvedType>, UnresolvedRustType> {
    let Some(def_id) = find_use_imported_fn_def_id(tcx, name) else { return Ok(None) };
    let sig = try_instantiate_free_fn_sig(tcx, def_id, name, type_args, caller_type_params)?;
    Ok(Some(rustc_ty_to_resolved_type(tcx, sig.output())))
}

/// Param types of a use-imported free function. None if not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_free_fn_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
    caller_type_params: &[String],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(def_id) = find_use_imported_fn_def_id(tcx, name) else { return Ok(None) };
    let sig = try_instantiate_free_fn_sig(tcx, def_id, name, type_args, caller_type_params)?;
    Ok(Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect()))
}

/// Param types of a Rust inherent method. None if type or method not found.
/// Returns Err if a type arg isn't imported per @RTMEIZ.
pub fn rust_method_param_types<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
    caller_type_params: &[String],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(type_def_id) = find_rust_type_def_id(tcx, type_name) else { return Ok(None) };
    let Some(method_def_id) = find_inherent_method(tcx, type_def_id, method_name) else { return Ok(None) };
    let arg_ctx = RustTypeLookupContext::InherentMethodTypeArg {
        type_name: type_name.to_string(), method: method_name.to_string(),
    };
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| Ok(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx, caller_type_params)?)))
        .collect::<Result<Vec<_>, _>>()?;
    // @ELASZ
    let args = build_generic_args_for_item(tcx, method_def_id, &all_ty_args);
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
    caller_type_params: &[String],
) -> Result<Option<Vec<crate::toylang::typed_ast::ResolvedType>>, UnresolvedRustType> {
    let Some(trait_def_id) = find_use_imported_trait_def_id(tcx, trait_name) else { return Ok(None) };
    let self_resolved = strip_ref(receiver_ty);
    let self_ctx = RustTypeLookupContext::TraitCallSelf {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let self_ty = try_resolved_to_rustc_ty(tcx, self_resolved, &self_ctx, caller_type_params)?;
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
        all_ty_args.push(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx, caller_type_params)?));
    }
    // @ELASZ
    let args = build_generic_args_for_item(tcx, trait_method_def_id, &all_ty_args);
    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(ty::TypingEnv::fully_monomorphized(), sig);
    Ok(Some(sig.inputs().iter().map(|&t| rustc_ty_to_resolved_type(tcx, t)).collect()))
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

/// Workstream B — does `ty` (anywhere reachable through nested generics or
/// `Ref` wrappers) contain a `TypeParam`? The trait-method oracle queries use
/// this to defer when Self or any type arg is still abstract at registry-build
/// time. The per-Instance substituted pass redoes the query with concrete args.
pub fn contains_type_param(ty: &crate::toylang::typed_ast::ResolvedType) -> bool {
    use crate::toylang::typed_ast::ResolvedType;
    match ty {
        ResolvedType::TypeParam(_) => true,
        ResolvedType::Ref { inner } => contains_type_param(inner),
        ResolvedType::StructRef { type_args, .. }
        | ResolvedType::RustType { type_args, .. } => type_args.iter().any(contains_type_param),
        ResolvedType::Struct { type_args, field_types, .. } =>
            type_args.iter().any(contains_type_param)
            || field_types.iter().any(contains_type_param),
        _ => false,
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
    caller_type_params: &[String],
) -> Result<crate::toylang::typed_ast::ResolvedType, UnresolvedRustType> {
    // Per @IVTDBTZ, this lookup can legitimately fail when the dispatch
    // classifier misfires or when the trait isn't `use`-imported; return
    // a structured error instead of panicking so the user sees an
    // actionable message at the call site.
    let trait_def_id = find_use_imported_trait_def_id(tcx, trait_name)
        .ok_or_else(|| UnresolvedRustType {
            name: trait_name.to_string(),
            context: RustTypeLookupContext::TraitCallName {
                method: method_name.to_string(),
            },
        })?;
    // Per @TVIMDGAZ, strip &ref to get Self and use the trait definition's method DefId.
    // Per @RTMEIZ, the Self type of a trait call must be `use`-imported in the toylang
    // source even though the source never names it — try_resolved_to_rustc_ty needs it
    // findable in the type registry.
    let self_resolved = strip_ref(receiver_ty);
    let self_ctx = RustTypeLookupContext::TraitCallSelf {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let self_ty = try_resolved_to_rustc_ty(tcx, self_resolved, &self_ctx, caller_type_params)?;

    // Per @IVTDBTZ, method-not-found on an imported trait is a source-level
    // error (typo at the call site), not an invariant violation.
    let trait_method_def_id = {
        let mut found = None;
        for &item_id in tcx.associated_item_def_ids(trait_def_id) {
            if tcx.item_name(item_id).as_str() == method_name {
                found = Some(item_id);
                break;
            }
        }
        found.ok_or_else(|| UnresolvedRustType {
            name: method_name.to_string(),
            context: RustTypeLookupContext::TraitMethodName {
                trait_name: trait_name.to_string(),
            },
        })?
    };

    // Per @TVIMDGAZ, args are [Self, ...explicit] for the trait definition's method DefId
    let arg_ctx = RustTypeLookupContext::TraitMethodTypeArg {
        trait_name: trait_name.to_string(), method: method_name.to_string(),
    };
    let mut all_ty_args: Vec<ty::GenericArg<'tcx>> = vec![ty::GenericArg::from(self_ty)];
    for ta in type_args {
        all_ty_args.push(ty::GenericArg::from(try_resolved_to_rustc_ty(tcx, ta, &arg_ctx, caller_type_params)?));
    }
    // @ELASZ
    let args = build_generic_args_for_item(tcx, trait_method_def_id, &all_ty_args);

    let sig = tcx.fn_sig(trait_method_def_id).instantiate(tcx, args);
    let sig = tcx.normalize_erasing_late_bound_regions(
        ty::TypingEnv::fully_monomorphized(), sig,
    );
    Ok(rustc_ty_to_resolved_type(tcx, sig.output()))
}

// ---------------------------------------------------------------------------
// Phase E Path 2 — `__ToylangOpaque<const T: u64>` const-generic plumbing
// ---------------------------------------------------------------------------
//
// Phase 2 ships these helpers with no in-tree consumers. Phase 3 (stub_gen
// Sky-struct emission) starts using `find_toylang_opaque_def_id` +
// `build_opaque_args`; Phase 4 (`layout_of` wrapper intercept) starts using
// `is_toylang_opaque` + `extract_typeid_from_args`. Each helper carries
// `#[allow(dead_code)]` until its consumer lands; remove when that consumer
// merges. The tests below exercise the encoders/decoders end-to-end so
// they aren't truly dead — just not yet wired into the build pipeline.
//
// Architecture: `rust-interop-architecture.md` §10.6. The wrapper carries a
// content-addressed u64 typeid as its only generic parameter. Phase 3 will
// start emitting `__ToylangOpaque<HASH>` references inside Sky struct stubs;
// Phase 4 will intercept `layout_of(__ToylangOpaque<HASH>)` and decode the
// const arg to look up the Sky type. The helpers below are the
// encode/decode/predicate triple they need.
//
// Naming: `__ToylangOpaque` is the toylang-specific name for what Sky's
// architecture calls `SkyOpaqueType` (§10.6). Toylang's stub_gen emits it at
// every stub rlib's crate root (Phase 1.2).

/// The crate-root item name of toylang's opaque wrapper. Match against this
/// at `tcx.item_name(adt_def.did())` time when intercepting wrapper layouts /
/// symbols / drop glue.
#[allow(dead_code)]
pub const TOYLANG_OPAQUE: &str = "__ToylangOpaque";

/// Locate the wrapper's DefId at the stub rlib's crate root. Models the
/// existing `find_stub_fn_in_stub_rlib` pattern: walk every marker-bearing
/// crate (handles multi-toylang-crate projects per E.1/E.6) and look for a
/// struct named `__ToylangOpaque` whose def_id's crate is THIS crate (the
/// §4.5 parentage check that protects against `use __lang_stubs::*;` glob
/// re-exports inadvertently lifting the wrapper into a downstream's
/// `module_children`).
///
/// Returns `None` when no stub rlib is loaded or none of the loaded stub
/// rlibs carry the wrapper — both impossible in any toylang build that ran
/// Phase 1.2's stub_gen but worth surfacing as `None` rather than panicking
/// so the caller can fall through to its default `layout_of` path.
#[allow(dead_code)]
pub fn find_toylang_opaque_def_id(tcx: TyCtxt<'_>) -> Option<DefId> {
    use rustc_hir::def::Res;
    // At the rlib compile the wrapper lives in the LOCAL crate (being
    // built); at the user-bin compile it lives in an extern stub rlib.
    // Walk LOCAL_CRATE first so the rlib compile resolves it immediately
    // without a redundant extern-crate iteration. Modelled on
    // `crate_has_sky_marker`'s local-vs-extern split (facade lib.rs).
    let local_cnum = rustc_hir::def_id::LOCAL_CRATE;
    if rustc_lang_facade::is_from_lang_stubs(tcx, local_cnum.as_def_id()) {
        for child in tcx.module_children_local(rustc_hir::def_id::CRATE_DEF_ID) {
            if child.ident.as_str() != TOYLANG_OPAQUE { continue; }
            if let Res::Def(DefKind::Struct, def_id) = child.res.expect_non_local::<rustc_hir::def_id::DefId>() {
                if def_id.krate == local_cnum {
                    return Some(def_id);
                }
            }
        }
    }
    for c in tcx.crates(()).iter().copied() {
        if !rustc_lang_facade::is_from_lang_stubs(tcx, c.as_def_id()) {
            continue;
        }
        let stubs_root = c.as_def_id();
        for child in tcx.module_children(stubs_root) {
            if child.ident.as_str() != TOYLANG_OPAQUE { continue; }
            if let Res::Def(DefKind::Struct, def_id) = child.res {
                // §4.5 parentage check — reject any re-export of the wrapper
                // into a different crate's `module_children` (would surface
                // here if a downstream did `use __lang_stubs::*;` and the
                // glob lifted `__ToylangOpaque` into its crate root).
                if def_id.krate == c {
                    return Some(def_id);
                }
            }
        }
    }
    None
}

/// True iff `def_id` is the toylang opaque wrapper struct.
///
/// Phase 4's `layout_of` intercept gates on this before extracting the
/// typeid. The check is name-and-parentage based: name equals
/// `__ToylangOpaque` AND the defining crate carries the Sky stubs marker
/// (which excludes user-defined `__ToylangOpaque` imports and glob
/// re-exports). Caches across calls would be a future-trivial addition; for
/// now we walk on every call because the walk visits at most one item per
/// marker-bearing crate (the wrapper is the only such item at the crate
/// root) and the producer count is small.
#[allow(dead_code)]
pub fn is_toylang_opaque(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    if tcx.item_name(def_id).as_str() != TOYLANG_OPAQUE { return false; }
    rustc_lang_facade::is_from_lang_stubs(tcx, def_id)
}

/// Decode the typeid from the const-u64 generic argument at slot 0 of a
/// `__ToylangOpaque<HASH>` instantiation.
///
/// The caller is responsible for first confirming via `is_toylang_opaque`
/// that the ADT is the wrapper; this helper just reads the const at index 0
/// and converts to `u64`. Returns `None` when the const slot is empty
/// (impossible for a well-typed wrapper instantiation but treated
/// defensively) or the const is non-leaf (e.g., still a `ConstKind::Param`
/// from `instantiate_identity()` — a structural inspection rather than a
/// monomorphized instance).
///
/// Architecture §10.8: the typeid is content-addressed, so the decoded u64
/// is identical to whatever the original `crate::typeid::compute(name, &[])`
/// computed at stub-emission time.
#[allow(dead_code)]
pub fn extract_typeid_from_args<'tcx>(
    args: ty::GenericArgsRef<'tcx>,
) -> Option<u64> {
    let ct = args.const_at(0);
    ct.try_to_leaf().map(|scalar| scalar.to_u64())
}

/// Build a `GenericArgs` for `__ToylangOpaque<HASH>` with the typeid as the
/// single const-u64 argument. The const is interned via `ty::Const::from_bits`
/// against the target's `u64` type. The wrapper has exactly one generic
/// parameter slot (the const u64) by construction (Phase 1.2 emission), so
/// the `for_item` walk produces exactly this one entry.
///
/// Phase 3 calls this when stub_gen emits Sky structs as
/// `Foo(__ToylangOpaque<HASH>);` — the field type's GenericArgs need the
/// concrete HASH.
#[allow(dead_code)]
pub fn build_opaque_args<'tcx>(
    tcx: TyCtxt<'tcx>,
    opaque_def_id: DefId,
    typeid: u64,
) -> ty::GenericArgsRef<'tcx> {
    let u64_ty = tcx.types.u64;
    let typeid_const = ty::Const::from_bits(
        tcx,
        typeid as u128,
        ty::TypingEnv::fully_monomorphized(),
        u64_ty,
    );
    let const_arg: ty::GenericArg<'tcx> = typeid_const.into();
    let args = ty::GenericArgs::for_item(tcx, opaque_def_id, |param, _| {
        match param.kind {
            ty::GenericParamDefKind::Const { .. } => const_arg,
            ty::GenericParamDefKind::Lifetime => tcx.lifetimes.re_erased.into(),
            ty::GenericParamDefKind::Type { .. } => panic!(
                "build_opaque_args: __ToylangOpaque must have only const params, got Type at {:?}",
                param.name,
            ),
        }
    });
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toylang::typed_ast::ResolvedType;

    #[test]
    fn contains_type_param_detects_direct() {
        assert!(contains_type_param(&ResolvedType::TypeParam("T".into())));
        assert!(!contains_type_param(&ResolvedType::I32));
    }

    #[test]
    fn contains_type_param_walks_ref_and_generics() {
        let inner = ResolvedType::Ref { inner: Box::new(ResolvedType::TypeParam("T".into())) };
        assert!(contains_type_param(&inner));
        let nested = ResolvedType::RustType {
            name: "Vec".into(),
            type_args: vec![ResolvedType::TypeParam("T".into())],
        };
        assert!(contains_type_param(&nested));
        let concrete = ResolvedType::RustType {
            name: "Vec".into(),
            type_args: vec![ResolvedType::I32],
        };
        assert!(!contains_type_param(&concrete));
    }

    #[test]
    fn unresolved_rust_type_is_deferred_only_for_deferred_context() {
        let deferred = UnresolvedRustType {
            name: "<TypeParam>".into(),
            context: RustTypeLookupContext::DeferredTypeParam {
                query: "trait call `Clone::clone`".into(),
            },
        };
        assert!(deferred.is_deferred());
        let non_deferred = UnresolvedRustType {
            name: "Foo".into(),
            context: RustTypeLookupContext::TraitCallSelf {
                trait_name: "Clone".into(), method: "clone".into(),
            },
        };
        assert!(!non_deferred.is_deferred());
    }
}
