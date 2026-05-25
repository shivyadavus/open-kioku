use open_kioku_config::OcfConfig;
use open_kioku_ingest::Indexer;

#[test]
fn indexes_fixture_repo() {
    let root = std::path::Path::new("../../test-repos/rust-mini");
    let config = OcfConfig::default();
    let snapshot = Indexer::default().index_repo(root, &config).unwrap();
    assert!(snapshot.manifest.file_count >= 1);
    assert!(snapshot
        .symbols
        .iter()
        .any(|symbol| symbol.name == "retry_import"));
    assert!(!snapshot.chunks.is_empty());
}
