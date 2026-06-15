//! `toylang.toml` parser.
//!
//! Per @MRRIWMZ, this file is parsed twice per build: once by `toylangc
//! build` orchestrating cargo (via `build::build_project`), and once by
//! the wrapper-mode child process cargo spawns for the primary crate
//! (via `main::run_wrapper_mode`). Both sites call `parse` below so any
//! schema change takes effect on both automatically. Schema changes that
//! affect the path-resolution semantics (e.g., `[project].source`'s
//! relative-vs-absolute interpretation) must keep both call sites in
//! sync — the arcana documents the side-channel invariant.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default, rename = "rust-dependencies")]
    pub rust_dependencies: BTreeMap<String, DepSpec>,
    /// Other toylang projects this project depends on. Phase 3 E.2:
    /// path-style only (no version, no crates.io) per architecture §6.1's
    /// per-Sky-library stub rlib model. The key is the dep's short name
    /// (used both as the Rust crate name and as the stub rlib name);
    /// the path points at another toylang.toml. E.3 wires the build
    /// orchestration to fan out per-dep stub_gen + per-dep cargo workspace
    /// members; until E.3 lands, this field parses but the build script
    /// rejects non-empty entries.
    #[serde(default, rename = "toylang-dependencies")]
    pub toylang_dependencies: BTreeMap<String, ToylangDepSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    pub source: String,
    #[serde(default = "default_edition")]
    pub edition: String,
    #[serde(default)]
    pub features: Vec<String>,
    /// Phase 1 D: optional path (relative to the project dir) to a Rust
    /// source file that supplies the binary's `fn main`. When set, the
    /// user_bin's `src/main.rs` is composed by prepending the standard
    /// `use __lang_stubs::*;` + force-link `extern crate` preamble and then
    /// appending the rust_caller file's contents — replacing the default
    /// `fn main() { __toylang_main(); }` shim entirely. Used to exercise
    /// Cases 1a/1b/3/5 of the seven-case taxonomy where the binary's
    /// top-level is Rust source rather than toylang's `main`.
    #[serde(default)]
    pub rust_caller: Option<String>,
}

fn default_edition() -> String {
    "2021".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DepSpec {
    Version(String),
    /// Path dep — `{ path = "../foo" }`. The path is interpreted relative
    /// to the toylang.toml file's directory; build.rs resolves it to an
    /// absolute path before writing it into the generated stub rlib's
    /// Cargo.toml. Stage 5c integration tests use this to pull in the
    /// shared `test_helpers` crate.
    Path {
        path: String,
    },
    Detailed {
        version: String,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default, rename = "default-features")]
        default_features: Option<bool>,
    },
}

/// Toylang-on-toylang dependency. Path-style only in v1. The path is
/// interpreted relative to the toylang.toml's directory and must point
/// at another toylang.toml (or its containing directory). E.3 resolves
/// to absolute paths during build-graph construction.
#[derive(Debug, Clone, Deserialize)]
pub struct ToylangDepSpec {
    pub path: String,
}

pub fn parse(path: &Path) -> Result<Manifest, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read manifest {}: {}", path.display(), e))?;
    parse_str(&contents)
        .map_err(|e| format!("invalid toml in {}: {}", path.display(), e))
}

fn parse_str(s: &str) -> Result<Manifest, toml::de::Error> {
    toml::from_str(s)
}

/// A resolved toylang project: the manifest plus its on-disk locations.
/// `manifest_path` is canonicalized so two paths pointing at the same project
/// dedupe by identity.
#[derive(Debug, Clone)]
pub struct ResolvedProject {
    pub manifest: Manifest,
    /// Absolute path to the toylang.toml file. Kept for diagnostics + future
    /// callers (E.4/E.5/E.6) even when the build orchestration alone doesn't
    /// need it.
    #[allow(dead_code)]
    pub manifest_path: PathBuf,
    /// Absolute path to the project directory (containing the toylang.toml).
    pub project_dir: PathBuf,
    /// Names of this project's direct toylang dependencies, in BTreeMap iteration
    /// order (deterministic). These names match keys into the overall
    /// dependency graph.
    pub toylang_dep_names: Vec<String>,
}

