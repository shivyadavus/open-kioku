fn normalize_path_fragment(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

fn build_context_pack(
    repo: &Path,
    store: &SqliteStore,
    task: &str,
    limit: usize,
) -> anyhow::Result<open_kioku_core::ContextPack> {
    let search_dir = default_index_dir(repo);
    let mut ranking_options = ranking_options_for_repo(repo)?;
    ranking_options.query = Some(task.into());
    let builder = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_ranking_options(ranking_options);
    let mut pack = if TantivySearchIndex::exists(&search_dir) {
        let index = TantivySearchIndex::open_or_create(&search_dir)?;
        let primary = search_context_candidates(&index, task, ranking_candidate_limit(limit))?;
        builder.build_from_primary(task, limit, primary)?
    } else {
        builder.build(task, limit)?
    };
    pack.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(pack)
}

fn search_context_candidates(
    index: &TantivySearchIndex,
    task: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut merged = std::collections::BTreeMap::<String, SearchResult>::new();
    for term in expanded_task_search_terms(task) {
        let mut results = index.search(&term, limit)?;
        for result in &mut results {
            if term != task {
                let evidence = format!("expanded task query `{term}` matched");
                if !result.evidence.contains(&evidence) {
                    result.evidence.push(evidence);
                }
                if !result.match_reason.contains(&term) {
                    result.match_reason =
                        format!("{}; expanded task query `{term}`", result.match_reason);
                }
            }
        }
        for result in results {
            merge_context_candidate(&mut merged, result);
        }
    }
    Ok(merged.into_values().collect())
}

fn merge_context_candidate(
    merged: &mut std::collections::BTreeMap<String, SearchResult>,
    result: SearchResult,
) {
    let key = search_result_key(&result);
    match merged.get_mut(&key) {
        Some(existing) => {
            if result.score > existing.score {
                existing.score = result.score;
                existing.snippet = result.snippet.clone();
                existing.line_range = result.line_range.clone();
                existing.symbol = result.symbol.clone();
            }
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
            if !existing.match_reason.contains(&result.match_reason) {
                existing.match_reason =
                    format!("{}; {}", existing.match_reason, result.match_reason);
            }
            for component in result.score_breakdown {
                existing.score_breakdown.push(component);
            }
            existing.reconcile_score_breakdown();
        }
        None => {
            merged.insert(key, result);
        }
    }
}

fn search_result_key(result: &SearchResult) -> String {
    format!(
        "{}:{}-{}",
        normalize_path_fragment(&result.path.to_string_lossy()),
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

fn configured_architecture_policy_report<S>(
    repo: &Path,
    store: &S,
) -> anyhow::Result<Option<PolicyCheckReport>>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    let Some(policy) = load_architecture_policy(repo)? else {
        return Ok(None);
    };
    let resolver = PolicyResolver::new(&policy)?;
    Ok(Some(evaluate_policy(store, &resolver, &policy)?))
}

fn ownership_components<S>(
    repo: &Path,
    store: &S,
    path: &Path,
) -> anyhow::Result<Vec<PolicyComponentMatch>>
where
    S: MetadataStore,
{
    if let Some(policy) = load_architecture_policy(repo)? {
        let resolver = PolicyResolver::new(&policy)?;
        return Ok(resolver.resolve_file(path));
    }

    let path_text = path.display().to_string();
    let summary = ArchitectureDetector::new(store, None).detect()?;
    Ok(summary
        .components
        .into_iter()
        .filter(|component| {
            component
                .paths
                .iter()
                .any(|candidate| candidate == &path_text)
        })
        .map(|component| PolicyComponentMatch {
            component_id: component.id,
            matched_glob: "inferred_component_path".into(),
        })
        .collect())
}

fn ownership_memory_facts(
    repo: &Path,
    path: &Path,
    components: &[PolicyComponentMatch],
) -> anyhow::Result<Vec<open_kioku_core::MemorySearchResult>> {
    let mut query_terms = vec![
        "ownership".to_string(),
        "owner".to_string(),
        "owners".to_string(),
        "maintainer".to_string(),
        path.display().to_string(),
    ];
    query_terms.extend(
        components
            .iter()
            .map(|component| component.component_id.clone()),
    );
    Ok(RepoMemoryStore::open_repo(repo)?.search(&query_terms.join(" "), 20)?)
}
