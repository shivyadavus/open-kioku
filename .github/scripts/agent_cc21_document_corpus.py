from pathlib import Path
import re

ROOT = Path('.')


def read(path: str) -> str:
    return (ROOT / path).read_text()


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text)


def replace_once(path: str, old: str, new: str) -> None:
    text = read(path)
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{path}: expected one match, found {count}: {old[:120]!r}')
    write(path, text.replace(old, new, 1))


def insert_before_once(path: str, marker: str, insertion: str) -> None:
    text = read(path)
    count = text.count(marker)
    if count != 1:
        raise SystemExit(f'{path}: expected one insertion marker, found {count}: {marker[:120]!r}')
    write(path, text.replace(marker, insertion + marker, 1))


def append(path: str, text: str) -> None:
    current = read(path)
    if text.strip() in current:
        raise SystemExit(f'{path}: append payload already present')
    write(path, current.rstrip() + '\n\n' + text.strip() + '\n')


CORE = 'crates/open-kioku-core/src/lib.rs'
STORAGE = 'crates/open-kioku-storage/src/lib.rs'
SQLITE = 'crates/open-kioku-storage-sqlite/src/lib.rs'
INGEST = 'crates/open-kioku-ingest/src/lib.rs'
CONTEXT = 'crates/open-kioku-context/src/candidates/builtins.rs'
CLI_INDEX = 'crates/open-kioku-cli/src/commands/index.rs'
CLI_LIB = 'crates/open-kioku-cli/src/lib.rs'
WATCH = 'crates/open-kioku-watch/src/lib.rs'

# --- Core document model + explicit per-phase timing/count telemetry. ---
replace_once(
    CORE,
    '''impl LineRange {\n    pub fn single(line: u32) -> Self {\n        Self {\n            start: line,\n            end: line,\n        }\n    }\n}\n\n#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]\npub struct FileRange {''',
    '''impl LineRange {\n    pub fn single(line: u32) -> Self {\n        Self {\n            start: line,\n            end: line,\n        }\n    }\n}\n\n#[derive(\n    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,\n)]\n#[serde(rename_all = "snake_case")]\npub enum DocumentType {\n    Markdown,\n    Mdx,\n    Readme,\n    Adr,\n    Architecture,\n    Guide,\n    PlainText,\n}\n\n#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]\npub struct DocumentSection {\n    pub path: PathBuf,\n    pub heading_path: Vec<String>,\n    pub line_range: LineRange,\n    pub content_hash: String,\n    pub content: String,\n    pub document_type: DocumentType,\n}\n\n#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]\npub struct FileRange {''',
)
replace_once(
    CORE,
    '''pub struct IndexPhaseReport {\n    pub phase: String,\n    pub elapsed_ms: u64,\n    pub scanned_files: usize,\n    pub indexed_files: usize,\n    pub nodes_added: usize,''',
    '''pub struct IndexPhaseReport {\n    pub phase: String,\n    pub elapsed_ms: u64,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub duration_ms: Option<u64>,\n    pub scanned_files: usize,\n    pub indexed_files: usize,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub document_files: Option<usize>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub document_sections: Option<usize>,\n    pub nodes_added: usize,''',
)
replace_once(
    CORE,
    '"fast index mode may skip docs, examples, testdata, generated, vendor, unsupported, and oversized paths".into(),',
    '"fast code-analysis mode may skip examples, testdata, generated, vendor, unsupported, and oversized paths; documentation is handled by the lightweight document corpus when available".into(),',
)

# --- Storage contract: documents are independent from code metadata and have path-level delta updates. ---
replace_once(
    STORAGE,
    'AnalysisFact, ChurnSummary, CodeChunk, EvidenceSourceType, File, FileId, FileProvenance,',
    'AnalysisFact, ChurnSummary, CodeChunk, DocumentSection, EvidenceSourceType, File, FileId, FileProvenance,',
)
replace_once(STORAGE, 'use std::path::Path;', 'use std::path::{Path, PathBuf};')
replace_once(
    STORAGE,
    '''    fn replace_index(&self, data: IndexData<'_>) -> Result<()>;\n    fn replace_files_index(&self, _update: PartialIndexUpdate<'_>) -> Result<()> {''',
    '''    fn replace_index(&self, data: IndexData<'_>) -> Result<()>;\n    fn replace_index_with_documents(\n        &self,\n        data: IndexData<'_>,\n        document_sections: &[DocumentSection],\n    ) -> Result<()> {\n        self.replace_index(data)?;\n        self.replace_document_corpus(document_sections)\n    }\n    fn replace_files_index(&self, _update: PartialIndexUpdate<'_>) -> Result<()> {''',
)
replace_once(
    STORAGE,
    '''    fn all_chunks(&self) -> Result<Vec<CodeChunk>>;\n    fn tests(&self) -> Result<Vec<TestTarget>>;''',
    '''    fn all_chunks(&self) -> Result<Vec<CodeChunk>>;\n    fn document_sections(&self) -> Result<Vec<DocumentSection>> {\n        Ok(Vec::new())\n    }\n    fn replace_document_corpus(&self, _sections: &[DocumentSection]) -> Result<()> {\n        Err(OkError::Unsupported(\n            "document corpus replacement is not implemented by this metadata store".into(),\n        ))\n    }\n    fn replace_document_sections_for_paths(\n        &self,\n        _paths: &[PathBuf],\n        _sections: &[DocumentSection],\n    ) -> Result<()> {\n        Err(OkError::Unsupported(\n            "incremental document corpus replacement is not implemented by this metadata store"\n                .into(),\n        ))\n    }\n    fn tests(&self) -> Result<Vec<TestTarget>>;''',
)
insert_before_once(
    STORAGE,
    '\n#[cfg(test)]\nmod tests {',
    r'''
pub fn changed_document_paths(
    previous: &[DocumentSection],
    next: &[DocumentSection],
) -> BTreeSet<PathBuf> {
    fn fingerprints(
        sections: &[DocumentSection],
    ) -> std::collections::BTreeMap<PathBuf, Vec<(u32, u32, String)>> {
        let mut grouped = std::collections::BTreeMap::<
            PathBuf,
            Vec<(u32, u32, String)>,
        >::new();
        for section in sections {
            grouped.entry(section.path.clone()).or_default().push((
                section.line_range.start,
                section.line_range.end,
                section.content_hash.clone(),
            ));
        }
        for values in grouped.values_mut() {
            values.sort();
        }
        grouped
    }

    let previous = fingerprints(previous);
    let next = fingerprints(next);
    previous
        .keys()
        .chain(next.keys())
        .filter(|path| previous.get(*path) != next.get(*path))
        .cloned()
        .collect()
}
''',
)
append(
    STORAGE,
    r'''
#[cfg(test)]
mod document_change_tests {
    use super::changed_document_paths;
    use open_kioku_core::{DocumentSection, DocumentType, LineRange};
    use std::path::PathBuf;

    fn section(path: &str, hash: &str) -> DocumentSection {
        DocumentSection {
            path: PathBuf::from(path),
            heading_path: vec!["Guide".into()],
            line_range: LineRange { start: 1, end: 3 },
            content_hash: hash.into(),
            content: format!("content-{hash}"),
            document_type: DocumentType::Markdown,
        }
    }

    #[test]
    fn document_delta_reports_only_changed_added_and_deleted_paths() {
        let previous = vec![
            section("docs/stable.md", "same"),
            section("docs/changed.md", "old"),
            section("docs/deleted.md", "gone"),
        ];
        let next = vec![
            section("docs/stable.md", "same"),
            section("docs/changed.md", "new"),
            section("docs/added.md", "fresh"),
        ];
        let changed = changed_document_paths(&previous, &next);
        assert_eq!(
            changed,
            [
                PathBuf::from("docs/added.md"),
                PathBuf::from("docs/changed.md"),
                PathBuf::from("docs/deleted.md"),
            ]
            .into_iter()
            .collect()
        );
    }
}
''',
)

