from pathlib import Path


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one marker, found {count}")
    path.write_text(text.replace(old, new, 1))


def replace_between(path: Path, start: str, end: str, replacement: str, label: str) -> None:
    text = path.read_text()
    start_index = text.find(start)
    if start_index < 0:
        raise SystemExit(f"{label}: start marker not found")
    end_index = text.find(end, start_index)
    if end_index < 0:
        raise SystemExit(f"{label}: end marker not found")
    if text.find(start, start_index + len(start)) >= 0:
        raise SystemExit(f"{label}: start marker is not unique")
    path.write_text(text[:start_index] + replacement + text[end_index:])


# Preserve bounded retrieval-unit provenance in diagnostics.
path = Path("crates/open-kioku-core/src/lib.rs")
replace_once(
    path,
    '''pub struct RetrievalTrace {
    pub path: PathBuf,
    pub fused_score: f32,
''',
    '''pub struct RetrievalTrace {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    pub fused_score: f32,
''',
    "retrieval trace line range",
)

# Make fusion unit-aware for document sections and tighten external lexical document filtering.
path = Path("crates/open-kioku-context/src/candidates.rs")
text = path.read_text()
replacements = [
    (
        '    let mut by_path = BTreeMap::<String, FusedEntry>::new();',
        '    let mut by_unit = BTreeMap::<String, FusedEntry>::new();',
        "fusion map name",
    ),
    (
        '        let deduped = dedupe_stream_candidates(&stream.candidates);',
        '        let deduped = dedupe_stream_candidates(stream.source, &stream.candidates);',
        "source-aware stream dedupe",
    ),
    (
        '            let key = normalize_candidate_path(&candidate.result.path.to_string_lossy());',
        '            let key = retrieval_unit_key(stream.source, &candidate.result);',
        "fusion unit key",
    ),
    (
        '            let entry = by_path.entry(key).or_insert_with(|| FusedEntry {',
        '            let entry = by_unit.entry(key).or_insert_with(|| FusedEntry {',
        "fusion unit entry",
    ),
    (
        '    let mut entries = by_path.into_values().collect::<Vec<_>>();',
        '    let mut entries = by_unit.into_values().collect::<Vec<_>>();',
        "fusion unit values",
    ),
    (
        '''        traces.push(RetrievalTrace {
            path: entry.representative.path.clone(),
            fused_score: entry.fused_score,
''',
        '''        traces.push(RetrievalTrace {
            path: entry.representative.path.clone(),
            line_range: entry.representative.line_range.clone(),
            fused_score: entry.fused_score,
''',
        "trace unit range",
    ),
    (
        'fn dedupe_stream_candidates(candidates: &[StreamCandidate]) -> Vec<StreamCandidate> {',
        'fn dedupe_stream_candidates(\n    source: RetrievalSourceKind,\n    candidates: &[StreamCandidate],\n) -> Vec<StreamCandidate> {',
        "dedupe signature",
    ),
    (
        '        let key = normalize_candidate_path(&candidate.result.path.to_string_lossy());',
        '        let key = retrieval_unit_key(source, &candidate.result);',
        "dedupe unit key",
    ),
]
for old, new, label in replacements:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one marker, found {count}")
    text = text.replace(old, new, 1)
path.write_text(text)

replace_between(
    path,
    'fn normalize_candidate_path(path: &str) -> String {',
    '\n#[cfg(test)]\nmod tests {',
    '''fn normalize_candidate_path(path: &str) -> String {
    path.replace('\\\\', "/").trim_start_matches("./").to_string()
}

fn retrieval_unit_key(source: RetrievalSourceKind, result: &SearchResult) -> String {
    let path = normalize_candidate_path(&result.path.to_string_lossy());
    if source == RetrievalSourceKind::Document {
        if let Some(range) = &result.line_range {
            return format!("{path}#L{}-{}", range.start, range.end);
        }
    }
    path
}

fn is_document_candidate_path(path: &str) -> bool {
    let path = normalize_candidate_path(path).to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    file_name.ends_with(".md")
        || file_name.ends_with(".mdx")
        || file_name.ends_with(".markdown")
        || matches!(file_name, "readme" | "contributing" | "changelog")
}
''',
    "candidate unit helpers",
)

