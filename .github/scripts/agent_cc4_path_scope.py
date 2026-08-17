from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one marker, found {count}")
    p.write_text(text.replace(old, new, 1))


# 1) Carry an explicit, normalized repository path scope in the routing decision.
replace_once(
    "crates/open-kioku-context/src/routing.rs",
    '''    pub query_shape_fallback_reason: Option<String>,\n    pub policy: RetrievalPolicy,\n''',
    '''    pub query_shape_fallback_reason: Option<String>,\n    /// Explicit repository-relative path filters extracted from the query. Mere path mentions\n    /// remain retrieval anchors unless the query is a path lookup or uses an explicit scope verb.\n    pub path_scope: Vec<String>,\n    pub policy: RetrievalPolicy,\n''',
)

replace_once(
    "crates/open-kioku-context/src/routing.rs",
    '''    let query = classify_query_shape(task);\n    TaskRoutingDecision {\n        family,\n        confidence,\n        reasons,\n        query_shape: query.shape,\n        query_shape_confidence: query.confidence,\n        query_shape_signals: query.signals,\n        query_shape_ambiguities: query.ambiguities,\n        query_shape_fallback_reason: query.fallback_reason,\n        policy: policy_for(family, query.shape),\n    }\n''',
    '''    let query = classify_query_shape(task);\n    let path_scope = extract_path_scope(task, query.shape);\n    let mut query_shape_signals = query.signals;\n    query_shape_signals.extend(\n        path_scope\n            .iter()\n            .map(|path| format!("explicit repository path scope `{path}`")),\n    );\n    TaskRoutingDecision {\n        family,\n        confidence,\n        reasons,\n        query_shape: query.shape,\n        query_shape_confidence: query.confidence,\n        query_shape_signals,\n        query_shape_ambiguities: query.ambiguities,\n        query_shape_fallback_reason: query.fallback_reason,\n        path_scope,\n        policy: policy_for(family, query.shape),\n    }\n''',
)

replace_once(
    "crates/open-kioku-context/src/routing.rs",
    '''fn contains_path_reference(query: &str) -> bool {\n    query\n        .split_whitespace()\n        .map(trim_query_token)\n        .any(is_source_path_token)\n}\n\nfn is_source_path_token(token: &str) -> bool {\n''',
    '''fn contains_path_reference(query: &str) -> bool {\n    query\n        .split_whitespace()\n        .map(trim_query_token)\n        .any(is_source_path_token)\n}\n\nfn extract_path_scope(query: &str, shape: QueryShape) -> Vec<String> {\n    if matches!(shape, QueryShape::ErrorTrace | QueryShape::ApiResource) {\n        return Vec::new();\n    }\n\n    let tokens = query.split_whitespace().collect::<Vec<_>>();\n    let single_path_lookup = matches!(shape, QueryShape::PathReference)\n        && tokens.len() == 1;\n    let mut scope = Vec::new();\n    for (index, raw) in tokens.iter().enumerate() {\n        let Some(path) = normalize_scope_path(raw) else {\n            continue;\n        };\n        let previous = index\n            .checked_sub(1)\n            .and_then(|position| tokens.get(position))\n            .map(|token| trim_query_token(token).to_ascii_lowercase());\n        if single_path_lookup\n            || previous\n                .as_deref()\n                .is_some_and(is_explicit_path_scope_marker)\n        {\n            if !scope.contains(&path) {\n                scope.push(path);\n            }\n        }\n    }\n    scope.sort();\n    scope\n}\n\nfn is_explicit_path_scope_marker(token: &str) -> bool {\n    matches!(\n        token,\n        "in"\n            | "within"\n            | "under"\n            | "file"\n            | "path"\n            | "directory"\n            | "dir"\n            | "open"\n            | "find"\n            | "show"\n            | "inspect"\n            | "edit"\n            | "change"\n            | "update"\n            | "modify"\n    )\n}\n\nfn normalize_scope_path(raw: &str) -> Option<String> {\n    let mut token = trim_query_token(raw).to_string();\n    // Stack/error locations such as `src/lib.rs:42:7` are paths, but traces are intentionally\n    // excluded above because an observed frame is evidence, not a user-requested scope. This\n    // normalization still makes explicit `open src/lib.rs:42` queries useful.\n    loop {\n        let Some((prefix, suffix)) = token.rsplit_once(':') else {\n            break;\n        };\n        if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty() {\n            token = prefix.to_string();\n        } else {\n            break;\n        }\n    }\n    if token.contains("://") {\n        return None;\n    }\n    token = token.replace('\\\\', "/");\n    while let Some(stripped) = token.strip_prefix("./") {\n        token = stripped.to_string();\n    }\n    token = token.trim_end_matches('/').to_string();\n    if token.is_empty()\n        || token.starts_with('/')\n        || token.split('/').any(|part| part == "..")\n        || !is_source_path_token(&token)\n    {\n        return None;\n    }\n    Some(token)\n}\n\nfn is_source_path_token(token: &str) -> bool {\n''',
)

