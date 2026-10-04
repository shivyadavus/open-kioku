//! Which directories discovery cuts from the walk, and why.
//!
//! A directory is pruned only on evidence that it is build output or installed packages, never
//! by its name alone: a Rust `src/build/` module and a Java `com.acme.dist` package were pruned
//! with Cargo's `target/` until #477, and nothing recorded that they had been. Every walk over
//! the repository (discovery, the `.gitignore`/`.okignore` file walk, the Git ignore candidate
//! walk, the project model's and import resolver's manifest walks, the snapshot freshness
//! check, the watcher's event filter and `ok doctor`'s language sampling) asks this module, so
//! they cannot disagree about which files exist.
//!
//! The rule, by directory name:
//!
//! - `.git`, `.ok`: tooling, always pruned and never reported; they are never user source.
//! - `node_modules`: installed packages, always pruned.
//! - `.venv`, `venv`: pruned when they hold `pyvenv.cfg` (venv, virtualenv 20+) or
//!   `conda-meta` (conda). Without either, the walk goes in; a `.venv` is hidden, so its files
//!   are still skipped, one by one, by the hidden-file policy.
//! - `target`: pruned when it holds `CACHEDIR.TAG` (Cargo writes one) or sits beside a
//!   `Cargo.toml`, `pom.xml`, `build.sbt`, `build.properties` (sbt's `project/`) or
//!   `project.clj`. A `target` package under `src/main/java` is walked.
//! - `build`, `dist`: walked when a module or package declares them: a `mod.rs`, an
//!   `__init__.py` or a `.go` file directly inside; a Rust `<name>.rs` beside them that is not a
//!   Cargo build script; or a place under a `src/` directory with no build manifest beside
//!   them. Otherwise pruned: as `build_output` when a `CACHEDIR.TAG` is inside or a build
//!   manifest (`package.json`, `build.gradle`, `setup.py`, ...) sits beside them, and as
//!   `undeclared_build_dir` when nothing accounts for them either way.
//!
//! Undeclared `build` and `dist` default to pruned because that case is overwhelmingly
//! Gradle, setuptools, CMake and bundler output, often larger than the source. What the default
//! gets wrong stays visible: every pruned directory is recorded by path, and git-tracked source
//! under an `undeclared_build_dir` is counted as missing (`SkipReason::Pruned`). Under a
//! directory pruned on strong evidence (a cache tag, a manifest beside it, `node_modules`, a
//! marked environment), committed files are listed with their count but never lower coverage:
//! a committed `dist/` bundle beside a `package.json` is a published artifact, not lost source.
//!
//! When the rule is wrong for a repository, `[index] keep_dirs` names `build` and `dist`
//! directories to walk whatever sits beside them. Walking one only lets discovery see its files:
//! each is still judged by the security, hidden-file, exclude and ignore policy like any other.

use open_kioku_config::OkConfig;
use open_kioku_core::PruneReason;
use open_kioku_errors::Result;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// What discovery does with one directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirVerdict {
    Walk,
    /// `.git` and `.ok`: pruned and never reported.
    Tooling,
    Prune(PruneReason),
}

/// Files whose presence beside a `build`, `dist` or `target` directory says it is that
/// project's output rather than a source package.
const BUILD_MANIFESTS: [&str; 11] = [
    "Cargo.toml",
    "pom.xml",
    "build.sbt",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
    "setup.py",
    "pyproject.toml",
    "CMakeLists.txt",
    "go.mod",
    "meson.build",
];

/// Build manifests whose tools write a `target` beside them: Cargo, Maven, sbt (`build.sbt`,
/// and `build.properties` for the `project/` meta-build) and Leiningen.
const TARGET_MANIFESTS: [&str; 5] = [
    "Cargo.toml",
    "pom.xml",
    "build.sbt",
    "build.properties",
    "project.clj",
];

/// Discovery's directory rule for one repository: the evidence rule above, with the `build`
/// and `dist` directories `[index] keep_dirs` lists walked. Every walk over the repository
/// builds one from the same configuration, so they agree on which files exist.
#[derive(Debug, Clone)]
pub struct DiscoveryPruner {
    root: PathBuf,
    /// Repository-relative, validated by `IndexConfig::kept_dirs`.
    kept: BTreeSet<PathBuf>,
}

impl DiscoveryPruner {
    /// The rule for the repository at `root` under `config`; fails on an invalid `keep_dirs`.
    pub fn new(root: &Path, config: &OkConfig) -> Result<Self> {
        Ok(Self::with_kept(root, config.index.kept_dirs()?))
    }

