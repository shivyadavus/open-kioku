use open_kioku_core::{
    ChangeBoundary, CompressedContextPack, ConfidenceBreakdown, ContextHandle, ContextPack,
    Evidence, LineRange, MemorySearchResult, NegativeEvidence, PlanReport, RuntimeSignal,
    ScoreComponent, SearchResult, Symbol, TestTarget, ToolCallRecommendation,
};
use std::path::PathBuf;

pub fn render_context_pack_toon(pack: &ContextPack) -> String {
    let mut out = String::new();
    push_kv(&mut out, 0, "format", "toon");
    push_kv(&mut out, 0, "type", "context_pack");
    push_kv(&mut out, 0, "task", &pack.task);
    push_kv(&mut out, 0, "intent", &pack.intent);
    push_kv(&mut out, 0, "confidence_summary", &pack.confidence_summary);
    push_confidence_breakdown(&mut out, &pack.confidence_breakdown);
    push_search_results(&mut out, "primary_context", &pack.primary_files);
    push_search_results(&mut out, "supporting_impact", &pack.supporting_files);
    push_runtime_signals(&mut out, &pack.runtime_signals);
    push_tests(&mut out, "validation", &pack.validation_plan.tests);
    push_path_list(
        &mut out,
        "allowed_files",
        &pack.recommended_change_boundary.allowed_files,
    );
    push_evidence(&mut out, &pack.evidence);
    out
}

pub fn render_compressed_context_toon(pack: &CompressedContextPack) -> String {
    let mut out = String::new();
    push_kv(&mut out, 0, "format", "toon");
    push_kv(&mut out, 0, "type", "compressed_context_pack");
    push_kv(&mut out, 0, "task", &pack.task);
    push_kv(&mut out, 0, "summary", &pack.summary);
    out.push_str("metrics:\n");
    push_kv(
        &mut out,
        1,
        "original_tokens_estimate",
        pack.original_tokens_estimate,
    );
    push_kv(
        &mut out,
        1,
        "compressed_tokens_estimate",
        pack.compressed_tokens_estimate,
    );
    push_kv(
        &mut out,
        1,
        "compression_ratio",
        format!("{:.3}", pack.compression_ratio),
    );
    push_context_handles(&mut out, &pack.handles);
    push_evidence(&mut out, &pack.evidence);
    out
}

pub fn render_plan_toon(report: &PlanReport) -> String {
    let mut out = String::new();
    push_kv(&mut out, 0, "format", "toon");
    push_kv(&mut out, 0, "type", "plan_report");
    push_kv(&mut out, 0, "task", &report.task);
    push_kv(&mut out, 0, "summary", &report.summary);
    out.push_str("risk:\n");
    push_kv(&mut out, 1, "level", &report.risk.level);
    push_kv(&mut out, 1, "score", format!("{:.2}", report.risk.score));
    push_string_list(&mut out, 1, "reasons", &report.risk.reasons);
    push_confidence_breakdown(&mut out, &report.confidence_breakdown);
    push_search_results(&mut out, "primary_context", &report.primary_context);
    push_symbols(&mut out, &report.relevant_symbols);
    push_search_results(&mut out, "impact_direct", &report.impact.direct_impacts);
    push_search_results(&mut out, "impact_indirect", &report.impact.indirect_impacts);
    push_runtime_signals(&mut out, &report.runtime_signals);
    push_tests(&mut out, "validation", &report.validation);
    push_negative_evidence(&mut out, &report.negative_evidence);
    push_evidence_by_section(&mut out, &report.evidence_by_section);
    push_score_components(&mut out, "plan_score_breakdown", &report.score_breakdown);
    push_score_components(
        &mut out,
        "impact_score_breakdown",
        &report.impact.score_breakdown,
    );
    push_memory(&mut out, &report.memory_facts);
    push_path_list(
        &mut out,
        "allowed_files",
        &report.recommended_change_boundary.allowed_files,
    );
    push_path_list(
        &mut out,
        "caution_files",
        &report.recommended_change_boundary.caution_files,
    );
    push_boundary_rules(&mut out, &report.recommended_change_boundary);
    push_string_list(&mut out, 0, "next_steps", &report.recommended_next_steps);
    push_tool_calls(&mut out, &report.tool_calls);
    push_evidence(&mut out, &report.evidence);
    push_kv(
        &mut out,
        0,
        "confidence_summary",
        &report.confidence_summary,
    );
    out
}

