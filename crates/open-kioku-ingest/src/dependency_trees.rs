//! Which directories of policy-excluded files hold installed third-party packages.
//!
//! A coverage gap is priced by whether its missing files could hold callers of the code under
//! edit (#503). Installed packages cannot, so a gap made of them is reported without capping
//! confidence. The classification is on evidence a package manager writes, never on a name:
//!
//! - a directory holding `pyvenv.cfg` or `conda-meta/` is a Python environment, whatever it is
//!   called (`env/`, `py311/`);
//! - a `site-packages` or `dist-packages` directory holding a `*.dist-info` or `*.egg-info`
//!   entry is an installed-package tree;
//! - a `vendor` directory holding `modules.txt` (`go mod vendor`) or `composer/installed.json`
//!   (Composer) is vendored dependencies.
//!
//! `node_modules`, and `.venv`/`venv` holding a marker, never reach this module: discovery prunes
//! them before any file is visited (`prune.rs`). A directory none of the rules recognises is
//! unclassified and priced as first-party source: a git-ignored `generated/` tree plausibly holds
//! callers, and so does a `vendor/` nothing accounts for.
//!
//! Only the ancestors of an excluded file are probed, each once per scan, with a few `stat`s and
//! at most one directory read for a `site-packages`.

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

/// The evidence that the directory at `path` holds installed packages, if any.
fn probe(path: &Path) -> Option<DependencyEvidence> {
    if path.join("pyvenv.cfg").is_file() || path.join("conda-meta").is_dir() {
        return Some(DependencyEvidence::PythonEnvironment);
    }
    match path.file_name().and_then(|name| name.to_str()) {
        Some("site-packages" | "dist-packages") if holds_distribution_metadata(path) => {
            Some(DependencyEvidence::SitePackages)
        }
        Some("vendor") if path.join("modules.txt").is_file() => Some(DependencyEvidence::GoVendor),
        Some("vendor") if path.join("composer").join("installed.json").is_file() => {
            Some(DependencyEvidence::ComposerVendor)
        }
        _ => None,
    }
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
        write(root, "py/conda/lib/store.py");
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

        assert_eq!(
            enclosing(root, "env/lib/python3.12/site-packages/ledger/core.py"),
            Some(("env".into(), DependencyEvidence::PythonEnvironment))
        );
        assert_eq!(
            enclosing(root, "py/conda/lib/store.py"),
            Some(("py/conda".into(), DependencyEvidence::PythonEnvironment))
        );
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
        // Outside the site-packages tree, an unmarked environment's own files are unclassified.
        assert_eq!(enclosing(root, "venv/bin/tool.py"), None);
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
