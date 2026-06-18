use chrono::Utc;
use open_kioku_core::{
    search_result_evidence_ids, AnalysisFact, CodeChunk, Confidence, Evidence, EvidenceId,
    EvidenceSourceType, File, FileId, FileRange, ImpactReport, RiskReport, ScoreComponent,
    SearchResult, Symbol, SymbolOccurrence,
};
use open_kioku_errors::Result;
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{MetadataStore, SearchIndex};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub struct ImpactEngine<'a> {
    store: &'a dyn MetadataStore,
    search_index: Option<&'a dyn SearchIndex>,
}

impl<'a> ImpactEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self {
            store,
            search_index: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn for_file(&self, path: &Path) -> Result<ImpactReport> {
        let file = self.store.get_file_by_path(path)?;
        let target_symbols = if let Some(file) = &file {
            self.store.symbols_for_file(&file.id)?
        } else {
            Vec::new()
        };
        let runtime_facts = if let Some(file) = &file {
            runtime_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };
        let git_facts = if let Some(file) = &file {
            git_history_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };

        let direct = if let Some(file) = &file {
            let mut direct = exact_reference_impacts(self.store, file, &target_symbols)?;
            direct.extend(git_cochange_impacts(self.store, file, &git_facts)?);
            direct.extend(runtime_impacts(
                self.store,
                self.search_index,
                file,
                &runtime_facts,
            )?);
            for term in impact_terms(path, file, &target_symbols)
                .into_iter()
                .take(8)
            {
                let results = if let Some(index) = self.search_index {
                    index.search(&term, 25)?
                } else {
                    let files = self.store.list_files(usize::MAX, 0)?;
                    let chunks = self.store.all_chunks()?;
                    let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
                    search_chunks(&chunks, &files, &symbols, &term, 25)?
                };
                direct.extend(
                    results
                        .into_iter()
                        .filter(|result| result.path != file.path),
                );
            }
            direct = dedupe_results(direct);
            direct.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            direct.truncate(25);
            direct
        } else {
            Vec::new()
        };

        // Second-level: for each direct impact, search for that file's stem
        // to find indirect dependents (callers-of-callers).
        let mut indirect: Vec<open_kioku_core::SearchResult> = Vec::new();
        let direct_paths: std::collections::HashSet<_> =
            direct.iter().map(|r| r.path.clone()).collect();
        for direct_result in direct.iter().take(5) {
            let indirect_stem = direct_result
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if indirect_stem.is_empty() || indirect_stem.len() < 3 {
                continue;
            }
            let second = if let Some(index) = self.search_index {
                index.search(indirect_stem, 10)?
            } else {
                let files = self.store.list_files(usize::MAX, 0)?;
                let chunks = self.store.all_chunks()?;
                let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
                search_chunks(&chunks, &files, &symbols, indirect_stem, 10)?
            };
            for result in second {
                if result.path != path && !direct_paths.contains(&result.path) {
                    indirect.push(result);
                }
            }
        }
        indirect.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        indirect.dedup_by(|a, b| a.path == b.path);
        indirect.truncate(15);
        let mut reasons = Vec::new();
        let exact_reference_count = direct
            .iter()
            .filter(|result| result.match_reason.contains("exact symbol reference"))
            .count();
        if exact_reference_count > 0 {
            reasons.push(format!(
                "{exact_reference_count} exact indexed symbol reference(s) found"
            ));
        }
        if direct.len() > 10 {
            reasons.push("many lexical dependents reference this file or its symbols".into());
        }
        if !runtime_facts.is_empty() {
            reasons.push(format!(
                "{} local runtime trace/log/incident fact(s) touch this file",
                runtime_facts.len()
            ));
        }
        if !git_facts.is_empty() {
            reasons.push(format!(
                "{} git co-change or historical validation fact(s) touch this file",
                git_facts.len()
            ));
        }
        if path.to_string_lossy().contains("api") {
            reasons.push("API-layer path suggests public integration surface".into());
        }
        if reasons.is_empty() {
            reasons.push("limited indexed downstream references found".into());
        }
        let runtime_score = (runtime_facts.len() as f32 / 12.0).min(0.25);
        let git_score = (git_facts.len() as f32 / 12.0).min(0.20);
        let direct_reference_score = (direct.len() as f32 / 20.0).min(1.0);
        let score = (direct_reference_score + runtime_score + git_score).min(1.0);
        let evidence = Evidence {
            id: EvidenceId::new(format!("impact:{}", path.display())),
            source: "open-kioku-impact".into(),
            source_type: if exact_reference_count > 0 {
                EvidenceSourceType::Scip
            } else {
                EvidenceSourceType::Lexical
            },
            file_range: Some(FileRange {
                path: path.to_path_buf(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: if file.is_some() {
                if exact_reference_count > 0 {
                    Confidence::High
                } else {
                    Confidence::Medium
                }
            } else {
                Confidence::Low
            },
            message: if exact_reference_count > 0 {
                "impact report derived from exact indexed symbol references and lexical references"
                    .into()
            } else {
                "impact report derived from indexed symbols and lexical references".into()
            },
            indexed_at: Utc::now(),
            ..Default::default()
        };
        let runtime_evidence = runtime_facts
            .iter()
            .map(|fact| runtime_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let git_evidence = git_facts
            .iter()
            .map(|fact| git_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let mut report = ImpactReport {
            target: path.display().to_string(),
            direct_impacts: direct,
            indirect_impacts: indirect,
            risk_report: RiskReport {
                level: if score > 0.6 {
                    "high"
                } else if score > 0.25 {
                    "medium"
                } else {
                    "low"
                }
                .into(),
                score,
                reasons,
            },
            evidence: std::iter::once(evidence)
                .chain(runtime_evidence)
                .chain(git_evidence)
                .collect(),
            score_breakdown: vec![ScoreComponent::single(
                "direct_reference_density",
                direct_reference_score,
                vec![format!("impact:{}", path.display())],
                "impact risk from count of exact and lexical direct dependents",
            )],
        };
        if runtime_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "runtime_corroboration",
                runtime_score,
                runtime_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by local runtime trace/log/incident facts",
            ));
        }
        if git_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "git_cochange",
                git_score,
                git_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by git co-change and historical validation facts",
            ));
        }
        report.reconcile_score_breakdown();
        Ok(report)
    }
}

