fn source_root_hash(repo: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"open-kioku-source-root-v1\0");
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    if let Some(commit) = open_kioku_git::commit(repo) {
        hasher.update(commit.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn open_store(repo: impl AsRef<Path>) -> anyhow::Result<SqliteStore> {
    Ok(SqliteStore::open(repo.as_ref().join(".ok/index.sqlite"))?)
}

fn search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    search_with_ranking_mode(repo, store, query, limit, RankingMode::Fusion)
}

fn graph_search(
    repo: impl AsRef<Path>,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let index_dir = default_index_dir(repo);
    if !TantivySearchIndex::exists(&index_dir) {
        anyhow::bail!("graph search index is missing; run `ok index .` first");
    }
    Ok(TantivySearchIndex::open_or_create(index_dir)?.search_graph(query, limit)?)
}

fn semantic_search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let mut config = OkConfig::load_from_repo(repo)?;
    config.semantic.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &config.semantic);
    let mut results = manager.search(query, limit)?;
    let mut options = ranking_options_for_repo(repo)?;
    options.query = Some(query.into());
    Ok(top_unique_paths(
        rerank_with_options(results.split_off(0), &options),
        limit,
    ))
}

fn hybrid_search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let candidate_limit = ranking_candidate_limit(limit);
    let mut raw = search_raw(repo, store, query, candidate_limit)?;
    annotate_candidates_with_git_history(store, &mut raw)?;

    let mut config = OkConfig::load_from_repo(repo)?;
    config.semantic.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &config.semantic);
    if manager.status().ready {
        raw.extend(manager.search(query, candidate_limit)?);
    }

    let mut options = ranking_options_for_repo(repo)?;
    options.query = Some(query.into());
    Ok(top_unique_paths_merging(
        rerank_with_options(raw, &options),
        limit,
    ))
}

fn search_with_ranking_mode(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
    mode: RankingMode,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let candidate_limit = ranking_candidate_limit(limit);
    let mut raw = search_raw(repo, store, query, candidate_limit)?;
    annotate_candidates_with_git_history(store, &mut raw)?;
    let mut options = ranking_options_for_repo(repo)?;
    options.mode = mode;
    options.query = Some(query.into());
    Ok(top_unique_paths(rerank_with_options(raw, &options), limit))
}

fn annotate_candidates_with_git_history(
    store: &dyn MetadataStore,
    results: &mut Vec<open_kioku_core::SearchResult>,
) -> anyhow::Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let facts = store.analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?;
    if facts.is_empty() {
        return Ok(());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_path = files
        .into_iter()
        .map(|file| (normalize_path_fragment(&file.path.to_string_lossy()), file))
        .collect::<std::collections::HashMap<_, _>>();
    let mut existing_paths = results
        .iter()
        .map(|result| normalize_path_fragment(&result.path.to_string_lossy()))
        .collect::<std::collections::HashSet<_>>();
    let mut additions = Vec::new();
    for result in &mut *results {
        let Some(file) =
            files_by_path.get(&normalize_path_fragment(&result.path.to_string_lossy()))
        else {
            continue;
        };
        let matched = facts
            .iter()
            .filter(|fact| fact.file_id == file.id)
            .take(32)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            continue;
        }
        let displayed = matched.iter().copied().take(3).collect::<Vec<_>>();
        let evidence_ids = displayed
            .iter()
            .map(|fact| fact.id.clone())
            .collect::<Vec<_>>();
        let labels = displayed
            .iter()
            .map(|fact| fact.target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        for fact in &displayed {
            let evidence = format!(
                "git co-change from local history: `{}` ({})",
                fact.target, fact.message
            );
            if !result.evidence.contains(&evidence) {
                result.evidence.push(evidence);
            }
        }
        for id in &evidence_ids {
            if !result.evidence_refs.contains(id) {
                result.evidence_refs.push(id.clone());
            }
        }
        result.score_breakdown.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            (0.12 * matched.len() as f32).min(0.18),
            evidence_ids,
            format!("bounded local git history says this result co-changed with: {labels}"),
        ));
        for fact in matched {
            let target_path = normalize_path_fragment(&fact.target);
            if !existing_paths.insert(target_path.clone()) {
                continue;
            }
            let Some(target_file) = files_by_path.get(&target_path) else {
                continue;
            };
            let snippet = store
                .chunks_for_file(&target_file.id)?
                .first()
                .map(|chunk| chunk.text.clone())
                .unwrap_or_else(|| target_file.path.display().to_string());
            additions.push(open_kioku_core::SearchResult {
                path: target_file.path.clone(),
                line_range: None,
                snippet,
                symbol: None,
                score: 0.18 + (fact.confidence.score() * 0.05).min(0.05),
                match_reason: "historical git co-change candidate".into(),
                evidence: vec![format!(
                    "git co-change from local history: `{}` ({})",
                    fact.target, fact.message
                )],
                evidence_refs: vec![fact.id.clone()],
                confidence: fact.confidence.score(),
                score_breakdown: vec![ScoreComponent::single(
                    "similar_change_overlap",
                    0.18,
                    vec![fact.id.clone()],
                    "candidate added from bounded historical similar-change evidence",
                )],
            });
        }
    }
    results.extend(additions);
    Ok(())
}

