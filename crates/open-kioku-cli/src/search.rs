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

/// The repository's index for a read command. A repository that has never been indexed is
/// refused with the same sentence `ok status`, `ok doctor` and the MCP server use, and
/// nothing is created on disk: an empty database written here used to make every later
/// read report a legacy index awaiting rebuild instead.
fn open_store(repo: impl AsRef<Path>) -> anyhow::Result<SqliteStore> {
    let repo = repo.as_ref();
    SqliteStore::open_repo_index(repo)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            open_kioku_storage::generations::not_indexed_message(repo)
        )
    })
}

/// The index for a command that writes it (`ok index`, `ok snapshot import`): created when
/// absent, which is exactly what a read must never do.
fn open_store_for_write(repo: impl AsRef<Path>) -> anyhow::Result<SqliteStore> {
    let location = open_kioku_storage::generations::resolve_index_location(repo.as_ref());
    Ok(SqliteStore::open(location.sqlite_path())?)
}

fn search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    ranked_search_results(repo.as_ref(), store, query, SearchMode::Code, limit)
}

/// `ok search --regex` answers with the caveats attached, not alongside them.
///
/// The other search modes print a bare `Vec<SearchResult>`, but under `--json`
/// stdout is the whole answer: a note on stderr is not part of it, and an empty
/// array would read as "this pattern is absent from the repository" rather than
/// "absent from the part of it that is indexed". These are the same fields the
/// MCP `regex_search` response carries, so both surfaces disclose the same thing.
#[derive(serde::Serialize)]
struct RegexSearchReport {
    results: Vec<open_kioku_core::SearchResult>,
    truncated: bool,
    warnings: Vec<String>,
    caveats: Vec<String>,
}

/// Exact regex matching over the indexed corpus, the same call the MCP
/// `regex_search` tool makes, so the two surfaces answer identically.
fn regex_search(
    store: &dyn MetadataStore,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<RegexSearchReport> {
    let scan = regex_search_index(store, pattern, limit)?;
    let mut report = RegexSearchReport {
        results: scan.results,
        truncated: scan.files_capped,
        warnings: Vec::new(),
        caveats: [
            Some(format!(
                "the pattern was evaluated over indexed chunk text from {} file(s), not the working tree; regions the indexer did not chunk were not searched",
                scan.files_scanned
            )),
            redaction_caveat(store),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };
    if scan.files_capped {
        report.warnings.push(format!(
            "the regex scan stopped after {MAX_REGEX_SCAN_FILES} files; results are incomplete"
        ));
    }
    Ok(report)
}

/// `ok search` answers with its caveats attached, not alongside them, in every ranked mode.
/// `search_code` reports a filled candidate window as `truncated` with a warning; a bare array
/// under `--json` could not carry that, and the two surfaces would then disagree about how
/// complete the same answer is.
#[derive(serde::Serialize)]
struct RankedSearchReport {
    results: Vec<open_kioku_core::SearchResult>,
    truncated: bool,
    warnings: Vec<String>,
    caveats: Vec<String>,
}

/// One page of the shared ranked search at offset 0, with the report `search_code` returns for
/// the same state (#448). The `[ranking]` weights come from `ok.toml` on every call; the MCP
/// server reads them once, when it starts.
fn ranked_search_report(
    repo: &Path,
    store: &dyn MetadataStore,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> anyhow::Result<RankedSearchReport> {
    let page = ranked_search_page(repo, store, query, mode, limit)?;
    let warnings = page.truncation_warning().into_iter().collect::<Vec<_>>();
    Ok(RankedSearchReport {
        results: page.results,
        truncated: !warnings.is_empty(),
        warnings,
        caveats: redaction_caveat(store).into_iter().collect(),
    })
}

/// The redaction caveat the MCP search tools attach, from the same function in
/// `open_kioku_core`, so the human surface is no less honest than the agent surface (#379).
fn redaction_caveat(store: &dyn MetadataStore) -> Option<String> {
    let manifest = store.manifest().ok().flatten()?;
    open_kioku_core::redaction_search_caveat(manifest.quality.redacted_files)
}

fn ranked_search_results(
    repo: &Path,
    store: &dyn MetadataStore,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    Ok(ranked_search_page(repo, store, query, mode, limit)?.results)
}

fn ranked_search_page(
    repo: &Path,
    store: &dyn MetadataStore,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> anyhow::Result<open_kioku_context::search::RankedSearchPage> {
    let config = OkConfig::load_from_repo(repo)?;
    let request = RankedSearchRequest {
        query,
        mode,
        // `--limit 0` has always printed every unique path in the candidate pool.
        limit: (limit > 0).then_some(limit),
        offset: 0,
    };
    let page = if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
        let mut semantic_config = config.semantic.clone();
        semantic_config.enabled = true;
        let manager = SemanticIndexManager::new(repo, store, &semantic_config);
        let search = |query: &str, depth: usize| manager.search(query, depth);
        let semantic = SemanticCandidates {
            ready: manager.status().ready,
            search: &search,
        };
        ranked_search(repo, store, &request, Some(&semantic), &config.ranking)?
    } else {
        ranked_search(repo, store, &request, None, &config.ranking)?
    };
    Ok(page)
}

/// Lexical candidates before ranking, for the benchmarks that rank one pool several ways.
fn search_raw(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    Ok(lexical_candidates(repo.as_ref(), store, query, limit)?)
}

fn ranking_options_for_repo(repo: &Path) -> anyhow::Result<RankingOptions> {
    Ok(open_kioku_context::search::ranking_options(
        &OkConfig::load_from_repo(repo)?.ranking,
    ))
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
