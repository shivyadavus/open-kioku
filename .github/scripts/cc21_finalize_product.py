from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one marker, found {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


STORAGE = "crates/open-kioku-storage/src/lib.rs"
CONFIG = "crates/open-kioku-config/src/lib.rs"
INGEST = "crates/open-kioku-ingest/src/lib.rs"
CONTEXT = "crates/open-kioku-context/src/candidates/builtins.rs"
BENCH = "crates/open-kioku-cli/benches/index.rs"

# Fail closed for custom stores that cannot atomically persist a non-empty document corpus.
replace_once(
    STORAGE,
    '''    fn replace_index_with_documents(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
    ) -> Result<()> {
        self.replace_index(data)?;
        self.replace_document_corpus(document_sections)
    }
''',
    '''    fn replace_index_with_documents(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
    ) -> Result<()> {
        if !document_sections.is_empty() {
            return Err(OkError::Unsupported(
                "atomic document corpus replacement is not implemented by this metadata store"
                    .into(),
            ));
        }
        self.replace_index(data)
    }
''',
)

# First-class document corpus configuration independent from code index mode.
replace_once(
    CONFIG,
    '''    pub index: IndexConfig,
    pub languages: LanguagesConfig,''',
    '''    pub index: IndexConfig,
    #[serde(default)]
    pub documents: DocumentsConfig,
    pub languages: LanguagesConfig,''',
)
replace_once(
    CONFIG,
    '''            },
            languages: LanguagesConfig {''',
    '''            },
            documents: DocumentsConfig::default(),
            languages: LanguagesConfig {''',
)
replace_once(
    CONFIG,
    '''pub struct IndexConfig {
    pub incremental: bool,
    pub max_file_size: String,
    pub exclude: Vec<String>,
    #[serde(default)]
    pub resolution_mode: ResolutionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguagesConfig {''',
    '''pub struct IndexConfig {
    pub incremental: bool,
    pub max_file_size: String,
    pub exclude: Vec<String>,
    #[serde(default)]
    pub resolution_mode: ResolutionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Additional documentation-oriented plain-text paths. Markdown/MDX and README files are
    /// first-class and do not need to be listed here.
    #[serde(default)]
    pub plain_text: Vec<String>,
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            plain_text: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguagesConfig {''',
)

# Scanner: same security/ignore walk, but document corpus can be disabled and plain text is opt-in.
replace_once(
    INGEST,
    '''        let max_size = config.max_file_size_bytes()?;
        let excludes = compile_globs(&config.index.exclude)?;
        let denied = compile_globs(&config.paths.deny)?;
        let git_ignores = build_ignore_matcher(root, ".gitignore")?;''',
    '''        let max_size = config.max_file_size_bytes()?;
        let excludes = compile_globs(&config.index.exclude)?;
        let denied = compile_globs(&config.paths.deny)?;
        let document_plain_text = compile_globs(&config.documents.plain_text)?;
        let git_ignores = build_ignore_matcher(root, ".gitignore")?;''',
)
replace_once(
    INGEST,
    '''            if let Some(document_type) = document_type_for_path(&rel) {
                let document_started = Instant::now();''',
    '''            if config.documents.enabled {
                if let Some(document_type) = document_type_for_path(&rel, &document_plain_text) {
                    let document_started = Instant::now();''',
)
replace_once(
    INGEST,
    '''                document_elapsed_ms = document_elapsed_ms.saturating_add(
                    u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                continue;
            }
            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {''',
    '''                    document_elapsed_ms = document_elapsed_ms.saturating_add(
                        u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    );
                    continue;
                }
            }
            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {''',
)
replace_once(
    INGEST,
    '''        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("document_corpus")
                .scanned(scan.document_file_count)
                .indexed(scan.document_file_count)
                .total(Some(scan.document_file_count)),
        );''',
    '''        let document_event = if config.documents.enabled {
            ProgressEvent::new("document_corpus")
                .scanned(scan.document_file_count)
                .indexed(scan.document_file_count)
                .total(Some(scan.document_file_count))
        } else {
            ProgressEvent::new("document_corpus")
                .warning("document corpus disabled by configuration")
        };
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            document_event,
        );''',
)
replace_once(
    INGEST,
    '''            emit_progress(
                &on_progress,
                &mut phase_reports,
                started,
                ProgressEvent::new("document_corpus").warning(
                    "document corpus unavailable in cross-project mode; source was not scanned",
                ),
            );''',
    '''            let document_event = if config.documents.enabled {
                ProgressEvent::new("document_corpus").warning(
                    "document corpus unavailable in cross-project mode; source was not scanned",
                )
            } else {
                ProgressEvent::new("document_corpus")
                    .warning("document corpus disabled by configuration")
            };
            emit_progress(
                &on_progress,
                &mut phase_reports,
                started,
                document_event,
            );''',
)

replace_once(
    INGEST,
    '''fn document_type_for_path(path: &Path) -> Option<DocumentType> {
    let normalized = path
        .to_string_lossy()
        .replace('\\\\', "/")
        .to_ascii_lowercase();
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let readme = name == "readme" || name.starts_with("readme.");
    let markdown = matches!(extension.as_deref(), Some("md"));
    let mdx = matches!(extension.as_deref(), Some("mdx"));
    let plain_text = matches!(extension.as_deref(), Some("txt"))
        && (readme || normalized.starts_with("docs/") || normalized.contains("/docs/"));
    if !(readme || markdown || mdx || plain_text) {
        return None;
    }
''',
    '''fn document_type_for_path(path: &Path, plain_text_paths: &GlobSet) -> Option<DocumentType> {
    let normalized = path
        .to_string_lossy()
        .replace('\\\\', "/")
        .to_ascii_lowercase();
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let readme = matches!(
        name.as_str(),
        "readme" | "readme.md" | "readme.mdx" | "readme.txt" | "readme.rst" | "readme.adoc"
    );
    let markdown = matches!(extension.as_deref(), Some("md"));
    let mdx = matches!(extension.as_deref(), Some("mdx"));
    let plain_text = matches!(extension.as_deref(), Some("txt"))
        && (readme || plain_text_paths.is_match(path));
    if !(readme || markdown || mdx || plain_text) {
        return None;
    }
''',
)

# Markdown headings inside fenced code are prose/code examples, not document structure.
replace_once(
    INGEST,
    '''    let mut current_start = 1u32;
    let mut current_lines = Vec::<String>::new();

    for (index, line) in content.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if let Some((level, title)) = document_markdown_heading(line) {''',
    '''    let mut current_start = 1u32;
    let mut current_lines = Vec::<String>::new();
    let mut fence: Option<(char, usize)> = None;

    for (index, line) in content.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let fence_boundary = document_fence_boundary(line);
        let heading = if fence.is_none() && fence_boundary.is_none() {
            document_markdown_heading(line)
        } else {
            None
        };
        if let Some((level, title)) = heading {''',
)
replace_once(
    INGEST,
    '''        }
        current_lines.push(line.to_string());
    }

    if !current_lines.is_empty() {''',
    '''        }
        current_lines.push(line.to_string());
        if let Some((marker, width)) = fence_boundary {
            match fence {
                Some((open_marker, open_width)) if marker == open_marker && width >= open_width => {
                    fence = None;
                }
                None => fence = Some((marker, width)),
                _ => {}
            }
        }
    }

    if !current_lines.is_empty() {''',
)
replace_once(
    INGEST,
    '''fn document_markdown_heading(line: &str) -> Option<(usize, &str)> {''',
    '''fn document_fence_boundary(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let width = trimmed.chars().take_while(|ch| *ch == marker).count();
    (width >= 3).then_some((marker, width))
}

fn document_markdown_heading(line: &str) -> Option<(usize, &str)> {''',
)

# New indexes can explicitly disable docs; do not silently fall back to legacy chunk-derived docs.
replace_once(
    CONTEXT,
    '''            Ok(_) => {
                if self
                    .store
                    .manifest()
                    .ok()
                    .flatten()
                    .is_some_and(|manifest| manifest.index_mode == IndexMode::CrossProject)
                {
                    return CandidateStream::unavailable(
                        RetrievalSourceKind::Document,
                        "document corpus unavailable in cross-project mode; source was not indexed",
                    );
                }
                self.legacy_document_stream(request)
            }''',
    '''            Ok(_) => {
                let manifest = self.store.manifest().ok().flatten();
                if manifest.as_ref().is_some_and(|manifest| {
                    manifest.phase_reports.iter().any(|report| {
                        report.phase == "document_corpus"
                            && report.warnings.iter().any(|warning| {
                                warning.contains("disabled by configuration")
                            })
                    })
                }) {
                    return CandidateStream::unavailable(
                        RetrievalSourceKind::Document,
                        "document corpus disabled by configuration",
                    );
                }
                if manifest
                    .as_ref()
                    .is_some_and(|manifest| manifest.index_mode == IndexMode::CrossProject)
                {
                    return CandidateStream::unavailable(
                        RetrievalSourceKind::Document,
                        "document corpus unavailable in cross-project mode; source was not indexed",
                    );
                }
                self.legacy_document_stream(request)
            }''',
)

# Focused hardening tests live at crate root so they can call private helpers through super::*.
with Path(INGEST).open("a") as f:
    f.write(r'''

#[cfg(test)]
mod document_corpus_acceptance_hardening_tests {
    use super::*;
    use std::fs;

    #[test]
    fn plain_text_is_opt_in_and_readme_code_is_not_a_document() {
        let none = compile_globs(&[]).unwrap();
        let configured = compile_globs(&["docs/*.txt".into(), "docs/**/*.txt".into()]).unwrap();
        assert_eq!(
            document_type_for_path(Path::new("docs/guide.md"), &none),
            Some(DocumentType::Markdown)
        );
        assert!(document_type_for_path(Path::new("docs/notes.txt"), &none).is_none());
        assert_eq!(
            document_type_for_path(Path::new("docs/notes.txt"), &configured),
            Some(DocumentType::PlainText)
        );
        assert_eq!(
            document_type_for_path(Path::new("README.txt"), &none),
            Some(DocumentType::Readme)
        );
        assert!(document_type_for_path(Path::new("README.rs"), &none).is_none());
        assert!(document_type_for_path(Path::new("docs/examples/client.rs"), &none).is_none());
    }

    #[test]
    fn fenced_markdown_heading_is_not_document_structure() {
        let content = "# Root\nintro\n```text\n## Fake heading\nbody\n```\n## Real heading\nreal body\n";
        let sections = build_document_sections(
            Path::new("docs/guide.md"),
            content,
            DocumentType::Markdown,
        );
        assert!(!sections.iter().any(|section| {
            section.heading_path.iter().any(|heading| heading == "Fake heading")
        }));
        assert!(sections.iter().any(|section| {
            section.heading_path == ["Root", "Real heading"]
                && section.line_range == LineRange { start: 7, end: 8 }
        }));
    }

    #[test]
    fn corpus_inherits_okignore_and_can_be_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/visible.md"), "# Visible\nkept\n").unwrap();
        fs::write(root.join("docs/ignored.md"), "# Ignored\nsecret\n").unwrap();
        fs::write(root.join(".okignore"), "docs/ignored.md\n").unwrap();

        let mut config = OkConfig::default();
        config.history.enabled = false;
        let enabled = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(enabled
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/visible.md")));
        assert!(!enabled
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/ignored.md")));

        config.documents.enabled = false;
        let disabled = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(disabled.document_sections.is_empty());
        let report = disabled
            .phase_reports
            .iter()
            .find(|report| report.phase == "document_corpus")
            .unwrap();
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("disabled by configuration")));
    }
}
''')

# Criterion: paired observational Fast/Full timings on the same code+docs workload.
Path(BENCH).write_text(r'''use criterion::{criterion_group, criterion_main, Criterion};
use open_kioku_config::OkConfig;
use open_kioku_core::IndexMode;
use open_kioku_ingest::Indexer;
use std::fs;
use tempfile::{tempdir, TempDir};

fn benchmark_repo() -> TempDir {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("examples")).unwrap();
    fs::create_dir_all(repo.join("testdata")).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    for i in 0..100 {
        fs::write(
            repo.join("src").join(format!("file_{i}.rs")),
            format!("pub fn function_{i}() {{ println!(\"Hello\"); }}"),
        )
        .unwrap();
    }
    for i in 0..40 {
        fs::write(
            repo.join("examples").join(format!("example_{i}.rs")),
            format!("pub fn example_{i}() {{}}"),
        )
        .unwrap();
        fs::write(
            repo.join("testdata").join(format!("fixture_{i}.rs")),
            format!("pub fn fixture_{i}() {{}}"),
        )
        .unwrap();
    }
    for i in 0..20 {
        fs::write(
            repo.join("docs").join(format!("guide_{i}.md")),
            format!("# Guide {i}\nintro\n## Workflow\nStep {i} uses deterministic context.\n"),
        )
        .unwrap();
    }
    temp
}

fn benchmark_config() -> OkConfig {
    let mut config = OkConfig::default();
    config.history.enabled = false;
    config.scip.enabled = false;
    config.semantic.enabled = false;
    config
}

fn benchmark_indexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("indexing");
    group.sample_size(10);

    group.bench_function("index_sample_repo", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let _ = Indexer::default()
                .index_repo(&repo, &benchmark_config())
                .unwrap();
        });
    });

    group.bench_function("index_mode_fast_with_document_corpus", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let snapshot = Indexer::default()
                .index_repo_with_mode(&repo, &benchmark_config(), IndexMode::Fast)
                .unwrap();
            assert!(!snapshot.document_sections.is_empty());
        });
    });

    group.bench_function("index_mode_full_with_document_corpus", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let snapshot = Indexer::default()
                .index_repo_with_mode(&repo, &benchmark_config(), IndexMode::Full)
                .unwrap();
            assert!(!snapshot.document_sections.is_empty());
        });
    });

    group.finish();
}

criterion_group!(benches, benchmark_indexing);
criterion_main!(benches);
''')

print("CC2.1 final product hardening applied")
