use open_kioku_config::OkConfig;
use open_kioku_core::SkipReason;
use open_kioku_ingest::Indexer;
use std::fs;
use std::path::Path;

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const UPPER_DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Credential-shaped test values are assembled at run time so that no string in the repository
/// matches a real provider's key format or reads as a leaked secret to a scanner.
fn striding(alphabet: &[u8], len: usize, stride: usize, offset: usize) -> String {
    (0..len)
        .map(|index| char::from(alphabet[(index * stride + offset) % alphabet.len()]))
        .collect()
}

/// Shaped like a cloud access key id (a four-letter prefix and 16 upper-case letters and
/// digits) without matching any provider's prefix.
fn cloud_key_shaped() -> String {
    format!(
        "{}{}",
        ["OK", "CK"].concat(),
        striding(UPPER_DIGITS, 16, 7, 3)
    )
}

/// 32 distinct characters: 5 bits of entropy per character, under a key no rule names.
fn high_entropy_token() -> String {
    striding(ALNUM, 32, 17, 5)
}

fn config_with_secrets(cloud_key: &str, token: &str) -> String {
    format!(
        "storage:\n  provider: objectstore\n  region: north-1\n  access_key_id: {cloud_key}\nwebhooks:\n  delivery_nonce: \"{token}\"\n"
    )
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn config() -> OkConfig {
    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config
}

/// Redaction runs before the parser and the document corpus see the text, so every record
/// the index stores for a data, config, or prose file is free of the value, and the file is
/// still indexed by its keys.
#[test]
fn config_and_prose_values_are_redacted_before_anything_is_derived_from_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (cloud_key, token) = (cloud_key_shaped(), high_entropy_token());
    write(root, "src/lib.rs", "pub fn live() {}\n");
    write(
        root,
        "config/settings.yaml",
        &config_with_secrets(&cloud_key, &token),
    );
    write(
        root,
        "docs/setup.md",
        &format!("# Setup\n\nExport `STORAGE_TOKEN={token}` before running.\n"),
    );
    // A bare token pasted into a file whose name says it holds secrets: no key, no URL and no
    // PEM header, so only the file's name can decide that it is credential-bearing content.
    write(
        root,
        "docs/SECRETS.md",
        &format!("# Key rotation\n\nCurrent value:\n\n{token}\n"),
    );
    // Named for a secret but not key material: indexed like any other config file.
    write(
        root,
        "config/secrets.yaml",
        &config_with_secrets(&cloud_key, &token),
    );

    let snapshot = Indexer::default().index_repo(root, &config()).unwrap();

    let stored = [
        serde_json::to_string(&snapshot.chunks).unwrap(),
        serde_json::to_string(&snapshot.symbols).unwrap(),
        serde_json::to_string(&snapshot.document_sections).unwrap(),
        serde_json::to_string(&snapshot.analysis_facts).unwrap(),
        serde_json::to_string(&snapshot.tests).unwrap(),
        serde_json::to_string(&snapshot.occurrences).unwrap(),
        serde_json::to_string(&snapshot.imports).unwrap(),
        serde_json::to_string(&snapshot.manifest).unwrap(),
    ]
    .concat();
    for secret in [&cloud_key, &token] {
        assert!(!stored.contains(secret.as_str()), "{secret} was indexed");
    }

    let settings = snapshot
        .files
        .iter()
        .find(|file| file.path == Path::new("config/settings.yaml"))
        .expect("the config file is indexed");
    let settings_text = snapshot
        .chunks
        .iter()
        .filter(|chunk| chunk.file_id == settings.id)
        .map(|chunk| chunk.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        settings_text.contains("  access_key_id: [REDACTED]"),
        "{settings_text}"
    );
    assert!(
        settings_text.contains("  delivery_nonce: \"[REDACTED]\""),
        "{settings_text}"
    );
    assert!(
        settings_text.contains("provider: objectstore"),
        "{settings_text}"
    );

    let guide = snapshot
        .document_sections
        .iter()
        .find(|section| section.path == Path::new("docs/setup.md"))
        .expect("the prose file is in the document corpus");
    assert!(
        guide.content.contains("STORAGE_TOKEN=[REDACTED]"),
        "{}",
        guide.content
    );

    let secrets = snapshot
        .files
        .iter()
        .find(|file| file.path == Path::new("config/secrets.yaml"))
        .expect("a config file named for a secret is indexed");
    assert!(snapshot.chunks.iter().any(
        |chunk| chunk.file_id == secrets.id && chunk.text.contains("access_key_id: [REDACTED]")
    ));

    let quality = &snapshot.manifest.quality;
    assert_eq!(quality.redacted_files, Some(4));
    assert!(!quality.pending_pre_redaction_compaction);
    let coverage = quality.coverage.as_ref().unwrap();
    assert!(!coverage.by_language["yaml"]
        .skipped
        .contains_key(&SkipReason::SecretPolicy));
}

/// The scope boundary: programming-language source is indexed exactly as written, and a
/// repository with nothing to redact reports zero rather than "not recorded".
#[test]
fn programming_source_is_indexed_as_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let token = high_entropy_token();
    write(
        root,
        "src/lib.rs",
        &format!("pub const DELIVERY_NONCE: &str = \"{token}\";\n"),
    );

    let snapshot = Indexer::default().index_repo(root, &config()).unwrap();

    assert!(snapshot
        .chunks
        .iter()
        .any(|chunk| chunk.text.contains(&token)));
    assert_eq!(snapshot.manifest.quality.redacted_files, Some(0));
}
