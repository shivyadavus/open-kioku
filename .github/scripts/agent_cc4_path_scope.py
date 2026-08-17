from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: {path}: expected exactly one marker, found {count}")
    p.write_text(text.replace(old, new, 1))


routing = "crates/open-kioku-context/src/routing.rs"
replace_once(
    routing,
    "    pub query_shape_fallback_reason: Option<String>,\n    pub policy: RetrievalPolicy,\n",
    "    pub query_shape_fallback_reason: Option<String>,\n    /// Explicit repository-relative path filters extracted from the query. Mere path mentions\n    /// remain retrieval anchors unless the query is a path lookup or uses an explicit scope verb.\n    pub path_scope: Vec<String>,\n    pub policy: RetrievalPolicy,\n",
    "routing-decision-field",
)
replace_once(
    routing,
    """    let query = classify_query_shape(task);
    TaskRoutingDecision {
        family,
        confidence,
        reasons,
        query_shape: query.shape,
        query_shape_confidence: query.confidence,
        query_shape_signals: query.signals,
        query_shape_ambiguities: query.ambiguities,
        query_shape_fallback_reason: query.fallback_reason,
        policy: policy_for(family, query.shape),
    }
""",
    """    let query = classify_query_shape(task);
    let path_scope = extract_path_scope(task, query.shape);
    let mut query_shape_signals = query.signals;
    query_shape_signals.extend(
        path_scope
            .iter()
            .map(|path| format!("explicit repository path scope `{path}`")),
    );
    TaskRoutingDecision {
        family,
        confidence,
        reasons,
        query_shape: query.shape,
        query_shape_confidence: query.confidence,
        query_shape_signals,
        query_shape_ambiguities: query.ambiguities,
        query_shape_fallback_reason: query.fallback_reason,
        path_scope,
        policy: policy_for(family, query.shape),
    }
""",
    "routing-decision-construction",
)
replace_once(
    routing,
    """fn contains_path_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_source_path_token)
}

fn is_source_path_token(token: &str) -> bool {
""",
    """fn contains_path_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_source_path_token)
}

fn extract_path_scope(query: &str, shape: QueryShape) -> Vec<String> {
    if matches!(shape, QueryShape::ErrorTrace | QueryShape::ApiResource) {
        return Vec::new();
    }

    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let single_path_lookup = matches!(shape, QueryShape::PathReference) && tokens.len() == 1;
    let mut scope = Vec::new();
    for (index, raw) in tokens.iter().enumerate() {
        let Some(path) = normalize_scope_path(raw) else {
            continue;
        };
        let previous = index
            .checked_sub(1)
            .and_then(|position| tokens.get(position))
            .map(|token| trim_query_token(token).to_ascii_lowercase());
        if single_path_lookup
            || previous
                .as_deref()
                .is_some_and(is_explicit_path_scope_marker)
        {
            if !scope.contains(&path) {
                scope.push(path);
            }
        }
    }
    scope.sort();
    scope
}

fn is_explicit_path_scope_marker(token: &str) -> bool {
    matches!(
        token,
        "in"
            | "within"
            | "under"
            | "file"
            | "path"
            | "directory"
            | "dir"
            | "open"
            | "find"
            | "show"
            | "inspect"
            | "edit"
            | "change"
            | "update"
            | "modify"
    )
}

fn normalize_scope_path(raw: &str) -> Option<String> {
    let mut token = trim_query_token(raw).to_string();
    loop {
        let Some((prefix, suffix)) = token.rsplit_once(':') else {
            break;
        };
        if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty() {
            token = prefix.to_string();
        } else {
            break;
        }
    }
    if token.contains("://") {
        return None;
    }
    token = token.replace('\\', "/");
    while let Some(stripped) = token.strip_prefix("./") {
        token = stripped.to_string();
    }
    token = token.trim_end_matches('/').to_string();
    if token.is_empty()
        || token.starts_with('/')
        || token.split('/').any(|part| part == "..")
        || !is_source_path_token(&token)
    {
        return None;
    }
    Some(token)
}

fn is_source_path_token(token: &str) -> bool {
""",
    "routing-scope-extraction",
)
replace_once(
    routing,
    """    #[test]
    fn diagnostics_preserve_query_shape_reasoning_for_json_and_mcp_consumers() {
        let diagnostics = classify_task("fix panic in src/index.rs").diagnostics();
        assert_eq!(
            diagnostics.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert!(diagnostics.query_shape_confidence > 0.0);
        assert!(!diagnostics.query_shape_signals.is_empty());
    }
""",
    """    #[test]
    fn diagnostics_preserve_query_shape_reasoning_for_json_and_mcp_consumers() {
        let diagnostics = classify_task("fix panic in src/index.rs").diagnostics();
        assert_eq!(
            diagnostics.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert!(diagnostics.query_shape_confidence > 0.0);
        assert!(!diagnostics.query_shape_signals.is_empty());
    }

    #[test]
    fn explicit_repository_path_scope_is_normalized_and_deterministic() {
        let route = classify_task(
            "update ./crates/open-kioku-context/src/lib.rs and inspect crates\\open-kioku-context\\src",
        );
        assert_eq!(
            route.path_scope,
            vec![
                "crates/open-kioku-context/src".to_string(),
                "crates/open-kioku-context/src/lib.rs".to_string(),
            ]
        );
        assert!(route
            .query_shape_signals
            .iter()
            .any(|signal| signal.contains("explicit repository path scope")));
    }

    #[test]
    fn path_mentions_are_not_silently_promoted_to_filters() {
        let reference = classify_task(
            "implement a new parser similar to crates/open-kioku-parse/src/lib.rs",
        );
        assert!(reference.path_scope.is_empty());

        let trace = classify_task(
            "panic stack trace at crates/open-kioku-context/src/lib.rs:412:9",
        );
        assert_eq!(trace.query_shape, QueryShape::ErrorTrace);
        assert!(trace.path_scope.is_empty());

        let url = classify_task("inspect https://example.com/src/lib.rs");
        assert!(url.path_scope.is_empty());
    }

    #[test]
    fn unsafe_parent_scope_is_rejected_instead_of_broadening() {
        let route = classify_task("inspect ../outside/src/lib.rs");
        assert!(route.path_scope.is_empty());
    }
""",
    "routing-adversarial-tests",
)

