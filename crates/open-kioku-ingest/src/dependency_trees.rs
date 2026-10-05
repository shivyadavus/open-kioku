//! Which directories of policy-excluded files hold installed third-party packages.
//!
//! A coverage gap is priced by whether its missing files could hold callers of the code under
//! edit (#503). Installed packages cannot, so a gap made of them is reported without capping
//! confidence. The classification is on evidence a package manager writes, never on a name, and
//! it covers only the directory the tool installs packages into, never the tree around it:
//!
//! - a `site-packages` or `dist-packages` directory holding a `*.dist-info` or `*.egg-info`
//!   entry is an installed-package tree;
//! - the `lib/python*/site-packages` (or Windows `Lib/site-packages`) of a directory holding
//!   `pyvenv.cfg` or `conda-meta/` is a Python environment's installed packages, whatever the
//!   environment is called (`env/`, `py311/`);
//! - a `vendor` directory holding `modules.txt` (`go mod vendor`) or `composer/installed.json`
//!   (Composer) is vendored dependencies.
//!
//! A marker does not reclassify the directory holding it. `python -m venv .` run inside a
//! first-party `services/api/` writes `pyvenv.cfg` there beside the service's own code; only its
//! `lib/python*/site-packages` is installed packages, and a git-ignored `services/api/generated/`
//! stays source. So do an environment's `bin/` scripts, and conda's `pkgs/` cache is not read as
//! evidence either.
//!
//! `node_modules`, and `.venv`/`venv` holding a marker, never reach this module: discovery prunes
//! them before any file is visited (`prune.rs`). A directory none of the rules recognises is
//! unclassified and priced as first-party source: a git-ignored `generated/` tree plausibly holds
//! callers, and so does a `vendor/` nothing accounts for.
//!
//! Only the ancestors of an excluded file are probed, each once per scan. A directory is probed
//! only when its name is `site-packages`, `dist-packages` or `vendor`, with at most one directory
//! read and a few `stat`s.

use open_kioku_core::DependencyEvidence;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// The dependency verdict of every directory probed so far, by repository-relative path.
#[derive(Debug, Default)]
pub(crate) struct DependencyTrees {
    verdicts: HashMap<PathBuf, Option<DependencyEvidence>>,
}

impl DependencyTrees {
    /// The outermost directory above `rel` (a file path relative to `root`) that holds
    /// installed packages, `/`-separated, with its evidence; `None` when no ancestor does.
    pub(crate) fn enclosing(
        &mut self,
        root: &Path,
        rel: &Path,
    ) -> Option<(String, DependencyEvidence)> {
        let parent = rel.parent()?;
        let mut dir = PathBuf::new();
        for component in parent.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            dir.push(part);
            let verdict = match self.verdicts.get(&dir) {
                Some(verdict) => *verdict,
                None => {
                    let verdict = probe(&root.join(&dir));
                    self.verdicts.insert(dir.clone(), verdict);
                    verdict
                }
            };
            if let Some(evidence) = verdict {
                let path = dir
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                return Some((path, evidence));
            }
        }
        None
    }
}

/// The evidence that the directory at `path` holds installed packages, if any. Only a directory
/// a package tool installs into can qualify; a marker elsewhere says nothing about its siblings.
fn probe(path: &Path) -> Option<DependencyEvidence> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("site-packages" | "dist-packages") => {
            if environment_root(path).is_some_and(is_python_environment) {
                Some(DependencyEvidence::PythonEnvironment)
            } else if holds_distribution_metadata(path) {
                Some(DependencyEvidence::SitePackages)
            } else {
                None
            }
        }
        Some("vendor") if path.join("modules.txt").is_file() => Some(DependencyEvidence::GoVendor),
        Some("vendor") if path.join("composer").join("installed.json").is_file() => {
            Some(DependencyEvidence::ComposerVendor)
        }
        _ => None,
    }
}

/// The environment directory a `site-packages` at `path` belongs to by layout:
/// `<env>/lib/python<version>/site-packages` (venv, virtualenv, conda on POSIX) or
/// `<env>/Lib/site-packages` (Windows). `None` for any other placement.
fn environment_root(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    let parent_name = parent.file_name()?.to_str()?;
    if parent_name == "Lib" {
        return parent.parent();
    }
    let lib = parent.parent()?;
    (parent_name.starts_with("python") && lib.file_name()? == "lib").then_some(())?;
    lib.parent()
}

/// Whether `path` holds the marker venv/virtualenv 20+ (`pyvenv.cfg`) or conda (`conda-meta/`)
/// writes at an environment's root.
fn is_python_environment(path: &Path) -> bool {
    path.join("pyvenv.cfg").is_file() || path.join("conda-meta").is_dir()
}

/// Whether the directory holds an installed distribution's metadata, as pip (`*.dist-info`) and
/// setuptools (`*.egg-info`) write it.
fn holds_distribution_metadata(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.ends_with(".dist-info") || name.ends_with(".egg-info")
    })
}

