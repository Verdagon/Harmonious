//! LLVM IR generation for toylang function bodies using inkwell.
//!
//! Walks the toylang AST generically — each expression node lowers independently.
//! No special-case generators for specific function shapes.

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::collections::HashMap;

use std::num::NonZero;

use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::builder::Builder;
use inkwell::types::{BasicType, BasicTypeEnum, StructType, BasicMetadataTypeEnum, IntType};
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue, FunctionValue};
use inkwell::AddressSpace;

use rustc_middle::ty::{self, GenericArg, TyCtxt};

use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::typed_ast::*;
use crate::toylang::registry::{ToylangRegistry, ToyFieldType, ToyStruct};
use rustc_lang_facade::LangCallbacks;

/// Convert BasicTypeEnum to AnyTypeEnum (inkwell doesn't impl From directly).
fn basic_type_to_any<'ctx>(ty: BasicTypeEnum<'ctx>) -> inkwell::types::AnyTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::ArrayType(t) => t.into(),
        BasicTypeEnum::FloatType(t) => t.into(),
        BasicTypeEnum::IntType(t) => t.into(),
        BasicTypeEnum::PointerType(t) => t.into(),
        BasicTypeEnum::StructType(t) => t.into(),
        BasicTypeEnum::VectorType(t) => t.into(),
        BasicTypeEnum::ScalableVectorType(t) => t.into(),
    }
}

// ============================================================================
// Codegen context
// ============================================================================

struct CodegenCtx<'ctx, 'tcx, 'reg> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    tcx: TyCtxt<'tcx>,
    registry: &'reg ToylangRegistry,
    pointer_bits: u64,
    pointer_align: u64,
    /// Variables in scope: name → (alloca pointer, type)
    vars: HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    /// Rust symbols that need external linkage
    rust_symbols: Vec<String>,
    /// Declared external functions (cached by symbol name)
    declared_fns: HashMap<String, FunctionValue<'ctx>>,
}

impl<'ctx, 'tcx, 'reg> CodegenCtx<'ctx, 'tcx, 'reg> {
    fn new(
        context: &'ctx Context,
        tcx: TyCtxt<'tcx>,
        registry: &'reg ToylangRegistry,
    ) -> Self {
        let dl = &tcx.data_layout;
        let module = context.create_module("toylang");

        // Set target layout and triple
        let target_datalayout = tcx.sess.target.data_layout.to_string();
        let target_triple = tcx.sess.opts.target_triple.tuple();
        module.set_data_layout(
            &inkwell::targets::TargetData::create(&target_datalayout).get_data_layout(),
        );
        module.set_triple(
            &inkwell::targets::TargetTriple::create(target_triple),
        );

        CodegenCtx {
            context,
            module,
            builder: context.create_builder(),
            tcx,
            registry,
            pointer_bits: dl.pointer_size.bits(),
            pointer_align: dl.pointer_align.abi.bytes(),
            vars: HashMap::new(),
            rust_symbols: Vec::new(),
            declared_fns: HashMap::new(),
        }
    }

