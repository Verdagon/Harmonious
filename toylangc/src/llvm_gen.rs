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

use crate::toylang::typed_ast::*;
use crate::toylang::registry::{ToylangRegistry, ToyStruct};


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
    /// Rust method functions keyed by mangled symbol name.
    /// Lazily populated via get_or_resolve_rust_method.
    rust_method_info: HashMap<String, RustMethodInfo<'ctx>>,
}

/// Info about a resolved Rust method (Vec::push, Vec::new, etc.)
struct RustMethodInfo<'ctx> {
    func: FunctionValue<'ctx>,
    is_sret: bool,
    /// Per @TCHAPZ, #[track_caller] functions have a hidden &Location parameter
    /// that we must pass as null at every call site.
    has_track_caller: bool,
    /// ABI-coerced parameter types, used at call sites to detect ScalarPair splitting.
    coerced_params: Vec<rustc_lang_facade::abi_helpers::CoercedParam>,
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
            rust_method_info: HashMap::new(),
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
            ResolvedType::RustType { .. } => self.rust_ty_to_llvm_opaque(ty).0,
            // Per @UTAIRZ, bare Str/ByteSlice are unsized and have no LLVM layout;
            // they must be wrapped in Ref before reaching codegen.
            ResolvedType::Str => panic!("bare Str should not appear without Ref wrapper"),
            ResolvedType::ByteSlice => panic!("bare ByteSlice should not appear without Ref wrapper"),
            // Per @UTAIRZ, &str and &[u8] share the ScalarPair layout { ptr, i64 };
            // this single arm handles both.
            ResolvedType::Ref { inner } if matches!(
                inner.as_ref(),
                ResolvedType::ByteSlice | ResolvedType::Str,
            ) => {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let len_ty = self.context.i64_type();
                self.context.struct_type(&[ptr_ty.into(), len_ty.into()], false).into()
            }
            ResolvedType::Ref { inner: _ } => {
                self.context.ptr_type(AddressSpace::default()).into()
            }
            ResolvedType::TypeParam(name) => panic!("TypeParam '{}' should be substituted before codegen", name),
            ResolvedType::StructRef { name, .. } => panic!("StructRef '{}' should be resolved to Struct before codegen", name),
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
            ResolvedType::RustType { .. } => panic!("Vec is opaque — use rust_ty_to_llvm_opaque, not struct GEP"),
            _ => panic!("expected struct type, got {:?}", ty),
        }
    }

    // --- Type resolution ---

    fn struct_type(&self, s: &ToyStruct) -> StructType<'ctx> {
        let fields: Vec<BasicTypeEnum<'ctx>> = s.fields.iter()
            .map(|f| {
                let resolved = crate::toylang::type_resolve::resolve_struct_fields(&f.rust_type, self.registry)
                    .expect("struct field resolution should succeed (already validated)");
                self.resolved_to_inkwell(&resolved)
            })
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

        // Build type param → concrete ResolvedType map
        let subst: HashMap<String, ResolvedType> = toy_struct.type_params.iter()
            .enumerate()
            .map(|(i, param_name)| {
                let concrete_ty = args[i].expect_ty();
                (param_name.clone(), crate::oracle::rustc_ty_to_resolved_type(self.tcx, concrete_ty))
            })
            .collect();

        // Resolve each field type with substitution
        let fields: Vec<BasicTypeEnum<'ctx>> = toy_struct.fields.iter()
            .map(|f| {
                let resolved = crate::toylang::type_resolve::substitute_type_params(&f.rust_type, &subst);
                let resolved = crate::toylang::type_resolve::resolve_struct_fields(&resolved, self.registry)
                    .expect("struct field resolution should succeed (already validated)");
                self.resolved_to_inkwell(&resolved)
            })
            .collect();
        self.context.struct_type(&fields, false)
    }

    fn resolved_type_to_rustc_ty(&self, resolved: &ResolvedType) -> ty::Ty<'tcx> {
        crate::oracle::resolved_to_rustc_ty(self.tcx, resolved)
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

    // --- Rust method resolution ---

    /// Lazily resolve and cache a Rust method. Returns the mangled symbol name
    /// (which is the key into rust_method_info).
    fn get_or_resolve_rust_method(
        &mut self,
        type_name: &str,
        method_name: &str,
        type_args: &[ResolvedType],
        receiver_ty: Option<&ResolvedType>,
    ) -> String {
        let (method_def_id, args) = if let Some(recv_ty) = receiver_ty {
            // Trait static call: look up trait, find impl for receiver
            if let Some(trait_def_id) = crate::oracle::find_use_imported_trait_def_id(self.tcx, type_name) {
                let self_resolved = crate::oracle::strip_ref(recv_ty);
                let self_ty = self.resolved_type_to_rustc_ty(self_resolved);
                // Per @TVIMDGAZ, use trait definition method DefId with [Self, ...] args
                let trait_method_def_id = self.tcx.associated_item_def_ids(trait_def_id)
                    .iter()
                    .find(|&&id| self.tcx.item_name(id).as_str() == method_name)
                    .copied()
                    .unwrap_or_else(|| panic!("method '{}' not defined on trait '{}'", method_name, type_name));
                let mut all_ty_args: Vec<GenericArg<'tcx>> = vec![GenericArg::from(self_ty)];
                for ta in type_args {
                    all_ty_args.push(GenericArg::from(self.resolved_type_to_rustc_ty(ta)));
                }
                // @ELASZ
                let args = crate::oracle::build_generic_args_for_item(self.tcx, trait_method_def_id, &all_ty_args);
                (trait_method_def_id, args)
            } else {
                // Fall through to inherent lookup
                let type_def_id = crate::oracle::find_rust_type_def_id(self.tcx, type_name)
                    .unwrap_or_else(|| panic!("Rust type '{}' not found", type_name));
                let method_def_id = crate::oracle::find_inherent_method(self.tcx, type_def_id, method_name)
                    .unwrap_or_else(|| panic!("method '{}' not found on '{}'", method_name, type_name));
                let all_ty_args: Vec<GenericArg<'tcx>> = type_args.iter()
                    .map(|ta| GenericArg::from(self.resolved_type_to_rustc_ty(ta)))
                    .collect();
                // @ELASZ
                let args = crate::oracle::build_generic_args_for_item(self.tcx, method_def_id, &all_ty_args);
                (method_def_id, args)
            }
        } else {
            // Phase 6: redirect to wrapper if applicable. Must call the same
            // helper as collect_toylang_fn_deps_inner so tcx.symbol_name
            // produces identical output on the dep-registration and codegen
            // paths.
            if let Some((wdef, wargs)) = crate::oracle::redirect_to_wrapper(
                self.tcx, type_name, method_name, type_args,
            ) {
                (wdef, wargs)
            } else {
                // Inherent method call
                let type_def_id = crate::oracle::find_rust_type_def_id(self.tcx, type_name)
                    .unwrap_or_else(|| panic!("Rust type '{}' not found", type_name));
                let method_def_id = crate::oracle::find_inherent_method(self.tcx, type_def_id, method_name)
                    .unwrap_or_else(|| panic!("method '{}' not found on '{}'", method_name, type_name));
                let all_ty_args: Vec<GenericArg<'tcx>> = type_args.iter()
                    .map(|ta| GenericArg::from(self.resolved_type_to_rustc_ty(ta)))
                    .collect();
                // @ELASZ
                let args = crate::oracle::build_generic_args_for_item(self.tcx, method_def_id, &all_ty_args);
                (method_def_id, args)
            }
        };

        // Build Instance for ABI query. Per @SMINCZ, expect_resolve and
        // symbol_name are read-only — they don't drive codegen. Codegen
        // is driven separately via `rust_deps` registration in
        // collect_toylang_fn_deps_inner; both sites must produce the same
        // Instance so the symbol string here matches what rustc emits.
        let instance = ty::Instance::expect_resolve(
            self.tcx,
            ty::TypingEnv::fully_monomorphized(),
            method_def_id,
            args,
            rustc_span::DUMMY_SP,
        );
        // Detect hidden #[track_caller] parameter (see @TCHAPZ)
        let has_track_caller = instance.def.requires_caller_location(self.tcx);
        let symbol = self.tcx.symbol_name(instance).name.to_string();

        // Check cache
        if self.rust_method_info.contains_key(&symbol) {
            return symbol;
        }

        self.rust_symbols.push(symbol.clone());

        // Query rustc ABI to determine calling convention
        let coerced_ret = rustc_lang_facade::abi_helpers::coerced_return_type_for_instance(self.tcx, instance);
        let coerced_params = rustc_lang_facade::abi_helpers::coerced_param_types_for_instance(self.tcx, instance);


        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Build param types from ABI
        let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        let is_sret = matches!(coerced_ret, rustc_lang_facade::abi_helpers::CoercedReturn::Indirect);
        if is_sret {
            param_types.push(ptr_ty.into()); // sret pointer
        }
        for cp in &coerced_params {
            match cp {
                rustc_lang_facade::abi_helpers::CoercedParam::Ignore => {}
                rustc_lang_facade::abi_helpers::CoercedParam::Direct(ty_str) => {
                    param_types.push(parse_coerced_type(self, ty_str).into());
                }
                rustc_lang_facade::abi_helpers::CoercedParam::Pair(a_str, b_str) => {
                    param_types.push(parse_coerced_type(self, a_str).into());
                    param_types.push(parse_coerced_type(self, b_str).into());
                }
                rustc_lang_facade::abi_helpers::CoercedParam::Indirect => {
                    param_types.push(ptr_ty.into());
                }
            }
        }

        // Per @ACRTFDZ, must use parse_coerced_type (the ABI type), not
        // resolved_to_inkwell (the toylang type).
        let ret_type: Option<BasicTypeEnum<'ctx>> = match &coerced_ret {
            rustc_lang_facade::abi_helpers::CoercedReturn::Direct(s) => {
                Some(parse_coerced_type(self, s))
            }
            rustc_lang_facade::abi_helpers::CoercedReturn::Indirect | rustc_lang_facade::abi_helpers::CoercedReturn::Void => None,
        };

        // For sret, get the opaque type + alignment for the sret attribute
        let sret_info = if is_sret {
            // Get return type from fn_sig to determine opaque layout
            let sig = self.tcx.fn_sig(method_def_id).instantiate(self.tcx, args);
            let sig = self.tcx.normalize_erasing_late_bound_regions(
                ty::TypingEnv::fully_monomorphized(), sig,
            );
            let ret_resolved = crate::oracle::rustc_ty_to_resolved_type(self.tcx, sig.output());
            Some(self.rust_ty_to_llvm_opaque(&ret_resolved))
        } else {
            None
        };

        let func = self.declare_external_fn(
            &symbol,
            &param_types,
            ret_type,
            sret_info,
        );

        self.rust_method_info.insert(symbol.clone(), RustMethodInfo { func, is_sret, has_track_caller, coerced_params });
        symbol
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