# Add regression tests proving section identity is not collapsed and code examples under docs remain code.
text = path.read_text()
marker = '''    #[test]
    fn default_fusion_is_unweighted_until_calibration_is_benchmarked() {
'''
insert = '''    #[test]
    fn document_sections_remain_distinct_retrieval_units() {
        let mut first = result("docs/guide.md", 2.0, None);
        first.line_range = Some(LineRange { start: 1, end: 8 });
        first.evidence_refs = vec!["document:docs/guide.md:1-8".into()];
        let mut second = result("docs/guide.md", 1.0, None);
        second.line_range = Some(LineRange { start: 20, end: 28 });
        second.evidence_refs = vec!["document:docs/guide.md:20-28".into()];
        let stream = CandidateStream::success(
            RetrievalSourceKind::Document,
            vec![
                StreamCandidate::from_result(
                    first,
                    RetrievalAuthority::Heuristic,
                    "first document section",
                ),
                StreamCandidate::from_result(
                    second,
                    RetrievalAuthority::Heuristic,
                    "second document section",
                ),
            ],
        );

        let fused = fuse_candidate_streams(&[stream], 10, &FusionConfig::unweighted());
        assert_eq!(fused.results.len(), 2);
        let ranges = fused
            .results
            .iter()
            .filter_map(|result| result.line_range.as_ref())
            .map(|range| (range.start, range.end))
            .collect::<BTreeSet<_>>();
        assert_eq!(ranges, BTreeSet::from([(1, 8), (20, 28)]));
        assert!(fused
            .diagnostics
            .traces
            .iter()
            .all(|trace| trace.line_range.is_some()));
    }

    #[test]
    fn document_classifier_does_not_steal_code_examples_under_docs() {
        assert!(is_document_candidate_path("docs/guide.md"));
        assert!(is_document_candidate_path("README.mdx"));
        assert!(!is_document_candidate_path("docs/examples/demo.rs"));
        assert!(!is_document_candidate_path("docs/examples/demo.py"));
    }

    #[test]
    fn default_fusion_is_unweighted_until_calibration_is_benchmarked() {
'''
if text.count(marker) != 1:
    raise SystemExit(f"candidate regression test marker count={text.count(marker)}")
path.write_text(text.replace(marker, insert, 1))

# Replace the window-level document tagging with exact ATX-heading section extraction.
path = Path("crates/open-kioku-context/src/candidates/builtins.rs")
text = path.read_text()
old_import = '''    identity::symbol_node_id, AnalysisFact, CodeChunk, EvidenceSourceType, File, GraphEdge, NodeId,
    RetrievalAuthority, RetrievalSourceKind, SearchResult, Symbol, TestTarget,
'''
new_import = '''    identity::symbol_node_id, AnalysisFact, CodeChunk, EvidenceSourceType, File, GraphEdge,
    Language, LineRange, NodeId, RetrievalAuthority, RetrievalSourceKind, SearchResult, Symbol,
    TestTarget,
'''
if text.count(old_import) != 1:
    raise SystemExit(f"builtin import marker count={text.count(old_import)}")
path.write_text(text.replace(old_import, new_import, 1))