fn runtime_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    Ok(store
        .analysis_facts(Some(EvidenceSourceType::Runtime), 500)?
        .into_iter()
        .filter(|fact| &fact.file_id == file_id)
        .take(12)
        .collect())
}

fn git_history_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    Ok(store
        .analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?
        .into_iter()
        .filter(|fact| &fact.file_id == file_id)
        .take(12)
        .collect())
}

fn git_cochange_impacts(
    store: &dyn MetadataStore,
    target_file: &File,
    git_facts: &[AnalysisFact],
) -> Result<Vec<SearchResult>> {
    let mut results = Vec::new();
    for fact in git_facts.iter().take(12) {
        let target = Path::new(&fact.target);
        let Some(file) = store.get_file_by_path(target)? else {
            continue;
        };
        if file.path == target_file.path {
            continue;
        }
        let snippet = store
            .chunks_for_file(&file.id)?
            .first()
            .map(|chunk| chunk.text.clone())
            .unwrap_or_else(|| file.path.display().to_string());
        let evidence = vec![format!(
            "git co-change from local history: `{}` changed with `{}` ({})",
            target_file.path.display(),
            file.path.display(),
            fact.message
        )];
        results.push(SearchResult {
            path: file.path.clone(),
            line_range: None,
            snippet,
            symbol: None,
            score: 1.15 + fact.confidence.score(),
            match_reason: "historical git co-change with target file".into(),
            evidence,
            evidence_refs: vec![fact.id.clone()],
            confidence: fact.confidence.score(),
            score_breakdown: vec![ScoreComponent::single(
                "git_cochange",
                0.20,
                vec![fact.id.clone()],
                "impact candidate historically changed with the target file",
            )],
        });
    }
    Ok(dedupe_results(results))
}

