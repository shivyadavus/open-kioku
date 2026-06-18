use chrono::{Duration, Utc};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use open_kioku_core::{
    Confidence, EvidenceSourceType, File, FileId, GitChangeKind, GitCommitId, GitCommitRecord,
    GitFileTouch, GitSymbolTouch, HistoryRecordId, HistorySnapshot, IndexManifest, IndexQuality,
    Language, LineRange, Owner, Repository, RepositoryId, Symbol, SymbolId, SymbolKind,
    HISTORY_SCHEMA_VERSION,
};
use open_kioku_storage::{HistoryStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;

fn provenance_lookup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("index.sqlite")).unwrap();
    let file = File {
        id: FileId::new("file"),
        repository_id: RepositoryId::new("repo"),
        path: "src/history.rs".into(),
        language: Language::Rust,
        size_bytes: 100,
        content_hash: "hash".into(),
        is_generated: false,
        is_vendor: false,
    };
    let symbol = Symbol {
        id: SymbolId::new("symbol"),
        name: "history_for_file".into(),
        qualified_name: "crate::history_for_file".into(),
        kind: SymbolKind::Function,
        file_id: file.id.clone(),
        range: Some(LineRange { start: 10, end: 30 }),
        language: Language::Rust,
        confidence: Confidence::High,
        provenance: EvidenceSourceType::TreeSitter,
    };
    let manifest = IndexManifest {
        repository: Repository {
            id: RepositoryId::new("repo"),
            name: "bench".into(),
            root: dir.path().into(),
            branch: Some("main".into()),
            commit: Some("commit-0".into()),
            indexed_at: Some(Utc::now()),
        },
        file_count: 1,
        symbol_count: 1,
        chunk_count: 0,
        indexed_at: Utc::now(),
        schema_version: 1,
        index_mode: Default::default(),
        phase_reports: Vec::new(),
        quality: IndexQuality::default(),
    };
    store
        .replace_index(IndexData {
            manifest: &manifest,
            files: std::slice::from_ref(&file),
            symbols: std::slice::from_ref(&symbol),
            chunks: &[],
            tests: &[],
            imports: &[],
            occurrences: &[],
            analysis_facts: &[],
        })
        .unwrap();

    let now = Utc::now();
    let mut commits = Vec::new();
    let mut file_touches = Vec::new();
    let mut symbol_touches = Vec::new();
    for index in 0..500 {
        let id = GitCommitId::new(format!("commit-{index}"));
        let at = now - Duration::minutes(index as i64);
        commits.push(GitCommitRecord {
            id: id.clone(),
            parent_ids: Vec::new(),
            author: Owner {
                name: "Benchmark".into(),
                email: Some("bench@example.com".into()),
            },
            committer: None,
            authored_at: at,
            committed_at: at,
            summary: format!("touch {index}"),
            message: format!("touch {index}"),
            file_count: 1,
        });
        file_touches.push(GitFileTouch {
            id: HistoryRecordId::new(format!("file-{index}")),
            commit_id: id.clone(),
            path: file.path.clone(),
            previous_path: None,
            change_kind: GitChangeKind::Modified,
            additions: None,
            deletions: None,
            touched_at: at,
        });
        symbol_touches.push(GitSymbolTouch {
            id: HistoryRecordId::new(format!("symbol-{index}")),
            commit_id: id,
            symbol_id: Some(symbol.id.clone()),
            qualified_name: symbol.qualified_name.clone(),
            file_path: file.path.clone(),
            change_kind: GitChangeKind::Modified,
            line_ranges: vec![LineRange { start: 12, end: 12 }],
            confidence: Confidence::Medium,
            uncertainty: vec!["bounded benchmark history".into()],
            touched_at: at,
        });
    }
    store
        .put_history_snapshot(&HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits,
            file_touches,
            symbol_touches,
            cochange_edges: Vec::new(),
            reviewer_evidence: Vec::new(),
        })
        .unwrap();

    c.bench_function("provenance_for_path_500_commits", |b| {
        b.iter(|| {
            black_box(
                store
                    .provenance_for_path(std::path::Path::new("src/history.rs"), 20)
                    .unwrap(),
            )
        })
    });
    c.bench_function("provenance_for_symbol_500_commits", |b| {
        b.iter(|| black_box(store.provenance_for_symbol(&symbol.id, 20).unwrap()))
    });
}

criterion_group!(benches, provenance_lookup);
criterion_main!(benches);