/// Resolve the full transitive toylang dependency graph rooted at `root_path`.
///
/// The result is **topologically sorted**: dependencies appear before their
/// dependents. The root is the last entry. Each entry's `toylang_dep_names`
/// references earlier entries by their `manifest.project.name`.
///
/// Cycles in the dep graph produce a clear error. Path resolution is relative
/// to each manifest's own directory, then canonicalized so different
/// path forms (`../my_utils` vs `./../my_utils`) collapse to one node.
///
/// Name collisions across distinct paths produce an error — two different
/// projects cannot both declare `name = "my_utils"`.
pub fn resolve_dep_graph(root_path: &Path) -> Result<Vec<ResolvedProject>, String> {
    use std::collections::BTreeMap;

    // Canonicalize the root once. All comparisons use canonical paths.
    let root_path = canonicalize(root_path)?;

    // DFS state: order is the postorder; visiting is the active stack
    // (cycle detection); resolved maps canonical path → ResolvedProject.
    let mut order: Vec<PathBuf> = Vec::new();
    let mut visiting: Vec<PathBuf> = Vec::new();
    let mut resolved: BTreeMap<PathBuf, ResolvedProject> = BTreeMap::new();

    visit(&root_path, &mut order, &mut visiting, &mut resolved)?;

    // Name-collision check across the resolved graph.
    let mut seen_names: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in &order {
        let proj = &resolved[path];
        let name = proj.manifest.project.name.clone();
        if let Some(prev) = seen_names.insert(name.clone(), path.clone()) {
            return Err(format!(
                "two distinct toylang projects share the name '{}': {} and {}",
                name,
                prev.display(),
                path.display()
            ));
        }
    }

    Ok(order.into_iter().map(|p| resolved.remove(&p).unwrap()).collect())
}

fn visit(
    path: &Path,
    order: &mut Vec<PathBuf>,
    visiting: &mut Vec<PathBuf>,
    resolved: &mut std::collections::BTreeMap<PathBuf, ResolvedProject>,
) -> Result<(), String> {
    if resolved.contains_key(path) {
        return Ok(());
    }
    if visiting.iter().any(|p| p == path) {
        let mut chain: Vec<String> =
            visiting.iter().map(|p| p.display().to_string()).collect();
        chain.push(path.display().to_string());
        return Err(format!("toylang dependency cycle: {}", chain.join(" -> ")));
    }
    visiting.push(path.to_path_buf());

    let manifest = parse(path)?;
    let project_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut toylang_dep_names: Vec<String> = Vec::new();

    // Recurse into each toylang dep.
    for (dep_name, spec) in &manifest.toylang_dependencies {
        let raw = if Path::new(&spec.path).is_absolute() {
            PathBuf::from(&spec.path)
        } else {
            project_dir.join(&spec.path)
        };
        let dep_manifest_path = if raw.is_dir() {
            raw.join("toylang.toml")
        } else {
            raw
        };
        let dep_manifest_path = canonicalize(&dep_manifest_path)?;
        visit(&dep_manifest_path, order, visiting, resolved)?;
        let dep_proj = &resolved[&dep_manifest_path];
        if &dep_proj.manifest.project.name != dep_name {
            return Err(format!(
                "toylang-dependency key '{}' does not match its [project].name '{}' \
                 at {}",
                dep_name,
                dep_proj.manifest.project.name,
                dep_manifest_path.display()
            ));
        }
        toylang_dep_names.push(dep_name.clone());
    }

    visiting.pop();
    let resolved_proj = ResolvedProject {
        manifest,
        manifest_path: path.to_path_buf(),
        project_dir,
        toylang_dep_names,
    };
    resolved.insert(path.to_path_buf(), resolved_proj);
    order.push(path.to_path_buf());
    Ok(())
}

fn canonicalize(p: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(p)
        .map_err(|e| format!("cannot canonicalize {}: {}", p.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
"#,
        )
        .unwrap();
        assert_eq!(m.project.name, "app");
        assert_eq!(m.project.source, "main.toylang");
        assert_eq!(m.project.edition, "2021");
        assert!(m.project.features.is_empty());
        assert!(m.rust_dependencies.is_empty());
    }

    #[test]
    fn test_parse_explicit_edition() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
edition = "2024"
"#,
        )
        .unwrap();
        assert_eq!(m.project.edition, "2024");
    }

    #[test]
    fn test_parse_features() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
features = ["allocator_api", "step_trait"]
"#,
        )
        .unwrap();
        assert_eq!(m.project.features, vec!["allocator_api", "step_trait"]);
    }

    #[test]
    fn test_parse_simple_dep() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"

