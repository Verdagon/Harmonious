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
use inkwell::values::{BasicValueEnum, PointerValue, FunctionValue, BasicValue, IntValue, CallSiteValue};
use inkwell::AddressSpace;

use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, GenericArg, TyCtxt};

use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::typed_ast::*;
use crate::toylang::registry::{ToylangRegistry, ToyFieldType, ToyStruct};

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
    pointer_size: u64,
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
            pointer_size: dl.pointer_size.bytes(),
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
            ResolvedType::Vec { .. } => self.vec_type().into(),
            ResolvedType::Ref { inner } => {
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
            ResolvedType::Vec { .. } => self.vec_type(),
            _ => panic!("expected struct/vec type, got {:?}", ty),
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
                    "Vec" => self.vec_type().into(),
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

    fn vec_type(&self) -> StructType<'ctx> {
        // Vec is 3 pointers: { iN, iN, iN }
        let ptr_ty = self.usize_type();
        self.context.struct_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false)
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
                        return self.vec_type().into();
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
        is_sret: Option<(StructType<'ctx>, u64)>, // (struct type, align) for sret
    ) -> FunctionValue<'ctx> {
        if let Some(&cached) = self.declared_fns.get(symbol) {
            return cached;
        }

        let fn_type = if let Some(ret) = ret_type {
            ret.fn_type(param_types, false)
        } else {
            self.context.void_type().fn_type(param_types, false)
        };

        let func = self.module.add_function(symbol, fn_type, Some(inkwell::module::Linkage::External));

        if let Some((_sret_ty, _align)) = is_sret {
            // sret attribute on first parameter
            func.add_attribute(
                inkwell::attributes::AttributeLoc::Param(0),
                self.context.create_type_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id("sret"),
                    _sret_ty.into(),
                ),
            );
        }

        self.declared_fns.insert(symbol.to_string(), func);
        func
    }

    // --- Vec operation helpers ---

    fn resolve_vec_symbols(&mut self, elem_ty_name: &str) -> VecSymbols<'ctx> {
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

        let vec_ty = self.vec_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let usize_ty = self.usize_type();

        // Declare Vec::new (sret)
        let new_fn = self.declare_external_fn(
            &new_sym,
            &[ptr_ty.into()],
            None,
            Some((vec_ty, self.pointer_align)),
        );

        // Declare Vec::push
        let push_fn = self.declare_external_fn(
            &push_sym,
            &[ptr_ty.into(), ptr_ty.into()],
            None,
            None,
        );

        // Declare Vec::len
        let len_fn = self.declare_external_fn(
            &len_sym,
            &[ptr_ty.into()],
            Some(usize_ty.into()),
            None,
        );

        VecSymbols { new_fn, push_fn, len_fn, vec_ty }
    }
}

struct VecSymbols<'ctx> {
    new_fn: FunctionValue<'ctx>,
    push_fn: FunctionValue<'ctx>,
    len_fn: FunctionValue<'ctx>,
    vec_ty: StructType<'ctx>,
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

fn lower_expr<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    expr: &Expr,
) -> ExprResult<'ctx> {
    match expr {
        Expr::IntLit(n) => {
            let val = ctx.context.i64_type().const_int(*n as u64, *n < 0);
            ExprResult::Value(val.into())
        }

        Expr::BoolLit(b) => {
            let val = ctx.context.bool_type().const_int(if *b { 1 } else { 0 }, false);
            ExprResult::Value(val.into())
        }

        Expr::Var(name) => {
            let (ptr, ty) = ctx.vars.get(name.as_str())
                .unwrap_or_else(|| panic!("variable '{}' not in scope", name))
                .clone();
            ExprResult::Ptr(ptr, ty)
        }

        Expr::StructLit { name, fields } => {
            lower_struct_lit(ctx, name, fields)
        }

        Expr::StaticCall { ty, method, args } => {
            lower_static_call(ctx, ty, method, args)
        }

        Expr::MethodCall { receiver, method, args } => {
            lower_method_call(ctx, receiver, method, args)
        }
    }
}