fn push_search_results(out: &mut String, name: &str, results: &[SearchResult]) {
    out.push_str(&format!(
        "{name}[{}]{{path,lines,score,evidence_refs,signals,reason,symbol,summary}}:\n",
        results.len()
    ));
    for result in results {
        let symbol = result
            .symbol
            .as_ref()
            .map(|symbol| symbol.qualified_name.as_str())
            .unwrap_or("");
        push_row(
            out,
            &[
                result.path.display().to_string(),
                line_range(&result.line_range),
                format!("{:.3}", result.score),
                result.derived_evidence_ids().join(","),
                top_score_signals(&result.score_breakdown),
                result.match_reason.clone(),
                symbol.to_string(),
                one_line(&result.snippet),
            ],
        );
    }
}

fn push_context_handles(out: &mut String, handles: &[ContextHandle]) {
    out.push_str(&format!(
        "handles[{}]{{id,kind,path,lines,original_tokens,compressed_tokens,entities,summary}}:\n",
        handles.len()
    ));
    for handle in handles {
        let (path, lines) = handle
            .file_range
            .as_ref()
            .map(|range| {
                (
                    range.path.display().to_string(),
                    line_range(&range.line_range),
                )
            })
            .unwrap_or_else(|| ("".into(), "".into()));
        let entities = handle
            .entities
            .iter()
            .take(6)
            .map(|entity| format!("{}:{}", entity.kind, entity.value))
            .collect::<Vec<_>>()
            .join(",");
        push_row(
            out,
            &[
                handle.id.0.clone(),
                handle.kind.clone(),
                path,
                lines,
                handle.original_tokens_estimate.to_string(),
                handle.compressed_tokens_estimate.to_string(),
                entities,
                handle.summary.clone(),
            ],
        );
    }
}

fn push_symbols(out: &mut String, symbols: &[Symbol]) {
    out.push_str(&format!(
        "relevant_symbols[{}]{{name,qualified_name,kind,file_id,lines}}:\n",
        symbols.len()
    ));
    for symbol in symbols {
        push_row(
            out,
            &[
                symbol.name.clone(),
                symbol.qualified_name.clone(),
                format!("{:?}", symbol.kind),
                symbol.file_id.0.clone(),
                line_range(&symbol.range),
            ],
        );
    }
}

fn push_runtime_signals(out: &mut String, signals: &[RuntimeSignal]) {
    out.push_str(&format!(
        "runtime_signals[{}]{{id,kind,path,lines,confidence,message}}:\n",
        signals.len()
    ));
    for signal in signals {
        let (path, lines) = signal
            .file_range
            .as_ref()
            .map(|range| {
                (
                    range.path.display().to_string(),
                    line_range(&range.line_range),
                )
            })
            .unwrap_or_else(|| ("".into(), "".into()));
        push_row(
            out,
            &[
                signal.id.clone(),
                signal.kind.clone(),
                path,
                lines,
                format!("{:?}", signal.confidence),
                one_line(&signal.message),
            ],
        );
    }
}

fn push_tests(out: &mut String, name: &str, tests: &[TestTarget]) {
    out.push_str(&format!(
        "{name}[{}]{{name,command,confidence,evidence_refs,signals,reason}}:\n",
        tests.len()
    ));
    for test in tests {
        push_row(
            out,
            &[
                test.name.clone(),
                test.command
                    .clone()
                    .unwrap_or_else(|| "manual validation".into()),
                format!("{:?}", test.confidence),
                test.evidence_refs.join(","),
                top_score_signals(&test.score_breakdown),
                test.reason.clone(),
            ],
        );
    }
}