# Add adversarial routing tests before the existing test module closes by anchoring a known test.
replace_once(
    "crates/open-kioku-context/src/routing.rs",
    '''    fn query_shape_does_not_enable_sources_forbidden_by_task_family() {\n        let route = classify_task("document crates/open-kioku-context/src/lib.rs");\n        assert_eq!(route.family, TaskFamily::Documentation);\n        assert!(!route.policy.allows(RetrievalSourceKind::SemanticVector));\n        assert!(!route.policy.allows(RetrievalSourceKind::Runtime));\n    }\n''',
    '''    fn query_shape_does_not_enable_sources_forbidden_by_task_family() {\n        let route = classify_task("document crates/open-kioku-context/src/lib.rs");\n        assert_eq!(route.family, TaskFamily::Documentation);\n        assert!(!route.policy.allows(RetrievalSourceKind::SemanticVector));\n        assert!(!route.policy.allows(RetrievalSourceKind::Runtime));\n    }\n\n    #[test]\n    fn explicit_repository_path_scope_is_normalized_and_deterministic() {\n        let route = classify_task(\n            "update ./crates/open-kioku-context/src/lib.rs and inspect crates\\\\open-kioku-context\\\\src",\n        );\n        assert_eq!(\n            route.path_scope,\n            vec![\n                "crates/open-kioku-context/src".to_string(),\n                "crates/open-kioku-context/src/lib.rs".to_string(),\n            ]\n        );\n        assert!(route\n            .query_shape_signals\n            .iter()\n            .any(|signal| signal.contains("explicit repository path scope")));\n    }\n\n    #[test]\n    fn path_mentions_are_not_silently_promoted_to_filters() {\n        let reference = classify_task(\n            "implement a new parser similar to crates/open-kioku-parse/src/lib.rs",\n        );\n        assert!(reference.path_scope.is_empty());\n\n        let trace = classify_task(\n            "panic stack trace at crates/open-kioku-context/src/lib.rs:412:9",\n        );\n        assert_eq!(trace.query_shape, QueryShape::ErrorTrace);\n        assert!(trace.path_scope.is_empty());\n\n        let url = classify_task("inspect https://example.com/src/lib.rs");\n        assert!(url.path_scope.is_empty());\n    }\n\n    #[test]\n    fn unsafe_parent_scope_is_rejected_instead_of_broadening() {\n        let route = classify_task("inspect ../outside/src/lib.rs");\n        assert!(route.path_scope.is_empty());\n    }\n''',
)

# 2) Put scope on the shared candidate request so external sources can honor it without reparsing.
replace_once(
    "crates/open-kioku-context/src/candidates.rs",
    '''pub struct CandidateRequest {\n    pub task: String,\n    pub search_terms: Vec<String>,\n    pub limit: usize,\n}\n\nimpl CandidateRequest {\n    pub fn new(task: impl Into<String>, search_terms: Vec<String>, limit: usize) -> Self {\n        Self {\n            task: task.into(),\n            search_terms,\n            limit: limit.clamp(1, 200),\n        }\n    }\n}\n''',
    '''pub struct CandidateRequest {\n    pub task: String,\n    pub search_terms: Vec<String>,\n    pub limit: usize,\n    /// Explicit repository-relative path scope produced by task routing. An empty scope means\n    /// unscoped retrieval; a non-empty scope must never be silently widened by a source.\n    pub path_scope: Vec<String>,\n}\n\nimpl CandidateRequest {\n    pub fn new(task: impl Into<String>, search_terms: Vec<String>, limit: usize) -> Self {\n        Self {\n            task: task.into(),\n            search_terms,\n            limit: limit.clamp(1, 200),\n            path_scope: Vec::new(),\n        }\n    }\n\n    pub fn with_path_scope(mut self, mut path_scope: Vec<String>) -> Self {\n        path_scope.sort();\n        path_scope.dedup();\n        self.path_scope = path_scope;\n        self\n    }\n}\n''',
)

# 3) Propagate the routing decision into every external source request.
replace_once(
    "crates/open-kioku-context/src/lib.rs",
    '''        let request =\n            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit);\n''',
    '''        let request = candidates::CandidateRequest::new(\n            task,\n            intent.search_terms(task),\n            candidate_limit,\n        )\n        .with_path_scope(routing.path_scope.clone());\n''',
)

# 4) Make the production CLI semantic source honor scope rather than reparsing task text.
replace_once(
    "crates/open-kioku-cli/src/commands/context.rs",
    '''        let results = self.manager.search(&request.task, request.limit)?;\n''',
    '''        let results = self.manager.search_with_path_scope(\n            &request.task,\n            request.limit,\n            &request.path_scope,\n        )?;\n''',
)

