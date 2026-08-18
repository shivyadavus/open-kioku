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

fn persist_snapshot(repo: &Path, store: &SqliteStore, text: &str, content_hash: &str) {
    let repository_id = RepositoryId("repo".into());
    let file = File {
        id: FileId("file_auth".into()),
        repository_id: repository_id.clone(),
        path: PathBuf::from("src/auth.rs"),
        language: Language::Rust,
        size_bytes: text.len() as u64,
        content_hash: content_hash.into(),
        is_generated: false,
        is_vendor: false,
    };
    let chunk = CodeChunk {
        id: "chunk:file_auth".into(),
        file_id: file.id.clone(),
        range: LineRange::single(1),
        language: Language::Rust,
        text: text.into(),
        symbol_id: None,
    };
    // Keep the Git commit deliberately unchanged. `ok watch` indexes uncommitted working-tree
    // edits, so semantic freshness must follow the authoritative index generation rather than
    // relying on source_commit alone.
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: repository_id,
            name: "repo".into(),
            root: repo.to_path_buf(),
            branch: Some("main".into()),
            commit: Some("same-commit".into()),
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
fn authoritative_reindex_invalidates_semantic_generation_until_rebuilt() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();

    persist_snapshot(
        repo,
        &store,
        "pub fn alpha_working_tree_token() { alpha_working_tree_token(); }",
        "auth-v1",
    );
    let manager = SemanticIndexManager::new(repo, &store, &config);
    let first = manager.index().unwrap();
    assert!(first.status.ready);
    assert!(first.status.ann_active);
    assert!(manager
        .search("alpha working tree token", 10)
        .unwrap()
        .iter()
        .any(|result| result.path == Path::new("src/auth.rs")));

    persist_snapshot(
        repo,
        &store,
        "pub fn beta_working_tree_token() { beta_working_tree_token(); }",
        "auth-v2",
    );

    let stale = manager.status();
    assert!(!stale.ready);
    assert!(stale.stale);
    assert!(!stale.ann_active);
    assert_eq!(stale.state, "stale");
    assert!(stale
        .notes
        .iter()
        .any(|note| note.contains("authoritative index generation")));
    let err = manager.search("alpha working tree token", 10).unwrap_err();
    assert!(err.to_string().contains("semantic index is stale"));

    let rebuilt = manager.index().unwrap();
    assert!(rebuilt.status.ready);
    assert!(rebuilt.status.ann_active);
    assert!(manager
        .search("beta working tree token", 10)
        .unwrap()
        .iter()
        .any(|result| result.path == Path::new("src/auth.rs")));
}
