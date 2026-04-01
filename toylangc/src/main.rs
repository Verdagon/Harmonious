#![feature(rustc_private)]

extern crate rustc_driver;

mod llvm_gen;
mod oracle;
mod stub_gen;
mod toylang;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::toylang::registry::ToylangRegistry;

fn main() {
    rustc_driver::install_ice_hook(
        "https://github.com/your-org/toylang/issues",
        |_| {},
    );

    let exit_code = rustc_driver::catch_with_exit_code(|| {
        let mut args: Vec<String> = std::env::args().collect();
        let mut registry = extract_registry(&mut args);

        let ll_path = std::env::temp_dir().join("toylang_output.ll");
        let obj_path = std::env::temp_dir().join("toylang_output.o");

        llvm_gen::mark_compiled_functions(&mut registry);

        let has_external = registry.functions.values().any(|f| f.external_symbol.is_some());
        if has_external {
            args.push("-C".to_string());
            args.push("codegen-units=16".to_string());
        }

        let toylang_callbacks = toylang::callbacks_impl::ToylangCallbacks {
            registry: Arc::new(registry),
            llvm_paths: if has_external { Some((ll_path, obj_path)) } else { None },
        };

        rustc_lang_facade::driver::run_compiler(toylang_callbacks, &args);
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
    ToylangRegistry {
        structs: Default::default(),
        functions: Default::default(),
    }
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

/// Compile LLVM IR text (.ll) to native object code (.o).
pub fn compile_llvm_ir(ll_path: &Path, obj_path: &Path) {
    let llc = find_sysroot_tool("llc");
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