# --- SQLite corpus persistence. Additive table; no user_version bump required. ---
replace_once(
    SQLITE,
    'AnalysisFact, ChurnEntityKind, ChurnStats, ChurnSummary, CodeChunk, Confidence,',
    'AnalysisFact, ChurnEntityKind, ChurnStats, ChurnSummary, CodeChunk, Confidence, DocumentSection,',
)
replace_once(
    SQLITE,
    '''            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);\n            CREATE TABLE IF NOT EXISTS tests (''',
    '''            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);\n            CREATE TABLE IF NOT EXISTS document_sections (\n              path TEXT NOT NULL,\n              start_line INTEGER NOT NULL,\n              end_line INTEGER NOT NULL,\n              content_hash TEXT NOT NULL,\n              json TEXT NOT NULL,\n              PRIMARY KEY(path, start_line, end_line)\n            );\n            CREATE INDEX IF NOT EXISTS idx_document_sections_path\n              ON document_sections(path, start_line);\n            CREATE INDEX IF NOT EXISTS idx_document_sections_hash\n              ON document_sections(content_hash);\n            CREATE TABLE IF NOT EXISTS tests (''',
)
old_replace_index = r'''    fn replace_index(&self, data: IndexData<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        tx.execute("DELETE FROM call_sites", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM bindings", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM scopes", []).map_err(storage_err)?;
        tx.execute("DELETE FROM occurrences", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM analysis_facts", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM imports", []).map_err(storage_err)?;
        tx.execute("DELETE FROM tests", []).map_err(storage_err)?;
        tx.execute("DELETE FROM chunks", []).map_err(storage_err)?;
        tx.execute("DELETE FROM symbols", []).map_err(storage_err)?;
        tx.execute("DELETE FROM files", []).map_err(storage_err)?;
        tx.execute("DELETE FROM manifests", [])
            .map_err(storage_err)?;
        tx.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1)",
            params![serde_json::to_string(data.manifest)?],
        )
        .map_err(storage_err)?;
        insert_index_rows(
            &tx,
            IndexRows {
                files: data.files,
                symbols: data.symbols,
                chunks: data.chunks,
                tests: data.tests,
                imports: data.imports,
                occurrences: data.occurrences,
                analysis_facts: data.analysis_facts,
                scopes: data.scopes,
                bindings: data.bindings,
                call_sites: data.call_sites,
            },
        )?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }
'''
new_replace_index = r'''    fn replace_index(&self, data: IndexData<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        replace_index_rows(&tx, data)?;
        tx.execute("DELETE FROM document_sections", [])
            .map_err(storage_err)?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn replace_index_with_documents(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
    ) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        replace_index_rows(&tx, data)?;
        tx.execute("DELETE FROM document_sections", [])
            .map_err(storage_err)?;
        insert_document_sections(&tx, document_sections)?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }
'''
replace_once(SQLITE, old_replace_index, new_replace_index)
replace_once(
    SQLITE,
    '''    fn all_chunks(&self) -> Result<Vec<CodeChunk>> {\n        let conn = self\n            .connection\n            .lock()\n            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;\n        let mut stmt = conn\n            .prepare("SELECT json FROM chunks ORDER BY file_id, start_line")\n            .map_err(storage_err)?;\n        let rows = stmt\n            .query_map([], |row| row.get::<_, String>(0))\n            .map_err(storage_err)?;\n        collect_json(rows)\n    }\n\n    fn tests(&self) -> Result<Vec<TestTarget>> {''',
    '''    fn all_chunks(&self) -> Result<Vec<CodeChunk>> {\n        let conn = self\n            .connection\n            .lock()\n            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;\n        let mut stmt = conn\n            .prepare("SELECT json FROM chunks ORDER BY file_id, start_line")\n            .map_err(storage_err)?;\n        let rows = stmt\n            .query_map([], |row| row.get::<_, String>(0))\n            .map_err(storage_err)?;\n        collect_json(rows)\n    }\n\n    fn document_sections(&self) -> Result<Vec<DocumentSection>> {\n        let conn = self\n            .connection\n            .lock()\n            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;\n        let mut stmt = conn\n            .prepare(\n                "SELECT json FROM document_sections ORDER BY path, start_line, end_line",\n            )\n            .map_err(storage_err)?;\n        let rows = stmt\n            .query_map([], |row| row.get::<_, String>(0))\n            .map_err(storage_err)?;\n        collect_json(rows)\n    }\n\n    fn replace_document_corpus(&self, sections: &[DocumentSection]) -> Result<()> {\n        let mut conn = self\n            .connection\n            .lock()\n            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;\n        let tx = conn.transaction().map_err(storage_err)?;\n        tx.execute("DELETE FROM document_sections", [])\n            .map_err(storage_err)?;\n        insert_document_sections(&tx, sections)?;\n        tx.commit().map_err(storage_err)?;\n        Ok(())\n    }\n\n    fn replace_document_sections_for_paths(\n        &self,\n        paths: &[PathBuf],\n        sections: &[DocumentSection],\n    ) -> Result<()> {\n        if paths.is_empty() {\n            return Ok(());\n        }\n        let changed = paths\n            .iter()\n            .map(|path| path.to_string_lossy().replace('\\\\', "/"))\n            .collect::<BTreeSet<_>>();\n        let mut conn = self\n            .connection\n            .lock()\n            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;\n        let tx = conn.transaction().map_err(storage_err)?;\n        for path in &changed {\n            tx.execute(\n                "DELETE FROM document_sections WHERE path = ?1",\n                params![path],\n            )\n            .map_err(storage_err)?;\n        }\n        let replacements = sections\n            .iter()\n            .filter(|section| {\n                changed.contains(&section.path.to_string_lossy().replace('\\\\', "/"))\n            })\n            .cloned()\n            .collect::<Vec<_>>();\n        insert_document_sections(&tx, &replacements)?;\n        tx.commit().map_err(storage_err)?;\n        Ok(())\n    }\n\n    fn tests(&self) -> Result<Vec<TestTarget>> {''',
)
insert_before_once(
    SQLITE,
    '\nfn insert_index_rows(',
    r'''
fn replace_index_rows(tx: &Transaction<'_>, data: IndexData<'_>) -> Result<()> {
    tx.execute("DELETE FROM call_sites", []).map_err(storage_err)?;
    tx.execute("DELETE FROM bindings", []).map_err(storage_err)?;
    tx.execute("DELETE FROM scopes", []).map_err(storage_err)?;
    tx.execute("DELETE FROM occurrences", []).map_err(storage_err)?;
    tx.execute("DELETE FROM analysis_facts", []).map_err(storage_err)?;
    tx.execute("DELETE FROM imports", []).map_err(storage_err)?;
    tx.execute("DELETE FROM tests", []).map_err(storage_err)?;
    tx.execute("DELETE FROM chunks", []).map_err(storage_err)?;
    tx.execute("DELETE FROM symbols", []).map_err(storage_err)?;
    tx.execute("DELETE FROM files", []).map_err(storage_err)?;
    tx.execute("DELETE FROM manifests", []).map_err(storage_err)?;
    tx.execute(
        "INSERT INTO manifests(id, json) VALUES(1, ?1)",
        params![serde_json::to_string(data.manifest)?],
    )
    .map_err(storage_err)?;
    insert_index_rows(
        tx,
        IndexRows {
            files: data.files,
            symbols: data.symbols,
            chunks: data.chunks,
            tests: data.tests,
            imports: data.imports,
            occurrences: data.occurrences,
            analysis_facts: data.analysis_facts,
            scopes: data.scopes,
            bindings: data.bindings,
            call_sites: data.call_sites,
        },
    )
}

fn insert_document_sections(tx: &Transaction<'_>, sections: &[DocumentSection]) -> Result<()> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO document_sections(path, start_line, end_line, content_hash, json) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(storage_err)?;
    for section in sections {
        let path = section.path.to_string_lossy().replace('\\', "/");
        stmt.execute(params![
            path,
            i64::from(section.line_range.start),
            i64::from(section.line_range.end),
            &section.content_hash,
            serde_json::to_string(section)?,
        ])
        .map_err(storage_err)?;
    }
    Ok(())
}
''',
)
append(
    SQLITE,
    r'''
#[cfg(test)]
mod document_corpus_tests {
    use super::*;
    use open_kioku_core::{DocumentSection, DocumentType, LineRange};
    use open_kioku_storage::MetadataStore;

    fn section(path: &str, hash: &str, content: &str) -> DocumentSection {
        DocumentSection {
            path: PathBuf::from(path),
            heading_path: vec!["Guide".into()],
            line_range: LineRange { start: 1, end: 3 },
            content_hash: hash.into(),
            content: content.into(),
            document_type: DocumentType::Markdown,
        }
    }

    #[test]
    fn partial_document_replacement_preserves_unaffected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("index.sqlite")).unwrap();
        store
            .replace_document_corpus(&[
                section("docs/a.md", "a1", "old-a"),
                section("docs/b.md", "b1", "stable-b"),
            ])
            .unwrap();

        store
            .replace_document_sections_for_paths(
                &[PathBuf::from("docs/a.md")],
                &[
                    section("docs/a.md", "a2", "new-a"),
                    section("docs/b.md", "b2-ignored", "should-not-rewrite"),
                ],
            )
            .unwrap();

        let sections = store.document_sections().unwrap();
        assert_eq!(sections.len(), 2);
        assert!(sections.iter().any(|section| {
            section.path == Path::new("docs/a.md") && section.content == "new-a"
        }));
        assert!(sections.iter().any(|section| {
            section.path == Path::new("docs/b.md") && section.content == "stable-b"
        }));
    }
}
''',
)

