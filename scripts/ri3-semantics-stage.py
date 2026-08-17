#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text)


def replace_once(path: str, old: str, new: str) -> None:
    text = read(path)
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one occurrence, found {count}: {old[:80]!r}")
    write(path, text.replace(old, new, 1))


def wire_core() -> None:
    path = "crates/open-kioku-core/src/lib.rs"
    text = read(path)
    if "pub mod analysis_semantics;" not in text:
        text = text.replace("pub mod identity;\n", "pub mod analysis_semantics;\npub mod identity;\n", 1)
    if "pub use analysis_semantics::*;" not in text:
        marker = "pub use relationship::{\n"
        text = text.replace(marker, "pub use analysis_semantics::*;\n\n" + marker, 1)
    old = "    pub schema_version: u32,\n    #[serde(default)]\n    pub index_mode: IndexMode,\n"
    new = (
        "    pub schema_version: u32,\n"
        "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n"
        "    pub analysis_semantics: Option<AnalysisSemanticsState>,\n"
        "    #[serde(default)]\n"
        "    pub index_mode: IndexMode,\n"
    )
    if "pub analysis_semantics: Option<AnalysisSemanticsState>" not in text:
        if old not in text:
            raise SystemExit("core IndexManifest insertion point missing")
        text = text.replace(old, new, 1)
    write(path, text)


def wire_manifest_literals() -> None:
    for path in sorted((ROOT / "crates").rglob("*.rs")):
        rel = path.relative_to(ROOT).as_posix()
        lines = path.read_text().splitlines(keepends=True)
        out = []
        changed = False
        for i, line in enumerate(lines):
            out.append(line)
            if "IndexManifest {" not in line or "struct IndexManifest" in line:
                continue
            lookahead = "".join(lines[i + 1 : i + 6])
            if "analysis_semantics:" in lookahead:
                continue
            indent = line[: len(line) - len(line.lstrip())] + "    "
            expr = "AnalysisSemanticsState::current()" if rel == "crates/open-kioku-core/src/lib.rs" else "open_kioku_core::AnalysisSemanticsState::current()"
            out.append(f"{indent}analysis_semantics: Some({expr}),\n")
            changed = True
        if changed:
            path.write_text("".join(out))


