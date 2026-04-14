#![feature(rustc_private)]

extern crate rustc_driver;

mod build;
mod llvm_gen;
mod manifest;
mod oracle;
mod stub_gen;
mod toylang;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::toylang::registry::ToylangRegistry;

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    // Mode 1: `toylangc build [manifest.toml]` — orchestrates cargo
    if argv.get(1).map(|s| s.as_str()) == Some("build") {
        let manifest_path = argv
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("toylang.toml"));
        std::process::exit(build::build_project(&manifest_path));
    }

    // Mode 2: wrapper mode (invoked by cargo as RUSTC_WORKSPACE_WRAPPER).
    // Cargo invokes as: toylangc <rustc-path> <rustc-args...>
    // Detect by checking if argv[1] is a path whose basename is "rustc".
    let is_wrapper = argv.get(1).map_or(false, |s| {
        Path::new(s)
            .file_stem()
            .map_or(false, |stem| stem == "rustc")
    });

    if is_wrapper {
        run_wrapper_mode(argv);
        return;
    }

    // Mode 3: direct mode (existing behavior — --toylang-input in args)
    run_direct_mode(argv);
}

/// Direct mode: toylangc invoked with `--toylang-input <path>` and normal
/// rustc args. Used by integration tests. Unchanged existing behavior.
fn run_direct_mode(argv: Vec<String>) {
    rustc_driver::install_ice_hook(
        "https://github.com/your-org/toylang/issues",
        |_| {},
    );

    let exit_code = rustc_driver::catch_with_exit_code(|| {
        let mut args = argv;
        let registry = extract_registry(&mut args);
        run_toylang_compile(registry, args);
        Ok(())
    });

    std::process::exit(exit_code);
}

/// Wrapper mode: cargo invoked us as RUSTC_WORKSPACE_WRAPPER.
/// argv = [toylangc, <rustc-path>, <rustc-args...>].
/// Drop argv[1] (rustc path) so the remaining args are what rustc expects.
/// If CARGO_PRIMARY_PACKAGE is set, compile through toylang. Otherwise,
/// pass through to plain rustc (so dependency crates build normally).
fn run_wrapper_mode(mut argv: Vec<String>) {
    // argv = [toylangc, <rustc-path>, <args>...]
    // Drop argv[1] so argv = [toylangc, <args>...] which rustc_driver expects
    // (it skips argv[0] internally).
    argv.remove(1);

    rustc_driver::install_ice_hook(
        "https://github.com/your-org/toylang/issues",
        |_| {},
    );

    let exit_code = rustc_driver::catch_with_exit_code(|| {
        let is_primary = std::env::var("CARGO_PRIMARY_PACKAGE").is_ok();

        if !is_primary {
            run_plain_rustc(&argv);
            return Ok(());
        }

        // Per @MRRIWMZ, this is read site 2 of toylang.toml. Build mode parses
        // it first to orchestrate cargo; wrapper mode re-parses it here to
        // locate the .toylang source, using the manifest as a single source of
        // truth instead of an env var side-channel.
        // The generated Cargo project lives at <user-dir>/.toylang-build/,
        // so the user's manifest is one directory up from CARGO_MANIFEST_DIR.
        let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| panic!("wrapper mode: CARGO_MANIFEST_DIR not set"));
        let manifest_path = Path::new(&cargo_manifest_dir)
            .parent()
            .unwrap_or_else(|| panic!("CARGO_MANIFEST_DIR has no parent: {}", cargo_manifest_dir))
            .join("toylang.toml");

        let manifest = manifest::parse(&manifest_path)
            .unwrap_or_else(|e| panic!("wrapper mode: {}", e));
        let source_path = manifest_path
            .parent()
            .unwrap()
            .join(&manifest.project.source);
        let src = std::fs::read_to_string(&source_path).unwrap_or_else(|e| {
            panic!("cannot read toylang source {}: {}", source_path.display(), e)
        });
        let registry = crate::toylang::parser::parse(&src)
            .unwrap_or_else(|e| panic!("parse error in {}: {:?}", source_path.display(), e));

        run_toylang_compile(registry, argv.clone());
        Ok(())
    });

    std::process::exit(exit_code);
}

/// Shared toylang compilation path used by both direct and wrapper modes.
fn run_toylang_compile(registry: ToylangRegistry, mut args: Vec<String>) {
    let unique_id = std::process::id();
    let ll_path = std::env::temp_dir().join(format!("toylang_output_{}.ll", unique_id));
    let obj_path = std::env::temp_dir().join(format!("toylang_output_{}.o", unique_id));

    let has_functions = registry.functions.values().any(|f| f.body.is_some());
    if has_functions {
        args.push("-C".to_string());
        args.push("codegen-units=16".to_string());
    }

    let toylang_callbacks = toylang::callbacks_impl::ToylangCallbacks {
        registry: Arc::new(registry),
        llvm_paths: if has_functions { Some((ll_path, obj_path)) } else { None },
    };

    rustc_lang_facade::driver::run_compiler(toylang_callbacks, &args);
}

struct NoopCallbacks;
impl rustc_driver::Callbacks for NoopCallbacks {}

/// Pass-through: compile as plain rustc with no toylang processing.
/// Used for dependency crates in wrapper mode.
fn run_plain_rustc(args: &[String]) {
    let mut cb = NoopCallbacks;
    rustc_driver::RunCompiler::new(args, &mut cb).run();
}

fn extract_registry(args: &mut Vec<String>) -> ToylangRegistry {
    if let Some(pos) = args.iter().position(|a| a == "--toylang-input") {
        if pos + 1 < args.len() {
            let path = args[pos + 1].clone();
            args.drain(pos..=pos + 1);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("toylang: cannot read {}: {}", path, e));
            return crate::toylang::parser::parse(&src)
                .unwrap_or_else(|e| panic!("toylang: parse error in {}: {:?}", path, e));
        }
    }
    panic!("toylang: missing --toylang-input argument")
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