# --- Ingestion: common policy walk forks allowed files into code vs lightweight document corpus. ---
replace_once(
    INGEST,
    'AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GitCochangeEdge,',
    'AnalysisFact, CodeChunk, Confidence, DocumentSection, DocumentType, EvidenceSourceType, File, FileId, GitCochangeEdge,',
)
replace_once(
    INGEST,
    'use std::collections::{BTreeMap, HashMap, HashSet};',
    'use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};',
)
replace_once(
    INGEST,
    '''    pub chunks: Vec<CodeChunk>,\n    pub tests: Vec<TestTarget>,''',
    '''    pub chunks: Vec<CodeChunk>,\n    pub document_sections: Vec<DocumentSection>,\n    pub tests: Vec<TestTarget>,''',
)
# Cross-project snapshot explicitly reports unavailable document corpus.
replace_once(
    INGEST,
    '''            emit_progress(\n                &on_progress,\n                &mut phase_reports,\n                started,\n                ProgressEvent::new("cross_project")\n                    .warning("cross-project mode records repository status without parsing source"),\n            );\n            let quality = index_quality(IndexQualityInput {''',
    '''            emit_progress(\n                &on_progress,\n                &mut phase_reports,\n                started,\n                ProgressEvent::new("cross_project")\n                    .warning("cross-project mode records repository status without parsing source"),\n            );\n            emit_progress(\n                &on_progress,\n                &mut phase_reports,\n                started,\n                ProgressEvent::new("document_corpus")\n                    .warning("document corpus unavailable in cross-project mode; source was not scanned"),\n            );\n            if let Some(report) = phase_reports.last_mut() {\n                report.duration_ms = Some(0);\n                report.document_files = Some(0);\n                report.document_sections = Some(0);\n            }\n            let quality = index_quality(IndexQualityInput {''',
)
replace_once(
    INGEST,
    '''                    chunks: Vec::new(),\n                    tests: Vec::new(),''',
    '''                    chunks: Vec::new(),\n                    document_sections: Vec::new(),\n                    tests: Vec::new(),''',
)
# Emit document telemetry before code parsing.
replace_once(
    INGEST,
    '''        let files = scan.files;\n        emit_progress(\n            &on_progress,\n            &mut phase_reports,\n            started,\n            ProgressEvent::new("parse")''',
    '''        let files = scan.files;\n        let document_sections = scan.document_sections;\n        emit_progress(\n            &on_progress,\n            &mut phase_reports,\n            started,\n            ProgressEvent::new("document_corpus")\n                .scanned(scan.document_file_count)\n                .indexed(scan.document_file_count)\n                .total(Some(scan.document_file_count)),\n        );\n        if let Some(report) = phase_reports.last_mut() {\n            report.duration_ms = Some(scan.document_elapsed_ms);\n            report.document_files = Some(scan.document_file_count);\n            report.document_sections = Some(document_sections.len());\n        }\n        emit_progress(\n            &on_progress,\n            &mut phase_reports,\n            started,\n            ProgressEvent::new("parse")''',
)
replace_once(
    INGEST,
    '''                chunks,\n                tests,''',
    '''                chunks,\n                document_sections,\n                tests,''',
)
# Scan variables.
replace_once(
    INGEST,
    '''        let mut files = Vec::new();\n        let mut skipped_paths = Vec::new();''',
    '''        let mut files = Vec::new();\n        let mut document_sections = Vec::new();\n        let mut document_paths = BTreeSet::<PathBuf>::new();\n        let mut document_elapsed_ms = 0u64;\n        let mut skipped_paths = Vec::new();''',
)
# Reorder size before fast-mode and divert documents before code language gating.
old_scan_segment = r'''            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::FastMode,
                    SkipSource::FastMode,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > max_size {
                push_skip(
                    root,
                    path,
                    SkipReason::TooLarge,
                    SkipSource::SizeLimit,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let language = detect_language(&rel);
'''
new_scan_segment = r'''            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > max_size {
                push_skip(
                    root,
                    path,
                    SkipReason::TooLarge,
                    SkipSource::SizeLimit,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if let Some(document_type) = document_type_for_path(&rel) {
                let document_started = Instant::now();
                let bytes = fs::read(path)?;
                if bytes.contains(&0) {
                    document_elapsed_ms = document_elapsed_ms.saturating_add(
                        u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    );
                    push_skip(
                        root,
                        path,
                        SkipReason::Binary,
                        SkipSource::Detector,
                        true,
                        &mut skipped_paths,
                    );
                    continue;
                }
                let content = String::from_utf8_lossy(&bytes).into_owned();
                if likely_generated(&content) {
                    document_elapsed_ms = document_elapsed_ms.saturating_add(
                        u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    );
                    push_skip(
                        root,
                        path,
                        SkipReason::Generated,
                        SkipSource::Detector,
                        true,
                        &mut skipped_paths,
                    );
                    continue;
                }
                document_paths.insert(rel.clone());
                document_sections.extend(build_document_sections(
                    &rel,
                    &content,
                    document_type,
                ));
                document_elapsed_ms = document_elapsed_ms.saturating_add(
                    u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                continue;
            }
            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::FastMode,
                    SkipSource::FastMode,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let language = detect_language(&rel);
'''
replace_once(INGEST, old_scan_segment, new_scan_segment)
replace_once(
    INGEST,
    '"fast mode skipped {fast_skipped} docs/examples/testdata/sample path(s)"',
    '"fast mode skipped {fast_skipped} code/example/testdata/sample path(s) from code analysis"',
)
replace_once(
    INGEST,
    '''        Ok(ScanResult {\n            files,\n            skipped,\n            warnings,\n            skipped_paths,\n        })''',
    '''        Ok(ScanResult {\n            files,\n            document_sections,\n            document_file_count: document_paths.len(),\n            document_elapsed_ms,\n            skipped,\n            warnings,\n            skipped_paths,\n        })''',
)
replace_once(
    INGEST,
    '''struct ScanResult {\n    files: Vec<File>,\n    skipped: usize,''',
    '''struct ScanResult {\n    files: Vec<File>,\n    document_sections: Vec<DocumentSection>,\n    document_file_count: usize,\n    document_elapsed_ms: u64,\n    skipped: usize,''',
)
replace_once(
    INGEST,
    '''            phase: self.phase.to_string(),\n            elapsed_ms: self.elapsed_ms,\n            scanned_files: self.scanned_files,\n            indexed_files: self.indexed_files,\n            nodes_added: self.nodes_added,''',
    '''            phase: self.phase.to_string(),\n            elapsed_ms: self.elapsed_ms,\n            duration_ms: None,\n            scanned_files: self.scanned_files,\n            indexed_files: self.indexed_files,\n            document_files: None,\n            document_sections: None,\n            nodes_added: self.nodes_added,''',
)
replace_once(
    INGEST,
    '"fast mode: docs, examples, generated files, vendor paths, testdata, unsupported files, and oversized files may be skipped"',
    '"fast mode: code analysis may skip docs/examples/generated/vendor/testdata/unsupported/oversized paths; allowed documentation is indexed separately in the lightweight document corpus"',
)

