//! A path the index stops holding leaves the semantic vector store in the same run (#564).

use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, IndexManifest, Language, LineRange, Repository, RepositoryId,
};
use open_kioku_semantic::{prune_vector_store, SemanticIndexManager, VectorStorePrune};
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::fs;
use std::path::{Path, PathBuf};

const KEPT_TEXT: &str = "pub fn render_invoice_total() { invoice ledger total }";
const REMOVED_TEXT: &str = "pub fn quarantined_payroll_formula() { zebra_payroll_marker }";

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

fn file(id: &str, path: &str) -> File {
    File {
        id: FileId(id.into()),
        repository_id: RepositoryId("repo".into()),
        path: PathBuf::from(path),
        language: Language::Rust,
        size_bytes: 0,
        content_hash: id.into(),
        is_generated: false,
        is_vendor: false,
    }
}

fn chunk(id: &str, file: &File, text: &str) -> CodeChunk {
    CodeChunk {
        id: id.into(),
        file_id: file.id.clone(),
        range: LineRange::single(1),
        language: Language::Rust,
        text: text.into(),
        symbol_id: None,
    }
}

fn persist(repo: &Path, store: &SqliteStore, files: &[File], chunks: &[CodeChunk]) {
    let manifest = IndexManifest {
        analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
        repository: Repository {
            id: RepositoryId("repo".into()),
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
        snapshot: None,
    };
    store
        .replace_index(IndexData {
            manifest: &manifest,
            files,
            symbols: &[],
            chunks,
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

/// Every file under `.ok/vectors` whose raw bytes contain `needle`.
fn vector_files_holding(repo: &Path, needle: &str) -> Vec<PathBuf> {
    let mut holders = Vec::new();
    let mut pending = vec![repo.join(".ok/vectors")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if String::from_utf8_lossy(&fs::read(&path).unwrap()).contains(needle) {
                holders.push(path);
            }
        }
    }
    holders
}

/// Builds a store over a kept and a removed file, then republishes the index with `remaining`
/// in place of the removed one and prunes, as `ok index` does after it publishes.
fn prune_after_removal(backend: &str, remaining: Option<File>) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let config = semantic_config(backend);
    let kept = file("file_kept", "src/invoice.rs");
    let removed = file("file_removed", "src/payroll.rs");
    let kept_chunk = chunk("chunk_kept", &kept, KEPT_TEXT);
    let removed_chunk = chunk("chunk_removed", &removed, REMOVED_TEXT);
    persist(
        repo,
        &store,
        &[kept.clone(), removed.clone()],
        &[kept_chunk.clone(), removed_chunk.clone()],
    );
    let manager = SemanticIndexManager::new(repo, &store, &config);
    manager.index().unwrap();
    assert!(
        !vector_files_holding(repo, "zebra_payroll_marker").is_empty(),
        "the store holds the file's text before the removal"
    );
    // An interrupted build holds target text too.
    let interrupted = repo.join(".ok/vectors/builds/build-1");
    fs::create_dir_all(&interrupted).unwrap();
    fs::write(interrupted.join("ids.json"), REMOVED_TEXT).unwrap();

    let (files, chunks) = match remaining {
        Some(still_indexed) => (
            vec![kept.clone(), still_indexed],
            vec![kept_chunk.clone(), removed_chunk.clone()],
        ),
        None => (vec![kept.clone()], vec![kept_chunk.clone()]),
    };
    persist(repo, &store, &files, &chunks);
    let pruned = prune_vector_store(repo, &store.list_files(usize::MAX, 0).unwrap()).unwrap();

    assert_eq!(
        pruned,
        VectorStorePrune {
            removed_targets: 1,
            removed_paths: 1,
            discarded_generations: 0,
            discarded_builds: 1,
        }
    );
    assert_eq!(
        vector_files_holding(repo, "zebra_payroll_marker"),
        Vec::<PathBuf>::new(),
        "no file under .ok/vectors keeps the removed path's text"
    );
    assert_eq!(
        vector_files_holding(repo, "src/payroll.rs"),
        Vec::<PathBuf>::new()
    );
    // Pruning does not make the store current: it still describes the earlier index.
    let status = manager.status();
    assert!(!status.corrupt, "{status:?}");
    assert!(status.stale && !status.ready, "{status:?}");
    assert!(manager.search("payroll formula", 5).is_err());
    // A second pass finds nothing left to remove.
    assert!(
        prune_vector_store(repo, &store.list_files(usize::MAX, 0).unwrap())
            .unwrap()
            .is_empty()
    );

    // The kept file's embedding survives the prune, so the rebuild re-embeds nothing.
    let rebuilt = manager.index().unwrap();
    assert_eq!(rebuilt.embedded_count, 0, "{rebuilt:?}");
    assert_eq!(rebuilt.reused_embeddings, 1, "{rebuilt:?}");
    let results = manager.search("payroll formula zebra", 5).unwrap();
    assert!(
        results
            .iter()
            .all(|result| result.path == Path::new("src/invoice.rs")),
        "{results:?}"
    );
}

#[test]
fn prune_removes_a_deleted_files_text_from_an_exact_flat_store() {
    prune_after_removal("exact-flat", None);
}

#[test]
fn prune_removes_a_deleted_files_text_from_an_hnsw_store() {
    prune_after_removal("usearch-hnsw-f32", None);
}

/// Indexed but no longer embedded: here vendored, the same rule that keeps a secret-like path's
/// key material out of the corpus.
#[test]
fn prune_removes_a_path_the_semantic_corpus_now_excludes() {
    let mut vendored = file("file_removed", "src/payroll.rs");
    vendored.is_vendor = true;
    prune_after_removal("exact-flat", Some(vendored));
}

#[test]
fn prune_discards_a_generation_it_cannot_read() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let current = repo.join(".ok/vectors/current");
    fs::create_dir_all(&current).unwrap();
    fs::write(current.join("ids.json"), REMOVED_TEXT).unwrap();

    let pruned = prune_vector_store(repo, &[]).unwrap();

    assert_eq!(pruned.discarded_generations, 1);
    assert!(!current.exists());
    assert!(!repo.join(".ok/models").exists());
}

#[test]
fn prune_without_a_vector_store_creates_nothing() {
    let temp = tempfile::tempdir().unwrap();
    assert!(prune_vector_store(temp.path(), &[]).unwrap().is_empty());
    assert!(!temp.path().join(".ok").exists());
}
