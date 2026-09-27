//! The Cargo package model as stored rows: what each indexed `Cargo.toml` declares about its
//! package, its workspace and its dependencies on packages of the repository, as analysis facts on
//! that manifest's file row. Query-time consumers (impact's Cargo reachability) read these instead
//! of parsing manifests themselves, so they answer from the same model the import resolver used.
//!
//! A manifest that did not parse gets no fact, which proves nothing about the files under it; a
//! dependency whose manifest is not indexed gets no edge.

use open_kioku_core::cargo_manifest::{
    cargo_package_label, CARGO_BUILD_DEPENDENCY_SOURCE, CARGO_DEPENDENCY_SOURCE,
    CARGO_DEV_DEPENDENCY_SOURCE, CARGO_LIBRARY_ROOT_SOURCE, CARGO_PACKAGE_SOURCE,
    CARGO_PROC_MACRO_PACKAGE_SOURCE, CARGO_VIRTUAL_WORKSPACE_SOURCE, CARGO_WORKSPACE_MEMBER_SOURCE,
};
use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, GraphEdgeType, GraphNodeType,
};
use open_kioku_semantic_model::{CargoDependencyKind, ProjectModel};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The facts every indexed, parsed `Cargo.toml` of `project` states.
pub(crate) fn cargo_manifest_facts(project: &ProjectModel, files: &[File]) -> Vec<AnalysisFact> {
    let manifests = files
        .iter()
        .filter(|file| {
            file.path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        })
        .map(|file| (file.path.parent().unwrap_or(Path::new("")), file))
        .collect::<HashMap<&Path, &File>>();
    let indexed = files
        .iter()
        .map(|file| file.path.as_path())
        .collect::<HashSet<&Path>>();
    let mut facts = Vec::new();
    for root in &project.roots {
        let (Some(manifest), Some(file)) = (
            root.cargo_manifest.as_ref(),
            manifests.get(root.path.as_path()),
        ) else {
            continue;
        };
        let dir = slash_path(&root.path);
        let fact =
            |edge_type, target_kind, target: String, source: &'static str, message| AnalysisFact {
                id: identity::stable_hash(&format!(
                    "cargo-manifest:{}:{source}:{target}",
                    file.path.display()
                )),
                file_id: file.id.clone(),
                symbol_id: None,
                target,
                target_kind,
                edge_type,
                range: None,
                confidence: Confidence::Exact,
                source: source.into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                message,
            };
        match manifest.package.as_deref() {
            Some(package) => {
                let crate_name = root
                    .package_name
                    .as_deref()
                    .unwrap_or(package)
                    .replace('-', "_");
                let source = if manifest.proc_macro {
                    CARGO_PROC_MACRO_PACKAGE_SOURCE
                } else {
                    CARGO_PACKAGE_SOURCE
                };
                facts.push(fact(
                    GraphEdgeType::Defines,
                    GraphNodeType::BuildTarget,
                    cargo_package_label(&crate_name, &dir),
                    source,
                    format!(
                        "`{}` declares package `{package}`, whose library crate other crates name `{crate_name}`{}",
                        file.path.display(),
                        if manifest.proc_macro {
                            " (a procedural-macro crate)"
                        } else {
                            ""
                        }
                    )
                    .into(),
                ));
                // Only an indexed library root: a package with binaries alone has none, and a
                // node for a file the index does not hold would claim one it never read.
                let library = root
                    .library_root
                    .clone()
                    .unwrap_or_else(|| root.path.join("src/lib.rs"));
                if indexed.contains(library.as_path()) {
                    facts.push(fact(
                        GraphEdgeType::Defines,
                        GraphNodeType::File,
                        slash_path(&library),
                        CARGO_LIBRARY_ROOT_SOURCE,
                        format!(
                            "`{}` roots the library crate `{crate_name}` at `{}`",
                            file.path.display(),
                            library.display()
                        )
                        .into(),
                    ));
                }
            }
            None => facts.push(fact(
                GraphEdgeType::Defines,
                GraphNodeType::BuildTarget,
                cargo_package_label("workspace", &dir),
                CARGO_VIRTUAL_WORKSPACE_SOURCE,
                format!(
                    "`{}` declares a workspace and no package",
                    file.path.display()
                )
                .into(),
            )),
        }
        for member in &manifest.workspace_members {
            facts.push(fact(
                GraphEdgeType::Contains,
                GraphNodeType::Directory,
                member.clone(),
                CARGO_WORKSPACE_MEMBER_SOURCE,
                format!(
                    "`{}` names workspace members under `{member}`",
                    file.path.display()
                )
                .into(),
            ));
        }
        for dependency in &manifest.dependencies {
            let dependency_manifest = dependency.manifest_dir.join("Cargo.toml");
            if !manifests.contains_key(dependency.manifest_dir.as_path()) {
                continue;
            }
            let (source, table) = match dependency.kind {
                CargoDependencyKind::Normal => (CARGO_DEPENDENCY_SOURCE, "dependencies"),
                CargoDependencyKind::Dev => (CARGO_DEV_DEPENDENCY_SOURCE, "dev-dependencies"),
                CargoDependencyKind::Build => (CARGO_BUILD_DEPENDENCY_SOURCE, "build-dependencies"),
            };
            let named = dependency
                .crate_name
                .as_deref()
                .map(|name| format!(", which its code names `{name}`"))
                .unwrap_or_default();
            facts.push(fact(
                GraphEdgeType::DependsOn,
                GraphNodeType::File,
                slash_path(&dependency_manifest),
                source,
                format!(
                    "`{}` declares package `{}` in its {table}{}{} at `{}`{named}",
                    file.path.display(),
                    dependency.package,
                    if dependency.target_specific {
                        " for a target"
                    } else {
                        ""
                    },
                    if dependency.inherited {
                        " (inherited from the workspace)"
                    } else {
                        ""
                    },
                    display_dir(&dependency.manifest_dir),
                )
                .into(),
            ));
        }
    }
    facts.sort_by(|left, right| left.id.cmp(&right.id));
    facts.dedup_by(|left, right| left.id == right.id);
    facts
}