fn without_git_history_candidates(
    results: Vec<open_kioku_core::SearchResult>,
) -> Vec<open_kioku_core::SearchResult> {
    results
        .into_iter()
        .filter(|result| result.match_reason != "historical git co-change candidate")
        .collect()
}

fn top_unique_paths(
    results: Vec<open_kioku_core::SearchResult>,
    limit: usize,
) -> Vec<open_kioku_core::SearchResult> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if !seen.insert(path) {
            continue;
        }
        unique.push(result);
        if unique.len() == limit {
            break;
        }
    }
    unique
}

fn top_unique_paths_merging(
    results: Vec<open_kioku_core::SearchResult>,
    limit: usize,
) -> Vec<open_kioku_core::SearchResult> {
    let mut indexes = std::collections::HashMap::<String, usize>::new();
    let mut unique = Vec::<open_kioku_core::SearchResult>::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if let Some(index) = indexes.get(&path).copied() {
            if !has_semantic_signal(&result) {
                continue;
            }
            let existing = &mut unique[index];
            for evidence in result.evidence {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            for evidence_ref in result.evidence_refs {
                if !existing.evidence_refs.contains(&evidence_ref) {
                    existing.evidence_refs.push(evidence_ref);
                }
            }
            for component in result.score_breakdown {
                if !existing
                    .score_breakdown
                    .iter()
                    .any(|existing| existing.signal == component.signal)
                {
                    existing.score_breakdown.push(component);
                }
            }
            existing.reconcile_score_breakdown();
            continue;
        }
        if unique.len() == limit {
            continue;
        }
        indexes.insert(path, unique.len());
        unique.push(result);
    }
    unique
}

fn has_semantic_signal(result: &open_kioku_core::SearchResult) -> bool {
    result
        .score_breakdown
        .iter()
        .any(|component| component.signal == "semantic_similarity")
}

fn search_raw(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let index_dir = default_index_dir(repo);
    if TantivySearchIndex::exists(&index_dir) {
        return Ok(TantivySearchIndex::open_or_create(index_dir)?.search(query, limit)?);
    }
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    Ok(search_chunks(&chunks, &files, &symbols, query, limit)?)
}

fn ranking_candidate_limit(limit: usize) -> usize {
    limit.clamp(1, 100).saturating_mul(4).clamp(100, 200)
}

fn ranking_options_for_repo(repo: &Path) -> anyhow::Result<RankingOptions> {
    let config = OkConfig::load_from_repo(repo)?;
    Ok(RankingOptions {
        weights: ranking_weights_from_config(&config.ranking),
        mode: RankingMode::Fusion,
        query: None,
    })
}