/// Push an argument to call_args, dispatching on `CoercedParam` (the rustc-side
/// ABI truth) rather than toylang's view of the arg type. Handles all four
/// variants: Direct (value, coerced), Pair (extract+split), Indirect (ptr),
/// Ignore (ZST — lower for side effects only).
fn push_arg_for_rust_call<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,
    arg_expr: &TypedExpr,
    coerced_param: &rustc_lang_facade::abi_helpers::CoercedParam,
    call_args: &mut Vec<inkwell::values::BasicMetadataValueEnum<'ctx>>,
) {
    use rustc_lang_facade::abi_helpers::CoercedParam;
    match coerced_param {
        CoercedParam::Indirect => {
            // Pass by pointer — the previous unconditional behavior, now
            // explicit. Used for sret-like aggregates and any large value
            // rustc passes via memory.
            let arg = lower_typed_expr(ctx, arg_expr);
            let arg_ty = ctx.resolved_to_inkwell(&arg_expr.ty);
            let arg_ptr = arg.into_ptr(&ctx.builder, arg_ty, "method_arg");
            call_args.push(arg_ptr.into());
        }
        CoercedParam::Pair(_a_str, _b_str) => {
            // Fat pointer / two-scalar aggregate — extract and pass separately.
            // Source of truth is rustc's coerced_param shape.
            let val = lower_typed_expr(ctx, arg_expr).into_value(&ctx.builder);
            let struct_val = val.into_struct_value();
            let first = ctx.builder.build_extract_value(struct_val, 0, "pair_first").unwrap();
            let second = ctx.builder.build_extract_value(struct_val, 1, "pair_second").unwrap();
            call_args.push(first.into());
            call_args.push(second.into());
        }
        CoercedParam::Direct(llvm_ty_str) => {
            // Pass by value, coerced to the LLVM type rustc declared. For
            // primitive scalars (i32, i64, etc.) this is the toylang value
            // possibly width-adjusted; for `ptr`-typed Direct (e.g. `&T`),
            // the toylang value is already a pointer.
            let val = lower_typed_expr(ctx, arg_expr).into_value(&ctx.builder);
            let target_ty = parse_coerced_type(ctx, llvm_ty_str);
            let coerced = coerce_int_to_type(ctx, val, target_ty);
            call_args.push(coerced.into());
        }
        CoercedParam::Ignore => {
            // ZST — no LLVM param. Lower for side effects only.
            let _ = lower_typed_expr(ctx, arg_expr);
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
/// Walk a typed body and resolve any Rust method symbols (StaticCall/MethodCall on RustType).
fn resolve_rust_methods_from_typed_body(ctx: &mut CodegenCtx, body: &TypedBlock) {
    fn walk_expr(ctx: &mut CodegenCtx, expr: &TypedExpr) {
        match &expr.kind {
            TypedExprKind::StaticCall { ty, method, type_args, args } => {
                let receiver_ty = args.first().map(|a| &a.ty);
                ctx.get_or_resolve_rust_method(ty, method, type_args, receiver_ty);
                for arg in args { walk_expr(ctx, arg); }
                return; // already walked children
            }
            TypedExprKind::MethodCall { receiver, method, args } => {
                // Extract RustType from receiver (direct or via &ref)
                let rust_info = match &receiver.ty {
                    ResolvedType::RustType { name, type_args } => Some((name.as_str(), type_args.as_slice())),
                    ResolvedType::Ref { inner } => match inner.as_ref() {
                        ResolvedType::RustType { name, type_args } => Some((name.as_str(), type_args.as_slice())),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((type_name, type_args)) = rust_info {
                    ctx.get_or_resolve_rust_method(type_name, method, type_args, None);
                }
                walk_expr(ctx, receiver);
                for arg in args { walk_expr(ctx, arg); }
                return; // already walked children
            }
            TypedExprKind::FnCall { args, .. } => {
                for arg in args { walk_expr(ctx, arg); }
            }
            TypedExprKind::StructLit { fields, .. } => {
                for (_, e) in fields { walk_expr(ctx, e); }
            }
            TypedExprKind::BinaryOp { left, right, .. } => {
                walk_expr(ctx, left);
                walk_expr(ctx, right);
            }
            TypedExprKind::FieldAccess { receiver, .. } => {
                walk_expr(ctx, receiver);
            }
            TypedExprKind::If { cond, then_stmts, then_expr, else_stmts, else_expr } => {
                walk_expr(ctx, cond);
                for stmt in then_stmts {
                    match stmt {
                        TypedStmt::Let { expr, .. } | TypedStmt::ExprStmt(expr) | TypedStmt::Assign { expr, .. } => walk_expr(ctx, expr),
                        TypedStmt::While { cond, body } => { walk_expr(ctx, cond); walk_body(ctx, body); }
                    }
                }
                if let Some(e) = then_expr { walk_expr(ctx, e); }
                for stmt in else_stmts {
                    match stmt {
                        TypedStmt::Let { expr, .. } | TypedStmt::ExprStmt(expr) | TypedStmt::Assign { expr, .. } => walk_expr(ctx, expr),
                        TypedStmt::While { cond, body } => { walk_expr(ctx, cond); walk_body(ctx, body); }
                    }
                }
                if let Some(e) = else_expr { walk_expr(ctx, e); }
            }
            TypedExprKind::Ref(inner) => {
                walk_expr(ctx, inner);
            }
            _ => {} // IntLit, BoolLit, Var, StringLit
        }
    }
    fn walk_body(ctx: &mut CodegenCtx, body: &crate::toylang::typed_ast::TypedBlock) {
        for stmt in &body.stmts {
            match stmt {
                TypedStmt::Let { expr, .. } => walk_expr(ctx, expr),
                TypedStmt::ExprStmt(expr) => walk_expr(ctx, expr),
                TypedStmt::While { cond, body } => {
                    walk_expr(ctx, cond);
                    walk_body(ctx, body);
                }
                TypedStmt::Assign { expr, .. } => walk_expr(ctx, expr),
            }
        }
        if let Some(ref ret) = body.ret {
            walk_expr(ctx, ret);
        }
    }
    walk_body(ctx, body);
}

fn is_internal_sret(ty: &ResolvedType) -> bool {
    matches!(ty, ResolvedType::Struct { .. } | ResolvedType::RustType { .. })
}

/// Codegen a toylang function body with the simple internal ABI.
/// Structs/Vec always use sret (ptr first param, void return).
/// Primitives return directly. No Rust ABI coercion.
fn codegen_internal_function<'ctx, 'tcx>(
    ctx: &mut CodegenCtx<'ctx, 'tcx, '_>,
    func: &crate::toylang::registry::ToyFunction,
    internal_symbol: &str,
) {
    // Per @MBMRVZ, this function's return type is inferred from the
    // body's tail expression. For `main`, that must be void, or the
    // extern wrapper (whose signature is pinned to `fn __toylang_main()`
    // by the Rust shim) will call us with a missing sret buffer and
    // SIGBUS during our final return-value store.
    // Query rustc for Rust method return types
    let rust_method_ret = |type_name: &str, method: &str, type_args: &[ResolvedType]| -> Result<ResolvedType, crate::oracle::UnresolvedRustType> {
        if type_name.is_empty() {
            crate::oracle::rust_free_fn_return_type(ctx.tcx, method, type_args)
                .map(|opt| opt.unwrap_or(ResolvedType::Void))
        } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
            let receiver_ty = &type_args[0];
            let explicit_args = &type_args[1..];
            crate::oracle::rust_trait_method_return_type(ctx.tcx, trait_name, method, receiver_ty, explicit_args)
        } else {
            crate::oracle::rust_method_return_type(ctx.tcx, type_name, method, type_args)
        }
    };
    let rust_param_types = |type_name: &str, method: &str, type_args: &[ResolvedType]| -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType> {
        if type_name.is_empty() {
            crate::oracle::rust_free_fn_param_types(ctx.tcx, method, type_args)
        } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
            crate::oracle::rust_trait_method_param_types(ctx.tcx, trait_name, method, &type_args[0], &type_args[1..])
        } else {
            crate::oracle::rust_method_param_types(ctx.tcx, type_name, method, type_args)
        }
    };
    // Per @IVTDBTZ, trait-vs-inherent dispatch predicate.
    let is_rust_trait = |name: &str| {
        crate::oracle::find_use_imported_trait_def_id(ctx.tcx, name).is_some()
    };
    let typed_body = crate::toylang::type_resolve::resolve_fn_body(ctx.registry, func, &rust_method_ret, &rust_param_types, &is_rust_trait)
        .expect("type resolution should succeed (already validated)");

    // Resolve any Rust method symbols used in this function by walking the typed body
    resolve_rust_methods_from_typed_body(ctx, &typed_body);

    // Determine internal return type from the typed AST
    let ret_resolved = typed_body.ret.as_ref()
        .map(|e| e.ty.clone())
        .unwrap_or(ResolvedType::Void);
    let internal_sret = is_internal_sret(&ret_resolved);

    // The LLVM type for the sret value (struct or opaque Vec).
    // Uses resolved_to_inkwell (NOT parse_coerced_type) because internal functions
    // use toylang's own ABI, not Rust's. Per @ACRTFDZ, only Rust-facing declarations
    // need ABI-coerced types.
    let ret_llvm_ty: Option<BasicTypeEnum<'ctx>> = if internal_sret {
        Some(ctx.resolved_to_inkwell(&ret_resolved))
    } else {
        None
    };

    // Build function signature
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let _usize_ty = ctx.usize_type();

    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    let mut param_names: Vec<String> = Vec::new();

    // sret pointer as first param for struct/vec returns
    if internal_sret {
        param_types.push(ptr_ty.into());
        param_names.push("retval".to_string());
    }

    // Real parameters
    for p in &func.params {
        let resolved = crate::toylang::type_resolve::resolve_struct_fields(&p.ty, ctx.registry)
            .expect("param type resolution should succeed (already validated)");
        let ty: BasicMetadataTypeEnum<'ctx> = ctx.resolved_to_inkwell(&resolved).into();
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
    // Per @MBMRVZ, the extern wrapper for `main` is pinned by the Rust
    // shim (`fn main() { __toylang_main(); }`) to a void return. If
    // the internal form's body has a non-void tail, this wrapper will
    // call internal with a missing sret pointer and the internal body
    // will SIGBUS on its final sret store.
    // Resolve the return type for internal ABI decisions
    let ret_resolved = crate::toylang::type_resolve::resolve_return_type(ctx.registry, func)
        .expect("return type resolution should succeed (already validated)");
    let ret_resolved = crate::toylang::type_resolve::resolve_struct_fields(&ret_resolved, ctx.registry)
        .expect("return type struct resolution should succeed (already validated)");
    let internal_sret = is_internal_sret(&ret_resolved);

    // Query Rust ABI for the extern wrapper's return convention
    let coerced = rustc_lang_facade::abi_helpers::coerced_return_type_for_instance(ctx.tcx, instance);

    // Determine LLVM return type for complex types (structs and opaque Rust types)
    let ret_complex_ty: Option<BasicTypeEnum<'ctx>> = match &ret_resolved {
        ResolvedType::Struct { .. } => Some(ctx.resolved_to_inkwell(&ret_resolved)),
        ResolvedType::RustType { .. } => Some(ctx.rust_ty_to_llvm_opaque(&ret_resolved).0),
        _ => None,
    };

    let rust_sret = matches!(coerced, rustc_lang_facade::abi_helpers::CoercedReturn::Indirect);

    // Query Rust ABI for parameter types.
    // Per @TCHAPZ, coerced_params may include a hidden #[track_caller] param,
    // but toylang-defined functions never have it, so the assert holds.
    let coerced_params = rustc_lang_facade::abi_helpers::coerced_param_types_for_instance(ctx.tcx, instance);
    assert_eq!(coerced_params.len(), func.params.len(),
        "fn_abi.args length mismatch for {}", extern_symbol);

    // Resolve internal types for each param (what the internal function expects)
    let internal_param_types: Vec<BasicTypeEnum<'ctx>> = func.params.iter()
        .map(|p| {
            let resolved = crate::toylang::type_resolve::resolve_struct_fields(&p.ty, ctx.registry)
                .expect("param type resolution should succeed (already validated)");
            ctx.resolved_to_inkwell(&resolved)
        })
        .collect();

    // Build extern wrapper signature (matching Rust ABI)
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let _usize_ty = ctx.usize_type();

    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    if rust_sret {
        param_types.push(ptr_ty.into());
    }
    for coerced in &coerced_params {
        match coerced {
            rustc_lang_facade::abi_helpers::CoercedParam::Ignore => {}
            rustc_lang_facade::abi_helpers::CoercedParam::Direct(ty_str) => {
                param_types.push(parse_coerced_type(ctx, ty_str).into());
            }
            rustc_lang_facade::abi_helpers::CoercedParam::Pair(a_str, b_str) => {
                param_types.push(parse_coerced_type(ctx, a_str).into());
                param_types.push(parse_coerced_type(ctx, b_str).into());
            }
            rustc_lang_facade::abi_helpers::CoercedParam::Indirect => {
                param_types.push(ptr_ty.into());
            }
        }
    }

    // Per @ACRTFDZ, the extern wrapper's return type must match the Rust ABI.
    // For structs, use parse_coerced_type; for primitives, resolved_to_inkwell
    // happens to match (both are scalars). For RustType, always sret.
    let rust_ret_type: Option<BasicTypeEnum<'ctx>> = if rust_sret {
        None
    } else {
        match &ret_resolved {
            ResolvedType::Void => None,
            ResolvedType::I32 | ResolvedType::I64 | ResolvedType::F64
            | ResolvedType::Bool | ResolvedType::Usize => {
                Some(ctx.resolved_to_inkwell(&ret_resolved))
            }
            ResolvedType::RustType { .. } => None, // opaque types use sret or indirect
            ResolvedType::Struct { .. } => {
                match &coerced {
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(coerced_str) => {
                        Some(parse_coerced_type(ctx, coerced_str))
                    }
                    _ => ret_complex_ty.map(|t| t.into()),
                }
            }
            _ => None,
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

    // Forward real params, converting from Rust ABI to internal ABI where needed
    let mut rust_llvm_idx = rust_param_offset;
    for (i, coerced) in coerced_params.iter().enumerate() {
        match coerced {
            rustc_lang_facade::abi_helpers::CoercedParam::Ignore => continue,
            rustc_lang_facade::abi_helpers::CoercedParam::Direct(ty_str) => {
                let param = function.get_nth_param(rust_llvm_idx as u32).unwrap();
                rust_llvm_idx += 1;
                let rust_abi_ty = parse_coerced_type(ctx, ty_str);
                let internal_ty = internal_param_types[i];
                if rust_abi_ty == internal_ty {
                    // Same type (primitives) — pass through
                    call_args.push(param.into());
                } else {
                    // Per @ACRTFDZ (same pattern for params), type mismatch
                    // (e.g. Rust passes i64 for { i32, i32 }) — store-through-pointer
                    // reinterpretation: alloca rust type, store, load as internal type
                    let alloca = ctx.builder.build_alloca(rust_abi_ty, "param_coerce").unwrap();
                    ctx.builder.build_store(alloca, param).unwrap();
                    let converted = ctx.builder.build_load(internal_ty, alloca, "converted").unwrap();
                    call_args.push(converted.into());
                }
            }
            rustc_lang_facade::abi_helpers::CoercedParam::Pair(a_str, b_str) => {
                // ScalarPair — Rust passes two separate params (e.g. ptr + len for &[u8]).
                // Reassemble into a struct for the internal function.
                let param_a = function.get_nth_param(rust_llvm_idx as u32).unwrap();
                rust_llvm_idx += 1;
                let param_b = function.get_nth_param(rust_llvm_idx as u32).unwrap();
                rust_llvm_idx += 1;
                let a_ty = parse_coerced_type(ctx, a_str);
                let b_ty = parse_coerced_type(ctx, b_str);
                let struct_ty = ctx.context.struct_type(&[a_ty, b_ty], false);
                // The reassembled struct must match what the internal function expects.
                assert_eq!(BasicTypeEnum::StructType(struct_ty), internal_param_types[i],
                    "Pair reassembly type mismatch for param {} of '{}': \
                     rebuilt {{ {}, {} }} but internal expects {:?}",
                    i, extern_symbol, a_str, b_str, internal_param_types[i]);
                let mut agg = struct_ty.get_undef();
                agg = ctx.builder.build_insert_value(agg, param_a, 0, "pair_a")
                    .unwrap().into_struct_value();
                agg = ctx.builder.build_insert_value(agg, param_b, 1, "pair_b")
                    .unwrap().into_struct_value();
                call_args.push(agg.into());
            }
            rustc_lang_facade::abi_helpers::CoercedParam::Indirect => {
                // Rust passes by pointer — load value for internal (which takes by value)
                let ptr = function.get_nth_param(rust_llvm_idx as u32).unwrap().into_pointer_value();
                rust_llvm_idx += 1;
                let internal_ty = internal_param_types[i];
                let loaded = ctx.builder.build_load(internal_ty, ptr, "deref_param").unwrap();
                call_args.push(loaded.into());
            }
        }
    }

    // Verify we consumed exactly the right number of LLVM params.
    // If this fires, a CoercedParam variant is incrementing rust_llvm_idx wrong.
    let expected_llvm_params = function.count_params() as usize;
    assert_eq!(rust_llvm_idx, expected_llvm_params,
        "extern wrapper '{}': consumed {} LLVM params but function has {}. \
         Check Pair/Direct/Indirect param index accounting.",
        extern_symbol, rust_llvm_idx, expected_llvm_params);

    let call_site = ctx.builder.build_call(internal_fn, &call_args, "wrapper_call").unwrap();

    // Handle return value adaptation
    if ret_resolved == ResolvedType::Void {
        ctx.builder.build_return(None).unwrap();
    } else if rust_sret {
        // Rust uses sret — internal already wrote to the sret pointer
        ctx.builder.build_return(None).unwrap();
    } else if internal_sret {
        // Per @ACRTFDZ, internal used sret but Rust expects a direct return
        // (e.g., {i32,i32} coerced to i64). Load from the sret alloca as the
        // ABI-coerced type so the return matches what the caller expects.
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
            use inkwell::IntPredicate;
            let lhs = lower_typed_expr(ctx, left).into_value(&ctx.builder);
            let rhs = lower_typed_expr(ctx, right).into_value(&ctx.builder);
            let result = match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let val = match op {
                        BinOp::Add => ctx.builder.build_int_add(l, r, "add").unwrap(),
                        BinOp::Sub => ctx.builder.build_int_sub(l, r, "sub").unwrap(),
                        BinOp::Mul => ctx.builder.build_int_mul(l, r, "mul").unwrap(),
                        BinOp::Div => ctx.builder.build_int_signed_div(l, r, "div").unwrap(),
                        BinOp::Eq => ctx.builder.build_int_compare(IntPredicate::EQ, l, r, "eq").unwrap(),
                        BinOp::Ne => ctx.builder.build_int_compare(IntPredicate::NE, l, r, "ne").unwrap(),
                        BinOp::Lt => ctx.builder.build_int_compare(IntPredicate::SLT, l, r, "lt").unwrap(),
                        BinOp::Le => ctx.builder.build_int_compare(IntPredicate::SLE, l, r, "le").unwrap(),
                        BinOp::Gt => ctx.builder.build_int_compare(IntPredicate::SGT, l, r, "gt").unwrap(),
                        BinOp::Ge => ctx.builder.build_int_compare(IntPredicate::SGE, l, r, "ge").unwrap(),
                        BinOp::And => ctx.builder.build_and(l, r, "and").unwrap(),
                        BinOp::Or  => ctx.builder.build_or(l, r, "or").unwrap(),
                    };
                    BasicValueEnum::IntValue(val)
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let val = match op {
                        BinOp::Add => ctx.builder.build_float_add(l, r, "fadd").unwrap(),
                        BinOp::Sub => ctx.builder.build_float_sub(l, r, "fsub").unwrap(),
                        BinOp::Mul => ctx.builder.build_float_mul(l, r, "fmul").unwrap(),
                        BinOp::Div => ctx.builder.build_float_div(l, r, "fdiv").unwrap(),
                        _ => panic!("BinaryOp: float comparisons not yet supported"),
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
                    ResolvedType::Struct { .. } | ResolvedType::RustType { .. } => {
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

        TypedExprKind::FnCall { name, type_args, args } => {
            // Three kinds of FnCall:
            // 1. Extern-declared (body-less `fn foo(x: i32)` in toylang) → linked from Rust source
            // 2. Use-imported (`use std::io::stdout` → `stdout()`) → real Rust function
            // 3. Toylang function (has body) → internal ABI, handled below
            //
            // Cases 1 and 2 need ABI-correct declarations from rustc's coerced_param/return
            // types. Case 3 uses toylang's simple internal ABI.
            let registry_fn = ctx.registry.functions.get(name.as_str());
            let is_extern_decl = registry_fn.map_or(false, |f| f.body.is_none());
            let is_use_import = registry_fn.is_none();

            if is_extern_decl || is_use_import {
                // Extern or use-imported Rust function — must match rustc's ABI exactly.
                // We query coerced_param_types and coerced_return_type to build the LLVM
                // function declaration, then pass args as direct values (not pointers).
                // This differs from MethodCall/StaticCall which use get_or_resolve_rust_method
                // and pass args as pointers (Indirect convention).
                let def_id = if is_extern_decl {
                    crate::oracle::find_extern_fn_def_id(ctx.tcx, name)
                        .unwrap_or_else(|| panic!("extern fn '{}' not found in Rust source", name))
                } else {
                    crate::oracle::find_use_imported_fn_def_id(ctx.tcx, name)
                        .unwrap_or_else(|| panic!("use-imported fn '{}' not found in Rust stubs", name))
                };
                let ty_arg_refs: Vec<ty::GenericArg<'_>> = type_args.iter()
                    .map(|ta| ty::GenericArg::from(crate::oracle::resolved_to_rustc_ty(ctx.tcx, ta)))
                    .collect();
                // @ELASZ
                let args_ref = crate::oracle::build_generic_args_for_item(ctx.tcx, def_id, &ty_arg_refs);
                let instance = ty::Instance::new(def_id, args_ref);
                let symbol = resolve_rust_symbol(ctx.tcx, def_id, args_ref);

                // Query Rust ABI for params and return convention
                let coerced_params = rustc_lang_facade::abi_helpers::coerced_param_types_for_instance(ctx.tcx, instance);
                let coerced_ret = rustc_lang_facade::abi_helpers::coerced_return_type_for_instance(ctx.tcx, instance);
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                let is_sret = matches!(coerced_ret, rustc_lang_facade::abi_helpers::CoercedReturn::Indirect);

                // Build param types from ABI
                let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
                if is_sret {
                    param_types.push(ptr_ty.into()); // sret pointer
                }
                for cp in &coerced_params {
                    match cp {
                        rustc_lang_facade::abi_helpers::CoercedParam::Ignore => {}
                        rustc_lang_facade::abi_helpers::CoercedParam::Direct(ty_str) => {
                            param_types.push(parse_coerced_type(ctx, ty_str).into());
                        }
                        rustc_lang_facade::abi_helpers::CoercedParam::Pair(a_str, b_str) => {
                            param_types.push(parse_coerced_type(ctx, a_str).into());
                            param_types.push(parse_coerced_type(ctx, b_str).into());
                        }
                        rustc_lang_facade::abi_helpers::CoercedParam::Indirect => {
                            param_types.push(ptr_ty.into());
                        }
                    }
                }

                // Per @ACRTFDZ, must use parse_coerced_type (the ABI type), NOT
                // resolved_to_inkwell (the toylang type). LLVM treats aggregate returns
                // (e.g., [8 x i8]) differently from scalar returns (e.g., i64) — using
                // the wrong one causes the return value to be read from the wrong location.
                let ret_type: Option<BasicTypeEnum<'ctx>> = match &coerced_ret {
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(s) => {
                        Some(parse_coerced_type(ctx, s))
                    }
                    rustc_lang_facade::abi_helpers::CoercedReturn::Indirect
                    | rustc_lang_facade::abi_helpers::CoercedReturn::Void => None,
                };

                // For sret, get the opaque type for the sret attribute
                let sret_info = if is_sret {
                    let sig = ctx.tcx.fn_sig(def_id).instantiate(ctx.tcx, args_ref);
                    let sig = ctx.tcx.normalize_erasing_late_bound_regions(
                        ty::TypingEnv::fully_monomorphized(), sig,
                    );
                    let ret_resolved = crate::oracle::rustc_ty_to_resolved_type(ctx.tcx, sig.output());
                    Some(ctx.rust_ty_to_llvm_opaque(&ret_resolved))
                } else {
                    None
                };

                let callee = ctx.declare_external_fn(&symbol, &param_types, ret_type, sret_info);
                ctx.rust_symbols.push(symbol);

                // Build call args via the shared per-CoercedParam dispatcher
                // (Direct/Pair/Indirect/Ignore). FnCall has no receiver, so
                // coerced_params[i] aligns with args[i] naturally — no +1 offset
                // like MethodCall/StaticCall need.
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();

                if is_sret {
                    // Allocate space for the sret return and pass pointer as first arg
                    let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, "fncall_sret");
                    call_args.push(alloca.into());
                }

                let coerced_params_for_args = coerced_params.clone();
                for (i, a) in args.iter().enumerate() {
                    push_arg_for_rust_call(ctx, a, &coerced_params_for_args[i], &mut call_args);
                }
                assert_eq!(call_args.len(), param_types.len(),
                    "FnCall '{}': call_args count ({}) != param_types count ({}). \
                     CoercedParam dispatch in push_arg_for_rust_call should keep these in sync.",
                    name, call_args.len(), param_types.len());

                if is_sret {
                    // sret: the first call arg is the alloca pointer where the callee
                    // writes the return value. The LLVM call returns void.
                    let alloca = call_args[0].into_pointer_value();
                    let opaque_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;
                    ctx.builder.build_call(callee, &call_args, "").unwrap();
                    ExprResult::Ptr(alloca, opaque_ty)
                } else {
                    let result = ctx.builder.build_call(callee, &call_args, "extern_call").unwrap();
                    match result.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(val) => {
                            // Per @ACRTFDZ, the call returns the ABI-coerced type (e.g., i64
                            // for Stdout). If this differs from toylang's type (e.g., [8 x i8]),
                            // store through a pointer to reinterpret the bits.
                            let toylang_ty = ctx.resolved_to_inkwell(&expr.ty);
                            if val.get_type() != toylang_ty {
                                let alloca = ctx.builder.build_alloca(val.get_type(), "abi_ret").unwrap();
                                ctx.builder.build_store(alloca, val).unwrap();
                                ExprResult::Ptr(alloca, toylang_ty)
                            } else {
                                ExprResult::Value(val)
                            }
                        }
                        _ => ExprResult::Value(ctx.context.i32_type().const_zero().into()),
                    }
                }
            } else {

            // Call a toylang function via its internal symbol (simple, predictable ABI).
            let internal_sym = {
                let mut sym = format!("__toylang_internal_{}", name);
                for arg in type_args {
                    sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(arg)));
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

            } // close else (toylang fn path)
        }

        TypedExprKind::StaticCall { ty, method, type_args, args } => {
            let receiver_ty = args.first().map(|a| &a.ty);
            let sym = ctx.get_or_resolve_rust_method(ty, method, type_args, receiver_ty);
            let info = ctx.rust_method_info.get(&sym).unwrap();
            let func = info.func;
            let is_sret = info.is_sret;
            let has_track_caller = info.has_track_caller;
            // Clone so we don't hold a borrow on ctx.rust_method_info while
            // lowering args (which needs &mut ctx).
            let coerced_params = info.coerced_params.clone();

            let is_trait_call = args.first().is_some()
                && crate::oracle::find_use_imported_trait_def_id(ctx.tcx, ty).is_some();
            if is_trait_call {
                // Trait static call: first arg is receiver, rest are regular args.
                // Handle receiver like MethodCall: for Ref types, load the pointer.
                let recv_expr = &args[0];
                let recv = lower_typed_expr(ctx, recv_expr);
                let recv_ptr = match &recv_expr.ty {
                    ResolvedType::Ref { .. } => {
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
                        match recv {
                            ExprResult::Ptr(ptr, _) => ptr,
                            _ => panic!("trait call receiver must be a pointer"),
                        }
                    }
                };

                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();

                if is_sret {
                    let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, &format!("{}_trait", ty));
                    call_args.push(alloca.into());
                    call_args.push(recv_ptr.into());
                    for (i, arg_expr) in args[1..].iter().enumerate() {
                        // coerced_params[0] is self; explicit args start at index 1.
                        push_arg_for_rust_call(ctx, arg_expr, &coerced_params[1 + i], &mut call_args);
                    }
                    if has_track_caller {
                        call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                    }
                    debug_assert_eq!(
                        call_args.len(), func.get_type().count_param_types() as usize,
                        "trait sret call arg count mismatch for {}::{}", ty, method,
                    );
                    ctx.builder.build_call(func, &call_args, "").unwrap();
                    let opaque_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;
                    ExprResult::Ptr(alloca, opaque_ty)
                } else {
                    call_args.push(recv_ptr.into());
                    for (i, arg_expr) in args[1..].iter().enumerate() {
                        push_arg_for_rust_call(ctx, arg_expr, &coerced_params[1 + i], &mut call_args);
                    }
                    if has_track_caller {
                        call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                    }
                    debug_assert_eq!(
                        call_args.len(), func.get_type().count_param_types() as usize,
                        "trait call arg count mismatch for {}::{}", ty, method,
                    );
                    let result = ctx.builder.build_call(func, &call_args, "trait_call").unwrap();
                    match result.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(val) => ExprResult::Value(val),
                        _ => ExprResult::Value(ctx.context.i32_type().const_zero().into()),
                    }
                }
            } else {
                // Per @IVTDBTZ — inherent static call (e.g., Vec::new,
                // Vec::with_capacity(n), Regex::new(pat)). Unlike the trait
                // branch above there's no receiver at args[0], so args and
                // coerced_params align directly (no +1 offset).
                // Previously this branch hardcoded `build_call(func, &[])`
                // which silently discarded every arg, producing garbage in
                // the called fn's params and SIGSEGVing for any non-zero-arg
                // inherent static call. Mirrors the trait branch's arg loop,
                // sret-prepend, track_caller tail, and debug_assert on count.
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
                if is_sret {
                    let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, &format!("{}_new", ty));
                    call_args.push(alloca.into());
                    for (i, arg_expr) in args.iter().enumerate() {
                        push_arg_for_rust_call(ctx, arg_expr, &coerced_params[i], &mut call_args);
                    }
                    if has_track_caller {
                        call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                    }
                    debug_assert_eq!(
                        call_args.len(), func.get_type().count_param_types() as usize,
                        "inherent sret static call arg count mismatch for {}::{}", ty, method,
                    );
                    ctx.builder.build_call(func, &call_args, "").unwrap();
                    let opaque_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;
                    ExprResult::Ptr(alloca, opaque_ty)
                } else {
                    for (i, arg_expr) in args.iter().enumerate() {
                        push_arg_for_rust_call(ctx, arg_expr, &coerced_params[i], &mut call_args);
                    }
                    if has_track_caller {
                        call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                    }
                    debug_assert_eq!(
                        call_args.len(), func.get_type().count_param_types() as usize,
                        "inherent static call arg count mismatch for {}::{}", ty, method,
                    );
                    let result = ctx.builder.build_call(func, &call_args, "static_call").unwrap();
                    match result.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(val) => ExprResult::Value(val),
                        _ => ExprResult::Value(ctx.context.i32_type().const_zero().into()),
                    }
                }
            }
        }

        TypedExprKind::StringLit(s) => {
            // Per @UTAIRZ, string literals codegen mirrors ByteStringLit: emit a
            // global byte array and build the { ptr, i64 } fat pointer that matches
            // `resolved_to_inkwell(Ref { Str })`.
            let bytes = s.as_bytes();
            let array_val = ctx.context.const_string(bytes, false);
            let array_type = array_val.get_type();
            let global = ctx.module.add_global(array_type, None, "str_lit");
            global.set_initializer(&array_val);
            global.set_constant(true);

            // GEP to get pointer to first element
            let ptr = unsafe {
                ctx.builder.build_gep(
                    array_type,
                    global.as_pointer_value(),
                    &[ctx.context.i64_type().const_zero(), ctx.context.i64_type().const_zero()],
                    "str_ptr",
                ).unwrap()
            };

            // Build fat pointer struct { ptr, i64 }.
            // Must match resolved_to_inkwell(Ref { inner: Str }).
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
            let len_ty = ctx.context.i64_type();
            let struct_ty = ctx.context.struct_type(&[ptr_ty.into(), len_ty.into()], false);
            debug_assert_eq!(
                BasicTypeEnum::StructType(struct_ty),
                ctx.resolved_to_inkwell(&expr.ty),
                "StringLit struct type doesn't match resolved_to_inkwell for {:?}", expr.ty,
            );
            let len_val = ctx.context.i64_type().const_int(bytes.len() as u64, false);
            let mut fat_ptr = struct_ty.get_undef();
            fat_ptr = ctx.builder.build_insert_value(fat_ptr, ptr, 0, "fat_ptr_data")
                .unwrap().into_struct_value();
            fat_ptr = ctx.builder.build_insert_value(fat_ptr, len_val, 1, "fat_ptr_len")
                .unwrap().into_struct_value();
            ExprResult::Value(fat_ptr.into())
        }

        TypedExprKind::ByteStringLit(bytes) => {
            // Per @UTAIRZ, byte string literals emit the same { ptr, i64 } fat
            // pointer shape as string literals; this is the template StringLit mirrors.
            let array_val = ctx.context.const_string(bytes, false);
            let array_type = array_val.get_type();
            let global = ctx.module.add_global(array_type, None, "byte_str_lit");
            global.set_initializer(&array_val);
            global.set_constant(true);

            // GEP to get pointer to first element
            let ptr = unsafe {
                ctx.builder.build_gep(
                    array_type,
                    global.as_pointer_value(),
                    &[ctx.context.i64_type().const_zero(), ctx.context.i64_type().const_zero()],
                    "byte_str_ptr",
                ).unwrap()
            };

            // Build fat pointer struct { ptr, i64 }.
            // Must match resolved_to_inkwell(Ref { inner: ByteSlice }).
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
            let len_ty = ctx.context.i64_type();
            let struct_ty = ctx.context.struct_type(&[ptr_ty.into(), len_ty.into()], false);
            debug_assert_eq!(
                BasicTypeEnum::StructType(struct_ty),
                ctx.resolved_to_inkwell(&expr.ty),
                "ByteStringLit struct type doesn't match resolved_to_inkwell for {:?}", expr.ty,
            );
            let len_val = ctx.context.i64_type().const_int(bytes.len() as u64, false);
            let mut fat_ptr = struct_ty.get_undef();
            fat_ptr = ctx.builder.build_insert_value(fat_ptr, ptr, 0, "fat_ptr_data")
                .unwrap().into_struct_value();
            fat_ptr = ctx.builder.build_insert_value(fat_ptr, len_val, 1, "fat_ptr_len")
                .unwrap().into_struct_value();
            ExprResult::Value(fat_ptr.into())
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
                ResolvedType::Struct { .. } | ResolvedType::RustType { .. } => {
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

            // Extract Rust type info from receiver (direct or via &ref)
            let (type_name, type_args) = match &receiver.ty {
                ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                ResolvedType::Ref { inner } => match inner.as_ref() {
                    ResolvedType::RustType { name, type_args } => (name.as_str(), type_args.as_slice()),
                    _ => panic!("method call on unsupported type: {:?}", receiver.ty),
                },
                _ => panic!("method call on unsupported type: {:?}", receiver.ty),
            };

            let sym = ctx.get_or_resolve_rust_method(type_name, method, type_args, None);
            let info = ctx.rust_method_info.get(&sym).unwrap();
            let func = info.func;
            let is_sret = info.is_sret;
            let has_track_caller = info.has_track_caller;
            // Clone so we don't hold a borrow on ctx.rust_method_info while
            // lowering args (which needs &mut ctx).
            let coerced_params = info.coerced_params.clone();
            if is_sret {
                // sret method call (constructor-like returning opaque type)
                let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, "method_sret");
                let opaque_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![alloca.into(), recv_ptr.into()];
                for (i, a) in args.iter().enumerate() {
                    // coerced_params[0] is self; explicit args start at index 1.
                    push_arg_for_rust_call(ctx, a, &coerced_params[1 + i], &mut call_args);
                }
                if has_track_caller { // @TCHAPZ: pass null for hidden Location param
                    call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                }
                debug_assert_eq!(
                    call_args.len(), func.get_type().count_param_types() as usize,
                    "method sret call arg count mismatch for {}::{}", type_name, method,
                );
                ctx.builder.build_call(func, &call_args, "").unwrap();
                ExprResult::Ptr(alloca, opaque_ty)
            } else {
                // Non-sret method call
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![recv_ptr.into()];
                for (i, a) in args.iter().enumerate() {
                    push_arg_for_rust_call(ctx, a, &coerced_params[1 + i], &mut call_args);
                }
                if has_track_caller { // @TCHAPZ: pass null for hidden Location param
                    call_args.push(ctx.context.ptr_type(AddressSpace::default()).const_null().into());
                }
                debug_assert_eq!(
                    call_args.len(), func.get_type().count_param_types() as usize,
                    "method call arg count mismatch for {}::{}", type_name, method,
                );
                let result = ctx.builder.build_call(func, &call_args, "method_call").unwrap();
                match result.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(val) => ExprResult::Value(val),
                    _ => ExprResult::Value(ctx.context.i32_type().const_zero().into()),
                }
            }
        }

        TypedExprKind::If { cond, then_stmts, then_expr, else_stmts, else_expr } => {
            let cond_val = lower_typed_expr(ctx, cond).into_value(&ctx.builder).into_int_value();
            let function = ctx.builder.get_insert_block().unwrap().get_parent().unwrap();

            let then_bb = ctx.context.append_basic_block(function, "then");
            let else_bb = ctx.context.append_basic_block(function, "else");
            let merge_bb = ctx.context.append_basic_block(function, "merge");

            ctx.builder.build_conditional_branch(cond_val, then_bb, if else_stmts.is_empty() && else_expr.is_none() { merge_bb } else { else_bb }).unwrap();

            // Then branch
            ctx.builder.position_at_end(then_bb);
            let saved_vars = ctx.vars.clone();
            for stmt in then_stmts { lower_typed_stmt(ctx, stmt); }
            let then_val = then_expr.as_ref().map(|e| lower_typed_expr(ctx, e).into_value(&ctx.builder));
            let then_end_bb = ctx.builder.get_insert_block().unwrap();
            ctx.builder.build_unconditional_branch(merge_bb).unwrap();
            ctx.vars = saved_vars.clone();

            // Else branch
            ctx.builder.position_at_end(else_bb);
            for stmt in else_stmts { lower_typed_stmt(ctx, stmt); }
            let else_val = else_expr.as_ref().map(|e| lower_typed_expr(ctx, e).into_value(&ctx.builder));
            let else_end_bb = ctx.builder.get_insert_block().unwrap();
            ctx.builder.build_unconditional_branch(merge_bb).unwrap();
            ctx.vars = saved_vars;

            // Merge block
            ctx.builder.position_at_end(merge_bb);

            if let (Some(tv), Some(ev)) = (then_val, else_val) {
                let phi = ctx.builder.build_phi(tv.get_type(), "if_val").unwrap();
                phi.add_incoming(&[(&tv, then_end_bb), (&ev, else_end_bb)]);
                ExprResult::Value(phi.as_basic_value())
            } else {
                ExprResult::Value(ctx.context.i32_type().const_zero().into())
            }
        }

        TypedExprKind::Ref(inner) => {
            // &expr — take a pointer to the inner expression
            let inner_result = lower_typed_expr(ctx, inner);
            let inner_ty = ctx.resolved_to_inkwell(&inner.ty);
            let ptr = inner_result.into_ptr(&ctx.builder, inner_ty, "ref_val");
            ExprResult::Value(ptr.into())
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
        TypedStmt::While { cond, body } => {
            let function = ctx.builder.get_insert_block().unwrap().get_parent().unwrap();
            let header_bb = ctx.context.append_basic_block(function, "while_header");
            let body_bb = ctx.context.append_basic_block(function, "while_body");
            let exit_bb = ctx.context.append_basic_block(function, "while_exit");

            // Snapshot vars before loop (the "original" allocas)
            let pre_loop_vars = ctx.vars.clone();

            // Jump to header
            ctx.builder.build_unconditional_branch(header_bb).unwrap();

            // Header: evaluate condition (uses current ctx.vars — original allocas on first pass)
            ctx.builder.position_at_end(header_bb);
            let cond_val = lower_typed_expr(ctx, cond).into_value(&ctx.builder).into_int_value();
            ctx.builder.build_conditional_branch(cond_val, body_bb, exit_bb).unwrap();

            // Body: lower stmts
            ctx.builder.position_at_end(body_bb);
            for stmt in &body.stmts { lower_typed_stmt(ctx, stmt); }
            if let Some(ref ret) = body.ret {
                let _ = lower_typed_expr(ctx, ret);
            }

            // Store rebound values back into original allocas so the header sees them
            for (name, (orig_ptr, orig_ty)) in &pre_loop_vars {
                if let Some((new_ptr, _)) = ctx.vars.get(name) {
                    if *new_ptr != *orig_ptr {
                        // Variable was rebound — load from new alloca, store into original
                        let val = ctx.builder.build_load(*orig_ty, *new_ptr, "loop_update").unwrap();
                        ctx.builder.build_store(*orig_ptr, val).unwrap();
                    }
                }
            }

            ctx.builder.build_unconditional_branch(header_bb).unwrap();

            // Restore vars to original allocas (header reads from these)
            ctx.vars = pre_loop_vars;

            // Continue after loop
            ctx.builder.position_at_end(exit_bb);
        }
        TypedStmt::Assign { name, expr } => {
            let (ptr, _ty) = ctx.vars.get(name.as_str())
                .expect("assign to undefined var (should be caught by type resolver)")
                .clone();
            let val = lower_typed_expr(ctx, expr).into_value(&ctx.builder);
            ctx.builder.build_store(ptr, val).unwrap();
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
    let field_strs = split_respecting_braces(inner);
    let fields: Vec<BasicTypeEnum<'ctx>> = field_strs.iter()
        .map(|f| parse_coerced_type(ctx, f.trim()))
        .collect();
    ctx.context.struct_type(&fields, false)
}

// ============================================================================
// Helpers (kept from old llvm_gen.rs)
// ============================================================================

// Per @SMINCZ, this function is read-only — it computes a symbol name but
// does NOT drive codegen of `def_id` with `args`. The caller must
// independently register the dep so `per_instance_mir` reifies a
// ReifyFnPointer to it; otherwise the linker fails on a missing symbol.
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


fn parse_coerced_type<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>, s: &str) -> BasicTypeEnum<'ctx> {
    if s == "ptr" {
        return ctx.context.ptr_type(AddressSpace::default()).into();
    }
    if s == "double" {
        return ctx.context.f64_type().into();
    }
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
    state: &mut crate::toylang::callbacks_impl::ToylangState,
) -> (String, Vec<String>) {
    let context = Context::create();
    let mut ctx = CodegenCtx::new(&context, tcx, registry);
    let mut seen_symbols = std::collections::HashSet::new();

    // Collect all toylang mono items (accessors and functions) first, then codegen.
    struct FnItem<'tcx> {
        resolved_func: crate::toylang::registry::ToyFunction,
        /// Some for entry-point functions (Rust calls them), None for internal-only.
        /// Used to generate extern ABI wrappers.
        instance: Option<ty::Instance<'tcx>>,
        extern_symbol: String,
    }
    let mut fn_items: Vec<FnItem<'tcx>> = Vec::new();

    // Walk MonoItems for accessor methods (still discovered via rustc).
    // Regular toylang functions come from state.toylang_instances instead.
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
                // instantiate_identity: structural inspection only — we want the impl's
                // self type with its own params as placeholders so we can read the ADT
                // name and look it up in the registry. Not producing a concrete type.
                let self_ty = tcx.type_of(impl_def_id).instantiate_identity();
                if let ty::TyKind::Adt(adt_def, _) = self_ty.kind() {
                    let struct_name = tcx.item_name(adt_def.did()).to_string();
                    if let Some(toy_struct) = registry.structs.get(&struct_name) {
                        let field_name = tcx.item_name(def_id).to_string();
                        if let Some(field_index) = toy_struct.fields.iter().position(|f| f.name == field_name) {
                            let callback_name = format!("{}.{}", struct_name, field_name);
                            let result = callbacks.monomorphize_fn_inner(state, &callback_name, tcx, local_def_id, instance);
                            if seen_symbols.insert(result.extern_symbol.clone()) {
                                let struct_ty = ctx.struct_type_for_instance(toy_struct, instance);
                                codegen_accessor_inline(&mut ctx, &result.extern_symbol, struct_ty, field_index);
                            }
                        }
                    }
                }
                continue;
            }

            // For regular consumer functions from MonoItems: record the Instance
            // so we can generate an extern wrapper. The resolved_func comes from
            // toylang_instances (populated during the deep walk in monomorphize_fn).
            let name = tcx.item_name(def_id).to_string();
            let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main".to_string() } else { name.clone() };
            if registry.functions.get(&registry_name).is_none() { continue; }
            let extern_symbol = crate::toylang::callbacks_impl::compute_fn_symbol(
                &registry_name, tcx, instance,
            );
            // Find the matching toylang_instance and promote it to have an Instance
            if let Some(inst) = state.toylang_instances.iter().find(|i| i.extern_symbol == extern_symbol) {
                if seen_symbols.insert(extern_symbol.clone()) {
                    fn_items.push(FnItem {
                        resolved_func: inst.resolved_func.clone(),
                        instance: Some(instance),
                        extern_symbol,
                    });
                }
            }
        }
    }

    // Add internal-only toylang instances (discovered during deep walk, not in MonoItems)
    for inst in &state.toylang_instances {
        if seen_symbols.insert(inst.extern_symbol.clone()) {
            fn_items.push(FnItem {
                resolved_func: inst.resolved_func.clone(),
                instance: None,
                extern_symbol: inst.extern_symbol.clone(),
            });
        }
    }

    // Two-pass codegen: internal functions first, then extern wrappers.
    // Internal functions use a simple ABI (structs always sret, primitives direct).
    // Extern wrappers adapt to Rust ABI and delegate to the internal function.
    // Only entry-point functions (with Instance) get extern wrappers.
    for item in &fn_items {
        let internal_symbol = item.extern_symbol.replace("__toylang_impl_", "__toylang_internal_");
        codegen_internal_function(&mut ctx, &item.resolved_func, &internal_symbol);
    }
    for item in &fn_items {
        if let Some(instance) = item.instance {
            let internal_symbol = item.extern_symbol.replace("__toylang_impl_", "__toylang_internal_");
            codegen_extern_wrapper(&mut ctx, &item.resolved_func, instance, &item.extern_symbol, &internal_symbol);
        }
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


