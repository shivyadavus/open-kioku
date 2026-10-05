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
//!   (Composer) is vendored dependencies;
//! - a package directory directly inside a `packages` or `.packages` directory is a restored
//!   NuGet package when it is in the `packages.config` layout (`<Id>.<Version>/` holding
//!   `<Id>.<Version>.nupkg` or `<Id>.nuspec`) or the global packages folder's (`<id>/` with a
//!   `<version>/` holding `.nupkg.metadata` or `<id>.<version>.nupkg.sha512`). Each package is
//!   classed on its own, never the `packages/` holding it: its other entries stay unclassified.
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
//! read and a few `stat`s, or when its parent is named `packages` or `.packages`, with one read
//! of it and two `stat`s per directory inside it.

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
    let name = path.file_name().and_then(|name| name.to_str());
    let in_packages = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == "packages" || parent == ".packages");
    if let (true, Some(name)) = (in_packages, name) {
        if let Some(evidence) = nuget_package(path, name) {
            return Some(evidence);
        }
    }
    match name {
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

/// The NuGet layout the package directory `package` (named `name`, directly inside a
/// `packages` or `.packages`) was restored in, read from the files a restore writes into it,
/// never from a `.nupkg` or `.nuspec` alone. Only that package directory is classed, never the
/// `packages/` around it: a JavaScript workspace's `packages/` can hold a git-ignored
/// first-party `web-gen/` beside one restored package, and a first-party
/// `packages/Ledger/Ledger.nuspec` that packs this repository's own code is authored, not
/// restored, and fits neither layout.
fn nuget_package(package: &Path, name: &str) -> Option<DependencyEvidence> {
    if is_packages_config_package(package, name) {
        Some(DependencyEvidence::NugetPackages)
    } else if is_global_packages_entry(package, name) {
        Some(DependencyEvidence::NugetGlobalPackages)
    } else {
        None
    }
}

/// `packages/<Id>.<Version>/`, as `packages.config` restore extracts a package: it holds
/// `<Id>.<Version>.nupkg`, or `<Id>.nuspec` where the directory name is that id followed by a
/// version.
fn is_packages_config_package(package: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(package) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return false;
        }
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if let Some(stem) = file.strip_suffix(".nupkg") {
            return stem.eq_ignore_ascii_case(name);
        }
        file.strip_suffix(".nuspec").is_some_and(|id| {
            name.len() > id.len() + 1
                && name.is_char_boundary(id.len())
                && name[..id.len()].eq_ignore_ascii_case(id)
                && name[id.len()..].starts_with('.')
                && name[id.len() + 1..].starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

/// `<id>/` of a global packages folder: a `<version>/` under it holds the `.nupkg.metadata`
/// (NuGet 4.x and later) or `<id>.<version>.nupkg.sha512` restore writes once a package is
/// complete.
fn is_global_packages_entry(package: &Path, id: &str) -> bool {
    subdirectories(package).any(|version| {
        version.join(".nupkg.metadata").is_file()
            || version
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| version.join(format!("{id}.{name}.nupkg.sha512")).is_file())
    })
}

/// The directories directly inside `path`, symlinks not followed.
fn subdirectories(path: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
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

    /// Both NuGet install layouts, by what restore writes into each package directory (#684).
    #[test]
    fn restored_nuget_packages_are_recognised_in_either_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // packages.config: `<Id>.<Version>/` with the package file, or with `<Id>.nuspec`.
        write(root, "packages/Acme.Ledger.2.1.0/Acme.Ledger.2.1.0.nupkg");
        write(root, "packages/Acme.Ledger.2.1.0/content/Entry.cs.pp");
        write(root, "legacy/packages/Store.1.0.0/Store.nuspec");
        write(root, "legacy/packages/Store.1.0.0/tools/install.ps1");
        // A global packages folder inside the repository: `.nuget/packages`, or `.packages`.
        write(root, ".nuget/packages/acme.ledger/2.1.0/.nupkg.metadata");
        write(root, ".nuget/packages/acme.ledger/2.1.0/src/Entry.cs");
        write(root, ".packages/store/1.0.0/store.1.0.0.nupkg.sha512");
        write(root, ".packages/store/1.0.0/lib/Store.cs");

        for (rel, tree, evidence) in [
            (
                "packages/Acme.Ledger.2.1.0/content/Entry.cs.pp",
                "packages/Acme.Ledger.2.1.0",
                DependencyEvidence::NugetPackages,
            ),
            (
                "legacy/packages/Store.1.0.0/tools/install.ps1",
                "legacy/packages/Store.1.0.0",
                DependencyEvidence::NugetPackages,
            ),
            (
                ".nuget/packages/acme.ledger/2.1.0/src/Entry.cs",
                ".nuget/packages/acme.ledger",
                DependencyEvidence::NugetGlobalPackages,
            ),
            (
                ".packages/store/1.0.0/lib/Store.cs",
                ".packages/store",
                DependencyEvidence::NugetGlobalPackages,
            ),
        ] {
            assert_eq!(enclosing(root, rel), Some((tree.into(), evidence)), "{rel}");
        }
    }

    /// A `packages/` NuGet did not restore stays first-party: a JavaScript workspace, a
    /// project that packs its own code with an authored `.nuspec`, a loose `.nupkg`, and an
    /// unfinished global-folder entry.
    #[test]
    fn packages_directories_without_restore_layout_are_not_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "web/packages/ledger/package.json");
        write(root, "web/packages/ledger/src/index.ts");
        write(root, "nuget/packages/Ledger/Ledger.nuspec");
        write(root, "nuget/packages/Ledger/src/Entry.cs");
        write(root, "nuget/packages/Ledger.Core/Ledger.nuspec");
        write(root, "nuget/packages/Ledger.Core/Entry.cs");
        write(root, "feed/packages/Ledger.1.0.0.nupkg");
        write(root, "feed/packages/tools/emit.py");
        // The package file under another name, and a version directory with no metadata.
        write(root, "odd/packages/Ledger.1.0.0/Store.1.0.0.nupkg");
        write(root, "odd/packages/Ledger.1.0.0/emit.py");
        write(root, "cache/packages/ledger/1.0.0/ledger.nuspec");
        write(root, "cache/packages/ledger/1.0.0/emit.py");
        // A `.nupkg` directory is not a package file.
        fs::create_dir_all(root.join("dirs/packages/Store.1.0.0/Store.1.0.0.nupkg")).unwrap();
        write(root, "dirs/packages/Store.1.0.0/emit.py");

        for rel in [
            "web/packages/ledger/src/index.ts",
            "nuget/packages/Ledger/src/Entry.cs",
            "nuget/packages/Ledger.Core/Entry.cs",
            "feed/packages/tools/emit.py",
            "odd/packages/Ledger.1.0.0/emit.py",
            "cache/packages/ledger/1.0.0/emit.py",
            "dirs/packages/Store.1.0.0/emit.py",
        ] {
            assert_eq!(enclosing(root, rel), None, "{rel}");
        }
    }

    /// One restored package classes itself, not the `packages/` around it: a JavaScript
    /// workspace's first-party `web-gen/` beside it stays unclassified, in either layout.
    #[test]
    fn a_restored_package_does_not_reclassify_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "packages/Newtonsoft.Json.13.0.1/Newtonsoft.Json.13.0.1.nupkg",
        );
        write(root, "packages/Newtonsoft.Json.13.0.1/tools/init.js");
        write(root, "packages/web-gen/src/client.ts");
        write(root, "packages/ui/package.json");
        write(root, "packages/ui/src/button.ts");
        write(root, ".packages/acme.grid/1.0.0/.nupkg.metadata");
        write(root, ".packages/acme.grid/1.0.0/content/grid.js");
        write(root, ".packages/web-gen/client.ts");

        for rel in [
            "packages/web-gen/src/client.ts",
            "packages/ui/src/button.ts",
            ".packages/web-gen/client.ts",
        ] {
            assert_eq!(enclosing(root, rel), None, "{rel}");
        }
        assert_eq!(
            enclosing(root, "packages/Newtonsoft.Json.13.0.1/tools/init.js"),
            Some((
                "packages/Newtonsoft.Json.13.0.1".into(),
                DependencyEvidence::NugetPackages
            ))
        );
        assert_eq!(
            enclosing(root, ".packages/acme.grid/1.0.0/content/grid.js"),
            Some((
                ".packages/acme.grid".into(),
                DependencyEvidence::NugetGlobalPackages
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
