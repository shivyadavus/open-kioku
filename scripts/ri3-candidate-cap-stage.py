from pathlib import Path

# 1) Core telemetry shape: additive and backwards-compatible for existing manifests.
core = Path("crates/open-kioku-core/src/lib.rs")
text = core.read_text()
old = '''#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolutionQualityReport {
    pub call_sites: usize,
    pub resolved_exact: usize,
    pub resolved_high: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub legacy_only: usize,
    pub semantic_only: usize,
    pub disagreement: usize,
    #[serde(default)]
    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,
}
'''
new = '''#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LanguageResolutionQuality {
    pub occurrences: usize,
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub candidate_cap_hits: usize,
    pub enrichment_time_us: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolutionQualityReport {
    pub call_sites: usize,
    pub resolved_exact: usize,
    pub resolved_high: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub legacy_only: usize,
    pub semantic_only: usize,
    pub disagreement: usize,
    #[serde(default)]
    pub candidate_cap_hits: usize,
    #[serde(default)]
    pub by_language: BTreeMap<String, LanguageResolutionQuality>,
    #[serde(default)]
    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,
}
'''
assert text.count(old) == 1, "ResolutionQualityReport layout changed unexpectedly"
core.write_text(text.replace(old, new))

# 2) Proof-gated evaluation: fail closed before any authoritative selection when the
# normalized unique candidate set exceeds the bounded semantic-resolution envelope.
pipeline = Path("crates/open-kioku-resolution/src/pipeline.rs")
text = pipeline.read_text()
anchor = 'use std::collections::BTreeMap;\n'
addition = '''use std::collections::BTreeMap;

/// Maximum unique structural targets considered for one semantic relationship occurrence.
/// Oversized sets fail closed as ambiguous; they are never truncated and then re-evaluated for
/// authority because doing so could manufacture a false unique winner.
pub const MAX_RESOLUTION_CANDIDATES: usize = 256;
'''
assert text.count(anchor) == 1, "pipeline import layout changed unexpectedly"
text = text.replace(anchor, addition)
old_enum = '''    Ambiguous {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
    },
    Unresolved {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
    },
'''
new_enum = '''    Ambiguous {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
        candidates_considered: usize,
        candidate_cap_hit: bool,
    },
    Unresolved {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
        candidates_considered: usize,
    },
'''
assert text.count(old_enum) == 1, "ResolutionOutcome layout changed unexpectedly"
text = text.replace(old_enum, new_enum)
old_eval = '''    let candidates = normalize_candidates(candidates);
    let candidates_considered = candidates.len();
    let heuristic_candidates_retained = candidates
'''
new_eval = '''    let candidates = normalize_candidates(candidates);
    let candidates_considered = candidates.len();
    if candidates_considered > MAX_RESOLUTION_CANDIDATES {
        return ResolutionOutcome::Ambiguous {
            candidates,
            reason: format!(
                "candidate cap hit: {candidates_considered} unique structural candidates exceed the safe maximum {MAX_RESOLUTION_CANDIDATES}; authoritative emission suppressed"
            ),
            candidates_considered,
            candidate_cap_hit: true,
        };
    }
    let heuristic_candidates_retained = candidates
'''
assert text.count(old_eval) == 1, "candidate evaluation prelude changed unexpectedly"
text = text.replace(old_eval, new_eval)
old_multi = '''        return ResolutionOutcome::Ambiguous {
            candidates,
            reason: format!(
                "{} candidates satisfy authoritative relationship proof policy",
                authoritative.len()
            ),
        };
'''
new_multi = '''        return ResolutionOutcome::Ambiguous {
            candidates,
            reason: format!(
                "{} candidates satisfy authoritative relationship proof policy",
                authoritative.len()
            ),
            candidates_considered,
            candidate_cap_hit: false,
        };
'''
assert text.count(old_multi) == 1, "authoritative ambiguity branch changed unexpectedly"
text = text.replace(old_multi, new_multi)
old_plausible = '''        return ResolutionOutcome::Ambiguous {
            reason: format!(
                "{} plausible candidates remain but none has unique authoritative proof",
                candidates.len()
            ),
            candidates,
        };
'''
new_plausible = '''        return ResolutionOutcome::Ambiguous {
            reason: format!(
                "{} plausible candidates remain but none has unique authoritative proof",
                candidates.len()
            ),
            candidates,
            candidates_considered,
            candidate_cap_hit: false,
        };
'''
assert text.count(old_plausible) == 1, "plausible ambiguity branch changed unexpectedly"
text = text.replace(old_plausible, new_plausible)
old_unresolved = '''    ResolutionOutcome::Unresolved {
        reason: if candidates.is_empty() {
            "no plausible structural candidate was discovered".into()
        } else {
            "candidate discovered but relationship proof policy did not authorize it".into()
        },
        candidates,
    }
'''
new_unresolved = '''    ResolutionOutcome::Unresolved {
        reason: if candidates.is_empty() {
            "no plausible structural candidate was discovered".into()
        } else {
            "candidate discovered but relationship proof policy did not authorize it".into()
        },
        candidates,
        candidates_considered,
    }
'''
assert text.count(old_unresolved) == 1, "unresolved branch changed unexpectedly"
text = text.replace(old_unresolved, new_unresolved)
old_legacy = '''            Self::Ambiguous { candidates, reason } => ResolutionResult::Ambiguous {
'''
new_legacy = '''            Self::Ambiguous {
                candidates, reason, ..
            } => ResolutionResult::Ambiguous {
'''
assert text.count(old_legacy) == 1, "legacy ambiguous adapter changed unexpectedly"
text = text.replace(old_legacy, new_legacy)
old_legacy_unresolved = '''            Self::Unresolved { candidates, reason } => {
'''
new_legacy_unresolved = '''            Self::Unresolved {
                candidates, reason, ..
            } => {
'''
assert text.count(old_legacy_unresolved) == 1, "legacy unresolved adapter changed unexpectedly"
text = text.replace(old_legacy_unresolved, new_legacy_unresolved)