fn push_score_components(out: &mut String, name: &str, components: &[ScoreComponent]) {
    out.push_str(&format!(
        "{name}[{}]{{signal,raw,normalized,weight,contribution,evidence,rationale}}:\n",
        components.len()
    ));
    for component in components {
        push_row(
            out,
            &[
                component.signal.clone(),
                format!("{:.3}", component.raw_value),
                format!("{:.3}", component.normalized_value),
                format!("{:.3}", component.weight),
                format!("{:.3}", component.contribution),
                component.evidence_ids.join(","),
                component.rationale.clone(),
            ],
        );
    }
}

fn push_negative_evidence(out: &mut String, items: &[NegativeEvidence]) {
    out.push_str(&format!(
        "negative_evidence[{}]{{scope,query,confidence,inspected_sources,reason,next_probe}}:\n",
        items.len()
    ));
    for item in items {
        push_row(
            out,
            &[
                item.scope.clone(),
                item.query.clone(),
                format!("{:.3}", item.confidence),
                item.inspected_sources.join(","),
                item.reason.clone(),
                item.suggested_next_probe.clone().unwrap_or_default(),
            ],
        );
    }
}

fn push_evidence_by_section(
    out: &mut String,
    sections: &std::collections::BTreeMap<String, Vec<String>>,
) {
    out.push_str(&format!(
        "evidence_by_section[{}]{{section,evidence_refs}}:\n",
        sections.len()
    ));
    for (section, refs) in sections {
        push_row(out, &[section.clone(), refs.join(",")]);
    }
}

fn push_confidence_breakdown(out: &mut String, breakdown: &ConfidenceBreakdown) {
    out.push_str("confidence:\n");
    push_kv(
        out,
        1,
        "overall_enum",
        format!("{:?}", breakdown.overall_enum),
    );
    push_kv(
        out,
        1,
        "overall_score",
        format!("{:.3}", breakdown.overall_score),
    );
    push_score_components(out, "confidence_components", &breakdown.components);
    push_string_list(out, 1, "blockers", &breakdown.blockers);
    push_string_list(out, 1, "caveats", &breakdown.caveats);
}

fn top_score_signals(components: &[ScoreComponent]) -> String {
    let signals = components
        .iter()
        .filter(|component| component.contribution.abs() > 0.001)
        .take(3)
        .map(|component| format!("{}{:+.3}", component.signal, component.contribution))
        .collect::<Vec<_>>();
    if signals.is_empty() {
        "none".into()
    } else {
        signals.join(",")
    }
}

fn push_memory(out: &mut String, facts: &[MemorySearchResult]) {
    out.push_str(&format!(
        "memory_facts[{}]{{id,score,source,confidence,text}}:\n",
        facts.len()
    ));
    for result in facts {
        push_row(
            out,
            &[
                result.fact.id.0.clone(),
                format!("{:.3}", result.score),
                result.fact.source.clone(),
                format!("{:?}", result.fact.confidence),
                result.fact.text.clone(),
            ],
        );
    }
}

fn push_tool_calls(out: &mut String, calls: &[ToolCallRecommendation]) {
    out.push_str(&format!(
        "tool_calls[{}]{{tool,purpose,arguments_json}}:\n",
        calls.len()
    ));
    for call in calls {
        push_row(
            out,
            &[
                call.tool.clone(),
                call.purpose.clone(),
                serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
            ],
        );
    }
}

fn push_evidence(out: &mut String, evidence: &[Evidence]) {
    out.push_str(&format!(
        "evidence[{}]{{id,source,confidence,message}}:\n",
        evidence.len()
    ));
    for item in evidence {
        push_row(
            out,
            &[
                item.id.0.clone(),
                item.source.clone(),
                format!("{:?}", item.confidence),
                item.message.clone(),
            ],
        );
    }
}

