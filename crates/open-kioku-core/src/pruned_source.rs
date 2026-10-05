//! Pruned directories holding git-tracked source, linked to the task a pack or plan answers.
//!
//! Discovery prunes some directories that Git says hold committed source: a `build` or `dist`
//! nothing declares (`undeclared_build_dir`), a directory behind a stray `.git` (`submodule`
//! with tracked files), and committed output beside its build manifest (`build_output`). The
//! index never reads those files, so a name defined there is absent from the index without
//! being absent from the repository. [`IndexCoverage::pruned_source_links`] decides, without
//! reading any of those files, which of the directories a task may be about, and
//! [`PrunedSourceLinks`] words and prices the result for context packs and plans alike.
//!
//! The link is deliberately narrow, so a repository with a pruned directory does not caveat
//! every task:
//!
//! - **Named by the task**: a path-like task token (one holding a `/`) is the directory or a
//!   path below it (`tools/build/plan.py`, `build/`). A bare word never counts: "fix the build"
//!   names no directory. Any prune reason qualifies, because the task asked about those files.
//! - **An undefined name**: a named task identifier that no selected context spells and that
//!   no indexed symbol defines, beside a directory pruned on the weak rule or behind a stray
//!   `.git`. Directories pruned on strong evidence (a cache tag, a build manifest beside them)
//!   never qualify this way: their committed files are output, and a name missing from source
//!   is not explained by a bundle.
//!
//! Prune records carry no per-file languages, so language is not used to narrow the link.
//!
//! Only a directory the task names by path, whose tracked source counts as missing, caps
//! confidence. An undefined name is reported but does not cap: a name the task asks to create,
//! or to rename something to, has no indexed definition by construction. A missing name already
//! lowers confidence through the `anchor` negative evidence item.

use std::path::Path;

use crate::{
    group_thousands, is_secret_like_path, negative_evidence_scope, CoverageInput, IndexCoverage,
    NegativeEvidence, PruneReason, PrunedDir,
};

/// Score-component signal emitted when a task reaches a pruned directory holding git-tracked
/// source. Zero weight like `index_coverage`: the caps price it, and the component carries the
/// `coverage:pruned:<path>` evidence ids a reader traces them to. `ok preflight` reads it to
/// withhold `safe_to_start`, so it is a stable name rather than prose to match on.
pub const PRUNED_SOURCE_SIGNAL: &str = "index_coverage_pruned_source";

/// At most this many directories are named in one caveat, blocker or probe; the rest are
/// counted.
const PRUNED_SOURCE_DIRS_NAMED: usize = 3;

/// The pruned directories holding git-tracked source that a task reaches, and how. Built by
/// [`IndexCoverage::pruned_source_links`]; empty when the task reaches none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrunedSourceLinks {
    /// Named task identifiers that no selected context spells and no indexed symbol defines,
    /// in task order. Empty when `unresolved_dirs` is.
    pub undefined_identifiers: Vec<String>,
    /// Directories pruned on the weak rule or behind a stray `.git` that hold tracked source,
    /// where `undefined_identifiers` may be defined.
    pub unresolved_dirs: Vec<PrunedDir>,
    /// Directories holding tracked source whose path the task names, whatever their reason.
    pub named_dirs: Vec<PrunedDir>,
}

impl IndexCoverage {
    /// Pruned directories that hold git-tracked source and may be named in a pack: never a
    /// secret-like path, whatever an older or imported manifest recorded.
    fn linkable_pruned_dirs(&self) -> impl Iterator<Item = &PrunedDir> {
        self.pruned.iter().filter(|dir| {
            dir.tracked_source_files.is_some_and(|count| count > 0)
                && !is_secret_like_path(Path::new(&dir.path))
        })
    }

    /// Whether an undefined task identifier could be linked to a pruned directory: one pruned
    /// on the weak rule or behind a stray `.git` holds tracked source. Callers look task
    /// identifiers up in the symbol table only when this holds, so a repository without such a
    /// directory pays nothing.
    pub fn may_hide_tracked_source(&self) -> bool {
        self.linkable_pruned_dirs()
            .any(|dir| dir.reason.counts_tracked_source())
    }

