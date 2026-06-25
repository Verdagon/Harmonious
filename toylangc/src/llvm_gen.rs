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
use crate::toylang::registry::ToylangRegistry;


/// Convert BasicTypeEnum to AnyTypeEnum (inkwell doesn't impl From directly).
/// Path B: build the Sky-internal symbol for a function instance.
///
/// Sky's bitcode emits each consumer function in two pieces: an extern
/// wrapper under the rustc-mangled name (for rustc-visible items) and an
/// internal body under a Sky-chosen name. This helper computes the Sky-
/// internal name: `__toylang_internal_<registry-name>__<type-arg-mangles>`.
fn internal_symbol_for_instance<'tcx>(
    registry_name: &str,
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> String {
    let mut sym = format!("__toylang_internal_{}", registry_name);
    for arg in instance.args.iter() {
        if let ty::GenericArgKind::Type(ty) = arg.kind() {
            let resolved = crate::oracle::rustc_ty_to_resolved_type(tcx, ty);
            sym.push_str(&format!("__{}", crate::oracle::resolved_type_to_mangled_name(&resolved)));
        }
    }
    sym
}

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
    // Approach B (patch 4 rev 2): the module is owned by rustc; the caller
    // (consumer_fill_modules's closure) wraps the borrowed LLVMModuleRef in
    // a suppressed-Drop Inkwell handle and lends us this borrow. Every
    // Inkwell `Module` method we use takes `&self`, so a borrow is enough.
    module: &'ctx Module<'ctx>,
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
    /// Approach B (patch 4 rev 2): the caller supplies a borrowed
    /// `&'ctx Context` and a borrowed `&'ctx Module<'ctx>` wrapping rustc's
    /// LLVMContext + LLVMModule. We DO NOT call `module.set_data_layout` /
    /// `module.set_triple` here — rustc's `ModuleLlvm::new` already set both
    /// from `tcx.sess.target` via `create_target_machine`. Double-setting
    /// would be at best redundant, at worst inconsistent if rustc's
    /// later-pipeline TargetMachine drifts from what Sky configured.
    fn new(
        context: &'ctx Context,
        module: &'ctx Module<'ctx>,
        tcx: TyCtxt<'tcx>,
        registry: &'reg ToylangRegistry,
    ) -> Self {
        let dl = &tcx.data_layout;
        CodegenCtx {
            context,
            module,
            builder: context.create_builder(),
            tcx,
            registry,
            pointer_bits: dl.pointer_size().bits(),
            pointer_align: dl.pointer_align().abi.bytes(),
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
            // Void has no `BasicTypeEnum` representation — callers must
            // branch on `Void` themselves and produce `None` / `void_type()`
            // on the LLVM side before reaching here. The former silent
            // fallback to `i8` silently materialized as `declare i8
            // @fn() { ret void }` when a forward-declaration site forgot
            // to guard Void (B7 bool bug, fixed at the two `internal_sret
            // || == Void` call sites in this file). Panicking here turns
            // future equivalent bugs into a loud crash rather than an
            // LLVM IR verifier error 200 stack frames deep.
            ResolvedType::Void => panic!(
                "resolved_to_inkwell called on Void — the caller must guard \
                 against void types before mapping to a BasicTypeEnum"
            ),
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
            ResolvedType::StructRef { name, type_args } => {
                // Session 9 sharpening — when the call-site lowering surfaces
                // a `StructRef` (e.g., the return type of a Rust generic
                // intermediary like `duplicate<Widget>(&w)` where the oracle
                // produced `StructRef "Widget"` rather than the fully-flattened
                // `Struct`), resolve it lazily via the registry instead of
                // bailing. The registry has the struct definition by name;
                // we only need to mirror `resolve_struct_fields`' work for
                // this one type.
                //
                // The eager `resolve_struct_fields` calls at function
                // boundaries (params + return type) still fire — this fallback
                // catches transient StructRefs in expression positions that
                // the earlier pass didn't reach.
                let resolved = crate::toylang::type_resolve::resolve_struct_fields(
                    &ResolvedType::StructRef { name: name.clone(), type_args: type_args.clone() },
                    self.registry,
                ).unwrap_or_else(|e| {
                    panic!("failed to resolve StructRef '{}' lazily in codegen: {:?}", name, e)
                });
                if matches!(&resolved, ResolvedType::StructRef { .. }) {
                    // Registry didn't have the struct — that's the original
                    // "should be resolved before codegen" condition, surface
                    // the panic so the diagnostic remains useful.
                    panic!("StructRef '{}' could not be resolved by registry", name);
                }
                self.resolved_to_inkwell(&resolved)
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
            ResolvedType::RustType { .. } => panic!("Vec is opaque — use rust_ty_to_llvm_opaque, not struct GEP"),
            _ => panic!("expected struct type, got {:?}", ty),
        }
    }

    // --- Type resolution ---

    // Tier 3 #3 Phase 1c: `struct_type` + `struct_type_for_instance`
    // retired alongside `codegen_accessor_inline`. The standard
    // `resolved_to_inkwell` lowering handles consumer struct types
    // directly (via the StructRef / Struct arms); per-Instance generic
    // substitution happens in `resolve_caller_from_instance` before
    // codegen sees the body. The dedicated accessor helpers were
    // specific to the old GEP-based emission and have no analog in the
    // unified pipeline.

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

    // `emit_drop_in_place` + `emit_sky_struct_drop` + `emit_scope_drops`
    // retired 2026-06-23 (Phase E.d). Drop calls now appear as
    // synthesized `Drop::drop(&local)` StaticCall AST nodes that the
    // existing `lower_typed_expr::StaticCall` arm emits uniformly —
    // no drop-specific emission helpers. See callbacks_impl.rs's
    // `insert_scope_end_drops` for the synthesis pass.

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
        CoercedParam::Pair(a_str, b_str) => {
            // Fat pointer / two-scalar aggregate — extract and pass separately.
            // Source of truth is rustc's coerced_param shape.
            //
            // Phase R Site #1: the original implementation assumed the
            // source value was always a struct-typed loaded value (so
            // `into_struct_value()` would succeed). That holds for
            // `&str` / `&[u8]` (Sky resolves both as `{ ptr, i64 }`
            // struct) — but NOT for sources whose toylang type
            // resolves to a non-struct LLVM type while rustc's ABI
            // happens to coerce the arg to `Pair`. The likely path is
            // narrow under today's toylang grammar (thin refs are
            // Direct, not Pair), but a thin pointer typed at the
            // toylang side as `Ref { inner: Struct }` would surface
            // here as a `PointerType` value → `into_struct_value()`
            // would panic.
            //
            // Defensive fix: if the source value's LLVM type doesn't
            // match the expected Pair struct shape, reinterpret via
            // memory (same @ACRTFDZ pattern Direct's aggregate→scalar
            // case uses). Build the target `{ a, b }` struct type from
            // the coerced strings, alloca, store source bits, load as
            // the Pair struct, then extract.
            let arg_result = lower_typed_expr(ctx, arg_expr);
            let arg_toylang_ty = ctx.resolved_to_inkwell(&arg_expr.ty);
            let a_ty = parse_coerced_type(ctx, a_str);
            let b_ty = parse_coerced_type(ctx, b_str);
            let pair_struct_ty = ctx.context.struct_type(&[a_ty, b_ty], false);
            let pair_basic: BasicTypeEnum = pair_struct_ty.into();

            let struct_val = if arg_toylang_ty == pair_basic {
                arg_result.into_value(&ctx.builder).into_struct_value()
            } else {
                // Memory reinterpret. Use the LARGER of the two LLVM
                // types as the alloca to accommodate both writes.
                let pointer_bytes = ctx.pointer_align;
                let src_size = static_size_bytes(arg_toylang_ty, pointer_bytes);
                let pair_size = static_size_bytes(pair_basic, pointer_bytes);
                let alloca_ty = if pair_size >= src_size { pair_basic } else { arg_toylang_ty };
                let buf = ctx.builder.build_alloca(alloca_ty, "pair_coerce_buf").unwrap();
                // Store source value into the buffer at its native type.
                let src_ptr = arg_result.into_ptr(&ctx.builder, arg_toylang_ty, "pair_src");
                let copy_size = src_size.min(pair_size);
                let copy_size_val = ctx.context.i64_type().const_int(copy_size, false);
                ctx.builder.build_memcpy(
                    buf, ctx.pointer_align as u32,
                    src_ptr, ctx.pointer_align as u32,
                    copy_size_val,
                ).unwrap();
                ctx.builder
                    .build_load(pair_basic, buf, "pair_coerce_load")
                    .unwrap()
                    .into_struct_value()
            };
            let first = ctx.builder.build_extract_value(struct_val, 0, "pair_first").unwrap();
            let second = ctx.builder.build_extract_value(struct_val, 1, "pair_second").unwrap();
            call_args.push(first.into());
            call_args.push(second.into());
        }
        CoercedParam::Direct(llvm_ty_str) => {
            // Pass by value, coerced to the LLVM type rustc declared. Three sub-cases:
            //   (a) Source toylang type matches target_ty directly (e.g. i32 → i32,
            //       &T → ptr): pass the loaded value possibly width-adjusted.
            //   (b) Source is an aggregate (struct) in memory, target_ty is a
            //       scalar (e.g. Widget = { i32 } coerced to i32 by rustc's ABI):
            //       reinterpret via the source allocation's memory — load as
            //       target_ty directly. This mirrors @ACRTFDZ's pattern used for
            //       incoming params in codegen_extern_wrapper. Without this, the
            //       call instruction's arg type ({ i32 }) doesn't match the
            //       declared param type (i32) and LLVM's BitcodeWriter produces
            //       malformed bitcode that fails to round-trip at opt-level≥1
            //       (B10's residual trigger).
            //   (c) Source is a scalar but width-differs from target (e.g. i64
            //       literal → i32 param): trunc/sext via coerce_int_to_type.
            let target_ty = parse_coerced_type(ctx, llvm_ty_str);
            let arg_toylang_ty = ctx.resolved_to_inkwell(&arg_expr.ty);

            if arg_toylang_ty != target_ty && matches!(arg_toylang_ty, BasicTypeEnum::StructType(_)) {
                // (b) Aggregate → scalar coercion via memory reinterpretation.
                let arg_result = lower_typed_expr(ctx, arg_expr);
                let arg_ptr = arg_result.into_ptr(&ctx.builder, arg_toylang_ty, "abi_coerce_src");
                let coerced = ctx.builder
                    .build_load(target_ty, arg_ptr, "abi_coerce_load")
                    .unwrap();
                call_args.push(coerced.into());
            } else {
                // (a) and (c): scalar passthrough or width adjustment.
                let val = lower_typed_expr(ctx, arg_expr).into_value(&ctx.builder);
                let coerced = coerce_int_to_type(ctx, val, target_ty);
                call_args.push(coerced.into());
            }
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

/// Compute the LLVM-level byte size of a BasicTypeEnum at codegen time.
/// Used by codegen_extern_wrapper's sret-bridge to size the staging alloca
/// (`max(internal_ty_size, rust_ret_type_size)`) — see Phase R Site #8.
///
/// LLVM's `type.size_of()` returns a const-expression IntValue that we
/// can't directly extract as a u64 (it's `LLVMSizeOf(ty)`, a constant
/// expression, not a literal constant). This helper computes the same
/// thing at the Rust level by walking the type structure.
///
/// Alignment-padding follows the heuristic `field_align = min(field_size,
/// pointer_bytes)`. This matches LLVM's actual rules EXACTLY for every
/// type Sky's `resolved_to_inkwell` produces today:
///   - bool/i1 (size 1, align 1) ✓
///   - i32 (size 4, align 4) ✓
///   - i64 / usize / ptr (size 8, align 8) ✓
///   - f64 (size 8, align 8) ✓
///   - struct of the above (align = max-field-align) ✓
///
/// Known divergence from LLVM for types Sky doesn't emit today:
///   - i128 fields (LLVM aligns to 16; heuristic clamps to pointer_bytes=8)
///   - Array fields (LLVM uses element-alignment; heuristic uses
///     min(total_size, pointer_bytes))
///   - Vector types (panic — unsupported)
///
/// When toylang grammar grows i128, fixed-size byte arrays, or SIMD,
/// audit this helper against `TargetData::abi_alignment_of_type` (via
/// inkwell) or migrate to using LLVM's data layout directly. Caller
/// supplies `pointer_bytes` (typically 8 on aarch64; comes from
/// `tcx.data_layout.pointer_size().bytes()`).
fn static_size_bytes(ty: BasicTypeEnum<'_>, pointer_bytes: u64) -> u64 {
    fn align_up(n: u64, align: u64) -> u64 {
        if align <= 1 { n } else { (n + align - 1) & !(align - 1) }
    }
    match ty {
        BasicTypeEnum::IntType(it) => {
            let bits = it.get_bit_width() as u64;
            (bits + 7) / 8
        }
        BasicTypeEnum::FloatType(_) => {
            // inkwell's FloatType doesn't expose bit_width; rely on
            // printed-name match. f32=4, f64=8, half=2, x86_fp80=10,
            // fp128/ppc_fp128=16. Sky never emits float types outside
            // f64 today, but cover the standard set defensively.
            let name = ty.print_to_string().to_str().unwrap_or("").to_string();
            match name.as_str() {
                "half" => 2,
                "float" => 4,
                "double" => 8,
                "x86_fp80" => 10,
                "fp128" | "ppc_fp128" => 16,
                _ => panic!("static_size_bytes: unrecognized float type '{}'", name),
            }
        }
        BasicTypeEnum::PointerType(_) => pointer_bytes,
        BasicTypeEnum::ArrayType(at) => {
            let elem = at.get_element_type();
            let count = at.len() as u64;
            static_size_bytes(elem, pointer_bytes) * count
        }
        BasicTypeEnum::StructType(st) => {
            // Mirror LLVM's natural struct layout: each field at its own
            // alignment, struct rounds up to max-field alignment.
            let mut offset = 0u64;
            let mut max_align = 1u64;
            for i in 0..st.count_fields() {
                let field = st.get_field_type_at_index(i)
                    .unwrap_or_else(|| panic!(
                        "static_size_bytes: struct field {} missing", i
                    ));
                let field_size = static_size_bytes(field, pointer_bytes);
                let field_align = field_size.max(1).min(pointer_bytes);
                offset = align_up(offset, field_align);
                offset += field_size;
                max_align = max_align.max(field_align);
            }
            align_up(offset, max_align)
        }
        BasicTypeEnum::VectorType(_) | BasicTypeEnum::ScalableVectorType(_) => {
            panic!("static_size_bytes: vector types not supported");
        }
    }
}

/// Codegen a toylang function body with the simple internal ABI.
/// Structs/Vec always use sret (ptr first param, void return).
/// Primitives return directly. No Rust ABI coercion.
///
/// Sunny-karp (2026-06-25): accepts `precomputed_typed_body` produced at
/// populate time by `resolve_caller_from_instance`/`from_type_args` (which
/// substituted the cached typed body + ran the late drop-synth pass). When
/// the caller can't supply one (rare — synthesized accessor before the
/// populate channel fills the cache), we fall back to re-running
/// `resolve_fn_body` + `insert_scope_end_drops` here. The fallback exists
/// only for compatibility while the cache fills incrementally.
fn codegen_internal_function<'ctx, 'tcx>(
    ctx: &mut CodegenCtx<'ctx, 'tcx, '_>,
    func: &crate::toylang::registry::ToyFunction,
    internal_symbol: &str,
    precomputed_typed_body: Option<&crate::toylang::typed_ast::TypedBlock>,
) {
    // Per @MBMRVZ, this function's return type is inferred from the
    // body's tail expression. For `main`, that must be void, or the
    // extern wrapper (whose signature is pinned to `fn __toylang_main()`
    // by the Rust shim) will call us with a missing sret buffer and
    // SIGBUS during our final return-value store.
    let owned_fallback: Option<crate::toylang::typed_ast::TypedBlock> = if precomputed_typed_body.is_some() {
        None
    } else {
        // Fallback: re-resolve. Bind tcx/registry locally to dodge the
        // `move` requirement on `ctx`.
        let tcx = ctx.tcx;
        let registry: &crate::toylang::registry::ToylangRegistry = ctx.registry;
        let caller_type_params = func.type_params.clone();
        let caller_a = caller_type_params.clone();
        let caller_b = caller_type_params.clone();
        let rust_method_ret = move |type_name: &str, method: &str, type_args: &[ResolvedType]| -> Result<ResolvedType, crate::oracle::UnresolvedRustType> {
            if type_name.is_empty() {
                crate::oracle::rust_free_fn_return_type(tcx, method, type_args, &caller_a)
                    .map(|opt| opt.unwrap_or(ResolvedType::Void))
            } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                let receiver_ty = &type_args[0];
                let explicit_args = &type_args[1..];
                crate::oracle::rust_trait_method_return_type(tcx, trait_name, method, receiver_ty, explicit_args, &caller_a)
            } else {
                crate::oracle::rust_method_return_type(tcx, type_name, method, type_args, &caller_a)
            }
        };
        let rust_param_types = move |type_name: &str, method: &str, type_args: &[ResolvedType]| -> Result<Option<Vec<ResolvedType>>, crate::oracle::UnresolvedRustType> {
            if type_name.is_empty() {
                crate::oracle::rust_free_fn_param_types(tcx, method, type_args, &caller_b)
            } else if let Some(trait_name) = type_name.strip_prefix("__trait::") {
                crate::oracle::rust_trait_method_param_types(tcx, trait_name, method, &type_args[0], &type_args[1..], &caller_b)
            } else {
                crate::oracle::rust_method_param_types(tcx, type_name, method, type_args, &caller_b)
            }
        };
        let is_rust_trait = move |name: &str| {
            crate::oracle::find_use_imported_trait_def_id(tcx, name).is_some()
        };
        let mut body = crate::toylang::type_resolve::resolve_fn_body(
            registry, func, &rust_method_ret, &rust_param_types, &is_rust_trait,
        ).expect("type resolution should succeed (already validated)");
        let returns_void = matches!(
            &func.return_ty,
            None | Some(ResolvedType::Void),
        );
        crate::toylang::callbacks_impl::insert_scope_end_drops(
            tcx, &mut body, registry, returns_void, &caller_type_params,
        );
        Some(body)
    };
    let typed_body: &crate::toylang::typed_ast::TypedBlock = match precomputed_typed_body {
        Some(tb) => tb,
        None => owned_fallback.as_ref().expect("fallback body was built above"),
    };

    // Resolve any Rust method symbols used in this function by walking the typed body
    resolve_rust_methods_from_typed_body(ctx, typed_body);

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

    // Phase E.b — scope-end drop emission discipline:
    //
    //  * Void return / void-tailed body: emit drops for every tracked
    //    local. No move semantics to worry about; the locals fall out of
    //    scope at fn exit and rustc-side ownership stays self-contained.
    //
    //  * sret return: the return value is a struct that was memcpy'd
    //    from a local. Emitting drops here would double-drop the
    //    returned-via-memcpy value (the source local's storage shares
    //    contents with the sret slot, and the caller will subsequently
    //    drop the sret value too). The MOVE-OUT discipline is: the
    //    caller's `let v = make_vec();` binding is the one that
    //    schedules the eventual drop. SKIP scope drops on sret return.
    //
    //  * Primitive return (i32/bool/...): the return is a register
    //    value — safely independent of any local storage. But if any
    //    local owns a Drop'able value, dropping it here is correct.
    //    Conservative for now: skip — toylang fns that compute then
    //    return primitives typically don't own externally-Drop'able
    //    locals that need cleanup, and the move-semantics edge cases
    //    (`let v = make_vec(); v.len()`) are subtle. Move tracking is
    //    Phase E.c follow-up. The leak is bounded: a forgetting-to-drop
    //    Vec at fn exit gets dropped by the caller's binding.
    //
    // The result: locals only fully participate in scope-end drops at
    // void-returning fns (main / unit-bodied helpers). For non-void
    // returns we lean on the caller's binding to do the drop. Fixture 1
    // (Vec<Widget> in `fn main()`) exercises the void-return path.
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
            // Phase E.d: scope-end drop calls are inserted into the
            // typed AST by `insert_scope_end_drops`. For non-void
            // returns the synthesizer skips entirely — caller's
            // binding owns the drop. Nothing to do here.
            ctx.builder.build_return(None).unwrap();
        } else if llvm_ret_type.is_some() {
            // Primitive return — same as above.
            let val = result.into_value(&ctx.builder);
            ctx.builder.build_return(Some(&val)).unwrap();
        } else {
            // Void-typed ret_expr (e.g. a unit expression). Synthesized
            // drop calls are already present as ExprStmt entries in
            // the block's stmts and were lowered above.
            ctx.builder.build_return(None).unwrap();
        }
    } else {
        // Void-tailed fn body (the @MBMRVZ shape — `fn main() { ... }`
        // with trailing `;`). Same — synth drop calls were lowered
        // earlier as part of the stmt loop.
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
            ResolvedType::RustType { .. } => {
                // Phase 1 D Case 3: when rustc's ABI coerces a small
                // user-defined Rust struct to a direct register return
                // (e.g., `struct MyCounter { count: i32 }` returned as
                // `i32`), the rust_ret_type must be the coerced LLVM type
                // — not None. The earlier `RustType => None` arm assumed
                // all RustTypes go through sret, which holds for opaque
                // toylang views of Rust types in toylang source but not
                // for user-defined types arriving via per_instance_mir
                // substitution from a Rust call site. Same pattern as the
                // `Struct` arm below.
                match &coerced {
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(coerced_str) => {
                        Some(parse_coerced_type(ctx, coerced_str))
                    }
                    _ => None,
                }
            }
            ResolvedType::Struct { .. } => {
                match &coerced {
                    rustc_lang_facade::abi_helpers::CoercedReturn::Direct(coerced_str) => {
                        Some(parse_coerced_type(ctx, coerced_str))
                    }
                    _ => ret_complex_ty.map(|t| t.into()),
                }
            }
            // Tier 3 #3 Phase 1b: synthesised accessors return `&FieldType`
            // (`Ref { inner }`). Returns as a pointer — same shape as
            // `resolved_to_inkwell`'s default Ref arm. Before this arm
            // accessor return types fell through to `_ => None` and the
            // wrapper-codegen path panicked on `rust_ret_type.unwrap()`
            // (llvm_gen.rs:1104). All non-{Str/ByteSlice}-payload Refs
            // are simple pointers at the ABI level.
            ResolvedType::Ref { .. } => {
                Some(ctx.context.ptr_type(AddressSpace::default()).into())
            }
            _ => None,
        }
    };

    let fn_type = match rust_ret_type {
        Some(ret) => ret.fn_type(&param_types, false),
        None => ctx.context.void_type().fn_type(&param_types, false),
    };

    // The rustc-visible extern wrapper. Per @SMPLZ, this symbol gets
    // pinned in `@llvm.used` later in `fill_module` so LTO `internalize`
    // doesn't demote its linkage and break cross-crate references.
    //
    // Reuse an existing declaration of the same symbol if one was
    // already added — e.g. when an internal body (`__toylang_internal_main`)
    // referenced this extern symbol via `declare_external_fn` before
    // the extern wrapper itself was codegen'd. A naive `add_function`
    // would create a SECOND definition with the same name; LLVM
    // disambiguates with a `.1` suffix, the call sites stay bound to
    // the original (now defless) name, and the link fails with
    // "undefined symbol: <T as Drop>::drop". Phase E.c surfaced this
    // through Fixture 6: `__toylang_internal_main`'s emit_scope_drops
    // declares the extern Drop method before the cascade-drain's
    // codegen_extern_wrapper runs to define it.
    let function = match ctx.module.get_function(extern_symbol) {
        Some(existing) => existing,
        None => ctx.module.add_function(extern_symbol, fn_type, None),
    };

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
        // (e.g., {i32,i32} coerced to i64). Bridge via two allocas + memcpy:
        //   1. internal_buf (the existing wrapper_sret alloca, sized as the
        //      internal toylang LLVM type) is the sret target the internal
        //      fn wrote into.
        //   2. coerced_buf (new alloca sized as rust_ret_type) is the staging
        //      buffer we load the return value from.
        //   3. memcpy min(internal_size, rust_size) bytes between them.
        //
        // The min() guard handles BOTH directions:
        //   - rust_ret_type SIZE > internal_size (e.g., 5-byte struct of
        //     bools coerced to i64). Without the second alloca, loading
        //     rust_ret_type from internal_buf reads past internal_buf →
        //     stack garbage at -O1+ (Site #8 of the round-4-close emission
        //     audit; silent miscompile pre-fix).
        //   - rust_ret_type SIZE < internal_size. The memcpy copies only
        //     rust_size bytes; coerced_buf is exactly that size; load fits.
        //
        // Same B10 shape as the push_arg_for_rust_call::Direct fix: declared
        // signature and actual emission must match exactly under ABI coercion.
        let internal_buf = call_args[0].into_pointer_value();
        let rust_ty = rust_ret_type.unwrap();
        let pointer_bytes = ctx.pointer_align;
        let internal_size = static_size_bytes(ret_complex_ty.unwrap(), pointer_bytes);
        let rust_size = static_size_bytes(rust_ty, pointer_bytes);
        let copy_size = internal_size.min(rust_size);

        let coerced_buf = ctx.builder
            .build_alloca(rust_ty, "wrapper_sret_coerced")
            .unwrap();
        let copy_size_val = ctx.context.i64_type().const_int(copy_size, false);
        ctx.builder.build_memcpy(
            coerced_buf, ctx.pointer_align as u32,
            internal_buf, ctx.pointer_align as u32,
            copy_size_val,
        ).unwrap();
        let coerced_val = ctx.builder
            .build_load(rust_ty, coerced_buf, "coerced_ret")
            .unwrap();
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
                let instance = ty::Instance::new_raw(def_id, args_ref);
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

            // Internal ABI return type: void for sret AND for void-returning
            // fns; direct for primitives. Mirrors the predicate used in
            // `codegen_internal_function` when emitting the callee's
            // definition (line ~731) — the forward declaration built here
            // MUST produce the same signature or the forward-declared type
            // shadows the definition's type via `ctx.module.get_function`'s
            // existing-decl lookup, leaving us with `declare i8 @fn()` but
            // `ret void` in the body (LLVM IR verifier rejects).
            //
            // Stale pre-fix behavior: `resolved_to_inkwell(Void)` falls
            // through to `i8` (commented "shouldn't be needed") so void
            // fns forward-declared without this guard got `i8` returns
            // and llc caught the mismatch only for certain toylang
            // programs (hash-order-dependent — HashMap iteration over
            // the call-site set affected whether the call-site decl or
            // the defn decl landed first in LLVM's symbol table).
            let internal_ret_type: Option<BasicTypeEnum<'ctx>> = if callee_sret || expr.ty == ResolvedType::Void {
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

                // Phase R Sites #5/#6: when the receiver coerces to
                // `Pair` (e.g. trait impl on `[T]` / `str` where self is
                // `&[T]` / `&str` — a fat pointer rustc decomposes into
                // (ptr, len) scalars), pushing the receiver as a single
                // `ptr` would mismatch the function signature. Dispatch
                // through `push_arg_for_rust_call` to extract both
                // scalars and push them in sequence. Direct/Indirect
                // shapes keep the existing thin-pointer push, which is
                // what every Sky struct + Rust opaque-type receiver
                // produces today.
                let push_receiver = |ctx: &mut CodegenCtx<'ctx, '_, '_>,
                                     call_args: &mut Vec<inkwell::values::BasicMetadataValueEnum<'ctx>>| {
                    use rustc_lang_facade::abi_helpers::CoercedParam;
                    match &coerced_params[0] {
                        CoercedParam::Pair(_, _) => {
                            push_arg_for_rust_call(ctx, recv_expr, &coerced_params[0], call_args);
                        }
                        _ => {
                            call_args.push(recv_ptr.into());
                        }
                    }
                };
                if is_sret {
                    let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, &format!("{}_trait", ty));
                    call_args.push(alloca.into());
                    push_receiver(ctx, &mut call_args);
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
                    push_receiver(ctx, &mut call_args);
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
            // Phase 2 C.3 mirror — auto-deref through one `&T` layer for
            // field access. Method bodies in impl blocks carry receivers
            // typed as `Ref { inner: Struct }`; the type resolver permits
            // `self.field` there. Codegen must:
            //   1. Use the underlying Struct type for GEP indexing.
            //   2. LOAD the receiver pointer first — `into_ptr` returns the
            //      address of the receiver value (a Widget* stored on the
            //      stack), but for field access we need the address that
            //      pointer holds (the actual Widget). Without the load the
            //      GEP indexes into the stack slot itself and reads
            //      arbitrary stack bytes.
            let auto_deref = matches!(&receiver.ty,
                ResolvedType::Ref { inner } if matches!(**inner, ResolvedType::Struct { .. }));
            let recv_struct_ty = match &receiver.ty {
                ResolvedType::Ref { inner } if matches!(**inner, ResolvedType::Struct { .. })
                    => &**inner,
                other => other,
            };
            let ResolvedType::Struct { name: struct_name, .. } = recv_struct_ty else {
                panic!("field access on non-struct");
            };
            let toy_struct = ctx.registry.structs.get(struct_name.as_str()).unwrap();
            let field_idx = toy_struct.fields.iter()
                .position(|f| f.name == *field)
                .unwrap() as u32;
            let struct_ty = ctx.resolved_to_struct_type(recv_struct_ty);
            let struct_ptr = if auto_deref {
                // Receiver is `&Struct`. `into_ptr` with a ptr-typed slot
                // returns the address of the slot; load it to get the
                // Widget pointer the slot holds.
                let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
                let slot = recv_result.into_ptr(&ctx.builder,
                    ptr_ty.as_basic_type_enum(), "fa_recv_slot");
                ctx.builder.build_load(ptr_ty, slot, "fa_recv_deref")
                    .unwrap().into_pointer_value()
            } else {
                recv_result.into_ptr(&ctx.builder,
                    struct_ty.as_basic_type_enum(), "fa_recv")
            };
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
            // Phase R Site #6: same Pair-receiver dispatch as Site #5
            // (trait static call). When the method's `self` coerces to
            // `Pair` (e.g., `impl SomeTrait for [T]`'s `&self` →
            // `(ptr, len)`), pushing `recv_ptr` as a single thin pointer
            // mismatches the function signature. Direct/Indirect shapes
            // keep the existing thin-pointer push.
            let push_method_receiver = |ctx: &mut CodegenCtx<'ctx, '_, '_>,
                                        call_args: &mut Vec<inkwell::values::BasicMetadataValueEnum<'ctx>>| {
                use rustc_lang_facade::abi_helpers::CoercedParam;
                match &coerced_params[0] {
                    CoercedParam::Pair(_, _) => {
                        push_arg_for_rust_call(ctx, receiver, &coerced_params[0], call_args);
                    }
                    _ => {
                        call_args.push(recv_ptr.into());
                    }
                }
            };
            if is_sret {
                // sret method call (constructor-like returning opaque type)
                let alloca = ctx.alloca_opaque_rust_ty(&expr.ty, "method_sret");
                let opaque_ty = ctx.rust_ty_to_llvm_opaque(&expr.ty).0;
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![alloca.into()];
                push_method_receiver(ctx, &mut call_args);
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
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
                push_method_receiver(ctx, &mut call_args);
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
            // &expr — take a pointer to the inner expression.
            //
            // **This is a CORRECTNESS fix, not an optimization** (reviewer
            // round-4-followup note 2026-06-25). The natural-looking
            // "alloca + store + return ptr" path is incorrect for any
            // primitive type whose LLVM storage type's bit-padding bits are
            // UNSPECIFIED — notably `i1` for `bool`, per LLVM's IR
            // semantics. The fix returns the GEP pointer to the field's
            // actual storage in the receiver struct, matching what rustc
            // does for the same reason. Future readers MUST NOT "simplify"
            // this back to the load-realloc shape — see the bool subtree
            // below for the bit-level reasoning.
            //
            // Special-case `&struct.field` for primitive-typed fields: the
            // FieldAccess primitive arm eagerly LOADS the field value (so
            // `let x = w.field` gets copy-by-value semantics). If we then
            // round-trip through `into_ptr` (alloca + store + return ptr),
            // the returned pointer points to a FRESH alloca of the
            // primitive's LLVM storage type — NOT to the original field
            // location.
            //
            // For most primitives that round-trip preserves observable
            // behavior. For `bool`, Sky maps `ResolvedType::Bool` to LLVM
            // `i1`, whose in-memory storage is 1 byte with the bool's value
            // in the LSB and the upper 7 bits *unspecified* (LLVM Lang Ref:
            // "the type-bit-padding bits are unspecified"). When a Rust
            // caller dereferences the returned `&bool` via a safe
            // reference, rustc emits `load i8` — reading those unspecified
            // upper bits. Even though the LSB carries the correct value,
            // the resulting `i8` is unreliable (LLVM may exploit the
            // unspecified bits during optimization) → `*&w.bool_field`
            // reads as `false` regardless of the actual stored value.
            //
            // Symptom of the unfixed shape: any Sky export with a `bool`
            // field, called from Rust through the accessor `w.field()`,
            // returned undefined bool values to the Rust caller. Surfaced
            // during Phase R Site #8 probing 2026-06-25.
            //
            // Fix: when the inner expression is a FieldAccess on a struct,
            // return the GEP pointer directly — same shape as the
            // FieldAccess Struct/RustType arms, which already produce
            // `ExprResult::Ptr(gep, _)`. The returned `&field` now points
            // into the receiver struct's actual memory, where the byte's
            // value matches what was stored (no unspecified upper bits).
            if let TypedExprKind::FieldAccess { receiver, field } = &inner.kind {
                let recv_struct_ty = match &receiver.ty {
                    ResolvedType::Ref { inner: r }
                        if matches!(**r, ResolvedType::Struct { .. }) => &**r,
                    other => other,
                };
                if let ResolvedType::Struct { name: struct_name, .. } = recv_struct_ty {
                    let toy_struct = ctx.registry.structs.get(struct_name.as_str()).unwrap();
                    let field_idx = toy_struct.fields.iter()
                        .position(|f| f.name == *field)
                        .unwrap() as u32;
                    let struct_ty = ctx.resolved_to_struct_type(recv_struct_ty);
                    let recv_result = lower_typed_expr(ctx, receiver);
                    let struct_ptr = match &receiver.ty {
                        ResolvedType::Ref { inner: r }
                            if matches!(**r, ResolvedType::Struct { .. }) =>
                        {
                            let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
                            let slot = recv_result.into_ptr(
                                &ctx.builder, ptr_ty.as_basic_type_enum(), "ref_fa_slot");
                            ctx.builder.build_load(ptr_ty, slot, "ref_fa_deref")
                                .unwrap().into_pointer_value()
                        }
                        _ => recv_result.into_ptr(
                            &ctx.builder, struct_ty.as_basic_type_enum(), "ref_fa_recv"),
                    };
                    let gep = ctx.builder
                        .build_struct_gep(struct_ty, struct_ptr, field_idx, field)
                        .unwrap();
                    return ExprResult::Value(gep.into());
                }
            }
            // Default path (non-field-access referents): the round-trip
            // remains observable-correct for primitives whose storage
            // type matches the in-memory representation (i32/i64/f64/etc).
            let inner_result = lower_typed_expr(ctx, inner);
            let inner_ty = ctx.resolved_to_inkwell(&inner.ty);
            let ptr = inner_result.into_ptr(&ctx.builder, inner_ty, "ref_val");
            ExprResult::Value(ptr.into())
        }
    }
}