DOCUMENT_HELPERS = r'''
const MAX_DOCUMENT_SECTION_LINES: usize = 120;

fn document_type_for_path(path: &Path) -> Option<DocumentType> {
    let normalized = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
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

    if readme {
        return Some(DocumentType::Readme);
    }
    if normalized
        .split('/')
        .any(|component| matches!(component, "adr" | "adrs" | "decisions"))
        || name.starts_with("adr-")
        || name.starts_with("adr_")
    {
        return Some(DocumentType::Adr);
    }
    if normalized.contains("architecture")
        || name.contains("architecture")
        || name.contains("design")
    {
        return Some(DocumentType::Architecture);
    }
    if name.starts_with("contributing")
        || name.contains("developer")
        || name.contains("development")
    {
        return Some(DocumentType::Guide);
    }
    if markdown {
        Some(DocumentType::Markdown)
    } else if mdx {
        Some(DocumentType::Mdx)
    } else {
        Some(DocumentType::PlainText)
    }
}

fn build_document_sections(
    path: &Path,
    content: &str,
    document_type: DocumentType,
) -> Vec<DocumentSection> {
    if is_markdown_like_document(path) {
        build_markdown_document_sections(path, content, document_type)
    } else {
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let mut sections = Vec::new();
        push_bounded_document_sections(
            &mut sections,
            path,
            Vec::new(),
            1,
            &lines,
            document_type,
        );
        sections
    }
}

fn is_markdown_like_document(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    matches!(extension.as_deref(), Some("md" | "mdx"))
        || path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("readme"))
}

fn build_markdown_document_sections(
    path: &Path,
    content: &str,
    document_type: DocumentType,
) -> Vec<DocumentSection> {
    let mut sections = Vec::new();
    let mut heading_stack = Vec::<String>::new();
    let mut current_heading_path = Vec::<String>::new();
    let mut current_start = 1u32;
    let mut current_lines = Vec::<String>::new();

    for (index, line) in content.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if let Some((level, title)) = document_markdown_heading(line) {
            if !current_lines.is_empty() {
                push_bounded_document_sections(
                    &mut sections,
                    path,
                    current_heading_path.clone(),
                    current_start,
                    &current_lines,
                    document_type,
                );
                current_lines.clear();
            }
            update_document_heading_stack(&mut heading_stack, level, title);
            current_heading_path = heading_stack.clone();
            current_start = line_number;
        } else if current_lines.is_empty() {
            current_heading_path = heading_stack.clone();
            current_start = line_number;
        }
        current_lines.push(line.to_string());
    }

    if !current_lines.is_empty() {
        push_bounded_document_sections(
            &mut sections,
            path,
            current_heading_path,
            current_start,
            &current_lines,
            document_type,
        );
    }
    sections
}

fn push_bounded_document_sections(
    sections: &mut Vec<DocumentSection>,
    path: &Path,
    heading_path: Vec<String>,
    start_line: u32,
    lines: &[String],
    document_type: DocumentType,
) {
    for (index, window) in lines.chunks(MAX_DOCUMENT_SECTION_LINES).enumerate() {
        let text = window.join("\n");
        if text.trim().is_empty() {
            continue;
        }
        let offset = u32::try_from(index.saturating_mul(MAX_DOCUMENT_SECTION_LINES))
            .unwrap_or(u32::MAX);
        let start = start_line.saturating_add(offset);
        let end = start.saturating_add(u32::try_from(window.len().saturating_sub(1)).unwrap_or(u32::MAX));
        sections.push(DocumentSection {
            path: path.to_path_buf(),
            heading_path: heading_path.clone(),
            line_range: LineRange { start, end },
            content_hash: hash_bytes(text.as_bytes()),
            content: text,
            document_type,
        });
    }
}

fn document_markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let remainder = &trimmed[level..];
    if !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let title = remainder.trim();
    (!title.is_empty()).then_some((level, title))
}

fn update_document_heading_stack(headings: &mut Vec<String>, level: usize, title: &str) {
    let parent_count = level.saturating_sub(1);
    if headings.len() > parent_count {
        headings.truncate(parent_count);
    }
    headings.push(title.to_string());
}

'''
insert_before_once(INGEST, '\nfn fast_mode_skip_path(path: &Path) -> bool {', DOCUMENT_HELPERS)
append(
    INGEST,
    r'''
#[cfg(test)]
mod document_corpus_tests {
    use super::*;
    use std::fs;

    #[test]
    fn document_classifier_does_not_capture_code_examples_under_docs() {
        assert_eq!(
            document_type_for_path(Path::new("docs/guide.md")),
            Some(DocumentType::Markdown)
        );
        assert_eq!(
            document_type_for_path(Path::new("docs/notes.txt")),
            Some(DocumentType::PlainText)
        );
        assert_eq!(
            document_type_for_path(Path::new("README")),
            Some(DocumentType::Readme)
        );
        assert!(document_type_for_path(Path::new("notes.txt")).is_none());
        assert!(document_type_for_path(Path::new("docs/examples/client.rs")).is_none());
    }

    #[test]
    fn markdown_sections_preserve_heading_paths_and_are_bounded() {
        let mut content = String::from("# Root\nintro\n## Rotation\n");
        for index in 0..250 {
            content.push_str(&format!("rotation line {index}\n"));
        }
        let sections = build_document_sections(
            Path::new("docs/guide.md"),
            &content,
            DocumentType::Markdown,
        );
        let rotation = sections
            .iter()
            .filter(|section| section.heading_path == ["Root", "Rotation"])
            .collect::<Vec<_>>();
        assert_eq!(rotation.len(), 3);
        assert!(rotation
            .iter()
            .all(|section| section.line_range.end - section.line_range.start < 120));
        assert_eq!(rotation[0].line_range.start, 3);
        assert!(!rotation[0].content_hash.is_empty());
    }

    #[test]
    fn fast_mode_document_corpus_uses_common_security_and_ignore_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        fs::write(root.join("docs/visible.md"), "# Visible\nquasar protocol\n").unwrap();
        fs::write(root.join("docs/blocked.md"), "# Blocked\nsecret design\n").unwrap();
        let mut config = OkConfig::default();
        config.history.enabled = false;
        config.paths.deny = vec!["docs/blocked.md".into()];

        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(snapshot
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/visible.md")));
        assert!(!snapshot
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/blocked.md")));
        assert!(!snapshot
            .files
            .iter()
            .any(|file| file.path == Path::new("docs/visible.md")));
        let report = snapshot
            .phase_reports
            .iter()
            .find(|report| report.phase == "document_corpus")
            .unwrap();
        assert_eq!(report.document_files, Some(1));
        assert!(report.document_sections.is_some_and(|count| count >= 1));
        assert!(report.duration_ms.is_some());
    }
}
''',
)

