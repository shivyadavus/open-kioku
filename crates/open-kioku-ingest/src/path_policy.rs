//! The local indexing policy for one path: the security, hidden-file, exclude and ignore rules
//! discovery applies before it reads a file. It is one type so that a second consumer — an
//! imported index snapshot, whose rows were admitted under someone else's configuration — is
//! judged by exactly the rules `ok index` would apply here, not by a re-implementation of them.

use crate::{
    build_ignore_matcher, compile_globs, git_ignore, is_hidden_path, top_level_dir,
    ScopedIgnoreMatcher,
};
use globset::GlobSet;
use open_kioku_config::OkConfig;
use open_kioku_core::{File, IndexQuality, QualityNoteKind, SkipReason, SkipSource, SkippedPath};
use open_kioku_errors::Result;
use open_kioku_languages::is_supported_code;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Why the local policy keeps a path out of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyExclusion {
    pub reason: SkipReason,
    pub source: SkipSource,
    /// False for a secret-like path while redaction is on: the path itself is not reported.
    pub safe_to_show: bool,
}

/// The security rules alone — secret-like paths and `[paths] deny` — which need no filesystem
/// and no Git. Discovery applies them first, and anything that names paths discovery never
/// reads (Git history, graph labels, an imported index) applies the same ones, from here.
#[derive(Debug)]
pub struct SecurityPathPolicy {
    denied: GlobSet,
    redact_secrets: bool,
}

impl SecurityPathPolicy {
    pub fn new(config: &OkConfig) -> Result<Self> {
        Ok(Self {
            denied: compile_globs(&config.paths.deny)?,
            redact_secrets: config.security.redact_secrets,
        })
    }

    /// Why the security rules exclude `value`, or `None`. `value` need not be a repository
    /// path at all (a graph node's label: an import specifier such as `../utils/foo`, a route
    /// such as `/api/users`).
    pub fn exclusion(&self, value: &Path) -> Option<PolicyExclusion> {
        let secret_policy = open_kioku_core::is_secret_like_path(value);
        if !secret_policy && !self.denied.is_match(value) {
            return None;
        }
        Some(PolicyExclusion {
            reason: if secret_policy {
                SkipReason::SecretPolicy
            } else {
                SkipReason::Denied
            },
            source: SkipSource::SecurityPolicy,
            safe_to_show: !secret_policy || !self.redact_secrets,
        })
    }
}

#[derive(Debug)]
pub struct IndexPathPolicy {
    root: PathBuf,
    excludes: GlobSet,
    security: SecurityPathPolicy,
    allow_hidden_files: bool,
    /// Git's own verdict when `root` is in a work tree; `None` falls back to `git_ignores`.
    git_ignored: Option<HashSet<PathBuf>>,
    git_ignores: Option<ScopedIgnoreMatcher>,
    ok_ignores: ScopedIgnoreMatcher,
}

impl IndexPathPolicy {
    /// The policy discovery applies, with Git asked about every file present under `root`.
    pub(crate) fn for_scan(root: &Path, config: &OkConfig) -> Result<Self> {
        let git_ignored = git_ignore::ignored_paths(root)?;
        Self::build(root, config, git_ignored)
    }

    /// The same policy for `paths` (relative to `root`), which need not exist on disk: Git is
    /// asked about exactly these paths.
    pub fn for_paths(root: &Path, config: &OkConfig, paths: &[PathBuf]) -> Result<Self> {
        let git_ignored = git_ignore::ignored_among(root, paths)?;
        Self::build(root, config, git_ignored)
    }

    fn build(
        root: &Path,
        config: &OkConfig,
        git_ignored: Option<HashSet<PathBuf>>,
    ) -> Result<Self> {
        let git_ignores = if git_ignored.is_none() {
            Some(build_ignore_matcher(root, ".gitignore")?)
        } else {
            None
        };
        Ok(Self {
            root: root.to_path_buf(),
            excludes: compile_globs(&config.index.exclude)?,
            security: SecurityPathPolicy::new(config)?,
            allow_hidden_files: config.security.allow_hidden_files,
            git_ignored,
            git_ignores,
            ok_ignores: build_ignore_matcher(root, ".okignore")?,
        })
    }

    /// [`SecurityPathPolicy::exclusion`], with no Git call: for a value that may not be a
    /// repository path, which Git would reject and which the other rules do not govern.
    pub fn security_exclusion(&self, value: &Path) -> Option<PolicyExclusion> {
        self.security.exclusion(value)
    }

    /// The first rule that excludes `rel` (relative to the root), in discovery's order, or
    /// `None` when the policy admits it.
    pub fn exclusion(&self, rel: &Path) -> Option<PolicyExclusion> {
        if let Some(exclusion) = self.security_exclusion(rel) {
            return Some(exclusion);
        }
        let visible = |reason, source| {
            Some(PolicyExclusion {
                reason,
                source,
                safe_to_show: true,
            })
        };
        if !self.allow_hidden_files && is_hidden_path(rel) {
            return visible(SkipReason::Hidden, SkipSource::HiddenPolicy);
        }
        if self.excludes.is_match(rel) {
            return visible(SkipReason::Ignored, SkipSource::ConfigExclude);
        }
        let path = self.root.join(rel);
        let git_ignored = self
            .git_ignored
            .as_ref()
            .is_some_and(|paths| paths.contains(rel))
            || self
                .git_ignores
                .as_ref()
                .is_some_and(|matcher| matcher.is_ignored(&path, false));
        if git_ignored {
            return visible(SkipReason::Ignored, SkipSource::GitIgnore);
        }
        if self.ok_ignores.is_ignored(&path, false) {
            return visible(SkipReason::Ignored, SkipSource::OkIgnore);
        }
        None
    }
}

