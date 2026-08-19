use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::fs;
use std::path::{Path, PathBuf};

fn semantic_config() -> SemanticConfig {
    SemanticConfig {
        enabled: true,
        backend: "auto".into(),
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

fn persist_snapshot(repo: &Path, store: &SqliteStore) {
    let repository_id = RepositoryId("repo".into());
    let file = File {
        id: FileId("file_missing_ann_recovery".into()),
        repository_id: repository_id.clone(),
        path: PathBuf::from("src/recovery.rs"),
        language: Language::Rust,
        size_bytes: 54,
        content_hash: "missing-ann-recovery-v1".into(),
        is_generated: false,
        is_vendor: false,
    };
    let chunk = CodeChunk {
        id: "chunk:file_missing_ann_recovery".into(),
        file_id: file.id.clone(),
        range: LineRange::single(1),
        language: Language::Rust,
        text: "pub fn missing_ann_recovery_token() {}".into(),
        symbol_id: None,
    };
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: repository_id,
            name: "repo".into(),
            root: repo.to_path_buf(),
            branch: Some("main".into()),
            commit: Some("generation-a".into()),
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
fn restart_refuses_previous_generation_when_ann_artifact_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    persist_snapshot(repo, &store);
    let config = semantic_config();

    let manager = SemanticIndexManager::new(repo, &store, &config);
    let report = manager.index().unwrap();
    assert!(report.status.ready);
    assert!(report.status.ann_active);

    let vectors = repo.join(".ok/vectors");
    let current = vectors.join("current");
    let previous = vectors.join("previous");
    fs::rename(&current, &previous).unwrap();
    fs::remove_file(previous.join("index.usearch")).unwrap();
    drop(manager);

    let restarted = SemanticIndexManager::new(repo, &store, &config);
    let status = restarted.status();

    assert_eq!(status.state, "missing");
    assert!(!status.ready);
    assert!(!status.ann_active);
    assert!(!current.exists());
    assert!(previous.exists());
    assert!(!previous.join("index.usearch").exists());
    assert!(status
        .notes
        .iter()
        .any(|note| note.contains("previous semantic generation is incomplete")));
    assert!(!status
        .notes
        .iter()
        .any(|note| note.contains("recovered previous semantic generation")));

    let error = restarted
        .search("missing_ann_recovery_token", 5)
        .unwrap_err();
    assert!(error.to_string().contains("semantic index is missing"));
}