fn push_path_list(out: &mut String, name: &str, paths: &[PathBuf]) {
    out.push_str(&format!("{name}[{}]{{path}}:\n", paths.len()));
    for path in paths {
        push_row(out, &[path.display().to_string()]);
    }
}

fn push_boundary_rules(out: &mut String, boundary: &ChangeBoundary) {
    out.push_str(&format!(
        "allowed_rules[{}]{{path,reason,evidence_refs,symbols}}:\n",
        boundary.allowed_rules.len()
    ));
    for rule in &boundary.allowed_rules {
        push_row(
            out,
            &[
                rule.path.display().to_string(),
                rule.reason.clone(),
                rule.evidence_refs.join("|"),
                rule.symbols.join("|"),
            ],
        );
    }
    out.push_str(&format!(
        "caution_rules[{}]{{path,reason,evidence_refs,symbols}}:\n",
        boundary.caution_rules.len()
    ));
    for rule in &boundary.caution_rules {
        push_row(
            out,
            &[
                rule.path.display().to_string(),
                rule.reason.clone(),
                rule.evidence_refs.join("|"),
                rule.symbols.join("|"),
            ],
        );
    }
    out.push_str(&format!(
        "forbidden_rules[{}]{{pattern,reason,evidence_refs}}:\n",
        boundary.forbidden_rules.len()
    ));
    for rule in &boundary.forbidden_rules {
        push_row(
            out,
            &[
                rule.pattern.clone(),
                rule.reason.clone(),
                rule.evidence_refs.join("|"),
            ],
        );
    }
    out.push_str(&format!(
        "boundary_expansion[{}]{{reason,required_evidence_refs}}:\n",
        boundary.expansion_requirements.len()
    ));
    for requirement in &boundary.expansion_requirements {
        push_row(
            out,
            &[
                requirement.reason.clone(),
                requirement.required_evidence_refs.join("|"),
            ],
        );
    }
}

fn push_string_list(out: &mut String, indent: usize, name: &str, values: &[String]) {
    out.push_str(&format!(
        "{}{name}[{}]{{text}}:\n",
        "  ".repeat(indent),
        values.len()
    ));
    for value in values {
        out.push_str(&"  ".repeat(indent + 1));
        out.push_str(&cell(value));
        out.push('\n');
    }
}

fn push_kv(out: &mut String, indent: usize, key: &str, value: impl ToString) {
    out.push_str(&"  ".repeat(indent));
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&scalar(&value.to_string()));
    out.push('\n');
}

fn push_row(out: &mut String, cells: &[String]) {
    out.push_str("  ");
    out.push_str(
        &cells
            .iter()
            .map(|value| cell(value))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push('\n');
}

fn scalar(value: &str) -> String {
    let value = clean(value);
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, ':' | '|' | ',' | '{' | '}' | '[' | ']'))
    {
        serde_json::to_string(&value).unwrap_or_else(|_| "\"\"".into())
    } else {
        value
    }
}

fn cell(value: &str) -> String {
    clean(value)
        .replace('|', "\\|")
        .replace(',', "\\,")
        .replace('{', "\\{")
        .replace('}', "\\}")
}