/// Whether discovery never reaches `rel` because a directory on its way is pruned by name
/// (`.git`, `.ok`, `target`, `node_modules`, `dist`, `build`, `.venv`).
pub fn is_pruned_by_discovery(rel: &Path) -> bool {
    rel.ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .any(crate::is_heavy_discovery_dir)
}

/// Record in `quality` that `file`, which an index counted as indexed, is excluded by
/// `exclusion` after all: moved from `indexed` to the skip reason in the coverage record and
/// listed among the skipped paths, as discovery records a file it skips, so the coverage an
/// imported index reports agrees with the files it serves.
pub fn record_excluded_indexed_file(
    quality: &mut IndexQuality,
    file: &File,
    exclusion: PolicyExclusion,
) {
    quality.skipped_paths.push(SkippedPath {
        path: if exclusion.safe_to_show {
            file.path.clone()
        } else {
            PathBuf::from("[redacted]")
        },
        reason: exclusion.reason,
        source: exclusion.source,
        safe_to_show: exclusion.safe_to_show,
    });
    if !is_supported_code(&file.language) {
        return;
    }
    if let Some(coverage) = quality.coverage.as_mut() {
        coverage.record_indexed_dropped(&file.language, file.is_generated, exclusion.reason);
        let top_dir = exclusion
            .safe_to_show
            .then(|| top_level_dir(&file.path))
            .flatten();
        coverage.record_policy_exclusion(&file.language, exclusion.source, top_dir.as_deref());
    }
}

/// Withhold, under this repository's `[security] redact_secrets`, every secret-like path an
/// index recorded as skipped: an index written elsewhere may have run with redaction off.
/// A directory named in the coverage record that is itself secret-like (`.ssh`) is dropped
/// from it for the same reason. Returns how many entries were withheld.
pub fn redact_recorded_skips(quality: &mut IndexQuality, config: &OkConfig) -> usize {
    if !config.security.redact_secrets {
        return 0;
    }
    let mut withheld = 0;
    for skipped in &mut quality.skipped_paths {
        if skipped.safe_to_show && open_kioku_core::is_secret_like_path(&skipped.path) {
            skipped.path = PathBuf::from("[redacted]");
            skipped.safe_to_show = false;
            withheld += 1;
        }
    }
    if let Some(coverage) = quality.coverage.as_mut() {
        let before = coverage.policy_excluded_dirs.len();
        coverage
            .policy_excluded_dirs
            .retain(|dir, _| !open_kioku_core::is_secret_like_path(Path::new(dir)));
        withheld += before - coverage.policy_excluded_dirs.len();
    }
    withheld
}

/// What an index's quality notes may still say about files removed from it after it was built.
pub struct RemovedContent<'a> {
    /// The removed files' repository paths.
    pub paths: &'a BTreeSet<PathBuf>,
    /// The ids of the removed files' chunks.
    pub chunk_ids: &'a HashSet<String>,
    /// Every word (a run of letters, digits, `_` and `$`, as the symbol registry reads chunk
    /// text) of the chunks that remain.
    pub remaining_words: &'a HashSet<String>,
}

/// The words of `text` as the symbol registry splits it into tokens.
pub fn registry_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
        .filter(|word| !word.is_empty())
}