fn runtime_impacts(
    store: &dyn MetadataStore,
    search_index: Option<&dyn SearchIndex>,
    target_file: &File,
    runtime_facts: &[AnalysisFact],
) -> Result<Vec<SearchResult>> {
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = if search_index.is_none() {
        store.all_chunks()?
    } else {
        Vec::new()
    };
    let symbols = if search_index.is_none() {
        store.list_symbols(None, usize::MAX, 0)?
    } else {
        Vec::new()
    };
    let mut results = Vec::new();
    for fact in runtime_facts.iter().take(6) {
        for term in runtime_search_terms(fact).into_iter().take(3) {
            let matches = if let Some(index) = search_index {
                index.search(&term, 10)?
            } else {
                search_chunks(&chunks, &files, &symbols, &term, 10)?
            };
            for mut result in matches {
                if result.path == target_file.path {
                    continue;
                }
                annotate_runtime_impact(&mut result, fact);
                results.push(result);
            }
        }
    }
    Ok(dedupe_results(results))
}

fn annotate_runtime_impact(result: &mut SearchResult, fact: &AnalysisFact) {
    let evidence = format!(
        "runtime corroboration from local artifact `{}` targeting `{}`",
        fact.source, fact.target
    );
    if !result.evidence.contains(&evidence) {
        result.evidence.push(evidence);
    }
    if !result.evidence_refs.contains(&fact.id) {
        result.evidence_refs.push(fact.id.clone());
    }
    result.score += 0.20;
    result.confidence = result.confidence.max(fact.confidence.score());
    result.score_breakdown.push(ScoreComponent::adjustment(
        "runtime_corroboration",
        0.20,
        vec![fact.id.clone()],
        "impact candidate matched observed runtime endpoint, SQL table, or incident",
    ));
}

fn runtime_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::Runtime,
        file_range: Some(FileRange {
            path: path.to_path_buf(),
            line_range: fact.range.clone(),
        }),
        symbol_id: fact.symbol_id.clone(),
        confidence: fact.confidence,
        message: format!("{}: {}", fact.message, fact.target),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn git_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::GitHistory,
        file_range: Some(FileRange {
            path: path.to_path_buf(),
            line_range: None,
        }),
        symbol_id: None,
        confidence: fact.confidence,
        message: format!("{}: {}", fact.message, fact.target),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn runtime_search_terms(fact: &AnalysisFact) -> Vec<String> {
    let mut terms = vec![fact.target.clone()];
    terms.extend(
        fact.target
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
            .filter(|part| part.len() >= 4)
            .map(ToOwned::to_owned),
    );
    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

fn exact_reference_impacts(
    store: &dyn MetadataStore,
    target_file: &File,
    symbols: &[Symbol],
) -> Result<Vec<SearchResult>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    let files = store.list_files(usize::MAX, 0)?;
    let files_by_id = files
        .iter()
        .map(|file| (file.id.clone(), file.clone()))
        .collect::<HashMap<FileId, File>>();
    let mut results = Vec::new();
    for symbol in symbols
        .iter()
        .filter(|symbol| symbol.file_id == target_file.id)
    {
        if is_generic_symbol_name(&symbol.name) {
            continue;
        }
        for occurrence in store.references_for_symbol(&symbol.id, 100)? {
            if occurrence.file_id == target_file.id {
                continue;
            }
            if let Some(result) = occurrence_result(store, &files_by_id, symbol, &occurrence)? {
                results.push(result);
            }
        }
    }

    Ok(dedupe_results(results))
}

