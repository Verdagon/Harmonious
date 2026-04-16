//! ABI coercion helpers.
//!
//! When rustc compiles a function that returns a small struct (e.g. `{ i32, i32 }`),
//! it may coerce the return into a scalar register (e.g. `i64` on aarch64). The
//! consumer's LLVM backend must match this coercion exactly, or the caller will
//! read garbage from the wrong registers.
//!
//! This module wraps rustc's `fn_abi_of_instance` query to determine what LLVM
//! return type rustc expects. The consumer calls this during `generate_and_compile`
//! (Phase 3) when generating LLVM IR for its function bodies.
//!
//! See `docs/historical/problem-abi-coercion.md` for the investigation that led to
//! this approach — small struct returns were silently zeroing fields before this fix.

use rustc_abi::Size;
use rustc_target::callconv::{Reg, RegKind};
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, TyCtxt};
use rustc_target::callconv::{CastTarget, PassMode};

/// Describes how a function's return value should be represented in LLVM IR.
/// Per @ACRTFDZ, the consumer must use this coerced type (not its own type
/// representation) when declaring LLVM functions that call Rust code.
///
/// - `Direct("i64")` → the struct is returned as a scalar, use alloca+store+load pattern
/// - `Indirect` → the caller provides an sret pointer, return via pointer write
/// - `Void` → no return value
pub enum CoercedReturn {
    /// Return directly in the coerced type (e.g., "i64" for `{ i32, i32 }` on aarch64).
    /// The string is an LLVM type like "i64", "[2 x i32]", or "{ i32, float }".
    Direct(String),
    /// Return via sret pointer. The caller allocates space and passes a pointer as
    /// the first argument. Used for large structs (>16 bytes on aarch64).
    Indirect,
    /// No return value (ZST or void).
    Void,
}

/// Query rustc for the ABI-coerced LLVM return type of a function.
///
/// This wraps `tcx.fn_abi_of_instance` and translates the `PassMode` into an
/// LLVM type string. The consumer calls this for each function it compiles to
/// LLVM IR, to ensure the return instruction matches what rustc's caller expects.
///
/// The coercion rules are target-specific (aarch64 vs x86_64 vs wasm have different
/// thresholds and register assignments). By querying rustc, we get the correct answer
/// for whatever target rustc is compiling for.
pub fn coerced_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CoercedReturn {
    let instance = ty::Instance::mono(tcx, def_id.to_def_id());
    coerced_return_type_for_instance(tcx, instance)
}

/// Query rustc for the ABI-coerced return type of a function instance.
/// Per @ACRTFDZ, callers must use this result (not the source-level type)
/// when declaring LLVM functions.
pub fn coerced_return_type_for_instance<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> CoercedReturn {
    let typing_env = ty::TypingEnv::fully_monomorphized();

    let fn_abi = tcx
        .fn_abi_of_instance(typing_env.as_query_input((instance, ty::List::empty())))
        .expect("fn_abi_of_instance failed");


    match &fn_abi.ret.mode {
        PassMode::Ignore => CoercedReturn::Void,
        PassMode::Direct(_) => {
            // Scalar return — the natural LLVM type is correct.
            // Map the scalar size to an LLVM type.
            let size = fn_abi.ret.layout.size;
            CoercedReturn::Direct(format!("i{}", size.bits()))
        }
        PassMode::Pair(_, _) => {
            // ScalarPair — two values returned in registers.
            if let rustc_abi::BackendRepr::ScalarPair(s1, s2) = fn_abi.ret.layout.backend_repr {
                let a_str = primitive_to_llvm_str(s1.primitive());
                let b_str = primitive_to_llvm_str(s2.primitive());
                CoercedReturn::Direct(format!("{{ {}, {} }}", a_str, b_str))
            } else {
                CoercedReturn::Direct(format!("i{}", fn_abi.ret.layout.size.bits()))
            }
        }
        PassMode::Cast { cast, .. } => {
            CoercedReturn::Direct(cast_target_to_llvm_str(cast))
        }
        PassMode::Indirect { .. } => CoercedReturn::Indirect,
    }
}

/// Describes how a function parameter should be represented in LLVM IR.
#[derive(Clone, Debug)]
pub enum CoercedParam {
    /// Passed as the given LLVM type string (e.g. "i64" for a coerced struct,
    /// "i32" for a scalar). The consumer must convert to the internal type if different.
    Direct(String),
    /// ScalarPair — two values passed in separate registers (e.g. ptr + len for &[u8]).
    /// Rustc emits these as two separate LLVM function parameters, not one struct.
    Pair(String, String),
    /// Passed by pointer (large structs, or byval on 32-bit x86).
    Indirect,
    /// ZST — not present in the LLVM signature.
    Ignore,
}

