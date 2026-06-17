use crate::manifest::{self, DepSpec, Manifest, ResolvedProject};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// Per @MRRIWMZ, this is read site 1 of toylang.toml. Wrapper mode re-parses
// the same manifest in main.rs:run_wrapper_mode — any schema change here
// must be kept in sync there.
pub fn build_project(manifest_path: &Path) -> i32 {
    // Phase 3 E.3: resolve the toylang dep graph (root last). For a
    // dep-less project this is a single-entry vector and the build
    // behaves identically to the pre-E.3 single-project flow. With
    // `[toylang-dependencies]` populated, each transitive dep becomes
    // its own workspace member with its own stub rlib.
    let graph = match manifest::resolve_dep_graph(manifest_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("toylangc: {}", e);
            return 1;
        }
    };
    let root = graph.last().expect("dep graph is empty");
    let project_dir = root.project_dir.clone();
    let manifest = &root.manifest;

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

    // Stage 5b + E.3: two-crate-per-project workspace layout. The ROOT
    // project's stub rlib lives in `.toylang-build/lang_stubs_crate/`
    // (legacy name preserved for the existing 229 tests; the user_bin
    // shim uses `use __lang_stubs::*;` and the lib name is fixed at
    // `__lang_stubs`). Each toylang DEP gets its own stub rlib at
    // `.toylang-build/lang_stubs_<dep_name>/` with its declared name.
    // The user_bin lives in `.toylang-build/user_bin/`.
    let user_dir = build_dir.join("user_bin");
    if let Err(e) = fs::create_dir_all(user_dir.join("src")) {
        eprintln!("toylangc: cannot create {}: {}", user_dir.display(), e);
        return 1;
    }

    // Build the stub crate dir name + crate-name resolution for every project
    // in the graph. The root's stub crate keeps the legacy `__lang_stubs`
    // crate name; deps use their declared project name.
    let n = graph.len();
    let mut stub_dirs: Vec<PathBuf> = Vec::with_capacity(n);
    let mut stub_dir_names: Vec<String> = Vec::with_capacity(n);
    let mut crate_names: Vec<String> = Vec::with_capacity(n);
    let mut pkg_names: Vec<String> = Vec::with_capacity(n);
    for (i, proj) in graph.iter().enumerate() {
        let sanitized = sanitize_name(&proj.manifest.project.name);
        let is_root = i + 1 == n;
        let dir_name = if is_root {
            "lang_stubs_crate".to_string()
        } else {
            format!("lang_stubs_{}", sanitized)
        };
        stub_dirs.push(build_dir.join(&dir_name));
        stub_dir_names.push(dir_name);
        crate_names.push(if is_root { "__lang_stubs".to_string() } else { sanitized.clone() });
        pkg_names.push(format!("lang_stubs_{}", sanitized));
    }
    // Map from project name → index for path-dep wiring.
    let name_index: std::collections::BTreeMap<String, usize> = graph
        .iter()
        .enumerate()
        .map(|(i, p)| (p.manifest.project.name.clone(), i))
        .collect();

    if let Err(e) = write_workspace_toml(&build_dir, &stub_dir_names) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    for (i, proj) in graph.iter().enumerate() {
        if let Err(e) = write_stub_crate(
            &stub_dirs[i],
            proj,
            &crate_names[i],
            &pkg_names[i],
            &name_index,
            &stub_dirs,
            &crate_names,
            &pkg_names,
        ) {
            eprintln!("toylangc: {}", e);
            return 1;
        }
    }
    if let Err(e) = write_user_bin_cargo_toml(
        &user_dir,
        &project_dir,
        manifest,
        &root.toylang_dep_names,
        &name_index,
        &stub_dirs,
        &crate_names,
        &pkg_names,
    ) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_main_shim(
        &user_dir,
        &project_dir,
        manifest,
        &root.toylang_dep_names,
        &name_index,
        &crate_names,
    ) {
        eprintln!("toylangc: {}", e);
        return 1;
    }
    if let Err(e) = write_toolchain(&build_dir) {
        eprintln!("toylangc: {}", e);
        return 1;
    }

    run_cargo_build(&build_dir)
}

