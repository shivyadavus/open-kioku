//! Indexing one tree twice must produce the same index, whatever order the filesystem lists
//! its files in and however rayon schedules the parse and resolution work (#461, #468).

use open_kioku_config::OkConfig;
use open_kioku_ingest::Indexer;
use std::fs;
use std::path::Path;

const FILES: usize = 24;

/// Writes the fixture's files in `order`. Some filesystems list directory entries in creation
/// order, so copies written in opposite orders are walked in opposite orders.
fn write_fixture(root: &Path, order: impl Iterator<Item = usize>) {
    fs::create_dir_all(root.join("src")).unwrap();
    for index in order {
        // Four unresolved calls per file overfill the registry's unresolved-note cap.
        fs::write(
            root.join(format!("src/widget_{index:02}.rs")),
            format!(
                "pub fn render_widget_{index:02}(frame: u32) -> u32 {{\n    absent_alpha_{index:02}();\n    absent_bravo_{index:02}();\n    absent_charlie_{index:02}();\n    absent_delta_{index:02}();\n    frame\n}}\n"
            ),
        )
        .unwrap();
    }
}

struct Fingerprint {
    quality_notes: Vec<String>,
    chunk_ids: Vec<String>,
    symbol_ids: Vec<String>,
}

fn fingerprint(root: &Path) -> Fingerprint {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let snapshot = pool
        .install(|| Indexer::default().index_repo(root, &OkConfig::default()))
        .unwrap();
    let root_text = root.display().to_string();
    Fingerprint {
        quality_notes: snapshot
            .manifest
            .quality
            .quality_notes
            .iter()
            .map(|note| format!("{:?}: {}", note.kind, note.message).replace(&root_text, "<root>"))
            .collect(),
        chunk_ids: snapshot
            .chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
        symbol_ids: snapshot
            .symbols
            .iter()
            .map(|symbol| symbol.id.0.clone())
            .collect(),
    }
}

#[test]
fn indexing_one_tree_repeatedly_yields_identical_notes_and_order() {
    let forward = tempfile::tempdir().unwrap();
    let reverse = tempfile::tempdir().unwrap();
    write_fixture(forward.path(), 0..FILES);
    write_fixture(reverse.path(), (0..FILES).rev());

    let baseline = fingerprint(forward.path());
    let unresolved = baseline
        .quality_notes
        .iter()
        .filter(|note| note.contains("symbol registry unresolved") && note.contains("in chunk"))
        .count();
    assert_eq!(
        unresolved, 64,
        "the fixture must fill the unresolved-note cap for the test to exercise it"
    );
    assert!(
        baseline
            .quality_notes
            .iter()
            .any(|note| note.contains("more unresolved name(s) not listed")),
        "the capped notes must say how many names were withheld"
    );
    assert!(!baseline.chunk_ids.is_empty());

    for run in 0..10 {
        let root = if run % 2 == 0 {
            reverse.path()
        } else {
            forward.path()
        };
        let observed = fingerprint(root);
        assert_eq!(observed.quality_notes, baseline.quality_notes, "run {run}");
        assert_eq!(observed.chunk_ids, baseline.chunk_ids, "run {run}");
        assert_eq!(observed.symbol_ids, baseline.symbol_ids, "run {run}");
    }
}
