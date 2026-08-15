use open_kioku_core::{File, FileId, Language, RepositoryId};
use open_kioku_tree_sitter::parse_file;
use std::collections::HashSet;

#[test]
fn chained_same_name_calls_have_distinct_ids() {
    let file = File {
        id: FileId::new("file-rust-call-identity"),
        repository_id: RepositoryId::new("repo"),
        path: "src/lib.rs".into(),
        language: Language::Rust,
        size_bytes: 0,
        content_hash: "hash".into(),
        is_generated: false,
        is_vendor: false,
    };
    let code = r#"
struct Repo;
impl Repo {
    fn save(&self) -> &Self { self }
}
fn run(repo: &Repo) {
    repo.save().save();
}
"#;

    let facts = parse_file(&file, code).expect("Rust fixture should parse");
    let save_calls = facts
        .calls
        .iter()
        .filter(|call| call.callee_name == "save")
        .collect::<Vec<_>>();

    assert_eq!(
        save_calls.len(),
        2,
        "fixture must contain two nested save calls"
    );
    let unique_ids = save_calls
        .iter()
        .map(|call| call.id.0.clone())
        .collect::<HashSet<_>>();
    assert_eq!(
        unique_ids.len(),
        2,
        "distinct call AST nodes need distinct IDs"
    );
}
