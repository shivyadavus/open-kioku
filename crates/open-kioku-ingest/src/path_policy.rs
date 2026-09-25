//! The local indexing policy for one path: the security, hidden-file, exclude and ignore rules
//! discovery applies before it reads a file. It is one type so that a second consumer — an
//! imported index snapshot, whose rows were admitted under someone else's configuration — is
//! judged by exactly the rules `ok index` would apply here, not by a re-implementation of them.

use crate::{build_ignore_matcher, compile_globs, git_ignore, is_hidden_path, ScopedIgnoreMatcher};
use globset::GlobSet;
use open_kioku_config::OkConfig;
use open_kioku_core::{SkipReason, SkipSource};
use open_kioku_errors::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Why the local policy keeps a path out of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyExclusion {
    pub reason: SkipReason,
    pub source: SkipSource,
    /// False for a secret-like path while redaction is on: the path itself is not reported.
    pub safe_to_show: bool,
}

#[derive(Debug)]
pub struct IndexPathPolicy {
    root: PathBuf,
    excludes: GlobSet,
    denied: GlobSet,
    allow_hidden_files: bool,
    redact_secrets: bool,
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
            denied: compile_globs(&config.paths.deny)?,
            allow_hidden_files: config.security.allow_hidden_files,
            redact_secrets: config.security.redact_secrets,
            git_ignored,
            git_ignores,
            ok_ignores: build_ignore_matcher(root, ".okignore")?,
        })
    }

    /// The first rule that excludes `rel` (relative to the root), in discovery's order, or
    /// `None` when the policy admits it.
    pub fn exclusion(&self, rel: &Path) -> Option<PolicyExclusion> {
        let secret_policy = open_kioku_core::is_secret_like_path(rel);
        if secret_policy || self.denied.is_match(rel) {
            return Some(PolicyExclusion {
                reason: if secret_policy {
                    SkipReason::SecretPolicy
                } else {
                    SkipReason::Denied
                },
                source: SkipSource::SecurityPolicy,
                safe_to_show: !secret_policy || !self.redact_secrets,
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn policy_for(root: &Path, config: &OkConfig, paths: &[&str]) -> IndexPathPolicy {
        let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        IndexPathPolicy::for_paths(root, config, &paths).unwrap()
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