# Public introspection keeps telemetry consumers decoupled from variant field details.
impl_anchor = '''impl ResolutionOutcome {
    /// Compatibility adapter for the existing public resolver result while callers migrate to the
'''
impl_new = '''impl ResolutionOutcome {
    pub fn candidates_considered(&self) -> usize {
        match self {
            Self::Proven { candidate } => candidate.candidates_considered,
            Self::Ambiguous {
                candidates_considered,
                ..
            }
            | Self::Unresolved {
                candidates_considered,
                ..
            } => *candidates_considered,
            Self::External { .. } => 0,
        }
    }

    pub fn candidate_cap_hit(&self) -> bool {
        matches!(
            self,
            Self::Ambiguous {
                candidate_cap_hit: true,
                ..
            }
        )
    }

    /// Compatibility adapter for the existing public resolver result while callers migrate to the
'''
assert text.count(impl_anchor) == 1, "ResolutionOutcome impl anchor changed unexpectedly"
text = text.replace(impl_anchor, impl_new)

# Any direct test constructors need the additive outcome fields.
text = text.replace(
    'ResolutionOutcome::Ambiguous {\n            candidates,\n            reason:',
    'ResolutionOutcome::Ambiguous {\n            candidates,\n            reason:'
)

# Add a deterministic fail-closed regression.
test_anchor = '''    #[test]
    fn normalization_is_independent_of_candidate_generation_order() {
'''
cap_test = '''    #[test]
    fn pathological_candidate_sets_fail_closed_before_authority_selection() {
        let candidates = (0..=MAX_RESOLUTION_CANDIDATES)
            .map(|index| {
                let target = SymbolId::new(format!("symbol:{index:04}"));
                let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
                candidate.proofs.push(proof(
                    RelationshipProofKind::ExactReference,
                    &target,
                    "pathological_exact",
                ));
                candidate
            })
            .collect::<Vec<_>>();

        let outcome = evaluate_candidates(&GraphEdgeType::References, candidates.clone());
        let reversed = evaluate_candidates(
            &GraphEdgeType::References,
            candidates.into_iter().rev().collect(),
        );
        for observed in [&outcome, &reversed] {
            let ResolutionOutcome::Ambiguous {
                candidates,
                candidates_considered,
                candidate_cap_hit,
                reason,
            } = observed
            else {
                panic!("oversized candidate set must fail closed as ambiguous");
            };
            assert_eq!(*candidates_considered, MAX_RESOLUTION_CANDIDATES + 1);
            assert!(*candidate_cap_hit);
            assert_eq!(candidates.len(), MAX_RESOLUTION_CANDIDATES + 1);
            assert!(reason.contains("authoritative emission suppressed"));
        }
        assert_eq!(outcome, reversed);
    }

    #[test]
'''
assert text.count(test_anchor) == 1, "pipeline test anchor changed unexpectedly"
text = text.replace(test_anchor, cap_test + '    fn normalization_is_independent_of_candidate_generation_order() {\n')
pipeline.write_text(text)

# 3) Re-export the cap so ingest and diagnostics use the same contract.
resolution_lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = resolution_lib.read_text()
old = '''pub use pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
};
'''
new = '''pub use pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
    MAX_RESOLUTION_CANDIDATES,
};
'''
assert text.count(old) == 1, "resolution pipeline re-export changed unexpectedly"
resolution_lib.write_text(text.replace(old, new))