#[cfg(test)]
mod tests {
    use super::DependencyTrees;
    use open_kioku_core::DependencyEvidence;
    use std::fs;
    use std::path::Path;

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    fn enclosing(root: &Path, rel: &str) -> Option<(String, DependencyEvidence)> {
        DependencyTrees::default().enclosing(root, Path::new(rel))
    }

    #[test]
    fn installed_package_trees_are_recognised_by_what_their_tools_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "env/pyvenv.cfg");
        write(root, "env/lib/python3.12/site-packages/ledger/core.py");
        write(root, "py/conda/conda-meta/history");
        write(root, "py/conda/lib/python3.11/site-packages/store/s.py");
        write(root, "win/env/pyvenv.cfg");
        write(root, "win/env/Lib/site-packages/ledger/core.py");
        write(root, "venv/lib/python3.11/site-packages/ledger/core.py");
        write(
            root,
            "venv/lib/python3.11/site-packages/ledger-1.0.dist-info/METADATA",
        );
        write(root, "venv/bin/tool.py");
        write(root, "svc/vendor/modules.txt");
        write(root, "svc/vendor/example.com/ledger/entry.go");
        write(root, "php/vendor/composer/installed.json");
        write(root, "php/vendor/acme/ledger/src/Entry.php");

        for (rel, site) in [
            (
                "env/lib/python3.12/site-packages/ledger/core.py",
                "env/lib/python3.12/site-packages",
            ),
            (
                "py/conda/lib/python3.11/site-packages/store/s.py",
                "py/conda/lib/python3.11/site-packages",
            ),
            (
                "win/env/Lib/site-packages/ledger/core.py",
                "win/env/Lib/site-packages",
            ),
        ] {
            assert_eq!(
                enclosing(root, rel),
                Some((site.into(), DependencyEvidence::PythonEnvironment)),
                "{rel}"
            );
        }
        assert_eq!(
            enclosing(root, "venv/lib/python3.11/site-packages/ledger/core.py"),
            Some((
                "venv/lib/python3.11/site-packages".into(),
                DependencyEvidence::SitePackages
            ))
        );
        assert_eq!(
            enclosing(root, "svc/vendor/example.com/ledger/entry.go"),
            Some(("svc/vendor".into(), DependencyEvidence::GoVendor))
        );
        assert_eq!(
            enclosing(root, "php/vendor/acme/ledger/src/Entry.php"),
            Some(("php/vendor".into(), DependencyEvidence::ComposerVendor))
        );
        // Outside the site-packages tree, an environment's own files are unclassified.
        assert_eq!(enclosing(root, "venv/bin/tool.py"), None);
    }

    /// A marker covers the environment's installed packages, not the directory holding it:
    /// `python -m venv .` inside a first-party service, or a `conda-meta/` beside a tool's
    /// generated code, must not turn that code into dependencies.
    #[test]
    fn an_environment_marker_does_not_reclassify_the_tree_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "services/api/pyvenv.cfg");
        write(
            root,
            "services/api/lib/python3.12/site-packages/ledger/core.py",
        );
        write(root, "services/api/generated/ledger_pb2.py");
        write(root, "services/api/handlers.py");
        write(root, "services/api/bin/serve.py");
        write(root, "tools/conda-meta/history");
        write(root, "tools/gen/emit.py");
        write(root, "tools/pkgs/cached/mod.py");
        // A `site-packages` placed anywhere but an environment's library is not covered by it.
        write(root, "services/api/site-packages/ledger/core.py");

        for rel in [
            "services/api/generated/ledger_pb2.py",
            "services/api/handlers.py",
            "services/api/bin/serve.py",
            "tools/gen/emit.py",
            "tools/pkgs/cached/mod.py",
            "services/api/site-packages/ledger/core.py",
        ] {
            assert_eq!(enclosing(root, rel), None, "{rel}");
        }
        assert_eq!(
            enclosing(
                root,
                "services/api/lib/python3.12/site-packages/ledger/core.py"
            ),
            Some((
                "services/api/lib/python3.12/site-packages".into(),
                DependencyEvidence::PythonEnvironment
            ))
        );
    }

    #[test]
    fn names_alone_are_not_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A `site-packages` with no installed-distribution metadata, a `vendor` with no manifest,
        // and a `generated` tree are all unclassified.
        write(root, "lib/site-packages/ledger/core.py");
        write(root, "vendor/ledger/core.go");
        write(root, "generated/ledger_pb2.py");
        write(root, "pyvenv.py");
        for rel in [
            "lib/site-packages/ledger/core.py",
            "vendor/ledger/core.go",
            "generated/ledger_pb2.py",
            "pyvenv.py",
        ] {
            assert_eq!(enclosing(root, rel), None, "{rel}");
        }
    }
}