fn clean(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn one_line(value: &str) -> String {
    let line = clean(value);
    const MAX: usize = 220;
    if line.len() <= MAX {
        line
    } else {
        format!("{}...", line.chars().take(MAX).collect::<String>())
    }
}

fn line_range(range: &Option<LineRange>) -> String {
    range
        .as_ref()
        .map(|range| format!("{}-{}", range.start, range.end))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use open_kioku_core::{
        ChangeBoundary, Confidence, ContextHandleId, EvidenceId, EvidenceSourceType, FileRange,
        ImpactReport, RiskReport, ScoreComponent,
    };

    #[test]
    fn renders_compressed_context_as_columnar_toon() {
        let pack = CompressedContextPack {
            task: "fix token auth".into(),
            summary: "1 handle".into(),
            handles: vec![ContextHandle {
                id: ContextHandleId::new("ctx:abc"),
                kind: "primary".into(),
                summary: "primary auth.rs:1-3 issue_token".into(),
                file_range: Some(FileRange {
                    path: "src/auth.rs".into(),
                    line_range: Some(LineRange { start: 1, end: 3 }),
                }),
                entities: Vec::new(),
                original_tokens_estimate: 40,
                compressed_tokens_estimate: 8,
            }],
            original_tokens_estimate: 40,
            compressed_tokens_estimate: 8,
            compression_ratio: 0.2,
            evidence: Vec::new(),
        };

        let rendered = render_compressed_context_toon(&pack);

        assert!(rendered.contains("type: compressed_context_pack"));
        assert!(rendered.contains("handles[1]{id,kind,path,lines"));
        assert!(rendered.contains("ctx:abc | primary | src/auth.rs | 1-3"));
    }

    #[test]
    fn renders_plan_without_repeating_record_keys() {
        let report = PlanReport {
            task: "fix token auth".into(),
            summary: "Found context".into(),
            primary_context: Vec::new(),
            relevant_symbols: Vec::new(),
            impact: ImpactReport {
                target: "src/auth.rs".into(),
                direct_impacts: Vec::new(),
                indirect_impacts: Vec::new(),
                risk_report: RiskReport {
                    level: "low".into(),
                    score: 0.1,
                    reasons: Vec::new(),
                },
                evidence: Vec::new(),
                score_breakdown: vec![ScoreComponent::single(
                    "impact_fixture",
                    0.1,
                    Vec::new(),
                    "format fixture",
                )],
            },
            validation: Vec::new(),
            risk: RiskReport {
                level: "low".into(),
                score: 0.1,
                reasons: vec!["bounded".into()],
            },
            recommended_change_boundary: ChangeBoundary {
                allowed_files: vec!["src/auth.rs".into()],
                caution_files: Vec::new(),
                forbidden_files: Vec::new(),
                evidence_refs: Vec::new(),
                ..Default::default()
            },
            recommended_next_steps: vec!["Inspect context".into()],
            tool_calls: Vec::new(),
            memory_facts: Vec::new(),
            runtime_signals: Vec::new(),
            evidence: vec![Evidence {
                id: EvidenceId::new("ev"),
                source: "test".into(),
                source_type: EvidenceSourceType::Heuristic,
                file_range: None,
                symbol_id: None,
                confidence: Confidence::Medium,
                message: "rendered".into(),
                indexed_at: Utc::now(),
                ..Default::default()
            }],
            evidence_by_section: std::collections::BTreeMap::new(),
            negative_evidence: Vec::new(),
            confidence_summary: "test".into(),
            confidence_breakdown: ConfidenceBreakdown::default(),
            score_breakdown: vec![ScoreComponent::single(
                "plan_fixture",
                0.1,
                vec!["ev".into()],
                "format fixture",
            )],
        };

        let rendered = render_plan_toon(&report);

        assert!(rendered.contains("type: plan_report"));
        assert!(rendered.contains("confidence:"));
        assert!(rendered.contains("confidence_components"));
        assert!(rendered.contains(
            "primary_context[0]{path,lines,score,evidence_refs,signals,reason,symbol,summary}:"
        ));
        assert!(rendered.contains("evidence_by_section"));
        assert!(rendered.contains("allowed_files[1]{path}:"));
        assert!(rendered.contains("allowed_rules[0]{path,reason,evidence_refs,symbols}:"));
        assert!(rendered.contains("forbidden_rules[0]{pattern,reason,evidence_refs}:"));
    }
}
