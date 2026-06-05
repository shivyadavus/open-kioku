use open_kioku_context::ContextPackBuilder;
use open_kioku_core::{
    ChangeBoundary, ConfidenceBreakdown, ConfidenceSignalInput, ContextPack, FileId, ImpactReport,
    MemorySearchResult, PlanReport, RiskReport, ScoreComponent, SearchResult, TestTarget,
    ToolCallRecommendation,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_storage::{MetadataStore, OkStore, SearchIndex};
use open_kioku_tests::TestSelector;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const DEFAULT_CONTEXT_LIMIT: usize = 12;
const MAX_PRIMARY_CONTEXT: usize = 8;
const MAX_SYMBOLS: usize = 8;
const MAX_VALIDATION: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PlanFormat {
    Text,
    Markdown,
    Json,
    Toon,
}

impl PlanFormat {
    pub fn render(&self, report: &PlanReport) -> Result<String> {
        match self {
            Self::Json => Ok(serde_json::to_string_pretty(report)?),
            Self::Toon => Ok(open_kioku_format::render_plan_toon(report)),
            Self::Markdown => Ok(render_markdown(report)),
            Self::Text => Ok(render_text(report)),
        }
    }
}

pub struct PlanEngine<'a> {
    store: &'a dyn OkStore,
    search_index: Option<&'a dyn SearchIndex>,
    memory_facts: Vec<MemorySearchResult>,
}

