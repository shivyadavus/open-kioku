from pathlib import Path

# 1) Add a first-class document retrieval source.
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

# 2) Keep code lexical and document pools independent before fusion.
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

# Strengthen external lexical regression: Markdown must not consume a lexical vote.
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
path.write_text(text)

# 3) Built-in heading-aware Markdown/MDX stream. It remains heuristic supporting evidence only.
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
        let mut chunks_by_file = BTreeMap::<_, Vec<&CodeChunk>>::new();
        for chunk in self.chunks {
            let Some(file) = files_by_id.get(&chunk.file_id).copied() else {
                continue;
            };
            if is_document_path(&file.path) {
                chunks_by_file.entry(chunk.file_id.clone()).or_default().push(chunk);
            }
        }

        let mut scored = Vec::new();
        for (file_id, mut chunks) in chunks_by_file {
            let Some(file) = files_by_id.get(&file_id).copied() else {
                continue;
            };
            chunks.sort_by_key(|chunk| (chunk.range.start, chunk.range.end));
            let mut heading_stack = Vec::<String>::new();
            for chunk in chunks {
                for section in markdown_sections(chunk, &mut heading_stack) {
                    let haystack = format!(
                        "{} {} {}",
                        file.path.display(),
                        section.heading_path,
                        section.text
                    )
                    .to_ascii_lowercase();
                    let overlap = term_overlap(&terms, &haystack);
                    if overlap == 0 {
                        continue;
                    }
                    let heading_label = if section.heading_path.is_empty() {
                        "document root".to_string()
                    } else {
                        section.heading_path.clone()
                    };
                    let evidence_ref = format!(
                        "document:{}:{}-{}",
                        normalized_path(&file.path),
                        section.range.start,
                        section.range.end
                    );
                    let reason = format!(
                        "document section `{heading_label}` matched task vocabulary"
                    );
                    let result = SearchResult {
                        path: file.path.clone(),
                        line_range: Some(section.range.clone()),
                        snippet: section.text,
                        symbol: None,
                        score: overlap as f32,
                        match_reason: reason.clone(),
                        evidence: vec![
                            reason,
                            format!("document heading path: {heading_label}"),
                        ],
                        evidence_refs: vec![evidence_ref],
                        confidence: 0.65,
                        score_breakdown: Vec::new(),
                    };
                    scored.push((
                        overlap,
                        StreamCandidate::from_result(
                            result,
                            RetrievalAuthority::Heuristic,
                            "heading-aware documentation candidate; supporting evidence only",
                        ),
                    ));
                }
            }
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

#[derive(Debug, Clone)]
struct MarkdownSection {
    heading_path: String,
    range: LineRange,
    text: String,
}

fn markdown_sections(chunk: &CodeChunk, heading_stack: &mut Vec<String>) -> Vec<MarkdownSection> {
    let lines = chunk.text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut sections = Vec::new();
    let mut section_start = 0usize;
    let mut section_heading = heading_stack.join(" > ");

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if !(1..=6).contains(&level) || !trimmed[level..].starts_with(char::is_whitespace) {
            continue;
        }
        let title = trimmed[level..].trim();
        if title.is_empty() {
            continue;
        }
        if index > section_start {
            sections.push(markdown_section(
                chunk,
                section_start,
                index - 1,
                &section_heading,
                &lines,
            ));
        }
        heading_stack.truncate(level.saturating_sub(1));
        while heading_stack.len() < level.saturating_sub(1) {
            heading_stack.push(String::new());
        }
        heading_stack.push(title.to_string());
        section_heading = heading_stack
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" > ");
        section_start = index;
    }

    if section_start < lines.len() {
        sections.push(markdown_section(
            chunk,
            section_start,
            lines.len() - 1,
            &section_heading,
            &lines,
        ));
    }
    sections
}

fn markdown_section(
    chunk: &CodeChunk,
    start_index: usize,
    end_index: usize,
    heading_path: &str,
    lines: &[&str],
) -> MarkdownSection {
    MarkdownSection {
        heading_path: heading_path.to_string(),
        range: LineRange {
            start: chunk.range.start + start_index,
            end: chunk.range.start + end_index,
        },
        text: lines[start_index..=end_index].join("\\n"),
    }
}
'''
if text.count(marker) != 1:
    raise SystemExit(f'document helper insertion marker count={text.count(marker)}')
text = text.replace(marker, helpers, 1)

# Add adversarial section-provenance tests at the end of the exact-authority test module marker.
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
'''
if text.count(marker) != 1:
    raise SystemExit(f'document path test marker count={text.count(marker)}')
text = text.replace(marker, new_tests, 1)

# Insert section continuity test before the module's closing exact-authority tests is difficult to
# anchor globally, so attach it near the path test using a self-contained CodeChunk fixture.
insert = '''
    #[test]
    fn markdown_sections_preserve_inherited_heading_and_line_ranges_across_chunks() {
        let mut headings = Vec::new();
        let first = CodeChunk {
            id: open_kioku_core::ChunkId::new("doc-1"),
            file_id: FileId::new("doc"),
            range: LineRange { start: 10, end: 12 },
            text: "# Guide\\n## Setup\\ninstall things".into(),
            symbol_id: None,
            language: Language::Unknown,
        };
        let second = CodeChunk {
            id: open_kioku_core::ChunkId::new("doc-2"),
            file_id: FileId::new("doc"),
            range: LineRange { start: 13, end: 14 },
            text: "continue setup\\nmore details".into(),
            symbol_id: None,
            language: Language::Unknown,
        };
        let first_sections = markdown_sections(&first, &mut headings);
        let second_sections = markdown_sections(&second, &mut headings);
        assert_eq!(first_sections.last().unwrap().heading_path, "Guide > Setup");
        assert_eq!(first_sections.last().unwrap().range, LineRange { start: 11, end: 12 });
        assert_eq!(second_sections[0].heading_path, "Guide > Setup");
        assert_eq!(second_sections[0].range, LineRange { start: 13, end: 14 });
    }
'''
anchor = new_tests
if text.count(anchor) != 1:
    raise SystemExit('document continuity test insertion anchor missing')
text = text.replace(anchor, anchor + insert, 1)
path.write_text(text)

# 4) Expose the document stream to per-stream benchmark ablation.
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
