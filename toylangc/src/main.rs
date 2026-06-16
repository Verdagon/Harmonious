#![feature(rustc_private)]

extern crate rustc_driver;

mod build;
mod llvm_gen;
mod manifest;
mod oracle;
mod sidecar;
mod stub_gen;
mod toylang;
mod typeid;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::toylang::registry::ToylangRegistry;

/// The pinned nightly rustc toolchain toylangc was built against. Referenced
/// in code sites that spawn rustc or cargo via rustup's `+<pin>` selector,
/// and written into the generated stub crate's `rust-toolchain.toml`. The
/// pin is duplicated in `rust-toolchain.toml` at the repo root (the canonical
/// anchor for `cargo`/`rustc` without a `+pin`) and independently in
/// `tests/integration_projects.rs` and `tests/standalone_tests.rs` because
/// the toylangc crate has no lib target, so integration tests cannot
/// `use toylangc::TOYLANG_NIGHTLY`. When bumping the pin, update all four
/// sites — see `HANDOFF-nightly-bump.md` §3.2.
pub const TOYLANG_NIGHTLY: &str = "rustc-fork";

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
///     (see `is_user_bin_compile`) — the internal-callee walk is skipped
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
        // Per @MRRIWMZ, this is read site 2 of toylang.toml. Build mode parses
        // it first to orchestrate cargo; wrapper mode re-parses it here to
        // locate the .toylang source, using the manifest as a single source of
        // truth instead of an env var side-channel.
        //
        // **Activation gate** (course-correct.md #14): the presence of a
        // `toylang.toml` in the vicinity of `CARGO_MANIFEST_DIR` is the sole
        // signal that this rustc invocation should run toylang's machinery.
        // The prior `CARGO_PRIMARY_PACKAGE=1` gate is retired: it broke for
        // toylang libs depended on by other toylang projects (where cargo
        // doesn't mark the dep "primary"), and it added nothing the
        // manifest lookup doesn't already cover. Per architecture §4.5 the
        // canonical Sky activation signal is the `__SKY_STUBS_MARKER` in
        // the local crate's items, which fires after expansion; the
        // manifest-vicinity check is its pre-expansion analog.
        //
        // Lookup order:
        //   1. CARGO_MANIFEST_DIR itself. Phase 3 E.6: each stub crate dir
        //      contains a project-correct `toylang.toml` copy planted by
        //      `build::write_stub_crate`. Searching here first is what makes
        //      multi-toylang-project builds work — without it, a dep's stub
        //      compile would walk up to the ROOT project's toylang.toml and
        //      mis-parse the registry from the wrong source.
        //   2-3. Ancestors. Backward compat with the pre-E.6 user_bin layout
        //      (`<user-dir>/.toylang-build/user_bin/`) where the toylang.toml
        //      lives two directories up.
        //
        // If nothing is found this isn't a toylang-authored package — pass
        // through to plain rustc.
        let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR");
        let manifest_path = cargo_manifest_dir.ok().and_then(|d| {
            Path::new(&d)
                .ancestors()
                .take(4)
                .map(|dir| dir.join("toylang.toml"))
                .find(|p| p.exists())
        });
        let manifest_path = match manifest_path {
            Some(p) => p,
            None => {
                run_plain_rustc(&argv);
                return;
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
        let is_user_bin_compile = !pkg_name.starts_with("lang_stubs_");
        run_toylang_compile(registry, argv.clone(), is_user_bin_compile);
    });

    std::process::exit(exit_code);
}

/// Toylang compilation path invoked from wrapper mode.
///
/// `is_user_bin_compile` is true for the user-bin compile under two-crate
/// wrapper mode (course-correct.md items #11 + #15, Workstream A). Under
/// the architecture's locked invariant (rust-interop-architecture.md §5.5,
/// §9.6), Sky libraries ship `rlib + sidecar only` and the binary compile
/// codegens every reachable Sky item. For toylang's single-crate-program
/// shape this maps to:
///
/// - **rlib compile (`is_user_bin_compile = false`):** parses `.toylang`
///   source, validates the registry, writes the `.sky-meta` sidecar
///   (S.3), produces NO toylang `.o`. `llvm_paths` is None, so
///   `generate_and_compile` short-circuits.
/// - **user-bin compile (`is_user_bin_compile = true`):** loads the
///   upstream `__lang_stubs` sidecar via S.4, iterates the registry to
///   discover consumer fns (A.4), emits the consumer `.o` that the
///   linker bundles into the binary. `llvm_paths` is allocated here.
fn run_toylang_compile(
    registry: ToylangRegistry,
    mut args: Vec<String>,
    is_user_bin_compile: bool,
) {
    let unique_id = std::process::id();
    let ll_path = std::env::temp_dir().join(format!("toylang_output_{}.ll", unique_id));
    let obj_path = std::env::temp_dir().join(format!("toylang_output_{}.o", unique_id));

    let has_functions = registry.functions.values().any(|f| f.body.is_some());
    if has_functions {
        args.push("-C".to_string());
        args.push("codegen-units=16".to_string());
    }

    // Workstream A inversion (A.1): allocate `llvm_paths` only at the
    // user-bin compile. The rlib compile under A produces no toylang
    // `.o`; the user-bin compile is the codegen site.
    let llvm_paths = if has_functions && is_user_bin_compile {
        Some((ll_path, obj_path))
    } else {
        None
    };

    let toylang_callbacks = toylang::callbacks_impl::ToylangCallbacks {
        registry: Arc::new(registry),
        llvm_paths,
        is_user_bin_compile,
        // Tier 3 #7.4 retired `upstream_fn_names` + `upstream_type_names`
        // — the facade's `SkyUniverse` carries those now.
        upstream_structs: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };

    rustc_lang_facade::driver::run_compiler(toylang_callbacks, &args);
}

struct NoopCallbacks;
impl rustc_driver::Callbacks for NoopCallbacks {}

/// Pass-through: compile as plain rustc with no toylang processing.
/// Used for dependency crates in wrapper mode.
fn run_plain_rustc(args: &[String]) {
    let mut cb = NoopCallbacks;
    rustc_driver::run_compiler(args, &mut cb);
}

pub fn find_sysroot_tool(tool_name: &str) -> PathBuf {
    let plus_pin = format!("+{}", TOYLANG_NIGHTLY);
    let sysroot = std::process::Command::new("rustc")
        .arg(&plus_pin)
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("failed to run rustc --print sysroot");
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let sysroot = sysroot.trim();

    let host = std::process::Command::new("rustc")
        .arg(&plus_pin)
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