fn ranking_weights_from_config(config: &RankingConfig) -> RankingWeights {
    RankingWeights {
        text_relevance: config.text_relevance,
        exact_reference: config.exact_reference,
        graph_proximity: config.graph_proximity,
        boundary_fit: config.boundary_fit,
        runtime_corroboration: config.runtime_corroboration,
        git_cochange: config.git_cochange,
        validation_proximity: config.validation_proximity,
        memory_signal: config.memory_signal,
        path_quality: config.path_quality,
        semantic_similarity: config.semantic_similarity,
    }
}

fn print_semantic_status(status: &open_kioku_semantic::SemanticStatus) {
    println!("# Open Kioku Semantic Status");
    println!("state: {}", status.state);
    println!("backend: {}", status.backend);
    println!("provider: {}", status.provider);
    println!("model: {}", status.model);
    println!("dimensions: {}", status.dimensions);
    println!("vectors: {}", status.vector_count);
    println!("indexed: {}", status.indexed_count);
    println!("stale: {}", status.stale_count);
    println!("failed: {}", status.failed_count);
    println!("disk_bytes: {}", status.disk_usage_bytes);
    if !status.notes.is_empty() {
        println!("notes:");
        for note in &status.notes {
            println!("- {note}");
        }
    }
}

fn resolve_provenance_symbol(store: &dyn MetadataStore, query: &str) -> anyhow::Result<Symbol> {
    if let Some(symbol) = store.symbol_by_id(&SymbolId::new(query))? {
        return Ok(symbol);
    }
    let candidates = store.list_symbols(Some(query), 101, 0)?;
    let exact = candidates
        .iter()
        .filter(|symbol| symbol.name == query || symbol.qualified_name == query)
        .cloned()
        .collect::<Vec<_>>();
    match exact.as_slice() {
        [symbol] => Ok(symbol.clone()),
        [] if candidates.len() == 1 => Ok(candidates[0].clone()),
        [] if candidates.is_empty() => Err(anyhow::anyhow!("symbol not found: {query}")),
        matches => {
            let ambiguous = if matches.is_empty() {
                &candidates
            } else {
                matches
            };
            let names = ambiguous
                .iter()
                .take(10)
                .map(|symbol| format!("{} [{}]", symbol.qualified_name, symbol.id.0))
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow::anyhow!(
                "symbol query `{query}` is ambiguous; use a qualified name or symbol ID: {names}"
            ))
        }
    }
}

fn print_file_provenance(provenance: &FileProvenance) {
    println!("File provenance: {}", provenance.path.display());
    print_provenance_summary(
        provenance.first_seen.as_ref(),
        provenance.last_touched.as_ref(),
        &provenance.recent_touches,
        provenance.confidence,
        provenance.truncated,
        &provenance.uncertainty,
    );
}

fn print_symbol_provenance(provenance: &SymbolProvenance) {
    println!("Symbol provenance: {}", provenance.qualified_name);
    println!("File: {}", provenance.file_path.display());
    if let Some(range) = &provenance.range {
        println!("Current range: {}-{}", range.start, range.end);
    } else {
        println!("Current range: unavailable");
    }
    print_provenance_summary(
        provenance.first_seen.as_ref(),
        provenance.last_touched.as_ref(),
        &provenance.recent_touches,
        provenance.confidence,
        provenance.truncated,
        &provenance.uncertainty,
    );
}