# 5) Implement fail-closed path filtering at the semantic index boundary.
replace_once(
    "crates/open-kioku-semantic/src/lib.rs",
    '''    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {\n        self.search_with_allowlist(query, limit, None)\n    }\n\n    pub fn search_with_allowlist(\n''',
    '''    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {\n        self.search_with_allowlist(query, limit, None)\n    }\n\n    pub fn search_with_path_scope(\n        &self,\n        query: &str,\n        limit: usize,\n        path_scope: &[String],\n    ) -> Result<Vec<SearchResult>> {\n        if path_scope.is_empty() {\n            return self.search(query, limit);\n        }\n        let targets = read_targets(&self.current_dir().join("ids.json"))?;\n        let allowlist = path_scope_allowlist(&targets, path_scope);\n        if allowlist.is_empty() {\n            // Explicit scopes are filters, not ranking hints. Failing closed here prevents a typo\n            // or stale path from silently turning into an unscoped repository-wide semantic query.\n            return Ok(Vec::new());\n        }\n        self.search_with_allowlist(query, limit, Some(allowlist))\n    }\n\n    pub fn search_with_allowlist(\n''',
)

replace_once(
    "crates/open-kioku-semantic/src/lib.rs",
    '''    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {\n        self.manager.search(query, limit)\n    }\n}\n''',
    '''    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {\n        self.manager.search(query, limit)\n    }\n\n    pub fn search_with_path_scope(\n        &self,\n        query: &str,\n        limit: usize,\n        path_scope: &[String],\n    ) -> Result<Vec<SearchResult>> {\n        self.manager.search_with_path_scope(query, limit, path_scope)\n    }\n}\n''',
)

# Place pure scope helpers immediately before target hydration.
replace_once(
    "crates/open-kioku-semantic/src/lib.rs",
    '''fn hydrate_hits(\n    store: &dyn MetadataStore,\n''',
    '''fn path_scope_allowlist(\n    targets: &HashMap<String, SemanticTarget>,\n    path_scope: &[String],\n) -> HashSet<VectorId> {\n    targets\n        .values()\n        .filter(|target| path_matches_scope(&target.path, path_scope))\n        .map(|target| target.vector_id)\n        .collect()\n}\n\nfn path_matches_scope(path: &Path, path_scope: &[String]) -> bool {\n    let candidate = path.to_string_lossy().replace('\\\\', "/");\n    path_scope.iter().any(|scope| {\n        let normalized = scope.replace('\\\\', "/").trim_end_matches('/').to_string();\n        candidate == normalized\n            || candidate\n                .strip_prefix(&normalized)\n                .is_some_and(|suffix| suffix.starts_with('/'))\n    })\n}\n\nfn hydrate_hits(\n    store: &dyn MetadataStore,\n''',
)

# Add pure adversarial path-filter tests next to existing semantic unit tests.
semantic = Path("crates/open-kioku-semantic/src/lib.rs")
text = semantic.read_text()
marker = '''    #[test]\n    fn disabled_config_returns_no_provider() {'''
insert = '''    #[test]\n    fn path_scope_matches_exact_files_and_subtrees_without_prefix_bleed() {\n        let scope = vec!["crates/open-kioku-context/src".to_string()];\n        assert!(path_matches_scope(\n            Path::new("crates/open-kioku-context/src/lib.rs"),\n            &scope\n        ));\n        assert!(path_matches_scope(\n            Path::new("crates/open-kioku-context/src"),\n            &scope\n        ));\n        assert!(!path_matches_scope(\n            Path::new("crates/open-kioku-context/src-old/lib.rs"),\n            &scope\n        ));\n        assert!(!path_matches_scope(\n            Path::new("crates/open-kioku-cli/src/lib.rs"),\n            &scope\n        ));\n    }\n\n    #[test]\n    fn path_scope_union_is_deterministic_and_empty_match_stays_empty() {\n        let target = |stable_id: &str, path: &str, vector_id: u64| SemanticTarget {\n            stable_id: stable_id.into(),\n            kind: "chunk".into(),\n            file_id: stable_id.into(),\n            path: PathBuf::from(path),\n            line_range: None,\n            symbol_id: None,\n            text: String::new(),\n            content_hash: String::new(),\n            vector_id: VectorId(vector_id),\n        };\n        let targets = HashMap::from([\n            (\n                "a".to_string(),\n                target("a", "crates/a/src/lib.rs", 1),\n            ),\n            (\n                "b".to_string(),\n                target("b", "crates/b/src/lib.rs", 2),\n            ),\n        ]);\n        let allowlist = path_scope_allowlist(\n            &targets,\n            &["crates/b".to_string(), "crates/a/src/lib.rs".to_string()],\n        );\n        assert_eq!(allowlist, HashSet::from([VectorId(1), VectorId(2)]));\n        assert!(path_scope_allowlist(&targets, &["crates/missing".to_string()]).is_empty());\n    }\n\n    #[test]\n    fn disabled_config_returns_no_provider() {'''
count = text.count(marker)
if count != 1:
    raise SystemExit(f"semantic test marker count={count}")
semantic.write_text(text.replace(marker, insert, 1))