    /// Create an integer type with the given bit width.
    fn int_type(&self, bits: u32) -> IntType<'ctx> {
        self.context.custom_width_int_type(NonZero::new(bits).unwrap()).unwrap()
    }

    /// Pointer-width integer type (usize equivalent).
    fn usize_type(&self) -> IntType<'ctx> {
        self.int_type(self.pointer_bits as u32)
    }

    /// Convert a ResolvedType to an inkwell type.
    fn resolved_to_inkwell(&self, ty: &ResolvedType) -> BasicTypeEnum<'ctx> {
        match ty {
            ResolvedType::I32 => self.context.i32_type().into(),
            ResolvedType::I64 => self.context.i64_type().into(),
            ResolvedType::F64 => self.context.f64_type().into(),
            ResolvedType::Bool => self.context.bool_type().into(),
            ResolvedType::Usize => self.usize_type().into(),
            ResolvedType::Void => self.context.i8_type().into(), // shouldn't be needed
            ResolvedType::Struct { field_types, .. } => {
                let fields: Vec<BasicTypeEnum<'ctx>> = field_types.iter()
                    .map(|ft| self.resolved_to_inkwell(ft))
                    .collect();
                self.context.struct_type(&fields, false).into()
            }
            ResolvedType::Vec { .. } => self.rust_ty_to_llvm_opaque(ty).0,
            ResolvedType::Str => self.context.ptr_type(AddressSpace::default()).into(),
            ResolvedType::Ref { inner: _ } => {
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    /// Convert a ResolvedType to an inkwell StructType (panics if not a struct/vec).
    fn resolved_to_struct_type(&self, ty: &ResolvedType) -> StructType<'ctx> {
        match ty {
            ResolvedType::Struct { field_types, .. } => {
                let fields: Vec<BasicTypeEnum<'ctx>> = field_types.iter()
                    .map(|ft| self.resolved_to_inkwell(ft))
                    .collect();
                self.context.struct_type(&fields, false)
            }
            ResolvedType::Vec { .. } => panic!("Vec is opaque — use rust_ty_to_llvm_opaque, not struct GEP"),
            _ => panic!("expected struct type, got {:?}", ty),
        }
    }

    // --- Type resolution ---

    fn resolve_type(&self, ft: &ToyFieldType) -> BasicTypeEnum<'ctx> {
        match ft {
            ToyFieldType::I32 => self.context.i32_type().into(),
            ToyFieldType::I64 => self.context.i64_type().into(),
            ToyFieldType::F64 => self.context.f64_type().into(),
            ToyFieldType::Bool => self.context.bool_type().into(),
            ToyFieldType::TypeParam(_) => panic!("TypeParam should be resolved before codegen"),
            ToyFieldType::ToyStruct(name) => {
                let s = self.registry.structs.get(name.as_str())
                    .unwrap_or_else(|| panic!("struct '{}' not found", name));
                self.struct_type(s).into()
            }
            ToyFieldType::RustGeneric(type_name, _) => {
                match type_name.as_str() {
                    "Vec" => {
                        // All Vec<T> have identical layout, element type irrelevant
                        let resolved = ResolvedType::Vec { elem: Box::new(ResolvedType::I32) };
                        self.rust_ty_to_llvm_opaque(&resolved).0
                    }
                    other => panic!("unsupported Rust generic type '{}'", other),
                }
            }
        }
    }

    fn struct_type(&self, s: &ToyStruct) -> StructType<'ctx> {
        let fields: Vec<BasicTypeEnum<'ctx>> = s.fields.iter()
            .map(|f| self.resolve_type(&f.rust_type))
            .collect();
        self.context.struct_type(&fields, false)
    }

    /// Build an inkwell StructType for a concrete struct instantiation.
    /// Uses the instance's generic args to substitute type params in field types.
    fn struct_type_for_instance(
        &self,
        toy_struct: &ToyStruct,
        instance: ty::Instance<'tcx>,
    ) -> StructType<'ctx> {
        if toy_struct.type_params.is_empty() {
            return self.struct_type(toy_struct);
        }

        // Get concrete type args from the accessor's self type
        let sig = self.tcx.fn_sig(instance.def_id()).instantiate(self.tcx, instance.args);
        let sig = self.tcx.normalize_erasing_late_bound_regions(
            ty::TypingEnv::fully_monomorphized(), sig,
        );
        let self_ref_ty = sig.inputs()[0]; // &Self
        let ty::TyKind::Ref(_, self_ty, _) = self_ref_ty.kind() else {
            return self.struct_type(toy_struct);
        };
        let ty::TyKind::Adt(_, args) = self_ty.kind() else {
            return self.struct_type(toy_struct);
        };

        // Build type param → concrete type string map
        let mut subst: HashMap<&str, &str> = HashMap::new();
        let type_arg_strings: Vec<String> = toy_struct.type_params.iter()
            .enumerate()
            .map(|(i, _)| {
                let concrete_ty = args[i].expect_ty();
                crate::toylang::callbacks_impl::rustc_ty_to_type_string(self.tcx, concrete_ty)
            })
            .collect();
        for (i, param_name) in toy_struct.type_params.iter().enumerate() {
            subst.insert(param_name.as_str(), type_arg_strings[i].as_str());
        }

        // Resolve each field type with substitution
        let fields: Vec<BasicTypeEnum<'ctx>> = toy_struct.fields.iter()
            .map(|f| self.resolve_field_type_with_subst(&f.rust_type, &subst))
            .collect();
        self.context.struct_type(&fields, false)
    }

    /// Resolve a ToyFieldType with type param substitution, returning an inkwell type.
    fn resolve_field_type_with_subst(
        &self,
        ft: &ToyFieldType,
        subst: &HashMap<&str, &str>,
    ) -> BasicTypeEnum<'ctx> {
        match ft {
            ToyFieldType::TypeParam(name) => {
                let concrete = subst.get(name.as_str())
                    .unwrap_or_else(|| panic!("TypeParam '{}' not in subst", name));
                self.type_from_string(concrete)
            }
            _ => self.resolve_type(ft),
        }
    }

    /// Convert a ResolvedType to a rustc Ty<'tcx>.
    /// Generalizes resolve_rust_ty_from_string to work on the typed AST directly.
    fn resolved_type_to_rustc_ty(&self, resolved: &ResolvedType) -> ty::Ty<'tcx> {
        match resolved {
            ResolvedType::I32 => self.tcx.types.i32,
            ResolvedType::I64 => self.tcx.types.i64,
            ResolvedType::F64 => self.tcx.types.f64,
            ResolvedType::Bool => self.tcx.types.bool,
            ResolvedType::Usize => self.tcx.types.usize,
            ResolvedType::Void => self.tcx.types.unit,
            ResolvedType::Struct { name, type_args, .. } => {
                if type_args.is_empty() {
                    // Non-generic struct — bare ADT with no args
                    crate::oracle::find_local_struct_ty(self.tcx, name)
                        .unwrap_or_else(|| panic!("struct '{}' not found in rustc", name))
                } else {
                    // Generic struct — construct ADT with concrete type args
                    let adt_def_id = crate::oracle::find_local_struct_def_id(self.tcx, name)
                        .unwrap_or_else(|| panic!("struct '{}' not found in rustc", name));
                    let adt_def = self.tcx.adt_def(adt_def_id);
                    let args: Vec<GenericArg<'tcx>> = type_args.iter()
                        .map(|ta| GenericArg::from(self.resolved_type_to_rustc_ty(ta)))
                        .collect();
                    ty::Ty::new_adt(self.tcx, adt_def, self.tcx.mk_args(&args))
                }
            }
            ResolvedType::Vec { elem } => {
                let elem_ty = self.resolved_type_to_rustc_ty(elem);
                let vec_did = self.tcx.get_diagnostic_item(rustc_span::sym::Vec).unwrap();
                let new_def_id = crate::oracle::find_vec_method(self.tcx, "new").unwrap();
                let global_ty = crate::oracle::extract_global_ty(self.tcx, elem_ty, new_def_id).unwrap();
                let adt_def = self.tcx.adt_def(vec_did);
                ty::Ty::new_adt(self.tcx, adt_def,
                    self.tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(global_ty)]))
            }
            ResolvedType::Ref { inner } => {
                let inner_ty = self.resolved_type_to_rustc_ty(inner);
                ty::Ty::new_imm_ref(self.tcx, self.tcx.lifetimes.re_erased, inner_ty)
            }
            ResolvedType::Str => panic!("Str type should not need rustc Ty conversion"),
        }
    }

    /// Query rustc's layout_of for a Rust type, returning an opaque [N x i8] LLVM type
    /// and alignment. Used for Rust types (Vec, etc.) that toylang treats as opaque blobs.
    fn rust_ty_to_llvm_opaque(&self, resolved: &ResolvedType) -> (BasicTypeEnum<'ctx>, u64) {
        let ty = self.resolved_type_to_rustc_ty(resolved);
        let layout = self.tcx.layout_of(
            rustc_middle::ty::PseudoCanonicalInput {
                value: ty,
                typing_env: rustc_middle::ty::TypingEnv::fully_monomorphized(),
            }
        ).expect("layout_of failed");
        let size = layout.layout.size().bytes() as u32;
        let align = layout.layout.align().abi.bytes();
        let array_ty = self.context.i8_type().array_type(size);
        (array_ty.into(), align)
    }

    /// Allocate stack space for an opaque Rust type with correct alignment.
    fn alloca_opaque_rust_ty(&self, resolved: &ResolvedType, name: &str) -> PointerValue<'ctx> {
        let (llvm_ty, align) = self.rust_ty_to_llvm_opaque(resolved);
        let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
        alloca.as_instruction_value().unwrap()
            .set_alignment(align as u32).unwrap();
        alloca
    }

    fn struct_type_for_ret(&self, ret_ty_name: &str) -> Option<StructType<'ctx>> {
        if let Some(s) = self.registry.structs.get(ret_ty_name) {
            return Some(self.struct_type(s));
        }
        if let Some((base, type_args)) = parse_generic_type(ret_ty_name) {
            if let Some(s) = self.registry.structs.get(base) {
                let fields: Vec<BasicTypeEnum<'ctx>> = s.fields.iter()
                    .map(|f| self.resolve_field_with_args(f, &s.type_params, &type_args))
                    .collect();
                return Some(self.context.struct_type(&fields, false));
            }
        }
        None
    }

    fn resolve_field_with_args(
        &self,
        field: &crate::toylang::registry::ToyField,
        type_params: &[String],
        type_args: &[&str],
    ) -> BasicTypeEnum<'ctx> {
        match &field.rust_type {
            ToyFieldType::TypeParam(name) => {
                let idx = type_params.iter().position(|p| p == name)
                    .unwrap_or_else(|| panic!("type param '{}' not found", name));
                self.type_from_string(type_args[idx])
            }
            other => self.resolve_type(other),
        }
    }

    /// Resolve a type string (from generic type args) to an inkwell type.
    /// Handles primitives, struct names, and generic types like Vec<i32>.
    fn type_from_string(&self, name: &str) -> BasicTypeEnum<'ctx> {
        match name {
            "i32" => self.context.i32_type().into(),
            "i64" => self.context.i64_type().into(),
            "f64" => self.context.f64_type().into(),
            "bool" => self.context.bool_type().into(),
            "usize" => self.usize_type().into(),
            _ => {
                // Check for generic types: Vec<i32>, Pair<i32, i64>
                if let Some((base, type_args)) = parse_generic_type(name) {
                    if base == "Vec" {
                        // All Vec<T> have identical layout, element type irrelevant
                        let resolved = ResolvedType::Vec { elem: Box::new(ResolvedType::I32) };
                        return self.rust_ty_to_llvm_opaque(&resolved).0;
                    }
                    if let Some(s) = self.registry.structs.get(base) {
                        let fields: Vec<BasicTypeEnum<'ctx>> = s.fields.iter()
                            .map(|f| self.resolve_field_with_args(f, &s.type_params, &type_args))
                            .collect();
                        return self.context.struct_type(&fields, false).into();
                    }
                }
                // Check for non-generic struct
                if let Some(s) = self.registry.structs.get(name) {
                    return self.struct_type(s).into();
                }
                panic!("unsupported type string '{}'", name)
            }
        }
    }

    /// Resolve a type name string to a rustc Ty. Handles primitives, structs, and generics.
    fn resolve_rust_ty_from_string(&self, name: &str) -> ty::Ty<'tcx> {
        match name {
            "i32" => self.tcx.types.i32,
            "i64" => self.tcx.types.i64,
            "f64" => self.tcx.types.f64,
            "bool" => self.tcx.types.bool,
            _ => {
                // Try local struct
                if let Some(ty) = crate::oracle::find_local_struct_ty(self.tcx, name) {
                    return ty;
                }
                // Try generic: "Vec<ToyPoint>"
                if let Some((base, args)) = parse_generic_type(name) {
                    if base == "Vec" {
                        let inner_ty = self.resolve_rust_ty_from_string(args[0]);
                        let vec_did = self.tcx.get_diagnostic_item(rustc_span::sym::Vec)
                            .expect("Vec not found");
                        let new_def_id = crate::oracle::find_vec_method(self.tcx, "new").unwrap();
                        let global_ty = crate::oracle::extract_global_ty(self.tcx, inner_ty, new_def_id).unwrap();
                        let adt_def = self.tcx.adt_def(vec_did);
                        return ty::Ty::new_adt(self.tcx, adt_def,
                            self.tcx.mk_args(&[GenericArg::from(inner_ty), GenericArg::from(global_ty)]));
                    }
                }
                panic!("resolve_rust_ty_from_string: unsupported type '{}'", name)
            }
        }
    }

    // --- External function declaration ---

    fn declare_external_fn(
        &mut self,
        symbol: &str,
        param_types: &[BasicMetadataTypeEnum<'ctx>],
        ret_type: Option<BasicTypeEnum<'ctx>>,
        is_sret: Option<(BasicTypeEnum<'ctx>, u64)>,
    ) -> FunctionValue<'ctx> {
        if let Some(&cached) = self.declared_fns.get(symbol) {
            return cached;
        }

        // Check if the module already has a function with this name
        // (e.g., another consumer function already codegenned in this module)
        if let Some(existing) = self.module.get_function(symbol) {
            self.declared_fns.insert(symbol.to_string(), existing);
            return existing;
        }

        let fn_type = if let Some(ret) = ret_type {
            ret.fn_type(param_types, false)
        } else {
            self.context.void_type().fn_type(param_types, false)
        };

        let func = self.module.add_function(symbol, fn_type, Some(inkwell::module::Linkage::External));

        if let Some((sret_ty, _align)) = is_sret {
            // sret attribute on first parameter
            let any_ty = basic_type_to_any(sret_ty);
            func.add_attribute(
                inkwell::attributes::AttributeLoc::Param(0),
                self.context.create_type_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id("sret"),
                    any_ty,
                ),
            );
        }

        self.declared_fns.insert(symbol.to_string(), func);
        func
    }

    // --- Vec operation helpers ---

    fn resolve_vec_symbols(&mut self, elem_ty_name: &str) {
        let elem_ty = self.resolve_rust_ty_from_string(elem_ty_name);


        let new_def_id = crate::oracle::find_vec_method(self.tcx, "new").unwrap();
        let push_def_id = crate::oracle::find_vec_method(self.tcx, "push").unwrap();
        let len_def_id = crate::oracle::find_vec_method(self.tcx, "len").unwrap();
        let global_ty = crate::oracle::extract_global_ty(self.tcx, elem_ty, new_def_id).unwrap();

        let new_args = self.tcx.mk_args(&[GenericArg::from(elem_ty)]);
        let push_args = self.tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(global_ty)]);
        let len_args = self.tcx.mk_args(&[GenericArg::from(elem_ty), GenericArg::from(global_ty)]);

        let new_sym = resolve_rust_symbol(self.tcx, new_def_id, new_args);
        let push_sym = resolve_rust_symbol(self.tcx, push_def_id, push_args);
        let len_sym = resolve_rust_symbol(self.tcx, len_def_id, len_args);

        self.rust_symbols.extend([new_sym.clone(), push_sym.clone(), len_sym.clone()]);

        // All Vec<T> have identical layout
        let vec_resolved = ResolvedType::Vec { elem: Box::new(ResolvedType::I32) };
        let (vec_opaque_ty, vec_align) = self.rust_ty_to_llvm_opaque(&vec_resolved);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let usize_ty = self.usize_type();

        // Declare Vec::new (sret)
        let _new_fn = self.declare_external_fn(
            &new_sym,
            &[ptr_ty.into()],
            None,
            Some((vec_opaque_ty, vec_align)),
        );

        // Declare Vec::push
        let _push_fn = self.declare_external_fn(
            &push_sym,
            &[ptr_ty.into(), ptr_ty.into()],
            None,
            None,
        );

        // Declare Vec::len
        let _len_fn = self.declare_external_fn(
            &len_sym,
            &[ptr_ty.into()],
            Some(usize_ty.into()),
            None,
        );

        // The symbols are now cached in declared_fns; no return value needed.
    }
}

