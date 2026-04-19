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
    if let Err(e) = fs::create_dir_all(build_dir.join("src")) {
        eprintln!("toylangc: cannot create {}: {}", build_dir.display(), e);
        return 1;
    }

    if let Err(e) = write_cargo_toml(&build_dir, &manifest) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_main_shim(&build_dir, &manifest) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_toolchain(&build_dir) {
        eprintln!("toylangc: {}", e);
        return 1;
    }

    run_cargo_build(&build_dir)
}

fn write_cargo_toml(build_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut s = String::new();
    s.push_str("[package]\n");
    s.push_str(&format!("name = \"{}\"\n", sanitize_name(&manifest.project.name)));
    s.push_str("version = \"0.0.0\"\n");
    s.push_str(&format!("edition = \"{}\"\n", manifest.project.edition));
    s.push_str("\n");
    s.push_str("[[bin]]\n");
    s.push_str(&format!("name = \"{}\"\n", sanitize_name(&manifest.project.name)));
    s.push_str("path = \"src/main.rs\"\n");
    s.push_str("\n");

    if !manifest.rust_dependencies.is_empty() {
        s.push_str("[dependencies]\n");
        for (name, spec) in &manifest.rust_dependencies {
            s.push_str(&format!("{} = {}\n", name, render_dep(spec)));
        }
    }

    // Mark the generated project as its own workspace root. Otherwise, if
    // the user's project sits inside another cargo workspace (common for
    // our own tests at toylangc/tests/standalone/*), cargo walks up, finds
    // the parent [workspace] table, and errors with "current package
    // believes it's in a workspace when it's not."
    s.push_str("\n[workspace]\n");

    fs::write(build_dir.join("Cargo.toml"), s)
        .map_err(|e| format!("cannot write Cargo.toml: {}", e))
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

fn write_main_shim(build_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut s = String::new();
    for feat in &manifest.project.features {
        s.push_str(&format!("#![feature({})]\n", feat));
    }
    if !manifest.project.features.is_empty() {
        s.push_str("\n");
    }
    // `mod __lang_stubs;` must come before `use __lang_stubs::*;`.
    // The facade's LangFileLoader intercepts `__lang_stubs.rs` by filename,
    // serving virtual stubs instead of reading from disk.
    s.push_str("mod __lang_stubs;\n");
    s.push_str("use __lang_stubs::*;\n");
    s.push_str("\n");
    s.push_str("fn main() { __toylang_main(); }\n");

    fs::write(build_dir.join("src/main.rs"), s)
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