/// A repository-relative directory as a fact spells it: `.` for the repository root.
fn display_dir(dir: &Path) -> String {
    let dir = slash_path(dir);
    if dir.is_empty() {
        ".".to_string()
    } else {
        dir
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_model::ProjectModelDiscovery;
    use open_kioku_core::{FileId, Language, RepositoryId};

    #[test]
    fn indexed_manifests_state_their_package_workspace_and_repository_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, content: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nengine = { path = \"crates/engine\" }\n",
        );
        write(
            "crates/engine/Cargo.toml",
            "[package]\nname = \"plan-engine\"\n\n[lib]\nname = \"engine\"\n",
        );
        write(
            "crates/macros/Cargo.toml",
            "[package]\nname = \"macros\"\n\n[lib]\nproc-macro = true\n\n[dependencies]\nengine.workspace = true\n",
        );
        write(
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\nengine.workspace = true\nmacros = { path = \"../macros\" }\nhidden = { path = \"../hidden\" }\n\n[dev-dependencies]\nserde = \"1\"\n",
        );
        write("crates/hidden/Cargo.toml", "[package]\nname = \"hidden\"\n");
        let project = ProjectModel::discover(dir.path());
        // `crates/hidden/Cargo.toml` is not indexed: it states nothing, and no edge reaches it.
        let files = [
            "Cargo.toml",
            "crates/engine/Cargo.toml",
            "crates/engine/src/lib.rs",
            "crates/macros/Cargo.toml",
            "crates/app/Cargo.toml",
            "crates/app/src/main.rs",
        ]
        .map(|path| File {
            id: FileId::new(format!("file:{path}")),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Unknown,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        });
        let facts = cargo_manifest_facts(&project, &files);
        let mut stated = facts
            .iter()
            .map(|fact| {
                format!(
                    "{} {:?} {} [{}]",
                    fact.file_id.0,
                    fact.edge_type,
                    fact.target,
                    fact.source.as_str().trim_start_matches(
                        open_kioku_core::cargo_manifest::CARGO_MANIFEST_SOURCE_PREFIX
                    )
                )
            })
            .collect::<Vec<_>>();
        stated.sort();
        assert_eq!(
            stated,
            vec![
                "file:Cargo.toml Contains crates/ [workspace-member]",
                "file:Cargo.toml Defines workspace@. [virtual-workspace]",
                "file:crates/app/Cargo.toml Defines app@crates/app [package]",
                "file:crates/app/Cargo.toml DependsOn crates/engine/Cargo.toml [dependency]",
                "file:crates/app/Cargo.toml DependsOn crates/macros/Cargo.toml [dependency]",
                "file:crates/engine/Cargo.toml Defines crates/engine/src/lib.rs [library-root]",
                "file:crates/engine/Cargo.toml Defines engine@crates/engine [package]",
                "file:crates/macros/Cargo.toml Defines macros@crates/macros [proc-macro-package]",
                "file:crates/macros/Cargo.toml DependsOn crates/engine/Cargo.toml [dependency]",
            ]
        );
        assert!(facts
            .iter()
            .all(|fact| fact.confidence == Confidence::Exact));
    }
}
