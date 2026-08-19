use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

fn semantic_config() -> SemanticConfig {
    SemanticConfig {
        enabled: true,
        backend: "usearch-hnsw-f32".into(),
        provider: "local".into(),
        model: "local-hash".into(),
        dimensions: 64,
        distance: "cosine".into(),
        batch_size: 16,
        ann_min_rows: 1,
        index_symbols: false,
        index_chunks: true,
        index_docs: false,
        index_memory: false,
        external_provider_allowed: false,
    }
}

fn persist_snapshot(repo: &Path, store: &SqliteStore, commit: &str, text: &str, hash: &str) {
    let repository_id = RepositoryId("repo".into());
    let file = File {
        id: FileId("file_status".into()),
        repository_id: repository_id.clone(),
        path: PathBuf::from("src/status.rs"),
        language: Language::Rust,
        size_bytes: text.len() as u64,
        content_hash: hash.into(),
        is_generated: false,
        is_vendor: false,
    };
    let chunk = CodeChunk {
        id: "chunk:file_status".into(),
        file_id: file.id.clone(),
        range: LineRange::single(1),
        language: Language::Rust,
        text: text.into(),
        symbol_id: None,
    };
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: repository_id,
            name: "repo".into(),
            root: repo.to_path_buf(),
            branch: Some("main".into()),
            commit: Some(commit.into()),
            indexed_at: Some(Utc::now()),
        },
        file_count: 1,
        symbol_count: 0,
        chunk_count: 1,
        indexed_at: Utc::now(),
        schema_version: 1,
        index_mode: Default::default(),
        phase_reports: Vec::new(),
        quality: Default::default(),
    };

    store
        .replace_index(IndexData {
            manifest: &manifest,
            files: &[file],
            symbols: &[],
            chunks: &[chunk],
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
fn lifecycle_status_distinguishes_authoritative_generation_staleness() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();

    persist_snapshot(
        repo,
        &store,
        "generation-a",
        "pub fn alpha_status_token() {}",
        "hash-a",
    );
    let manager = SemanticIndexManager::new(repo, &store, &config);
    let indexed = manager.index().unwrap().status;
    assert!(indexed.ready);
    assert!(indexed.ann_active);
    assert!(indexed.ann_profile.is_some());
    assert_eq!(indexed.state, "ready");
    assert_eq!(indexed.vector_count, 1);
    assert_eq!(indexed.backend, "usearch-hnsw-f32");
    assert_eq!(indexed.model, "local-hash");

    persist_snapshot(
        repo,
        &store,
        "generation-b",
        "pub fn beta_status_token() {}",
        "hash-b",
    );

    let stale = manager.status();
    assert_eq!(stale.state, "stale");
    assert!(stale.stale);
    assert!(!stale.ready);
    assert!(!stale.corrupt);
    assert!(!stale.ann_active);
    assert_eq!(stale.vector_count, 1);
    assert!(stale.notes.iter().any(|note| {
        note.contains("stale for the current authoritative index generation")
            && note.contains("rebuild semantic index")
    }));
}

#[test]
fn lifecycle_status_distinguishes_semantic_profile_incompatibility() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();

    persist_snapshot(
        repo,
        &store,
        "generation-a",
        "pub fn profile_status_token() {}",
        "profile-hash",
    );
    let manager = SemanticIndexManager::new(repo, &store, &config);
    assert!(manager.index().unwrap().status.ready);

    let mut incompatible = config.clone();
    incompatible.dimensions = 32;
    let mismatched = SemanticIndexManager::new(repo, &store, &incompatible).status();

    assert_eq!(mismatched.state, "stale");
    assert!(mismatched.stale);
    assert!(!mismatched.ready);
    assert!(!mismatched.corrupt);
    assert!(!mismatched.ann_active);
    assert!(mismatched
        .notes
        .iter()
        .any(|note| note.contains("manifest is stale for the current semantic config")));
    assert!(!mismatched
        .notes
        .iter()
        .any(|note| note.contains("authoritative index generation")));
}