replace_between(
    path,
    '    fn document_stream(&self, request: &CandidateRequest) -> CandidateStream {',
    '    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {',
    '''    fn document_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let terms = document_retrieval_terms(request);
        if terms.is_empty() {
            return CandidateStream::success(RetrievalSourceKind::Document, Vec::new());
        }
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
                is_document_file(file).then_some((file, chunk))
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
            for section in markdown_sections(chunk, headings) {
                let heading_label = render_heading_path(&section.heading_path);
                let path_text = path_key.to_ascii_lowercase();
                let heading_text = heading_label.to_ascii_lowercase();
                let content_text = section.text.to_ascii_lowercase();
                let path_hits = term_overlap(&terms, &path_text);
                let heading_hits = term_overlap(&terms, &heading_text);
                let content_hits = term_overlap(&terms, &content_text);
                let rank_score = heading_hits * 4 + path_hits * 2 + content_hits;
                if rank_score == 0 {
                    continue;
                }
                let display_heading = if heading_label.is_empty() {
                    "document root".to_string()
                } else {
                    heading_label
                };
                let evidence_ref = format!(
                    "document:{}:{}-{}",
                    path_key, section.range.start, section.range.end
                );
                let reason = format!(
                    "document section `{display_heading}` matched task vocabulary"
                );
                let result = SearchResult {
                    path: file.path.clone(),
                    line_range: Some(section.range.clone()),
                    snippet: section.text,
                    symbol: None,
                    score: rank_score as f32,
                    match_reason: reason.clone(),
                    evidence: vec![
                        reason,
                        format!("document heading path: {display_heading}"),
                    ],
                    evidence_refs: vec![evidence_ref],
                    confidence: 0.65,
                    score_breakdown: Vec::new(),
                };
                scored.push((
                    rank_score,
                    heading_hits,
                    content_hits,
                    StreamCandidate::from_result(
                        result,
                        RetrievalAuthority::Heuristic,
                        "heading-aware documentation section candidate",
                    ),
                ));
            }
        }
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| left.3.result.path.cmp(&right.3.result.path))
                .then_with(|| {
                    left.3
                        .result
                        .line_range
                        .as_ref()
                        .map(|range| range.start)
                        .unwrap_or_default()
                        .cmp(
                            &right
                                .3
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
                .map(|(_, _, _, candidate)| candidate)
                .take(request.limit)
                .collect(),
        )
    }

''',
    "document stream section extraction",
)

# Add retrieval-term fallback without changing the terms used by other candidate streams.
text = path.read_text()
marker = '''fn term_overlap(terms: &[String], haystack: &str) -> usize {
'''
insert = '''fn document_retrieval_terms(request: &CandidateRequest) -> Vec<String> {
    request
        .search_terms
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(request.task.as_str()))
        .flat_map(|term| term.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-'))
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn term_overlap(terms: &[String], haystack: &str) -> usize {
'''
if text.count(marker) != 1:
    raise SystemExit(f"document retrieval term marker count={text.count(marker)}")
path.write_text(text.replace(marker, insert, 1))

replace_between(
    path,
    'fn is_document_path(path: &Path) -> bool {',
    '\n#[cfg(test)]\nmod exact_authority_tests {',
    '''fn is_document_path(path: &Path) -> bool {
    let path = normalized_path(path).to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    file_name.ends_with(".md")
        || file_name.ends_with(".mdx")
        || file_name.ends_with(".markdown")
        || matches!(file_name, "readme" | "contributing" | "changelog")
}

fn is_document_file(file: &File) -> bool {
    file.language == Language::Markdown || is_document_path(&file.path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownSection {
    heading_path: Vec<String>,
    range: LineRange,
    text: String,
}

fn markdown_sections(chunk: &CodeChunk, headings: &mut Vec<String>) -> Vec<MarkdownSection> {
    let lines = chunk.text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut sections = Vec::new();
    let mut section_start = 0usize;
    let mut section_heading = headings.clone();
    for (index, line) in lines.iter().enumerate() {
        let Some((level, title)) = markdown_heading(line) else {
            continue;
        };
        if index > section_start {
            push_markdown_section(
                &mut sections,
                chunk,
                &lines,
                section_start,
                index,
                &section_heading,
            );
        }
        apply_markdown_heading(headings, level, title);
        section_heading = headings.clone();
        section_start = index;
    }
    push_markdown_section(
        &mut sections,
        chunk,
        &lines,
        section_start,
        lines.len(),
        &section_heading,
    );
    sections
}

fn push_markdown_section(
    sections: &mut Vec<MarkdownSection>,
    chunk: &CodeChunk,
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    heading_path: &[String],
) {
    if start_index >= end_index {
        return;
    }
    let text = lines[start_index..end_index].join("\n");
    if text.trim().is_empty() {
        return;
    }
    let start = chunk.range.start.saturating_add(start_index as u32);
    let end = start
        .saturating_add((end_index - start_index).saturating_sub(1) as u32)
        .min(chunk.range.end);
    sections.push(MarkdownSection {
        heading_path: heading_path.to_vec(),
        range: LineRange { start, end },
        text,
    });
}

fn markdown_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let remainder = &trimmed[level..];
    if remainder.is_empty()
        || !remainder
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let title = remainder.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| (level, title.to_string()))
}

fn apply_markdown_heading(headings: &mut Vec<String>, level: usize, title: String) {
    headings.truncate(level.saturating_sub(1));
    while headings.len() < level.saturating_sub(1) {
        headings.push(String::new());
    }
    headings.push(title);
}

fn render_heading_path(headings: &[String]) -> String {
    headings
        .iter()
        .filter(|heading| !heading.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" > ")
}
''',
    "document helper replacement",
)