# --- Context: prefer persisted document corpus; retain old chunk-derived fallback for migration. ---
replace_once(
    CONTEXT,
    'identity::symbol_node_id, AnalysisFact, CodeChunk, EvidenceSourceType, File, GraphEdge,\n    LineRange, NodeId, RetrievalAuthority, RetrievalSourceKind, SearchResult, Symbol, TestTarget,',
    'identity::symbol_node_id, AnalysisFact, CodeChunk, DocumentSection, EvidenceSourceType, File, GraphEdge,\n    IndexMode, LineRange, NodeId, RetrievalAuthority, RetrievalSourceKind, SearchResult, Symbol, TestTarget,',
)
replace_once(
    CONTEXT,
    '    fn document_stream(&self, request: &CandidateRequest) -> CandidateStream {\n        let terms = retrieval_terms(request);',
    '''    fn document_stream(&self, request: &CandidateRequest) -> CandidateStream {\n        match self.store.document_sections() {\n            Ok(sections) if !sections.is_empty() => indexed_document_stream(request, &sections),\n            Ok(_) => {\n                if self\n                    .store\n                    .manifest()\n                    .ok()\n                    .flatten()\n                    .is_some_and(|manifest| manifest.index_mode == IndexMode::CrossProject)\n                {\n                    return CandidateStream::unavailable(\n                        RetrievalSourceKind::Document,\n                        "document corpus unavailable in cross-project mode; source was not indexed",\n                    );\n                }\n                self.legacy_document_stream(request)\n            }\n            Err(err) => CandidateStream::unavailable(\n                RetrievalSourceKind::Document,\n                format!("document corpus unavailable: {err}"),\n            ),\n        }\n    }\n\n    fn legacy_document_stream(&self, request: &CandidateRequest) -> CandidateStream {\n        let terms = retrieval_terms(request);''',
)
insert_before_once(
    CONTEXT,
    '\npub(super) fn incident_edge_ids(',
    r'''
fn indexed_document_stream(
    request: &CandidateRequest,
    sections: &[DocumentSection],
) -> CandidateStream {
    let terms = retrieval_terms(request);
    let mut scored = sections
        .iter()
        .filter_map(|section| {
            let path = normalized_path(&section.path);
            let heading_label = if section.heading_path.is_empty() {
                "document root".to_string()
            } else {
                section.heading_path.join(" > ")
            };
            let heading_overlap = term_overlap(&terms, &heading_label.to_ascii_lowercase());
            let body_overlap = term_overlap(&terms, &section.content.to_ascii_lowercase());
            let path_overlap = term_overlap(&terms, &path.to_ascii_lowercase());
            if heading_overlap + body_overlap + path_overlap == 0 {
                return None;
            }
            let score =
                body_overlap as f32 + heading_overlap as f32 * 1.5 + path_overlap as f32 * 0.5;
            let evidence_ref = format!(
                "document:{}:{}-{}",
                path, section.line_range.start, section.line_range.end
            );
            let reason = format!("document section `{heading_label}` matched task vocabulary");
            let result = SearchResult {
                path: section.path.clone(),
                line_range: Some(section.line_range.clone()),
                snippet: section.content.clone(),
                symbol: None,
                score,
                match_reason: reason.clone(),
                evidence: vec![
                    reason,
                    format!("document heading path: {heading_label}"),
                    format!("document content hash: {}", section.content_hash),
                ],
                evidence_refs: vec![evidence_ref],
                confidence: 0.65,
                score_breakdown: Vec::new(),
            };
            Some((
                score,
                StreamCandidate::from_result(
                    result,
                    RetrievalAuthority::Heuristic,
                    "indexed document section matched the task",
                ),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.result.path.cmp(&right.1.result.path))
            .then_with(|| {
                left.1
                    .result
                    .line_range
                    .as_ref()
                    .map(|range| (range.start, range.end))
                    .cmp(
                        &right
                            .1
                            .result
                            .line_range
                            .as_ref()
                            .map(|range| (range.start, range.end)),
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

''',
)
append(
    CONTEXT,
    r'''
#[cfg(test)]
mod indexed_document_stream_tests {
    use super::*;
    use open_kioku_core::{DocumentType, LineRange};
    use std::path::PathBuf;

    #[test]
    fn indexed_document_stream_preserves_heading_range_and_heuristic_authority() {
        let section = DocumentSection {
            path: PathBuf::from("docs/architecture.md"),
            heading_path: vec!["Runtime".into(), "Rotation protocol".into()],
            line_range: LineRange { start: 3, end: 5 },
            content_hash: "abc123".into(),
            content: "## Rotation protocol\nThe quasar token rotates nightly.\nRun the verifier.".into(),
            document_type: DocumentType::Architecture,
        };
        let request = CandidateRequest::new(
            "quasar token rotation protocol",
            vec!["quasar".into(), "token".into(), "rotation".into(), "protocol".into()],
            10,
        );
        let stream = indexed_document_stream(&request, &[section]);
        assert_eq!(stream.candidates.len(), 1);
        let candidate = &stream.candidates[0];
        assert_eq!(candidate.authority, RetrievalAuthority::Heuristic);
        assert_eq!(candidate.result.line_range, Some(LineRange { start: 3, end: 5 }));
        assert!(candidate
            .result
            .evidence
            .iter()
            .any(|value| value == "document heading path: Runtime > Rotation protocol"));
    }
}
''',
)