// ============================================================================
// Expression lowering
// ============================================================================

/// Result of lowering an expression. Either a direct value or a pointer to memory.
enum ExprResult<'ctx> {
    /// A direct LLVM value (int, float, small struct in register)
    Value(BasicValueEnum<'ctx>),
    /// A pointer to a value in memory (structs, Vecs)
    Ptr(PointerValue<'ctx>, BasicTypeEnum<'ctx>),
}

impl<'ctx> ExprResult<'ctx> {
    /// Get as a direct value, loading from memory if necessary.
    fn into_value(self, builder: &Builder<'ctx>) -> BasicValueEnum<'ctx> {
        match self {
            ExprResult::Value(v) => v,
            ExprResult::Ptr(ptr, ty) => builder.build_load(ty, ptr, "load").unwrap(),
        }
    }

    /// Get as a pointer, storing to an alloca if necessary.
    fn into_ptr(self, builder: &Builder<'ctx>, ty: BasicTypeEnum<'ctx>, name: &str) -> PointerValue<'ctx> {
        match self {
            ExprResult::Ptr(ptr, _) => ptr,
            ExprResult::Value(val) => {
                let alloca = builder.build_alloca(ty, name).unwrap();
                builder.build_store(alloca, val).unwrap();
                alloca
            }
        }
    }
}

/// Coerce an i64 literal to the correct field width (i32, i1, etc.)
fn coerce_int_to_type<'ctx>(
    ctx: &CodegenCtx<'ctx, '_, '_>,
    val: BasicValueEnum<'ctx>,
    target_ty: BasicTypeEnum<'ctx>,
) -> BasicValueEnum<'ctx> {
    if val.get_type() == target_ty {
        return val;
    }
    if let (BasicValueEnum::IntValue(int_val), BasicTypeEnum::IntType(target_int)) = (val, target_ty) {
        let src_bits = int_val.get_type().get_bit_width();
        let dst_bits = target_int.get_bit_width();
        if src_bits > dst_bits {
            return ctx.builder.build_int_truncate(int_val, target_int, "trunc").unwrap().into();
        } else if src_bits < dst_bits {
            return ctx.builder.build_int_s_extend(int_val, target_int, "sext").unwrap().into();
        }
    }
    val
}

// ============================================================================
// Function codegen
// ============================================================================

/// Whether a ResolvedType uses sret under the internal ABI.
fn is_internal_sret(ty: &ResolvedType) -> bool {
    matches!(ty, ResolvedType::Struct { .. } | ResolvedType::Vec { .. })
}

