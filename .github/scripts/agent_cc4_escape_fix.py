from pathlib import Path

bs = chr(92)

for name in [
    "crates/open-kioku-context/src/routing.rs",
    "crates/open-kioku-semantic/src/lib.rs",
]:
    path = Path(name)
    text = path.read_text()
    bad = "replace('" + bs + "', \"/\")"
    good = "replace('" + (bs * 2) + "', \"/\")"
    if bad not in text:
        raise SystemExit(f"{name}: missing generated backslash replacement marker")
    text = text.replace(bad, good)
    path.write_text(text)

routing = Path("crates/open-kioku-context/src/routing.rs")
text = routing.read_text()
bad_path = "crates" + bs + "open-kioku-context" + bs + "src"
good_path = "crates" + (bs * 2) + "open-kioku-context" + (bs * 2) + "src"
if bad_path not in text:
    raise SystemExit("routing.rs: missing generated Windows path test marker")
text = text.replace(bad_path, good_path, 1)

old = '''        if single_path_lookup
            || previous
                .as_deref()
                .is_some_and(is_explicit_path_scope_marker)
        {
            if !scope.contains(&path) {
                scope.push(path);
            }
        }
'''
new = '''        if (single_path_lookup
            || previous
                .as_deref()
                .is_some_and(is_explicit_path_scope_marker))
            && !scope.contains(&path)
        {
            scope.push(path);
        }
'''
if text.count(old) != 1:
    raise SystemExit(f"routing.rs: collapsible-if marker count={text.count(old)}")
text = text.replace(old, new, 1)

old = '''fn normalize_scope_path(raw: &str) -> Option<String> {
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
'''
new = '''fn normalize_scope_path(raw: &str) -> Option<String> {
    // Keep leading `./`, path separators, and filename dots intact. `trim_query_token` is
    // intentionally broader for lexical classification and would turn `./src/lib.rs` into an
    // absolute-looking `/src/lib.rs`, causing a valid repository-relative scope to be rejected.
    let mut token = raw
        .trim_matches(|ch: char| {
            matches!(ch, '`' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '!' | '?')
        })
        .trim_end_matches('.')
        .to_string();
    while let Some((prefix, suffix)) = token.rsplit_once(':') {
        if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty() {
            token = prefix.to_string();
        } else {
            break;
        }
    }
'''
if text.count(old) != 1:
    raise SystemExit(f"routing.rs: normalize/while-let marker count={text.count(old)}")
text = text.replace(old, new, 1)
routing.write_text(text)

# Candidate scope is a retrieval contract, not merely a semantic hint. Filter every external
# source after retrieval and every built-in stream, while still allowing semantic search to push
# the filter down to the vector backend for correctness/efficiency.
candidates = Path("crates/open-kioku-context/src/candidates.rs")
text = candidates.read_text()
marker = '''pub trait ContextCandidateSource: Send + Sync {
'''
helper = '''pub(super) fn retain_request_path_scope(
    stream: &mut CandidateStream,
    request: &CandidateRequest,
) {
    if request.path_scope.is_empty() {
        return;
    }
    stream
        .candidates
        .retain(|candidate| path_matches_scope(&candidate.result.path, &request.path_scope));
}

fn path_matches_scope(path: &std::path::Path, path_scope: &[String]) -> bool {
    if path_scope.is_empty() {
        return true;
    }
    let candidate = normalize_candidate_path(&path.to_string_lossy());
    path_scope.iter().any(|scope| {
        let normalized = normalize_candidate_path(scope).trim_end_matches('/').to_string();
        !normalized.is_empty()
            && (candidate == normalized
                || candidate
                    .strip_prefix(&normalized)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    })
}

pub trait ContextCandidateSource: Send + Sync {
'''
if text.count(marker) != 1:
    raise SystemExit(f"candidates.rs: scope helper marker count={text.count(marker)}")
text = text.replace(marker, helper, 1)
old = '''                Ok(stream) if stream.source == expected => stream,
'''
new = '''                Ok(mut stream) if stream.source == expected => {
                    retain_request_path_scope(&mut stream, request);
                    stream
                }
'''
if text.count(old) != 1:
    raise SystemExit(f"candidates.rs: external scope marker count={text.count(old)}")