# --- CLI full indexing persists code + document corpus atomically in SQLite. ---
replace_once(
    CLI_INDEX,
    '''            "writing {} files, {} symbols, {} chunks, {} occurrences, {} analysis facts",\n            snapshot.files.len(),\n            snapshot.symbols.len(),\n            snapshot.chunks.len(),\n            snapshot.occurrences.len(),\n            snapshot.analysis_facts.len()''',
    '''            "writing {} files, {} symbols, {} chunks, {} document sections, {} occurrences, {} analysis facts",\n            snapshot.files.len(),\n            snapshot.symbols.len(),\n            snapshot.chunks.len(),\n            snapshot.document_sections.len(),\n            snapshot.occurrences.len(),\n            snapshot.analysis_facts.len()''',
)
replace_once(
    CLI_INDEX,
    '''    store.replace_index(IndexData {\n        manifest: &snapshot.manifest,\n        files: &snapshot.files,\n        symbols: &snapshot.symbols,\n        chunks: &snapshot.chunks,\n        tests: &snapshot.tests,\n        imports: &snapshot.imports,\n        occurrences: &snapshot.occurrences,\n        analysis_facts: &snapshot.analysis_facts,\n        scopes: &snapshot.scopes,\n        bindings: &snapshot.bindings,\n        call_sites: &snapshot.call_sites,\n    })?;''',
    '''    store.replace_index_with_documents(\n        IndexData {\n            manifest: &snapshot.manifest,\n            files: &snapshot.files,\n            symbols: &snapshot.symbols,\n            chunks: &snapshot.chunks,\n            tests: &snapshot.tests,\n            imports: &snapshot.imports,\n            occurrences: &snapshot.occurrences,\n            analysis_facts: &snapshot.analysis_facts,\n            scopes: &snapshot.scopes,\n            bindings: &snapshot.bindings,\n            call_sites: &snapshot.call_sites,\n        },\n        &snapshot.document_sections,\n    )?;''',
)

