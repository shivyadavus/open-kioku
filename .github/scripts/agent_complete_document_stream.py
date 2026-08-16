from pathlib import Path

# Core taxonomy.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
old = '''pub enum RetrievalSourceKind {
    Lexical,
    ExactSemantic,
'''
new = '''pub enum RetrievalSourceKind {
    Lexical,
    Document,
    ExactSemantic,
'''
if text.count(old) != 1:
    raise SystemExit(f'core retrieval enum marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

# Shared source/fusion plumbing and strict lexical/document pool separation.
path = Path('crates/open-kioku-context/src/candidates.rs')
text = path.read_text()
old = '''            for (index, mut result) in self
                .index
                .search(term, request.limit)?
                .into_iter()
                .enumerate()
            {
'''
new = '''            for (index, mut result) in self
                .index
                .search(term, request.limit)?
                .into_iter()
                .filter(|result| !is_document_candidate_path(&result.path.to_string_lossy()))
                .enumerate()
            {
'''
if text.count(old) != 1:
    raise SystemExit(f'external lexical filter marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''                (RetrievalSourceKind::Lexical, 1.00),
                (RetrievalSourceKind::ExactSemantic, 1.50),
'''
new = '''                (RetrievalSourceKind::Lexical, 1.00),
                (RetrievalSourceKind::Document, 0.90),
                (RetrievalSourceKind::ExactSemantic, 1.50),
'''
if text.count(old) != 1:
    raise SystemExit(f'evidence prior marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            (
                RetrievalSourceKind::Lexical,
                options.weights.text_relevance,
                defaults.text_relevance,
            ),
            (
                RetrievalSourceKind::ExactSemantic,
'''
new = '''            (
                RetrievalSourceKind::Lexical,
                options.weights.text_relevance,
                defaults.text_relevance,
            ),
            (
                RetrievalSourceKind::Document,
                options.weights.text_relevance,
                defaults.text_relevance,
            ),
            (
                RetrievalSourceKind::ExactSemantic,
'''
if text.count(old) != 1:
    raise SystemExit(f'ranking document mapping marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        if let RankingMode::WithoutSignal(signal) = options.mode {
            if let Some(source) = source_for_ranking_signal(signal) {
                config.source_weights.insert(source, 0.0);
            }
        }
'''
new = '''        if let RankingMode::WithoutSignal(signal) = options.mode {
            if let Some(source) = source_for_ranking_signal(signal) {
                config.source_weights.insert(source, 0.0);
            }
            if matches!(signal, RankingSignal::TextRelevance) {
                config.source_weights.insert(RetrievalSourceKind::Document, 0.0);
            }
        }
'''
if text.count(old) != 1:
    raise SystemExit(f'text relevance document ablation marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''fn all_retrieval_sources() -> [RetrievalSourceKind; 7] {
    [
        RetrievalSourceKind::Lexical,
        RetrievalSourceKind::ExactSemantic,
'''
new = '''fn all_retrieval_sources() -> [RetrievalSourceKind; 8] {
    [
        RetrievalSourceKind::Lexical,
        RetrievalSourceKind::Document,
        RetrievalSourceKind::ExactSemantic,
'''
if text.count(old) != 1:
    raise SystemExit(f'all sources marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''    match source {
        RetrievalSourceKind::Lexical => "lexical",
        RetrievalSourceKind::ExactSemantic => "exact_semantic",
'''
new = '''    match source {
        RetrievalSourceKind::Lexical => "lexical",
        RetrievalSourceKind::Document => "document",
        RetrievalSourceKind::ExactSemantic => "exact_semantic",
'''
if text.count(old) != 1:
    raise SystemExit(f'source label marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''fn normalize_candidate_path(path: &str) -> String {
    path.replace('\\\\', "/").trim_start_matches("./").to_string()
}
'''
replacement = '''fn normalize_candidate_path(path: &str) -> String {
    path.replace('\\\\', "/").trim_start_matches("./").to_string()
}

fn is_document_candidate_path(path: &str) -> bool {
    let path = normalize_candidate_path(path).to_ascii_lowercase();
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.ends_with("readme.md")
        || path.ends_with("readme.mdx")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
}
'''
if text.count(marker) != 1:
    raise SystemExit(f'document path helper marker count={text.count(marker)}')
text = text.replace(marker, replacement, 1)

old = '''        let index = FixtureSearchIndex {
            results: vec![
                result("src/a.rs", 0.9, Some("a")),
                result("src/a.rs", 0.8, Some("a2")),
                result("src/b.rs", 0.7, Some("b")),
            ],
        };
'''
new = '''        let index = FixtureSearchIndex {
            results: vec![
                result("docs/guide.md", 1.0, None),
                result("src/a.rs", 0.9, Some("a")),
                result("src/a.rs", 0.8, Some("a2")),
                result("src/b.rs", 0.7, Some("b")),
            ],
        };
'''
if text.count(old) != 1:
    raise SystemExit(f'search index fixture marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        assert_eq!(stream.candidates.len(), 2);
        assert_eq!(stream.candidates[0].result.path, PathBuf::from("src/a.rs"));
        assert_eq!(stream.candidates[1].result.path, PathBuf::from("src/b.rs"));
'''
new = '''        assert_eq!(stream.candidates.len(), 2);
        assert_eq!(stream.candidates[0].result.path, PathBuf::from("src/a.rs"));
        assert_eq!(stream.candidates[1].result.path, PathBuf::from("src/b.rs"));
        assert!(stream
            .candidates
            .iter()
            .all(|candidate| !is_document_candidate_path(&candidate.result.path.to_string_lossy())));
'''
if text.count(old) != 1:
    raise SystemExit(f'lexical excludes docs assertion marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

# Built-in independent document stream with cross-chunk heading context and line provenance.
path = Path('crates/open-kioku-context/src/candidates/builtins.rs')
text = path.read_text()
old = '''        if !excluded.contains(&RetrievalSourceKind::Lexical) {
            streams.push(self.lexical_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::ExactSemantic) {
'''
new = '''        if !excluded.contains(&RetrievalSourceKind::Lexical) {
            streams.push(self.lexical_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::Document) {
            streams.push(self.document_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::ExactSemantic) {
'''
if text.count(old) != 1:
    raise SystemExit(f'collect document stream marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            Ok(results) => CandidateStream::success(
                RetrievalSourceKind::Lexical,
                rerank_baseline(results)
                    .into_iter()
'''
new = '''            Ok(results) => CandidateStream::success(
                RetrievalSourceKind::Lexical,
                rerank_baseline(
                    results
                        .into_iter()
                        .filter(|result| !is_document_path(&result.path))
                        .collect(),
                )
                .into_iter()
'''
if text.count(old) != 1:
    raise SystemExit(f'builtin lexical filter marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {
'''
document_fn = '''    fn document_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let terms = retrieval_terms(request);
        let files_by_id = self
            .files
            .iter()
            .map(|file| (file.id.clone(), file))
            .collect::<BTreeMap<_, _>>();
        let mut chunks = self
            .chunks
            .iter()
            .filter_map(|chunk| {
                let file = files_by_id.get(&chunk.file_id).copied()?;
                is_document_path(&file.path).then_some((file, chunk))
            })
            .collect::<Vec<_>>();
        chunks.sort_by(|left, right| {
            left.0
                .path
                .cmp(&right.0.path)
                .then_with(|| left.1.range.start.cmp(&right.1.range.start))
                .then_with(|| left.1.range.end.cmp(&right.1.range.end))
        });

        let mut headings_by_path = BTreeMap::<String, Vec<String>>::new();
        let mut scored = Vec::new();
        for (file, chunk) in chunks {
            let path_key = normalized_path(&file.path);
            let headings = headings_by_path.entry(path_key.clone()).or_default();
            update_document_heading_path(headings, &chunk.text);
            let heading_path = headings.join(" > ");
            let haystack = format!("{} {} {}", path_key, heading_path, chunk.text)
                .to_ascii_lowercase();
            let overlap = term_overlap(&terms, &haystack);
            if overlap == 0 {
                continue;
            }
            let heading_label = if heading_path.is_empty() {
                "document root".to_string()
            } else {
                heading_path
            };
            let evidence_ref = format!(
                "document:{}:{}-{}",
                path_key, chunk.range.start, chunk.range.end
            );
            let reason = format!("document section `{heading_label}` matched task vocabulary");
            let result = SearchResult {
                path: file.path.clone(),
                line_range: Some(chunk.range.clone()),
                snippet: chunk.text.clone(),
                symbol: None,
                score: overlap as f32,
                match_reason: reason.clone(),
                evidence: vec![reason, format!("document heading path: {heading_label}")],
                evidence_refs: vec![evidence_ref],
                confidence: 0.65,
                score_breakdown: Vec::new(),
            };
            scored.push((
                overlap,
                StreamCandidate::from_result(
                    result,
                    RetrievalAuthority::Heuristic,
                    "heading-aware documentation candidate",
                ),
            ));
        }
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.result.path.cmp(&right.1.result.path))
                .then_with(|| {
                    left.1
                        .result
                        .line_range
                        .as_ref()
                        .map(|range| range.start)
                        .unwrap_or_default()
                        .cmp(
                            &right
                                .1
                                .result
                                .line_range
                                .as_ref()
                                .map(|range| range.start)
                                .unwrap_or_default(),
                        )
                })
        });
        CandidateStream::success(
            RetrievalSourceKind::Document,
            scored
                .into_iter()
                .map(|(_, candidate)| candidate)
                .take(request.limit)
                .collect(),
        )
    }

    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {
'''
if text.count(marker) != 1:
    raise SystemExit(f'document function insertion marker count={text.count(marker)}')
text = text.replace(marker, document_fn, 1)

marker = '''fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\\\', "/")
        .trim_start_matches("./")
        .to_string()
}
'''
helpers = '''fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn is_document_path(path: &Path) -> bool {
    let path = normalized_path(path).to_ascii_lowercase();
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.ends_with("readme.md")
        || path.ends_with("readme.mdx")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
}

fn update_document_heading_path(headings: &mut Vec<String>, text: &str) {
    for line in text.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let remainder = &trimmed[level..];
        if !remainder.starts_with(char::is_whitespace) {
            continue;
        }
        let title = remainder.trim();
        if title.is_empty() {
            continue;
        }
        headings.truncate(level.saturating_sub(1));
        while headings.len() < level.saturating_sub(1) {
            headings.push(String::new());
        }
        headings.push(title.to_string());
    }
    headings.retain(|heading| !heading.is_empty());
}
'''
if text.count(marker) != 1:
    raise SystemExit(f'document helper insertion marker count={text.count(marker)}')
text = text.replace(marker, helpers, 1)

marker = '''    #[test]
    fn source_paths_are_not_promoted_to_exact_symbol_anchors() {
        let anchors = symbol_anchor_keys("change src/Foo.java and config.json", &[]);
        assert!(!anchors.contains("src::Foo::java"));
        assert!(!anchors.contains("config::json"));
    }
'''
new_tests = '''    #[test]
    fn source_paths_are_not_promoted_to_exact_symbol_anchors() {
        let anchors = symbol_anchor_keys("change src/Foo.java and config.json", &[]);
        assert!(!anchors.contains("src::Foo::java"));
        assert!(!anchors.contains("config::json"));
    }

    #[test]
    fn document_paths_are_classified_without_claiming_code_authority() {
        assert!(is_document_path(Path::new("docs/guides/agent-workflows.md")));
        assert!(is_document_path(Path::new("README.md")));
        assert!(!is_document_path(Path::new("src/lib.rs")));
    }

    #[test]
    fn document_heading_path_tracks_nested_sections_across_chunks() {
        let mut headings = Vec::new();
        update_document_heading_path(&mut headings, "# Context Compiler\nintro\n## Retrieval\ntext");
        update_document_heading_path(&mut headings, "continued text\n### Documents\nmore");
        assert_eq!(headings, vec!["Context Compiler", "Retrieval", "Documents"]);
    }
'''
if text.count(marker) != 1:
    raise SystemExit(f'document tests marker count={text.count(marker)}')
text = text.replace(marker, new_tests, 1)
path.write_text(text)

# Benchmark documents as an independent advisory CC2 source without touching frozen CC1.
path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()
old = '''fn cc2_benchmark_sources() -> [open_kioku_core::RetrievalSourceKind; 6] {
    [
        open_kioku_core::RetrievalSourceKind::Lexical,
        open_kioku_core::RetrievalSourceKind::ExactSemantic,
'''
new = '''fn cc2_benchmark_sources() -> [open_kioku_core::RetrievalSourceKind; 7] {
    [
        open_kioku_core::RetrievalSourceKind::Lexical,
        open_kioku_core::RetrievalSourceKind::Document,
        open_kioku_core::RetrievalSourceKind::ExactSemantic,
'''
if text.count(old) != 1:
    raise SystemExit(f'benchmark source list marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''    match source {
        open_kioku_core::RetrievalSourceKind::Lexical => "lexical",
        open_kioku_core::RetrievalSourceKind::ExactSemantic => "exact_semantic",
'''
new = '''    match source {
        open_kioku_core::RetrievalSourceKind::Lexical => "lexical",
        open_kioku_core::RetrievalSourceKind::Document => "document",
        open_kioku_core::RetrievalSourceKind::ExactSemantic => "exact_semantic",
'''
if text.count(old) != 1:
    raise SystemExit(f'benchmark source label marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
