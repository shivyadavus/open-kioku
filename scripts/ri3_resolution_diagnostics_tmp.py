from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


# Core owns the durable diagnostics contract.
core = "crates/open-kioku-core/src/lib.rs"
replace_exact(
    core,
    '''#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]\npub struct IndexQuality {\n''',
    '''#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]\npub struct RelationshipResolutionQuality {\n    pub candidates_considered: usize,\n    pub proven: usize,\n    pub ambiguous: usize,\n    pub unresolved: usize,\n    pub external: usize,\n    pub heuristic_candidates_retained: usize,\n    #[serde(default)]\n    pub proof_kind_counts: BTreeMap<String, usize>,\n    #[serde(default)]\n    pub resolver_strategy_counts: BTreeMap<String, usize>,\n}\n\n#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]\npub struct ResolutionQualityReport {\n    pub call_sites: usize,\n    pub resolved_exact: usize,\n    pub resolved_high: usize,\n    pub ambiguous: usize,\n    pub unresolved: usize,\n    pub external: usize,\n    pub legacy_only: usize,\n    pub semantic_only: usize,\n    pub disagreement: usize,\n    #[serde(default)]\n    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,\n}\n\n#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]\npub struct IndexQuality {\n''',
    "core relationship diagnostics types",
)
replace_exact(
    core,
    '''    #[serde(default)]\n    pub skipped_paths: Vec<SkippedPath>,\n    pub quality_notes: Vec<String>,\n}\n''',
    '''    #[serde(default)]\n    pub skipped_paths: Vec<SkippedPath>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub resolution_quality: Option<ResolutionQualityReport>,\n    pub quality_notes: Vec<String>,\n}\n''',
    "index quality resolution diagnostics field",
)
core_path = Path(core)
core_text = core_path.read_text()
core_text += r'''

#[cfg(test)]
mod ri3_resolution_quality_core_tests {
    use super::*;

    #[test]
    fn old_index_quality_without_resolution_report_remains_readable() {
        let encoded = serde_json::to_value(IndexQuality::default()).unwrap();
        assert!(encoded.get("resolution_quality").is_none());
        let decoded: IndexQuality = serde_json::from_value(encoded).unwrap();
        assert!(decoded.resolution_quality.is_none());
    }

    #[test]
    fn resolution_report_round_trips_with_deterministic_relationship_order() {
        let mut report = ResolutionQualityReport::default();
        report.by_relationship.insert(
            "uses_type".into(),
            RelationshipResolutionQuality {
                candidates_considered: 2,
                proven: 1,
                heuristic_candidates_retained: 1,
                ..RelationshipResolutionQuality::default()
            },
        );
        report.by_relationship.insert(
            "calls".into(),
            RelationshipResolutionQuality {
                candidates_considered: 1,
                proven: 1,
                ..RelationshipResolutionQuality::default()
            },
        );
        let mut quality = IndexQuality::default();
        quality.resolution_quality = Some(report.clone());

        let first = serde_json::to_string(&quality).unwrap();
        let second = serde_json::to_string(&quality).unwrap();
        assert_eq!(first, second);
        assert!(first.find("\"calls\"").unwrap() < first.find("\"uses_type\"").unwrap());

        let decoded: IndexQuality = serde_json::from_str(&first).unwrap();
        assert_eq!(decoded.resolution_quality, Some(report));
    }
}
'''
core_path.write_text(core_text)

