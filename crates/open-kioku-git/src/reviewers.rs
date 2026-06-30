use chrono::{DateTime, Duration, Utc};
use open_kioku_core::{
    Confidence, Owner, OwnershipReport, OwnershipSourceType, ProvenanceTouch, ReviewerAvailability,
    ReviewerConfidenceBreakdown, ReviewerRole, ReviewerSignal, ReviewerSignalSourceType,
    ReviewerSuggestion, ReviewerSuggestionReport,
};
use open_kioku_errors::Result;
use open_kioku_storage::HistoryStore;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const HISTORY_LIMIT: usize = 100;
const STALE_AFTER_DAYS: i64 = 365;

pub struct ReviewerSuggestionInput<'a> {
    pub path: &'a Path,
    pub history: &'a dyn HistoryStore,
    pub ownership: Option<&'a OwnershipReport>,
}

pub fn suggest_reviewers(input: ReviewerSuggestionInput<'_>) -> Result<ReviewerSuggestionReport> {
    let generated_at = Utc::now();
    let path = input.path.to_path_buf();
    let mut uncertainty = Vec::new();
    let mut reviewers = BTreeMap::<String, ReviewerAggregate>::new();
    let mut saw_actual_review_evidence = false;

    match input.history.history_for_file(input.path, HISTORY_LIMIT) {
        Ok(summary) => {
            saw_actual_review_evidence = summary
                .reviewer_evidence
                .iter()
                .any(|evidence| is_actual_review_role(evidence.role));
            uncertainty.extend(summary.uncertainty.iter().cloned());
            for evidence in summary.reviewer_evidence {
                let actual_review_evidence = is_actual_review_role(evidence.role);
                let stale = is_stale(generated_at, evidence.observed_at);
                let score = reviewer_evidence_score(evidence.role, evidence.confidence, stale);
                let signal = ReviewerSignal {
                    source_type: ReviewerSignalSourceType::ReviewEvidence,
                    reviewer: evidence.reviewer.clone(),
                    source: evidence.source.clone(),
                    role: Some(evidence.role),
                    message: reviewer_evidence_message(
                        input.path,
                        evidence.role,
                        actual_review_evidence,
                    ),
                    confidence: Confidence::from_score(score),
                    observed_at: Some(evidence.observed_at),
                    stale,
                    actual_review_evidence,
                };
                add_signal(&mut reviewers, evidence.reviewer, signal, score);
            }
        }
        Err(err) => uncertainty.push(format!("reviewer history lookup failed: {err}")),
    }

    add_ownership_signals(
        input.ownership,
        generated_at,
        &mut reviewers,
        &mut uncertainty,
    );
    add_author_signals(
        input.history,
        input.path,
        generated_at,
        &mut reviewers,
        &mut uncertainty,
    );

    let suggestions = reviewer_suggestions(reviewers, &mut uncertainty);
    let availability = report_availability(&suggestions);

    if !saw_actual_review_evidence {
        uncertainty.push(
            "actual PR-review evidence is unavailable in the local index; suggestions are inferred from ownership and/or author history"
                .into(),
        );
    }
    if suggestions.is_empty() {
        uncertainty.push(format!(
            "no reviewer suggestions found for `{}` from review evidence, ownership, or git author history",
            path.display()
        ));
    }

    Ok(ReviewerSuggestionReport {
        path,
        generated_at,
        availability,
        suggestions,
        uncertainty,
    })
}

#[derive(Debug, Clone)]
struct ReviewerAggregate {
    reviewer: Owner,
    signals: Vec<ReviewerSignal>,
    review_evidence: f32,
    ownership: f32,
    author_history: f32,
}

impl ReviewerAggregate {
    fn new(reviewer: Owner) -> Self {
        Self {
            reviewer,
            signals: Vec::new(),
            review_evidence: 0.0,
            ownership: 0.0,
            author_history: 0.0,
        }
    }

    fn actual_review_evidence(&self) -> bool {
        self.signals
            .iter()
            .any(|signal| signal.actual_review_evidence)
    }

    fn inferred_from_authors(&self) -> bool {
        self.has_author_inference() && !self.actual_review_evidence()
    }