/// Workspace manifest tying the user bin and each project's stub rlib
/// together. Setting a workspace root here prevents cargo from walking up
/// into the user's actual project (where `toylang.toml` lives) looking
/// for a workspace.
///
/// `n_projects` is the count of projects in the resolved dep graph (incl.
/// the root). The root's stub crate is at `lang_stubs_crate`, each dep's
/// at `lang_stubs_<dep_name>` — see `build_project` for the mapping.
fn write_workspace_toml(build_dir: &Path, stub_dir_names: &[String]) -> Result<(), String> {
    let mut members: Vec<&str> = stub_dir_names.iter().map(|s| s.as_str()).collect();
    members.push("user_bin");
    let mut s = String::new();
    s.push_str("[workspace]\n");
    s.push_str("members = [");
    for (i, m) in members.iter().enumerate() {
        if i > 0 { s.push_str(", "); }
        s.push_str(&format!("\"{}\"", m));
    }
    s.push_str("]\n");
    s.push_str("resolver = \"2\"\n");
    fs::write(build_dir.join("Cargo.toml"), s)
        .map_err(|e| format!("cannot write workspace Cargo.toml: {}", e))
}

/// User-bin Cargo.toml. Depends on `__lang_stubs` by path AND re-lists
/// every user rust_dependencies entry directly.
///
/// Why re-list (post-Workstream A): under the binary-codegen model
/// (course-correct #11 + #15), toylang's emitted `.o` lives at the
/// user-bin compile, not bundled into the rlib. The undefined-symbol
/// concern from the pre-Workstream-A era is gone (the rlib no longer
/// references rust_deps symbols).
///
/// What remains load-bearing: rust_caller (Phase 1 D fixtures for cases
/// 1a/1b/3/5) writes Rust source compiled inside user_bin that names
/// the rust_dependencies directly (`use serde::...`, etc.), so cargo
/// must pass `--extern serde=...` to the user_bin's rustc. That happens
/// only when serde is a direct cargo dep of user_bin — the rlib's
/// transitive dep doesn't create the `--extern` flag.
///
/// Without the direct re-listing, user_bin's compile would fail with
/// "unresolved import `serde`" before linking ever ran.
fn write_user_bin_cargo_toml(
    user_dir: &Path,
    project_dir: &Path,
    manifest: &Manifest,
    root_toylang_dep_names: &[String],
    name_index: &std::collections::BTreeMap<String, usize>,
    all_stub_dirs: &[PathBuf],
    all_crate_names: &[String],
    all_pkg_names: &[String],
) -> Result<(), String> {
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
    s.push_str(&format!(
        "__lang_stubs = {{ package = \"lang_stubs_{}\", path = \"../lang_stubs_crate\" }}\n",
        name,
    ));
    for (dep_name, spec) in &manifest.rust_dependencies {
        s.push_str(&format!("{} = {}\n", dep_name, render_dep(spec, project_dir)));
    }
    // Phase 3 E.6: declare each toylang dep as a DIRECT cargo dep of the
    // user_bin (not just transitive via the root's stub rlib). Direct dep
    // makes cargo pass `--extern <crate> = <rlib>` so rustc can load the
    // crate when main.rs's `extern crate <crate> as _;` references it.
    // Without this declaration the user_bin's compile wouldn't have the
    // dep's `.sky-meta` reachable for S.4's sidecar walk, and codegen
    // would have an empty `upstream_registries` at user-bin time.
    for dep_name in root_toylang_dep_names {
        let idx = name_index[dep_name];
        let dep_dir_name = all_stub_dirs[idx]
            .file_name()
            .expect("stub dir has filename")
            .to_string_lossy();
        s.push_str(&format!(
            "{} = {{ package = \"{}\", path = \"../{}\" }}\n",
            all_crate_names[idx], all_pkg_names[idx], dep_dir_name
        ));
    }
    fs::write(user_dir.join("Cargo.toml"), s)
        .map_err(|e| format!("cannot write user_bin/Cargo.toml: {}", e))
}