[rust-dependencies]
rand = "0.8"
"#,
        )
        .unwrap();
        let rand = m.rust_dependencies.get("rand").unwrap();
        match rand {
            DepSpec::Version(v) => assert_eq!(v, "0.8"),
            _ => panic!("expected Version variant"),
        }
    }

    #[test]
    fn test_parse_detailed_dep() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"

[rust-dependencies]
regex = { version = "1", features = ["unicode"] }
"#,
        )
        .unwrap();
        let regex = m.rust_dependencies.get("regex").unwrap();
        match regex {
            DepSpec::Detailed {
                version, features, ..
            } => {
                assert_eq!(version, "1");
                assert_eq!(features, &vec!["unicode".to_string()]);
            }
            _ => panic!("expected Detailed variant"),
        }
    }

    #[test]
    fn test_parse_rust_caller() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
rust_caller = "rust_caller.rs"
"#,
        )
        .unwrap();
        assert_eq!(m.project.rust_caller.as_deref(), Some("rust_caller.rs"));
    }

    #[test]
    fn test_parse_no_rust_caller() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
"#,
        )
        .unwrap();
        assert!(m.project.rust_caller.is_none());
    }

    #[test]
    fn test_parse_toylang_dep() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"

[toylang-dependencies]
my_utils = { path = "../my_utils" }
"#,
        )
        .unwrap();
        let utils = m.toylang_dependencies.get("my_utils").unwrap();
        assert_eq!(utils.path, "../my_utils");
    }

    #[test]
    fn test_toylang_deps_default_empty() {
        let m = parse_str(
            r#"
[project]
name = "app"
source = "main.toylang"
"#,
        )
        .unwrap();
        assert!(m.toylang_dependencies.is_empty());
    }

    fn write_project(dir: &Path, name: &str, toylang_deps: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("main.toylang"), "fn main() {}\n").unwrap();
        let mut s = format!(
            "[project]\nname = \"{}\"\nsource = \"main.toylang\"\n",
            name
        );
        if !toylang_deps.is_empty() {
            s.push_str("\n[toylang-dependencies]\n");
            for (dep_name, dep_path) in toylang_deps {
                s.push_str(&format!("{} = {{ path = \"{}\" }}\n", dep_name, dep_path));
            }
        }
        std::fs::write(dir.join("toylang.toml"), s).unwrap();
    }

    #[test]
    fn test_resolve_dep_graph_root_only() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), "root", &[]);
        let graph = resolve_dep_graph(&tmp.path().join("toylang.toml")).unwrap();
        assert_eq!(graph.len(), 1);
        assert_eq!(graph[0].manifest.project.name, "root");
        assert!(graph[0].toylang_dep_names.is_empty());
    }

    #[test]
    fn test_resolve_dep_graph_root_plus_dep() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(&tmp.path().join("my_utils"), "my_utils", &[]);
        write_project(
            &tmp.path().join("app"),
            "app",
            &[("my_utils", "../my_utils")],
        );
        let graph =
            resolve_dep_graph(&tmp.path().join("app").join("toylang.toml")).unwrap();
        // Topological order: dep first, root last.
        assert_eq!(graph.len(), 2);
        assert_eq!(graph[0].manifest.project.name, "my_utils");
        assert_eq!(graph[1].manifest.project.name, "app");
        assert_eq!(graph[1].toylang_dep_names, vec!["my_utils".to_string()]);
    }

    #[test]
    fn test_resolve_dep_graph_cycle_detected() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(&tmp.path().join("a"), "a", &[("b", "../b")]);
        write_project(&tmp.path().join("b"), "b", &[("a", "../a")]);
        let err =
            resolve_dep_graph(&tmp.path().join("a").join("toylang.toml")).unwrap_err();
        assert!(err.contains("cycle"), "expected cycle error, got: {}", err);
    }

    #[test]
    fn test_resolve_dep_graph_name_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(&tmp.path().join("utils_v2"), "utils_v2", &[]);
        // Root declares dep key "my_utils" but the target's [project].name
        // is "utils_v2" — this should error so the user catches typos early.
        write_project(
            &tmp.path().join("app"),
            "app",
            &[("my_utils", "../utils_v2")],
        );
        let err = resolve_dep_graph(&tmp.path().join("app").join("toylang.toml"))
            .unwrap_err();
        assert!(
            err.contains("does not match"),
            "expected name-mismatch error, got: {}",
            err
        );
    }

    #[test]
    fn test_parse_missing_project_errors() {
        let result = parse_str(
            r#"
[rust-dependencies]
rand = "0.8"
"#,
        );
        assert!(result.is_err(), "expected error for missing [project]");
    }
}
