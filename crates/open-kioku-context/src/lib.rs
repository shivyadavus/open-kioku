use chrono::Utc;
use open_kioku_core::{
    ChangeBoundary, Confidence, ContextPack, Evidence, EvidenceId, EvidenceSourceType, GraphEdge,
    RiskReport, ValidationPlan,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_ranking::rerank;
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::OkStore;
use open_kioku_tests::TestSelector;

pub struct ContextPackBuilder<'a> {
    store: &'a dyn OkStore,
}

impl<'a> ContextPackBuilder<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self { store }
    }

    pub fn build(&self, task: &str, limit: usize) -> Result<ContextPack> {
        let files = self.store.list_files(usize::MAX, 0)?;
        let chunks = self.store.all_chunks()?;
        let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
        let primary = rerank(search_chunks(&chunks, &files, &symbols, task, limit)?);
        let primary_symbols = primary
            .iter()
            .filter_map(|result| result.symbol.clone())
            .take(10)
            .collect::<Vec<_>>();
        let mut tests = Vec::new();
        let selector = TestSelector::new(self.store as &dyn open_kioku_storage::MetadataStore);
        for result in primary.iter().take(3) {
            tests.extend(selector.for_changed_path(&result.path, 5)?);
        }
        tests.truncate(10);
        let impact = if let Some(first) = primary.first() {
            ImpactEngine::new(self.store as &dyn open_kioku_storage::MetadataStore)
                .for_file(&first.path)?
        } else {
            empty_impact(task)
        };

        let mut dependency_edges: Vec<GraphEdge> = Vec::new();
        for result in primary.iter().take(5) {
            let node_id = format!("file:{}", result.path.display());
            if let Ok((_nodes, edges)) = self.store.neighbors(&node_id, 20) {
                dependency_edges.extend(edges);
            }
        }
        dependency_edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        dependency_edges.dedup_by(|a, b| a.id == b.id);
        dependency_edges.truncate(50);

        let evidence = primary
            .iter()
            .take(20)
            .map(|result| result.evidence.clone())
            .chain(impact.evidence.clone())
            .collect::<Vec<_>>();
        let allowed_files = primary
            .iter()
            .take(8)
            .map(|result| result.path.clone())
            .collect();
        Ok(ContextPack {
            task: task.into(),
            intent: classify_intent(task).into(),
            primary_files: primary.iter().take(limit).cloned().collect(),
            primary_symbols,
            supporting_files: impact.direct_impacts.iter().take(10).cloned().collect(),
            dependency_edges,
            runtime_signals: Vec::new(),
            test_candidates: tests.clone(),
            risk_report: impact.risk_report,
            recommended_change_boundary: ChangeBoundary {
                allowed_files,
                caution_files: impact
                    .direct_impacts
                    .iter()
                    .take(8)
                    .map(|result| result.path.clone())
                    .collect(),
                forbidden_files: Vec::new(),
            },
            validation_plan: ValidationPlan {
                commands: tests.iter().filter_map(|test| test.command.clone()).collect(),
                tests,
                requires_approval: true,
                evidence: evidence.clone(),
            },
            evidence,
            confidence_summary: "ranked from lexical matches, symbol extraction, test heuristics, and impact analysis".into(),
        })
    }
}

fn classify_intent(task: &str) -> &'static str {
    let lower = task.to_ascii_lowercase();
    if lower.contains("fix")
        || lower.contains("add")
        || lower.contains("change")
        || lower.contains("implement")
    {
        "code_change"
    } else if lower.contains("test") {
        "validation"
    } else {
        "understanding"
    }
}

fn empty_impact(task: &str) -> open_kioku_core::ImpactReport {
    open_kioku_core::ImpactReport {
        target: task.into(),
        direct_impacts: Vec::new(),
        indirect_impacts: Vec::new(),
        risk_report: RiskReport {
            level: "unknown".into(),
            score: 0.0,
            reasons: vec!["no matching indexed files found".into()],
        },
        evidence: vec![Evidence {
            id: EvidenceId::new("context:no-match"),
            source: "open-kioku-context".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Low,
            message: "context pack search did not find indexed evidence".into(),
            indexed_at: Utc::now(),
        }],
    }
}