/// Query rustc for the ABI-coerced LLVM parameter types of a function.
///
/// Returns one `CoercedParam` per declared parameter (NOT including sret).
/// The consumer's extern wrapper uses this to build a signature matching Rust's ABI,
/// then converts each param to the internal function's expected type.
///
/// Per @TCHAPZ, for `#[track_caller]` functions this includes a hidden
/// `&Location` pointer as the last entry. Callers must detect this via
/// `instance.def.requires_caller_location(tcx)` and pass null at call sites.
pub fn coerced_param_types_for_instance<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
) -> Vec<CoercedParam> {
    let typing_env = ty::TypingEnv::fully_monomorphized();
    let fn_abi = tcx
        .fn_abi_of_instance(typing_env.as_query_input((instance, ty::List::empty())))
        .expect("fn_abi_of_instance failed");

    fn_abi.args.iter().map(|arg| {
        match &arg.mode {
            PassMode::Ignore => CoercedParam::Ignore,
            PassMode::Direct(_) => {
                // Check if this is a pointer type (e.g. &Vec<T>, &T)
                if let rustc_abi::BackendRepr::Scalar(scalar) = arg.layout.backend_repr {
                    if matches!(scalar.primitive(), rustc_abi::Primitive::Pointer(_)) {
                        return CoercedParam::Direct("ptr".to_string());
                    }
                }
                CoercedParam::Direct(format!("i{}", arg.layout.size.bits()))
            }
            PassMode::Pair(_, _) => {
                // ScalarPair — two values passed in separate registers.
                // Rustc emits these as two separate LLVM params, not one struct.
                if let rustc_abi::BackendRepr::ScalarPair(s1, s2) = arg.layout.backend_repr {
                    let a_str = primitive_to_llvm_str(s1.primitive());
                    let b_str = primitive_to_llvm_str(s2.primitive());
                    CoercedParam::Pair(a_str, b_str)
                } else {
                    CoercedParam::Direct(format!("i{}", arg.layout.size.bits()))
                }
            }
            PassMode::Cast { cast, .. } => {
                CoercedParam::Direct(cast_target_to_llvm_str(cast))
            }
            PassMode::Indirect { .. } => CoercedParam::Indirect,
        }
    }).collect()
}

fn cast_target_to_llvm_str(cast: &CastTarget) -> String {
    let has_prefix = cast.prefix.iter().any(|x| x.is_some());
    let unit = cast.rest.unit;
    let rest_count = if cast.rest.total == Size::ZERO {
        0
    } else {
        (cast.rest.total.bytes() + unit.size.bytes() - 1) / unit.size.bytes()
    };
    let unit_str = reg_to_llvm_str(unit);

    if !has_prefix {
        if rest_count <= 1 {
            unit_str
        } else {
            format!("[{} x {}]", rest_count, unit_str)
        }
    } else {
        let mut parts: Vec<String> = cast.prefix
            .iter()
            .filter_map(|r| r.map(reg_to_llvm_str))
            .collect();
        for _ in 0..rest_count {
            parts.push(unit_str.clone());
        }
        format!("{{ {} }}", parts.join(", "))
    }
}

fn primitive_to_llvm_str(prim: rustc_abi::Primitive) -> String {
    match prim {
        rustc_abi::Primitive::Int(int, _signed) => format!("i{}", int.size().bits()),
        rustc_abi::Primitive::Float(rustc_abi::Float::F16) => "half".to_string(),
        rustc_abi::Primitive::Float(rustc_abi::Float::F32) => "float".to_string(),
        rustc_abi::Primitive::Float(rustc_abi::Float::F64) => "double".to_string(),
        rustc_abi::Primitive::Float(rustc_abi::Float::F128) => "fp128".to_string(),
        rustc_abi::Primitive::Pointer(_) => "ptr".to_string(),
    }
}

fn reg_to_llvm_str(reg: Reg) -> String {
    match reg.kind {
        RegKind::Integer => format!("i{}", reg.size.bits()),
        RegKind::Float => match reg.size.bits() {
            16 => "half".to_string(),
            32 => "float".to_string(),
            64 => "double".to_string(),
            128 => "fp128".to_string(),
            b => panic!("unknown float size {}", b),
        },
        RegKind::Vector => format!("<{} x i8>", reg.size.bytes()),
    }
}
