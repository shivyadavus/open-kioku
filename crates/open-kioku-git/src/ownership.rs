use chrono::{DateTime, Duration, Utc};
use globset::Glob;
use open_kioku_core::{
    Confidence, MemorySearchResult, Owner, OwnerSuggestion, OwnershipConfidenceBreakdown,
    OwnershipEvidence, OwnershipReport, OwnershipSourceType, PolicyComponentMatch, ProvenanceTouch,
};
use open_kioku_errors::Result;
use open_kioku_storage::HistoryStore;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

const HISTORY_LIMIT: usize = 100;
const STALE_AFTER_DAYS: i64 = 365;
const CODEOWNERS_SCORE: f32 = 0.70;
const MEMORY_ONLY_CAP: f32 = 0.45;

const CODEOWNERS_CANDIDATES: &[&str] = &[
    ".open-kioku/CODEOWNERS",
    ".github/CODEOWNERS",
    "CODEOWNERS",
    "docs/CODEOWNERS",
    "OWNERS",
];

pub struct OwnershipInput<'a> {
    pub repo: &'a Path,
    pub path: &'a Path,
    pub history: &'a dyn HistoryStore,
    pub memory_facts: &'a [MemorySearchResult],
    pub components: Vec<PolicyComponentMatch>,
}

pub fn ownership_for_path(input: OwnershipInput<'_>) -> Result<OwnershipReport> {
    let generated_at = Utc::now();
    let path = repo_relative_path(input.repo, input.path);
    let mut uncertainty = Vec::new();
    let mut owners = BTreeMap::<String, OwnerAggregate>::new();

    add_codeowners_evidence(
        input.repo,
        &path,
        generated_at,
        &mut owners,
        &mut uncertainty,
    );
    add_git_history_evidence(
        input.history,
        &path,
        generated_at,
        &mut owners,
        &mut uncertainty,
    );
    add_memory_evidence(
        input.memory_facts,
        generated_at,
        &mut owners,
        &mut uncertainty,
    );

    let suggestions = owner_suggestions(owners, &mut uncertainty);
    if suggestions.is_empty() {
        uncertainty.push(format!(
            "no owner suggestions found for `{}` from CODEOWNERS, git history, or repo memory",
            path.display()
        ));
    }
    if suggestions.iter().any(|suggestion| {
        suggestion
            .source_types
            .iter()
            .all(|source| *source == OwnershipSourceType::RepoMemory)
    }) {
        uncertainty.push(
            "memory-only ownership evidence is secondary and uncorroborated by CODEOWNERS or git history"
                .into(),
        );
    }

    Ok(OwnershipReport {
        path,
        components: input.components,
        generated_at,
        owners: suggestions,
        uncertainty,
    })
}

#[derive(Debug, Clone)]
struct CodeownersRule {
    file: PathBuf,
    line_number: usize,
    pattern: String,
    owners: Vec<Owner>,
}

#[derive(Debug, Clone)]
struct OwnerAggregate {
    owner: Owner,
    evidence: Vec<OwnershipEvidence>,
    codeowners: f32,
    git_history: f32,
    memory: f32,
}

impl OwnerAggregate {
    fn new(owner: Owner) -> Self {
        Self {
            owner,
            evidence: Vec::new(),
            codeowners: 0.0,
            git_history: 0.0,
            memory: 0.0,
        }
    }

    fn source_types(&self) -> Vec<OwnershipSourceType> {
        [
            OwnershipSourceType::Codeowners,
            OwnershipSourceType::GitHistory,
            OwnershipSourceType::RepoMemory,
        ]
        .into_iter()
        .filter(|source| {
            self.evidence
                .iter()
                .any(|evidence| evidence.source_type == *source)
        })
        .collect()
    }

    fn has_source(&self, source_type: OwnershipSourceType) -> bool {
        self.evidence
            .iter()
            .any(|evidence| evidence.source_type == source_type)
    }

