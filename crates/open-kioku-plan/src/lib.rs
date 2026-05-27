use open_kioku_context::ContextPackBuilder;
use open_kioku_core::{
    ChangeBoundary, ContextPack, ImpactReport, PlanReport, RiskReport, SearchResult, TestTarget,
    ToolCallRecommendation,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_storage::{MetadataStore, OkStore};
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
}

impl PlanFormat {
    pub fn render(&self, report: &PlanReport) -> Result<String> {
        match self {
            Self::Json => Ok(serde_json::to_string_pretty(report)?),
            Self::Markdown => Ok(render_markdown(report)),
            Self::Text => Ok(render_text(report)),
        }
    }
}

pub struct PlanEngine<'a> {
    store: &'a dyn OkStore,
}

impl<'a> PlanEngine<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self { store }
    }

    pub fn plan(&self, task: &str, limit: usize) -> Result<PlanReport> {
        let context_limit = limit.clamp(1, 100);
        let context = ContextPackBuilder::new(self.store).build(task, context_limit)?;
        let primary_context = context
            .primary_files
            .iter()
            .take(MAX_PRIMARY_CONTEXT.min(context_limit))
            .cloned()
            .collect::<Vec<_>>();
        let impact_target = impact_target(&primary_context);
        let impact = self.impact_for_primary_context(task, impact_target, &context)?;
        let validation = self.validation_for_context(&primary_context, &context)?;
        let risk = merge_risk(
            &context.risk_report,
            &impact.risk_report,
            primary_context.is_empty(),
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
        let recommended_next_steps = next_steps(&primary_context, &impact, &validation);
        let tool_calls = tool_calls(task, impact_target);
        let evidence = context
            .evidence
            .iter()
            .chain(impact.evidence.iter())
            .cloned()
            .collect::<Vec<_>>();
        let summary = summary(task, &primary_context, &impact, &validation, &risk);

        Ok(PlanReport {
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
            evidence,
            confidence_summary:
                "derived from local lexical search, symbol extraction, impact analysis, and test heuristics"
                    .into(),
        })
    }

    fn impact_for_primary_context(
        &self,
        task: &str,
        impact_target: Option<&SearchResult>,
        context: &ContextPack,
    ) -> Result<ImpactReport> {
        if let Some(target) = impact_target {
            ImpactEngine::new(self.store as &dyn MetadataStore).for_file(&target.path)
        } else {
            Ok(ImpactReport {
                target: task.into(),
                direct_impacts: Vec::new(),
                indirect_impacts: Vec::new(),
                risk_report: context.risk_report.clone(),
                evidence: context.evidence.clone(),
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
            by_id.insert(test.id.clone(), test.clone());
        }

        let selector = TestSelector::new(self.store as &dyn MetadataStore);
        for result in source_results(primary_context).take(3) {
            for test in selector.for_changed_path(&result.path, MAX_VALIDATION)? {
                by_id.entry(test.id.clone()).or_insert(test);
            }
        }

        let mut tests = by_id.into_values().collect::<Vec<_>>();
        tests.sort_by(|a, b| {
            b.confidence
                .score()
                .partial_cmp(&a.confidence.score())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        tests.truncate(MAX_VALIDATION);
        Ok(tests)
    }
}

fn merge_risk(context: &RiskReport, impact: &RiskReport, no_matches: bool) -> RiskReport {
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

    let score = impact.score.max(context.score);
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

fn tool_calls(task: &str, impact_target: Option<&SearchResult>) -> Vec<ToolCallRecommendation> {
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

    calls
}

fn summary(
    task: &str,
    primary_context: &[SearchResult],
    impact: &ImpactReport,
    validation: &[TestTarget],
    risk: &RiskReport,
) -> String {
    if primary_context.is_empty() {
        return format!(
            "No indexed context matched `{task}`. Refine the task or re-index the repo."
        );
    }
    format!(
        "Found {} primary context item(s), {} direct impact candidate(s), {} validation candidate(s); risk is {}.",
        primary_context.len(),
        impact.direct_impacts.len(),
        validation.len(),
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
    for reason in &report.risk.reasons {
        out.push_str(&format!("  - {reason}\n"));
    }

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
            out.push_str(&format!("  - {} [{}]\n", test.name, command));
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
            out.push_str(&format!("- `{}` via `{}`\n", test.name, command));
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
            "  - {}{}: {}\n",
            result.path.display(),
            range,
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
            "- `{}`{}: {}\n",
            result.path.display(),
            line_range(result),
            one_line(&result.snippet)
        ));
    }
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
        CodeChunk, Confidence, File, FileId, IndexManifest, Language, LineRange, Repository,
        RepositoryId, Symbol, SymbolId, SymbolKind, TestTarget,
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
}
