use crate::manifest::{self, DepSpec, Manifest};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// Per @MRRIWMZ, this is read site 1 of toylang.toml. Wrapper mode re-parses
// the same manifest in main.rs:run_wrapper_mode — any schema change here
// must be kept in sync there.
pub fn build_project(manifest_path: &Path) -> i32 {
    let manifest = match manifest::parse(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("toylangc: {}", e);
            return 1;
        }
    };

    let project_dir = manifest_path
        .parent()
        .map(|p| if p.as_os_str().is_empty() { Path::new(".") } else { p })
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let source_path = project_dir.join(&manifest.project.source);
    if !source_path.exists() {
        eprintln!(
            "toylangc: source file not found: {}",
            source_path.display()
        );
        return 1;
    }

    let build_dir = project_dir.join(".toylang-build");
    if build_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&build_dir) {
            eprintln!("toylangc: cannot clean {}: {}", build_dir.display(), e);
            return 1;
        }
    }

    // Stage 5b: two-crate workspace layout. The stub rlib lives in
    // `.toylang-build/lang_stubs_crate/` as its own package, the user bin
    // lives in `.toylang-build/user_bin/`, and `.toylang-build/Cargo.toml`
    // is the workspace root tying them together. User bin's Cargo.toml
    // path-depends on the stub rlib via `__lang_stubs = { path = "..." }`.
    let stubs_dir = build_dir.join("lang_stubs_crate");
    let user_dir = build_dir.join("user_bin");
    if let Err(e) = fs::create_dir_all(user_dir.join("src")) {
        eprintln!("toylangc: cannot create {}: {}", user_dir.display(), e);
        return 1;
    }
    if let Err(e) = write_workspace_toml(&build_dir) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_stub_crate(&stubs_dir, &source_path, &manifest) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_user_bin_cargo_toml(&user_dir, &manifest) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_main_shim(&user_dir, &manifest) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_toolchain(&build_dir) {
        eprintln!("toylangc: {}", e);
        return 1;
    }

    run_cargo_build(&build_dir)
}

/// Workspace manifest tying the user bin and stub rlib together. Setting a
/// workspace root here prevents cargo from walking up into the user's actual
/// project (where `toylang.toml` lives) looking for a workspace.
fn write_workspace_toml(build_dir: &Path) -> Result<(), String> {
    let s = "[workspace]\n\
             members = [\"lang_stubs_crate\", \"user_bin\"]\n\
             resolver = \"2\"\n";
    fs::write(build_dir.join("Cargo.toml"), s)
        .map_err(|e| format!("cannot write workspace Cargo.toml: {}", e))
}

/// User-bin Cargo.toml. Depends on `__lang_stubs` by path. Re-lists user
/// rust_dependencies so they're directly resolvable in the bin too — the
/// bin shim is trivial today (`fn main() { __toylang_main(); }`) and only
/// needs `__lang_stubs`, but listing the deps keeps cargo's dep graph
/// honest and tolerates future bin-side changes.
fn write_user_bin_cargo_toml(user_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let name = sanitize_name(&manifest.project.name);
    let mut s = String::new();
    s.push_str("[package]\n");
    s.push_str(&format!("name = \"{}\"\n", name));
    s.push_str("version = \"0.0.0\"\n");
    s.push_str(&format!("edition = \"{}\"\n", manifest.project.edition));
    s.push_str("\n[[bin]]\n");
    s.push_str(&format!("name = \"{}\"\n", name));
    s.push_str("path = \"src/main.rs\"\n");
    s.push_str("\n[dependencies]\n");
    s.push_str("__lang_stubs = { path = \"../lang_stubs_crate\" }\n");
    fs::write(user_dir.join("Cargo.toml"), s)
        .map_err(|e| format!("cannot write user_bin/Cargo.toml: {}", e))
}