fn occurrence_result(
    store: &dyn MetadataStore,
    files_by_id: &HashMap<FileId, File>,
    symbol: &Symbol,
    occurrence: &SymbolOccurrence,
) -> Result<Option<SearchResult>> {
    let Some(file) = files_by_id.get(&occurrence.file_id) else {
        return Ok(None);
    };
    let chunks = store.chunks_for_file(&occurrence.file_id)?;
    let snippet = best_occurrence_snippet(&chunks, occurrence, &symbol.name);
    let source = match occurrence.provenance {
        EvidenceSourceType::Scip => "SCIP",
        EvidenceSourceType::TreeSitter => "tree-sitter",
        EvidenceSourceType::Lsp => "LSP",
        _ => "indexed",
    };
    let score = 1.25 + occurrence.confidence.score();
    let evidence = vec![format!(
        "exact reference to `{}` from `{source}` occurrence data",
        symbol.qualified_name
    )];
    let line_range = occurrence.range.clone();
    let evidence_ids = search_result_evidence_ids(&file.path, &line_range, evidence.len());
    Ok(Some(SearchResult {
        path: file.path.clone(),
        line_range,
        snippet,
        symbol: None,
        score,
        match_reason: format!("exact symbol reference via {source}"),
        evidence: evidence.clone(),
        evidence_refs: evidence_ids.clone(),
        confidence: occurrence.confidence.score(),
        score_breakdown: vec![ScoreComponent::single(
            "exact_symbol_reference",
            score,
            evidence_ids,
            format!("{source} occurrence confidence plus exact-reference base weight"),
        )],
    }))
}

fn best_occurrence_snippet(
    chunks: &[CodeChunk],
    occurrence: &SymbolOccurrence,
    symbol_name: &str,
) -> String {
    let occurrence_line = occurrence.range.as_ref().map(|range| range.start);
    let chunk = occurrence_line
        .and_then(|line| {
            chunks
                .iter()
                .find(|chunk| chunk.range.start <= line && line <= chunk.range.end)
        })
        .or_else(|| chunks.iter().find(|chunk| chunk.text.contains(symbol_name)))
        .or_else(|| chunks.first());

    chunk
        .and_then(|chunk| {
            chunk
                .text
                .lines()
                .find(|line| line.contains(symbol_name))
                .or_else(|| chunk.text.lines().next())
        })
        .unwrap_or(symbol_name)
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn dedupe_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut by_path = BTreeMap::<String, SearchResult>::new();
    for result in results {
        let key = result_key(&result);
        match by_path.get_mut(&key) {
            Some(existing) => {
                if result.score > existing.score {
                    existing.score = result.score;
                    existing.snippet = result.snippet.clone();
                    existing.line_range = result.line_range.clone();
                    existing.match_reason = result.match_reason.clone();
                    existing.confidence = existing.confidence.max(result.confidence);
                    existing.score_breakdown = result.score_breakdown.clone();
                }
                for evidence in result.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
                existing.reconcile_score_breakdown();
            }
            None => {
                by_path.insert(key, result);
            }
        }
    }
    by_path.into_values().collect()
}

fn result_key(result: &SearchResult) -> String {
    format!(
        "{}:{}-{}",
        result.path.display(),
        result
            .line_range
            .as_ref()
            .map(|range| range.start)
            .unwrap_or_default(),
        result
            .line_range
            .as_ref()
            .map(|range| range.end)
            .unwrap_or_default()
    )
}

fn impact_terms(
    path: &Path,
    file: &open_kioku_core::File,
    symbols: &[open_kioku_core::Symbol],
) -> Vec<String> {
    let mut terms = symbols
        .iter()
        .filter(|symbol| symbol.file_id == file.id)
        .filter(|symbol| !is_generic_symbol_name(&symbol.name))
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>();

    if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
        if !is_generic_symbol_name(stem) {
            terms.push(stem.into());
        }
    }

    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

