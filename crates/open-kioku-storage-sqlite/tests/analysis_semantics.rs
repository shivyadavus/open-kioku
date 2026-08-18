use chrono::Utc;
use open_kioku_core::{
    AnalysisSemanticsState, FileId, IndexManifest, IndexQuality, Repository, RepositoryId, SymbolId,
};
use open_kioku_storage::{GraphStore, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;

fn manifest(root: &std::path::Path) -> IndexManifest {
    IndexManifest {
        analysis_semantics: Some(AnalysisSemanticsState::current()),
        repository: Repository {
            id: RepositoryId::new("repo"),
            name: "semantics-guard".into(),
            root: root.into(),
            branch: Some("main".into()),
            commit: Some("fixture".into()),
            indexed_at: Some(Utc::now()),
        },
        file_count: 0,
        symbol_count: 0,
        chunk_count: 0,
        indexed_at: Utc::now(),
        schema_version: 1,
        index_mode: Default::default(),
        phase_reports: Vec::new(),
        quality: IndexQuality::default(),
    }
}

#[test]
fn incompatible_semantics_block_relationship_reads_but_preserve_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("index.sqlite")).unwrap();
    store.initialize().unwrap();

    let current = manifest(dir.path());
    store.put_manifest(&current).unwrap();

    assert!(store.neighbors("missing", 10).is_ok());
    assert!(store.shortest_path("missing", "also-missing", 4).is_ok());
    assert!(store.imports().is_ok());
    assert!(store
        .implementation_facts_for_target("missing", 10)
        .is_ok());
    assert!(store
        .references_for_symbol(&SymbolId::new("missing"), 10)
        .is_ok());
    assert!(store
        .occurrences_for_file(&FileId::new("missing"))
        .is_ok());

    let mut incompatible = current;
    let mut semantics = incompatible.analysis_semantics.take().unwrap();
    semantics.descriptor.relationship_resolver_version = "legacy-resolver".into();
    incompatible.analysis_semantics = Some(AnalysisSemanticsState::new(semantics.descriptor));
    store.put_manifest(&incompatible).unwrap();

    let failures = [
        store.neighbors("missing", 10).unwrap_err().to_string(),
        store
            .shortest_path("missing", "also-missing", 4)
            .unwrap_err()
            .to_string(),
        store.imports().unwrap_err().to_string(),
        store
            .implementation_facts_for_target("missing", 10)
            .unwrap_err()
            .to_string(),
        store
            .references_for_symbol(&SymbolId::new("missing"), 10)
            .unwrap_err()
            .to_string(),
        store
            .occurrences_for_file(&FileId::new("missing"))
            .unwrap_err()
            .to_string(),
    ];
    for message in failures {
        assert!(message.contains("authoritative relationship evidence unavailable"));
        assert!(message.contains("RebuildRequired"));
        assert!(message.contains("Run a full `ok index` rebuild"));
    }

    // Incompatibility blocks relationship truth, not observability needed to diagnose/rebuild it.
    assert_eq!(store.graph_counts().unwrap().edges, 0);
    assert!(store.list_files(10, 0).is_ok());
    assert!(store.manifest().unwrap().is_some());
}