/// Codegen a toylang function body with the simple internal ABI.
/// Structs/Vec always use sret (ptr first param, void return).
/// Primitives return directly. No Rust ABI coercion.
fn codegen_internal_function<'ctx, 'tcx>(
    ctx: &mut CodegenCtx<'ctx, 'tcx, '_>,
    func: &crate::toylang::registry::ToyFunction,
    internal_symbol: &str,
) {
    let ret_ty_name_owned;
    let ret_ty_name: &str = match func.return_ty.as_deref() {
        Some(s) => { ret_ty_name_owned = s.to_string(); &ret_ty_name_owned }
        None => "void",
    };

    // Run type resolution pass to get typed AST
    let typed_body = crate::toylang::type_resolve::resolve_fn_body(ctx.registry, func);
    let body = func.body.as_ref().unwrap();

    // Resolve Vec symbols if needed
    let uses_vec = body_uses_vec(body);
    if uses_vec {
        let elem_name = find_vec_elem_name(ret_ty_name, ctx.registry)
            .or_else(|| find_vec_elem_from_params(func))
            .or_else(|| find_vec_elem_from_body(body));
        if let Some(elem) = elem_name {
            ctx.resolve_vec_symbols(&elem);
        }
    }

    // Determine internal return type from the typed AST
    let ret_resolved = typed_body.ret.as_ref()
        .map(|e| e.ty.clone())
        .unwrap_or(ResolvedType::Void);
    let internal_sret = is_internal_sret(&ret_resolved);

    // The LLVM type for the sret value (struct or opaque Vec)
    let ret_llvm_ty: Option<BasicTypeEnum<'ctx>> = if internal_sret {
        Some(ctx.resolved_to_inkwell(&ret_resolved))
    } else {
        None
    };

    // Build function signature
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let usize_ty = ctx.usize_type();

    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    let mut param_names: Vec<String> = Vec::new();

    // sret pointer as first param for struct/vec returns
    if internal_sret {
        param_types.push(ptr_ty.into());
        param_names.push("retval".to_string());
    }

    // Real parameters
    for p in &func.params {
        let ty = match p.ty.as_str() {
            "i32" => ctx.context.i32_type().into(),
            "i64" => ctx.context.i64_type().into(),
            "f64" => ctx.context.f64_type().into(),
            "bool" => ctx.context.bool_type().into(),
            "usize" => usize_ty.into(),
            other if other.starts_with("&Vec<") => BasicMetadataTypeEnum::from(ptr_ty),
            other if other.starts_with("&") => BasicMetadataTypeEnum::from(ptr_ty),
            other if ctx.registry.structs.contains_key(other) => {
                let s = &ctx.registry.structs[other];
                ctx.struct_type(s).into()
            }
            other => panic!("unsupported param type '{}'", other),
        };
        param_types.push(ty);
        param_names.push(p.name.clone());
    }

    // LLVM return type: void for sret/void, direct type for primitives
    let llvm_ret_type: Option<BasicTypeEnum<'ctx>> = if internal_sret || ret_resolved == ResolvedType::Void {
        None
    } else {
        Some(ctx.resolved_to_inkwell(&ret_resolved))
    };

    let fn_type = match llvm_ret_type {
        Some(ret) => ret.fn_type(&param_types, false),
        None => ctx.context.void_type().fn_type(&param_types, false),
    };

    // Use existing declaration if forward-declared by a caller's FnCall arm
    let function = if let Some(existing) = ctx.module.get_function(internal_symbol) {
        existing
    } else {
        ctx.module.add_function(internal_symbol, fn_type, None)
    };

    let entry = ctx.context.append_basic_block(function, "entry");
    ctx.builder.position_at_end(entry);

    // Bind parameters to variables
    ctx.vars.clear();
    let param_offset = if internal_sret { 1 } else { 0 };
    for (i, p) in func.params.iter().enumerate() {
        let param_val = function.get_nth_param((i + param_offset) as u32).unwrap();
        let param_ty = param_val.get_type();
        let alloca = ctx.builder.build_alloca(param_ty, &p.name).unwrap();
        ctx.builder.build_store(alloca, param_val).unwrap();
        ctx.vars.insert(p.name.clone(), (alloca, param_ty));
    }

    // Lower body statements
    for stmt in &typed_body.stmts {
        lower_typed_stmt(ctx, stmt);
    }

    // Lower return expression
    if let Some(ref ret_expr) = typed_body.ret {
        let result = lower_typed_expr(ctx, ret_expr);

        if internal_sret {
            // Store into sret pointer
            let sret_ptr = function.get_nth_param(0).unwrap().into_pointer_value();
            let sret_ty = ret_llvm_ty.unwrap();
            let src_ptr = result.into_ptr(&ctx.builder, sret_ty, "ret_src");
            let size = match sret_ty {
                BasicTypeEnum::ArrayType(arr) => arr.size_of().unwrap(),
                BasicTypeEnum::StructType(st) => st.size_of().unwrap(),
                _ => panic!("unexpected sret type: {:?}", sret_ty),
            };
            ctx.builder.build_memcpy(
                sret_ptr, ctx.pointer_align as u32,
                src_ptr, ctx.pointer_align as u32,
                size,
            ).unwrap();
            ctx.builder.build_return(None).unwrap();
        } else if llvm_ret_type.is_some() {
            // Primitive return
            let val = result.into_value(&ctx.builder);
            ctx.builder.build_return(Some(&val)).unwrap();
        } else {
            ctx.builder.build_return(None).unwrap();
        }
    } else {
        ctx.builder.build_return(None).unwrap();
    }
}

/// Codegen a thin extern wrapper that matches Rust ABI and delegates to the internal function.
fn codegen_extern_wrapper<'ctx, 'tcx>(
    ctx: &mut CodegenCtx<'ctx, 'tcx, '_>,
    func: &crate::toylang::registry::ToyFunction,
    instance: ty::Instance<'tcx>,
    extern_symbol: &str,
    internal_symbol: &str,
) {
    let ret_ty_name_owned;
    let ret_ty_name: &str = match func.return_ty.as_deref() {
        Some(s) => { ret_ty_name_owned = s.to_string(); &ret_ty_name_owned }
        None => "void",
    };

    // Resolve the return type for internal ABI decisions
    let ret_resolved = crate::toylang::type_resolve::resolve_return_type(ctx.registry, func);
    let internal_sret = is_internal_sret(&ret_resolved);

    // Query Rust ABI for the extern wrapper's return convention
    let coerced = rustc_lang_facade::abi_helpers::coerced_return_type_for_instance(ctx.tcx, instance);

    // Determine LLVM return type for complex types (structs and opaque Rust types)
    let ret_complex_ty: Option<BasicTypeEnum<'ctx>> = if ret_ty_name.starts_with("Vec<") {
        let vec_resolved = ResolvedType::Vec { elem: Box::new(ResolvedType::I32) };
        Some(ctx.rust_ty_to_llvm_opaque(&vec_resolved).0)
    } else {
        ctx.struct_type_for_ret(ret_ty_name).map(|t| t.into())
    };

    let rust_sret = matches!(coerced, rustc_lang_facade::abi_helpers::CoercedReturn::Indirect);

    // Build extern wrapper signature (matching Rust ABI)
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let usize_ty = ctx.usize_type();

    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    if rust_sret {
        param_types.push(ptr_ty.into());
    }
    for p in &func.params {
        let ty = match p.ty.as_str() {
            "i32" => ctx.context.i32_type().into(),
            "i64" => ctx.context.i64_type().into(),
            "f64" => ctx.context.f64_type().into(),
            "bool" => ctx.context.bool_type().into(),
            "usize" => usize_ty.into(),
            other if other.starts_with("&Vec<") => BasicMetadataTypeEnum::from(ptr_ty),
            other if other.starts_with("&") => BasicMetadataTypeEnum::from(ptr_ty),
            other if ctx.registry.structs.contains_key(other) => {
                let s = &ctx.registry.structs[other];
                ctx.struct_type(s).into()
            }
            other => panic!("unsupported param type '{}'", other),
        };
        param_types.push(ty);
    }

    // Determine Rust ABI LLVM return type
    let rust_ret_type: Option<BasicTypeEnum<'ctx>> = if rust_sret {
        None
    } else if ret_ty_name == "void" {
        None
    } else if ret_ty_name == "usize" || ret_ty_name == "i32" || ret_ty_name == "i64"
           || ret_ty_name == "f64" || ret_ty_name == "bool" {
        Some(match ret_ty_name {
            "usize" => usize_ty.into(),
            "i32" => ctx.context.i32_type().into(),
            "i64" => ctx.context.i64_type().into(),
            "f64" => ctx.context.f64_type().into(),
            "bool" => ctx.context.bool_type().into(),
            _ => unreachable!(),
        })
    } else if ret_ty_name.starts_with("Vec<") {
        None
    } else {
        match &coerced {
            rustc_lang_facade::abi_helpers::CoercedReturn::Direct(coerced_str) => {
                Some(parse_coerced_type(ctx, coerced_str))
            }
            _ => ret_complex_ty.map(|t| t.into()),
        }
    };

    let fn_type = match rust_ret_type {
        Some(ret) => ret.fn_type(&param_types, false),
        None => ctx.context.void_type().fn_type(&param_types, false),
    };

    let function = ctx.module.add_function(extern_symbol, fn_type, None);

    // Add sret attribute if Rust ABI uses sret
    if rust_sret {
        if let Some(sty) = ret_complex_ty {
            let any_ty = basic_type_to_any(sty);
            function.add_attribute(
                inkwell::attributes::AttributeLoc::Param(0),
                ctx.context.create_type_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id("sret"),
                    any_ty,
                ),
            );
        }
    }

    let entry = ctx.context.append_basic_block(function, "entry");
    ctx.builder.position_at_end(entry);

    // Get the internal function (must already be defined)
    let internal_fn = ctx.module.get_function(internal_symbol)
        .unwrap_or_else(|| panic!("internal function '{}' not found", internal_symbol));

    // Build call args: forward params (with sret adaptation)
    let rust_param_offset = if rust_sret { 1 } else { 0 };
    let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();

    if internal_sret {
        if rust_sret {
            // Both use sret — pass Rust's sret pointer directly to internal
            let sret_ptr = function.get_nth_param(0).unwrap().into_pointer_value();
            call_args.push(sret_ptr.into());
        } else {
            // Internal uses sret but Rust returns directly — alloca a tmp
            let ret_ty = ctx.resolved_to_inkwell(&ret_resolved);
            let tmp = ctx.builder.build_alloca(ret_ty, "wrapper_sret").unwrap();
            call_args.push(tmp.into());
        }
    }

    // Forward real params
    for i in 0..func.params.len() {
        let param = function.get_nth_param((i + rust_param_offset) as u32).unwrap();
        call_args.push(param.into());
    }

    let call_site = ctx.builder.build_call(internal_fn, &call_args, "wrapper_call").unwrap();

    // Handle return value adaptation
    if ret_resolved == ResolvedType::Void {
        ctx.builder.build_return(None).unwrap();
    } else if rust_sret {
        // Rust uses sret — internal already wrote to the sret pointer
        ctx.builder.build_return(None).unwrap();
    } else if internal_sret {
        // Internal used sret, but Rust expects a direct return (coerced)
        // The tmp alloca has the result — load as the Rust-expected type
        let tmp = call_args[0].into_pointer_value();
        let coerced_val = ctx.builder.build_load(
            rust_ret_type.unwrap(), tmp, "coerced_ret",
        ).unwrap();
        ctx.builder.build_return(Some(&coerced_val)).unwrap();
    } else {
        // Both return directly — forward the internal function's return value
        match call_site.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(val) => {
                let coerced = coerce_int_to_type(ctx, val, rust_ret_type.unwrap());
                ctx.builder.build_return(Some(&coerced)).unwrap();
            }
            _ => {
                ctx.builder.build_return(None).unwrap();
            }
        }
    }
}