# Ingest re-exports the core types to preserve the existing public API while keeping
# resolution-specific accounting logic local to the resolver/ingest layer.
ingest = "crates/open-kioku-ingest/src/lib.rs"
replace_exact(
    ingest,
    '''pub mod validation;\n\nconst MAX_HISTORY_COCHANGE_EDGES: usize = 5000;\n''',
    '''pub mod validation;\n\npub use open_kioku_core::{RelationshipResolutionQuality, ResolutionQualityReport};\n\nconst MAX_HISTORY_COCHANGE_EDGES: usize = 5000;\n''',
    "ingest diagnostics re-export",
)
replace_exact(
    ingest,
    '''#[derive(Debug, Clone, Default, Serialize, Deserialize)]\npub struct RelationshipResolutionQuality {\n    pub candidates_considered: usize,\n    pub proven: usize,\n    pub ambiguous: usize,\n    pub unresolved: usize,\n    pub external: usize,\n    pub heuristic_candidates_retained: usize,\n    #[serde(default)]\n    pub proof_kind_counts: BTreeMap<String, usize>,\n    #[serde(default)]\n    pub resolver_strategy_counts: BTreeMap<String, usize>,\n}\n\n#[derive(Debug, Clone, Default, Serialize, Deserialize)]\npub struct ResolutionQualityReport {\n    pub call_sites: usize,\n    pub resolved_exact: usize,\n    pub resolved_high: usize,\n    pub ambiguous: usize,\n    pub unresolved: usize,\n    pub external: usize,\n    pub legacy_only: usize,\n    pub semantic_only: usize,\n    pub disagreement: usize,\n    #[serde(default)]\n    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,\n}\n\n''',
    '',
    "remove duplicate ingest diagnostics structs",
)
replace_exact(
    ingest,
    '''impl ResolutionQualityReport {\n''',
    '''trait ResolutionQualityReportExt {\n    fn record_outcome(\n        &mut self,\n        edge_type: &GraphEdgeType,\n        outcome: &open_kioku_resolution::ResolutionOutcome,\n    );\n\n    fn record_reference_occurrence(&mut self, occurrence: &SymbolOccurrence);\n}\n\nimpl ResolutionQualityReportExt for ResolutionQualityReport {\n''',
    "local ingest diagnostics extension trait",
)
replace_exact(
    ingest,
    '''        let mut mode_notes = mode_quality_notes(mode);\n        mode_notes.extend(resolver_quality_notes);\n        let quality = index_quality(IndexQualityInput {\n''',
    '''        let mut mode_notes = mode_quality_notes(mode);\n        mode_notes.extend(resolver_quality_notes);\n        let mut quality = index_quality(IndexQualityInput {\n''',
    "mutable index quality for persisted resolution diagnostics",
)
replace_exact(
    ingest,
    '''            skipped_paths: &scan.skipped_paths,\n        });\n        let manifest = IndexManifest {\n            repository,\n            file_count: files.len(),\n''',
    '''            skipped_paths: &scan.skipped_paths,\n        });\n        let resolution_quality =\n            if resolution_mode == open_kioku_config::ResolutionMode::Legacy {\n                None\n            } else {\n                Some(quality_report)\n            };\n        quality.resolution_quality = resolution_quality.clone();\n        let manifest = IndexManifest {\n            repository,\n            file_count: files.len(),\n''',
    "persist resolution diagnostics into manifest quality",
)
replace_exact(
    ingest,
    '''                resolution_diffs,\n                resolution_quality: if resolution_mode == open_kioku_config::ResolutionMode::Legacy\n                {\n                    None\n                } else {\n                    Some(quality_report)\n                },\n''',
    '''                resolution_diffs,\n                resolution_quality,\n''',
    "reuse persisted resolution diagnostics in snapshot",
)