text = text.replace(old, new, 1)

# Add a narrow regression at the shared boundary: exact subtree semantics, no prefix bleed.
marker = '''    fn result(path: &str, score: f32, symbol: Option<&str>) -> SearchResult {
'''
if text.count(marker) != 1:
    raise SystemExit(f"candidates.rs: test helper marker count={text.count(marker)}")
# Insert the test immediately before the first existing candidate test, after helper definitions.
test_marker = '''    #[test]
    fn rrf_combines_independent_ranks_without_normalizing_raw_scores() {
'''
test = '''    #[test]
    fn request_path_scope_filters_external_candidates_without_prefix_bleed() {
        let request = CandidateRequest::new("inspect src/core", vec![], 10)
            .with_path_scope(vec!["src/core".into()]);
        let mut stream = CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![
                StreamCandidate::from_result(
                    result("src/core/lib.rs", 1.0, None),
                    RetrievalAuthority::Heuristic,
                    "inside",
                ),
                StreamCandidate::from_result(
                    result("src/core-old/lib.rs", 0.9, None),
                    RetrievalAuthority::Heuristic,
                    "prefix collision",
                ),
            ],
        );
        retain_request_path_scope(&mut stream, &request);
        assert_eq!(stream.candidates.len(), 1);
        assert_eq!(stream.candidates[0].result.path, PathBuf::from("src/core/lib.rs"));
    }

    #[test]
    fn rrf_combines_independent_ranks_without_normalizing_raw_scores() {
'''
if text.count(test_marker) != 1:
    raise SystemExit(f"candidates.rs: scope test insertion marker count={text.count(test_marker)}")
text = text.replace(test_marker, test, 1)
candidates.write_text(text)

builtins = Path("crates/open-kioku-context/src/candidates/builtins.rs")
text = builtins.read_text()
old = '''        let exact = self.exact_symbol_stream(request);
        let anchor_symbols = exact
'''
new = '''        let mut exact = self.exact_symbol_stream(request);
        // Scope exact anchors before they seed graph/history expansion; otherwise an out-of-scope
        // exact match could indirectly reintroduce candidates the user explicitly excluded.
        super::retain_request_path_scope(&mut exact, request);
        let anchor_symbols = exact
'''
if text.count(old) != 1:
    raise SystemExit(f"builtins.rs: exact scope marker count={text.count(old)}")
text = text.replace(old, new, 1)
old = '''        if !excluded.contains(&RetrievalSourceKind::GitHistory) {
            streams.push(self.history_stream(request, &anchor_symbols));
        }
        streams
'''
new = '''        if !excluded.contains(&RetrievalSourceKind::GitHistory) {
            streams.push(self.history_stream(request, &anchor_symbols));
        }
        for stream in &mut streams {
            super::retain_request_path_scope(stream, request);
        }
        streams
'''
if text.count(old) != 1:
    raise SystemExit(f"builtins.rs: stream scope marker count={text.count(old)}")
text = text.replace(old, new, 1)
builtins.write_text(text)

# Preserve the normal semantic unavailable-state contract before consulting scoped target metadata.
semantic = Path("crates/open-kioku-semantic/src/lib.rs")
text = semantic.read_text()
old = '''        if path_scope.is_empty() {
            return self.search(query, limit);
        }
        let targets = read_targets(&self.current_dir().join("ids.json"))?;
'''
new = '''        if path_scope.is_empty() {
            return self.search(query, limit);
        }
        let status = self.status();
        if !status.ready {
            return Err(OkError::Unsupported(format!(
                "semantic index is {}; run `ok semantic index` first",
                status.state
            )));
        }
        let targets = read_targets(&self.current_dir().join("ids.json"))?;
'''
if text.count(old) != 1:
    raise SystemExit(f"semantic lib: scoped status marker count={text.count(old)}")
text = text.replace(old, new, 1)
semantic.write_text(text)