candidates = "crates/open-kioku-context/src/candidates.rs"
replace_once(
    candidates,
    """pub struct CandidateRequest {
    pub task: String,
    pub search_terms: Vec<String>,
    pub limit: usize,
}

impl CandidateRequest {
    pub fn new(task: impl Into<String>, search_terms: Vec<String>, limit: usize) -> Self {
        Self {
            task: task.into(),
            search_terms,
            limit: limit.clamp(1, 200),
        }
    }
}
""",
    """pub struct CandidateRequest {
    pub task: String,
    pub search_terms: Vec<String>,
    pub limit: usize,
    /// Explicit repository-relative path scope produced by task routing. An empty scope means
    /// unscoped retrieval; a non-empty scope must never be silently widened by a source.
    pub path_scope: Vec<String>,
}

impl CandidateRequest {
    pub fn new(task: impl Into<String>, search_terms: Vec<String>, limit: usize) -> Self {
        Self {
            task: task.into(),
            search_terms,
            limit: limit.clamp(1, 200),
            path_scope: Vec::new(),
        }
    }

    pub fn with_path_scope(mut self, mut path_scope: Vec<String>) -> Self {
        path_scope.sort();
        path_scope.dedup();
        self.path_scope = path_scope;
        self
    }
}
""",
    "candidate-request-scope",
)

context = "crates/open-kioku-context/src/lib.rs"
replace_once(
    context,
    """        let request =
            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit);
""",
    """        let request = candidates::CandidateRequest::new(
            task,
            intent.search_terms(task),
            candidate_limit,
        )
        .with_path_scope(routing.path_scope.clone());
""",
    "context-request-propagation",
)

cli = "crates/open-kioku-cli/src/commands/context.rs"
replace_once(
    cli,
    "        let results = self.manager.search(&request.task, request.limit)?;\n",
    """        let results = self.manager.search_with_path_scope(
            &request.task,
            request.limit,
            &request.path_scope,
        )?;
""",
    "cli-semantic-scope",
)