// ============================================================================
// Typed expression/statement lowering (uses TypedExpr from type_resolve)
// ============================================================================

fn lower_typed_expr<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,
    expr: &TypedExpr,
) -> ExprResult<'ctx> {
    match &expr.kind {
        TypedExprKind::IntLit(n) => {
            let inkwell_ty = ctx.resolved_to_inkwell(&expr.ty);
            match inkwell_ty {
                BasicTypeEnum::IntType(int_ty) => {
                    ExprResult::Value(int_ty.const_int(*n as u64, *n < 0).into())
                }
                _ => panic!("IntLit with non-int resolved type: {:?}", expr.ty),
            }
        }

        TypedExprKind::BoolLit(b) => {
            let val = ctx.context.bool_type().const_int(if *b { 1 } else { 0 }, false);
            ExprResult::Value(val.into())
        }

        TypedExprKind::Var(name) => {
            let (ptr, ty) = ctx.vars.get(name.as_str())
                .unwrap_or_else(|| panic!("variable '{}' not in scope", name))
                .clone();
            ExprResult::Ptr(ptr, ty)
        }

        TypedExprKind::BinaryOp { op, left, right } => {
            use crate::toylang::ast::BinOp;
            let lhs = lower_typed_expr(ctx, left).into_value(&ctx.builder);
            let rhs = lower_typed_expr(ctx, right).into_value(&ctx.builder);
            let result = match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let val = match op {
                        BinOp::Add => ctx.builder.build_int_add(l, r, "add").unwrap(),
                        BinOp::Sub => ctx.builder.build_int_sub(l, r, "sub").unwrap(),
                        BinOp::Mul => ctx.builder.build_int_mul(l, r, "mul").unwrap(),
                        BinOp::Div => ctx.builder.build_int_signed_div(l, r, "div").unwrap(),
                    };
                    BasicValueEnum::IntValue(val)
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let val = match op {
                        BinOp::Add => ctx.builder.build_float_add(l, r, "fadd").unwrap(),
                        BinOp::Sub => ctx.builder.build_float_sub(l, r, "fsub").unwrap(),
                        BinOp::Mul => ctx.builder.build_float_mul(l, r, "fmul").unwrap(),
                        BinOp::Div => ctx.builder.build_float_div(l, r, "fdiv").unwrap(),
                    };
                    BasicValueEnum::FloatValue(val)
                }
                _ => panic!("BinaryOp: mismatched operand types"),
            };
            ExprResult::Value(result)
        }

        TypedExprKind::StructLit { name, fields } => {
            let struct_ty = ctx.resolved_to_struct_type(&expr.ty);
            let alloca = ctx.builder.build_alloca(struct_ty, name).unwrap();

            let toy_struct = ctx.registry.structs.get(name.as_str())
                .unwrap_or_else(|| panic!("struct '{}' not found", name));

            for (field_name, field_expr) in fields {
                let field_idx = toy_struct.fields.iter()
                    .position(|f| f.name == *field_name)
                    .unwrap_or_else(|| panic!("field '{}' not found in '{}'", field_name, name));

                let gep = ctx.builder.build_struct_gep(struct_ty, alloca, field_idx as u32, field_name)
                    .unwrap();

                let val = lower_typed_expr(ctx, field_expr);
                let field_inkwell_ty = ctx.resolved_to_inkwell(&field_expr.ty);

                // For complex types (structs, Vecs), copy memory. For primitives, store directly.
                match &field_expr.ty {
                    ResolvedType::Struct { .. } | ResolvedType::Vec { .. } => {
                        let src_ptr = val.into_ptr(&ctx.builder, field_inkwell_ty, "src");
                        let size_val = field_inkwell_ty.size_of().unwrap();
                        ctx.builder.build_memcpy(gep, 8, src_ptr, 8, size_val).unwrap();
                    }
                    _ => {
                        let direct_val = val.into_value(&ctx.builder);
                        ctx.builder.build_store(gep, direct_val).unwrap();
                    }
                }
            }

            ExprResult::Ptr(alloca, struct_ty.into())
        }

        TypedExprKind::FnCall { name, args, .. } if name == "println" => {
            // Build printf format string from the first arg (must be StringLit)
            let TypedExprKind::StringLit(fmt) = &args[0].kind else {
                panic!("println first arg must be string literal");
            };

            // Replace {} with type-appropriate printf specifiers
            let mut printf_fmt = String::new();
            let mut arg_idx = 1usize;
            let mut chars = fmt.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '{' && chars.peek() == Some(&'}') {
                    chars.next(); // consume '}'
                    let spec = match &args[arg_idx].ty {
                        ResolvedType::I32 | ResolvedType::Bool => "%d",
                        ResolvedType::I64 => "%ld",
                        ResolvedType::Usize => "%zu",
                        ResolvedType::F64 => "%f",
                        other => panic!("println: unsupported type {:?} for format arg", other),
                    };
                    printf_fmt.push_str(spec);
                    arg_idx += 1;
                } else {
                    printf_fmt.push(c);
                }
            }
            printf_fmt.push('\n');

            // Create global string constant for format
            let fmt_ptr = ctx.builder.build_global_string_ptr(&printf_fmt, "println_fmt")
                .unwrap()
                .as_pointer_value();

            // Declare printf (variadic, C linkage)
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
            let printf_ty = ctx.context.i32_type().fn_type(&[ptr_ty.into()], true);
            let printf_fn = ctx.module.get_function("printf").unwrap_or_else(|| {
                ctx.module.add_function("printf", printf_ty, Some(inkwell::module::Linkage::External))
            });

            // Build call args: format string + each value arg
            let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = vec![fmt_ptr.into()];
            for arg_expr in &args[1..] {
                let val = lower_typed_expr(ctx, arg_expr).into_value(&ctx.builder);
                // Coerce bool (i1) to i32 for printf
                let coerced = if arg_expr.ty == ResolvedType::Bool {
                    ctx.builder.build_int_z_extend(
                        val.into_int_value(),
                        ctx.context.i32_type(), "bool_to_i32").unwrap().into()
                } else {
                    val
                };
                call_args.push(coerced.into());
            }

            ctx.builder.build_call(printf_fn, &call_args, "printf_ret").unwrap();
            ExprResult::Value(ctx.context.i32_type().const_zero().into())
        }

        TypedExprKind::FnCall { name, type_args, args } => {
            // Call a toylang function via its internal symbol (simple, predictable ABI).
            let internal_sym = {
                let mut sym = format!("__toylang_internal_{}", name);
                for arg in type_args {
                    sym.push_str(&format!("__{}", arg));
                }
                sym
            };

            let callee_sret = is_internal_sret(&expr.ty);

            // Build param types for the internal function declaration
            let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
            if callee_sret {
                param_types.push(ctx.context.ptr_type(AddressSpace::default()).into());
            }
            for a in args {
                param_types.push(ctx.resolved_to_inkwell(&a.ty).into());
            }

            // Internal ABI return type: void for sret, direct for primitives
            let internal_ret_type: Option<BasicTypeEnum<'ctx>> = if callee_sret {
                None
            } else {
                Some(ctx.resolved_to_inkwell(&expr.ty))
            };

            let callee = ctx.declare_external_fn(
                &internal_sym, &param_types, internal_ret_type, None,
            );

            // Build call args
            let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();

            if callee_sret {
                // Allocate space for the result and pass as sret pointer
                let ret_ty = ctx.resolved_to_inkwell(&expr.ty);
                let alloca = ctx.builder.build_alloca(ret_ty, "fncall_sret").unwrap();
                call_args.push(alloca.into());

                for a in args {
                    let val = lower_typed_expr(ctx, a).into_value(&ctx.builder);
                    call_args.push(val.into());
                }

                ctx.builder.build_call(callee, &call_args, "").unwrap();
                ExprResult::Ptr(alloca, ret_ty)
            } else {
                for a in args {
                    let val = lower_typed_expr(ctx, a).into_value(&ctx.builder);
                    call_args.push(val.into());
                }

                let result = ctx.builder.build_call(callee, &call_args, "call").unwrap();
                match result.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(val) => ExprResult::Value(val),
                    _ => ExprResult::Value(ctx.context.i32_type().const_zero().into()),
                }
            }
        }

        TypedExprKind::StaticCall { ty, method, args: _ } => {
            match (ty.as_str(), method.as_str()) {
                ("Vec", "new") => {
                    let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, "vec_new");
                    let vec_llvm_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;

                    let new_fn = ctx.declared_fns.iter()
                        .find(|(name, _)| name.contains("new"))
                        .map(|(_, f)| *f)
                        .expect("Vec::new not declared");

                    ctx.builder.build_call(new_fn, &[alloca.into()], "").unwrap();
                    ExprResult::Ptr(alloca, vec_llvm_ty)
                }
                _ => panic!("unsupported static call: {}::{}", ty, method),
            }
        }

        TypedExprKind::StringLit(s) => {
            let ptr = ctx.builder.build_global_string_ptr(s, "str_lit")
                .unwrap()
                .as_pointer_value();
            ExprResult::Value(ptr.into())
        }

        TypedExprKind::FieldAccess { receiver, field } => {
            let recv_result = lower_typed_expr(ctx, receiver);
            let ResolvedType::Struct { name: struct_name, .. } = &receiver.ty else {
                panic!("field access on non-struct");
            };
            let toy_struct = ctx.registry.structs.get(struct_name.as_str()).unwrap();
            let field_idx = toy_struct.fields.iter()
                .position(|f| f.name == *field)
                .unwrap() as u32;
            let struct_ty = ctx.resolved_to_struct_type(&receiver.ty);
            let struct_ptr = recv_result.into_ptr(&ctx.builder,
                struct_ty.as_basic_type_enum(), "fa_recv");
            let gep = ctx.builder.build_struct_gep(struct_ty, struct_ptr, field_idx, field).unwrap();
            match &expr.ty {
                ResolvedType::Struct { .. } | ResolvedType::Vec { .. } => {
                    // Complex types — return pointer to the field
                    ExprResult::Ptr(gep, ctx.resolved_to_inkwell(&expr.ty))
                }
                _ => {
                    // Primitives — load the value
                    let val = ctx.builder.build_load(
                        ctx.resolved_to_inkwell(&expr.ty), gep, field).unwrap();
                    ExprResult::Value(val)
                }
            }
        }

        TypedExprKind::MethodCall { receiver, method, args } => {
            let recv = lower_typed_expr(ctx, receiver);

            // For reference-typed receivers (&Vec<T>), we need the pointer value,
            // not the alloca containing the pointer. Load it.
            let recv_ptr = match &receiver.ty {
                ResolvedType::Ref { .. } => {
                    // Receiver is a reference — load the pointer from the alloca
                    match recv {
                        ExprResult::Ptr(alloca, ty) => {
                            ctx.builder.build_load(ty, alloca, "recv_load")
                                .unwrap()
                                .into_pointer_value()
                        }
                        ExprResult::Value(v) => v.into_pointer_value(),
                    }
                }
                _ => {
                    // Receiver is a value type (e.g., Vec itself) — use the alloca ptr
                    match recv {
                        ExprResult::Ptr(ptr, _) => ptr,
                        _ => panic!("method receiver must be a pointer"),
                    }
                }
            };

            match method.as_str() {
                "push" => {

                    let arg = lower_typed_expr(ctx, &args[0]);
                    let arg_ty = ctx.resolved_to_inkwell(&args[0].ty);
                    let arg_ptr = arg.into_ptr(&ctx.builder, arg_ty, "push_arg");

                    let push_fn = ctx.declared_fns.iter()
                        .find(|(name, _)| name.contains("push"))
                        .map(|(_, f)| *f)
                        .expect("Vec::push not declared");

                    ctx.builder.build_call(push_fn, &[recv_ptr.into(), arg_ptr.into()], "").unwrap();
                    ExprResult::Value(ctx.context.i32_type().const_zero().into())
                }

                "len" => {
                    let len_fn = ctx.declared_fns.iter()
                        .find(|(name, _)| name.contains("len"))
                        .map(|(_, f)| *f)
                        .expect("Vec::len not declared");

                    let call_site = ctx.builder.build_call(len_fn, &[recv_ptr.into()], "len").unwrap();
                    let result = match call_site.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(val) => val,
                        _ => panic!("len should return a basic value"),
                    };
                    ExprResult::Value(result)
                }

                _ => panic!("unsupported method: .{}", method),
            }
        }
    }
}

