//! One file the indexer cannot open must cost that file, not the index (#350).

#![cfg(unix)]

use open_kioku_config::OkConfig;
use open_kioku_core::{SkipReason, SkipSource};
use open_kioku_ingest::Indexer;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn unreadable_file_is_skipped_and_the_rest_of_the_repository_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn readable_entry() {}\n").unwrap();
    fs::write(root.join("src/other.rs"), "pub fn other_entry() {}\n").unwrap();
    let locked = root.join("src/locked.rs");
    fs::write(&locked, "pub fn locked_entry() {}\n").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // A superuser (some CI containers) can read a mode-000 file; the skip path is then not
    // exercised, and the honest assertion is that the file simply indexes.
    let unreadable = fs::read(&locked).is_err();

    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config.documents.enabled = false;
    let snapshot = Indexer::default()
        .index_repo(root, &config)
        .expect("an unreadable file must not abort the index");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

    let mut paths: Vec<_> = snapshot
        .files
        .iter()
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    if !unreadable {
        assert_eq!(paths, vec!["src/lib.rs", "src/locked.rs", "src/other.rs"]);
        return;
    }
    assert_eq!(paths, vec!["src/lib.rs", "src/other.rs"]);
    assert_eq!(snapshot.manifest.file_count, 2);
    assert_eq!(
        snapshot
            .manifest
            .quality
            .skip_counts
            .get(&SkipReason::Error),
        Some(&1)
    );
    let skip = snapshot
        .manifest
        .quality
        .skipped_paths
        .iter()
        .find(|skip| skip.reason == SkipReason::Error)
        .expect("the unreadable file is recorded as a skip");
    assert_eq!(skip.path.to_string_lossy(), "src/locked.rs");
    assert_eq!(skip.source, SkipSource::Filesystem);
    assert!(skip.safe_to_show);
    assert!(
        snapshot
            .phase_reports
            .iter()
            .any(|report| report.warnings.iter().any(|w| w.contains("src/locked.rs"))),
        "the skip is surfaced as a warning"
    );

    let coverage = snapshot
        .manifest
        .quality
        .coverage
        .as_ref()
        .expect("a full index records coverage");
    assert_eq!(coverage.discovered, 3);
    assert_eq!(coverage.indexed, 2);
    assert_eq!(coverage.skipped.get(&SkipReason::Error), Some(&1));
}