/// Write a single project's stub rlib package: its `src/lib.rs` is what
/// `stub_gen` produces, its `Cargo.toml` mirrors the project's
/// rust_dependencies AND path-depends on each toylang dependency's stub
/// rlib (so cross-toylang-lib `import` statements resolve at typecheck
/// time and the dep's `.sky-meta` sidecar is reachable via S.4's
/// `tcx.crates(())` walk).
///
/// `crate_name`: the Rust-level `[lib].name` (the root keeps the legacy
/// `__lang_stubs` for the user_bin shim's hardcoded `use __lang_stubs::*;`;
/// each dep uses its declared project name).
///
/// `pkg_name`: the Cargo `[package].name`, always `lang_stubs_<sanitized>`
/// so the shared CARGO_TARGET_DIR doesn't dedupe two projects' stub rlibs
/// into one.
fn write_stub_crate(
    stubs_dir: &Path,
    proj: &ResolvedProject,
    crate_name: &str,
    pkg_name: &str,
    name_index: &std::collections::BTreeMap<String, usize>,
    all_stub_dirs: &[PathBuf],
    all_crate_names: &[String],
    all_pkg_names: &[String],
) -> Result<(), String> {
    fs::create_dir_all(stubs_dir.join("src"))
        .map_err(|e| format!("cannot create {}: {}", stubs_dir.display(), e))?;

    let source_path = proj.project_dir.join(&proj.manifest.project.source);
    let manifest = &proj.manifest;

    // Parse the toylang source so we can feed stub_gen. Duplicates the parse
    // that wrapper mode will do later, which is fine — the stub generator is
    // deterministic and cheap.
    let src = fs::read_to_string(&source_path).map_err(|e| {
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

    let mut stubs_with_features = String::new();
    // Path B / Phase 4.5 touch point 5: exclude the stub rlib from ThinLTO's
    // IR linker pool. Without this, LTO sees the stub rlib's `unreachable!()`
    // bodies as candidate definitions for the rustc-mangled consumer symbols
    // alongside Sky's real bodies (contributed via patch (c) at the user_bin
    // compile), and the IR linker non-deterministically picks the wrong one —
    // user reports `arithmetic` panicking with `unreachable!()` under
    // `lto = "thin"`. `#![no_builtins]` is rustc's canonical per-crate LTO
    // exclusion mechanism (the same one `compiler_builtins` uses): the stub
    // rlib's `.rcgu.o`s still link normally (so the rlib still serves its
    // typecheck role) but its bitcode never enters the LTO module pool.
    // Cross-language inlining is unaffected — Sky's bodies live in user_bin's
    // bitcode and Rust deps' bodies live in their own rlibs; both participate
    // in LTO independently.
    stubs_with_features.push_str("#![no_builtins]\n\n");
    for feat in &manifest.project.features {
        stubs_with_features.push_str(&format!("#![feature({})]\n", feat));
    }
    if !manifest.project.features.is_empty() {
        stubs_with_features.push('\n');
    }
    // Phase 3 E.6: force-link each toylang dep so rustc actually LOADS its
    // crate metadata during this rlib compile. Without this, cargo's
    // `--extern case6_lib=...` lists the rlib but rustc only loads it if
    // referenced — and stub_gen's output doesn't reference dep items by
    // their Rust crate path. Loading is what makes the dep show up in
    // `tcx.crates(())`, which is what S.4's sidecar walker iterates, which
    // is what populates `upstream_registries` for E.5's typechecker merge.
    for dep_name in &proj.toylang_dep_names {
        let idx = name_index[dep_name];
        stubs_with_features.push_str(&format!(
            "extern crate {} as _;\n",
            all_crate_names[idx]
        ));
    }
    if !proj.toylang_dep_names.is_empty() {
        stubs_with_features.push('\n');
    }
    stubs_with_features.push_str(&stubs);

    fs::write(stubs_dir.join("src/lib.rs"), stubs_with_features)
        .map_err(|e| format!("cannot write {}/src/lib.rs: {}", stubs_dir.display(), e))?;

    let mut cargo = String::new();
    cargo.push_str("[package]\n");
    cargo.push_str(&format!("name = \"{}\"\n", pkg_name));
    cargo.push_str("version = \"0.0.0\"\n");
    cargo.push_str(&format!("edition = \"{}\"\n", manifest.project.edition));
    cargo.push_str("\n[lib]\n");
    cargo.push_str(&format!("name = \"{}\"\n", crate_name));
    cargo.push_str("path = \"src/lib.rs\"\n");
    cargo.push_str("crate-type = [\"rlib\"]\n");

    let has_rust_deps = !manifest.rust_dependencies.is_empty();
    let has_toylang_deps = !proj.toylang_dep_names.is_empty();
    if has_rust_deps || has_toylang_deps {
        cargo.push_str("\n[dependencies]\n");
        for (name, spec) in &manifest.rust_dependencies {
            cargo.push_str(&format!(
                "{} = {}\n",
                name,
                render_dep(spec, &proj.project_dir)
            ));
        }
        // Path-deps to each toylang dependency's stub rlib. The dep's
        // stub crate lives at a sibling `../<dep_dir_name>` workspace
        // member; alias as `<crate_name> = { package = "<pkg_name>",
        // path = "..." }` so the rust-level name is preserved.
        for dep_name in &proj.toylang_dep_names {
            let idx = name_index[dep_name];
            let dep_dir_name = all_stub_dirs[idx]
                .file_name()
                .expect("stub dir has filename")
                .to_string_lossy();
            cargo.push_str(&format!(
                "{} = {{ package = \"{}\", path = \"../{}\" }}\n",
                all_crate_names[idx], all_pkg_names[idx], dep_dir_name
            ));
        }
    }
    fs::write(stubs_dir.join("Cargo.toml"), cargo)
        .map_err(|e| format!("cannot write {}/Cargo.toml: {}", stubs_dir.display(), e))?;

    // Phase 3 E.6: copy the project's `toylang.toml` into the stub crate's
    // own directory. Wrapper mode's manifest lookup (see `main::run_wrapper_mode`)
    // searches starting from CARGO_MANIFEST_DIR. Without this copy, a dep's
    // stub-rlib compile would walk up the directory tree and incorrectly
    // find the ROOT project's toylang.toml — producing a registry built
    // from the wrong source file. Copying the dep's toylang.toml here makes
    // the lookup project-correct without an env-var side-channel.
    //
    // The copy uses `manifest.project.source = "main.toylang"` style with a
    // path relative to the stub crate dir. We embed an absolute path so the
    // wrapper's manifest re-parse can locate the actual toylang source
    // regardless of where the stub crate dir lives.
    let mut copied = String::new();
    copied.push_str("[project]\n");
    copied.push_str(&format!("name = \"{}\"\n", manifest.project.name));
    let abs_source =
        std::fs::canonicalize(&source_path).unwrap_or_else(|_| source_path.clone());
    copied.push_str(&format!("source = {:?}\n", abs_source.display().to_string()));
    copied.push_str(&format!("edition = \"{}\"\n", manifest.project.edition));
    if !manifest.project.features.is_empty() {
        copied.push_str("features = [");
        for (i, f) in manifest.project.features.iter().enumerate() {
            if i > 0 { copied.push_str(", "); }
            copied.push_str(&format!("\"{}\"", f));
        }
        copied.push_str("]\n");
    }
    // rust-dependencies and toylang-dependencies are NOT copied — the
    // wrapper only needs the source file's location to re-parse the
    // registry; build-mode owns dependency wiring.
    fs::write(stubs_dir.join("toylang.toml"), copied)
        .map_err(|e| format!("cannot write {}/toylang.toml: {}", stubs_dir.display(), e))?;

    Ok(())
}

fn sanitize_name(name: &str) -> String {
    name.replace('-', "_")
}

fn render_dep(spec: &DepSpec, project_dir: &Path) -> String {
    match spec {
        DepSpec::Version(v) => format!("\"{}\"", v),
        DepSpec::Path { path } => {
            // Resolve relative to the toylang.toml's directory, then emit as
            // an absolute path. The generated stub rlib's Cargo.toml lives
            // at `<project>/.toylang-build/lang_stubs_crate/Cargo.toml`,
            // two directories below `<project>`, so a literal pass-through
            // would require the user to count those `..`s. Resolving here
            // keeps the toylang.toml relative to the user's source tree.
            // Absolute paths also let multiple integration projects share
            // the same `test_helpers` cargo cache entry — cargo dedupes by
            // resolved path.
            let resolved = if std::path::Path::new(path).is_absolute() {
                std::path::PathBuf::from(path)
            } else {
                project_dir.join(path)
            };
            // canonicalize() resolves `..` segments; if the path doesn't yet
            // exist we still want the `../foo` form normalized so cargo's
            // dedup works. Fall back to the raw join for missing paths and
            // let cargo surface the error.
            let final_path = std::fs::canonicalize(&resolved).unwrap_or(resolved);
            format!("{{ path = {:?} }}", final_path.display().to_string())
        }
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

fn write_main_shim(
    user_dir: &Path,
    project_dir: &Path,
    manifest: &Manifest,
    root_toylang_dep_names: &[String],
    name_index: &std::collections::BTreeMap<String, usize>,
    all_crate_names: &[String],
) -> Result<(), String> {
    let mut s = String::new();
    for feat in &manifest.project.features {
        s.push_str(&format!("#![feature({})]\n", feat));
    }
    if !manifest.project.features.is_empty() {
        s.push_str("\n");
    }
    s.push_str("use __lang_stubs::*;\n");
    for name in manifest.rust_dependencies.keys() {
        s.push_str(&format!("extern crate {} as _;\n", name));
    }
    for dep_name in root_toylang_dep_names {
        let idx = name_index[dep_name];
        s.push_str(&format!("extern crate {} as _;\n", all_crate_names[idx]));
    }
    s.push_str("\n");

    // Phase 1 D: if `project.rust_caller` is set, append the file's contents
    // (which must define its own `fn main`) instead of the default toylang
    // shim. Exercises Case 1a/1b/3/5 of the seven-case taxonomy where the
    // binary's top-level is Rust source. The toylang source can still define
    // `fn main` if it wants — `__toylang_main` is emitted but never called.
    if let Some(rel) = manifest.project.rust_caller.as_ref() {
        let caller_path = project_dir.join(rel);
        let content = fs::read_to_string(&caller_path).map_err(|e| {
            format!("cannot read rust_caller {}: {}", caller_path.display(), e)
        })?;
        s.push_str(&content);
        if !content.ends_with('\n') {
            s.push('\n');
        }
    } else {
        s.push_str("fn main() { __toylang_main(); }\n");
    }

    fs::write(user_dir.join("src/main.rs"), s)
        .map_err(|e| format!("cannot write src/main.rs: {}", e))
}

fn write_toolchain(build_dir: &Path) -> Result<(), String> {
    fs::write(
        build_dir.join("rust-toolchain.toml"),
        format!("[toolchain]\nchannel = \"{}\"\n", crate::TOYLANG_NIGHTLY),
    )
    .map_err(|e| format!("cannot write rust-toolchain.toml: {}", e))
}

fn sysroot_lib() -> Option<PathBuf> {
    let out = Command::new("rustc")
        .arg(format!("+{}", crate::TOYLANG_NIGHTLY))
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
    cmd.arg(format!("+{}", crate::TOYLANG_NIGHTLY))
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