/// Write the stub rlib package: its `src/lib.rs` is the same content
/// `stub_gen` used to feed FileLoader, and its `Cargo.toml` mirrors the
/// user's rust_dependencies so the rlib's `pub use uuid::Uuid;` etc.
/// resolve directly against crates.io rather than transitively via the
/// user bin.
fn write_stub_crate(
    stubs_dir: &Path,
    source_path: &Path,
    manifest: &Manifest,
) -> Result<(), String> {
    fs::create_dir_all(stubs_dir.join("src"))
        .map_err(|e| format!("cannot create {}: {}", stubs_dir.display(), e))?;

    // Parse the toylang source so we can feed stub_gen. Duplicates the parse
    // that wrapper mode will do later, which is fine — the stub generator is
    // deterministic and cheap.
    let src = fs::read_to_string(source_path).map_err(|e| {
        format!(
            "cannot read toylang source {}: {}",
            source_path.display(),
            e
        )
    })?;
    let registry = crate::toylang::parser::parse(&src).map_err(|e| {
        format!("parse error in {}: {:?}", source_path.display(), e)
    })?;
    let stubs = crate::stub_gen::generate(&registry);

    fs::write(stubs_dir.join("src/lib.rs"), stubs)
        .map_err(|e| format!("cannot write lang_stubs_crate/src/lib.rs: {}", e))?;

    let mut cargo = String::new();
    cargo.push_str("[package]\n");
    cargo.push_str("name = \"__lang_stubs\"\n");
    cargo.push_str("version = \"0.0.0\"\n");
    cargo.push_str(&format!("edition = \"{}\"\n", manifest.project.edition));
    cargo.push_str("\n[lib]\n");
    cargo.push_str("path = \"src/lib.rs\"\n");
    cargo.push_str("crate-type = [\"rlib\"]\n");
    cargo.push_str("\n");
    if !manifest.rust_dependencies.is_empty() {
        cargo.push_str("[dependencies]\n");
        for (name, spec) in &manifest.rust_dependencies {
            cargo.push_str(&format!("{} = {}\n", name, render_dep(spec)));
        }
    }
    fs::write(stubs_dir.join("Cargo.toml"), cargo)
        .map_err(|e| format!("cannot write lang_stubs_crate/Cargo.toml: {}", e))?;

    Ok(())
}

fn sanitize_name(name: &str) -> String {
    name.replace('-', "_")
}

fn render_dep(spec: &DepSpec) -> String {
    match spec {
        DepSpec::Version(v) => format!("\"{}\"", v),
        DepSpec::Detailed {
            version,
            features,
            default_features,
        } => {
            let mut parts = vec![format!("version = \"{}\"", version)];
            if !features.is_empty() {
                let quoted: Vec<String> =
                    features.iter().map(|f| format!("\"{}\"", f)).collect();
                parts.push(format!("features = [{}]", quoted.join(", ")));
            }
            if let Some(df) = default_features {
                parts.push(format!("default-features = {}", df));
            }
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

fn write_main_shim(user_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut s = String::new();
    for feat in &manifest.project.features {
        s.push_str(&format!("#![feature({})]\n", feat));
    }
    if !manifest.project.features.is_empty() {
        s.push_str("\n");
    }
    // Two-crate layout: stubs live in a separate rlib; `__lang_stubs` is an
    // extern crate. Edition 2018+ makes `use` resolve transparently.
    s.push_str("use __lang_stubs::*;\n");
    s.push_str("\n");
    s.push_str("fn main() { __toylang_main(); }\n");

    fs::write(user_dir.join("src/main.rs"), s)
        .map_err(|e| format!("cannot write src/main.rs: {}", e))
}

fn write_toolchain(build_dir: &Path) -> Result<(), String> {
    fs::write(
        build_dir.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"nightly-2025-01-15\"\n",
    )
    .map_err(|e| format!("cannot write rust-toolchain.toml: {}", e))
}

fn sysroot_lib() -> Option<PathBuf> {
    let out = Command::new("rustc")
        .arg("+nightly-2025-01-15")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(s.trim()).join("lib"))
}

fn run_cargo_build(build_dir: &Path) -> i32 {
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("toylangc: cannot determine current_exe: {}", e);
            return 1;
        }
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("+nightly-2025-01-15")
        .arg("build")
        .current_dir(build_dir)
        .env("RUSTC_WORKSPACE_WRAPPER", &self_exe);

    if let Some(lib) = sysroot_lib() {
        // macOS needs DYLD_LIBRARY_PATH to find librustc_driver dylib when
        // cargo spawns the toylangc wrapper.
        cmd.env("DYLD_LIBRARY_PATH", &lib);
        cmd.env("LD_LIBRARY_PATH", &lib);
    }

    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("toylangc: failed to spawn cargo: {}", e);
            1
        }
    }
}