# Replace shallow document tests with section- and classifier-specific regression coverage.
text = path.read_text()
old_tests = '''    #[test]
    fn document_paths_are_classified_without_claiming_code_authority() {
        assert!(is_document_path(Path::new(
            "docs/guides/agent-workflows.md"
        )));
        assert!(is_document_path(Path::new("README.md")));
        assert!(!is_document_path(Path::new("src/lib.rs")));
    }

    #[test]
    fn document_heading_path_tracks_nested_sections_across_chunks() {
        let mut headings = Vec::new();
        update_document_heading_path(
            &mut headings,
            "# Context Compiler\nintro\n## Retrieval\ntext",
        );
        update_document_heading_path(
            &mut headings,
            "continued text\n### Documents\nmore",
        );
        assert_eq!(headings, vec!["Context Compiler", "Retrieval", "Documents"]);
    }
'''
new_tests = '''    #[test]
    fn document_paths_are_classified_without_stealing_code_examples() {
        assert!(is_document_path(Path::new("docs/guides/agent-workflows.md")));
        assert!(is_document_path(Path::new("README.mdx")));
        assert!(!is_document_path(Path::new("docs/examples/demo.rs")));
        assert!(!is_document_path(Path::new("src/lib.rs")));
    }

    #[test]
    fn markdown_sections_preserve_heading_paths_and_exact_ranges() {
        let chunk = CodeChunk {
            id: "doc:1".into(),
            file_id: open_kioku_core::FileId::new("doc-file"),
            range: LineRange { start: 1, end: 8 },
            language: Language::Markdown,
            text: "# Context\nintro\n## Retrieval\ntext\n### Documents\nmore\n## Safety\nfinal".into(),
            symbol_id: None,
        };
        let mut headings = Vec::new();
        let sections = markdown_sections(&chunk, &mut headings);
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].heading_path, vec!["Context"]);
        assert_eq!(sections[0].range, LineRange { start: 1, end: 2 });
        assert_eq!(sections[1].heading_path, vec!["Context", "Retrieval"]);
        assert_eq!(sections[1].range, LineRange { start: 3, end: 4 });
        assert_eq!(
            sections[2].heading_path,
            vec!["Context", "Retrieval", "Documents"]
        );
        assert_eq!(sections[2].range, LineRange { start: 5, end: 6 });
        assert_eq!(sections[3].heading_path, vec!["Context", "Safety"]);
        assert_eq!(sections[3].range, LineRange { start: 7, end: 8 });
    }

    #[test]
    fn markdown_heading_context_survives_chunk_boundaries() {
        let first = CodeChunk {
            id: "doc:1".into(),
            file_id: open_kioku_core::FileId::new("doc-file"),
            range: LineRange { start: 1, end: 2 },
            language: Language::Markdown,
            text: "# Context\nintro".into(),
            symbol_id: None,
        };
        let second = CodeChunk {
            id: "doc:2".into(),
            file_id: open_kioku_core::FileId::new("doc-file"),
            range: LineRange { start: 3, end: 5 },
            language: Language::Markdown,
            text: "continued\n## Retrieval\ntext".into(),
            symbol_id: None,
        };
        let mut headings = Vec::new();
        let first_sections = markdown_sections(&first, &mut headings);
        let second_sections = markdown_sections(&second, &mut headings);
        assert_eq!(first_sections[0].heading_path, vec!["Context"]);
        assert_eq!(second_sections[0].heading_path, vec!["Context"]);
        assert_eq!(second_sections[0].range, LineRange { start: 3, end: 3 });
        assert_eq!(second_sections[1].heading_path, vec!["Context", "Retrieval"]);
        assert_eq!(second_sections[1].range, LineRange { start: 4, end: 5 });
    }
'''
if text.count(old_tests) != 1:
    raise SystemExit(f"document test replacement marker count={text.count(old_tests)}")
