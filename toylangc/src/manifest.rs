use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default, rename = "rust-dependencies")]
    pub rust_dependencies: BTreeMap<String, DepSpec>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub name: String,
    pub source: String,
    #[serde(default = "default_edition")]
    pub edition: String,
    #[serde(default)]
    pub features: Vec<String>,
}

fn default_edition() -> String {
    "2021".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DepSpec {
    Version(String),
    Detailed {
        version: String,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default, rename = "default-features")]
        default_features: Option<bool>,
    },
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
