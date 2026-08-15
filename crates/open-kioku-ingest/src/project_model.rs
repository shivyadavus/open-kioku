use open_kioku_core::Language;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectRoot {
    pub path: PathBuf,
    pub language: Language,
    pub source_roots: Vec<PathBuf>,
    pub package_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: Option<String>,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PathAlias {
    pub owner_root: PathBuf,
    pub pattern: String,
    pub targets: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectModel {
    pub roots: Vec<ProjectRoot>,
    pub packages: HashMap<String, PackageInfo>,
    pub aliases: Vec<PathAlias>,
    pub dependencies: HashSet<String>,
}

impl ProjectModel {
    pub fn discover(repo_root: &Path) -> Self {
        let mut model = Self::default();

        if !repo_root.exists() {
            return model;
        }

        Self::walk_discover(repo_root, repo_root, &mut model);
        model
    }

    fn walk_discover(current: &Path, repo_root: &Path, model: &mut ProjectModel) {
        let entries = match fs::read_dir(current) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" || name == "vendor" {
                    continue;
                }
                Self::walk_discover(&path, repo_root, model);
            } else if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    match file_name {
                        "Cargo.toml" => {
                            model.roots.push(ProjectRoot {
                                path: current.to_path_buf(),
                                language: Language::Rust,
                                source_roots: vec![current.join("src")],
                                package_name: None,
                            });
                        }
                        "go.mod" => {
                            let mut pkg_name = None;
                            if let Ok(content) = fs::read_to_string(&path) {
                                for line in content.lines() {
                                    if line.starts_with("module ") {
                                        pkg_name = Some(line.trim_start_matches("module ").trim().to_string());
                                        break;
                                    }
                                }
                            }
                            model.roots.push(ProjectRoot {
                                path: current.to_path_buf(),
                                language: Language::Go,
                                source_roots: vec![current.to_path_buf()],
                                package_name: pkg_name,
                            });
                        }
                        "package.json" => {
                            let mut pkg_name = None;
                            if let Ok(content) = fs::read_to_string(&path) {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                    pkg_name = json.get("name").and_then(|n| n.as_str()).map(|s| s.to_string());
                                }
                            }
                            model.roots.push(ProjectRoot {
                                path: current.to_path_buf(),
                                language: Language::TypeScript,
                                source_roots: vec![current.join("src"), current.to_path_buf()],
                                package_name: pkg_name,
                            });
                        }
                        "pyproject.toml" | "setup.py" | "setup.cfg" => {
                            model.roots.push(ProjectRoot {
                                path: current.to_path_buf(),
                                language: Language::Python,
                                source_roots: vec![current.join("src"), current.to_path_buf()],
                                package_name: None,
                            });
                        }
                        "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                            model.roots.push(ProjectRoot {
                                path: current.to_path_buf(),
                                language: Language::Java,
                                source_roots: vec![current.join("src/main/java"), current.join("src")],
                                package_name: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    pub fn module_path_from_file(&self, relative_path: &Path, language: &Language) -> String {
        let clean_path = relative_path.to_string_lossy().replace('\\', "/");
        let path_without_ext = relative_path.with_extension("");
        let path_str = path_without_ext.to_string_lossy().replace('\\', "/");

        match language {
            Language::Java => {
                let mut stripped = path_str.as_str();
                if let Some(pos) = stripped.find("src/main/java/") {
                    stripped = &stripped[pos + "src/main/java/".len()..];
                } else if let Some(pos) = stripped.find("src/") {
                    stripped = &stripped[pos + "src/".len()..];
                }
                stripped.replace('/', ".")
            }
            Language::Python => {
                let mut stripped = path_str.as_str();
                if let Some(pos) = stripped.find("src/") {
                    stripped = &stripped[pos + "src/".len()..];
                }
                let dotted = stripped.replace('/', ".");
                if dotted.ends_with(".__init__") {
                    dotted.trim_end_matches(".__init__").to_string()
                } else {
                    dotted
                }
            }
            Language::Rust => {
                let mut stripped = path_str.as_str();
                if let Some(pos) = stripped.find("src/") {
                    stripped = &stripped[pos + "src/".len()..];
                }
                let mod_path = stripped.replace('/', "::");
                if mod_path == "lib" || mod_path == "main" {
                    "crate".to_string()
                } else if mod_path.ends_with("::mod") {
                    format!("crate::{}", mod_path.trim_end_matches("::mod"))
                } else {
                    format!("crate::{mod_path}")
                }
            }
            Language::Go => {
                let go_root = self.roots.iter().find(|r| r.language == Language::Go);
                let pkg_prefix = go_root
                    .and_then(|r| r.package_name.as_deref())
                    .unwrap_or("module");
                let dir_part = relative_path.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                if dir_part.is_empty() || dir_part == "." {
                    pkg_prefix.to_string()
                } else {
                    format!("{pkg_prefix}/{dir_part}")
                }
            }
            Language::TypeScript | Language::JavaScript => {
                let ts_root = self.roots.iter().find(|r| r.language == Language::TypeScript || r.language == Language::JavaScript);
                let pkg_prefix = ts_root
                    .and_then(|r| r.package_name.as_deref())
                    .unwrap_or("@app");
                let mut stripped = clean_path.as_str();
                if let Some(pos) = stripped.find("src/") {
                    stripped = &stripped[pos + "src/".len()..];
                }
                let stripped_no_ext = Path::new(stripped).with_extension("");
                format!("{pkg_prefix}/{}", stripped_no_ext.to_string_lossy())
            }
            _ => clean_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn computes_semantic_module_paths() {
        let model = ProjectModel::default();

        let java_path = Path::new("src/main/java/com/acme/booking/ReservationService.java");
        assert_eq!(
            model.module_path_from_file(java_path, &Language::Java),
            "com.acme.booking.ReservationService"
        );

        let py_path = Path::new("src/app/booking/service.py");
        assert_eq!(
            model.module_path_from_file(py_path, &Language::Python),
            "app.booking.service"
        );

        let rust_path = Path::new("src/booking/service.rs");
        assert_eq!(
            model.module_path_from_file(rust_path, &Language::Rust),
            "crate::booking::service"
        );
    }
}
