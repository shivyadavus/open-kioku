from pathlib import Path


path = Path("crates/open-kioku-context/src/lib.rs")
text = path.read_text()


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    text = text.replace(old, new, 1)


replace_once(
    """struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
}""",
    """struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
    lexical_anchors: Vec<String>,
}""",
    "TaskSearchIntent",
)

replace_once(
    """        }

        intent
    }

    fn search_terms(&self, task: &str) -> Vec<String> {""",
    """        }

        intent.lexical_anchors = task_lexical_terms(task);
        intent
    }

    fn search_terms(&self, task: &str) -> Vec<String> {""",
    "TaskSearchIntent::parse tail",
)

replace_once(
    """            .chain(self.primary_anchors.iter())
            .chain(self.reference_anchors.iter())
            .chain(alias_terms.iter())""",
    """            .chain(self.primary_anchors.iter())
            .chain(self.reference_anchors.iter())
            .chain(self.lexical_anchors.iter())
            .chain(alias_terms.iter())""",
    "search term chain",
)

for temporary_alias in (
    '        "verify" | "verifies" | "verified" | "verifying" => "verification".into(),\n',
    '        "changed" | "changes" | "changing" => "change".into(),\n',
    '        "plans" | "planned" | "planning" => "plan".into(),\n',
):
    replace_once(temporary_alias, "", f"temporary alias {temporary_alias.strip()}")

helper = """fn task_lexical_terms(task: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in task
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 4)
    {
        if is_task_stopword(&token) || tokens.iter().any(|existing| existing == &token) {
            continue;
        }
        tokens.push(token);
        if tokens.len() >= 8 {
            break;
        }
    }

    let mut terms = tokens.clone();
    for pair in tokens.windows(2).take(6) {
        push_unique_alias(&mut terms, &format!("{} {}", pair[0], pair[1]));
    }
    terms
}

fn is_task_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "against"
            | "before"
            | "between"
            | "from"
            | "into"
            | "that"
            | "their"
            | "there"
            | "these"
            | "this"
            | "those"
            | "through"
            | "under"
            | "using"
            | "with"
            | "without"
    )
}

"""
replace_once(
    "fn task_alias_terms(task: &str) -> Vec<String> {",
    helper + "fn task_alias_terms(task: &str) -> Vec<String> {",
    "task_alias_terms insertion point",
)

test_anchor = """    #[test]
    fn expanded_task_search_terms_include_config_aliases() {
        let terms = expanded_task_search_terms("add history configuration defaults");

        assert!(terms.iter().any(|term| term == "config"));
        assert!(terms.iter().any(|term| term == "default"));
        assert!(terms.iter().any(|term| term == "history config"));
        assert!(terms.iter().any(|term| term == "config default"));
    }
"""

test_addition = test_anchor + """
    #[test]
    fn natural_language_workflow_terms_retrieve_patch_verifier_context() {
        let repo_id = RepositoryId::new("repo");
        let patch_file = File {
            id: FileId::new("patch"),
            repository_id: repo_id.clone(),
            path: "crates/open-kioku-patch/src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "patch".into(),
            is_generated: false,
            is_vendor: false,
        };
        let noise_file = File {
            id: FileId::new("noise"),
            repository_id: repo_id,
            path: "crates/open-kioku-cli/src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "noise".into(),
            is_generated: false,
            is_vendor: false,
        };
        let patch_symbol = Symbol {
            id: SymbolId::new("change-verifier"),
            name: "ChangeVerifier".into(),
            qualified_name: "open_kioku_patch::ChangeVerifier".into(),
            kind: SymbolKind::Class,
            file_id: patch_file.id.clone(),
            range: Some(LineRange { start: 1, end: 8 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let chunks = vec![
            CodeChunk {
                id: "patch-chunk".into(),
                file_id: patch_file.id.clone(),
                range: LineRange { start: 1, end: 8 },
                language: Language::Rust,
                text: "pub struct ChangeVerifier; impl ChangeVerifier { fn verify(&self, changed_files: Vec<PathBuf>, plan: &PlanReport) {} }".into(),
                symbol_id: Some(patch_symbol.id.clone()),
            },
            CodeChunk {
                id: "noise-chunk".into(),
                file_id: noise_file.id.clone(),
                range: LineRange { start: 1, end: 4 },
                language: Language::Rust,
                text: "fn save_workspace_files() {}".into(),
                symbol_id: None,
            },
        ];
        let files = vec![patch_file, noise_file];
        let symbols = vec![patch_symbol];
        let task = "verify changed files against saved plans";
        let intent = TaskSearchIntent::parse(task);
        let results = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, 10, &intent).unwrap(),
            &intent,
            &RankingOptions::default(),
        );

        assert_eq!(
            results.first().map(|result| result.path.as_path()),
            Some(Path::new("crates/open-kioku-patch/src/lib.rs"))
        );
    }
"""
replace_once(test_anchor, test_addition, "context retrieval regression test insertion")

path.write_text(text)

Path("crates/open-kioku-context/tests/workflow_terms.rs").write_text(
    """use open_kioku_context::expanded_task_search_terms;

#[test]
fn natural_language_workflow_terms_expand_without_case_specific_aliases() {
    let terms = expanded_task_search_terms("verify changed files against saved plans");

    assert!(terms.iter().any(|term| term == "verify"));
    assert!(terms.iter().any(|term| term == "changed"));
    assert!(terms.iter().any(|term| term == "files"));
    assert!(terms.iter().any(|term| term == "saved"));
    assert!(terms.iter().any(|term| term == "plans"));
    assert!(terms.iter().any(|term| term == "verify changed"));
    assert!(!terms.iter().any(|term| term == "against"));
}
"""
)