    fn stale(&self) -> bool {
        !self.evidence.is_empty() && self.evidence.iter().all(|evidence| evidence.stale)
    }
}

fn add_codeowners_evidence(
    repo: &Path,
    path: &Path,
    generated_at: DateTime<Utc>,
    owners: &mut BTreeMap<String, OwnerAggregate>,
    uncertainty: &mut Vec<String>,
) {
    let rules = read_codeowners_rules(repo, uncertainty);
    if rules.is_empty() {
        uncertainty.push("no CODEOWNERS or owner config file was found".into());
        return;
    }

    let mut matched = None;
    for rule in &rules {
        match codeowners_pattern_matches(&rule.pattern, path) {
            Ok(true) => matched = Some(rule),
            Ok(false) => {}
            Err(err) => uncertainty.push(format!(
                "ignored invalid CODEOWNERS pattern `{}` in {}:{}: {err}",
                rule.pattern,
                rule.file.display(),
                rule.line_number
            )),
        }
    }

    let Some(rule) = matched else {
        uncertainty.push(format!(
            "CODEOWNERS files were present but no rule matched `{}`",
            path.display()
        ));
        return;
    };

    for owner in &rule.owners {
        let evidence = OwnershipEvidence {
            source_type: OwnershipSourceType::Codeowners,
            owner: owner.clone(),
            source: format!(
                "{}:{} `{}`",
                rule.file.display(),
                rule.line_number,
                rule.pattern
            ),
            message: format!(
                "CODEOWNERS rule `{}` matched `{}`",
                rule.pattern,
                path.display()
            ),
            confidence: Confidence::High,
            observed_at: Some(generated_at),
            stale: false,
        };
        add_evidence(
            owners,
            owner.clone(),
            evidence,
            OwnershipSourceType::Codeowners,
            CODEOWNERS_SCORE,
        );
    }
}

fn read_codeowners_rules(repo: &Path, uncertainty: &mut Vec<String>) -> Vec<CodeownersRule> {
    let mut rules = Vec::new();
    for candidate in CODEOWNERS_CANDIDATES {
        let path = repo.join(candidate);
        if !path.is_file() {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            uncertainty.push(format!("could not read owner config `{}`", path.display()));
            continue;
        };
        for (index, raw_line) in contents.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(pattern) = parts.next() else {
                continue;
            };
            if pattern.starts_with('!') {
                uncertainty.push(format!(
                    "ignored unsupported negative CODEOWNERS pattern `{pattern}` in {}:{line_number}",
                    path.display()
                ));
                continue;
            }
            let mut rule_owners = Vec::new();
            for token in parts {
                if token.starts_with('#') {
                    break;
                }
                if let Some(owner) = owner_from_token(token) {
                    rule_owners.push(owner);
                }
            }
            if rule_owners.is_empty() {
                uncertainty.push(format!(
                    "ignored CODEOWNERS rule `{}` in {}:{} because it has no owner",
                    pattern,
                    path.display(),
                    line_number
                ));
                continue;
            }
            rules.push(CodeownersRule {
                file: PathBuf::from(candidate),
                line_number,
                pattern: pattern.to_string(),
                owners: rule_owners,
            });
        }
    }
    rules
}

