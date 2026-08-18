use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_vector::PRODUCTION_HNSW_PROFILE;
use std::path::PathBuf;

#[test]
fn auto_backend_persists_hnsw_and_reports_resolved_backend_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: RepositoryId("repo".into()),
            name: "repo".into(),
            root: temp.path().to_path_buf(),
            branch: Some("main".into()),
            commit: Some("abc".into()),
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
    let files = vec![File {
        id: FileId("file_auth".into()),
        repository_id: RepositoryId("repo".into()),
        path: PathBuf::from("src/auth.rs"),
        language: Language::Rust,
        size_bytes: 32,
        content_hash: "file-hash".into(),
        is_generated: false,
        is_vendor: false,
    }];
    let chunks = vec![CodeChunk {
        id: "chunk_auth".into(),
        file_id: FileId("file_auth".into()),
        range: LineRange::single(1),
        language: Language::Rust,
        text: "pub fn issue_token() { create session token }".into(),
        symbol_id: None,
    }];
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

    let config = SemanticConfig {
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
    };

    let manager = SemanticIndexManager::new(temp.path(), &store, &config);
    let report = manager.index().unwrap();
    assert_eq!(report.status.backend, "usearch-hnsw-f32");
    assert!(report.status.ann_active);
    assert_eq!(
        report.status.ann_profile.as_deref(),
        Some(PRODUCTION_HNSW_PROFILE)
    );
    assert!(temp
        .path()
        .join(".ok/vectors/current/index.usearch")
        .is_file());
    assert!(temp
        .path()
        .join(".ok/vectors/current/index.meta.json")
        .is_file());
    assert!(!temp.path().join(".ok/vectors/current/index.json").exists());

    let restarted = SemanticIndexManager::new(temp.path(), &store, &config);
    let status = restarted.status();
    assert!(status.ready);
    assert!(status.ann_active);
    assert_eq!(status.backend, "usearch-hnsw-f32");
    assert_eq!(status.ann_profile.as_deref(), Some(PRODUCTION_HNSW_PROFILE));
    assert_eq!(
        status.manifest.as_ref().unwrap().backend,
        "usearch-hnsw-f32"
    );
    assert_eq!(
        status.manifest.as_ref().unwrap().index_version,
        PRODUCTION_HNSW_PROFILE
    );

    let results = restarted.search("issue token", 5).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].path, PathBuf::from("src/auth.rs"));
}