impl<'a> PlanEngine<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            search_index: None,
            memory_facts: Vec::new(),
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn with_memory_facts(mut self, memory_facts: Vec<MemorySearchResult>) -> Self {
        self.memory_facts = memory_facts;
        self
    }

    pub fn plan(&self, task: &str, limit: usize) -> Result<PlanReport> {
        let context_limit = limit.clamp(1, 100);
        let context = ContextPackBuilder::new(self.store).build(task, context_limit)?;
        self.plan_from_context(task, context_limit, context)
    }

    pub fn plan_from_context(
        &self,
        task: &str,
        limit: usize,
        context: ContextPack,
    ) -> Result<PlanReport> {
        let context_limit = limit.clamp(1, 100);
        let mut primary_context = context
            .primary_files
            .iter()
            .take(MAX_PRIMARY_CONTEXT.min(context_limit))
            .cloned()
            .collect::<Vec<_>>();
        for result in &mut primary_context {
            result.reconcile_score_breakdown();
        }
        let impact_target = impact_target(&primary_context);
        let mut impact = self.impact_for_primary_context(task, impact_target, &context)?;
        impact.reconcile_score_breakdown();
        let mut validation = self.validation_for_context(&primary_context, &context)?;
        for test in &mut validation {
            test.reconcile_score_breakdown();
        }
        let unmatched_anchors = unmatched_named_anchors(task, &primary_context);
        let risk = merge_risk(
            &context.risk_report,
            &impact.risk_report,
            primary_context.is_empty(),
            &unmatched_anchors,
        );
        let relevant_symbols = context
            .primary_symbols
            .iter()
            .take(MAX_SYMBOLS)
            .cloned()
            .collect::<Vec<_>>();
        let recommended_change_boundary = change_boundary(
            &primary_context,
            &impact,
            &context.recommended_change_boundary,
        );
        let recommended_next_steps =
            next_steps(&primary_context, &impact, &validation, &self.memory_facts);
        let tool_calls = tool_calls(task, impact_target, !self.memory_facts.is_empty());
        let evidence = context
            .evidence
            .iter()
            .chain(impact.evidence.iter())
            .cloned()
            .collect::<Vec<_>>();
        let summary = summary(
            task,
            &primary_context,
            &impact,
            &validation,
            &risk,
            &self.memory_facts,
        );
        let score_breakdown = plan_score_breakdown(&risk);
        let confidence_breakdown = confidence_for_plan(
            &primary_context,
            &impact,
            &validation,
            &risk,
            &recommended_change_boundary,
            &evidence,
            context.runtime_signals.len(),
        );
        let confidence_summary = confidence_summary(&confidence_breakdown);

        let mut report = PlanReport {
            task: task.into(),
            summary,
            primary_context,
            relevant_symbols,
            impact,
            validation,
            risk,
            recommended_change_boundary,
            recommended_next_steps,
            tool_calls,
            memory_facts: self.memory_facts.clone(),
            evidence,
            confidence_summary,
            confidence_breakdown,
            score_breakdown,
        };
        report.reconcile_score_breakdown();
        Ok(report)
    }

    fn impact_for_primary_context(
        &self,
        task: &str,
        impact_target: Option<&SearchResult>,
        context: &ContextPack,
    ) -> Result<ImpactReport> {
        if context_has_bounded_impact(context) {
            let evidence = context
                .evidence
                .iter()
                .filter(|evidence| evidence.id.0 == "context:bounded-search")
                .cloned()
                .collect::<Vec<_>>();
            return Ok(ImpactReport {
                target: impact_target
                    .map(|target| target.path.display().to_string())
                    .unwrap_or_else(|| task.into()),
                direct_impacts: context.supporting_files.clone(),
                indirect_impacts: Vec::new(),
                risk_report: context.risk_report.clone(),
                evidence,
                score_breakdown: vec![ScoreComponent::single(
                    "bounded_context_impact",
                    context.risk_report.score,
                    vec!["context:bounded-search".into()],
                    "bounded context reused persisted supporting files",
                )],
            });
        }
        if let Some(target) = impact_target {
            ImpactEngine::new(self.store as &dyn MetadataStore)
                .with_search_index(self.search_index)
                .for_file(&target.path)
        } else {
            Ok(ImpactReport {
                target: task.into(),
                direct_impacts: Vec::new(),
                indirect_impacts: Vec::new(),
                risk_report: context.risk_report.clone(),
                evidence: context.evidence.clone(),
                score_breakdown: vec![ScoreComponent::single(
                    "context_risk_fallback",
                    context.risk_report.score,
                    context
                        .evidence
                        .iter()
                        .map(|evidence| evidence.id.0.clone())
                        .collect(),
                    "no primary impact target; using context risk",
                )],
            })
        }
    }

    fn validation_for_context(
        &self,
        primary_context: &[SearchResult],
        context: &ContextPack,
    ) -> Result<Vec<TestTarget>> {
        let mut by_id = BTreeMap::new();
        for test in &context.validation_plan.tests {
            if is_plausible_test(test) {
                by_id.insert(test.id.clone(), test.clone());
            }
        }

        let selector = TestSelector::new(self.store as &dyn MetadataStore);
        for result in source_results(primary_context).take(3) {
            for test in selector.for_changed_path_with_evidence(&result.path, MAX_VALIDATION)? {
                if is_plausible_test(&test) {
                    by_id.entry(test.id.clone()).or_insert(test);
                }
            }
        }

        let tests = by_id.into_values().collect::<Vec<_>>();

        // Group by file_id to prefer class-like test targets
        let mut filtered = Vec::new();
        let mut by_file: BTreeMap<FileId, Vec<TestTarget>> = BTreeMap::new();
        for test in tests {
            by_file.entry(test.file_id.clone()).or_default().push(test);
        }
        for (_, mut file_tests) in by_file {
            let has_class_like = file_tests.iter().any(|t| {
                t.name.len() > 8
                    && t.name
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    && t.name.chars().any(|c| c.is_lowercase())
            });
            if has_class_like {
                file_tests.retain(|t| {
                    t.name.len() > 8
                        && t.name
                            .chars()
                            .next()
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                        && t.name.chars().any(|c| c.is_lowercase())
                });
            }
            filtered.extend(file_tests);
        }

        filtered.sort_by(|a, b| {
            b.confidence
                .score()
                .partial_cmp(&a.confidence.score())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        filtered.truncate(MAX_VALIDATION);
        Ok(filtered)
    }
}

fn is_plausible_test(test: &TestTarget) -> bool {
    let name = &test.name;
    // Filter out screaming snake case constants like AD_DOMAIN
    let is_screaming_snake = name
        .chars()
        .all(|c| c.is_uppercase() || c == '_' || c.is_numeric())
        && name.chars().any(|c| c.is_alphabetic());
    if is_screaming_snake {
        return false;
    }
    if is_generic_validation_test(name) {
        return false;
    }

    // Always keep explicit/confident test names
    let is_test_named = name.ends_with("Tests")
        || name.ends_with("Test")
        || name.ends_with("IT")
        || name.ends_with("Spec")
        || name.starts_with("test")
        || name.starts_with("test_")
        || name.contains("Test")
        || name.contains("test");

    // Keep class-like names (PascalCase with >8 chars)
    let is_class_like = name.len() > 8
        && name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        && name.chars().any(|c| c.is_lowercase());

    // Keep snake_case names (like login_returns_valid_token, typical in Rust/Python/Go tests)
    let is_snake_case_func = name
        .chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
        && name.contains('_');

    is_test_named || is_class_like || is_snake_case_func
}

fn is_generic_validation_test(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "validationtest" | "validationtests" | "validatortest" | "validatortests"
    )
}

fn context_has_bounded_impact(context: &ContextPack) -> bool {
    context
        .evidence
        .iter()
        .any(|evidence| evidence.id.0 == "context:bounded-search")
}