semantic = "crates/open-kioku-semantic/src/lib.rs"
replace_once(
    semantic,
    """    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_with_allowlist(query, limit, None)
    }

    pub fn search_with_allowlist(
""",
    """    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_with_allowlist(query, limit, None)
    }

    pub fn search_with_path_scope(
        &self,
        query: &str,
        limit: usize,
        path_scope: &[String],
    ) -> Result<Vec<SearchResult>> {
        if path_scope.is_empty() {
            return self.search(query, limit);
        }
        let targets = read_targets(&self.current_dir().join("ids.json"))?;
        let allowlist = path_scope_allowlist(&targets, path_scope);
        if allowlist.is_empty() {
            // Explicit scopes are filters, not hints. Never widen a typo or stale path into a
            // repository-wide semantic search.
            return Ok(Vec::new());
        }
        self.search_with_allowlist(query, limit, Some(allowlist))
    }

    pub fn search_with_allowlist(
""",
    "semantic-manager-scope",
)
replace_once(
    semantic,
    """    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.manager.search(query, limit)
    }
}
""",
    """    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.manager.search(query, limit)
    }

    pub fn search_with_path_scope(
        &self,
        query: &str,
        limit: usize,
        path_scope: &[String],
    ) -> Result<Vec<SearchResult>> {
        self.manager.search_with_path_scope(query, limit, path_scope)
    }
}
""",
    "semantic-engine-scope",
)
replace_once(
    semantic,
    """fn hydrate_hits(
    store: &dyn MetadataStore,
""",
    """fn path_scope_allowlist(
    targets: &HashMap<String, SemanticTarget>,
    path_scope: &[String],
) -> HashSet<VectorId> {
    targets
        .values()
        .filter(|target| path_matches_scope(&target.path, path_scope))
        .map(|target| target.vector_id)
        .collect()
}

fn path_matches_scope(path: &Path, path_scope: &[String]) -> bool {
    let candidate = path.to_string_lossy().replace('\\', "/");
    path_scope.iter().any(|scope| {
        let normalized = scope.replace('\\', "/").trim_end_matches('/').to_string();
        candidate == normalized
            || candidate
                .strip_prefix(&normalized)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn hydrate_hits(
    store: &dyn MetadataStore,
""",
    "semantic-scope-helper",
)
replace_once(
    semantic,
    """    #[test]
    fn disabled_config_returns_no_provider() {
""",
    """    #[test]
    fn path_scope_matches_exact_files_and_subtrees_without_prefix_bleed() {
        let scope = vec!["crates/open-kioku-context/src".to_string()];
        assert!(path_matches_scope(
            Path::new("crates/open-kioku-context/src/lib.rs"),
            &scope
        ));
        assert!(path_matches_scope(
            Path::new("crates/open-kioku-context/src"),
            &scope
        ));
        assert!(!path_matches_scope(
            Path::new("crates/open-kioku-context/src-old/lib.rs"),
            &scope
        ));
        assert!(!path_matches_scope(
            Path::new("crates/open-kioku-cli/src/lib.rs"),
            &scope
        ));
    }

    #[test]
    fn path_scope_union_is_deterministic_and_empty_match_stays_empty() {
        let target = |stable_id: &str, path: &str, vector_id: u64| SemanticTarget {
            stable_id: stable_id.into(),
            kind: "chunk".into(),
            file_id: stable_id.into(),
            path: PathBuf::from(path),
            line_range: None,
            symbol_id: None,
            text: String::new(),
            content_hash: String::new(),
            vector_id: VectorId(vector_id),
        };
        let targets = HashMap::from([
            ("a".to_string(), target("a", "crates/a/src/lib.rs", 1)),
            ("b".to_string(), target("b", "crates/b/src/lib.rs", 2)),
        ]);
        let allowlist = path_scope_allowlist(
            &targets,
            &["crates/b".to_string(), "crates/a/src/lib.rs".to_string()],
        );
        assert_eq!(allowlist, HashSet::from([VectorId(1), VectorId(2)]));
        assert!(path_scope_allowlist(&targets, &["crates/missing".to_string()]).is_empty());
    }

    #[test]
    fn disabled_config_returns_no_provider() {
""",
    "semantic-scope-tests",
)
