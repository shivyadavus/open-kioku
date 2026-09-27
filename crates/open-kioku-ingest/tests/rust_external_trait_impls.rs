//! A Rust `impl` of a trait defined outside the repository is counted as an `external`
//! `IMPLEMENTS` resolution, not an unresolved one (#599). Only the standard library, the prelude
//! and dependencies the package's manifest places outside the repository count as outside.

use open_kioku_config::OkConfig;
use open_kioku_ingest::Indexer;

fn implements_counts(files: &[(&str, &str)]) -> (usize, usize, usize) {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let absolute = dir.path().join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, content).unwrap();
    }
    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config.semantic.enabled = false;
    let snapshot = Indexer::default().index_repo(dir.path(), &config).unwrap();
    let quality = snapshot
        .resolution_quality
        .expect("indexing reports resolution quality");
    let implements = quality
        .by_relationship
        .get("IMPLEMENTS")
        .cloned()
        .unwrap_or_default();
    (
        implements.proven,
        implements.external,
        implements.unresolved,
    )
}

const LIB: &str = "use std::fmt;\nuse std::ops::Deref;\nuse bytes::Buf;\nuse crate::local::Link;\n\nmod local;\n\npub struct Frame(Vec<u8>);\n\nimpl fmt::Debug for Frame {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        Ok(())\n    }\n}\n\nimpl Deref for Frame {\n    type Target = [u8];\n    fn deref(&self) -> &[u8] {\n        &self.0\n    }\n}\n\nimpl Iterator for Frame {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> {\n        None\n    }\n}\n\nimpl Buf for Frame {}\n\nimpl Link for Frame {}\n\nimpl undeclared::Marker for Frame {}\n\npub trait Codec {}\n\nimpl Codec for Frame {}\n";

#[test]
fn rust_impls_of_standard_and_dependency_traits_count_as_external() {
    let (proven, external, unresolved) = implements_counts(&[
        (
            "Cargo.toml",
            "[package]\nname = \"frames\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbytes = \"1\"\n",
        ),
        ("src/lib.rs", LIB),
        ("src/local.rs", "pub trait Link {}\n"),
    ]);
    // `Codec` and `Link` are the repository's; `fmt::Debug`, `Deref`, `Iterator` and
    // `bytes::Buf` are not; the manifest declares no crate `undeclared`.
    assert_eq!((proven, external, unresolved), (2, 4, 1));
}

#[test]
fn rust_impl_of_an_undeclared_crates_trait_stays_unresolved() {
    let (proven, external, unresolved) = implements_counts(&[
        (
            "Cargo.toml",
            "[package]\nname = \"frames\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", LIB),
        ("src/local.rs", "pub trait Link {}\n"),
    ]);
    // Without the manifest declaring `bytes`, nothing shows the crate is outside the repository.
    assert_eq!((proven, external, unresolved), (2, 3, 2));
}

#[test]
fn rust_impl_of_a_trait_from_a_crate_patched_into_the_repository_stays_unresolved() {
    // `util = "1"` is a registry dependency, but the workspace root's `[patch.crates-io]` builds
    // the repository's `crates/util`, so its trait is the repository's.
    let (_, external, unresolved) = implements_counts(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n\n[patch.crates-io]\nutil = { path = \"crates/util\" }\n",
        ),
        (
            "crates/util/Cargo.toml",
            "[package]\nname = \"util\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        ),
        ("crates/util/src/lib.rs", "pub trait Tool {}\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nutil = \"1\"\n",
        ),
        (
            "crates/app/src/lib.rs",
            "pub struct S2;\n\nimpl util::Tool for S2 {}\n",
        ),
    ]);
    assert_eq!((external, unresolved), (0, 1));
}