# 4) Index-quality telemetry: per-language enrichment/candidate data plus explicit cap-hit notes.
ingest = Path("crates/open-kioku-ingest/src/lib.rs")
text = ingest.read_text()
old_import = '''    HistoryRecordId, HistorySnapshot, Import, IndexManifest, IndexMode, IndexPhaseReport,
    IndexQuality, LineRange, Repository, RepositoryId, SkipReason, SkipSource, SkippedPath, Symbol,
'''
new_import = '''    HistoryRecordId, HistorySnapshot, Import, IndexManifest, IndexMode, IndexPhaseReport,
    IndexQuality, Language, LineRange, Repository, RepositoryId, SkipReason, SkipSource, SkippedPath, Symbol,
'''
assert text.count(old_import) == 1, "ingest core imports changed unexpectedly"
text = text.replace(old_import, new_import)
old_sig = '''    fn record_outcome(
        &mut self,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
    );
'''
new_sig = '''    fn record_outcome(
        &mut self,
        language: &Language,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
        enrichment_time_us: u64,
    );
'''
assert text.count(old_sig) == 1, "ResolutionQualityReportExt signature changed unexpectedly"
text = text.replace(old_sig, new_sig)
old_impl_sig = '''    fn record_outcome(
        &mut self,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
    ) {
        let key = relationship_metric_key(edge_type);
        let metrics = self.by_relationship.entry(key).or_default();
'''
new_impl_sig = '''    fn record_outcome(
        &mut self,
        language: &Language,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
        enrichment_time_us: u64,
    ) {
        let language_metrics = self
            .by_language
            .entry(language_metric_key(language))
            .or_default();
        language_metrics.occurrences += 1;
        language_metrics.candidates_considered += outcome.candidates_considered();
        language_metrics.enrichment_time_us = language_metrics
            .enrichment_time_us
            .saturating_add(enrichment_time_us);
        if outcome.candidate_cap_hit() {
            self.candidate_cap_hits += 1;
            language_metrics.candidate_cap_hits += 1;
        }
        match outcome {
            open_kioku_resolution::ResolutionOutcome::Proven { .. } => {
                language_metrics.proven += 1;
            }
            open_kioku_resolution::ResolutionOutcome::Ambiguous { .. } => {
                language_metrics.ambiguous += 1;
            }
            open_kioku_resolution::ResolutionOutcome::Unresolved { .. } => {
                language_metrics.unresolved += 1;
            }
            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                language_metrics.external += 1;
            }
        }

        let key = relationship_metric_key(edge_type);
        let metrics = self.by_relationship.entry(key).or_default();
'''
assert text.count(old_impl_sig) == 1, "ResolutionQualityReportExt impl changed unexpectedly"
text = text.replace(old_impl_sig, new_impl_sig)
old_amb = '''            open_kioku_resolution::ResolutionOutcome::Ambiguous { candidates, .. } => {
                metrics.candidates_considered += candidates.len();
'''
new_amb = '''            open_kioku_resolution::ResolutionOutcome::Ambiguous {
                candidates,
                candidates_considered,
                ..
            } => {
                metrics.candidates_considered += *candidates_considered;
'''
assert text.count(old_amb) == 1, "ambiguous telemetry arm changed unexpectedly"
text = text.replace(old_amb, new_amb)
old_unr = '''            open_kioku_resolution::ResolutionOutcome::Unresolved { candidates, .. } => {
                metrics.candidates_considered += candidates.len();
'''
new_unr = '''            open_kioku_resolution::ResolutionOutcome::Unresolved {
                candidates,
                candidates_considered,
                ..
            } => {
                metrics.candidates_considered += *candidates_considered;
'''
assert text.count(old_unr) == 1, "unresolved telemetry arm changed unexpectedly"
text = text.replace(old_unr, new_unr)
metric_anchor = '''fn relationship_metric_key(edge_type: &GraphEdgeType) -> String {
'''
metric_new = '''fn language_metric_key(language: &Language) -> String {
    serde_json::to_value(language)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{language:?}"))
}

fn relationship_metric_key(edge_type: &GraphEdgeType) -> String {
'''
assert text.count(metric_anchor) == 1, "relationship metric key anchor changed unexpectedly"
text = text.replace(metric_anchor, metric_new)

# Time only the semantic enrichment call, not surrounding legacy-comparison or graph persistence.
old_call = '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        quality_report.record_outcome(&GraphEdgeType::Calls, &v2_outcome);
'''
new_call = '''                        let enrichment_started = Instant::now();
                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        quality_report.record_outcome(
                            &file.language,
                            &GraphEdgeType::Calls,
                            &v2_outcome,
                            elapsed_micros(enrichment_started),
                        );
