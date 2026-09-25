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

/// The crate name a Rust path may spell instead of `crate::`, from a Cargo manifest.
///
/// That name is the **library target's**, which `[lib] name` sets independently of the package:
/// a package `foo-utils` with `[lib] name = "baz"` is `baz::` in a path and never `foo_utils::`.
/// `[package] name` is the fallback, since a manifest without a `[lib]` table takes the target
/// name from the package. A workspace-only manifest has neither, a `[[bin]]` name is a different
/// target, and `name.workspace = true` is not a name.
fn cargo_package_name(content: &str) -> Option<String> {
    let mut section = "";
    let (mut package, mut lib) = (None, None);
    for line in content.lines() {
        let line = line.trim();
        if let Some(table) = line.strip_prefix('[') {
            section = if table.starts_with("package]") {
                "package"
            } else if table.starts_with("lib]") {
                "lib"
            } else {
                ""
            };
            continue;
        }
        if section.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        let Some(quoted) = value.trim().strip_prefix('"') else {
            continue;
        };
        let Some((name, _)) = quoted.split_once('"') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        match section {
            "lib" => lib = Some(name.to_string()),
            _ => package = Some(name.to_string()),
        }
    }
    lib.or(package)
}

/// The `path` a Cargo manifest's `[lib]` table sets for the library crate root, relative to the
/// manifest's directory. `None` when the manifest keeps the default `src/lib.rs`.
fn cargo_library_path(content: &str) -> Option<String> {
    let mut in_lib = false;
    for line in content.lines() {
        let line = line.trim();
        if let Some(table) = line.strip_prefix('[') {
            in_lib = table.starts_with("lib]");
            continue;
        }
        if !in_lib {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let quoted = value.trim();
        let Some(quote) = quoted.chars().next().filter(|ch| matches!(ch, '"' | '\'')) else {
            continue;
        };
        let Some((path, _)) = quoted[1..].split_once(quote) else {
            continue;
        };
        let path = path.trim_start_matches("./");
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    None
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
        library_root: None,
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
                        let content = fs::read_to_string(&path).ok();
                        push_project_root(
                            model,
                            repo_root,
                            current,
                            Language::Rust,
                            vec![current.join("src")],
                            content.as_deref().and_then(cargo_package_name),
                        );
                        if let Some(root) = model.roots.last_mut() {
                            root.library_root = content
                                .as_deref()
                                .and_then(cargo_library_path)
                                .map(|library| root.path.join(library));
                        }
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

    #[test]
    fn rust_roots_carry_the_cargo_package_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/app\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"demo-crate\" # the package\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"tool\"\n",
        )
        .unwrap();

        // A renamed library target is the name a path spells, not the package name.
        std::fs::create_dir_all(dir.path().join("crates/renamed/src")).unwrap();
        std::fs::write(
            dir.path().join("crates/renamed/Cargo.toml"),
            "[package]\nname = \"foo-utils\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"baz\"\n",
        )
        .unwrap();
        // A subtree with no manifest of its own belongs to the nearest manifest above it.
        std::fs::create_dir_all(dir.path().join("examples/snippet")).unwrap();
        std::fs::write(
            dir.path().join("examples/snippet/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();

        let model = ProjectModel::discover(dir.path());
        let member = model
            .nearest_root_for(Path::new("crates/app/src/lib.rs"), Language::Rust)
            .expect("the member is a Rust project root");
        assert_eq!(member.package_name.as_deref(), Some("demo-crate"));
        let renamed = model
            .nearest_root_for(Path::new("crates/renamed/src/lib.rs"), Language::Rust)
            .expect("the renamed member is a Rust project root");
        assert_eq!(renamed.package_name.as_deref(), Some("baz"));
        // The manifest-less subtree is attributed to the workspace root rather than to no root,
        // so an in-crate path there is answered by that root's module tree, not by the
        // repository-path fall-through.
        let snippet = model
            .nearest_root_for(Path::new("examples/snippet/main.rs"), Language::Rust)
            .expect("a manifest-less subtree takes the nearest manifest above it");
        assert_eq!(snippet.path, Path::new(""));
        // A workspace manifest names no package, and `[[bin]]` names a target rather than one.
        let workspace = model
            .nearest_root_for(Path::new("Cargo.toml"), Language::Rust)
            .expect("the workspace root is a Rust project root");
        assert_eq!(workspace.package_name, None);
        assert_eq!(workspace.library_root, None);
        assert_eq!(member.library_root, None);
    }

    #[test]
    fn rust_roots_carry_the_library_root_a_manifest_sets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
        std::fs::write(
            dir.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\npath = \"not/the/lib.rs\"\n\n[lib]\nname = \"app\"\npath = \"./src/app_lib.rs\" # moved\n\n[[bin]]\npath = \"src/cli.rs\"\n",
        )
        .unwrap();

        let model = ProjectModel::discover(dir.path());
        let app = model
            .nearest_root_for(Path::new("crates/app/src/app_lib.rs"), Language::Rust)
            .expect("the package is a Rust project root");
        assert_eq!(
            app.library_root.as_deref(),
            Some(Path::new("crates/app/src/app_lib.rs"))
        );
        assert_eq!(
            cargo_library_path("[lib]\npath = 'lib.rs'\n").as_deref(),
            Some("lib.rs")
        );
        assert_eq!(cargo_library_path("[[bin]]\npath = \"src/cli.rs\"\n"), None);
    }
}