def wire_storage() -> None:
    path = "crates/open-kioku-storage/src/lib.rs"
    text = read(path)
    text = text.replace(
        "    ParserVersionStale,\n    SchemaVersionStale,\n",
        "    ParserVersionStale,\n    AnalysisSemanticsStale,\n    SchemaVersionStale,\n",
        1,
    )
    parser_block = """    if previous_parser_version\n        .zip(next_parser_version)\n        .is_some_and(|(previous, next)| previous != next)\n    {\n        return next_files\n            .iter()\n            .map(|file| IndexChange {\n                old_path: Some(file.path.clone()),\n                new_path: Some(file.path.clone()),\n                file_id: Some(file.id.clone()),\n                kind: IndexChangeKind::ParserVersionStale,\n            })\n            .collect();\n    }\n"""
    semantics_block = parser_block + """    if previous_manifest.is_some_and(|manifest| {\n        analysis_semantics_compatibility(Some(manifest), next_manifest).status\n            != open_kioku_core::AnalysisSemanticsCompatibilityStatus::Compatible\n    }) {\n        return next_files\n            .iter()\n            .map(|file| IndexChange {\n                old_path: Some(file.path.clone()),\n                new_path: Some(file.path.clone()),\n                file_id: Some(file.id.clone()),\n                kind: IndexChangeKind::AnalysisSemanticsStale,\n            })\n            .collect();\n    }\n"""
    if "kind: IndexChangeKind::AnalysisSemanticsStale" not in text:
        if parser_block not in text:
            raise SystemExit("storage parser staleness block missing")
        text = text.replace(parser_block, semantics_block, 1)
    old_partial = """pub fn partial_index_supported(previous: Option<&IndexManifest>, next: &IndexManifest) -> bool {\n    previous.is_some_and(|previous| {\n        previous.schema_version == next.schema_version && previous.index_mode == next.index_mode\n    })\n}\n"""
    new_partial = """pub fn analysis_semantics_compatibility(\n    previous: Option<&IndexManifest>,\n    next: &IndexManifest,\n) -> open_kioku_core::AnalysisSemanticsCompatibility {\n    let current = next\n        .analysis_semantics\n        .clone()\n        .unwrap_or_else(open_kioku_core::AnalysisSemanticsState::current);\n    open_kioku_core::classify_analysis_semantics(\n        previous.and_then(|manifest| manifest.analysis_semantics.as_ref()),\n        &current,\n    )\n}\n\npub fn partial_index_supported(previous: Option<&IndexManifest>, next: &IndexManifest) -> bool {\n    previous.is_some_and(|previous| {\n        previous.schema_version == next.schema_version\n            && previous.index_mode == next.index_mode\n            && analysis_semantics_compatibility(Some(previous), next)\n                .status\n                .allows_partial_index_update()\n    })\n}\n"""
    if "pub fn analysis_semantics_compatibility(" not in text:
        if old_partial not in text:
            raise SystemExit("storage partial-index function missing")
        text = text.replace(old_partial, new_partial, 1)
    old_import = """    use super::{\n        classify_file_changes, classify_file_changes_with_parser_version, IndexChangeKind,\n    };\n"""
    new_import = """    use super::{\n        analysis_semantics_compatibility, classify_file_changes,\n        classify_file_changes_with_parser_version, partial_index_supported, IndexChangeKind,\n    };\n"""
    if "analysis_semantics_compatibility, classify_file_changes" not in text:
        if old_import not in text:
            raise SystemExit("storage test import missing")
        text = text.replace(old_import, new_import, 1)
    insert_before = """    fn manifest(schema_version: u32) -> IndexManifest {\n"""
    tests = """    #[test]\n    fn analysis_semantics_change_disables_partial_updates_without_schema_change() {\n        let next = manifest(1);\n        let mut previous = manifest(1);\n        let mut state = previous.analysis_semantics.clone().unwrap();\n        state.descriptor.proof_policy_version = \"old-proof-policy\".into();\n        previous.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::new(state.descriptor));\n\n        let compatibility = analysis_semantics_compatibility(Some(&previous), &next);\n        assert_eq!(\n            compatibility.status,\n            open_kioku_core::AnalysisSemanticsCompatibilityStatus::RebuildRequired\n        );\n        assert!(!partial_index_supported(Some(&previous), &next));\n\n        let files = vec![file(\"f1\", \"src/lib.rs\", \"a\")];\n        let changes = classify_file_changes(Some(&previous), &next, &files, &files);\n        assert_eq!(changes[0].kind, IndexChangeKind::AnalysisSemanticsStale);\n    }\n\n    #[test]\n    fn schema_version_is_independent_from_analysis_semantics() {\n        let first = manifest(1);\n        let second = manifest(2);\n        assert_eq!(\n            first.analysis_semantics.as_ref().unwrap().fingerprint,\n            second.analysis_semantics.as_ref().unwrap().fingerprint\n        );\n        assert_eq!(\n            analysis_semantics_compatibility(Some(&first), &second).status,\n            open_kioku_core::AnalysisSemanticsCompatibilityStatus::Compatible\n        );\n        assert!(!partial_index_supported(Some(&first), &second));\n    }\n\n    #[test]\n    fn legacy_manifest_requires_rebuild() {\n        let next = manifest(1);\n        let mut legacy = manifest(1);\n        legacy.analysis_semantics = None;\n        let compatibility = analysis_semantics_compatibility(Some(&legacy), &next);\n        assert_eq!(\n            compatibility.status,\n            open_kioku_core::AnalysisSemanticsCompatibilityStatus::RebuildRequired\n        );\n        assert!(!partial_index_supported(Some(&legacy), &next));\n    }\n\n"""
    if "analysis_semantics_change_disables_partial_updates_without_schema_change" not in text:
        if insert_before not in text:
            raise SystemExit("storage manifest helper missing")
        text = text.replace(insert_before, tests + insert_before, 1)
    write(path, text)


