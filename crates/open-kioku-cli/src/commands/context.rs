fn normalize_path_fragment(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

struct SemanticContextCandidateSource<'a> {
    manager: SemanticIndexManager<'a>,
}

impl<'a> open_kioku_context::candidates::ContextCandidateSource
    for SemanticContextCandidateSource<'a>
{
    fn source(&self) -> open_kioku_core::RetrievalSourceKind {
        open_kioku_core::RetrievalSourceKind::SemanticVector
    }

    fn retrieve(
        &self,
        request: &open_kioku_context::candidates::CandidateRequest,
    ) -> open_kioku_errors::Result<open_kioku_context::candidates::CandidateStream> {
        let report = self.manager.search_with_path_prefixes(
            &request.task,
            request.limit,
            &request.scope.path_prefixes,
        )?;
        let rationale = format!(
            "local semantic-vector similarity; backend={} eligible={}/{} selectivity={} reason={}",
            report.routing.selected_backend,
            report.routing.eligible_candidate_count,
            report.routing.total_vector_count,
            report.routing.filter_selectivity,
            report.routing.routing_reason,
        );
        let mut stream = open_kioku_context::candidates::CandidateStream::success(
            open_kioku_core::RetrievalSourceKind::SemanticVector,
            report
                .results
                .into_iter()
                .map(|result| {
                    open_kioku_context::candidates::StreamCandidate::from_result(
                        result,
                        open_kioku_core::RetrievalAuthority::Heuristic,
                        rationale.clone(),
                    )
                })
                .collect(),
        );
        stream.caveats.extend(report.routing.caveats);
        Ok(stream)
    }
}

fn build_context_pack(
    repo: &Path,
    store: &SqliteStore,
    task: &str,
    limit: usize,
) -> anyhow::Result<open_kioku_core::ContextPack> {
    let search_dir = default_index_dir(repo);
    let config = OkConfig::load_from_repo(repo)?;
    let mut ranking_options = ranking_options_for_repo(repo)?;
    ranking_options.query = Some(task.into());
    let builder = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_ranking_options(ranking_options)
        .with_abstention_policy(
            open_kioku_core::abstention::AbstentionActivation::load_for_repo(repo)
                .map(|activation| activation.policy),
        );

    let mut lexical_index_source = None;
    let mut lexical_failure_source = None;
    if TantivySearchIndex::exists(&search_dir) {
        match TantivySearchIndex::open_or_create(&search_dir) {
            Ok(index) => {
                lexical_index_source = Some(
                    open_kioku_context::candidates::SearchIndexCandidateSource::new(index),
                );
            }
            Err(err) => {
                lexical_failure_source = Some(
                    open_kioku_context::candidates::UnavailableCandidateSource::new(
                        open_kioku_core::RetrievalSourceKind::Lexical,
                        format!("Tantivy lexical index unavailable; using regex fallback: {err}"),
                    ),
                );
            }
        }
    }

    let semantic_source = config.semantic.enabled.then(|| SemanticContextCandidateSource {
        manager: SemanticIndexManager::new(repo, store as &dyn MetadataStore, &config.semantic),
    });

    let mut sources = Vec::<&dyn open_kioku_context::candidates::ContextCandidateSource>::new();
    if let Some(source) = lexical_index_source.as_ref() {
        sources.push(source);
    } else if let Some(source) = lexical_failure_source.as_ref() {
        sources.push(source);
    }
    if let Some(source) = semantic_source.as_ref() {
        sources.push(source);
    }

    let mut pack = builder.build_with_sources(task, limit, &sources)?;
    pack.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(pack)
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
