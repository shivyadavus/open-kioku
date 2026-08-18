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
        id: FileId("file_atomicity".into()),
        repository_id: repository_id.clone(),
        path: PathBuf::from("src/atomicity.rs"),
        language: Language::Rust,
        size_bytes: 64,
        content_hash: "atomicity-v1".into(),
        is_generated: false,
        is_vendor: false,
    };
    let chunk = CodeChunk {
        id: "chunk:file_atomicity".into(),
        file_id: file.id.clone(),
        range: LineRange::single(1),
        language: Language::Rust,
        text: "pub fn last_known_good_token() { last_known_good_token(); }".into(),
        symbol_id: None,
    };
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: repository_id,
            name: "repo".into(),
            root: repo.to_path_buf(),
            branch: Some("main".into()),
            commit: Some("c1".into()),
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

fn copy_dir(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).unwrap();
        }
    }
}

#[test]
fn restart_keeps_last_good_generation_when_staging_build_is_partial() {
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
    let current_manifest = fs::read(current.join("manifest.json")).unwrap();
    let interrupted = vectors.join("builds/build-interrupted");
    fs::create_dir_all(&interrupted).unwrap();
    fs::write(interrupted.join("manifest.json"), b"{incomplete generation").unwrap();
    fs::write(interrupted.join("index.usearch.part"), b"partial").unwrap();
    drop(manager);

    let restarted = SemanticIndexManager::new(repo, &store, &config);
    let status = restarted.status();
    assert!(status.ready);
    assert!(status.ann_active);
    assert_eq!(
        fs::read(current.join("manifest.json")).unwrap(),
        current_manifest
    );

    let results = restarted.search("last known good token", 10).unwrap();
    assert!(results
        .iter()
        .any(|result| result.path == Path::new("src/atomicity.rs")));
}

#[test]
fn restart_prefers_complete_current_when_previous_cleanup_was_interrupted() {
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
    copy_dir(&current, &previous);
    assert!(current.exists());
    assert!(previous.exists());
    drop(manager);

    let restarted = SemanticIndexManager::new(repo, &store, &config);
    let status = restarted.status();
    assert!(status.ready);
    assert!(status.ann_active);
    assert!(current.exists());
    assert!(previous.exists());
    assert!(!status
        .notes
        .iter()
        .any(|note| note.contains("recovered previous semantic generation")));

    let results = restarted.search("last known good token", 10).unwrap();
    assert!(results
        .iter()
        .any(|result| result.path == Path::new("src/atomicity.rs")));
}
