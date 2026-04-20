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

    // Mode 1: `toylangc build [manifest.toml]` — orchestrates cargo.
    if argv.get(1).map(|s| s.as_str()) == Some("build") {
        let manifest_path = argv
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("toylang.toml"));
        std::process::exit(build::build_project(&manifest_path));
    }

    // Mode 2: wrapper mode (invoked by cargo as RUSTC_WORKSPACE_WRAPPER).
    // Cargo invokes as: toylangc <rustc-path> <rustc-args...>, detected by
    // argv[1] being a path whose basename is "rustc". The two-crate
    // architecture (stage 5b) drives every toylang compile through this
    // mode; stage 5c.4 retired the former `--toylang-input`-based direct
    // mode along with FileLoader.
    let is_wrapper = argv.get(1).map_or(false, |s| {
        Path::new(s)
            .file_stem()
            .map_or(false, |stem| stem == "rustc")
    });

    if is_wrapper {
        run_wrapper_mode(argv);
        return;
    }

    eprintln!(
        "toylangc: expected `toylangc build [manifest.toml]` or invocation as \
         RUSTC_WORKSPACE_WRAPPER (argv[1] a path to rustc). Got argv = {:?}",
        argv,
    );
    std::process::exit(2);
}

/// Wrapper mode: cargo invoked us as RUSTC_WORKSPACE_WRAPPER.
/// argv = [toylangc, <rustc-path>, <rustc-args...>].
/// Drop argv[1] (rustc path) so the remaining args are what rustc expects.
///
/// Stage 5b: under the two-crate workspace layout, both `__lang_stubs` (the
/// stub rlib) and the user bin are workspace-primary packages. CARGO_PKG_NAME
/// distinguishes them:
///   - `__lang_stubs`: full toylang compile. Consumer `.o` is generated here
///     and bundled into the rlib via the codegen wrapper. The internal-callee
///     walk + `generate_and_compile` fire on this side.
///   - user bin: full toylang compile but with the downstream-of-stubs gating
///     (see `is_downstream_of_stubs`) — the internal-callee walk is skipped
///     because callees are already in the rlib's `.o`, and
///     `generate_and_compile` returns None so no duplicate `.o` is injected.
///     Facade overrides remain installed so `upstream_monomorphizations_for`
///     can route generic consumer wrappers (e.g. `__toylang_option_unwrap<T>`)
///     to local user-bin codegen.
///   - non-primary deps: plain rustc, as before.
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
        //
        // Two-crate layout: the user bin is at `<user-dir>/.toylang-build/user_bin/`
        // and the stub rlib is at `<user-dir>/.toylang-build/lang_stubs_crate/`,
        // so `toylang.toml` is two directories up from CARGO_MANIFEST_DIR. Walk
        // up looking for it; if not found this isn't a toylang-authored package
        // (e.g., an auxiliary crate cargo happens to have set primary on) — pass
        // through to plain rustc rather than panicking on manifest parse.
        let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| panic!("wrapper mode: CARGO_MANIFEST_DIR not set"));
        let manifest_path = Path::new(&cargo_manifest_dir)
            .ancestors()
            .skip(1)
            .take(3)
            .map(|d| d.join("toylang.toml"))
            .find(|p| p.exists());
        let manifest_path = match manifest_path {
            Some(p) => p,
            None => {
                run_plain_rustc(&argv);
                return Ok(());
            }
        };

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

        // Detect downstream-of-stubs (user bin) vs the rlib compile.
        //
        // The stub rlib's cargo package is named `lang_stubs_<project>`
        // (per-project unique to bust cargo's cross-project dedup under a
        // shared CARGO_TARGET_DIR — see `build::write_stub_crate`'s package-
        // name comment). The rust-level crate name is fixed at `__lang_stubs`
        // via `[lib].name`. CARGO_PKG_NAME reports the cargo PACKAGE name,
        // not the lib name, so we match on the `lang_stubs_` prefix rather
        // than the literal `__lang_stubs`.
        let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let is_downstream = !pkg_name.starts_with("lang_stubs_");
        run_toylang_compile(registry, argv.clone(), is_downstream);
        Ok(())
    });

    std::process::exit(exit_code);
}

/// Toylang compilation path invoked from wrapper mode.
///
/// `is_downstream_of_stubs` is true for the user-bin compile in stage-5
/// two-crate wrapper mode. In that mode the stub rlib has already produced
/// the consumer `.o` (linked into the rlib) and walked all internal callees;
/// the user-bin compile only needs facade overrides installed so cross-crate
/// queries (`upstream_monomorphizations_for`, `symbol_name`-via-metadata,
/// etc.) route correctly. Allocating LLVM paths in this mode would lead to
/// duplicate consumer codegen and a colliding `.o`; pass `None` instead so
/// `generate_and_compile` short-circuits.
fn run_toylang_compile(
    registry: ToylangRegistry,
    mut args: Vec<String>,
    is_downstream_of_stubs: bool,
) {
    let unique_id = std::process::id();
    let ll_path = std::env::temp_dir().join(format!("toylang_output_{}.ll", unique_id));
    let obj_path = std::env::temp_dir().join(format!("toylang_output_{}.o", unique_id));

    let has_functions = registry.functions.values().any(|f| f.body.is_some());
    if has_functions {
        args.push("-C".to_string());
        args.push("codegen-units=16".to_string());
    }

    let llvm_paths = if has_functions && !is_downstream_of_stubs {
        Some((ll_path, obj_path))
    } else {
        None
    };

    let toylang_callbacks = toylang::callbacks_impl::ToylangCallbacks {
        registry: Arc::new(registry),
        llvm_paths,
        is_downstream_of_stubs,
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