fn merge_risk(
    context: &RiskReport,
    impact: &RiskReport,
    no_matches: bool,
    unmatched_anchors: &[String],
) -> RiskReport {
    if no_matches {
        return RiskReport {
            level: "unknown".into(),
            score: 0.0,
            reasons: vec!["no matching indexed context found for the task".into()],
        };
    }

    let mut reasons = impact.reasons.clone();
    for reason in &context.reasons {
        if !reasons.contains(reason) {
            reasons.push(reason.clone());
        }
    }
    if !unmatched_anchors.is_empty() {
        reasons.push(format!(
            "low confidence: top context did not match named task anchor(s): {}",
            unmatched_anchors.join(", ")
        ));
    }

    let score = if unmatched_anchors.is_empty() {
        impact.score.max(context.score)
    } else {
        impact.score.max(context.score).max(0.45)
    };
    let level = if score > 0.6 {
        "high"
    } else if score > 0.25 {
        "medium"
    } else {
        "low"
    };

    RiskReport {
        level: level.into(),
        score,
        reasons,
    }
}

fn plan_score_breakdown(risk: &RiskReport) -> Vec<ScoreComponent> {
    let mut components = Vec::new();
    let reason_ids = risk
        .reasons
        .iter()
        .enumerate()
        .map(|(index, _)| format!("risk:reason:{index}"))
        .collect::<Vec<_>>();
    components.push(ScoreComponent::single(
        "plan_risk_score",
        risk.score,
        reason_ids,
        format!(
            "plan risk is `{}` from merged context and impact risk",
            risk.level
        ),
    ));
    components
}

fn confidence_for_plan(
    primary_context: &[SearchResult],
    impact: &ImpactReport,
    validation: &[TestTarget],
    risk: &RiskReport,
    boundary: &ChangeBoundary,
    evidence: &[open_kioku_core::Evidence],
    context_runtime_signal_count: usize,
) -> ConfidenceBreakdown {
    ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
        primary_file_count: primary_context.len(),
        evidence_count: evidence.len(),
        exact_reference_count: exact_reference_count(primary_context, impact),
        validation_count: validation.len(),
        validation_with_command_count: validation
            .iter()
            .filter(|test| test.command.is_some())
            .count(),
        negative_evidence_count: negative_evidence_count(risk),
        allowed_file_count: boundary.allowed_files.len(),
        runtime_signal_count: context_runtime_signal_count
            + runtime_signal_count(primary_context)
            + runtime_signal_count(&impact.direct_impacts)
            + runtime_signal_count(&impact.indirect_impacts)
            + evidence
                .iter()
                .filter(|item| item.source_type == open_kioku_core::EvidenceSourceType::Runtime)
                .count(),
    })
}

fn confidence_summary(breakdown: &ConfidenceBreakdown) -> String {
    let mut parts = vec![format!(
        "overall {:?} ({:.2}) from evidence density, references, validation, boundaries, runtime, and negative evidence",
        breakdown.overall_enum, breakdown.overall_score
    )];
    if let Some(blocker) = breakdown.blockers.first() {
        parts.push(format!("blocker: {blocker}"));
    }
    if let Some(caveat) = breakdown.caveats.first() {
        parts.push(format!("caveat: {caveat}"));
    }
    parts.join("; ")
}

fn exact_reference_count(primary_context: &[SearchResult], impact: &ImpactReport) -> usize {
    primary_context
        .iter()
        .chain(impact.direct_impacts.iter())
        .chain(impact.indirect_impacts.iter())
        .filter(|result| has_exact_reference_signal(result))
        .count()
}

fn has_exact_reference_signal(result: &SearchResult) -> bool {
    result
        .evidence
        .iter()
        .any(|evidence| contains_exact_reference(evidence))
        || contains_exact_reference(&result.match_reason)
}

fn contains_exact_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("exact reference")
        || lower.contains("exact symbol reference")
        || lower.contains("scip")
}

fn runtime_signal_count(results: &[SearchResult]) -> usize {
    results
        .iter()
        .filter(|result| {
            result.score_breakdown.iter().any(|component| {
                component.signal == "runtime_corroboration" && component.contribution > 0.0
            }) || result
                .evidence
                .iter()
                .any(|evidence| evidence.to_ascii_lowercase().contains("runtime"))
        })
        .count()
}

fn negative_evidence_count(risk: &RiskReport) -> usize {
    risk.reasons
        .iter()
        .filter(|reason| {
            let lower = reason.to_ascii_lowercase();
            lower.contains("low confidence")
                || lower.contains("no matching")
                || lower.contains("missing")
                || lower.contains("absent")
                || lower.contains("unavailable")
                || lower.contains("weak")
                || lower.contains("unknown")
        })
        .count()
}

fn unmatched_named_anchors(task: &str, primary_context: &[SearchResult]) -> Vec<String> {
    let anchors = named_anchors(task);
    if anchors.is_empty() || primary_context.is_empty() {
        return Vec::new();
    }
    let top_context = primary_context
        .iter()
        .take(5)
        .map(|result| {
            format!(
                "{} {} {} {}",
                result.path.display(),
                result.snippet,
                result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.name.as_str())
                    .unwrap_or_default(),
                result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.qualified_name.as_str())
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    anchors
        .into_iter()
        .filter(|anchor| {
            let lower = anchor.to_ascii_lowercase();
            !top_context.contains(&lower) && !top_context.contains(&normalize_anchor(anchor))
        })
        .collect()
}

fn named_anchors(task: &str) -> Vec<String> {
    let mut anchors = Vec::new();
    for token in task.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        let token = token.trim_matches('-');
        if token.len() < 3 || is_ticket_anchor(token) {
            continue;
        }
        let has_lower = token.chars().any(|ch| ch.is_ascii_lowercase());
        let has_upper = token.chars().any(|ch| ch.is_ascii_uppercase());
        let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        let has_separator = token.contains('_') || token.contains('-');
        if ((has_lower && has_upper) || has_separator || (has_digit && has_upper))
            && !anchors.iter().any(|existing| existing == token)
        {
            anchors.push(token.to_string());
        }
    }
    anchors
}