/// Remove from `quality` the notes that describe only `removed` content: an import-resolver
/// caveat in a removed file, an unresolved name in a removed chunk, and a symbol-registry
/// caveat for a name no remaining chunk uses (the registry notes each name once, without its
/// chunk). `ok index` records none of them for a file discovery skips, and they spell the
/// file's path or the names in it. Returns how many were removed.
pub fn withhold_notes_about_removed_content(
    quality: &mut IndexQuality,
    removed: &RemovedContent<'_>,
) -> usize {
    let resolver_prefixes = removed
        .paths
        .iter()
        .map(|path| format!("import resolver caveat in {} for `", path.display()))
        .collect::<Vec<_>>();
    let before = quality.quality_notes.len();
    quality.quality_notes.retain(|note| {
        let message = note.message.as_str();
        match note.kind {
            QualityNoteKind::ImportResolverCaveat => !resolver_prefixes
                .iter()
                .any(|prefix| message.starts_with(prefix.as_str())),
            QualityNoteKind::SymbolRegistryUnresolved => message
                .rsplit_once(" in chunk ")
                .is_none_or(|(_, chunk)| !removed.chunk_ids.contains(chunk)),
            QualityNoteKind::SymbolRegistryCaveat => message
                .strip_prefix("symbol registry caveat for `")
                .and_then(|rest| rest.split_once("` via "))
                .is_none_or(|(token, _)| removed.remaining_words.contains(token)),
            _ => true,
        }
    });
    before - quality.quality_notes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn notes_only_removed_content_accounts_for_are_withheld() {
        let note = |kind, message: &str| open_kioku_core::QualityNote::new(kind, message);
        let mut quality = IndexQuality {
            quality_notes: vec![
                note(
                    QualityNoteKind::ImportResolverCaveat,
                    "import resolver caveat in internal/vault/keys.rs for `chrono::Utc`: none",
                ),
                note(
                    QualityNoteKind::ImportResolverCaveat,
                    "import resolver caveat in internal/vault/keys.rs.bak for `x`: none",
                ),
                note(
                    QualityNoteKind::SymbolRegistryUnresolved,
                    "symbol registry unresolved `seal_inner` in chunk c-removed",
                ),
                note(
                    QualityNoteKind::SymbolRegistryUnresolved,
                    "symbol registry unresolved `main_loop` in chunk c-kept",
                ),
                note(
                    QualityNoteKind::SymbolRegistryCaveat,
                    "symbol registry caveat for `seal_inner` via unresolved: no candidate",
                ),
                note(
                    QualityNoteKind::SymbolRegistryCaveat,
                    "symbol registry caveat for `check` via unique-project-name: 2 candidates",
                ),
                note(QualityNoteKind::Discovery, "discovery skipped 1 path(s)"),
            ],
            ..Default::default()
        };
        let paths = BTreeSet::from([PathBuf::from("internal/vault/keys.rs")]);
        let chunk_ids = HashSet::from(["c-removed".to_string()]);
        let remaining_words = registry_words("fn main() { anchor.check(); main_loop() }")
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let withheld = withhold_notes_about_removed_content(
            &mut quality,
            &RemovedContent {
                paths: &paths,
                chunk_ids: &chunk_ids,
                remaining_words: &remaining_words,
            },
        );
        assert_eq!(withheld, 3);
        let kept = quality
            .quality_notes
            .iter()
            .map(|note| note.message.as_str())
            .collect::<Vec<_>>();
        assert!(kept.iter().all(|message| !message.contains("seal_inner")));
        assert!(kept.iter().any(|message| message.contains("keys.rs.bak")));
        assert!(kept.iter().any(|message| message.contains("main_loop")));
        assert!(kept.iter().any(|message| message.contains("`check`")));
        assert!(kept.iter().any(|message| message.starts_with("discovery")));
    }

    fn policy_for(root: &Path, config: &OkConfig, paths: &[&str]) -> IndexPathPolicy {
        let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        IndexPathPolicy::for_paths(root, config, &paths).unwrap()
    }

    #[test]
    fn values_that_are_not_repository_paths_get_the_security_rules_without_git() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let policy = policy_for(root, &OkConfig::default(), &["src/lib.rs"]);
        for value in ["../utils/foo", "/api/users", "HTTP https://example.com/x"] {
            assert_eq!(policy.security_exclusion(Path::new(value)), None, "{value}");
        }
        assert_eq!(
            policy
                .security_exclusion(Path::new("../deploy/id_rsa"))
                .map(|exclusion| exclusion.source),
            Some(SkipSource::SecurityPolicy)
        );
        // Git itself is never asked about a value outside the work tree.
        let paths = ["../utils/foo", "/api", "src/lib.rs"].map(PathBuf::from);
        assert!(IndexPathPolicy::for_paths(root, &OkConfig::default(), &paths).is_ok());
    }

    #[test]
    fn pruned_directories_are_recognised_at_any_depth() {
        assert!(is_pruned_by_discovery(Path::new(".ok/index.sqlite")));
        assert!(is_pruned_by_discovery(Path::new(
            "web/node_modules/x/index.js"
        )));
        assert!(!is_pruned_by_discovery(Path::new("src/build.rs")));
    }

    #[test]
    fn paths_absent_from_disk_are_judged_by_the_local_rules() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join(".okignore"), "generated/\n").unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        let mut config = OkConfig::default();
        config.index.exclude = vec!["legacy/**".into()];
        let policy = policy_for(
            root,
            &config,
            &[
                ".env",
                "legacy/old.rs",
                "generated/api.rs",
                "debug.log",
                ".github/ci.yml",
                "src/lib.rs",
            ],
        );

        let reason = |rel: &str| policy.exclusion(Path::new(rel)).map(|e| e.source);
        assert_eq!(reason(".env"), Some(SkipSource::SecurityPolicy));
        assert!(!policy.exclusion(Path::new(".env")).unwrap().safe_to_show);
        assert_eq!(reason("legacy/old.rs"), Some(SkipSource::ConfigExclude));
        assert_eq!(reason("generated/api.rs"), Some(SkipSource::OkIgnore));
        assert_eq!(reason("debug.log"), Some(SkipSource::GitIgnore));
        assert_eq!(reason(".github/ci.yml"), Some(SkipSource::HiddenPolicy));
        assert_eq!(reason("src/lib.rs"), None);
    }
}