    /// The pruned directories holding tracked source that `task` reaches: those whose path it
    /// names, and, when `undefined_identifiers` is not empty, every one pruned on the weak rule
    /// or behind a stray `.git`. `undefined_identifiers` are the task's named identifiers that
    /// no selected context spells and no indexed symbol defines; the caller looks them up.
    pub fn pruned_source_links(
        &self,
        task: &str,
        undefined_identifiers: Vec<String>,
    ) -> PrunedSourceLinks {
        let named_dirs = self
            .linkable_pruned_dirs()
            .filter(|dir| task_names_directory(task, &dir.path))
            .cloned()
            .collect::<Vec<_>>();
        let unresolved_dirs = if undefined_identifiers.is_empty() {
            Vec::new()
        } else {
            self.linkable_pruned_dirs()
                .filter(|dir| dir.reason.counts_tracked_source())
                .cloned()
                .collect::<Vec<_>>()
        };
        PrunedSourceLinks {
            undefined_identifiers: if unresolved_dirs.is_empty() {
                Vec::new()
            } else {
                undefined_identifiers
            },
            unresolved_dirs,
            named_dirs,
        }
    }
}

/// Whether a path-like token of `task` is `dir` or a path below it. Only tokens holding a `/`
/// count, so a prose word that happens to be a directory's name (`build`, `dist`) never does;
/// `build/` and `./tools/build/plan.py` do.
pub fn task_names_directory(task: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return false;
    }
    task.split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '/' | '\\' | '.' | '_' | '-')))
        .map(|token| token.replace('\\', "/"))
        .filter(|token| token.contains('/'))
        .any(|token| {
            // A sentence may end on the path (`... in tools/build/.`).
            let token = token.trim_end_matches('.');
            let token = token.strip_prefix("./").unwrap_or(token);
            let token = token.trim_end_matches('/');
            token == dir
                || token
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// How a caveat names a prune reason.
fn reason_phrase(reason: PruneReason) -> &'static str {
    match reason {
        PruneReason::UndeclaredBuildDir => "undeclared build directory",
        // A tracked file under a nested work tree means its `.git` is stray: Git tracks no file
        // under a real submodule, and only directories holding tracked source are linked.
        PruneReason::Submodule => "nested repository",
        PruneReason::BuildOutput => "build output",
        PruneReason::Dependencies => "installed dependencies",
        PruneReason::VirtualEnv => "virtual environment",
    }
}

/// `tools/build/: undeclared build directory, 3 tracked source files`
fn dir_detail(dir: &PrunedDir) -> String {
    let count = dir.tracked_source_files.unwrap_or(0);
    format!(
        "{}/: {}, {} tracked source {}",
        dir.path,
        reason_phrase(dir.reason),
        group_thousands(count),
        if count == 1 { "file" } else { "files" }
    )
}

/// `tools/build/`, `tools/build/ or web/dist/`, or the first few and how many more.
fn dir_list(dirs: &[&PrunedDir]) -> String {
    let shown = dirs
        .iter()
        .take(PRUNED_SOURCE_DIRS_NAMED)
        .map(|dir| format!("{}/", dir.path))
        .collect::<Vec<_>>();
    let rest = dirs.len().saturating_sub(shown.len());
    let mut list = match shown.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, others)) if rest == 0 => format!("{} or {last}", others.join(", ")),
        Some(_) => shown.join(", "),
    };
    if rest > 0 {
        list.push_str(&format!(
            " or {} more pruned director{}",
            group_thousands(rest),
            if rest == 1 { "y" } else { "ies" }
        ));
    }
    list
}