def wire_watch() -> None:
    path = "crates/open-kioku-watch/src/lib.rs"
    text = read(path)
    old_import = """    changed_document_paths, classify_file_changes, partial_index_supported, GraphStore,\n"""
    new_import = """    analysis_semantics_compatibility, changed_document_paths, classify_file_changes,\n    partial_index_supported, GraphStore,\n"""
    if "analysis_semantics_compatibility, changed_document_paths" not in text:
        if old_import not in text:
            raise SystemExit("watch storage import missing")
        text = text.replace(old_import, new_import, 1)
    marker = """    let previous_documents = store.document_sections()?;\n    let changed_paths = changed_paths\n"""
    gate = """    let previous_documents = store.document_sections()?;\n    if previous_manifest.is_some() {\n        let compatibility =\n            analysis_semantics_compatibility(previous_manifest.as_ref(), &snapshot.manifest);\n        if !compatibility.status.allows_partial_index_update() {\n            return Err(OkError::Index(format!(\n                \"analysis semantics {}: {}; stored={}, current={}; {}\",\n                serde_json::to_value(compatibility.status)\n                    .ok()\n                    .and_then(|value| value.as_str().map(ToOwned::to_owned))\n                    .unwrap_or_else(|| \"rebuild_required\".into()),\n                compatibility.reasons.join(\"; \"),\n                compatibility.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n                compatibility.current_fingerprint,\n                compatibility.recommended_action\n            )));\n        }\n    }\n    let changed_paths = changed_paths\n"""
    if "analysis semantics {}:" not in text:
        if marker not in text:
            raise SystemExit("watch compatibility insertion point missing")
        text = text.replace(marker, gate, 1)
    # serde_json is already a transitive workspace dependency only if declared; avoid adding a new
    # crate dependency by using Debug if the watch Cargo manifest does not declare serde_json.
    text = text.replace(
        """                serde_json::to_value(compatibility.status)\n                    .ok()\n                    .and_then(|value| value.as_str().map(ToOwned::to_owned))\n                    .unwrap_or_else(|| \"rebuild_required\".into()),\n""",
        """                format!(\"{:?}\", compatibility.status).to_ascii_lowercase(),\n""",
    )
    # Add a focused persistence-safety regression using the existing watch fixture style.
    test_marker = """    #[test]\n    fn reindex_repo_writes_sqlite_and_search_indexes() {\n"""
    test = """    #[test]\n    fn incremental_reindex_refuses_incompatible_semantics_and_preserves_manifest() {\n        let temp = tempfile::tempdir().unwrap();\n        let repo = temp.path();\n        fs::create_dir_all(repo.join(\"src\")).unwrap();\n        fs::write(repo.join(\"src/lib.rs\"), \"pub fn stable() {}\\n\").unwrap();\n        OkConfig::write_default(repo.join(\"ok.toml\")).unwrap();\n        git(repo, &[\"init\", \"--quiet\"]);\n        git(repo, &[\"config\", \"user.email\", \"watch@example.com\"]);\n        git(repo, &[\"config\", \"user.name\", \"Watch Test\"]);\n        git(repo, &[\"config\", \"commit.gpgsign\", \"false\"]);\n        git(repo, &[\"add\", \".\"]);\n        git(repo, &[\"commit\", \"--quiet\", \"-m\", \"initial source\"]);\n\n        reindex_repo(repo).unwrap();\n        let store = SqliteStore::open(repo.join(\".ok/index.sqlite\")).unwrap();\n        let mut legacy = store.manifest().unwrap().unwrap();\n        let mut state = legacy.analysis_semantics.clone().unwrap();\n        state.descriptor.relationship_resolver_version = \"old-resolver\".into();\n        legacy.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::new(state.descriptor));\n        let legacy_fingerprint = legacy.analysis_semantics.as_ref().unwrap().fingerprint.clone();\n        store.put_manifest(&legacy).unwrap();\n\n        fs::write(repo.join(\"src/lib.rs\"), \"pub fn stable() { let _ = 1; }\\n\").unwrap();\n        let err = reindex_repo_after_changes(repo, [repo.join(\"src/lib.rs\")].iter().map(PathBuf::as_path))\n            .unwrap_err();\n        assert!(err.to_string().contains(\"analysis semantics\"));\n\n        let persisted = store.manifest().unwrap().unwrap();\n        assert_eq!(\n            persisted.analysis_semantics.as_ref().unwrap().fingerprint,\n            legacy_fingerprint\n        );\n    }\n\n"""
    if "incremental_reindex_refuses_incompatible_semantics_and_preserves_manifest" not in text:
        if test_marker not in text:
            raise SystemExit("watch test insertion point missing")
        text = text.replace(test_marker, test + test_marker, 1)
    write(path, text)


def wire_relationship_benchmark() -> None:
    path = "crates/open-kioku-cli/src/bench/relationship_live.rs"
    text = read(path)
    old = """    let fingerprint_input = format!(\n        \"relationship-bench={};corpus={};proof-policy=ri3-v1;language-capability-contract=v1;resolution-mode=shadow\",\n        RELATIONSHIP_BENCH_SCHEMA_VERSION, corpus.corpus_version\n    );\n    let semantics_fingerprint = format!(\"{:x}\", Sha256::digest(fingerprint_input.as_bytes()));\n"""
    new = """    let semantics_fingerprint = open_kioku_core::AnalysisSemanticsState::current().fingerprint;\n"""
    if old in text:
        text = text.replace(old, new, 1)
    # Keep SHA256 import if it is used elsewhere in this live fixture file; rustfmt/clippy will tell
    # us if the import can be removed. The durable benchmark now reports the product semantics hash.
    write(path, text)


wire_core()
wire_manifest_literals()
wire_storage()
wire_watch()
wire_relationship_benchmark()
print("RI3 semantics manifest integration staged")