// `local_needs_scope_drop` retired 2026-06-23 (Phase E.d). The
// equivalent predicate lives next to the synthesis pass in
// `callbacks_impl::local_needs_scope_drop`. Lowering itself no
// longer makes drop decisions — drop calls are present in the
// typed AST as ordinary StaticCall nodes by the time lowering runs.

fn lower_typed_stmt<'ctx>(ctx: &mut CodegenCtx<'ctx, '_, '_>, stmt: &TypedStmt) {
    match stmt {
        TypedStmt::Let { name, expr, drop_synthesized: _ } => {
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
// independently register the dep so the `optimized_mir` override reifies
// a ReifyFnPointer to it; otherwise the linker fails on a missing symbol.
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
    // Float arms — covers the full set rustc's abi_helpers emits via
    // `primitive_to_llvm_str` and `reg_to_llvm_str`. Phase R Site #10:
    // before this, only "double" was handled; any other float string
    // (Sky source returning/taking f32 once toylang grammar grows it,
    // or SIMD-using Sky exports producing vector returns) would panic
    // with "unsupported coerced type: ...".
    match s {
        "ptr" => return ctx.context.ptr_type(AddressSpace::default()).into(),
        "half" => return ctx.context.f16_type().into(),
        "float" => return ctx.context.f32_type().into(),
        "double" => return ctx.context.f64_type().into(),
        "fp128" => return ctx.context.f128_type().into(),
        _ => {}
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
    } else if s.starts_with("<") && s.ends_with(">") {
        // LLVM vector type like "<8 x i8>" — produced by
        // `reg_to_llvm_str` for `RegKind::Vector`. Rustc emits this
        // when the ABI uses a vector register for a coerced return /
        // param. Not currently triggered by toylang (no SIMD types in
        // the grammar) but added defensively so the trigger surface
        // doesn't ICE on first SIMD use.
        let inner = &s[1..s.len()-1]; // "8 x i8"
        let parts: Vec<&str> = inner.split(" x ").collect();
        let count: u32 = parts[0].trim().parse()
            .unwrap_or_else(|_| panic!("bad vector size in coerced type: {}", s));
        let elem = parse_coerced_type(ctx, parts[1].trim());
        match elem {
            BasicTypeEnum::IntType(it) => it.vec_type(count).into(),
            BasicTypeEnum::FloatType(ft) => ft.vec_type(count).into(),
            _ => panic!("unsupported vector element type: {}", parts[1].trim()),
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

/// Approach B (patch 4 rev 2): fill the rustc-owned LLVM module with Sky's
/// codegen output. The caller — `consumer_fill_modules` in
/// `callbacks_impl.rs` — has wrapped the borrowed `LLVMContextRef` +
/// `LLVMModuleRef` (handed out by the facade's `LlvmModuleFactory`) in
/// suppressed-Drop Inkwell handles and lends us `&Context` + `&Module<'ctx>`.
/// We construct a `CodegenCtx`, walk the consumer instances + accessors,
/// and emit IR directly into rustc's module via Inkwell's `&self` APIs.
///
/// No bitcode serialisation, no IR-text round-trip, no LLVM-context migration:
/// rustc retains ownership of the module throughout, and finalises it through
/// the standard optimise → ThinLTO → emission pipeline after the
/// `fill_extra_modules` call returns.
pub fn fill_module<'tcx, 'ctx>(
    tcx: TyCtxt<'tcx>,
    registry: &ToylangRegistry,
    // Tier 3 #3 Phase 1c: `_callbacks` is unused since the accessor branch
    // retired — the only caller was `notify_concrete_entry_point_inner`.
    // Kept in the signature to avoid disturbing the public API for
    // consumer-language reuse; the parameter could be removed in a
    // separate API-cleanup PR.
    _callbacks: &crate::toylang::callbacks_impl::ToylangCallbacks,
    state: &mut crate::toylang::callbacks_impl::ToylangState,
    context: &'ctx Context,
    module: &'ctx Module<'ctx>,
) -> Vec<String> {
    let mut ctx = CodegenCtx::new(context, module, tcx, registry);
    let mut seen_symbols = std::collections::HashSet::new();

    // Collect all toylang mono items (accessors and functions) first, then codegen.
    struct FnItem<'tcx> {
        resolved_func: crate::toylang::registry::ToyFunction,
        /// Sunny-karp (2026-06-25) — substituted typed body produced at
        /// populate time. `None` only for extern declarations (which have
        /// no Sky body to lower). Codegen reads this directly rather than
        /// re-running `type_resolve_body` at the per-Instance step.
        typed_body: Option<crate::toylang::typed_ast::TypedBlock>,
        /// Some for entry-point functions (Rust calls them), None for internal-only.
        /// Used to generate extern ABI wrappers.
        instance: Option<ty::Instance<'tcx>>,
        /// Path B: rustc-mangled name for rustc-visible items (so Sky's body is
        /// the sole definition of the symbol rustc emits call sites to); Sky-
        /// internal name for deep-walker items rustc never references.
        extern_symbol: String,
        /// Sky-internal symbol for the simple-ABI body. Distinct from
        /// `extern_symbol` for rustc-visible items (the extern wrapper calls
        /// the internal). Equal to `extern_symbol` for Sky-internal items
        /// (which never get a wrapper).
        internal_symbol: String,
    }
    let mut fn_items: Vec<FnItem<'tcx>> = Vec::new();

    // Walk MonoItems for accessor methods (still discovered via rustc).
    // Regular toylang functions come from state.toylang_instances instead.
    // We re-call the saved upstream partition provider to walk the CGU
    // slice for Case 1b generic toylang fns instantiated from Rust
    // callers (`__lang_stubs::wrap::<LocalThing>(42)` from a
    // `rust_caller.rs`). Historically (pre-Option 4) the facade overrode
    // `collect_and_partition_mono_items` to filter consumer items out of
    // rustc's CGU list, and this call bypassed the in-memory cache to
    // get the unfiltered slice. Option 4 (arch §F.14, 2026-06-20) retired
    // that override — `codegen_fn_attrs` now marks consumer items with
    // `AvailableExternally` linkage instead, so rustc emits no `.o`
    // symbol for them but they still appear in the CGU list. The walk
    // below now sees consumer items naturally (the `is_consumer_codegen_target`
    // checks downstream skip them); we keep the upstream-default call
    // so the saved fn pointer remains the single source of truth even
    // if a future override is added back.
    let partitions = rustc_lang_facade::default_collect_and_partition()(tcx, ());
    let cgus = partitions.codegen_units;
    for cgu in cgus.iter() {
        for (&mono_item, _) in cgu.items() {
            let rustc_middle::mir::mono::MonoItem::Fn(instance) = mono_item else { continue };
            let def_id = instance.def_id();
            if !rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
                continue;
            }

            // Tier 3 #3 Phase 1c: accessor methods retired from the CGU
            // walk. Synthesised at registry build via
            // `synthesize_accessor_pairs`, populated via the standard
            // `ToylangInstance` pipeline at populate time (non-generic
            // structs) — see the accessor_pairs loop in
            // `populate_toylang_instances_from_cgus`. Their
            // codegen flows through the same
            // `codegen_internal_function` + `codegen_extern_wrapper`
            // pipeline as any other consumer fn, so the dedicated
            // `codegen_accessor_inline` path is dead. Skipping the
            // continue-on-assoc-item gate also drops Phase 1b's redundant
            // dedup hit on the populate-side accessor entries.

            // For regular consumer functions from MonoItems: record the Instance
            // so we can generate an extern wrapper.
            //
            // Phase 1 D Case 1b: the resolved_func may come from either
            // `state.toylang_instances` (already-walked, populated by
            // populate_toylang_instances_from_cgus's entry-point walk + the
            // transitive callee surfacing inside walk_and_stash_internal_callees)
            // OR, for generic consumer fns instantiated only via Rust
            // call sites (`__lang_stubs::identity::<i32>(42)` from a
            // `rust_caller` source), be synthesized here from the registry
            // entry + the concrete Instance args. The synthesis is the
            // load-bearing piece for Case 1b/4/6: cross-language generic
            // monomorphization (architecture §20.8.5) requires rustc's
            // mono collector to surface the concrete Instance, since Sky's
            // frontend has no way to enumerate what Rust source will
            // instantiate. Generic exports stay non-roots in the entry-
            // point walk by design; this codepath is where they enter the
            // codegen queue.
            let name = tcx.item_name(def_id).to_string();
            let registry_name = if name == crate::oracle::TOYLANG_MAIN { "main".to_string() } else { name.clone() };
            let Some(toy_fn) = registry.functions.get(&registry_name) else { continue; };
            let extern_symbol = crate::toylang::callbacks_impl::compute_fn_symbol(
                &registry_name, tcx, instance,
            );
            if !seen_symbols.insert(extern_symbol.clone()) { continue; }
            let (resolved_func, typed_body) = if let Some(inst) = state.toylang_instances
                .iter()
                .find(|i| i.extern_symbol == extern_symbol)
            {
                (inst.resolved_func.clone(), inst.typed_body.clone())
            } else {
                let resolved_caller = crate::toylang::callbacks_impl::resolve_caller_from_instance(
                    tcx, registry, &state.typed_bodies, &registry_name, toy_fn, instance,
                );
                (resolved_caller.func, resolved_caller.typed_body)
            };
            let internal_symbol = internal_symbol_for_instance(&registry_name, tcx, instance);
            fn_items.push(FnItem {
                resolved_func,
                typed_body,
                instance: Some(instance),
                extern_symbol,
                internal_symbol,
            });
        }
    }

    // Add toylang instances populated by `populate_toylang_instances_from_cgus`.
    // Under Workstream A (course-correct.md items #11 + #15) this iterates
    // the user-bin compile's registry-driven discovery output. Each instance
    // that carries a `stub_def_id` (looked up via the upstream `__lang_stubs`
    // rlib's `pub fn` shell) gets promoted to `instance: Some(...)` here so
    // it qualifies for extern-wrapper codegen below — without that promotion
    // link would fail with the rustc-mangled stub symbol undefined.
    // The `def_id`-only Instance (`Instance::new_raw(def_id, empty_args)`)
    // carries enough info for `fn_abi_of_instance` to compute the Rust ABI
    // for non-generic fns AND for `default_symbol_name` to return the
    // rustc-mangled name Path B's `compute_fn_symbol` used at populate time.
    for inst in &state.toylang_instances {
        if seen_symbols.insert(inst.extern_symbol.clone()) {
            let instance = inst.stub_def_id.map(|def_id| {
                // Use the stored concrete args (empty `Vec` for non-generics —
                // the degenerate case of the general path; `build_generic_args_for_item`
                // produces empty `GenericArgs` for an item with no type params).
                // For generic items (discovered via Option B sidecar) the stored
                // `instance_args` carry the concrete `ResolvedType` per type
                // param so codegen reconstructs the rustc Instance correctly.
                let rustc_type_args: Vec<ty::GenericArg<'tcx>> = inst.instance_args.iter()
                    .map(|a| ty::GenericArg::from(
                        crate::oracle::resolved_to_rustc_ty(tcx, a)
                    ))
                    .collect();
                let args = crate::oracle::build_generic_args_for_item(
                    tcx, def_id, &rustc_type_args,
                );
                ty::Instance::new_raw(def_id, args)
            });
            // Path B: internal_symbol is now pre-computed at populate time
            // (see ToylangInstance docs). For rustc-visible items it's
            // distinct from the rustc-mangled extern_symbol; for Sky-
            // internal items it equals extern_symbol (the wrapper loop
            // skips them anyway).
            fn_items.push(FnItem {
                resolved_func: inst.resolved_func.clone(),
                typed_body: inst.typed_body.clone(),
                instance,
                extern_symbol: inst.extern_symbol.clone(),
                internal_symbol: inst.internal_symbol.clone(),
            });
        }
    }

    // Two-pass codegen: internal functions first, then extern wrappers.
    // Internal functions use a simple ABI (structs always sret, primitives direct).
    // Extern wrappers adapt to Rust ABI and delegate to the internal function.
    // Only entry-point functions (with Instance) get extern wrappers.
    for item in &fn_items {
        codegen_internal_function(&mut ctx, &item.resolved_func, &item.internal_symbol, item.typed_body.as_ref());
    }
    for item in &fn_items {
        if let Some(instance) = item.instance {
            codegen_extern_wrapper(&mut ctx, &item.resolved_func, instance, &item.extern_symbol, &item.internal_symbol);
        }
    }

    // Phase 3 inline-codegen plan: mirror rustc's per-function attribute set
    // so LLVM's cross-module ThinLTO inliner accepts Sky callers ↔ Rust
    // callees inlining. Required attributes per
    // `rustc_codegen_llvm/src/attributes.rs::llfn_attrs_from_instance` and
    // LLVM's `functionsHaveCompatibleAttributes` check.
    apply_rust_compat_attributes(&ctx);

    // Per @SMPLZ, pin every Sky-emitted rustc-visible function in
    // `@llvm.used` so BOTH the in-process optimize pipeline AND the LTO
    // `internalize` pass preserve them at -O>=2.
    //
    // Functions like `<Wrapper<i32> as Clone>::clone` are emitted into
    // Sky's CGU but referenced only from OTHER compile units (the stub
    // rlib's `duplicate<Wrapper<i32>>` body calls them via the rustc-
    // mangled name). Within Sky's CGU there is no in-module caller.
    //
    // Three relevant LLVM passes try to remove or rewrite these:
    //   1. `GlobalOpt`/`GlobalDCE` (non-LTO and LTO pre-merge): would
    //      mark the function dead and remove it.
    //   2. LTO `internalize`: would change `External` linkage to
    //      `Internal` because no callers in the merged module reach it.
    //   3. Linker dead-strip (-Wl,-dead_strip on macOS): would remove
    //      the symbol entirely from the final binary.
    //
    // `@llvm.used` defeats all three. The earlier `@llvm.compiler.used`
    // variant only defeats (1) — at non-LTO that was sufficient (no LTO
    // internalize, no merge), but under fat LTO (2) ran and silently
    // turned the symbol into a local definition. The stub rlib's
    // external reference then went unresolved at the linker step.
    //
    // The cost of `llvm.used` over `llvm.compiler.used` is that the
    // linker no longer dead-strips these symbols even when truly dead.
    // For Sky's case the symbols are by construction reachable from
    // other compile units (the entire point of emitting them is for
    // cross-crate calls), so the linker would have kept them anyway.
    // See rust-interop-architecture.md §25.2 B15 for the fat-LTO
    // regression that motivated this; non-LTO and ThinLTO behave the
    // same as before.
    // §5.5 Step 2: under the narrower revision (non-generic Sky bodies
    // emit at owning crate, not user_bin), Sky-INTERNAL symbols also
    // become cross-crate-referenced. lang_stubs_<bin>'s `__toylang_main`
    // body calls `__toylang_internal_<callee>` directly; under Step 2
    // those internal symbols live in the upstream's rlib. At -O>=2,
    // LLVM's GlobalDCE strips internals at the owning crate when the
    // in-CGU caller (the extern wrapper) gets inlined-and-stripped
    // itself. Pin both extern wrappers AND internal symbols so they
    // survive cross-crate references.
    //
    // For Sky-internal-only items (no extern wrapper — `f.instance.is_none()`),
    // the internal_symbol equals extern_symbol; we still pin them for
    // the cross-crate-internal-callee case.
    let mut pinned_syms: Vec<&str> = Vec::with_capacity(fn_items.len() * 2);
    for f in &fn_items {
        if f.instance.is_some() {
            pinned_syms.push(f.extern_symbol.as_str());
        }
        // The internal symbol may equal the extern symbol (for Sky-internal-
        // only items); HashSet semantics aren't needed — `@llvm.used` is
        // tolerant of duplicate entries.
        if f.internal_symbol != f.extern_symbol || f.instance.is_none() {
            pinned_syms.push(f.internal_symbol.as_str());
        }
    }
    pin_in_llvm_used(&ctx, &pinned_syms);

    let rust_symbols = ctx.rust_symbols.clone();

    // Approach B: no bitcode emission, no IR-text round-trip. rustc's
    // module has been filled in place via the borrowed Inkwell wrappers;
    // dropping `ctx` here calls ManuallyDrop's no-op Drop on both `module`
    // and (via `ctx_owner` going out of scope at the end of fill_module)
    // the Context. rustc retains ownership and finalises the module
    // through the standard pipeline.
    rust_symbols
}

/// Phase 3: mirror rustc's per-function attribute set on every Sky-emitted
/// function so cross-module ThinLTO can inline Sky ↔ Rust call sites.
///
/// LLVM's `InlineCost.cpp::functionsHaveCompatibleAttributes` rejects
/// inlining across module boundaries when attributes mismatch. Required
/// for compatibility:
/// - "target-cpu" — drives the LLVM feature-bitset subset check.
/// - "target-features" — same.
/// - "frame-pointer" — common cause of "incompatible function attributes".
/// - "probe-stack" — same.
/// - `uwtable` — sync vs async unwind tables must match.
/// - `nounwind` — required under panic=abort (Sky discipline).
///
/// Values pulled from `tcx.sess.target` so they automatically track rustc's
/// per-target defaults (e.g., "non-leaf" frame pointer on aarch64-apple-*).
fn apply_rust_compat_attributes<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>) {
    use inkwell::attributes::AttributeLoc;
    use rustc_target::spec::{FramePointer, StackProbeType};

    let target = &ctx.tcx.sess.target;
    let target_cpu = target.cpu.to_string();

    let frame_pointer = match target.frame_pointer {
        FramePointer::Always => Some("all"),
        FramePointer::NonLeaf => Some("non-leaf"),
        FramePointer::MayOmit => None,
    };

    let probe_stack: Option<&'static str> = match &target.stack_probes {
        StackProbeType::None => None,
        // Inline / InlineOrCall (under LLVM 21 the latter resolves to Inline)
        // both map to "inline-asm".
        StackProbeType::Inline | StackProbeType::InlineOrCall { .. } => Some("inline-asm"),
        // Call form invokes __rust_probestack; the attribute value is the
        // symbol name. We don't currently call into __rust_probestack so the
        // attribute is omitted (Sky's IR doesn't need stack probing for its
        // simple frames). If a future Sky-emitted function has a large
        // stack frame, revisit.
        StackProbeType::Call => None,
    };

    let nounwind_kind = inkwell::attributes::Attribute::get_named_enum_kind_id("nounwind");
    let uwtable_kind = inkwell::attributes::Attribute::get_named_enum_kind_id("uwtable");

    // uwtable values: 0 = none, 1 = sync, 2 = async. Match rustc's "must emit
    // unwind tables" decision.
    let uwtable_value: u64 = if ctx.tcx.sess.must_emit_unwind_tables() { 1 } else { 0 };

    let nounwind_attr = ctx.context.create_enum_attribute(nounwind_kind, 0);
    let uwtable_attr = ctx.context.create_enum_attribute(uwtable_kind, uwtable_value);
    let target_cpu_attr = ctx.context.create_string_attribute("target-cpu", &target_cpu);
    let frame_pointer_attr = frame_pointer
        .map(|v| ctx.context.create_string_attribute("frame-pointer", v));
    let probe_stack_attr = probe_stack
        .map(|v| ctx.context.create_string_attribute("probe-stack", v));

    for f in ctx.module.get_functions() {
        // Skip pure declarations (extern decls). Their callees will get
        // attributes when rustc emits the corresponding CGUs; cross-module
        // ThinLTO uses the callee's attributes, not the local declaration's.
        if f.count_basic_blocks() == 0 {
            continue;
        }
        f.add_attribute(AttributeLoc::Function, target_cpu_attr);
        if let Some(attr) = frame_pointer_attr {
            f.add_attribute(AttributeLoc::Function, attr);
        }
        if uwtable_value != 0 {
            f.add_attribute(AttributeLoc::Function, uwtable_attr);
        }
        // Sky discipline: panic=abort, so every fn is nounwind.
        f.add_attribute(AttributeLoc::Function, nounwind_attr);
        if let Some(attr) = probe_stack_attr {
            f.add_attribute(AttributeLoc::Function, attr);
        }
    }
}


// Tier 3 #3 Phase 1c: `codegen_accessor_inline` retired. Accessors now
// flow through the regular `codegen_internal_function` +
// `codegen_extern_wrapper` pipeline via the synthesized `&self.field`
// body. LLVM inlines the trivial wrapper at -O1 and above; pre-opt IR
// carries the extra forwarding call but is bounded (one per field).

/// Append the given function symbols to an `@llvm.used` array so LLVM's
/// `GlobalOpt`/`GlobalDCE`, LTO `internalize`, and linker `-dead_strip`
/// passes all preserve them, AND preserve their original linkage.
///
/// Codifies @SMPLZ — the discipline that any Sky-emitted symbol whose
/// intended caller is in another compile unit's machine code MUST be
/// pinned in `@llvm.used` (not the weaker `@llvm.compiler.used`).
///
/// Why this is needed: Sky emits rustc-visible bodies (the rustc-mangled
/// extern symbols for trait-impl methods etc.) into its own CGU. Their only
/// callers are in OTHER compile units' bitcode (the stub rlib's
/// `duplicate<Wrapper<T>>` body references `<Wrapper<T> as Clone>::clone`
/// via the rustc-mangled name). Within Sky's CGU there is no internal use.
///
/// Three relevant LLVM passes try to remove or rewrite these symbols:
///   1. `GlobalOpt`/`GlobalDCE` at -O>=2 (non-LTO + LTO pre-merge).
///   2. LTO `internalize`: changes External to Internal linkage when no
///      callers in the merged module reach it (which would silently
///      break cross-crate calls from the stub rlib).
///   3. Linker dead-strip (`-Wl,-dead_strip` on macOS) at final link.
///
/// `@llvm.used` defeats all three: passes (1) and (2) treat its entries
/// as "must preserve including linkage"; the linker treats them as
/// gc-roots. The weaker `@llvm.compiler.used` variant defeats only
/// (1) — non-LTO and ThinLTO survive on it, but fat LTO trips (2)
/// and the symbol becomes Local in the final `.o`, breaking the cross-
/// crate reference at link time. Toylang's `opt_level_3_fat_lto_smoke`
/// surfaced this empirically; see rust-interop-architecture.md §25.2 B15.
///
/// Cost trade-off: linker no longer dead-strips these even when dead.
/// For Sky's case the entries are by construction cross-crate-callable
/// (the whole reason to emit them); the linker would have kept them
/// anyway, so the trade is essentially free in practice.
///
/// Pure declarations (zero basic blocks) are skipped — they have no
/// body to preserve. Missing-name lookups silently no-op as a defensive
/// guard; by construction every listed name is present in the module.
fn pin_in_llvm_used<'ctx>(ctx: &CodegenCtx<'ctx, '_, '_>, names: &[&str]) {
    use inkwell::module::Linkage;
    use inkwell::values::BasicValueEnum;

    if names.is_empty() {
        return;
    }

    let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
    let mut entries: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(names.len());
    for name in names {
        let Some(f) = ctx.module.get_function(name) else { continue };
        if f.count_basic_blocks() == 0 {
            continue;
        }
        entries.push(f.as_global_value().as_pointer_value().into());
    }

    if entries.is_empty() {
        return;
    }

    let pointers: Vec<inkwell::values::PointerValue<'ctx>> = entries
        .into_iter()
        .map(|v| match v {
            BasicValueEnum::PointerValue(p) => p,
            _ => unreachable!("entries are built from as_pointer_value above"),
        })
        .collect();
    let array_ty = ptr_ty.array_type(pointers.len() as u32);
    let initializer = ptr_ty.const_array(&pointers);

    let global = ctx.module.add_global(array_ty, None, "llvm.used");
    global.set_linkage(Linkage::Appending);
    global.set_section(Some("llvm.metadata"));
    global.set_initializer(&initializer);
}


