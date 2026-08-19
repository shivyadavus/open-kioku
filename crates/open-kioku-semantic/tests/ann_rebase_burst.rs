use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct FixtureFile {
    id: String,
    path: String,
    text: String,
    content_hash: String,
}

fn semantic_config(backend: &str) -> SemanticConfig {
    SemanticConfig {
        enabled: true,
        backend: backend.into(),
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

fn fixture(id: usize, path: impl Into<String>, token: impl Into<String>) -> FixtureFile {
    let token = token.into();
    FixtureFile {
        id: format!("file_{id}"),
        path: path.into(),
        text: format!("pub fn {token}() {{ {token}(); }}"),
        content_hash: format!("hash:{id}:{token}"),
    }
}

fn persist_snapshot(repo: &Path, store: &SqliteStore, commit: &str, fixtures: &[FixtureFile]) {
    let repository_id = RepositoryId("repo".into());
    let files = fixtures
        .iter()
        .map(|fixture| File {
            id: FileId(fixture.id.clone()),
            repository_id: repository_id.clone(),
            path: PathBuf::from(&fixture.path),
            language: Language::Rust,
            size_bytes: fixture.text.len() as u64,
            content_hash: fixture.content_hash.clone(),
            is_generated: false,
            is_vendor: false,
        })
        .collect::<Vec<_>>();
    let chunks = fixtures
        .iter()
        .map(|fixture| CodeChunk {
            id: format!("chunk:{}", fixture.id),
            file_id: FileId(fixture.id.clone()),
            range: LineRange::single(1),
            language: Language::Rust,
            text: fixture.text.clone(),
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

fn result_paths(manager: &SemanticIndexManager<'_>, query: &str, limit: usize) -> Vec<PathBuf> {
    manager
        .search(query, limit)
        .unwrap()
        .into_iter()
        .map(|result| result.path)
        .collect()
}

fn persisted_generation_paths(current_dir: &Path) -> BTreeSet<PathBuf> {
    let targets: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(current_dir.join("ids.json")).unwrap()).unwrap();
    targets
        .into_iter()
        .map(|target| {
            PathBuf::from(
                target
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .expect("semantic target must persist a path"),
            )
        })
        .collect()
}

#[test]
fn ann_large_rebase_burst_matches_exact_flat_and_drops_stale_identities() {
    let ann_temp = tempfile::tempdir().unwrap();
    let exact_temp = tempfile::tempdir().unwrap();
    let ann_repo = ann_temp.path();
    let exact_repo = exact_temp.path();
    let ann_store = SqliteStore::open(ann_repo.join(".ok/index.sqlite")).unwrap();
    let exact_store = SqliteStore::open(exact_repo.join(".ok/index.sqlite")).unwrap();
    let ann_config = semantic_config("usearch-hnsw-f32");
    let exact_config = semantic_config("exact-flat");
    let ann = SemanticIndexManager::new(ann_repo, &ann_store, &ann_config);
    let exact = SemanticIndexManager::new(exact_repo, &exact_store, &exact_config);

    let initial = (0..8)
        .map(|id| {
            fixture(
                id,
                format!("src/module_{id}.rs"),
                format!("baseline_token_{id}"),
            )
        })
        .collect::<Vec<_>>();
    persist_snapshot(ann_repo, &ann_store, "before-rebase", &initial);
    persist_snapshot(exact_repo, &exact_store, "before-rebase", &initial);
    assert_eq!(ann.index().unwrap().status.vector_count, 8);
    assert_eq!(exact.index().unwrap().status.vector_count, 8);

    let mut rebased = vec![
        initial[0].clone(),
        initial[1].clone(),
        fixture(2, "src/module_2.rs", "rebased_auth_guard"),
        fixture(3, "src/module_3.rs", "rebased_invoice_total"),
        fixture(4, "src/module_4.rs", "rebased_session_refresh"),
        fixture(5, "src/module_5.rs", "rebased_policy_check"),
        fixture(6, "src/security/module_6.rs", "baseline_token_6"),
    ];
    rebased.extend((8..13).map(|id| {
        fixture(
            id,
            format!("src/new_module_{id}.rs"),
            format!("rebase_addition_token_{id}"),
        )
    }));

    persist_snapshot(ann_repo, &ann_store, "after-rebase", &rebased);
    persist_snapshot(exact_repo, &exact_store, "after-rebase", &rebased);

    let stale = ann.status();
    assert!(stale.stale);
    assert!(!stale.ready);
    assert!(!stale.ann_active);
    assert!(ann.search("rebased auth guard", 10).is_err());

    let ann_report = ann.index().unwrap();
    let exact_report = exact.index().unwrap();
    assert!(ann_report.status.ready);
    assert!(ann_report.status.ann_active);
    assert_eq!(ann_report.status.vector_count, rebased.len());
    assert_eq!(exact_report.status.vector_count, rebased.len());
    assert_eq!(ann_report.reused_embeddings, 2);
    assert_eq!(ann_report.embedded_count, rebased.len() - 2);

    let authoritative_paths = rebased
        .iter()
        .map(|fixture| PathBuf::from(&fixture.path))
        .collect::<BTreeSet<_>>();
    for query in [
        "rebased auth guard",
        "rebased invoice total",
        "baseline token 6",
        "rebase addition token 12",
    ] {
        let ann_paths = result_paths(&ann, query, rebased.len());
        let exact_paths = result_paths(&exact, query, rebased.len());
        assert_eq!(ann_paths.first(), exact_paths.first(), "query {query:?}");
        assert!(ann_paths
            .iter()
            .all(|path| authoritative_paths.contains(path)));
    }

    let persisted_paths = persisted_generation_paths(&ann_report.status.current_dir);
    assert_eq!(persisted_paths.len(), rebased.len());
    assert_eq!(persisted_paths, authoritative_paths);
    assert!(!persisted_paths.contains(&PathBuf::from("src/module_6.rs")));
    assert!(!persisted_paths.contains(&PathBuf::from("src/module_7.rs")));

    drop(ann);
    drop(ann_store);
    let restarted_store = SqliteStore::open(ann_repo.join(".ok/index.sqlite")).unwrap();
    let restarted = SemanticIndexManager::new(ann_repo, &restarted_store, &ann_config);
    let restarted_status = restarted.status();
    assert!(restarted_status.ready);
    assert!(restarted_status.ann_active);
    assert_eq!(restarted_status.vector_count, rebased.len());
    assert_eq!(
        persisted_generation_paths(&restarted_status.current_dir),
        authoritative_paths
    );
}