fn lower_struct_lit<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    struct_name: &str,
    fields: &[(String, Expr)],
) -> ExprResult<'ctx> {
    let toy_struct = ctx.registry.structs.get(struct_name)
        .unwrap_or_else(|| panic!("struct '{}' not found", struct_name));
    let struct_ty = ctx.struct_type(toy_struct);

    let alloca = ctx.builder.build_alloca(struct_ty, struct_name).unwrap();

    for (field_name, field_expr) in fields {
        let field_idx = toy_struct.fields.iter()
            .position(|f| f.name == *field_name)
            .unwrap_or_else(|| panic!("field '{}' not found in '{}'", field_name, struct_name));

        let field_ty = ctx.resolve_type(&toy_struct.fields[field_idx].rust_type);
        let gep = ctx.builder.build_struct_gep(struct_ty, alloca, field_idx as u32, field_name)
            .unwrap();

        let val = lower_expr(ctx, field_expr);

        // If the field is a large type (struct, Vec), copy memory instead of store
        match &toy_struct.fields[field_idx].rust_type {
            ToyFieldType::ToyStruct(_) | ToyFieldType::RustGeneric(_, _) => {
                let src_ptr = val.into_ptr(&ctx.builder, field_ty, "src");
                // Use size_of from the type — inkwell provides this
                let size_val = field_ty.size_of().unwrap();
                ctx.builder.build_memcpy(
                    gep, 8, src_ptr, 8,
                    size_val,
                ).unwrap();
            }
            _ => {
                // Primitive field — coerce int literal to correct width
                let coerced = coerce_int_to_type(ctx, val.into_value(&ctx.builder), field_ty);
                ctx.builder.build_store(gep, coerced).unwrap();
            }
        }
    }

    ExprResult::Ptr(alloca, struct_ty.into())
}

fn lower_static_call<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    ty_name: &str,
    method: &str,
    _args: &[Expr],
) -> ExprResult<'ctx> {
    match (ty_name, method) {
        ("Vec", "new") => {
            // Find what Vec element type this is for by looking at how the variable is used.
            // For now, discover from the function's context — the Vec symbols are resolved
            // when we first encounter Vec operations in a function.
            // The alloca is created by the let binding; Vec::new stores into it.
            let vec_ty = ctx.vec_type();
            let alloca = ctx.builder.build_alloca(vec_ty, "vec_new").unwrap();

            // We need the Vec symbols — they should have been resolved at function entry.
            // For now, find them from declared functions.
            let new_fn = ctx.declared_fns.iter()
                .find(|(name, _)| name.contains("Vec") && name.contains("new"))
                .map(|(_, f)| *f)
                .expect("Vec::new not declared — call resolve_vec_symbols first");

            ctx.builder.build_call(new_fn, &[alloca.into()], "").unwrap();
            ExprResult::Ptr(alloca, vec_ty.into())
        }
        _ => panic!("unsupported static call: {}::{}", ty_name, method),
    }
}

