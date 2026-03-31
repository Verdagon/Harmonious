extern crate rustc_abi;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_target;

use rustc_abi::Size;
use rustc_target::callconv::{Reg, RegKind};
use rustc_hir::def_id::LocalDefId;
use rustc_middle::ty::{self, TyCtxt};
use rustc_target::callconv::{CastTarget, PassMode};

/// Describes how a function's return value should be represented in LLVM IR.
pub enum CoercedReturn {
    /// Return directly in the coerced type (e.g., "i64" for `{ i32, i32 }` on aarch64)
    Direct(String),
    /// Return via sret pointer (large structs, >16 bytes on aarch64)
    Indirect,
    /// No return value (ZST or void)
    Void,
}

/// Query rustc for the ABI-coerced LLVM return type of a function.
pub fn coerced_return_type<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> CoercedReturn {
    let typing_env = ty::TypingEnv::fully_monomorphized();
    let instance = ty::Instance::mono(tcx, def_id.to_def_id());

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
            // ScalarPair — two values. For simplicity, treat as the natural type.
            // This case is rare for extern functions.
            CoercedReturn::Direct(format!("i{}", fn_abi.ret.layout.size.bits()))
        }
        PassMode::Cast { cast, .. } => {
            CoercedReturn::Direct(cast_target_to_llvm_str(cast))
        }
        PassMode::Indirect { .. } => CoercedReturn::Indirect,
    }
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