    /// The rule with nothing kept: what discovery does under a configuration with no
    /// `keep_dirs`.
    pub fn evidence_only(root: &Path) -> Self {
        Self::with_kept(root, Vec::new())
    }

    pub(crate) fn with_kept(root: &Path, kept: Vec<PathBuf>) -> Self {
        Self {
            root: root.to_path_buf(),
            kept: kept.into_iter().collect(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The verdict for `path`, a directory entry under the root (`is_dir` from the walker's file
    /// type, so a symlink is never followed to decide). Only a handful of names are ever pruned,
    /// and a directory with any other name costs one string comparison.
    pub(crate) fn classify(&self, path: &Path, is_dir: bool) -> DirVerdict {
        let root = self.root.as_path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return DirVerdict::Walk;
        };
        // A worktree's `.git` is a file; it is tooling all the same.
        if matches!(name, ".git" | ".ok") {
            return DirVerdict::Tooling;
        }
        if !is_dir || path == root {
            return DirVerdict::Walk;
        }
        match name {
            "node_modules" => DirVerdict::Prune(PruneReason::Dependencies),
            ".venv" | "venv" => {
                if path.join("pyvenv.cfg").is_file() || path.join("conda-meta").is_dir() {
                    DirVerdict::Prune(PruneReason::VirtualEnv)
                } else {
                    DirVerdict::Walk
                }
            }
            "target" => {
                if has_cache_tag(path) || beside_any(path, &TARGET_MANIFESTS) {
                    DirVerdict::Prune(PruneReason::BuildOutput)
                } else {
                    DirVerdict::Walk
                }
            }
            "build" | "dist" => {
                if self.is_kept(path) {
                    // The user's statement outranks the guess, the cache tag's included: they
                    // named this directory, and its files still pass every per-file policy.
                    DirVerdict::Walk
                } else if has_cache_tag(path) {
                    DirVerdict::Prune(PruneReason::BuildOutput)
                } else if declared_as_source(root, path, name) {
                    DirVerdict::Walk
                } else if beside_any(path, &BUILD_MANIFESTS) {
                    DirVerdict::Prune(PruneReason::BuildOutput)
                } else {
                    DirVerdict::Prune(PruneReason::UndeclaredBuildDir)
                }
            }
            _ => DirVerdict::Walk,
        }
    }

    /// Whether a walk that reached the entry at `path` should enter it, as discovery would: the
    /// filter for a walker that must see the same tree (`is_dir` from the walker's file type).
    pub fn walks(&self, path: &Path, is_dir: bool) -> bool {
        self.classify(path, is_dir) == DirVerdict::Walk
    }

    /// Whether `rel` (relative to the root) lies under a directory discovery prunes, tooling
    /// included: the files `ok index` never reaches.
    pub fn is_pruned(&self, rel: &Path) -> bool {
        let mut dir = self.root.clone();
        let mut components = rel.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(part) = component else {
                continue;
            };
            dir.push(part);
            // The last component is the path itself; only `.git`/`.ok` prune a file.
            let is_dir = components.peek().is_some() || dir.is_dir();
            if self.classify(&dir, is_dir) != DirVerdict::Walk {
                return true;
            }
        }
        false
    }

    /// Whether `[index] keep_dirs` lists the directory at `path`.
    fn is_kept(&self, path: &Path) -> bool {
        !self.kept.is_empty()
            && path
                .strip_prefix(&self.root)
                .is_ok_and(|rel| self.kept.contains(rel))
    }
}

/// Whether one of `manifests` sits beside `path`.
fn beside_any(path: &Path, manifests: &[&str]) -> bool {
    path.parent().is_some_and(|parent| {
        manifests
            .iter()
            .any(|manifest| parent.join(manifest).is_file())
    })
}

/// The cache-directory marker (<https://bford.info/cachedir/>) Cargo and other tools write.
fn has_cache_tag(path: &Path) -> bool {
    path.join("CACHEDIR.TAG").is_file()
}

/// Whether a module system declares the `build` or `dist` directory at `path`.
fn declared_as_source(root: &Path, path: &Path, name: &str) -> bool {
    if path.join("mod.rs").is_file() || path.join("__init__.py").is_file() {
        return true;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    // `src/build.rs` beside `src/build/` is the 2018-edition module file; `build.rs` beside a
    // `Cargo.toml` is the package's build script, and its `build/` is not a module.
    if parent.join(format!("{name}.rs")).is_file() && !parent.join("Cargo.toml").is_file() {
        return true;
    }
    if contains_go_source(path) {
        return true;
    }
    // Java, Kotlin, TypeScript and JavaScript declare no module file; a package directory
    // lives under the project's `src/`. Output sits beside a build manifest, even under `src/`.
    let under_src = path
        .strip_prefix(root)
        .ok()
        .and_then(Path::parent)
        .is_some_and(|rel_parent| {
            rel_parent
                .components()
                .any(|component| component.as_os_str() == "src")
        });
    under_src && !beside_any(path, &BUILD_MANIFESTS)
}

/// A Go package is a directory of `.go` files; build output never holds one.
fn contains_go_source(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        entry.path().extension().is_some_and(|ext| ext == "go")
            && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::{DirVerdict, DiscoveryPruner};
    use open_kioku_config::{OkConfig, KEEPABLE_DIR_NAMES};
    use open_kioku_core::PruneReason;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn classify(root: &Path, path: &Path, is_dir: bool) -> DirVerdict {
        DiscoveryPruner::evidence_only(root).classify(path, is_dir)
    }

    fn is_under_pruned_dir(root: &Path, rel: &Path) -> bool {
        DiscoveryPruner::evidence_only(root).is_pruned(rel)
    }

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    fn verdict(root: &Path, rel: &str) -> DirVerdict {
        classify(root, &root.join(rel), true)
    }

    const BUILD_OUTPUT: DirVerdict = DirVerdict::Prune(PruneReason::BuildOutput);

    #[test]
    fn declared_build_and_dist_modules_are_walked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml");
        write(root, "src/build/mod.rs");
        write(root, "src/dist/mod.rs");
        write(root, "crates/ledger/src/dist.rs");
        write(root, "crates/ledger/src/dist/fmt.rs");
        write(root, "tools/build/__init__.py");
        write(root, "internal/build/plan.go");
        write(root, "web/src/build/index.ts");
        write(root, "core/src/main/java/com/acme/dist/Entry.java");
        write(root, "core/src/main/java/com/acme/target/Entry.java");

        for walked in [
            "src/build",
            "src/dist",
            "crates/ledger/src/dist",
            "tools/build",
            "internal/build",
            "web/src/build",
            "core/src/main/java/com/acme/dist",
            "core/src/main/java/com/acme/target",
        ] {
            assert_eq!(verdict(root, walked), DirVerdict::Walk, "{walked}");
        }
    }

    #[test]
    fn build_output_and_dependencies_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml");
        write(root, "build.rs");
        write(root, "target/debug/gen.rs");
        write(root, "crates/tool/Cargo.toml");
        write(root, "crates/tool/target/keep.rs");
        // Cargo's marker prunes a `target` even with the manifest elsewhere.
        write(root, "out/target/CACHEDIR.TAG");
        write(root, "build/steps.py");
        write(root, "dist/app.js");
        write(root, "node_modules/pkg/index.js");
        write(root, ".venv/pyvenv.cfg");
        write(root, "py/venv/conda-meta/history");
        write(root, "java/pom.xml");
        write(root, "java/target/classes/Entry.java");
        // Output beside a manifest, under `src/`, is still output.
        write(root, "src/web/package.json");
        write(root, "src/web/dist/bundle.js");
        // sbt's meta-build and Leiningen write `target` beside these.
        write(root, "scala/project/build.properties");
        write(root, "scala/project/target/streams/x");
        write(root, "clj/project.clj");
        write(root, "clj/target/classes/x");
        // A cache tag is strong evidence wherever the directory sits.
        write(root, "tools/gen/build/CACHEDIR.TAG");
        // Nothing declares these and no manifest accounts for them: the weak rule.
        write(root, "tools/build/steps.py");
        write(root, "packaging/dist/app.js");

        for pruned in [
            "target",
            "crates/tool/target",
            "out/target",
            "build",
            "dist",
            "java/target",
            "src/web/dist",
            "scala/project/target",
            "clj/target",
            "tools/gen/build",
        ] {
            assert_eq!(verdict(root, pruned), BUILD_OUTPUT, "{pruned}");
        }
        for pruned in ["tools/build", "packaging/dist"] {
            assert_eq!(
                verdict(root, pruned),
                DirVerdict::Prune(PruneReason::UndeclaredBuildDir),
                "{pruned}"
            );
        }
        assert_eq!(
            verdict(root, "node_modules"),
            DirVerdict::Prune(PruneReason::Dependencies)
        );
        assert_eq!(
            verdict(root, ".venv"),
            DirVerdict::Prune(PruneReason::VirtualEnv)
        );
        assert_eq!(
            verdict(root, "py/venv"),
            DirVerdict::Prune(PruneReason::VirtualEnv)
        );
        assert_eq!(verdict(root, ".git"), DirVerdict::Tooling);
        assert_eq!(verdict(root, ".ok"), DirVerdict::Tooling);
    }

    #[test]
    fn unmarked_target_and_venv_are_walked_and_files_are_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "docs/target/notes.md");
        write(root, "scripts/venv/setup.py");
        write(root, "bin/build");
        assert_eq!(verdict(root, "docs/target"), DirVerdict::Walk);
        assert_eq!(verdict(root, "scripts/venv"), DirVerdict::Walk);
        assert_eq!(
            classify(root, &root.join("bin/build"), false),
            DirVerdict::Walk
        );
        // A worktree's `.git` file is tooling too.
        assert_eq!(
            classify(root, &root.join(".git"), false),
            DirVerdict::Tooling
        );
    }

    #[test]
    fn a_path_under_a_pruned_directory_is_reported_as_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml");
        write(root, "target/debug/gen.rs");
        write(root, "src/build/mod.rs");
        assert!(is_under_pruned_dir(root, Path::new("target/debug/gen.rs")));
        assert!(is_under_pruned_dir(root, Path::new(".ok/index.sqlite")));
        assert!(is_under_pruned_dir(root, Path::new("node_modules/x/y.js")));
        assert!(!is_under_pruned_dir(root, Path::new("src/build/mod.rs")));
        assert!(!is_under_pruned_dir(root, Path::new("src/build.rs")));
        // A deleted file under a declared module is judged by what is on disk now.
        assert!(!is_under_pruned_dir(root, Path::new("src/build/gone.rs")));
    }

    #[test]
    fn kept_build_and_dist_directories_are_walked_whatever_the_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "package.json");
        // Beside a manifest: build output on the rule's evidence.
        write(root, "build/release.js");
        // Undeclared: the weak guess.
        write(root, "tools/dist/emit.py");
        // A cache tag is overruled too: the user named the directory.
        write(root, "gen/build/CACHEDIR.TAG");
        write(root, "packaging/dist/app.js");
        write(root, "Cargo.toml");
        write(root, "target/debug/gen.rs");

        let mut config = OkConfig::default();
        config.index.keep_dirs = vec!["build".into(), "tools/dist/".into(), "gen/build".into()];
        let pruner = DiscoveryPruner::new(root, &config).unwrap();
        for kept in ["build", "tools/dist", "gen/build"] {
            assert_eq!(
                pruner.classify(&root.join(kept), true),
                DirVerdict::Walk,
                "{kept}"
            );
            assert!(!pruner.is_pruned(&Path::new(kept).join("x.js")), "{kept}");
        }
        // Only the listed directories: a sibling and Cargo's `target` keep their verdicts.
        assert_eq!(
            pruner.classify(&root.join("packaging/dist"), true),
            DirVerdict::Prune(PruneReason::UndeclaredBuildDir)
        );
        assert!(pruner.is_pruned(Path::new("target/debug/gen.rs")));
        // Without the key, the same tree prunes all three.
        for pruned in ["build", "tools/dist", "gen/build"] {
            assert!(
                is_under_pruned_dir(root, &Path::new(pruned).join("x.js")),
                "{pruned}"
            );
        }

        config.index.keep_dirs = vec!["target".into()];
        assert!(DiscoveryPruner::new(root, &config).is_err());
    }

    /// Every name `keep_dirs` admits is one the rule can prune on a guess, so no listed entry
    /// is a no-op by construction.
    #[test]
    fn keepable_names_are_the_ones_pruned_on_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in KEEPABLE_DIR_NAMES {
            write(root, &format!("tools/{name}/x.js"));
            assert_eq!(
                verdict(root, &format!("tools/{name}")),
                DirVerdict::Prune(PruneReason::UndeclaredBuildDir),
                "{name}"
            );
            let kept =
                DiscoveryPruner::with_kept(root, vec![PathBuf::from(format!("tools/{name}"))]);
            assert_eq!(
                kept.classify(&root.join(format!("tools/{name}")), true),
                DirVerdict::Walk
            );
        }
    }
}