fn lower_method_call<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    receiver: &Expr,
    method: &str,
    args: &[Expr],
) -> ExprResult<'ctx> {
    let recv = lower_expr(ctx, receiver);

    match method {
        "push" => {
            let recv_ptr = match recv {
                ExprResult::Ptr(ptr, _) => ptr,
                _ => panic!("push receiver must be a pointer"),
            };

            let arg_result = lower_expr(ctx, &args[0]);
            let arg_ty = match &arg_result {
                ExprResult::Value(v) => v.get_type(),
                ExprResult::Ptr(_, ty) => *ty,
            };
            let arg_ptr = arg_result.into_ptr(&ctx.builder, arg_ty, "push_arg");

            let push_fn = ctx.declared_fns.iter()
                .find(|(name, _)| name.contains("push"))
                .map(|(_, f)| *f)
                .expect("Vec::push not declared");

            ctx.builder.build_call(push_fn, &[recv_ptr.into(), arg_ptr.into()], "").unwrap();
            ExprResult::Value(ctx.context.i32_type().const_zero().into()) // void-like
        }

        "len" => {
            let recv_ptr = match recv {
                ExprResult::Ptr(ptr, _) => ptr,
                _ => panic!("len receiver must be a pointer"),
            };

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

        _ => panic!("unsupported method call: .{}", method),
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

fn codegen_function<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    name: &str,
    func: &crate::toylang::registry::ToyFunction,
) {
    let symbol = func.external_symbol.as_ref().unwrap();
    let ret_ty_name = func.return_ty.as_ref().unwrap();

    // Run type resolution pass to get typed AST
    let typed_body = crate::toylang::type_resolve::resolve_fn_body(ctx.registry, func);
    let body = func.body.as_ref().unwrap(); // still needed for body_uses_vec

    // Determine return ABI
    let fn_def_id = find_fn_def_id(ctx.tcx, name)
        .unwrap_or_else(|| panic!("function '{}' not found in HIR", name));
    let coerced = rustc_lang_facade::abi_helpers::coerced_return_type(ctx.tcx, fn_def_id);

    // Determine LLVM return type
    let ret_struct_ty = if ret_ty_name.starts_with("Vec<") {
        Some(ctx.vec_type())
    } else {
        ctx.struct_type_for_ret(ret_ty_name)
    };

    let is_sret = matches!(coerced, rustc_lang_facade::abi_helpers::CoercedReturn::Indirect);

    // Resolve Vec symbols if needed
    let uses_vec = body_uses_vec(body);
    if uses_vec {
        let elem_name = find_vec_elem_name(ret_ty_name, ctx.registry)
            .or_else(|| find_vec_elem_from_params(func));
        if let Some(elem) = elem_name {
            ctx.resolve_vec_symbols(&elem);
        }
    }

    // Build function signature
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let usize_ty = ctx.usize_type();

    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    let mut param_names: Vec<String> = Vec::new();

    // sret pointer as first param
    if is_sret {
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

    // Determine LLVM return type
    let llvm_ret_type: Option<BasicTypeEnum<'ctx>> = if is_sret {
        None // void return
    } else if ret_ty_name.as_str() == "usize" || ret_ty_name.as_str() == "i32" || ret_ty_name.as_str() == "i64" {
        Some(match ret_ty_name.as_str() {
            "usize" => usize_ty.into(),
            "i32" => ctx.context.i32_type().into(),
            "i64" => ctx.context.i64_type().into(),
            _ => unreachable!(),
        })
    } else if ret_ty_name.starts_with("Vec<") {
        None // Vec returns via sret always
    } else {
        // Struct return — may be coerced
        match &coerced {
            rustc_lang_facade::abi_helpers::CoercedReturn::Direct(coerced_str) => {
                Some(parse_coerced_type(ctx, coerced_str))
            }
            _ => ret_struct_ty.map(|t| t.into()),
        }
    };

    let fn_type = match llvm_ret_type {
        Some(ret) => ret.fn_type(&param_types, false),
        None => ctx.context.void_type().fn_type(&param_types, false),
    };

    let function = ctx.module.add_function(symbol, fn_type, None);

    // Add sret attribute if needed
    if is_sret {
        if let Some(sty) = ret_struct_ty {
            function.add_attribute(
                inkwell::attributes::AttributeLoc::Param(0),
                ctx.context.create_type_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id("sret"),
                    sty.into(),
                ),
            );
        }
    }

    let entry = ctx.context.append_basic_block(function, "entry");
    ctx.builder.position_at_end(entry);

    // Bind parameters to variables
    ctx.vars.clear();
    let param_offset = if is_sret { 1 } else { 0 };
    for (i, p) in func.params.iter().enumerate() {
        let param_val = function.get_nth_param((i + param_offset) as u32).unwrap();
        let param_ty = param_val.get_type();
        let alloca = ctx.builder.build_alloca(param_ty, &p.name).unwrap();
        ctx.builder.build_store(alloca, param_val).unwrap();
        ctx.vars.insert(p.name.clone(), (alloca, param_ty));
    }

    // Lower body statements (using typed AST)
    for stmt in &typed_body.stmts {
        lower_typed_stmt(ctx, stmt);
    }

    // Lower return expression (using typed AST)
    if let Some(ref ret_expr) = typed_body.ret {
        let result = lower_typed_expr(ctx, ret_expr);

        if is_sret {
            // Store into sret pointer
            let sret_ptr = function.get_nth_param(0).unwrap().into_pointer_value();
            let result_ty = ret_struct_ty.unwrap().into();
            let src_ptr = result.into_ptr(&ctx.builder, result_ty, "ret_src");
            let size = ret_struct_ty.unwrap().size_of().unwrap();
            ctx.builder.build_memcpy(
                sret_ptr, ctx.pointer_align as u32,
                src_ptr, ctx.pointer_align as u32,
                size,
            ).unwrap();
            ctx.builder.build_return(None).unwrap();
        } else if ret_ty_name.starts_with("Vec<") {
            // Vec return — also sret (always)
            let sret_ptr = function.get_nth_param(0).unwrap().into_pointer_value();
            let vec_ty: BasicTypeEnum = ctx.vec_type().into();
            let src_ptr = result.into_ptr(&ctx.builder, vec_ty, "ret_src");
            let vec_size = ctx.usize_type()
                .const_int(ctx.pointer_size * 3, false);
            ctx.builder.build_memcpy(
                sret_ptr, ctx.pointer_align as u32,
                src_ptr, ctx.pointer_align as u32,
                vec_size,
            ).unwrap();
            ctx.builder.build_return(None).unwrap();
        } else if llvm_ret_type.is_some() {
            if let Some(sty) = ret_struct_ty {
                // Struct return — load from memory as the coerced type
                let src_ptr = result.into_ptr(&ctx.builder, sty.into(), "ret_coerce");
                let coerced_val = ctx.builder.build_load(
                    llvm_ret_type.unwrap(), src_ptr, "coerced_ret",
                ).unwrap();
                ctx.builder.build_return(Some(&coerced_val)).unwrap();
            } else {
                // Primitive return
                let val = result.into_value(&ctx.builder);
                let coerced = coerce_int_to_type(ctx, val, llvm_ret_type.unwrap());
                ctx.builder.build_return(Some(&coerced)).unwrap();
            }
        } else {
            ctx.builder.build_return(None).unwrap();
        }
    } else {
        ctx.builder.build_return(None).unwrap();
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

        TypedExprKind::StaticCall { ty, method, args } => {
            match (ty.as_str(), method.as_str()) {
                ("Vec", "new") => {
                    let vec_ty = ctx.vec_type();
                    let alloca = ctx.builder.build_alloca(vec_ty, "vec_new").unwrap();

                    let new_fn = ctx.declared_fns.iter()
                        .find(|(name, _)| name.contains("new"))
                        .map(|(_, f)| *f)
                        .expect("Vec::new not declared");

                    ctx.builder.build_call(new_fn, &[alloca.into()], "").unwrap();
                    ExprResult::Ptr(alloca, vec_ty.into())
                }
                _ => panic!("unsupported static call: {}::{}", ty, method),
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

fn lower_stmt<'ctx>(ctx: &mut CodegenCtx<'ctx, '_, '_>,
 stmt: &Stmt) {
    match stmt {
        Stmt::Let { name, expr } => {
            let result = lower_expr(ctx, expr);
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
        Stmt::ExprStmt(expr) => {
            let _ = lower_expr(ctx, expr);
        }
    }
}

// ============================================================================
// Accessor codegen
// ============================================================================

fn codegen_accessors<'ctx>(
    ctx: &mut CodegenCtx<'ctx, '_, '_>,

    discovered_accessors: &[crate::toylang::callbacks_impl::DiscoveredAccessor],
) {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    // Non-generic struct accessors from registry
    let mut done_syms = std::collections::HashSet::new();
    for (name, toy_struct) in &ctx.registry.structs {
        if toy_struct.type_params.is_empty() {
            let struct_ty = ctx.struct_type(toy_struct);
            for (i, field) in toy_struct.fields.iter().enumerate() {
                let sym = format!("__toylang_accessor_{}_{}", name, field.name);
                if done_syms.insert(sym.clone()) {
                    let fn_type = ptr_ty.fn_type(&[ptr_ty.into()], false);
                    let func = ctx.module.add_function(&sym, fn_type, None);
                    let entry = ctx.context.append_basic_block(func, "entry");
                    ctx.builder.position_at_end(entry);
                    let self_ptr = func.get_nth_param(0).unwrap().into_pointer_value();
                    let gep = ctx.builder.build_struct_gep(struct_ty, self_ptr, i as u32, "ptr")
                        .unwrap();
                    ctx.builder.build_return(Some(&gep)).unwrap();
                }
            }
        }
    }

    // Discovered generic accessor instances
    for acc in discovered_accessors {
        if done_syms.insert(acc.extern_symbol.clone()) {
            // Parse the LLVM struct type string to reconstruct the inkwell type
            // For now, look up the struct from registry and resolve with type args
            let toy_struct = ctx.registry.structs.get(&acc.struct_name)
                .unwrap_or_else(|| panic!("struct '{}' not found for accessor", acc.struct_name));

            // Use the pre-computed llvm_struct_ty to determine the struct layout.
            // Since we have the field index, just build the GEP accessor.
            // We need the inkwell StructType. Let's reconstruct it from the string.
            // This is a hack — ideally discovered_accessors would carry the types directly.
            let struct_ty = parse_struct_type_str(ctx, &acc.llvm_struct_ty);

            let fn_type = ptr_ty.fn_type(&[ptr_ty.into()], false);
            let func = ctx.module.add_function(&acc.extern_symbol, fn_type, None);
            let entry = ctx.context.append_basic_block(func, "entry");
            ctx.builder.position_at_end(entry);
            let self_ptr = func.get_nth_param(0).unwrap().into_pointer_value();
            let gep = ctx.builder.build_struct_gep(struct_ty, self_ptr, acc.field_index as u32, "ptr")
                .unwrap();
            ctx.builder.build_return(Some(&gep)).unwrap();
        }
    }
}

/// Parse an LLVM struct type string like "{ i32, i64 }" into an inkwell StructType.
fn parse_struct_type_str<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>, s: &str) -> StructType<'ctx> {
    let inner = s.trim().trim_start_matches('{').trim_end_matches('}').trim();
    let fields: Vec<BasicTypeEnum<'ctx>> = inner.split(',')
        .map(|f| {
            let f = f.trim();
            match f {
                "i32" => ctx.context.i32_type().into(),
                "i64" => ctx.context.i64_type().into(),
                "double" => ctx.context.f64_type().into(),
                "i1" => ctx.context.bool_type().into(),
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

fn find_fn_def_id(tcx: TyCtxt<'_>, name: &str) -> Option<LocalDefId> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        if matches!(tcx.def_kind(local_def_id), DefKind::Fn) {
            if tcx.item_name(local_def_id.to_def_id()).as_str() == name {
                return Some(local_def_id);
            }
        }
    }
    None
}

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

fn parse_generic_type(ty: &str) -> Option<(&str, Vec<&str>)> {
    let open = ty.find('<')?;
    if !ty.ends_with('>') { return None; }
    let base = &ty[..open];
    let args_str = &ty[open+1..ty.len()-1];
    let args: Vec<&str> = args_str.split(',').map(|s| s.trim()).collect();
    Some((base, args))
}

/// Find the Vec element type name from context. Searches recursively into nested structs.
fn find_vec_elem_name(ret_ty_name: &str, registry: &ToylangRegistry) -> Option<String> {
    // Direct Vec return: "Vec<Point>" → "Point"
    if ret_ty_name.starts_with("Vec<") && ret_ty_name.ends_with('>') {
        return Some(ret_ty_name[4..ret_ty_name.len()-1].to_string());
    }
    // Struct with Vec field (recursive)
    if let Some(s) = registry.structs.get(ret_ty_name) {
        return find_vec_in_struct_fields(s, registry);
    }
    None
}

/// Recursively search struct fields for a Vec type, returning its element type name.
fn find_vec_in_struct_fields(s: &ToyStruct, registry: &ToylangRegistry) -> Option<String> {
    for field in &s.fields {
        match &field.rust_type {
            ToyFieldType::RustGeneric(name, args) if name == "Vec" && !args.is_empty() => {
                return Some(field_type_to_string(&args[0]));
            }
            ToyFieldType::ToyStruct(struct_name) => {
                if let Some(inner) = registry.structs.get(struct_name.as_str()) {
                    if let Some(elem) = find_vec_in_struct_fields(inner, registry) {
                        return Some(elem);
                    }
                }
            }
            _ => {}
        }
    }
    None
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

fn field_type_to_string(ft: &ToyFieldType) -> String {
    match ft {
        ToyFieldType::I32 => "i32".to_string(),
        ToyFieldType::I64 => "i64".to_string(),
        ToyFieldType::F64 => "f64".to_string(),
        ToyFieldType::Bool => "bool".to_string(),
        ToyFieldType::TypeParam(s) => s.clone(),
        ToyFieldType::ToyStruct(s) => s.clone(),
        ToyFieldType::RustGeneric(name, args) => {
            let arg_strs: Vec<String> = args.iter().map(field_type_to_string).collect();
            format!("{}<{}>", name, arg_strs.join(", "))
        }
    }
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
fn is_eligible(
    _fn_name: &str,
    func: &crate::toylang::registry::ToyFunction,
    registry: &ToylangRegistry,
) -> bool {
    if func.body.is_none() {
        return false;
    }
    let ret_ty = match &func.return_ty {
        Some(t) => t,
        None => return false,
    };
    match ret_ty.as_str() {
        "usize" | "i32" | "i64" | "f64" | "bool" => return true,
        _ => {}
    }
    if registry.structs.contains_key(ret_ty.as_str()) {
        return true;
    }
    if ret_ty.starts_with("Vec<") && ret_ty.ends_with('>') {
        return true;
    }
    if let Some((base, _)) = parse_generic_type(ret_ty) {
        return registry.structs.contains_key(base);
    }
    false
}

/// Mark functions that were compiled by the LLVM backend by setting their external_symbol.
pub fn mark_compiled_functions(registry: &mut ToylangRegistry) {
    let names: Vec<String> = registry.functions.keys().cloned().collect();
    for name in names {
        let func = registry.functions.get(&name).unwrap();
        if is_eligible(&name, func, registry) {
            let symbol = format!("__toylang_impl_{}", name);
            registry.functions.get_mut(&name).unwrap().external_symbol = Some(symbol);
        }
    }
}

/// Generate LLVM IR as a string (for writing to .ll file) and return Rust symbols to globalize.
pub fn generate_with_tcx<'tcx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    discovered_accessors: &[crate::toylang::callbacks_impl::DiscoveredAccessor],
) -> (String, Vec<String>) {
    let context = Context::create();
    let mut ctx = CodegenCtx::new(&context, tcx, registry);

    // Codegen each eligible function
    for (name, func) in &registry.functions {
        if func.external_symbol.is_none() {
            continue;
        }
        codegen_function(&mut ctx, name, func);
    }

    // Codegen accessor functions
    codegen_accessors(&mut ctx, discovered_accessors);

    // Extract results
    let rust_symbols = ctx.rust_symbols.clone();
    let ir = ctx.module.print_to_string().to_string();

    (ir, rust_symbols)
}

// --- Public type helpers used by callbacks_impl.rs ---

pub fn llvm_struct_type_full_pub(s: &ToyStruct, registry: &ToylangRegistry, pointer_bits: u64) -> String {
    let fields: Vec<String> = s.fields.iter()
        .map(|f| field_type_to_llvm_str(&f.rust_type, registry, pointer_bits))
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

pub fn rust_ty_to_llvm_str<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: ty::Ty<'tcx>,
    registry: &ToylangRegistry,
    pointer_bits: u64,
) -> String {
    match ty.kind() {
        ty::TyKind::Int(ty::IntTy::I32) => "i32".to_string(),
        ty::TyKind::Int(ty::IntTy::I64) => "i64".to_string(),
        ty::TyKind::Float(ty::FloatTy::F64) => "double".to_string(),
        ty::TyKind::Bool => "i1".to_string(),
        ty::TyKind::Adt(adt_def, args) => {
            let name = tcx.item_name(adt_def.did()).to_string();
            if let Some(toy_struct) = registry.structs.get(name.as_str()) {
                if toy_struct.type_params.is_empty() {
                    llvm_struct_type_full_pub(toy_struct, registry, pointer_bits)
                } else {
                    let mut subst = HashMap::new();
                    for (i, param_name) in toy_struct.type_params.iter().enumerate() {
                        let inner_ty = args[i].expect_ty();
                        subst.insert(param_name.clone(), rust_ty_to_llvm_str(tcx, inner_ty, registry, pointer_bits));
                    }
                    let fields: Vec<String> = toy_struct.fields.iter()
                        .map(|f| resolve_field_type_with_subst(&f.rust_type, registry, pointer_bits, &subst))
                        .collect();
                    format!("{{ {} }}", fields.join(", "))
                }
            } else {
                let ptr_ty = format!("i{}", pointer_bits);
                format!("{{ {0}, {0}, {0} }}", ptr_ty)
            }
        }
        _ => panic!("rust_ty_to_llvm_str: unsupported type {:?}", ty),
    }
}

pub fn resolve_field_type_with_subst(
    ft: &ToyFieldType,
    registry: &ToylangRegistry,
    pointer_bits: u64,
    subst: &HashMap<String, String>,
) -> String {
    match ft {
        ToyFieldType::TypeParam(name) => {
            subst.get(name).unwrap_or_else(|| panic!("TypeParam '{}' not in subst", name)).clone()
        }
        _ => field_type_to_llvm_str(ft, registry, pointer_bits),
    }
}

fn field_type_to_llvm_str(ft: &ToyFieldType, registry: &ToylangRegistry, pointer_bits: u64) -> String {
    match ft {
        ToyFieldType::I32 => "i32".to_string(),
        ToyFieldType::I64 => "i64".to_string(),
        ToyFieldType::F64 => "double".to_string(),
        ToyFieldType::Bool => "i1".to_string(),
        ToyFieldType::TypeParam(_) => panic!("TypeParam not resolved"),
        ToyFieldType::ToyStruct(name) => {
            let s = registry.structs.get(name.as_str()).unwrap();
            llvm_struct_type_full_pub(s, registry, pointer_bits)
        }
        ToyFieldType::RustGeneric(type_name, _) => {
            match type_name.as_str() {
                "Vec" => {
                    let ptr_ty = format!("i{}", pointer_bits);
                    format!("{{ {0}, {0}, {0} }}", ptr_ty)
                }
                other => panic!("unsupported Rust generic '{}'", other),
            }
        }
    }
}