/// `(tools/build/: undeclared build directory, 3 tracked source files; ...)` for the named
/// directories.
fn dir_details(dirs: &[&PrunedDir]) -> String {
    dirs.iter()
        .take(PRUNED_SOURCE_DIRS_NAMED)
        .map(|dir| dir_detail(dir))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `tools/build/ (3 tracked source files), out/dist/ (1 tracked source file)`, the first few
/// and how many more.
fn dir_labels(dirs: &[&PrunedDir]) -> String {
    let mut labels = dirs
        .iter()
        .take(PRUNED_SOURCE_DIRS_NAMED)
        .map(|dir| dir.label())
        .collect::<Vec<_>>()
        .join(", ");
    let rest = dirs.len().saturating_sub(PRUNED_SOURCE_DIRS_NAMED);
    if rest > 0 {
        labels.push_str(&format!(" and {} more", group_thousands(rest)));
    }
    labels
}

impl PrunedSourceLinks {
    pub fn is_empty(&self) -> bool {
        self.unresolved_dirs.is_empty() && self.named_dirs.is_empty()
    }

    /// Every linked directory once, unresolved first.
    fn dirs(&self) -> Vec<&PrunedDir> {
        let mut dirs: Vec<&PrunedDir> = Vec::new();
        for dir in self.unresolved_dirs.iter().chain(&self.named_dirs) {
            if !dirs.iter().any(|seen| seen.path == dir.path) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// Linked directories whose tracked source counts as missing (the weak rule's guess, or a
    /// stray `.git`), however the task reached them.
    fn missing_source_dirs(&self) -> Vec<&PrunedDir> {
        self.dirs()
            .into_iter()
            .filter(|dir| dir.reason.counts_tracked_source())
            .collect()
    }

    /// Directories the task names by path whose tracked source counts as missing: the only
    /// links that cap. The task points at files the index never read. An undefined name does
    /// not cap. A name the task asks to create or rename to is undefined by construction, and
    /// a missing name already lowers confidence through the `anchor` item. A directory pruned on
    /// strong evidence caps nothing, as its committed files never lower coverage.
    fn capping_dirs(&self) -> Vec<&PrunedDir> {
        self.named_dirs
            .iter()
            .filter(|dir| dir.reason.counts_tracked_source())
            .collect()
    }

    /// Whether the 0.50 cap applies: the task names the path of a directory whose tracked
    /// source the index counts as missing.
    pub fn caps(&self) -> bool {
        !self.capping_dirs().is_empty()
    }

    /// `coverage:pruned:<path>` for every linked directory: the evidence ref plans already
    /// cite for a pruned directory, naming its `coverage.pruned` entry in `repo_status`.
    pub fn evidence_ids(&self) -> Vec<String> {
        self.dirs()
            .iter()
            .map(|dir| format!("coverage:pruned:{}", dir.path))
            .collect()
    }

    /// Coverage caveats, exempt from the any-caveat cap like a coverage gap's.
    pub fn caveats(&self) -> Vec<String> {
        let mut caveats = Vec::new();
        if !self.unresolved_dirs.is_empty() {
            let dirs = self.unresolved_dirs.iter().collect::<Vec<_>>();
            caveats.push(format!(
                "index coverage: {} {} no indexed definition and may be in {}, which the index pruned ({}); an absence there is not evidence",
                self.undefined_identifiers.join(", "),
                if self.undefined_identifiers.len() == 1 { "has" } else { "have" },
                dir_list(&dirs),
                dir_details(&dirs)
            ));
        }
        if !self.named_dirs.is_empty() {
            let dirs = self.named_dirs.iter().collect::<Vec<_>>();
            caveats.push(format!(
                "index coverage: the task names {}, which the index pruned ({}); the index never read those files",
                dir_list(&dirs),
                dir_details(&dirs)
            ));
        }
        caveats
    }

    /// The blocker beside the 0.50 cap, or `None` when nothing caps.
    pub fn blocker(&self) -> Option<String> {
        let dirs = self.capping_dirs();
        (!dirs.is_empty()).then(|| {
            format!(
                "the task names a directory the index pruned: {}",
                dir_labels(&dirs)
            )
        })
    }

    /// The `anchor` item's next probe: replaces "does not exist in this repository or needs
    /// `ok index`", which a pruned directory holding tracked source makes untrue. Only
    /// directories whose tracked source counts as missing are named: a missing name is not
    /// explained by build output. With `excluded_source` (a majority coverage gap) the probe
    /// says that too, so it never names a narrower cause than the evidence supports.
    pub fn anchor_probe(&self, excluded_source: bool) -> Option<String> {
        let dirs = self.missing_source_dirs();
        (!dirs.is_empty()).then(|| {
            let gap = if excluded_source {
                " The index also excluded most of a language's source, so the name may be defined in those files."
            } else {
                ""
            };
            format!(
                "Run `ok search <identifier>` for each name; a name the index does not hold may be in {}, which the index pruned (see the `coverage` negative evidence), so its absence from the index is not evidence it is absent from the repository.{gap}",
                dir_list(&dirs)
            )
        })
    }

    /// A plan's risk reason: `low confidence: …` beside the cap, and otherwise a disclosure
    /// naming the undefined anchors and where they may be.
    pub fn risk_reason(&self) -> Option<String> {
        if self.caps() {
            return Some(format!(
                "low confidence: the task names a directory the index pruned: {}",
                dir_labels(&self.capping_dirs())
            ));
        }
        (!self.unresolved_dirs.is_empty()).then(|| {
            format!(
                "named task anchor(s) {} have no indexed definition and may be defined in a directory the index pruned: {}",
                self.undefined_identifiers.join(", "),
                dir_labels(&self.unresolved_dirs.iter().collect::<Vec<_>>())
            )
        })
    }

    /// What to do before reading an absence as evidence, for the first linked directory.
    fn next_probe(&self) -> Option<String> {
        let dir = self.dirs().into_iter().next()?;
        let path = &dir.path;
        Some(match dir.reason {
            PruneReason::UndeclaredBuildDir => format!(
                "Search the files under `{path}/` directly (`git ls-files {path}`) before concluding a name is absent; if they are source an agent should see, list `{path}` under `[index] keep_dirs` in `ok.toml`, then run `ok index .`."
            ),
            PruneReason::Submodule => format!(
                "Search the files under `{path}/` directly (`git ls-files {path}`) before concluding a name is absent; this repository tracks them, so the `{path}/.git` that pruned them looks stray. If the directory is this repository's source, move that `.git` out of the tree rather than deleting it (it may hold another repository's unpushed history), then run `ok index .`."
            ),
            PruneReason::BuildOutput | PruneReason::Dependencies | PruneReason::VirtualEnv => {
                format!(
                    "Search the files under `{path}/` directly (`git ls-files {path}`); the index pruned the directory on evidence that it is {}, so change its files through their source, generator or manifest.",
                    reason_phrase(dir.reason)
                )
            }
        })
    }
}

impl NegativeEvidence {
    /// The one `coverage` item a pack or plan publishes: [`Self::for_coverage_input`], with the
    /// pruned directories the task reaches added to it, or an item of their own when the
    /// coverage record holds no gap. Context and plan both build it here.
    pub fn for_coverage(
        query: &str,
        coverage: &CoverageInput,
        pruned: &PrunedSourceLinks,
    ) -> Option<Self> {
        let base = Self::for_coverage_input(query, coverage);
        if pruned.is_empty() {
            return base;
        }
        let reason = pruned.caveats().join("; ");
        let probe = pruned.next_probe();
        let pruned_sources = std::iter::once("index_manifest.quality.coverage.pruned".to_owned())
            .chain(pruned.evidence_ids());
        Some(match base {
            Some(mut item) => {
                item.inspected_sources.extend(pruned_sources);
                item.reason = format!("{}; {reason}", item.reason);
                item.suggested_next_probe = match (item.suggested_next_probe.take(), probe) {
                    (Some(first), Some(second)) => Some(format!("{first} {second}")),
                    (first, second) => first.or(second),
                };
                item
            }
            None => Self {
                query: query.into(),
                scope: negative_evidence_scope::COVERAGE.into(),
                inspected_sources: pruned_sources.collect(),
                reason,
                confidence: 0.90,
                suggested_next_probe: probe,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, reason: PruneReason, tracked: Option<usize>) -> PrunedDir {
        PrunedDir {
            path: path.into(),
            reason,
            tracked_source_files: tracked,
        }
    }

    fn coverage(pruned: Vec<PrunedDir>) -> IndexCoverage {
        IndexCoverage {
            pruned_dirs: pruned.len(),
            pruned,
            ..IndexCoverage::default()
        }
    }

    #[test]
    fn a_directory_is_named_only_by_a_path_like_token() {
        assert!(task_names_directory(
            "edit tools/build/plan.py",
            "tools/build"
        ));
        assert!(task_names_directory(
            "what is in `tools/build`?",
            "tools/build"
        ));
        assert!(task_names_directory("clean up build/.", "build"));
        assert!(task_names_directory("read ./build/gen.rs", "build"));
        assert!(task_names_directory(
            "read tools\\build\\gen.rs",
            "tools/build"
        ));
        // Prose, and a sibling sharing a prefix, name nothing.
        assert!(!task_names_directory("fix the build", "build"));
        assert!(!task_names_directory("fix the build step in dist", "dist"));
        assert!(!task_names_directory(
            "edit tools/builder/x.rs",
            "tools/build"
        ));
        assert!(!task_names_directory("edit src/build/x.rs", "build"));
        assert!(!task_names_directory("edit tools/build", ""));
    }

    #[test]
    fn an_undefined_name_links_only_weak_or_stray_git_directories_holding_tracked_source() {
        let coverage = coverage(vec![
            dir("tools/build", PruneReason::UndeclaredBuildDir, Some(3)),
            dir("vendor/ledger", PruneReason::Submodule, Some(2)),
            dir("dist", PruneReason::BuildOutput, Some(60)),
            dir("node_modules", PruneReason::Dependencies, Some(0)),
            dir("ext/real", PruneReason::Submodule, Some(0)),
            dir("out/build", PruneReason::UndeclaredBuildDir, None),
        ]);
        assert!(coverage.may_hide_tracked_source());
        let links = coverage.pruned_source_links("fix compile_plan", vec!["compile_plan".into()]);
        let paths = links
            .unresolved_dirs
            .iter()
            .map(|dir| dir.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["tools/build", "vendor/ledger"]);
        assert!(links.named_dirs.is_empty());
        // Reported, never capped: a name the task asks to create is undefined too.
        assert!(!links.caps());
        assert_eq!(links.blocker(), None);
        assert_eq!(
            links.risk_reason().as_deref(),
            Some("named task anchor(s) compile_plan have no indexed definition and may be defined in a directory the index pruned: tools/build/ (3 tracked source files), vendor/ledger/ (2 tracked source files)")
        );
        assert_eq!(
            links.caveats(),
            ["index coverage: compile_plan has no indexed definition and may be in tools/build/ or vendor/ledger/, which the index pruned (tools/build/: undeclared build directory, 3 tracked source files; vendor/ledger/: nested repository, 2 tracked source files); an absence there is not evidence"]
        );
        assert_eq!(
            links.evidence_ids(),
            [
                "coverage:pruned:tools/build",
                "coverage:pruned:vendor/ledger"
            ]
        );
        assert!(links
            .anchor_probe(false)
            .unwrap()
            .contains("may be in tools/build/ or vendor/ledger/, which the index pruned"));
        assert!(links
            .anchor_probe(true)
            .unwrap()
            .ends_with("The index also excluded most of a language's source, so the name may be defined in those files."));

        // Without an undefined name nothing links.
        assert!(coverage
            .pruned_source_links("fix compile_plan", Vec::new())
            .is_empty());
    }

    #[test]
    fn strong_evidence_directories_link_only_when_the_task_names_them_and_never_cap() {
        let coverage = coverage(vec![dir("dist", PruneReason::BuildOutput, Some(60))]);
        assert!(!coverage.may_hide_tracked_source());
        assert!(coverage
            .pruned_source_links("fix renderBundle", vec!["renderBundle".into()])
            .is_empty());
        let named = coverage.pruned_source_links("why does dist/index.js differ", Vec::new());
        assert_eq!(named.named_dirs.len(), 1);
        assert!(!named.caps());
        assert_eq!(named.blocker(), None);
        assert_eq!(named.risk_reason(), None);
        assert_eq!(
            named.caveats(),
            ["index coverage: the task names dist/, which the index pruned (dist/: build output, 60 tracked source files); the index never read those files"]
        );
    }

    #[test]
    fn a_weak_or_stray_git_link_caps_at_low_and_a_strong_one_only_reports() {
        use crate::{Confidence, ConfidenceBreakdown, ConfidenceSignalInput};
        let input = |pruned_source: PrunedSourceLinks| ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 0.8,
            pruned_source,
            ..ConfidenceSignalInput::default()
        };
        let baseline = ConfidenceBreakdown::from_signals(input(PrunedSourceLinks::default()));
        assert!(baseline.overall_score > 0.74, "{baseline:?}");

        let coverage = coverage(vec![
            dir("vendor/ledger", PruneReason::Submodule, Some(2)),
            dir("dist", PruneReason::BuildOutput, Some(60)),
        ]);
        let weak = ConfidenceBreakdown::from_signals(input(
            coverage.pruned_source_links("edit vendor/ledger/lib.rs", Vec::new()),
        ));
        assert!(weak.overall_score <= 0.50, "{weak:?}");
        assert_eq!(weak.overall_enum, Confidence::Low);
        assert!(weak.blockers.contains(
            &"the task names a directory the index pruned: vendor/ledger/ (2 tracked source files)"
                .to_owned()
        ));
        let component = weak
            .components
            .iter()
            .find(|component| component.signal == PRUNED_SOURCE_SIGNAL)
            .expect("the signal");
        assert_eq!(component.weight, 0.0);
        assert_eq!(component.evidence_ids, ["coverage:pruned:vendor/ledger"]);

        // An undefined name is reported with the signal and caps nothing by itself; the
        // unmatched identifier lowers confidence through the `anchor` item instead.
        let named_only = ConfidenceBreakdown::from_signals(input(
            coverage.pruned_source_links("fix ledger_total", vec!["ledger_total".into()]),
        ));
        assert_eq!(named_only.overall_score, baseline.overall_score);
        assert!(named_only
            .components
            .iter()
            .any(|component| component.signal == PRUNED_SOURCE_SIGNAL));
        assert!(named_only
            .caveats
            .iter()
            .any(|caveat| caveat.starts_with("index coverage: ledger_total has no indexed")));
        assert!(named_only
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("pruned")));

        // A strong-evidence directory the task names is reported with its signal, but its
        // caveat, like a coverage gap's, does not trigger the any-caveat cap: the score is the
        // baseline's.
        let strong = ConfidenceBreakdown::from_signals(input(
            coverage.pruned_source_links("why does dist/app.js differ", Vec::new()),
        ));
        assert_eq!(strong.overall_score, baseline.overall_score, "{strong:?}");
        assert!(strong
            .caveats
            .iter()
            .any(|caveat| caveat.starts_with("index coverage: the task names dist/")));
        assert!(strong
            .components
            .iter()
            .any(|component| component.signal == PRUNED_SOURCE_SIGNAL));
        assert!(strong
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("pruned")));
    }

    #[test]
    fn a_secret_like_pruned_directory_is_never_named() {
        let coverage = coverage(vec![
            dir("ops/.ssh/build", PruneReason::UndeclaredBuildDir, Some(4)),
            dir(
                "deploy/.env.d/dist",
                PruneReason::UndeclaredBuildDir,
                Some(1),
            ),
        ]);
        assert!(!coverage.may_hide_tracked_source());
        let links = coverage.pruned_source_links(
            "read ops/.ssh/build/key.rs and deploy/.env.d/dist/x.rs for load_key",
            vec!["load_key".into()],
        );
        assert!(links.is_empty(), "{links:?}");
        assert!(
            NegativeEvidence::for_coverage("task", &CoverageInput::default(), &links).is_none()
        );
    }

    #[test]
    fn the_coverage_item_joins_a_gap_item_or_stands_alone() {
        let coverage = coverage(vec![dir(
            "tools/build",
            PruneReason::UndeclaredBuildDir,
            Some(1),
        )]);
        let links = coverage.pruned_source_links("edit tools/build/plan.py", Vec::new());
        assert!(links.caps());
        let item = NegativeEvidence::for_coverage("task", &CoverageInput::default(), &links)
            .expect("a coverage item");
        assert_eq!(item.scope, negative_evidence_scope::COVERAGE);
        assert!(!item.lowers_confidence());
        assert_eq!(
            item.inspected_sources,
            [
                "index_manifest.quality.coverage.pruned",
                "coverage:pruned:tools/build"
            ]
        );
        assert!(item
            .suggested_next_probe
            .as_deref()
            .unwrap()
            .contains("`[index] keep_dirs`"));
        let unavailable =
            NegativeEvidence::for_coverage("task", &CoverageInput::Unavailable, &links).unwrap();
        assert!(unavailable.reason.contains("index coverage is unrecorded"));
        assert!(unavailable.reason.contains("the task names tools/build/"));
    }
}