    fn stale(&self) -> bool {
        !self.signals.is_empty() && self.signals.iter().all(|signal| signal.stale)
    }

    fn has_author_inference(&self) -> bool {
        self.author_history > 0.0
            || self.signals.iter().any(|signal| {
                !signal.actual_review_evidence
                    && matches!(
                        signal.role,
                        Some(ReviewerRole::Author | ReviewerRole::Committer)
                    )
            })
    }

    fn has_ownership_inference(&self) -> bool {
        self.ownership > 0.0
            || self.signals.iter().any(|signal| {
                !signal.actual_review_evidence && signal.role == Some(ReviewerRole::Owner)
            })
    }

    fn source_types(&self) -> Vec<ReviewerSignalSourceType> {
        [
            ReviewerSignalSourceType::ReviewEvidence,
            ReviewerSignalSourceType::Ownership,
            ReviewerSignalSourceType::GitAuthor,
        ]
        .into_iter()
        .filter(|source| {
            self.signals
                .iter()
                .any(|signal| signal.source_type == *source)
        })
        .collect()
    }
}

fn add_signal(
    reviewers: &mut BTreeMap<String, ReviewerAggregate>,
    reviewer: Owner,
    signal: ReviewerSignal,
    score: f32,
) {
    let key = reviewer_key(&reviewer);
    let entry = reviewers
        .entry(key)
        .or_insert_with(|| ReviewerAggregate::new(reviewer));
    match signal.source_type {
        ReviewerSignalSourceType::ReviewEvidence => {
            entry.review_evidence = entry.review_evidence.max(score);
        }
        ReviewerSignalSourceType::Ownership => {
            entry.ownership = entry.ownership.max(score);
        }
        ReviewerSignalSourceType::GitAuthor => {
            entry.author_history = entry.author_history.max(score);
        }
    }
    entry.signals.push(signal);
}

fn add_ownership_signals(
    ownership: Option<&OwnershipReport>,
    generated_at: DateTime<Utc>,
    reviewers: &mut BTreeMap<String, ReviewerAggregate>,
    uncertainty: &mut Vec<String>,
) {
    let Some(ownership) = ownership else {
        uncertainty.push("ownership evidence was not provided for reviewer suggestions".into());
        return;
    };
    uncertainty.extend(ownership.uncertainty.iter().cloned());
    if ownership.owners.is_empty() {
        uncertainty.push("ownership lookup returned no owner suggestions".into());
        return;
    }

    for owner in &ownership.owners {
        let ownership_weight = if owner
            .source_types
            .contains(&OwnershipSourceType::Codeowners)
        {
            0.58
        } else if owner
            .source_types
            .contains(&OwnershipSourceType::GitHistory)
        {
            0.46
        } else {
            0.30
        };
        let score = (owner.score * ownership_weight).min(0.62);
        let signal = ReviewerSignal {
            source_type: ReviewerSignalSourceType::Ownership,
            reviewer: owner.owner.clone(),
            source: format!("ownership:{}", ownership.path.display()),
            role: Some(ReviewerRole::Owner),
            message: format!(
                "ownership lookup suggested this reviewer candidate: {}",
                owner.rationale
            ),
            confidence: Confidence::from_score(score),
            observed_at: Some(generated_at),
            stale: owner.stale,
            actual_review_evidence: false,
        };
        add_signal(reviewers, owner.owner.clone(), signal, score);
    }
}