# Human CLI diagnostics read from the same manifest field used by JSON/status persistence.
commands = "crates/open-kioku-cli/src/commands/mod.rs"
commands_path = Path(commands)
commands_text = commands_path.read_text()
commands_text = r'''fn relationship_resolution_summary_lines(
    report: &open_kioku_core::ResolutionQualityReport,
) -> Vec<String> {
    if report.by_relationship.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["Relationship resolution:".to_string()];
    for (relationship, metrics) in &report.by_relationship {
        lines.push(format!(
            "  {relationship}: {} proven / {} candidates, {} ambiguous, {} unresolved, {} external, {} heuristic candidates retained",
            metrics.proven,
            metrics.candidates_considered,
            metrics.ambiguous,
            metrics.unresolved,
            metrics.external,
            metrics.heuristic_candidates_retained,
        ));
    }
    lines
}

''' + commands_text
commands_path.write_text(commands_text)
replace_exact(
    commands,
    '''                if let Some(scip) = &snapshot.scip {\n                    println!(\n                        "SCIP: mode {:?}, imported {} index(es), {} exact references",\n                        scip.mode,\n                        scip.imported_paths.len(),\n                        scip.exact_references\n                    );\n                    for attempt in &scip.generator_attempts {\n                        println!(\n                            "SCIP {}: {:?} - {}",\n                            attempt.language, attempt.status, attempt.message\n                        );\n                    }\n                }\n            }\n        }\n        Command::Snapshot { command } => match command {\n''',
    '''                if let Some(scip) = &snapshot.scip {\n                    println!(\n                        "SCIP: mode {:?}, imported {} index(es), {} exact references",\n                        scip.mode,\n                        scip.imported_paths.len(),\n                        scip.exact_references\n                    );\n                    for attempt in &scip.generator_attempts {\n                        println!(\n                            "SCIP {}: {:?} - {}",\n                            attempt.language, attempt.status, attempt.message\n                        );\n                    }\n                }\n                if let Some(report) = snapshot.manifest.quality.resolution_quality.as_ref() {\n                    for line in relationship_resolution_summary_lines(report) {\n                        println!("{line}");\n                    }\n                }\n            }\n        }\n        Command::Snapshot { command } => match command {\n''',
    "index human relationship diagnostics",
)
replace_exact(
    commands,
    '''                println!(\n                    "Healthy index: {} files, {} symbols, {} skipped, mode {}, indexed at {}",\n                    manifest.file_count,\n                    manifest.symbol_count,\n                    manifest.quality.skipped_paths.len(),\n                    manifest.index_mode,\n                    manifest.indexed_at\n                );\n            } else {\n''',
    '''                println!(\n                    "Healthy index: {} files, {} symbols, {} skipped, mode {}, indexed at {}",\n                    manifest.file_count,\n                    manifest.symbol_count,\n                    manifest.quality.skipped_paths.len(),\n                    manifest.index_mode,\n                    manifest.indexed_at\n                );\n                if let Some(report) = manifest.quality.resolution_quality.as_ref() {\n                    for line in relationship_resolution_summary_lines(report) {\n                        println!("{line}");\n                    }\n                }\n            } else {\n''',
    "status human relationship diagnostics",
)

# Permanent integration regression: index in V2, persist to SQLite manifest, reload through
# the same status loader, and verify both JSON and human-summary sources.
cli_lib = Path("crates/open-kioku-cli/src/lib.rs")
cli_text = cli_lib.read_text()
cli_text += r'''

#[cfg(test)]
mod ri3_resolution_diagnostics_tests {
    use super::*;

    #[test]
    fn relationship_quality_persists_into_index_and_status_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ri3-diagnostics-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn callee() {}\npub fn caller() { callee(); }\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        let mut config = OkConfig::load_from_repo(repo).unwrap();
        config.index.resolution_mode = open_kioku_config::ResolutionMode::V2;
        config.scip.enabled = false;
        config.history.enabled = false;
        config.semantic.enabled = false;

        let snapshot = index_repo_with_config(repo, config, IndexMode::Full).unwrap();
        let report = snapshot
            .manifest
            .quality
            .resolution_quality
            .as_ref()
            .expect("V2 indexing should expose relationship resolution diagnostics");
        let calls = report
            .by_relationship
            .get("calls")
            .expect("call resolution should be represented in diagnostics");
        assert!(calls.candidates_considered >= 1);

        let persisted = load_index_manifest(repo)
            .unwrap()
            .expect("index manifest should be persisted for status");
        let persisted_report = persisted
            .quality
            .resolution_quality
            .as_ref()
            .expect("status manifest should retain relationship diagnostics");
        assert_eq!(persisted_report.by_relationship, report.by_relationship);

        let json = serde_json::to_value(&persisted).unwrap();
        assert!(json
            .pointer("/quality/resolution_quality/by_relationship/calls")
            .is_some());
        let lines = relationship_resolution_summary_lines(persisted_report);
        assert!(lines.iter().any(|line| {
            line.contains("calls:") && line.contains("proven") && line.contains("candidates")
        }));
    }
}
'''
cli_lib.write_text(cli_text)