# Full integration acceptance: fast/balanced/full all index -> persist -> retrieve same doc range.
replace_once(
    CLI_LIB,
    '''    fn resolve_repo_uses_global_path_when_command_path_is_default() {\n        assert_eq!(\n            resolve_repo(Path::new("/tmp/open-kioku-global"), PathBuf::from(".")),\n            PathBuf::from("/tmp/open-kioku-global")\n        );\n    }\n}''',
    '''    fn resolve_repo_uses_global_path_when_command_path_is_default() {\n        assert_eq!(\n            resolve_repo(Path::new("/tmp/open-kioku-global"), PathBuf::from(".")),\n            PathBuf::from("/tmp/open-kioku-global")\n        );\n    }\n\n    #[test]\n    fn document_corpus_mode_benchmark_preserves_context_provenance() {\n        let temp = tempfile::tempdir().unwrap();\n        let repo = temp.path();\n        fs::create_dir_all(repo.join("src")).unwrap();\n        fs::create_dir_all(repo.join("docs")).unwrap();\n        fs::write(repo.join("src/lib.rs"), "pub fn live() {}\\n").unwrap();\n        fs::write(\n            repo.join("docs/architecture.md"),\n            "# Runtime\\nintro\\n## Rotation protocol\\nThe quasar token rotates nightly.\\nRun the verifier.\\n## Failure\\nEscalate safely.\\n",\n        )\n        .unwrap();\n        OkConfig::write_default(repo.join("ok.toml")).unwrap();\n\n        for mode in [IndexMode::Fast, IndexMode::Balanced, IndexMode::Full] {\n            let mut config = OkConfig::load_from_repo(repo).unwrap();\n            config.history.enabled = false;\n            config.scip.enabled = false;\n            config.semantic.enabled = false;\n            let snapshot = index_repo_with_config(repo, config, mode).unwrap();\n            assert!(!snapshot\n                .files\n                .iter()\n                .any(|file| file.path == Path::new("docs/architecture.md")));\n            let report = snapshot\n                .phase_reports\n                .iter()\n                .find(|report| report.phase == "document_corpus")\n                .unwrap();\n            assert_eq!(report.document_files, Some(1));\n            assert!(report.document_sections.is_some_and(|count| count >= 3));\n            assert!(report.duration_ms.is_some());\n\n            let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();\n            let section = store\n                .document_sections()\n                .unwrap()\n                .into_iter()\n                .find(|section| {\n                    section.path == Path::new("docs/architecture.md")\n                        && section.heading_path == ["Runtime", "Rotation protocol"]\n                })\n                .unwrap();\n            assert_eq!(\n                section.line_range,\n                open_kioku_core::LineRange { start: 3, end: 5 }\n            );\n            assert!(!section.content_hash.is_empty());\n            assert!(store\n                .get_file_by_path(Path::new("docs/architecture.md"))\n                .unwrap()\n                .is_none());\n\n            let pack = build_context_pack(\n                repo,\n                &store,\n                "Find the quasar token rotation protocol in the architecture documentation",\n                10,\n            )\n            .unwrap();\n            let result = pack\n                .primary_files\n                .iter()\n                .find(|result| result.path == Path::new("docs/architecture.md"))\n                .unwrap();\n            assert_eq!(\n                result.line_range,\n                Some(open_kioku_core::LineRange { start: 3, end: 5 })\n            );\n            assert!(result.evidence.iter().any(|value| {\n                value == "document heading path: Runtime > Rotation protocol"\n            }));\n        }\n    }\n}''',
)

