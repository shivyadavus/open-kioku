from pathlib import Path

# Core: persist task-family routing as structured retrieval diagnostics.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
marker = '''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetrievalDiagnostics {
'''
insert = '''#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskFamily {
    IssueToCode,
    CodeToTest,
    TraceToCode,
    CommentToContext,
    EditToRipple,
    Documentation,
    MixedCodeDocs,
    #[default]
    General,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalRoutingDiagnostics {
    pub task_family: TaskFamily,
    pub confidence: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub enabled_sources: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub required_evidence: Vec<RetrievalSourceKind>,
}

impl Default for RetrievalRoutingDiagnostics {
    fn default() -> Self {
        Self {
            task_family: TaskFamily::General,
            confidence: 0.0,
            reasons: Vec::new(),
            enabled_sources: Vec::new(),
            required_evidence: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetrievalDiagnostics {
'''
if text.count(marker) != 1:
    raise SystemExit(f'core routing insertion marker count={text.count(marker)}')
text = text.replace(marker, insert, 1)
old = '''    #[serde(default)]
    pub selection: ContextSelectionDiagnostics,
}
'''
new = '''    #[serde(default)]
    pub selection: ContextSelectionDiagnostics,
    #[serde(default)]
    pub routing: RetrievalRoutingDiagnostics,
}
'''
if text.count(old) != 1:
    raise SystemExit(f'core retrieval diagnostics field marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))

# Fusion diagnostics get a backwards-compatible routing default.
path = Path('crates/open-kioku-context/src/candidates.rs')
text = path.read_text()
old = '''            sources_succeeded: succeeded.into_iter().collect(),
            selection: Default::default(),
        },
'''
new = '''            sources_succeeded: succeeded.into_iter().collect(),
            selection: Default::default(),
            routing: Default::default(),
        },
'''
if text.count(old) != 1:
    raise SystemExit(f'fusion diagnostics marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))

# Context: classify before retrieval, route candidate streams, preserve provenance, record rationale.
path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = 'pub mod candidates;\n'
new = 'pub mod candidates;\npub mod routing;\n'
if text.count(old) != 1:
    raise SystemExit(f'context module marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        let intent = TaskSearchIntent::parse(task);
        let candidate_limit = limit.saturating_mul(4).clamp(20, 200);
        let request =
            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit);
        let external_streams = candidates::retrieve_candidate_streams(external_sources, &request);
'''
new = '''        let intent = TaskSearchIntent::parse(task);
        let routing = routing::classify_task(task);
        let candidate_limit = limit
            .saturating_mul(routing.policy.candidate_multiplier)
            .clamp(20, 200);
        let request =
            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit);
        let routed_external_sources = external_sources
            .iter()
            .copied()
            .filter(|source| routing.policy.allows(source.source()))
            .collect::<Vec<_>>();
        let external_streams =
            candidates::retrieve_candidate_streams(&routed_external_sources, &request);
'''
if text.count(old) != 1:
    raise SystemExit(f'context request marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        let mut streams = candidates::builtins::BuiltinCandidateContext {
            store: self.store,
            history_store: self.history_store,
            files: &files,
            chunks: &chunks,
            symbols: &symbols,
        }
        .collect_excluding(&request, &overridden_sources);
        streams.extend(external_streams);
        let fusion_config = candidates::FusionConfig::from_ranking_options(&self.ranking_options);
        let fused = candidates::fuse_candidate_streams(&streams, candidate_limit, &fusion_config);
        let mut diagnostics = fused.diagnostics;
'''
new = '''        let mut streams = candidates::builtins::BuiltinCandidateContext {
            store: self.store,
            history_store: self.history_store,
            files: &files,
            chunks: &chunks,
            symbols: &symbols,
        }
        .collect_excluding(&request, &overridden_sources);
        streams.retain(|stream| routing.policy.allows(stream.source));
        streams.extend(external_streams);
        // Task routing changes which evidence families run and how much candidate headroom they
        // receive. It deliberately does not introduce uncalibrated fusion weights: the measured
        // product default remains unweighted RRF unless repository ranking configuration says otherwise.
        let fusion_config = candidates::FusionConfig::from_ranking_options(&self.ranking_options);
        let fused = candidates::fuse_candidate_streams(&streams, candidate_limit, &fusion_config);
        let mut diagnostics = fused.diagnostics;
        diagnostics.routing = routing.diagnostics();
        for required in &diagnostics.routing.required_evidence {
            let contributed = diagnostics.traces.iter().any(|trace| {
                trace
                    .contributions
                    .iter()
                    .any(|contribution| contribution.source == *required)
            });
            if !contributed {
                diagnostics.caveats.push(format!(
                    "task-family policy requires {} evidence, but it did not contribute task-relevant evidence",
                    retrieval_source_label(*required)
                ));
            }
        }
'''
if text.count(old) != 1:
    raise SystemExit(f'context fusion marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            open_kioku_core::RetrievalDiagnostics::default(),
        )
    }
'''
new = '''            {
                let mut diagnostics = open_kioku_core::RetrievalDiagnostics::default();
                diagnostics.routing = routing::classify_task(task).diagnostics();
                diagnostics
            },
        )
    }
'''
if text.count(old) != 1:
    raise SystemExit(f'context direct-primary diagnostics marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '    out.push_str("## Retrieval\\n\\n");\n'
new = '''    out.push_str("## Retrieval\\n\\n");
    out.push_str(&format!(
        "- Task family: `{:?}` (confidence `{:.2}`)\\n",
        diagnostics.routing.task_family, diagnostics.routing.confidence
    ));
    for reason in &diagnostics.routing.reasons {
        out.push_str(&format!("  - Routing rationale: {reason}\\n"));
    }
'''
if text.count(old) != 1:
    raise SystemExit(f'markdown routing marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''fn write_prompt_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    if !diagnostics.sources_attempted.is_empty() {
'''
new = '''fn write_prompt_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    out.push_str(&format!(
        "TASK_FAMILY: {:?} confidence={:.2}\\n",
        diagnostics.routing.task_family, diagnostics.routing.confidence
    ));
    for reason in &diagnostics.routing.reasons {
        out.push_str(&format!("TASK_ROUTING_RATIONALE: {reason}\\n"));
    }
    if !diagnostics.sources_attempted.is_empty() {
'''
if text.count(old) != 1:
    raise SystemExit(f'prompt routing marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))
