use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
struct FixtureFile<'a> {
    id: &'a str,
    path: &'a str,
    text: &'a str,
    content_hash: &'a str,
}

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

fn persist_snapshot(
    repo: &Path,
    store: &SqliteStore,
    commit: &str,
    fixtures: &[FixtureFile<'_>],
) {
    let repository_id = RepositoryId("repo".into());
    let files = fixtures
        .iter()
        .map(|fixture| File {
            id: FileId(fixture.id.into()),
            repository_id: repository_id.clone(),
            path: PathBuf::from(fixture.path),
            language: Language::Rust,
            size_bytes: fixture.text.len() as u64,
            content_hash: fixture.content_hash.into(),
            is_generated: false,
            is_vendor: false,
        })
        .collect::<Vec<_>>();
    let chunks = fixtures
        .iter()
        .map(|fixture| CodeChunk {
            id: format!("chunk:{}", fixture.id),
            file_id: FileId(fixture.id.into()),
            range: LineRange::single(1),
            language: Language::Rust,
            text: fixture.text.into(),
            symbol_id: None,
        })
        .collect::<Vec<_>>();
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

fn assert_search_path(manager: &SemanticIndexManager<'_>, query: &str, expected: &str) {
    let results = manager.search(query, 10).unwrap();
    assert!(
        results.iter().any(|result| result.path == Path::new(expected)),
        "expected semantic search for {query:?} to include {expected:?}, got {results:?}"
    );
}

#[test]
fn ann_generation_stays_clean_across_add_update_rename_delete_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config();
    let manager = SemanticIndexManager::new(repo, &store, &config);

    persist_snapshot(
        repo,
        &store,
        "c1",
        &[FixtureFile {
            id: "file_auth",
            path: "src/auth.rs",
            text: "pub fn alpha_issue_token() { alpha_issue_token(); }",
            content_hash: "auth-v1",
        }],
    );
    let initial = manager.index().unwrap();
    assert!(initial.status.ann_active);
    assert_eq!(initial.status.vector_count, 1);
    assert_eq!(initial.embedded_count, 1);
    assert_search_path(&manager, "alpha issue token", "src/auth.rs");

    persist_snapshot(
        repo,
        &store,
        "c2",
        &[
            FixtureFile {
                id: "file_auth",
                path: "src/auth.rs",
                text: "pub fn beta_refresh_token() { beta_refresh_token(); }",
                content_hash: "auth-v2",
            },
            FixtureFile {
                id: "file_billing",
                path: "src/billing.rs",
                text: "pub fn gamma_invoice_total() { gamma_invoice_total(); }",
                content_hash: "billing-v1",
            },
        ],
    );
    let updated = manager.index().unwrap();
    assert!(updated.status.ann_active);
    assert_eq!(updated.status.vector_count, 2);
    assert_eq!(updated.indexed_count, 2);
    assert_eq!(updated.embedded_count, 2);
    assert_search_path(&manager, "beta refresh token", "src/auth.rs");
    assert_search_path(&manager, "gamma invoice total", "src/billing.rs");

    persist_snapshot(
        repo,
        &store,
        "c3",
        &[
            FixtureFile {
                id: "file_auth",
                path: "src/security/auth.rs",
                text: "pub fn beta_refresh_token() { beta_refresh_token(); }",
                content_hash: "auth-v2",
            },
            FixtureFile {
                id: "file_billing",
                path: "src/billing.rs",
                text: "pub fn gamma_invoice_total() { gamma_invoice_total(); }",
                content_hash: "billing-v1",
            },
        ],
    );
    let renamed = manager.index().unwrap();
    assert_eq!(renamed.status.vector_count, 2);
    assert_eq!(renamed.reused_embeddings, 2);
    let renamed_results = manager.search("beta refresh token", 10).unwrap();
    assert!(renamed_results
        .iter()
        .any(|result| result.path == Path::new("src/security/auth.rs")));
    assert!(!renamed_results
        .iter()
        .any(|result| result.path == Path::new("src/auth.rs")));

    persist_snapshot(
        repo,
        &store,
        "c4",
        &[FixtureFile {
            id: "file_billing",
            path: "src/billing.rs",
            text: "pub fn gamma_invoice_total() { gamma_invoice_total(); }",
            content_hash: "billing-v1",
        }],
    );
    let deleted = manager.index().unwrap();
    assert_eq!(deleted.status.vector_count, 1);
    assert_eq!(deleted.indexed_count, 1);
    assert_eq!(deleted.reused_embeddings, 1);
    assert_eq!(deleted.removed_count, 1);
    let deleted_results = manager.search("beta refresh token", 10).unwrap();
    assert!(!deleted_results
        .iter()
        .any(|result| result.path == Path::new("src/security/auth.rs")));
    assert_search_path(&manager, "gamma invoice total", "src/billing.rs");

    drop(manager);
    drop(store);

    let restarted_store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let restarted = SemanticIndexManager::new(repo, &restarted_store, &config);
    let status = restarted.status();
    assert!(status.ready);
    assert!(status.ann_active);
    assert_eq!(status.vector_count, 1);
    assert_search_path(&restarted, "gamma invoice total", "src/billing.rs");
    let stale_results = restarted.search("beta refresh token", 10).unwrap();
    assert!(!stale_results
        .iter()
        .any(|result| result.path == Path::new("src/security/auth.rs")));
}
