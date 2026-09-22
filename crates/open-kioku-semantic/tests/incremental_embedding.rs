use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::{SemanticIndexManager, SemanticIndexProgress};
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

fn semantic_config() -> SemanticConfig {
    SemanticConfig {
        enabled: true,
        backend: "exact-flat".into(),
        provider: "local".into(),
        model: "local-hash".into(),
        dimensions: 64,
        distance: "cosine".into(),
        batch_size: 1,
        ann_min_rows: 10_000,
        index_symbols: false,
        index_chunks: true,
        index_docs: false,
        index_memory: false,
        external_provider_allowed: false,
    }
}

/// A source file as the index stores it: one chunk per entry of `chunks`.
struct Source {
    id: &'static str,
    path: &'static str,
    chunks: Vec<&'static str>,
}

fn auth(chunks: Vec<&'static str>) -> Source {
    Source {
        id: "file_auth",
        path: "src/auth.rs",
        chunks,
    }
}

fn billing() -> Source {
    Source {
        id: "file_billing",
        path: "src/billing.rs",
        chunks: vec!["pub fn issue_invoice() { billing invoice }"],
    }
}

fn persist(repo: &Path, store: &SqliteStore, sources: &[Source]) {
    let repository_id = RepositoryId("repo".into());
    let mut files = Vec::new();
    let mut chunks = Vec::new();
    for source in sources {
        let content = source.chunks.join("\n");
        let file = File {
            id: FileId(source.id.into()),
            repository_id: repository_id.clone(),
            path: PathBuf::from(source.path),
            language: Language::Rust,
            size_bytes: content.len() as u64,
            content_hash: content,
            is_generated: false,
            is_vendor: false,
        };
        for (line, text) in source.chunks.iter().enumerate() {
            chunks.push(CodeChunk {
                id: format!("chunk:{}:{}", source.id, line + 1),
                file_id: file.id.clone(),
                range: LineRange::single(line as u32 + 1),
                language: Language::Rust,
                text: (*text).into(),
                symbol_id: None,
            });
        }
        files.push(file);
    }
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: repository_id,
            name: "repo".into(),
            root: repo.to_path_buf(),
            branch: Some("main".into()),
            commit: Some("abc".into()),
            indexed_at: Some(Utc::now()),
        },
        file_count: files.len(),
        symbol_count: 0,
        chunk_count: chunks.len(),
        indexed_at: Utc::now(),
        schema_version: 1,
        index_mode: Default::default(),
        phase_reports: Vec::new(),
        quality: Default::default(),
    };
    store
        .replace_index(IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &[],
            chunks: &chunks,
            tests: &[],
            imports: &[],
            occurrences: &[],
            analysis_facts: &[],
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        })
        .unwrap();
}

#[test]
fn unchanged_sources_embed_nothing_and_a_changed_file_embeds_only_its_chunks() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();
    let manager = SemanticIndexManager::new(repo, &store, &config);

    persist(
        repo,
        &store,
        &[
            auth(vec![
                "pub fn issue_token() { create session token }",
                "pub fn validate_token() { check session token }",
            ]),
            billing(),
        ],
    );
    let first = manager.index().unwrap();
    assert_eq!(first.indexed_count, 3);
    assert_eq!(first.embedded_count, 3);
    assert_eq!(first.reused_embeddings, 0);

    let unchanged = manager.index().unwrap();
    assert_eq!(unchanged.indexed_count, 3);
    assert_eq!(unchanged.embedded_count, 0);
    assert_eq!(unchanged.reused_embeddings, 3);

    persist(
        repo,
        &store,
        &[
            auth(vec![
                "pub fn issue_token() { create refresh token }",
                "pub fn validate_token() { check refresh token }",
            ]),
            billing(),
        ],
    );
    let mut progress = Vec::new();
    let changed = manager
        .index_with_progress(false, &mut |update| progress.push(update))
        .unwrap();
    // Both auth chunks changed and billing did not: exactly the auth chunks are embedded.
    assert_eq!(changed.indexed_count, 3);
    assert_eq!(changed.embedded_count, 2);
    assert_eq!(changed.reused_embeddings, 1);
    assert!(progress.iter().all(|update| update.to_embed == 2));
    assert!(manager.search("refresh token", 3).unwrap()[0]
        .path
        .ends_with("src/auth.rs"));
}

#[test]
fn index_progress_reports_each_embedded_batch_and_a_fully_reused_build_once() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();
    let manager = SemanticIndexManager::new(repo, &store, &config);
    persist(
        repo,
        &store,
        &[
            auth(vec![
                "pub fn issue_token() { create session token }",
                "pub fn validate_token() { check session token }",
            ]),
            billing(),
        ],
    );

    let mut built = Vec::new();
    manager
        .rebuild_with_progress(false, &mut |update| built.push(update))
        .unwrap();
    // batch_size = 1: a start report, then one per embedded target.
    assert_eq!(
        built
            .iter()
            .map(|update| update.embedded)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert!(built
        .iter()
        .all(|update| update.to_embed == 3 && update.reused == 0 && update.total_targets == 3));

    let mut reused = Vec::new();
    manager
        .index_with_progress(false, &mut |update| reused.push(update))
        .unwrap();
    assert_eq!(
        reused,
        vec![SemanticIndexProgress {
            embedded: 0,
            to_embed: 0,
            reused: 3,
            total_targets: 3,
        }]
    );
}