'''
assert text.count(old_call) == 1, "call resolution integration changed unexpectedly"
text = text.replace(old_call, new_call)
old_inh = '''                let (edge_type, outcome) =
                    open_kioku_resolution::resolve_inheritance_relationship_outcome(site, &ctx);
                quality_report.record_outcome(&edge_type, &outcome);
'''
new_inh = '''                let enrichment_started = Instant::now();
                let (edge_type, outcome) =
                    open_kioku_resolution::resolve_inheritance_relationship_outcome(site, &ctx);
                quality_report.record_outcome(
                    &file.language,
                    &edge_type,
                    &outcome,
                    elapsed_micros(enrichment_started),
                );
'''
assert text.count(old_inh) == 1, "inheritance resolution integration changed unexpectedly"
text = text.replace(old_inh, new_inh)
old_type = '''                let Some((source, outcome)) =
                    open_kioku_resolution::resolve_declared_type_use_outcome(binding, &ctx)
                else {
                    continue;
                };
                quality_report.record_outcome(&GraphEdgeType::UsesType, &outcome);
'''
new_type = '''                let enrichment_started = Instant::now();
                let Some((source, outcome)) =
                    open_kioku_resolution::resolve_declared_type_use_outcome(binding, &ctx)
                else {
                    continue;
                };
                quality_report.record_outcome(
                    &file.language,
                    &GraphEdgeType::UsesType,
                    &outcome,
                    elapsed_micros(enrichment_started),
                );
'''
assert text.count(old_type) == 1, "type-use resolution integration changed unexpectedly"
text = text.replace(old_type, new_type)

# Centralize publication into IndexQuality so cap hits are impossible to hide from diagnostics.
old_attach = '''        let resolution_quality = if resolution_mode == open_kioku_config::ResolutionMode::Legacy {
            None
        } else {
            Some(quality_report)
        };
        quality.resolution_quality = resolution_quality.clone();
'''
new_attach = '''        let resolution_quality = if resolution_mode == open_kioku_config::ResolutionMode::Legacy {
            None
        } else {
            Some(quality_report)
        };
        attach_resolution_quality(&mut quality, resolution_quality.clone());
'''
assert text.count(old_attach) == 1, "resolution quality publication changed unexpectedly"
text = text.replace(old_attach, new_attach)
helper_anchor = '''fn relationship_metric_key(edge_type: &GraphEdgeType) -> String {
'''
# metric anchor now exists after language_metric_key; add helpers just before it.
helper = '''fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn attach_resolution_quality(
    quality: &mut IndexQuality,
    report: Option<ResolutionQualityReport>,
) {
    if let Some(report) = report.as_ref() {
        if report.candidate_cap_hits > 0 {
            quality.quality_notes.push(format!(
                "semantic relationship candidate cap ({}) hit for {} occurrence(s); authoritative emission was suppressed for every capped occurrence",
                open_kioku_resolution::MAX_RESOLUTION_CANDIDATES,
                report.candidate_cap_hits
            ));
            quality.quality_notes.sort();
            quality.quality_notes.dedup();
        }
    }
    quality.resolution_quality = report;
}

fn relationship_metric_key(edge_type: &GraphEdgeType) -> String {
'''
assert text.count(helper_anchor) == 1, "helper insertion anchor changed unexpectedly"
text = text.replace(helper_anchor, helper)

# Make the existing test module import the publication helper and add a direct guardrail regression.
old_test_import = '''    use super::{derive_occurrences, map_symbol_touches, Indexer};
'''
new_test_import = '''    use super::{attach_resolution_quality, derive_occurrences, map_symbol_touches, Indexer};
'''
assert text.count(old_test_import) == 1, "ingest test import changed unexpectedly"
text = text.replace(old_test_import, new_test_import)
test_anchor = '''    #[test]
    fn derive_occurrences_records_definitions_only_for_heuristic_indexing() {
'''
cap_note_test = '''    #[test]
    fn candidate_cap_hits_are_visible_in_index_quality() {
        let mut quality = open_kioku_core::IndexQuality::default();
        let mut report = open_kioku_core::ResolutionQualityReport::default();
        report.candidate_cap_hits = 2;
        attach_resolution_quality(&mut quality, Some(report));

        assert_eq!(
            quality
                .resolution_quality
                .as_ref()
                .map(|report| report.candidate_cap_hits),
            Some(2)
        );
        assert!(quality.quality_notes.iter().any(|note| {
            note.contains("candidate cap")
                && note.contains("2 occurrence(s)")
                && note.contains("authoritative emission was suppressed")
        }));
    }

    #[test]
'''
assert text.count(test_anchor) == 1, "ingest test anchor changed unexpectedly"
text = text.replace(test_anchor, cap_note_test + '    fn derive_occurrences_records_definitions_only_for_heuristic_indexing() {\n')
ingest.write_text(text)