fn add_author_signals(
    history: &dyn HistoryStore,
    path: &Path,
    generated_at: DateTime<Utc>,
    reviewers: &mut BTreeMap<String, ReviewerAggregate>,
    uncertainty: &mut Vec<String>,
) {
    let provenance = match history.provenance_for_path(path, HISTORY_LIMIT) {
        Ok(provenance) => provenance,
        Err(err) => {
            uncertainty.push(format!("author history lookup failed: {err}"));
            return;
        }
    };
    uncertainty.extend(provenance.uncertainty.iter().cloned());
    if provenance.truncated {
        uncertainty.push(format!(
            "author history reviewer evidence for `{}` is truncated at {HISTORY_LIMIT} touches",
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
    let mut by_author = BTreeMap::<String, AuthorStats>::new();
    for touch in touches {
        let key = reviewer_key(&touch.commit.author);
        let entry = by_author
            .entry(key)
            .or_insert_with(|| AuthorStats::new(touch.commit.author.clone()));
        entry.count += 1;
        entry.latest = entry.latest.max(Some(touch.commit.committed_at));
        entry.latest_commit = Some(touch.commit.id.0.clone());
        entry.latest_summary = Some(touch.commit.summary.clone());
    }

    for stats in by_author.into_values() {
        let share = stats.count as f32 / total;
        let count_factor = 0.60 + ((stats.count as f32 / 3.0).min(1.0) * 0.40);
        let observed_at = stats.latest.unwrap_or(generated_at);
        let stale = is_stale(generated_at, observed_at);
        let freshness_multiplier = if stale { 0.55 } else { 1.0 };
        let score = ((0.20 + (0.30 * share)) * count_factor * freshness_multiplier).min(0.52);
        let signal = ReviewerSignal {
            source_type: ReviewerSignalSourceType::GitAuthor,
            reviewer: stats.author.clone(),
            source: format!(
                "git author:{}",
                stats.latest_commit.as_deref().unwrap_or("unknown")
            ),
            role: Some(ReviewerRole::Author),
            message: format!(
                "{} authored {} of {} persisted touch(es) for `{}`; latest `{}`",
                stats.author.name,
                stats.count,
                total as usize,
                path.display(),
                stats
                    .latest_summary
                    .as_deref()
                    .unwrap_or("unknown commit summary")
            ),
            confidence: Confidence::from_score(score),
            observed_at: Some(observed_at),
            stale,
            actual_review_evidence: false,
        };
        add_signal(reviewers, stats.author, signal, score);
    }
}

#[derive(Debug)]
struct AuthorStats {
    author: Owner,
    count: usize,
    latest: Option<DateTime<Utc>>,
    latest_commit: Option<String>,
    latest_summary: Option<String>,
}

impl AuthorStats {
    fn new(author: Owner) -> Self {
        Self {
            author,
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

fn reviewer_suggestions(
    reviewers: BTreeMap<String, ReviewerAggregate>,
    uncertainty: &mut Vec<String>,
) -> Vec<ReviewerSuggestion> {
    let mut drafts = reviewers
        .into_values()
        .map(|reviewer| {
            let freshness = if reviewer.signals.iter().any(|signal| !signal.stale) {
                0.05
            } else {
                0.0
            };
            let mut raw_score = (reviewer.review_evidence
                + reviewer.ownership
                + reviewer.author_history
                + freshness)
                .min(1.0);
            if !reviewer.actual_review_evidence() {
                raw_score = raw_score.min(inferred_cap(&reviewer));
            }
            ReviewerDraft {
                reviewer,
                freshness,
                raw_score,
                ambiguity_penalty: 0.0,
            }
        })
        .collect::<Vec<_>>();

    drafts.sort_by(compare_drafts);
    if let Some(top_score) = drafts.first().map(|draft| draft.raw_score) {
        let close_inferred = drafts
            .iter()
            .filter(|draft| {
                !draft.reviewer.actual_review_evidence()
                    && (top_score - draft.raw_score).abs() <= 0.06
            })
            .count();
        if close_inferred > 1 {
            uncertainty.push(format!(
                "reviewer suggestions are ambiguous across {close_inferred} similarly scored inferred candidates"
            ));
            for draft in &mut drafts {
                if !draft.reviewer.actual_review_evidence()
                    && (top_score - draft.raw_score).abs() <= 0.06
                {
                    draft.ambiguity_penalty = 0.08;
                }
            }
        }
    }

    let mut suggestions = drafts
        .into_iter()
        .map(|draft| {
            let final_score = (draft.raw_score - draft.ambiguity_penalty).clamp(0.0, 1.0);
            let actual_review_evidence = draft.reviewer.actual_review_evidence();
            let availability = suggestion_availability(&draft.reviewer);
            ReviewerSuggestion {
                reviewer: draft.reviewer.reviewer.clone(),
                rationale: reviewer_rationale(&draft.reviewer, availability),
                confidence: Confidence::from_score(final_score),
                score: final_score,
                availability,
                source_types: draft.reviewer.source_types(),
                inferred_from_authors: draft.reviewer.inferred_from_authors(),
                actual_review_evidence,
                stale: draft.reviewer.stale(),
                signals: draft.reviewer.signals,
                confidence_breakdown: ReviewerConfidenceBreakdown {
                    review_evidence: draft.reviewer.review_evidence,
                    ownership: draft.reviewer.ownership,
                    author_history: draft.reviewer.author_history,
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

struct ReviewerDraft {
    reviewer: ReviewerAggregate,
    freshness: f32,
    raw_score: f32,
    ambiguity_penalty: f32,
}

fn inferred_cap(reviewer: &ReviewerAggregate) -> f32 {
    match (
        reviewer.has_ownership_inference(),
        reviewer.has_author_inference(),
    ) {
        (true, true) => 0.78,
        (true, false) => 0.68,
        (false, true) => 0.62,
        (false, false) => 0.0,
    }
}

fn suggestion_availability(reviewer: &ReviewerAggregate) -> ReviewerAvailability {
    if reviewer.actual_review_evidence() {
        ReviewerAvailability::ActualReviewEvidence
    } else {
        match (
            reviewer.has_ownership_inference(),
            reviewer.has_author_inference(),
        ) {
            (true, true) => ReviewerAvailability::InferredFromOwnershipAndAuthors,
            (true, false) => ReviewerAvailability::InferredFromOwnership,
            (false, true) => ReviewerAvailability::InferredFromAuthors,
            (false, false) => ReviewerAvailability::Unavailable,
        }
    }
}

fn report_availability(suggestions: &[ReviewerSuggestion]) -> ReviewerAvailability {
    if suggestions
        .iter()
        .any(|suggestion| suggestion.actual_review_evidence)
    {
        return ReviewerAvailability::ActualReviewEvidence;
    }
    if suggestions.is_empty() {
        return ReviewerAvailability::Unavailable;
    }
    if suggestions.iter().any(|suggestion| {
        suggestion.availability == ReviewerAvailability::InferredFromOwnershipAndAuthors
    }) {
        return ReviewerAvailability::InferredFromOwnershipAndAuthors;
    }
    if suggestions
        .iter()
        .any(|suggestion| suggestion.availability == ReviewerAvailability::InferredFromOwnership)
    {
        return ReviewerAvailability::InferredFromOwnership;
    }
    ReviewerAvailability::InferredFromAuthors
}

fn reviewer_rationale(reviewer: &ReviewerAggregate, availability: ReviewerAvailability) -> String {
    let mut parts = Vec::new();
    if reviewer.actual_review_evidence() {
        parts.push("stored review/approval evidence exists for this path");
    }
    if reviewer.ownership > 0.0 {
        parts.push("ownership lookup supports this reviewer candidate");
    }
    if reviewer.author_history > 0.0 {
        parts.push("local git author history supports this reviewer candidate");
    }
    if reviewer.signals.iter().any(|signal| {
        !signal.actual_review_evidence
            && matches!(
                signal.role,
                Some(ReviewerRole::Author | ReviewerRole::Committer)
            )
    }) {
        parts.push("stored author or committer evidence supports this reviewer candidate");
    }
    if reviewer
        .signals
        .iter()
        .any(|signal| !signal.actual_review_evidence && signal.role == Some(ReviewerRole::Owner))
    {
        parts.push("stored owner evidence supports this reviewer candidate");
    }
    if !matches!(availability, ReviewerAvailability::ActualReviewEvidence) {
        parts.push("actual PR-review evidence was unavailable, so this is inferred");
    }
    if reviewer.stale() {
        parts.push("all reviewer evidence is stale");
    }
    if parts.is_empty() {
        "reviewer evidence is unavailable".into()
    } else {
        parts.join("; ")
    }
}

fn compare_drafts(left: &ReviewerDraft, right: &ReviewerDraft) -> Ordering {
    right
        .raw_score
        .partial_cmp(&left.raw_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            left.reviewer
                .reviewer
                .name
                .cmp(&right.reviewer.reviewer.name)
        })
}

fn compare_suggestions(left: &ReviewerSuggestion, right: &ReviewerSuggestion) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.reviewer.name.cmp(&right.reviewer.name))
        .then_with(|| left.reviewer.email.cmp(&right.reviewer.email))
}

fn reviewer_evidence_score(role: ReviewerRole, confidence: Confidence, stale: bool) -> f32 {
    let base = match role {
        ReviewerRole::Approver => 0.78,
        ReviewerRole::Reviewer => 0.72,
        ReviewerRole::Owner => 0.50,
        ReviewerRole::Committer => 0.40,
        ReviewerRole::Author => 0.35,
    };
    let freshness_multiplier = if stale { 0.65 } else { 1.0 };
    ((base + (0.10 * confidence.score())) * freshness_multiplier).min(0.92)
}

fn reviewer_evidence_message(path: &Path, role: ReviewerRole, actual: bool) -> String {
    if actual {
        format!(
            "stored {role:?} review evidence matched `{}`",
            path.display()
        )
    } else {
        format!(
            "stored reviewer-adjacent {:?} evidence matched `{}` but is not treated as actual PR-review evidence",
            role,
            path.display()
        )
    }
}

fn is_actual_review_role(role: ReviewerRole) -> bool {
    matches!(role, ReviewerRole::Reviewer | ReviewerRole::Approver)
}

fn is_stale(generated_at: DateTime<Utc>, observed_at: DateTime<Utc>) -> bool {
    generated_at.signed_duration_since(observed_at) > Duration::days(STALE_AFTER_DAYS)
}

fn reviewer_key(owner: &Owner) -> String {
    owner
        .email
        .as_ref()
        .map(|email| email.to_ascii_lowercase())
        .unwrap_or_else(|| owner.name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use open_kioku_core::{
        FileProvenance, GitChangeKind, GitCochangeEdge, GitCommitId, GitCommitRecord,
        HistoryRecordId, HistorySnapshot, HistorySummary, OwnerSuggestion,
        OwnershipConfidenceBreakdown, OwnershipEvidence, SymbolId, SymbolProvenance,
    };
    use open_kioku_storage::HistoryStore;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubHistoryStore {
        history: Mutex<Option<HistorySummary>>,
        provenance: Mutex<Option<FileProvenance>>,
    }

    impl StubHistoryStore {
        fn with_provenance(provenance: FileProvenance) -> Self {
            Self {
                history: Mutex::new(None),
                provenance: Mutex::new(Some(provenance)),
            }
        }

        fn with_history_and_provenance(
            history: HistorySummary,
            provenance: FileProvenance,
        ) -> Self {
            Self {
                history: Mutex::new(Some(history)),
                provenance: Mutex::new(Some(provenance)),
            }
        }
    }

    impl HistoryStore for StubHistoryStore {
        fn put_history_snapshot(&self, _snapshot: &HistorySnapshot) -> Result<()> {
            Ok(())
        }

        fn history_for_file(&self, path: &Path, _limit: usize) -> Result<HistorySummary> {
            Ok(self
                .history
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| HistorySummary::empty(path)))
        }

        fn provenance_for_path(&self, path: &Path, _limit: usize) -> Result<FileProvenance> {
            Ok(self
                .provenance
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| empty_provenance(path)))
        }

        fn provenance_for_symbol(
            &self,
            symbol_id: &SymbolId,
            _limit: usize,
        ) -> Result<SymbolProvenance> {
            Ok(SymbolProvenance {
                symbol_id: symbol_id.clone(),
                qualified_name: "unknown".into(),
                file_path: "src/unknown.rs".into(),
                range: None,
                first_seen: None,
                last_touched: None,
                recent_touches: Vec::new(),
                confidence: Confidence::Low,
                truncated: false,
                uncertainty: vec!["stub symbol provenance unavailable".into()],
            })
        }

        fn cochange_neighbors(&self, _path: &Path, _limit: usize) -> Result<Vec<GitCochangeEdge>> {
            Ok(Vec::new())
        }

        fn recent_commits(&self, _limit: usize) -> Result<Vec<GitCommitRecord>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn actual_review_evidence_outranks_inferred_author() {
        let history = StubHistoryStore::with_history_and_provenance(
            HistorySummary {
                path: "src/a.rs".into(),
                recent_commits: Vec::new(),
                file_touches: Vec::new(),
                symbol_touches: Vec::new(),
                cochange_neighbors: Vec::new(),
                reviewer_evidence: vec![reviewer_evidence(
                    "reviewer@example.com",
                    ReviewerRole::Approver,
                )],
                truncated: false,
                uncertainty: Vec::new(),
            },
            provenance(vec![
                touch("author@example.com", 0),
                touch("author@example.com", 1),
            ]),
        );

        let report = suggest_reviewers(ReviewerSuggestionInput {
            path: Path::new("src/a.rs"),
            history: &history,
            ownership: None,
        })
        .unwrap();

        assert_eq!(
            report.availability,
            ReviewerAvailability::ActualReviewEvidence
        );
        assert_eq!(
            report.suggestions[0].reviewer.email.as_deref(),
            Some("reviewer@example.com")
        );
        assert!(report.suggestions[0].actual_review_evidence);
        assert!(!report.suggestions[0].inferred_from_authors);
    }

    #[test]
    fn author_only_suggestions_are_explicitly_inferred() {
        let history = StubHistoryStore::with_provenance(provenance(vec![
            touch("alice@example.com", 0),
            touch("alice@example.com", 1),
            touch("bob@example.com", 2),
        ]));

        let report = suggest_reviewers(ReviewerSuggestionInput {
            path: Path::new("src/a.rs"),
            history: &history,
            ownership: None,
        })
        .unwrap();

        assert_eq!(
            report.availability,
            ReviewerAvailability::InferredFromAuthors
        );
        assert_eq!(
            report.suggestions[0].reviewer.email.as_deref(),
            Some("alice@example.com")
        );
        assert!(report.suggestions[0].inferred_from_authors);
        assert!(!report.suggestions[0].actual_review_evidence);
        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("actual PR-review evidence is unavailable")));
    }

    #[test]
    fn stored_author_evidence_is_inferred_not_actual_review() {
        let history = StubHistoryStore::with_history_and_provenance(
            HistorySummary {
                path: "src/a.rs".into(),
                recent_commits: Vec::new(),
                file_touches: Vec::new(),
                symbol_touches: Vec::new(),
                cochange_neighbors: Vec::new(),
                reviewer_evidence: vec![reviewer_evidence(
                    "author@example.com",
                    ReviewerRole::Author,
                )],
                truncated: false,
                uncertainty: Vec::new(),
            },
            empty_provenance(Path::new("src/a.rs")),
        );

        let report = suggest_reviewers(ReviewerSuggestionInput {
            path: Path::new("src/a.rs"),
            history: &history,
            ownership: None,
        })
        .unwrap();

        assert_eq!(
            report.availability,
            ReviewerAvailability::InferredFromAuthors
        );
        assert_eq!(
            report.suggestions[0].reviewer.email.as_deref(),
            Some("author@example.com")
        );
        assert!(report.suggestions[0].inferred_from_authors);
        assert!(!report.suggestions[0].actual_review_evidence);
    }

    #[test]
    fn ownership_signals_are_incorporated_without_review_certainty() {
        let history = StubHistoryStore::default();
        let ownership = ownership_report(owner_suggestion("team@example.com", 0.86));

        let report = suggest_reviewers(ReviewerSuggestionInput {
            path: Path::new("src/a.rs"),
            history: &history,
            ownership: Some(&ownership),
        })
        .unwrap();

        assert_eq!(
            report.availability,
            ReviewerAvailability::InferredFromOwnership
        );
        assert_eq!(
            report.suggestions[0].reviewer.email.as_deref(),
            Some("team@example.com")
        );
        assert!(!report.suggestions[0].actual_review_evidence);
        assert_eq!(
            report.suggestions[0].availability,
            ReviewerAvailability::InferredFromOwnership
        );
        assert!(report.suggestions[0]
            .source_types
            .contains(&ReviewerSignalSourceType::Ownership));
    }

    #[test]
    fn unavailable_when_no_review_owner_or_author_evidence_exists() {
        let history = StubHistoryStore::default();

        let report = suggest_reviewers(ReviewerSuggestionInput {
            path: Path::new("src/missing.rs"),
            history: &history,
            ownership: None,
        })
        .unwrap();

        assert_eq!(report.availability, ReviewerAvailability::Unavailable);
        assert!(report.suggestions.is_empty());
        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("no reviewer suggestions found")));
    }

    fn reviewer_evidence(email: &str, role: ReviewerRole) -> open_kioku_core::ReviewerEvidence {
        open_kioku_core::ReviewerEvidence {
            id: HistoryRecordId::new(format!("review:{email}")),
            commit_id: Some(GitCommitId::new("commit-review")),
            path: Some("src/a.rs".into()),
            reviewer: owner(email),
            role,
            observed_at: ts(0),
            source: "synthetic-pr-review".into(),
            confidence: Confidence::High,
        }
    }

    fn ownership_report(owner: OwnerSuggestion) -> OwnershipReport {
        OwnershipReport {
            path: "src/a.rs".into(),
            components: Vec::new(),
            generated_at: ts(0),
            owners: vec![owner],
            uncertainty: Vec::new(),
        }
    }

    fn owner_suggestion(email: &str, score: f32) -> OwnerSuggestion {
        let owner = owner(email);
        OwnerSuggestion {
            owner: owner.clone(),
            rationale: "CODEOWNERS matched the queried path".into(),
            confidence: Confidence::from_score(score),
            score,
            source_types: vec![OwnershipSourceType::Codeowners],
            stale: false,
            evidence: vec![OwnershipEvidence {
                source_type: OwnershipSourceType::Codeowners,
                owner,
                source: ".github/CODEOWNERS:1".into(),
                message: "CODEOWNERS rule matched".into(),
                confidence: Confidence::High,
                observed_at: Some(ts(0)),
                stale: false,
            }],
            confidence_breakdown: OwnershipConfidenceBreakdown {
                codeowners: score,
                git_history: 0.0,
                memory: 0.0,
                freshness: 0.0,
                ambiguity_penalty: 0.0,
                final_score: score,
            },
        }
    }

    fn provenance(touches: Vec<ProvenanceTouch>) -> FileProvenance {
        FileProvenance {
            path: "src/a.rs".into(),
            first_seen: touches.last().cloned(),
            last_touched: touches.first().cloned(),
            recent_touches: touches,
            confidence: Confidence::High,
            truncated: false,
            uncertainty: Vec::new(),
        }
    }

    fn empty_provenance(path: &Path) -> FileProvenance {
        FileProvenance {
            path: path.to_path_buf(),
            first_seen: None,
            last_touched: None,
            recent_touches: Vec::new(),
            confidence: Confidence::Low,
            truncated: false,
            uncertainty: vec!["no persisted provenance is available".into()],
        }
    }

    fn touch(email: &str, days_ago: i64) -> ProvenanceTouch {
        let at = ts(days_ago);
        ProvenanceTouch {
            commit: GitCommitRecord {
                id: GitCommitId::new(format!("commit-{email}-{days_ago}")),
                parent_ids: Vec::new(),
                author: owner(email),
                committer: None,
                authored_at: at,
                committed_at: at,
                summary: format!("touch by {email}"),
                message: format!("touch by {email}"),
                file_count: 1,
            },
            path: "src/a.rs".into(),
            previous_path: None,
            symbol_id: None,
            qualified_name: None,
            change_kind: GitChangeKind::Modified,
            line_ranges: Vec::new(),
            confidence: Confidence::High,
            uncertainty: Vec::new(),
        }
    }

    fn owner(email: &str) -> Owner {
        let name = email.split('@').next().unwrap_or(email).to_string();
        Owner {
            name,
            email: Some(email.into()),
        }
    }

    fn ts(days_ago: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() - Duration::days(days_ago)
    }
}