# --- Watch incremental path: only changed document paths are rewritten. ---
replace_once(
    WATCH,
    'classify_file_changes, partial_index_supported, GraphStore, HistoryStore, IndexChangeKind,',
    'changed_document_paths, classify_file_changes, partial_index_supported, GraphStore, HistoryStore, IndexChangeKind,',
)
replace_once(
    WATCH,
    '''    let previous_manifest = store.manifest()?;\n    let previous_files = store.list_files(usize::MAX, 0)?;''',
    '''    let previous_manifest = store.manifest()?;\n    let previous_files = store.list_files(usize::MAX, 0)?;\n    let previous_documents = store.document_sections()?;''',
)
replace_once(
    WATCH,
    '''    } else {\n        persist_full_snapshot(&store, &snapshot)?;\n    }\n    store.put_history_snapshot(&history)?;''',
    '''    } else {\n        persist_full_snapshot(&store, &snapshot)?;\n    }\n    if partial {\n        let changed_documents = changed_document_paths(\n            &previous_documents,\n            &snapshot.document_sections,\n        )\n        .into_iter()\n        .collect::<Vec<_>>();\n        if !changed_documents.is_empty() {\n            store.replace_document_sections_for_paths(\n                &changed_documents,\n                &snapshot.document_sections,\n            )?;\n        }\n    }\n    store.put_history_snapshot(&history)?;''',
)
# Avoid rebuilding code search index for a document-only partial update.
replace_once(
    WATCH,
    '''    rebuild_disk_index(\n        default_index_dir(root),\n        &snapshot.chunks,\n        &snapshot.files,\n        &snapshot.symbols,\n    )?;\n\n    Ok(WatchIndexStatus {''',
    '''    if !partial || changed_file_count > 0 || deleted_file_count > 0 {\n        rebuild_disk_index(\n            default_index_dir(root),\n            &snapshot.chunks,\n            &snapshot.files,\n            &snapshot.symbols,\n        )?;\n    }\n\n    Ok(WatchIndexStatus {''',
)
replace_once(
    WATCH,
    '''    store.replace_index(IndexData {\n        manifest: &snapshot.manifest,\n        files: &snapshot.files,\n        symbols: &snapshot.symbols,\n        chunks: &snapshot.chunks,\n        tests: &snapshot.tests,\n        imports: &snapshot.imports,\n        occurrences: &snapshot.occurrences,\n        analysis_facts: &snapshot.analysis_facts,\n        scopes: &[],\n        bindings: &[],\n        call_sites: &[],\n    })''',
    '''    store.replace_index_with_documents(\n        IndexData {\n            manifest: &snapshot.manifest,\n            files: &snapshot.files,\n            symbols: &snapshot.symbols,\n            chunks: &snapshot.chunks,\n            tests: &snapshot.tests,\n            imports: &snapshot.imports,\n            occurrences: &snapshot.occurrences,\n            analysis_facts: &snapshot.analysis_facts,\n            scopes: &[],\n            bindings: &[],\n            call_sites: &[],\n        },\n        &snapshot.document_sections,\n    )''',
)

# Safety assertions: product patch must not introduce temporary workflow references into Rust.
for product in [CORE, STORAGE, SQLITE, INGEST, CONTEXT, CLI_INDEX, CLI_LIB, WATCH]:
    text = read(product)
    if 'agent_cc21_document_corpus' in text or 'agent-cc21-document-corpus' in text:
        raise SystemExit(f'{product}: staging artifact leaked into product source')

print('CC2.1 document corpus patch applied')
