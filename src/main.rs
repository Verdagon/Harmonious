#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_session;

mod abi_helpers;
mod callbacks;
mod codegen_wrapper;
mod file_loader;
mod llvm_gen;
mod queries;
mod oracle;
mod stub_gen;
mod toylang;
mod mir_helpers;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use rustc_driver::RunCompiler;
use crate::toylang::registry::ToylangRegistry;

fn main() {
    rustc_driver::install_ice_hook(
        "https://github.com/your-org/toylang/issues",
        |_| {},
    );

    let exit_code = rustc_driver::catch_with_exit_code(|| {
        let mut args: Vec<String> = std::env::args().collect();
        let mut registry = extract_registry(&mut args);

        // Pre-allocate temp paths for LLVM backend output.
        // The actual LLVM IR generation happens in after_analysis (where we have tcx).
        let ll_path = std::env::temp_dir().join("toylang_output.ll");
        let obj_path = std::env::temp_dir().join("toylang_output.o");

        // Mark which functions will be externally compiled (sets external_symbol).
        // This must happen before stub generation so the stubs include extern declarations.
        llvm_gen::mark_compiled_functions(&mut registry);

        let has_external = registry.functions.values().any(|f| f.external_symbol.is_some());
        if has_external {
            // Force multiple CGUs so the partitioner keeps Rust generic symbols
            // with external linkage (needed for cross-CGU references from Toylang .o).
            args.push("-C".to_string());
            args.push("codegen-units=16".to_string());
        }

        let stubs = stub_gen::generate(&registry);
        let registry = Arc::new(registry);
        let mut callbacks = callbacks::ToyCallbacks::new(
            registry,
            stubs,
            if has_external { Some((ll_path, obj_path)) } else { None },
        );
        RunCompiler::new(&args, &mut callbacks).run();
        Ok(())
    });

    std::process::exit(exit_code);
}

fn extract_registry(args: &mut Vec<String>) -> ToylangRegistry {
    if let Some(pos) = args.iter().position(|a| a == "--toylang-input") {
        if pos + 1 < args.len() {
            let path = args[pos + 1].clone();
            args.drain(pos..=pos + 1);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("toylang: cannot read {}: {}", path, e));
            return crate::toylang::parser::parse(&src)
                .unwrap_or_else(|e| panic!("toylang: parse error in {}: {}", path, e));
        }
    }
    ToylangRegistry::hardcoded_point()
}

fn find_sysroot_tool(tool_name: &str) -> PathBuf {
    let sysroot = std::process::Command::new("rustc")
        .arg("+nightly-2025-01-15")
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("failed to run rustc --print sysroot");
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let sysroot = sysroot.trim();

    let host = std::process::Command::new("rustc")
        .arg("+nightly-2025-01-15")
        .arg("-vV")
        .output()
        .expect("failed to run rustc -vV");
    let host_output = String::from_utf8(host.stdout).unwrap();
    let host_triple = host_output.lines()
        .find(|l| l.starts_with("host:"))
        .map(|l| l.trim_start_matches("host:").trim())
        .expect("could not determine host triple");

    PathBuf::from(sysroot)
        .join("lib/rustlib")
        .join(host_triple)
        .join("bin")
        .join(tool_name)
}

/// Assemble LLVM IR text (.ll) to LLVM bitcode (.bc).
pub fn compile_llvm_ir_to_bc(ll_path: &Path, bc_path: &Path) {
    let llvm_as = find_sysroot_tool("llvm-as");
    eprintln!("[toylang] assembling LLVM IR to bitcode: {} → {}", ll_path.display(), bc_path.display());
    let status = std::process::Command::new(&llvm_as)
        .arg("-o").arg(bc_path)
        .arg(ll_path)
        .status()
        .unwrap_or_else(|e| panic!("failed to run llvm-as at {}: {}", llvm_as.display(), e));
    assert!(status.success(), "llvm-as failed with status {}", status);
}

/// Compile LLVM IR text (.ll) to native object code (.o).
/// Uses llc from the rustc sysroot.
pub fn compile_llvm_ir(ll_path: &Path, obj_path: &Path) {
    let sysroot = std::process::Command::new("rustc")
        .arg("+nightly-2025-01-15")
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("failed to run rustc --print sysroot");
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let sysroot = sysroot.trim();

    let host = std::process::Command::new("rustc")
        .arg("+nightly-2025-01-15")
        .arg("-vV")
        .output()
        .expect("failed to run rustc -vV");
    let host_output = String::from_utf8(host.stdout).unwrap();
    let host_triple = host_output.lines()
        .find(|l| l.starts_with("host:"))
        .map(|l| l.trim_start_matches("host:").trim())
        .expect("could not determine host triple");

    let llc = PathBuf::from(sysroot)
        .join("lib/rustlib")
        .join(host_triple)
        .join("bin/llc");

    eprintln!("[toylang] compiling LLVM IR: {} → {}", ll_path.display(), obj_path.display());
    let status = std::process::Command::new(&llc)
        .arg("-filetype=obj")
        .arg("-o")
        .arg(obj_path)
        .arg(ll_path)
        .status()
        .unwrap_or_else(|e| panic!("failed to run llc at {}: {}", llc.display(), e));
    assert!(status.success(), "llc failed with status {}", status);
}