fn is_generic_symbol_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "args"
            | "cli"
            | "command"
            | "commands"
            | "config"
            | "from"
            | "helpers"
            | "index"
            | "lib"
            | "main"
            | "mod"
            | "output"
            | "path"
            | "repo"
            | "run"
            | "test"
            | "tests"
            | "to"
            | "types"
            | "utils"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        CodeChunk, File, FileId, IndexManifest, IndexQuality, Language, LineRange, Repository,
        RepositoryId, SymbolId, SymbolKind,
    };
    use open_kioku_storage::IndexData;
    use open_kioku_storage_sqlite::SqliteStore;
    use std::path::PathBuf;

    fn make_store() -> SqliteStore {
        SqliteStore::open(":memory:").unwrap()
    }

    #[test]
    fn derives_impacts_from_chunks() {
        let store = make_store();

        let manifest = IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 0,
            chunk_count: 2,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        let f1 = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/core.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f2 = File {
            id: FileId::new("f2"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/app.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f3 = File {
            id: FileId::new("f3"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };

        let c1 = CodeChunk {
            id: "c1".into(),
            file_id: FileId::new("f2"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::core::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };
        let c2 = CodeChunk {
            id: "c2".into(),
            file_id: FileId::new("f3"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::app::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[f1, f2, f3],
                symbols: &[],
                occurrences: &[],
                chunks: &[c1, c2],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
            })
            .unwrap();

        let engine = ImpactEngine::new(&store);

        let report = engine.for_file(Path::new("src/core.rs")).unwrap();

        // core is referenced by app (c1), so app is direct.
        assert_eq!(report.direct_impacts.len(), 1);
        assert_eq!(
            report.direct_impacts[0].path.display().to_string(),
            "src/app.rs"
        );

        // app is referenced by main (c2), so main is indirect.
        assert_eq!(report.indirect_impacts.len(), 1);
        assert_eq!(
            report.indirect_impacts[0].path.display().to_string(),
            "src/main.rs"
        );
    }

    #[test]
    fn exact_symbol_references_count_as_direct_impact() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let source = File {
            id: FileId::new("source"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/rates.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "source".into(),
            is_generated: false,
            is_vendor: false,
        };
        let caller = File {
            id: FileId::new("caller"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/publisher.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "caller".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("symbol:rate_validator"),
            name: "RateValidator".into(),
            qualified_name: "rates::RateValidator".into(),
            kind: SymbolKind::Class,
            file_id: source.id.clone(),
            range: Some(LineRange { start: 1, end: 5 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        };
        let chunks = vec![
            CodeChunk {
                id: "source-chunk".into(),
                file_id: source.id.clone(),
                range: LineRange { start: 1, end: 5 },
                language: Language::Rust,
                text: "pub struct RateValidator;".into(),
                symbol_id: Some(symbol.id.clone()),
            },
            CodeChunk {
                id: "caller-chunk".into(),
                file_id: caller.id.clone(),
                range: LineRange { start: 10, end: 12 },
                language: Language::Rust,
                text: "let validator = RateValidator::new();".into(),
                symbol_id: None,
            },
        ];
        let occurrences = vec![
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: source.id.clone(),
                range: Some(LineRange { start: 1, end: 1 }),
                is_definition: true,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: caller.id.clone(),
                range: Some(LineRange { start: 10, end: 10 }),
                is_definition: false,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
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
            file_count: 2,
            symbol_count: 1,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[source, caller],
                symbols: &[symbol],
                occurrences: &occurrences,
                chunks: &chunks,
                imports: &[],
                tests: &[],
                analysis_facts: &[],
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/rates.rs"))
            .unwrap();

        assert!(report
            .direct_impacts
            .iter()
            .any(|result| result.path == Path::new("src/publisher.rs")
                && result.match_reason.contains("exact symbol reference")));
        assert!(report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("exact indexed symbol reference")));
    }
}