fn print_similar_change_report(report: &SimilarChangeReport) {
    println!("Similar historical changes");
    println!("Generated at: {}", report.generated_at);
    if let Some(task) = &report.query.task {
        println!("Task: {task}");
    }
    if !report.query.paths.is_empty() {
        println!(
            "Paths: {}",
            report
                .query
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !report.query.symbols.is_empty() {
        println!("Symbols: {}", report.query.symbols.join(", "));
    }
    if report.hits.is_empty() {
        println!("Hits: none");
    } else {
        println!("Hits:");
        for hit in &report.hits {
            println!(
                "- {} score={:.3} confidence={:?} {}",
                hit.change.commit.id, hit.score, hit.confidence, hit.change.commit.summary
            );
            if !hit.change.touched_paths.is_empty() {
                println!(
                    "  paths: {}",
                    hit.change
                        .touched_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !hit.change.touched_symbols.is_empty() {
                println!("  symbols: {}", hit.change.touched_symbols.join(", "));
            }
            for evidence in &hit.evidence {
                println!(
                    "  - {:?} +{:.3}: {}",
                    evidence.source_type, evidence.score, evidence.message
                );
            }
            for note in &hit.uncertainty {
                println!("  ! {note}");
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_churn_summary(summary: &ChurnSummary) {
    println!("Churn target: {:?} {}", summary.entity_kind, summary.key);
    if let Some(path) = &summary.path {
        println!("Path: {}", path.display());
    }
    if let Some(name) = &summary.qualified_name {
        println!("Symbol: {name}");
    }
    if let Some(symbol_id) = &summary.symbol_id {
        println!("Symbol ID: {symbol_id}");
    }
    println!("Generated at: {}", summary.generated_at);
    println!("Confidence: {:?}", summary.confidence);
    println!("Touches: {}", summary.stats.touch_count);
    println!("All time: {}", summary.stats.all_time);
    println!("Last 30d: {}", summary.stats.last_30d);
    println!("Last 90d: {}", summary.stats.last_90d);
    println!("Recency weighted: {:.3}", summary.stats.recency_weighted);
    println!("Hotspot score: {:.3}", summary.stats.hotspot_score);
    if !summary.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &summary.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_ownership_report(report: &OwnershipReport) {
    println!("Ownership target: {}", report.path.display());
    if !report.components.is_empty() {
        println!("Components:");
        for component in &report.components {
            println!(
                "- {} via {}",
                component.component_id, component.matched_glob
            );
        }
    }
    println!("Generated at: {}", report.generated_at);
    if report.owners.is_empty() {
        println!("Owners: none");
    } else {
        println!("Owners:");
        for suggestion in &report.owners {
            let email = suggestion
                .owner
                .email
                .as_deref()
                .map(|email| format!(" <{email}>"))
                .unwrap_or_default();
            let sources = suggestion
                .source_types
                .iter()
                .map(|source| format!("{source:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "- {}{} confidence={:?} score={:.3} stale={} sources=[{}]",
                suggestion.owner.name,
                email,
                suggestion.confidence,
                suggestion.score,
                suggestion.stale,
                sources
            );
            println!("  rationale: {}", suggestion.rationale);
            for evidence in &suggestion.evidence {
                println!(
                    "  - {:?} {} confidence={:?} stale={}",
                    evidence.source_type, evidence.source, evidence.confidence, evidence.stale
                );
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_reviewer_suggestion_report(report: &ReviewerSuggestionReport) {
    println!("Reviewer target: {}", report.path.display());
    println!("Generated at: {}", report.generated_at);
    println!("Availability: {:?}", report.availability);
    if report.suggestions.is_empty() {
        println!("Reviewer suggestions: none");
    } else {
        println!("Reviewer suggestions:");
        for suggestion in &report.suggestions {
            let email = suggestion
                .reviewer
                .email
                .as_deref()
                .map(|email| format!(" <{email}>"))
                .unwrap_or_default();
            let sources = suggestion
                .source_types
                .iter()
                .map(|source| format!("{source:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "- {}{} confidence={:?} score={:.3} availability={:?} actual_review={} inferred_from_authors={} stale={} sources=[{}]",
                suggestion.reviewer.name,
                email,
                suggestion.confidence,
                suggestion.score,
                suggestion.availability,
                suggestion.actual_review_evidence,
                suggestion.inferred_from_authors,
                suggestion.stale,
                sources
            );
            println!("  rationale: {}", suggestion.rationale);
            for signal in &suggestion.signals {
                println!(
                    "  - {:?} {} confidence={:?} actual_review={} stale={}",
                    signal.source_type,
                    signal.source,
                    signal.confidence,
                    signal.actual_review_evidence,
                    signal.stale
                );
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_provenance_summary(
    first_seen: Option<&ProvenanceTouch>,
    last_touched: Option<&ProvenanceTouch>,
    recent_touches: &[ProvenanceTouch],
    confidence: Confidence,
    truncated: bool,
    uncertainty: &[String],
) {
    println!("Confidence: {confidence:?}");
    match first_seen {
        Some(touch) => println!("First seen: {}", format_provenance_touch(touch)),
        None => println!("First seen: unavailable"),
    }
    match last_touched {
        Some(touch) => println!("Last touched: {}", format_provenance_touch(touch)),
        None => println!("Last touched: unavailable"),
    }
    println!("Recent touches:");
    for touch in recent_touches {
        println!("- {}", format_provenance_touch(touch));
    }
    if recent_touches.is_empty() {
        println!("- none");
    }
    if truncated {
        println!("Results are truncated.");
    }
    if !uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in uncertainty {
            println!("- {note}");
        }
    }
}

fn format_provenance_touch(touch: &ProvenanceTouch) -> String {
    let ranges = if touch.line_ranges.is_empty() {
        String::new()
    } else {
        format!(
            " lines {}",
            touch
                .line_ranges
                .iter()
                .map(|range| format!("{}-{}", range.start, range.end))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    format!(
        "{} {} {} <{}> {:?}{} - {}",
        touch.commit.id,
        touch.commit.authored_at,
        touch.commit.author.name,
        touch.commit.author.email.as_deref().unwrap_or("unknown"),
        touch.change_kind,
        ranges,
        touch.commit.summary
    )
}

fn resolve_repo(global: &Path, command: PathBuf) -> PathBuf {
    if command == Path::new(".") {
        global.to_path_buf()
    } else {
        command
    }
}

fn normalize_to_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    let absolute_repo = std::fs::canonicalize(repo_root)
        .or_else(|_| absolutize(repo_root))
        .unwrap_or_else(|_| repo_root.to_path_buf());

    let absolute_path_canonical = std::fs::canonicalize(&absolute_path)
        .or_else(|_| absolutize(&absolute_path))
        .unwrap_or(absolute_path);

    if let Ok(rel) = absolute_path_canonical.strip_prefix(&absolute_repo) {
        rel.to_path_buf()
    } else if let Ok(rel) = absolute_path_canonical.strip_prefix(repo_root) {
        rel.to_path_buf()
    } else {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            let mut components = path.components();
            if let Some(std::path::Component::CurDir) = components.next() {
                components.as_path().to_path_buf()
            } else {
                path.to_path_buf()
            }
        }
    }
}

fn resolve_graph_node(store: &dyn MetadataStore, query: &str) -> anyhow::Result<String> {
    if query.starts_with("file:") || query.starts_with("symbol:") {
        return Ok(query.to_string());
    }
    if let Some(file) = store.get_file_by_path(Path::new(query))? {
        return Ok(format!("file:{}", file.path.display()));
    }
    if let Some(symbol) = store
        .list_symbols(Some(query), 10, 0)?
        .into_iter()
        .find(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
    {
        return Ok(format!("symbol:{}", symbol.id.0));
    }
    Ok(query.to_string())
}

fn output<T: serde::Serialize>(json: bool, value: &T, human: impl FnOnce()) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        let text = serde_json::to_string_pretty(value)?;
        if text.len() < 4096 {
            println!("{text}");
        } else {
            human();
        }
    }
    Ok(())
}

fn print_text_or_json(json: bool, text: &str, value: &serde_json::Value) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{text}");
    }
    Ok(())
}