fn lower_typed_stmt<'ctx>(ctx: &mut CodegenCtx<'ctx, '_, '_>, stmt: &TypedStmt) {
    match stmt {
        TypedStmt::Let { name, expr } => {
            let result = lower_typed_expr(ctx, expr);
            match result {
                ExprResult::Ptr(ptr, ty) => {
                    ctx.vars.insert(name.clone(), (ptr, ty));
                }
                ExprResult::Value(val) => {
                    let ty = val.get_type();
                    let alloca = ctx.builder.build_alloca(ty, name).unwrap();
                    ctx.builder.build_store(alloca, val).unwrap();
                    ctx.vars.insert(name.clone(), (alloca, ty));
                }
            }
        }
        TypedStmt::ExprStmt(expr) => {
            let _ = lower_typed_expr(ctx, expr);
        }
    }
}

// ============================================================================
// Accessor codegen
// ============================================================================

/// Split a string on commas, respecting nested { } braces.
fn split_respecting_braces(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// Parse an LLVM struct type string like "{ i32, i64 }" into an inkwell StructType.
fn parse_struct_type_str<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>, s: &str) -> StructType<'ctx> {
    let inner = s.trim().trim_start_matches('{').trim_end_matches('}').trim();
    // Depth-aware split: don't split on commas inside nested { }
    let field_strs = split_respecting_braces(inner);
    let fields: Vec<BasicTypeEnum<'ctx>> = field_strs.iter()
        .map(|f| {
            let f = f.trim();
            match f {
                "i32" => ctx.context.i32_type().into(),
                "i64" => ctx.context.i64_type().into(),
                "double" => ctx.context.f64_type().into(),
                "i1" => ctx.context.bool_type().into(),
                _ if f.starts_with("[") && f.ends_with("]") => {
                    // Array type like "[24 x i8]"
                    let inner = &f[1..f.len()-1].trim(); // "24 x i8"
                    let parts: Vec<&str> = inner.split(" x ").collect();
                    let count: u32 = parts[0].trim().parse().unwrap();
                    let elem = parts[1].trim();
                    match elem {
                        "i8" => ctx.context.i8_type().array_type(count).into(),
                        _ => panic!("unsupported array element type in struct string: '{}'", elem),
                    }
                }
                _ if f.starts_with("{ ") || f.starts_with("{") => {
                    // Nested struct — recurse
                    parse_struct_type_str(ctx, f).into()
                }
                _ if f.starts_with("i") => {
                    let bits: u32 = f[1..].parse().unwrap_or_else(|_| panic!("bad int type: {}", f));
                    ctx.int_type(bits).into()
                }
                _ => panic!("unsupported type in struct string: '{}'", f),
            }
        })
        .collect();
    ctx.context.struct_type(&fields, false)
}