fn is_ticket_anchor(value: &str) -> bool {
    let Some((prefix, number)) = value.split_once('-') else {
        return false;
    };
    prefix.len() >= 2
        && prefix.chars().all(|ch| ch.is_ascii_uppercase())
        && number.len() >= 2
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn normalize_anchor(value: &str) -> String {
    let mut out = String::new();
    let mut previous_lower_or_digit = false;
    for ch in value.chars() {
        if ch == '_' || ch == '-' {
            out.push(' ');
            previous_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower_or_digit {
            out.push(' ');
        }
        out.push(ch.to_ascii_lowercase());
        previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn change_boundary(
    primary_context: &[SearchResult],
    impact: &ImpactReport,
    context_boundary: &ChangeBoundary,
) -> ChangeBoundary {
    let mut allowed = BTreeSet::new();
    for result in primary_context {
        allowed.insert(result.path.clone());
    }
    for path in &context_boundary.allowed_files {
        allowed.insert(path.clone());
    }

    let mut caution = BTreeSet::new();
    for result in impact
        .direct_impacts
        .iter()
        .chain(impact.indirect_impacts.iter())
    {
        if !allowed.contains(&result.path) {
            caution.insert(result.path.clone());
        }
    }
    for path in &context_boundary.caution_files {
        if !allowed.contains(path) {
            caution.insert(path.clone());
        }
    }

    ChangeBoundary {
        allowed_files: allowed.into_iter().collect(),
        caution_files: caution.into_iter().collect(),
        forbidden_files: context_boundary.forbidden_files.clone(),
    }
}

fn next_steps(
    primary_context: &[SearchResult],
    impact: &ImpactReport,
    validation: &[TestTarget],
    memory_facts: &[MemorySearchResult],
) -> Vec<String> {
    let mut steps = Vec::new();
    if primary_context.is_empty() {
        steps.push("Refine the task query or run broader search before editing.".into());
        return steps;
    }

    steps.push("Inspect the primary context files and symbol ranges before editing.".into());
    if !impact.direct_impacts.is_empty() {
        steps.push("Review direct impact candidates before deciding the edit boundary.".into());
    }
    if !memory_facts.is_empty() {
        steps.push("Check matched repo memory facts, but verify them against indexed code before relying on them.".into());
    }
    if validation.is_empty() {
        steps.push("No indexed tests were found; choose a manual validation command.".into());
    } else {
        steps.push("Run the recommended validation commands after the change.".into());
    }
    steps.push(
        "Keep edits within allowed files unless new evidence justifies expanding scope.".into(),
    );
    steps
}

fn impact_target(primary_context: &[SearchResult]) -> Option<&SearchResult> {
    source_results(primary_context)
        .find(|result| !is_doc_path(&result.path))
        .or_else(|| source_results(primary_context).next())
        .or_else(|| primary_context.first())
}

fn source_results(primary_context: &[SearchResult]) -> impl Iterator<Item = &SearchResult> {
    primary_context
        .iter()
        .filter(|result| !is_test_path(&result.path))
}

fn is_test_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("test")
    })
}

fn is_doc_path(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.starts_with("docs/")
        || lower.contains("/docs/")
}

fn tool_calls(
    task: &str,
    impact_target: Option<&SearchResult>,
    has_memory_facts: bool,
) -> Vec<ToolCallRecommendation> {
    let mut calls = vec![
        ToolCallRecommendation {
            tool: "search_code".into(),
            purpose: "Find indexed evidence for the task.".into(),
            arguments: json!({"query": task, "limit": DEFAULT_CONTEXT_LIMIT}),
        },
        ToolCallRecommendation {
            tool: "build_context_pack".into(),
            purpose: "Assemble primary files, symbols, tests, and boundaries.".into(),
            arguments: json!({"task": task, "limit": DEFAULT_CONTEXT_LIMIT, "format": "markdown"}),
        },
    ];

    if let Some(target) = impact_target {
        calls.insert(
            1,
            ToolCallRecommendation {
                tool: "impact_analysis".into(),
                purpose: "Estimate likely downstream files for the primary source file.".into(),
                arguments: json!({"path": target.path}),
            },
        );
        calls.push(ToolCallRecommendation {
            tool: "find_tests_for_change".into(),
            purpose: "Find indexed validation candidates for the primary source file.".into(),
            arguments: json!({"path": target.path, "limit": MAX_VALIDATION}),
        });
    }

    calls.push(ToolCallRecommendation {
        tool: "search_memory".into(),
        purpose: if has_memory_facts {
            "Review matched repo memory facts and their provenance.".into()
        } else {
            "Check whether prior repo facts exist for this task.".into()
        },
        arguments: json!({"query": task, "limit": 8}),
    });

    calls
}

fn summary(
    task: &str,
    primary_context: &[SearchResult],
    impact: &ImpactReport,
    validation: &[TestTarget],
    risk: &RiskReport,
    memory_facts: &[MemorySearchResult],
) -> String {
    if primary_context.is_empty() {
        return format!(
            "No indexed context matched `{task}`. Refine the task or re-index the repo."
        );
    }
    format!(
        "Found {} primary context item(s), {} direct impact candidate(s), {} validation candidate(s), {} repo memory fact(s); risk is {}.",
        primary_context.len(),
        impact.direct_impacts.len(),
        validation.len(),
        memory_facts.len(),
        risk.level
    )
}

fn render_text(report: &PlanReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Plan: {}\n", report.task));
    out.push_str(&format!("Summary: {}\n", report.summary));
    out.push_str(&format!(
        "Risk: {} ({:.2})\n",
        report.risk.level, report.risk.score
    ));
    out.push_str(&format!(
        "Confidence: {:?} ({:.2})\n",
        report.confidence_breakdown.overall_enum, report.confidence_breakdown.overall_score
    ));
    for reason in &report.risk.reasons {
        out.push_str(&format!("  - {reason}\n"));
    }
    write_confidence_text(&mut out, &report.confidence_breakdown);

    out.push_str("\nPrimary context:\n");
    write_results(&mut out, &report.primary_context);

    out.push_str("\nRelevant symbols:\n");
    if report.relevant_symbols.is_empty() {
        out.push_str("  - none found\n");
    } else {
        for symbol in &report.relevant_symbols {
            out.push_str(&format!(
                "  - {} ({:?})\n",
                symbol.qualified_name, symbol.kind
            ));
        }
    }

    out.push_str("\nImpact candidates:\n");
    write_results(&mut out, &report.impact.direct_impacts);

    out.push_str("\nValidation candidates:\n");
    if report.validation.is_empty() {
        out.push_str("  - none found\n");
    } else {
        for test in &report.validation {
            let command = test.command.as_deref().unwrap_or("manual validation");
            out.push_str(&format!(
                "  - {} [{}] ({})\n",
                test.name,
                command,
                top_score_signals(&test.score_breakdown)
            ));
        }
    }

    out.push_str("\nRepo memory:\n");
    if report.memory_facts.is_empty() {
        out.push_str("  - none matched\n");
    } else {
        for result in &report.memory_facts {
            out.push_str(&format!(
                "  - {} ({:.2}, {})\n",
                one_line(&result.fact.text),
                result.score,
                result.fact.source
            ));
        }
    }

    out.push_str("\nRecommended next steps:\n");
    for step in &report.recommended_next_steps {
        out.push_str(&format!("  - {step}\n"));
    }

    out
}

fn render_markdown(report: &PlanReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Plan: {}\n\n", report.task));
    out.push_str(&format!("{}\n\n", report.summary));
    out.push_str(&format!(
        "## Risk\n\n- Level: `{}`\n- Score: `{:.2}`\n",
        report.risk.level, report.risk.score
    ));
    for reason in &report.risk.reasons {
        out.push_str(&format!("- {reason}\n"));
    }
    out.push_str("\n### Score Signals\n\n");
    write_markdown_score_components(&mut out, &report.score_breakdown);

    out.push_str("\n## Confidence\n\n");
    out.push_str(&format!(
        "- Overall: `{:?}` (`{:.2}`)\n",
        report.confidence_breakdown.overall_enum, report.confidence_breakdown.overall_score
    ));
    write_markdown_confidence_breakdown(&mut out, &report.confidence_breakdown);

    out.push_str("\n## Primary Context\n\n");
    write_markdown_results(&mut out, &report.primary_context);

    out.push_str("\n## Relevant Symbols\n\n");
    if report.relevant_symbols.is_empty() {
        out.push_str("- None found\n");
    } else {
        for symbol in &report.relevant_symbols {
            out.push_str(&format!(
                "- `{}` ({:?})\n",
                symbol.qualified_name, symbol.kind
            ));
        }
    }

    out.push_str("\n## Impact Candidates\n\n");
    write_markdown_results(&mut out, &report.impact.direct_impacts);

    out.push_str("\n## Validation Candidates\n\n");
    if report.validation.is_empty() {
        out.push_str("- None found\n");
    } else {
        for test in &report.validation {
            let command = test.command.as_deref().unwrap_or("manual validation");
            out.push_str(&format!(
                "- `{}` via `{}`; signals: {}\n",
                test.name,
                command,
                top_score_signals(&test.score_breakdown)
            ));
        }
    }

    out.push_str("\n## Repo Memory\n\n");
    if report.memory_facts.is_empty() {
        out.push_str("- None matched\n");
    } else {
        for result in &report.memory_facts {
            out.push_str(&format!(
                "- `{}` ({:.2}, source `{}`)\n",
                one_line(&result.fact.text),
                result.score,
                result.fact.source
            ));
        }
    }

    out.push_str("\n## Edit Boundary\n\n");
    out.push_str("Allowed files:\n");
    write_paths(&mut out, &report.recommended_change_boundary.allowed_files);
    out.push_str("\nCaution files:\n");
    write_paths(&mut out, &report.recommended_change_boundary.caution_files);

    out.push_str("\n## Recommended Next Steps\n\n");
    for step in &report.recommended_next_steps {
        out.push_str(&format!("- {step}\n"));
    }

    out.push_str("\n## Agent Tool Calls\n\n");
    for call in &report.tool_calls {
        out.push_str(&format!(
            "- `{}`: {} `{}`\n",
            call.tool, call.purpose, call.arguments
        ));
    }

    out
}

fn write_results(out: &mut String, results: &[SearchResult]) {
    if results.is_empty() {
        out.push_str("  - none found\n");
        return;
    }
    for result in results {
        let range = line_range(result);
        out.push_str(&format!(
            "  - {}{} [{:.3}; {}]: {}\n",
            result.path.display(),
            range,
            result.score,
            top_score_signals(&result.score_breakdown),
            one_line(&result.snippet)
        ));
    }
}

fn write_markdown_results(out: &mut String, results: &[SearchResult]) {
    if results.is_empty() {
        out.push_str("- None found\n");
        return;
    }
    for result in results {
        out.push_str(&format!(
            "- `{}`{}: {}\n  - score: `{:.3}`; signals: {}\n",
            result.path.display(),
            line_range(result),
            one_line(&result.snippet),
            result.score,
            top_score_signals(&result.score_breakdown)
        ));
    }
}

fn write_markdown_score_components(out: &mut String, components: &[ScoreComponent]) {
    if components.is_empty() {
        out.push_str("- None\n");
        return;
    }
    for component in components.iter().take(3) {
        out.push_str(&format!(
            "- `{}` contribution `{:.3}`: {}\n",
            component.signal,
            component.contribution,
            one_line(&component.rationale)
        ));
    }
}

fn write_confidence_text(out: &mut String, breakdown: &ConfidenceBreakdown) {
    if !breakdown.blockers.is_empty() {
        out.push_str("Confidence blockers:\n");
        for blocker in &breakdown.blockers {
            out.push_str(&format!("  - {blocker}\n"));
        }
    }
    if !breakdown.caveats.is_empty() {
        out.push_str("Confidence caveats:\n");
        for caveat in &breakdown.caveats {
            out.push_str(&format!("  - {caveat}\n"));
        }
    }
    out.push_str("Confidence components:\n");
    for component in &breakdown.components {
        out.push_str(&format!(
            "  - {}: {:.2} × {:.2} = {:.2}\n",
            component.signal, component.normalized_value, component.weight, component.contribution
        ));
    }
}

fn write_markdown_confidence_breakdown(out: &mut String, breakdown: &ConfidenceBreakdown) {
    if !breakdown.blockers.is_empty() {
        out.push_str("- Blockers:\n");
        for blocker in &breakdown.blockers {
            out.push_str(&format!("  - {blocker}\n"));
        }
    }
    if !breakdown.caveats.is_empty() {
        out.push_str("- Caveats:\n");
        for caveat in &breakdown.caveats {
            out.push_str(&format!("  - {caveat}\n"));
        }
    }
    out.push_str("- Components:\n");
    for component in &breakdown.components {
        out.push_str(&format!(
            "  - `{}` score `{:.2}`, weight `{:.2}`, contribution `{:.2}`: {}\n",
            component.signal,
            component.normalized_value,
            component.weight,
            component.contribution,
            one_line(&component.rationale)
        ));
    }
}

fn top_score_signals(components: &[ScoreComponent]) -> String {
    if components.is_empty() {
        return "no score signals".into();
    }
    components
        .iter()
        .filter(|component| component.contribution.abs() > 0.001)
        .take(3)
        .map(|component| format!("`{}` {:+.3}", component.signal, component.contribution))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_paths(out: &mut String, paths: &[PathBuf]) {
    if paths.is_empty() {
        out.push_str("- None\n");
        return;
    }
    for path in paths {
        out.push_str(&format!("- `{}`\n", path.display()));
    }
}

fn line_range(result: &SearchResult) -> String {
    result
        .line_range
        .as_ref()
        .map(|range| format!(":{}-{}", range.start, range.end))
        .unwrap_or_default()
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use open_kioku_core::{
        CodeChunk, Confidence, Evidence, EvidenceId, EvidenceSourceType, File, FileId,
        IndexManifest, Language, LineRange, Repository, RepositoryId, Symbol, SymbolId, SymbolKind,
        TestTarget, ValidationPlan,
    };
    use open_kioku_storage::IndexData;
    use open_kioku_storage_sqlite::SqliteStore;

    fn test_store() -> SqliteStore {
        let store = SqliteStore::open(":memory:").unwrap();
        let repo_id = RepositoryId::new("repo");
        let file_auth = File {
            id: FileId::new("auth"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/auth.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "auth-hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let file_lib = File {
            id: FileId::new("lib"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "lib-hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let file_test = File {
            id: FileId::new("test"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("tests/auth_flow.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "test-hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let issue_token = Symbol {
            id: SymbolId::new("issue-token"),
            name: "issue_token".into(),
            qualified_name: "src::auth::issue_token".into(),
            kind: SymbolKind::Function,
            file_id: file_auth.id.clone(),
            range: Some(LineRange { start: 3, end: 5 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: open_kioku_core::EvidenceSourceType::TreeSitter,
        };
        let login_test = TestTarget {
            id: "login-test".into(),
            name: "login_returns_valid_token".into(),
            file_id: file_test.id.clone(),
            range: Some(LineRange { start: 4, end: 7 }),
            command: Some("cargo test".into()),
            confidence: Confidence::High,
            reason: "test-like path".into(),
            score_breakdown: vec![ScoreComponent::single(
                "test_fixture_confidence",
                Confidence::High.score(),
                vec!["login-test".into()],
                "test-like path",
            )],
        };
        let chunks = vec![
            CodeChunk {
                id: "auth-token".into(),
                file_id: file_auth.id.clone(),
                range: LineRange { start: 3, end: 5 },
                language: Language::Rust,
                text: "pub fn issue_token(context: &RequestContext, ttl_seconds: u64) -> String"
                    .into(),
                symbol_id: Some(issue_token.id.clone()),
            },
            CodeChunk {
                id: "lib-token".into(),
                file_id: file_lib.id.clone(),
                range: LineRange { start: 7, end: 10 },
                language: Language::Rust,
                text: "auth::issue_token(&context, 3600)".into(),
                symbol_id: None,
            },
            CodeChunk {
                id: "test-token".into(),
                file_id: file_test.id.clone(),
                range: LineRange { start: 4, end: 7 },
                language: Language::Rust,
                text: "fn login_returns_valid_token()".into(),
                symbol_id: None,
            },
        ];
        let manifest = IndexManifest {
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 1,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            quality: open_kioku_core::IndexQuality::default(),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[file_auth, file_lib, file_test],
                symbols: &[issue_token],
                chunks: &chunks,
                tests: &[login_test],
                imports: &[],
                occurrences: &[],
            })
            .unwrap();
        store
    }

    #[test]
    fn builds_evidence_backed_plan() {
        let store = test_store();
        let report = PlanEngine::new(&store).plan("token", 10).unwrap();

        assert_eq!(report.task, "token");
        assert!(!report.primary_context.is_empty());
        assert!(report
            .primary_context
            .iter()
            .any(|result| result.path == Path::new("src/auth.rs")));
        assert!(report
            .validation
            .iter()
            .any(|test| test.name == "login_returns_valid_token"));
        assert!(report
            .tool_calls
            .iter()
            .any(|call| call.tool == "impact_analysis"));
        assert_ne!(report.risk.level, "unknown");
    }

    #[test]
    fn renders_markdown_and_text() {
        let store = test_store();
        let report = PlanEngine::new(&store).plan("token", 10).unwrap();
        let markdown = PlanFormat::Markdown.render(&report).unwrap();
        let text = PlanFormat::Text.render(&report).unwrap();

        assert!(markdown.contains("# Plan: token"));
        assert!(markdown.contains("## Primary Context"));
        assert!(text.contains("Plan: token"));
        assert!(text.contains("Validation candidates"));
    }

    #[test]
    fn plan_from_bounded_context_reuses_context_impact() {
        let store = test_store();
        let evidence = Evidence {
            id: EvidenceId::new("context:bounded-search"),
            source: "open-kioku-context".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Medium,
            message:
                "context pack used persisted search results without full-table impact expansion"
                    .into(),
            indexed_at: Utc::now(),
        };
        let primary = SearchResult {
            path: PathBuf::from("src/auth.rs"),
            line_range: Some(LineRange { start: 3, end: 5 }),
            snippet: "pub fn issue_token()".into(),
            symbol: None,
            score: 1.0,
            match_reason: "test".into(),
            evidence: vec!["test evidence".into()],
            confidence: 1.0,
            score_breakdown: vec![ScoreComponent::single(
                "test_score",
                1.0,
                vec!["test evidence".into()],
                "test fixture",
            )],
        };
        let context = ContextPack {
            task: "token".into(),
            intent: "understanding".into(),
            primary_files: vec![primary],
            primary_symbols: Vec::new(),
            supporting_files: Vec::new(),
            dependency_edges: Vec::new(),
            runtime_signals: Vec::new(),
            test_candidates: Vec::new(),
            risk_report: RiskReport {
                level: "low".into(),
                score: 0.1,
                reasons: vec!["bounded context built from persisted search results".into()],
            },
            recommended_change_boundary: ChangeBoundary {
                allowed_files: vec![PathBuf::from("src/auth.rs")],
                caution_files: Vec::new(),
                forbidden_files: Vec::new(),
            },
            validation_plan: ValidationPlan {
                commands: Vec::new(),
                tests: Vec::new(),
                requires_approval: true,
                evidence: vec![evidence.clone()],
            },
            evidence: vec![evidence],
            confidence_summary: "bounded".into(),
            confidence_breakdown: ConfidenceBreakdown::default(),
        };

        let report = PlanEngine::new(&store)
            .plan_from_context("token", 5, context)
            .unwrap();

        assert_eq!(report.impact.target, "src/auth.rs");
        assert_eq!(report.impact.risk_report.level, "low");
        assert_eq!(report.impact.evidence.len(), 1);
        assert_eq!(report.impact.evidence[0].id.0, "context:bounded-search");
        assert!(!report.score_breakdown.is_empty());
        assert!(!report.confidence_breakdown.components.is_empty());
        assert!(report
            .confidence_breakdown
            .caveats
            .iter()
            .any(|caveat| caveat.contains("exact symbol/reference")));
        assert!(!report.primary_context[0].score_breakdown.is_empty());
        assert!(!report.impact.score_breakdown.is_empty());
        assert!(
            (open_kioku_core::score_component_total(&report.primary_context[0].score_breakdown)
                - report.primary_context[0].score)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn named_anchor_miss_raises_low_confidence_risk() {
        let store = test_store();
        let evidence = Evidence {
            id: EvidenceId::new("context:bounded-search"),
            source: "open-kioku-context".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Medium,
            message:
                "context pack used persisted search results without full-table impact expansion"
                    .into(),
            indexed_at: Utc::now(),
        };
        let primary = SearchResult {
            path: PathBuf::from("src/EnterpriseRateValidator.java"),
            line_range: Some(LineRange { start: 1, end: 20 }),
            snippet: "class EnterpriseRateValidator { boolean validate() { return true; } }".into(),
            symbol: None,
            score: 1.0,
            match_reason: "test".into(),
            evidence: vec!["test evidence".into()],
            confidence: 1.0,
            score_breakdown: vec![ScoreComponent::single(
                "test_score",
                1.0,
                vec!["test evidence".into()],
                "test fixture",
            )],
        };
        let context = ContextPack {
            task:
                "add validation in PublishRestrictionsMutation similar to EnterpriseRateValidator"
                    .into(),
            intent: "code_change".into(),
            primary_files: vec![primary],
            primary_symbols: Vec::new(),
            supporting_files: Vec::new(),
            dependency_edges: Vec::new(),
            runtime_signals: Vec::new(),
            test_candidates: Vec::new(),
            risk_report: RiskReport {
                level: "low".into(),
                score: 0.1,
                reasons: vec!["bounded context built from persisted search results".into()],
            },
            recommended_change_boundary: ChangeBoundary {
                allowed_files: vec![PathBuf::from("src/EnterpriseRateValidator.java")],
                caution_files: Vec::new(),
                forbidden_files: Vec::new(),
            },
            validation_plan: ValidationPlan {
                commands: Vec::new(),
                tests: Vec::new(),
                requires_approval: true,
                evidence: vec![evidence.clone()],
            },
            evidence: vec![evidence],
            confidence_summary: "bounded".into(),
            confidence_breakdown: ConfidenceBreakdown::default(),
        };

        let report = PlanEngine::new(&store)
            .plan_from_context(
                "add validation in PublishRestrictionsMutation similar to EnterpriseRateValidator",
                5,
                context,
            )
            .unwrap();

        assert_eq!(report.risk.level, "medium");
        assert!(report
            .risk
            .reasons
            .iter()
            .any(|reason| reason.contains("PublishRestrictionsMutation")));
        assert_eq!(
            report.confidence_breakdown.overall_enum,
            open_kioku_core::Confidence::Low
        );
        assert!(!report.confidence_breakdown.blockers.is_empty());
    }

    #[test]
    fn markdown_plan_renders_score_signals() {
        let store = test_store();
        let report = PlanEngine::new(&store)
            .plan("change issue_token validation", 5)
            .unwrap();
        let rendered = PlanFormat::Markdown.render(&report).unwrap();

        assert!(rendered.contains("### Score Signals"));
        assert!(rendered.contains("## Confidence"));
        assert!(rendered.contains("exact_references"));
        assert!(rendered.contains("signals:"));
        assert!(rendered.contains("score:"));
    }
}
