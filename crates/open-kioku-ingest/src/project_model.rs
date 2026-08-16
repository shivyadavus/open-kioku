use open_kioku_core::Language;
pub use open_kioku_semantic_model::{ModuleInfo, PathAlias, ProjectModel, ProjectRoot};
use std::fs;
use std::path::{Path, PathBuf};

pub trait ProjectModelDiscovery {
    fn discover(repo_root: &Path) -> ProjectModel;
    fn module_path_from_file(&self, relative_path: &Path, language: &Language) -> String;
}

impl ProjectModelDiscovery for ProjectModel {
    fn discover(repo_root: &Path) -> ProjectModel {
        let mut model = ProjectModel::new();

        if !repo_root.exists() {
            return model;
        }

        walk_discover(repo_root, repo_root, &mut model);
        model
    }

    fn module_path_from_file(&self, relative_path: &Path, language: &Language) -> String {
        let nearest_root = self.nearest_root_for(relative_path, language.clone());
        let owner_relative = nearest_root
            .and_then(|root| relative_path.strip_prefix(&root.path).ok())
            .unwrap_or(relative_path);
        let clean_path = owner_relative.to_string_lossy().replace('\\', "/");
        let path_without_ext = owner_relative.with_extension("");
        let path_str = path_without_ext.to_string_lossy().replace('\\', "/");

        match language {
            Language::Java => {
                let parent_path = owner_relative
                    .parent()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let mut stripped = parent_path.as_str();
                if let Some(pos) = stripped.find("src/main/java/") {
                    stripped = &stripped[pos + "src/main/java/".len()..];
                } else if let Some(pos) = stripped.find("src/") {
                    stripped = &stripped[pos + "src/".len()..];
                }
                stripped.trim_matches('/').replace('/', ".")
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
                let pkg_prefix = nearest_root
                    .and_then(|r| r.package_name.as_deref())
                    .unwrap_or("module");
                let dir_part = owner_relative
                    .parent()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if dir_part.is_empty() || dir_part == "." {
                    pkg_prefix.to_string()
                } else {
                    format!("{pkg_prefix}/{dir_part}")
                }
            }
            Language::TypeScript | Language::JavaScript => {
                let pkg_prefix = nearest_root
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

fn repo_relative_path(path: &Path, repo_root: &Path) -> PathBuf {
    path.strip_prefix(repo_root).unwrap_or(path).to_path_buf()
}

fn push_project_root(
    model: &mut ProjectModel,
    repo_root: &Path,
    current: &Path,
    language: Language,
    source_roots: Vec<PathBuf>,
    package_name: Option<String>,
) {
    model.roots.push(ProjectRoot {
        path: repo_relative_path(current, repo_root),
        language,
        source_roots: source_roots
            .into_iter()
            .map(|root| repo_relative_path(&root, repo_root))
            .collect(),
        package_name,
    });
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
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == "vendor"
            {
                continue;
            }
            walk_discover(&path, repo_root, model);
        } else if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                match file_name {
                    "Cargo.toml" => {
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::Rust,
                            vec![current.join("src")],
                            None,
                        );
                    }
                    "go.mod" => {
                        let mut pkg_name = None;
                        if let Ok(content) = fs::read_to_string(&path) {
                            for line in content.lines() {
                                if line.starts_with("module ") {
                                    pkg_name =
                                        Some(line.trim_start_matches("module ").trim().to_string());
                                    break;
                                }
                            }
                        }
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::Go,
                            vec![current.to_path_buf()],
                            pkg_name,
                        );
                    }
                    "package.json" => {
                        let mut pkg_name = None;
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                pkg_name = json
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string());
                            }
                        }
                        let source_roots = vec![current.join("src"), current.to_path_buf()];
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::TypeScript,
                            source_roots.clone(),
                            pkg_name.clone(),
                        );
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::JavaScript,
                            source_roots,
                            pkg_name,
                        );
                    }
                    "pyproject.toml" | "setup.py" | "setup.cfg" => {
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::Python,
                            vec![current.join("src"), current.to_path_buf()],
                            None,
                        );
                    }
                    "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::Java,
                            vec![current.join("src/main/java"), current.join("src")],
                            None,
                        );
                    }
                    _ => {}
                }
            }
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
            "com.acme.booking"
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

    #[test]
    fn discovered_nested_roots_are_repo_relative_and_nearest_root_wins() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("packages/outer");
        let inner = outer.join("packages/inner");
        std::fs::create_dir_all(inner.join("src")).unwrap();
        std::fs::write(outer.join("package.json"), r#"{"name":"@acme/outer"}"#).unwrap();
        std::fs::write(inner.join("package.json"), r#"{"name":"@acme/inner"}"#).unwrap();

        let model = ProjectModel::discover(dir.path());
        assert!(model.roots.iter().all(|root| !root.path.is_absolute()));

        let file = Path::new("packages/outer/packages/inner/src/index.ts");
        assert_eq!(
            model.module_path_from_file(file, &Language::TypeScript),
            "@acme/inner/index"
        );
    }

    #[test]
    fn nested_go_module_uses_owner_relative_directory() {
        let dir = tempfile::tempdir().unwrap();
        let service = dir.path().join("services/orders");
        std::fs::create_dir_all(service.join("internal")).unwrap();
        std::fs::write(
            service.join("go.mod"),
            "module github.com/acme/orders\n\ngo 1.24\n",
        )
        .unwrap();

        let model = ProjectModel::discover(dir.path());
        let file = Path::new("services/orders/internal/handler.go");
        assert_eq!(
            model.module_path_from_file(file, &Language::Go),
            "github.com/acme/orders/internal"
        );
    }
}