// ============================================================================
// Helpers (kept from old llvm_gen.rs)
// ============================================================================

fn resolve_rust_symbol<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: rustc_span::def_id::DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> String {
    let instance = ty::Instance::expect_resolve(
        tcx,
        ty::TypingEnv::fully_monomorphized(),
        def_id,
        args,
        rustc_span::DUMMY_SP,
    );
    tcx.symbol_name(instance).name.to_string()
}

fn body_uses_vec(body: &FnBody) -> bool {
    body.stmts.iter().any(stmt_uses_vec)
        || body.ret.as_ref().map_or(false, expr_uses_vec)
}

fn stmt_uses_vec(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Let { expr, .. } => expr_uses_vec(expr),
        Stmt::ExprStmt(expr) => expr_uses_vec(expr),
    }
}

fn expr_uses_vec(expr: &Expr) -> bool {
    match expr {
        Expr::StaticCall { ty, .. } if ty == "Vec" => true,
        Expr::MethodCall { method, .. } if method == "push" || method == "len" => true,
        Expr::MethodCall { receiver, .. } => expr_uses_vec(receiver),
        _ => false,
    }
}

/// Find the Vec element type from push calls in the body (e.g. v.push(Point{...}) → "Point").
pub fn find_vec_elem_from_body(body: &FnBody) -> Option<String> {
    for stmt in &body.stmts {
        let expr = match stmt {
            Stmt::Let { expr, .. } => expr,
            Stmt::ExprStmt(expr) => expr,
        };
        if let Some(name) = find_vec_elem_from_expr(expr) {
            return Some(name);
        }
    }
    if let Some(ref ret) = body.ret {
        if let Some(name) = find_vec_elem_from_expr(ret) {
            return Some(name);
        }
    }
    None
}

fn find_vec_elem_from_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall { method, args, receiver, .. } if method == "push" => {
            // Check the pushed argument for a struct literal
            if let Some(arg) = args.first() {
                if let Expr::StructLit { name, .. } = arg {
                    return Some(name.clone());
                }
            }
            find_vec_elem_from_expr(receiver)
        }
        Expr::MethodCall { receiver, .. } => find_vec_elem_from_expr(receiver),
        _ => None,
    }
}

fn parse_generic_type(ty: &str) -> Option<(&str, Vec<&str>)> {
    let open = ty.find('<')?;
    if !ty.ends_with('>') { return None; }
    let base = &ty[..open];
    let args_str = &ty[open+1..ty.len()-1];
    let args: Vec<&str> = args_str.split(',').map(|s| s.trim()).collect();
    Some((base, args))
}

/// Find the Vec element type name from context. Searches recursively into nested structs,
/// resolving generic type params from the return type string.
fn find_vec_elem_name(ret_ty_name: &str, registry: &ToylangRegistry) -> Option<String> {
    // Direct Vec return: "Vec<Point>" → "Point"
    if ret_ty_name.starts_with("Vec<") && ret_ty_name.ends_with('>') {
        return Some(ret_ty_name[4..ret_ty_name.len()-1].to_string());
    }
    // Non-generic struct
    if let Some(s) = registry.structs.get(ret_ty_name) {
        return find_vec_in_struct_fields(s, registry, &HashMap::new());
    }
    // Generic struct: "ToyWrapper<Vec<i32>>" → parse type args and build subst
    if let Some((base, type_args)) = parse_generic_type(ret_ty_name) {
        if let Some(s) = registry.structs.get(base) {
            let subst: HashMap<&str, &str> = s.type_params.iter()
                .zip(type_args.iter())
                .map(|(param, arg)| (param.as_str(), *arg))
                .collect();
            return find_vec_in_struct_fields(s, registry, &subst);
        }
    }
    None
}