path.write_text(text.replace(old_tests, new_tests, 1))

# Make task reranking use the same bounded retrieval identity as diagnostics.
path = Path("crates/open-kioku-context/src/lib.rs")
text = path.read_text()
old_import = '''    RetrievalAuthority, RetrievalDiagnostics, RetrievalSourceKind, RiskReport, RuntimeSignal,
    ScoreComponent, SearchResult, Symbol, ValidationPlan,
'''
new_import = '''    RetrievalAuthority, RetrievalDiagnostics, RetrievalSourceKind, RetrievalTrace, RiskReport,
    RuntimeSignal, ScoreComponent, SearchResult, Symbol, ValidationPlan,
'''
if text.count(old_import) != 1:
    raise SystemExit(f"context retrieval trace import marker count={text.count(old_import)}")
text = text.replace(old_import, new_import, 1)
old = '''    let authority_by_path = diagnostics
        .traces
        .iter()
        .map(|trace| (normalize_path(&trace.path), trace.authority))
        .collect::<std::collections::BTreeMap<_, _>>();
'''
new = '''    let authority_by_unit = diagnostics
        .traces
        .iter()
        .map(|trace| (retrieval_trace_key(trace), trace.authority))
        .collect::<std::collections::BTreeMap<_, _>>();
'''
if text.count(old) != 1:
    raise SystemExit(f"authority map marker count={text.count(old)}")
text = text.replace(old, new, 1)
text = text.replace(
    '''                authority_by_path
                    .get(&normalize_path(&b.path))
''',
    '''                authority_by_unit
                    .get(&search_result_retrieval_key(b))
''',
    1,
)
text = text.replace(
    '''                        &authority_by_path
                            .get(&normalize_path(&a.path))
''',
    '''                        &authority_by_unit
                            .get(&search_result_retrieval_key(a))
''',
    1,
)
marker = '''fn normalize_path(path: &std::path::Path) -> String {
'''
helpers = '''fn search_result_retrieval_key(result: &SearchResult) -> String {
    retrieval_identity_key(&result.path, result.line_range.as_ref())
}

fn retrieval_trace_key(trace: &RetrievalTrace) -> String {
    retrieval_identity_key(&trace.path, trace.line_range.as_ref())
}

fn retrieval_identity_key(
    path: &std::path::Path,
    range: Option<&open_kioku_core::LineRange>,
) -> String {
    let path = normalize_path(path);
    match range {
        Some(range) => format!("{path}#L{}-{}", range.start, range.end),
        None => path,
    }
}

fn normalize_path(path: &std::path::Path) -> String {
'''
if text.count(marker) != 1:
    raise SystemExit(f"retrieval identity helper marker count={text.count(marker)}")
path.write_text(text.replace(marker, helpers, 1))

print("CC2 document retrieval-unit refinement applied")