fn codeowners_pattern_matches(pattern: &str, path: &Path) -> Result<bool> {
    let normalized_path = normalize_path_for_glob(path);
    for candidate in codeowners_globs(pattern) {
        let matcher = Glob::new(&candidate)
            .map_err(|err| open_kioku_errors::OkError::Config(err.to_string()))?
            .compile_matcher();
        if matcher.is_match(Path::new(&normalized_path)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn codeowners_globs(pattern: &str) -> Vec<String> {
    let anchored = pattern.starts_with('/');
    let directory = pattern.ends_with('/');
    let mut normalized = pattern
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if normalized.is_empty() {
        normalized = "**".into();
    }
    if directory {
        normalized.push_str("/**");
    }

    let mut candidates = Vec::new();
    if anchored {
        candidates.push(normalized);
    } else {
        candidates.push(normalized.clone());
        candidates.push(format!("**/{normalized}"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn add_git_history_evidence(
    history: &dyn HistoryStore,
    path: &Path,
    generated_at: DateTime<Utc>,
    owners: &mut BTreeMap<String, OwnerAggregate>,
    uncertainty: &mut Vec<String>,
) {
    let provenance = match history.provenance_for_path(path, HISTORY_LIMIT) {
        Ok(provenance) => provenance,
        Err(err) => {
            uncertainty.push(format!("git history ownership evidence unavailable: {err}"));
            return;
        }
    };
    uncertainty.extend(provenance.uncertainty.iter().cloned());
    if provenance.truncated {
        uncertainty.push(format!(
            "git history ownership evidence for `{}` is truncated at {HISTORY_LIMIT} touches",
            path.display()
        ));
    }

    let touches = unique_touches(&provenance.recent_touches);
    if touches.is_empty() {
        uncertainty.push(format!(
            "no git author touches were available for `{}`",
            path.display()
        ));
        return;
    }

    let total = touches.len() as f32;
    let mut by_owner = BTreeMap::<String, GitOwnerStats>::new();
    for touch in touches {
        let key = owner_key(&touch.commit.author);
        let entry = by_owner
            .entry(key)
            .or_insert_with(|| GitOwnerStats::new(touch.commit.author.clone()));
        entry.count += 1;
        entry.latest = entry.latest.max(Some(touch.commit.committed_at));
        entry.latest_commit = Some(touch.commit.id.0.clone());
        entry.latest_summary = Some(touch.commit.summary.clone());
    }

    for stats in by_owner.into_values() {
        let share = stats.count as f32 / total;
        let count_factor = 0.60 + ((stats.count as f32 / 3.0).min(1.0) * 0.40);
        let observed_at = stats.latest.unwrap_or(generated_at);
        let stale = is_stale(generated_at, observed_at);
        let freshness_multiplier = if stale { 0.55 } else { 1.0 };
        let git_score = ((0.30 + (0.32 * share)) * count_factor * freshness_multiplier).min(0.62);
        let message = format!(
            "{} authored {} of {} persisted touch(es) for `{}`; latest `{}`",
            stats.owner.name,
            stats.count,
            total as usize,
            path.display(),
            stats
                .latest_summary
                .as_deref()
                .unwrap_or("unknown commit summary")
        );
        let evidence = OwnershipEvidence {
            source_type: OwnershipSourceType::GitHistory,
            owner: stats.owner.clone(),
            source: format!(
                "git history:{}",
                stats.latest_commit.as_deref().unwrap_or("unknown")
            ),
            message,
            confidence: Confidence::from_score(git_score),
            observed_at: Some(observed_at),
            stale,
        };
        add_evidence(
            owners,
            stats.owner,
            evidence,
            OwnershipSourceType::GitHistory,
            git_score,
        );
    }
}

#[derive(Debug)]
struct GitOwnerStats {
    owner: Owner,
    count: usize,
    latest: Option<DateTime<Utc>>,
    latest_commit: Option<String>,
    latest_summary: Option<String>,
}

impl GitOwnerStats {
    fn new(owner: Owner) -> Self {
        Self {
            owner,
            count: 0,
            latest: None,
            latest_commit: None,
            latest_summary: None,
        }
    }
}

fn unique_touches(touches: &[ProvenanceTouch]) -> Vec<&ProvenanceTouch> {
    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    for touch in touches {
        let key = format!(
            "{}:{}:{}",
            touch.commit.id.0,
            touch.path.display(),
            touch.qualified_name.as_deref().unwrap_or("<file>")
        );
        if seen.insert(key) {
            unique.push(touch);
        }
    }
    unique
}

fn add_memory_evidence(
    memory_facts: &[MemorySearchResult],
    generated_at: DateTime<Utc>,
    owners: &mut BTreeMap<String, OwnerAggregate>,
    uncertainty: &mut Vec<String>,
) {
    if memory_facts.is_empty() {
        uncertainty.push("no repo memory ownership facts matched this path".into());
        return;
    }

    let mut owner_hits = 0;
    for result in memory_facts {
        let fact_owners = memory_owner_tokens(&result.fact.text);
        if fact_owners.is_empty() {
            continue;
        }
        owner_hits += fact_owners.len();
        for owner in fact_owners {
            let stale = is_stale(generated_at, result.fact.created_at);
            let memory_score = ((0.08 + 0.10 * result.score.clamp(0.0, 1.0))
                * result.fact.confidence.score())
            .min(0.22);
            let evidence = OwnershipEvidence {
                source_type: OwnershipSourceType::RepoMemory,
                owner: owner.clone(),
                source: result.fact.id.0.clone(),
                message: format!(
                    "repo memory matched ownership terms: {}; source `{}`",
                    result.match_reason, result.fact.source
                ),
                confidence: Confidence::from_score(memory_score),
                observed_at: Some(result.fact.created_at),
                stale,
            };
            add_evidence(
                owners,
                owner,
                evidence,
                OwnershipSourceType::RepoMemory,
                memory_score,
            );
        }
    }

    if owner_hits == 0 {
        uncertainty.push(
            "repo memory matched this path but did not contain owner handles or email tokens"
                .into(),
        );
    }
}

fn add_evidence(
    owners: &mut BTreeMap<String, OwnerAggregate>,
    owner: Owner,
    evidence: OwnershipEvidence,
    source_type: OwnershipSourceType,
    score: f32,
) {
    let key = owner_key(&owner);
    let entry = owners
        .entry(key)
        .or_insert_with(|| OwnerAggregate::new(owner));
    match source_type {
        OwnershipSourceType::Codeowners => entry.codeowners = entry.codeowners.max(score),
        OwnershipSourceType::GitHistory => entry.git_history = entry.git_history.max(score),
        OwnershipSourceType::RepoMemory => entry.memory = (entry.memory + score).min(0.22),
    }
    entry.evidence.push(evidence);
}

fn owner_suggestions(
    owners: BTreeMap<String, OwnerAggregate>,
    uncertainty: &mut Vec<String>,
) -> Vec<OwnerSuggestion> {
    let mut drafts = owners
        .into_values()
        .map(|owner| {
            let freshness = if owner.evidence.iter().any(|evidence| !evidence.stale) {
                0.08
            } else {
                0.0
            };
            let mut raw_score =
                (owner.codeowners + owner.git_history + owner.memory + freshness).min(1.0);
            if !owner.has_source(OwnershipSourceType::Codeowners)
                && !owner.has_source(OwnershipSourceType::GitHistory)
            {
                raw_score = raw_score.min(MEMORY_ONLY_CAP);
            }
            SuggestionDraft {
                owner,
                freshness,
                raw_score,
                ambiguity_penalty: 0.0,
            }
        })
        .collect::<Vec<_>>();

    drafts.sort_by(compare_drafts);
    if let Some(top_score) = drafts.first().map(|draft| draft.raw_score) {
        let close_without_codeowners = drafts
            .iter()
            .filter(|draft| {
                !draft.owner.has_source(OwnershipSourceType::Codeowners)
                    && (top_score - draft.raw_score).abs() <= 0.08
            })
            .count();
        if close_without_codeowners > 1 {
            uncertainty.push(format!(
                "ownership is ambiguous across {close_without_codeowners} similarly scored non-CODEOWNERS owner candidates"
            ));
            for draft in &mut drafts {
                if !draft.owner.has_source(OwnershipSourceType::Codeowners)
                    && (top_score - draft.raw_score).abs() <= 0.08
                {
                    draft.ambiguity_penalty = 0.12;
                }
            }
        }
    }

    let mut suggestions = drafts
        .into_iter()
        .map(|draft| {
            let final_score = (draft.raw_score - draft.ambiguity_penalty).clamp(0.0, 1.0);
            let source_types = draft.owner.source_types();
            let stale = draft.owner.stale();
            let rationale = ownership_rationale(&source_types, stale);
            OwnerSuggestion {
                owner: draft.owner.owner,
                rationale,
                confidence: Confidence::from_score(final_score),
                score: final_score,
                source_types,
                stale,
                evidence: draft.owner.evidence,
                confidence_breakdown: OwnershipConfidenceBreakdown {
                    codeowners: draft.owner.codeowners,
                    git_history: draft.owner.git_history,
                    memory: draft.owner.memory,
                    freshness: draft.freshness,
                    ambiguity_penalty: draft.ambiguity_penalty,
                    final_score,
                },
            }
        })
        .collect::<Vec<_>>();
    suggestions.sort_by(compare_suggestions);
    suggestions
}

struct SuggestionDraft {
    owner: OwnerAggregate,
    freshness: f32,
    raw_score: f32,
    ambiguity_penalty: f32,
}

fn compare_drafts(left: &SuggestionDraft, right: &SuggestionDraft) -> Ordering {
    right
        .raw_score
        .partial_cmp(&left.raw_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.owner.owner.name.cmp(&right.owner.owner.name))
}

fn compare_suggestions(left: &OwnerSuggestion, right: &OwnerSuggestion) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.owner.name.cmp(&right.owner.name))
}

fn ownership_rationale(source_types: &[OwnershipSourceType], stale: bool) -> String {
    let mut parts = Vec::new();
    if source_types.contains(&OwnershipSourceType::Codeowners) {
        parts.push("CODEOWNERS matched the queried path");
    }
    if source_types.contains(&OwnershipSourceType::GitHistory) {
        parts.push("local git history shows author touch evidence");
    }
    if source_types.contains(&OwnershipSourceType::RepoMemory) {
        parts.push("repo memory contributed secondary ownership evidence");
    }
    if stale {
        parts.push("all ownership evidence is stale");
    }
    if parts.is_empty() {
        "ownership evidence is unavailable".into()
    } else {
        parts.join("; ")
    }
}

fn memory_owner_tokens(text: &str) -> Vec<Owner> {
    let mut owners = Vec::new();
    let mut seen = BTreeSet::new();
    for token in text.split_whitespace() {
        if let Some(owner) = owner_from_token(token) {
            let key = owner_key(&owner);
            if seen.insert(key) {
                owners.push(owner);
            }
        }
    }
    owners
}

fn owner_from_token(token: &str) -> Option<Owner> {
    let cleaned = token.trim_matches(|ch: char| {
        matches!(
            ch,
            ',' | ';' | ':' | '.' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\''
        )
    });
    if cleaned.len() < 2 {
        return None;
    }
    if cleaned.starts_with('@') && cleaned.len() > 1 {
        return Some(Owner {
            name: cleaned.to_string(),
            email: None,
        });
    }
    if looks_like_email(cleaned) {
        return Some(Owner {
            name: cleaned.to_string(),
            email: Some(cleaned.to_string()),
        });
    }
    None
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

fn owner_key(owner: &Owner) -> String {
    owner
        .email
        .as_deref()
        .unwrap_or(&owner.name)
        .trim_start_matches('@')
        .to_ascii_lowercase()
}

fn is_stale(generated_at: DateTime<Utc>, observed_at: DateTime<Utc>) -> bool {
    generated_at.signed_duration_since(observed_at) > Duration::days(STALE_AFTER_DAYS)
}

fn repo_relative_path(repo: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Ok(relative) = path.strip_prefix(repo) {
            return clean_relative_path(relative);
        }
        return clean_relative_path(&path);
    }
    clean_relative_path(path)
}

fn clean_relative_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => cleaned.push(value),
            Component::ParentDir => cleaned.push(".."),
            Component::RootDir | Component::Prefix(_) => cleaned.push(component.as_os_str()),
        }
    }
    if cleaned.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        cleaned
    }
}

fn normalize_path_for_glob(path: &Path) -> String {
    clean_relative_path(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use open_kioku_core::{
        FileProvenance, GitChangeKind, GitCochangeEdge, GitCommitId, GitCommitRecord,
        HistorySnapshot, HistorySummary, MemoryFact, MemoryFactId, ProvenanceTouch,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubHistoryStore {
        provenance: Mutex<Option<FileProvenance>>,
    }

    impl StubHistoryStore {
        fn with_provenance(provenance: FileProvenance) -> Self {
            Self {
                provenance: Mutex::new(Some(provenance)),
            }
        }
    }

    impl HistoryStore for StubHistoryStore {
        fn put_history_snapshot(&self, _snapshot: &HistorySnapshot) -> Result<()> {
            Ok(())
        }

        fn history_for_file(&self, path: &Path, _limit: usize) -> Result<HistorySummary> {
            Ok(HistorySummary::empty(path))
        }

        fn provenance_for_path(&self, path: &Path, _limit: usize) -> Result<FileProvenance> {
            Ok(self
                .provenance
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| empty_provenance(path)))
        }

        fn cochange_neighbors(&self, _path: &Path, _limit: usize) -> Result<Vec<GitCochangeEdge>> {
            Ok(Vec::new())
        }

        fn recent_commits(&self, _limit: usize) -> Result<Vec<GitCommitRecord>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn codeowners_outranks_weak_memory_only_evidence() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".github")).unwrap();
        fs::write(
            dir.path().join(".github/CODEOWNERS"),
            "src/** @platform-team\n",
        )
        .unwrap();
        let history = StubHistoryStore::default();
        let memory = vec![memory_result(
            "src/a.rs owner @memory-team",
            Confidence::High,
        )];

        let report = ownership_for_path(OwnershipInput {
            repo: dir.path(),
            path: Path::new("src/a.rs"),
            history: &history,
            memory_facts: &memory,
            components: Vec::new(),
        })
        .unwrap();

        assert_eq!(report.owners[0].owner.name, "@platform-team");
        assert_eq!(report.owners[0].confidence, Confidence::High);
        assert!(report.owners[0]
            .source_types
            .contains(&OwnershipSourceType::Codeowners));
        let memory_owner = report
            .owners
            .iter()
            .find(|owner| owner.owner.name == "@memory-team")
            .unwrap();
        assert!(report.owners[0].score > memory_owner.score);
        assert_eq!(memory_owner.confidence, Confidence::Low);
    }

    #[test]
    fn source_mixing_raises_confidence_for_same_owner() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("CODEOWNERS"), "src/** dev@example.com\n").unwrap();
        let history = StubHistoryStore::with_provenance(FileProvenance {
            path: PathBuf::from("src/a.rs"),
            first_seen: None,
            last_touched: None,
            recent_touches: vec![touch("one", "Dev", "dev@example.com", 2026, 6, 1)],
            confidence: Confidence::High,
            truncated: false,
            uncertainty: Vec::new(),
        });
        let memory = vec![memory_result(
            "src/a.rs maintainer dev@example.com",
            Confidence::High,
        )];

        let report = ownership_for_path(OwnershipInput {
            repo: dir.path(),
            path: Path::new("src/a.rs"),
            history: &history,
            memory_facts: &memory,
            components: Vec::new(),
        })
        .unwrap();

        let owner = &report.owners[0];
        assert_eq!(owner.owner.email.as_deref(), Some("dev@example.com"));
        assert!(owner
            .source_types
            .contains(&OwnershipSourceType::Codeowners));
        assert!(owner
            .source_types
            .contains(&OwnershipSourceType::GitHistory));
        assert!(owner
            .source_types
            .contains(&OwnershipSourceType::RepoMemory));
        assert!(owner.score >= 0.95);
    }

    #[test]
    fn stale_ambiguous_git_history_is_not_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        let history = StubHistoryStore::with_provenance(FileProvenance {
            path: PathBuf::from("src/a.rs"),
            first_seen: None,
            last_touched: None,
            recent_touches: vec![
                touch("one", "Old One", "one@example.com", 2020, 1, 1),
                touch("two", "Old Two", "two@example.com", 2020, 1, 2),
            ],
            confidence: Confidence::Medium,
            truncated: false,
            uncertainty: Vec::new(),
        });

        let report = ownership_for_path(OwnershipInput {
            repo: dir.path(),
            path: Path::new("src/a.rs"),
            history: &history,
            memory_facts: &[],
            components: Vec::new(),
        })
        .unwrap();

        assert_eq!(report.owners.len(), 2);
        assert!(report.owners.iter().all(|owner| owner.stale));
        assert!(report
            .owners
            .iter()
            .all(|owner| owner.confidence == Confidence::Low));
        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("ambiguous")));
    }

    #[test]
    fn missing_ownership_returns_uncertainty_without_fabricating_owner() {
        let dir = tempfile::tempdir().unwrap();
        let history = StubHistoryStore::default();

        let report = ownership_for_path(OwnershipInput {
            repo: dir.path(),
            path: Path::new("src/a.rs"),
            history: &history,
            memory_facts: &[],
            components: Vec::new(),
        })
        .unwrap();

        assert!(report.owners.is_empty());
        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("no owner suggestions")));
    }

    #[test]
    fn invalid_codeowners_pattern_is_reported_as_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("CODEOWNERS"), "[ @team\n").unwrap();
        let history = StubHistoryStore::default();

        let report = ownership_for_path(OwnershipInput {
            repo: dir.path(),
            path: Path::new("src/a.rs"),
            history: &history,
            memory_facts: &[],
            components: Vec::new(),
        })
        .unwrap();

        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("invalid CODEOWNERS pattern")));
    }

    fn empty_provenance(path: &Path) -> FileProvenance {
        FileProvenance {
            path: path.to_path_buf(),
            first_seen: None,
            last_touched: None,
            recent_touches: Vec::new(),
            confidence: Confidence::Low,
            truncated: false,
            uncertainty: Vec::new(),
        }
    }

    fn touch(
        id: &str,
        name: &str,
        email: &str,
        year: i32,
        month: u32,
        day: u32,
    ) -> ProvenanceTouch {
        let timestamp = Utc
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .unwrap();
        ProvenanceTouch {
            commit: GitCommitRecord {
                id: GitCommitId::new(id),
                parent_ids: Vec::new(),
                author: Owner {
                    name: name.into(),
                    email: Some(email.into()),
                },
                committer: None,
                authored_at: timestamp,
                committed_at: timestamp,
                summary: format!("commit {id}"),
                message: format!("commit {id}"),
                file_count: 1,
            },
            path: PathBuf::from("src/a.rs"),
            previous_path: None,
            symbol_id: None,
            qualified_name: None,
            change_kind: GitChangeKind::Modified,
            line_ranges: Vec::new(),
            confidence: Confidence::High,
            uncertainty: Vec::new(),
        }
    }

    fn memory_result(text: &str, confidence: Confidence) -> MemorySearchResult {
        MemorySearchResult {
            fact: MemoryFact {
                id: MemoryFactId::new(format!("memory:{}", text.len())),
                text: text.into(),
                source: "test".into(),
                confidence,
                entities: Vec::new(),
                created_at: Utc::now(),
            },
            score: 0.50,
            match_reason: "test memory match".into(),
            evidence: vec!["test".into()],
        }
    }
}