/// Recursively search struct fields for a Vec type, returning its element type name.
/// `subst` maps type param names to their concrete string representations.
fn find_vec_in_struct_fields(
    s: &ToyStruct,
    registry: &ToylangRegistry,
    subst: &HashMap<&str, &str>,
) -> Option<String> {
    for field in &s.fields {
        match &field.rust_type {
            ToyFieldType::RustGeneric(name, args) if name == "Vec" && !args.is_empty() => {
                // Resolve the element type, substituting type params
                let elem_str = resolve_field_type_name(&args[0], subst);
                return Some(elem_str);
            }
            ToyFieldType::TypeParam(param_name) => {
                // Resolve via subst — the concrete type might be a Vec
                if let Some(&concrete) = subst.get(param_name.as_str()) {
                    if concrete.starts_with("Vec<") && concrete.ends_with('>') {
                        return Some(concrete[4..concrete.len()-1].to_string());
                    }
                }
            }
            ToyFieldType::ToyStruct(struct_name) => {
                if let Some(inner) = registry.structs.get(struct_name.as_str()) {
                    if let Some(elem) = find_vec_in_struct_fields(inner, registry, subst) {
                        return Some(elem);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Resolve a ToyFieldType to a type name string, substituting type params.
fn resolve_field_type_name(ft: &ToyFieldType, subst: &HashMap<&str, &str>) -> String {
    match ft {
        ToyFieldType::I32 => "i32".to_string(),
        ToyFieldType::I64 => "i64".to_string(),
        ToyFieldType::F64 => "f64".to_string(),
        ToyFieldType::Bool => "bool".to_string(),
        ToyFieldType::TypeParam(name) => {
            subst.get(name.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| name.clone())
        }
        ToyFieldType::ToyStruct(name) => name.clone(),
        ToyFieldType::RustGeneric(name, args) => {
            let arg_strs: Vec<String> = args.iter()
                .map(|a| resolve_field_type_name(a, subst))
                .collect();
            format!("{}<{}>", name, arg_strs.join(", "))
        }
    }
}

/// Search function parameters for Vec<T> and return the element type name.
fn find_vec_elem_from_params(func: &crate::toylang::registry::ToyFunction) -> Option<String> {
    for p in &func.params {
        let ty = p.ty.trim_start_matches('&');
        if ty.starts_with("Vec<") && ty.ends_with('>') {
            return Some(ty[4..ty.len() - 1].to_string());
        }
    }
    None
}

fn parse_coerced_type<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>, s: &str) -> BasicTypeEnum<'ctx> {
    if s.starts_with("i") {
        let bits: u32 = s[1..].parse().unwrap_or_else(|_| panic!("bad coerced type: {}", s));
        ctx.int_type(bits).into()
    } else if s.starts_with("[") {
        // Array type like "[2 x i32]"
        let inner = &s[1..s.len()-1]; // "2 x i32"
        let parts: Vec<&str> = inner.split(" x ").collect();
        let count: u32 = parts[0].trim().parse().unwrap();
        let elem = parse_coerced_type(ctx, parts[1].trim());
        match elem {
            BasicTypeEnum::IntType(it) => it.array_type(count).into(),
            BasicTypeEnum::FloatType(ft) => ft.array_type(count).into(),
            _ => panic!("unsupported array element type"),
        }
    } else if s.starts_with("{") {
        parse_struct_type_str(ctx, s).into()
    } else {
        panic!("unsupported coerced type: {}", s)
    }
}

// ============================================================================
// Public API (preserved for callbacks_impl.rs and main.rs)
// ============================================================================

/// Check if a function is eligible for external LLVM compilation.
/// Generate LLVM IR by walking MonoItems to find all consumer instances and codegen them.
/// Discovery and codegen happen in the same `'tcx` scope so we can use live Instance values.
pub fn generate_with_tcx<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    callbacks: &crate::toylang::callbacks_impl::ToylangCallbacks,
) -> (String, Vec<String>) {
    let context = Context::create();
    let mut ctx = CodegenCtx::new(&context, tcx, registry);
    let mut seen_symbols = std::collections::HashSet::new();

    // Collect all toylang mono items (accessors and functions) first, then codegen.
    struct FnItem<'tcx> {
        resolved_func: crate::toylang::registry::ToyFunction,
        instance: ty::Instance<'tcx>,
        extern_symbol: String,
    }
    let mut fn_items: Vec<FnItem<'tcx>> = Vec::new();

    let (_, cgus) = tcx.collect_and_partition_mono_items(());
    for cgu in cgus.iter() {
        for (&mono_item, _) in cgu.items() {
            let rustc_middle::mir::mono::MonoItem::Fn(instance) = mono_item else { continue };
            let def_id = instance.def_id();
            if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
                continue;
            }
            let Some(local_def_id) = def_id.as_local() else { continue };

            // Check if it's an accessor method
            if let Some(assoc_item) = tcx.opt_associated_item(def_id) {
                let impl_def_id = assoc_item.container_id(tcx);
                let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
                if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
                    let struct_name = tcx.item_name(adt_def.did()).to_string();
                    if let Some(toy_struct) = registry.structs.get(&struct_name) {
                        let field_name = tcx.item_name(def_id).to_string();
                        if let Some(field_index) = toy_struct.fields.iter().position(|f| f.name == field_name) {
                            let callback_name = format!("{}.{}", struct_name, field_name);
                            let result = callbacks.monomorphize_fn(&callback_name, tcx, local_def_id, instance);
                            if seen_symbols.insert(result.extern_symbol.clone()) {
                                let struct_ty = ctx.struct_type_for_instance(toy_struct, instance);
                                codegen_accessor_inline(&mut ctx, &result.extern_symbol, struct_ty, field_index);
                            }
                        }
                    }
                }
                continue;
            }

            // Check if it's a consumer function
            let name = tcx.item_name(def_id).to_string();
            // __toylang_main wrapper maps back to "main" in the registry
            let registry_name = if name == "__toylang_main" { "main".to_string() } else { name.clone() };
            let Some(toy_fn) = registry.functions.get(&registry_name) else { continue };
            let result = callbacks.monomorphize_fn(&name, tcx, local_def_id, instance);
            if !seen_symbols.insert(result.extern_symbol.clone()) {
                continue;
            }

            // Build resolved function with concrete type args substituted
            let resolved_func = resolve_function_for_instance(toy_fn, &registry_name, instance, tcx);
            fn_items.push(FnItem {
                resolved_func,
                instance,
                extern_symbol: result.extern_symbol,
            });
        }
    }

    // Two-pass codegen: internal functions first, then extern wrappers.
    // Internal functions use a simple ABI (structs always sret, primitives direct).
    // Extern wrappers adapt to Rust ABI and delegate to the internal function.
    // No ordering dependency between internal functions since the internal ABI
    // is predictable from ResolvedType alone.
    for item in &fn_items {
        let internal_symbol = item.extern_symbol.replace("__toylang_impl_", "__toylang_internal_");
        codegen_internal_function(&mut ctx, &item.resolved_func, &internal_symbol);
    }
    for item in &fn_items {
        let internal_symbol = item.extern_symbol.replace("__toylang_impl_", "__toylang_internal_");
        codegen_extern_wrapper(&mut ctx, &item.resolved_func, item.instance, &item.extern_symbol, &internal_symbol);
    }

    let rust_symbols = ctx.rust_symbols.clone();
    let ir = ctx.module.print_to_string().to_string();
    (ir, rust_symbols)
}

/// Codegen an accessor function inline (GEP to field offset, return pointer).
fn codegen_accessor_inline<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,
    extern_symbol: &str,
    struct_ty: StructType<'ctx>,
    field_index: usize,
) {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let fn_type = ptr_ty.fn_type(&[ptr_ty.into()], false);
    let func = ctx.module.add_function(extern_symbol, fn_type, None);
    let entry = ctx.context.append_basic_block(func, "entry");
    ctx.builder.position_at_end(entry);
    let self_ptr = func.get_nth_param(0).unwrap().into_pointer_value();
    let gep = ctx.builder.build_struct_gep(struct_ty, self_ptr, field_index as u32, "ptr").unwrap();
    ctx.builder.build_return(Some(&gep)).unwrap();
}

/// Build a resolved ToyFunction with type params substituted for a concrete instance.
fn resolve_function_for_instance<'tcx>(
    toy_fn: &crate::toylang::registry::ToyFunction,
    name: &str,
    instance: ty::Instance<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> crate::toylang::registry::ToyFunction {
    if toy_fn.type_params.is_empty() {
        return toy_fn.clone();
    }

    // Generic function — substitute type params
    let mut type_arg_subst = std::collections::HashMap::new();
    for (i, param_name) in toy_fn.type_params.iter().enumerate() {
        if let Some(arg) = instance.args.get(i) {
            if let ty::GenericArgKind::Type(ty) = arg.unpack() {
                type_arg_subst.insert(
                    param_name.clone(),
                    crate::toylang::callbacks_impl::rustc_ty_to_type_string(tcx, ty),
                );
            }
        }
    }

    crate::toylang::registry::ToyFunction {
        name: name.to_string(),
        type_params: vec![],
        params: toy_fn.params.iter().map(|p| {
            crate::toylang::registry::ToyParam {
                name: p.name.clone(),
                ty: substitute_type_params_str(&p.ty, &type_arg_subst),
            }
        }).collect(),
        return_ty: toy_fn.return_ty.as_deref().map(|s| substitute_type_params_str(s, &type_arg_subst)),
        body: toy_fn.body.clone(),
    }
}

/// Simple string substitution for type param resolution.
pub fn substitute_type_params_str_pub(s: &str, subst: &std::collections::HashMap<String, String>) -> String {
    substitute_type_params_str(s, subst)
}

fn substitute_type_params_str(s: &str, subst: &std::collections::HashMap<String, String>) -> String {
    // Direct match
    if let Some(replacement) = subst.get(s) {
        return replacement.clone();
    }
    // Generic: "Wrapper<T>" → "Wrapper<i32>"
    if let Some(open) = s.find('<') {
        if s.ends_with('>') {
            let base = &s[..open];
            let args_str = &s[open + 1..s.len() - 1];
            let args: Vec<&str> = split_respecting_braces(args_str);
            let resolved: Vec<String> = args.iter()
                .map(|a| substitute_type_params_str(a.trim(), subst))
                .collect();
            return format!("{}<{}>", base, resolved.join(", "));
        }
    }
    s.to_string()
}

