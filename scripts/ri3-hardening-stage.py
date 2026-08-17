#!/usr/bin/env python3
from __future__ import annotations

import json
from copy import deepcopy
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FULL = ROOT / "benchmarks/relationship-cases.json"
COMPACT = ROOT / "benchmarks/relationship-ci-cases.json"
LIVE = ROOT / "crates/open-kioku-cli/src/bench/relationship_live.rs"


def harden_corpus(path: Path) -> None:
    payload = json.loads(path.read_text())
    cases = payload["cases"]

    # These CALLS scenarios must actually execute ambiguous/dynamic source rather than the
    # ordinary happy-path fixture. They are negative precision probes by design.
    for case in cases:
        if case["relationship"] == "CALLS" and case["scenario"] in {
            "same_simple_name",
            "unrelated_receiver",
        }:
            case["expected_outcome"] = "must_not_emit"
            case.pop("expected_target", None)
            case.pop("expected_source_range", None)
            case["expected_proof_kinds"] = []
            case["forbidden_proof_kinds"] = []
            case["notes"] = (
                "Adversarial precision probe: ambiguous or unrelated same-named call targets "
                "must not become authoritative structural CALLS edges."
            )

    # Turn the multiple-exact-sites REFERENCES probe into a positive aggregation case for every
    # authoritative cohort. Reuse the cohort's approved positive contract, then require the live
    # producer to prove that two distinct exact source sites survive graph aggregation.
    by_cohort: dict[tuple[str, str], dict] = {}
    for case in cases:
        key = (case["language"], case["relationship"])
        if (
            case["capability_state"] == "authoritative"
            and case["expected_outcome"] == "must_emit"
            and case.get("expected_target")
        ):
            by_cohort.setdefault(key, case)
    for case in cases:
        if (
            case["relationship"] == "REFERENCES"
            and case["scenario"] == "multiple_exact_sites"
            and case["capability_state"] == "authoritative"
        ):
            template = by_cohort[(case["language"], case["relationship"])]
            case["expected_outcome"] = "must_emit"
            case["expected_target"] = deepcopy(template["expected_target"])
            case["expected_source_range"] = deepcopy(template["expected_source_range"])
            case["expected_proof_kinds"] = deepcopy(template["expected_proof_kinds"])
            case["forbidden_proof_kinds"] = deepcopy(template.get("forbidden_proof_kinds", []))
            case["notes"] = (
                "Two exact SCIP-equivalent occurrences to the same target must aggregate into "
                "one authoritative REFERENCES relationship while preserving both source sites."
            )

    path.write_text(json.dumps(payload, indent=2) + "\n")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise SystemExit(f"hardening anchor not found: {label}")
    return text.replace(old, new, 1)


def harden_live_producer() -> None:
    text = LIVE.read_text()

    text = replace_once(
        text,
        """        let mut snapshot = Indexer::default().index_repo_with_mode(&root, &config, IndexMode::Full)?;\n        inject_reference_fixture_occurrence(case, &mut snapshot)?;\n        let graph = InMemoryGraph::from_index_with_resolved_relationships(\n""",
        """        let mut snapshot = Indexer::default().index_repo_with_mode(&root, &config, IndexMode::Full)?;\n        inject_reference_fixture_occurrence(case, &mut snapshot)?;\n        if case.scenario == \"metamorphic_b\" {\n            // Exercise order independence after parsing/indexing: graph construction and proof\n            // normalization must not depend on discovery/insertion order of persisted evidence.\n            snapshot.files.reverse();\n            snapshot.symbols.reverse();\n            snapshot.chunks.reverse();\n            snapshot.occurrences.reverse();\n            snapshot.imports.reverse();\n            snapshot.analysis_facts.reverse();\n            snapshot.resolved_relationships.reverse();\n        }\n        let graph = InMemoryGraph::from_index_with_resolved_relationships(\n""",
        "metamorphic snapshot reversal",
    )

    text = replace_once(
        text,
        """        normalize_observed_relationships(&mut relationships);\n        let authoritative = relationships\n""",
        """        normalize_observed_relationships(&mut relationships);\n        if case.scenario == \"multiple_exact_sites\"\n            && case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit\n        {\n            let distinct_sites = relationships\n                .iter()\n                .filter(|relationship| {\n                    relationship.authority\n                        == open_kioku_core::RelationshipAuthority::Authoritative\n                })\n                .flat_map(|relationship| relationship.source_ranges.iter())\n                .map(|range| {\n                    (\n                        range.start_line,\n                        range.start_column,\n                        range.end_line,\n                        range.end_column,\n                    )\n                })\n                .collect::<BTreeSet<_>>();\n            if distinct_sites.len() < 2 {\n                anyhow::bail!(\n                    \"case {} expected at least two exact reference sites, observed {}\",\n                    case.id,\n                    distinct_sites.len()\n                );\n            }\n        }\n        let authoritative = relationships\n""",
        "multiple exact site assertion",
    )

    text = replace_once(
        text,
        """        symbol_id: target.id,\n        file_id: source_file.id,\n""",
        """        symbol_id: target.id.clone(),\n        file_id: source_file.id.clone(),\n""",
        "clone primary reference identity",
    )
    text = replace_once(
        text,
        """        source_range: Some(range),\n""",
        """        source_range: Some(range.clone()),\n""",
        "clone primary reference range",
    )
    text = replace_once(
        text,
        """    });\n    Ok(())\n}\n\nfn write_live_relationship_fixture(\n""",
        """    });\n    if should_inject_exact && case.scenario == \"multiple_exact_sites\" {\n        let second = open_kioku_core::SourceRange {\n            start_line: range.end_line.saturating_add(1),\n            start_column: 1,\n            end_line: range.end_line.saturating_add(1),\n            end_column: 10,\n        };\n        snapshot.occurrences.push(open_kioku_core::SymbolOccurrence {\n            symbol_id: target.id,\n            file_id: source_file.id,\n            range: Some(open_kioku_core::LineRange {\n                start: second.start_line,\n                end: second.end_line,\n            }),\n            source_range: Some(second),\n            is_definition: false,\n            confidence: Confidence::Exact,\n            provenance: EvidenceSourceType::Scip,\n        });\n    }\n    Ok(())\n}\n\nfn write_live_relationship_fixture(\n""",
        "second exact reference occurrence",
    )

    text = replace_once(
        text,
        """    if case.scenario == \"skipped_path\" {\n        let skipped = root.join(\"vendor/generated/ignored.txt\");\n        fs::create_dir_all(skipped.parent().expect(\"skipped fixture has parent\"))?;\n        fs::write(skipped, \"generated fixture noise\")?;\n    }\n    Ok(())\n}\n""",
        """    if case.scenario == \"skipped_path\" {\n        // Put real relationship-shaped source in a vendor/generated path. If secure ingest ever\n        // leaks skipped source, endpoint identity or structural precision will change.\n        let relative = PathBuf::from(\"vendor/generated\").join(main_path(case.language));\n        let skipped = root.join(relative);\n        fs::create_dir_all(skipped.parent().expect(\"skipped fixture has parent\"))?;\n        fs::write(skipped, positive_call_source(case.language))?;\n    }\n    if case.scenario == \"malformed_partial\" {\n        let (path, content) = malformed_live_fixture_file(case.language);\n        let absolute = root.join(path);\n        if let Some(parent) = absolute.parent() {\n            fs::create_dir_all(parent)?;\n        }\n        fs::write(absolute, content)?;\n    }\n    Ok(())\n}\n\nfn malformed_live_fixture_file(language: RelationshipBenchLanguage) -> (PathBuf, &'static str) {\n    match language {\n        RelationshipBenchLanguage::Rust => (PathBuf::from(\"src/broken.rs\"), \"pub fn broken( {\"),\n        RelationshipBenchLanguage::TypeScript => (PathBuf::from(\"src/broken.ts\"), \"export function broken( {\"),\n        RelationshipBenchLanguage::JavaScript => (PathBuf::from(\"src/broken.js\"), \"export function broken( {\"),\n        RelationshipBenchLanguage::Python => (PathBuf::from(\"src/broken.py\"), \"def broken(:\\n    pass\\n\"),\n        RelationshipBenchLanguage::Java => (PathBuf::from(\"src/Broken.java\"), \"class Broken { void broken( { }\"),\n        RelationshipBenchLanguage::Go => (PathBuf::from(\"broken.go\"), \"package bench\\nfunc Broken( {\\n\"),\n    }\n}\n""",
        "skipped and malformed executable fixtures",
    )

    LIVE.write_text(text)


def main() -> None:
    harden_corpus(FULL)
    harden_corpus(COMPACT)
    harden_live_producer()


if __name__ == "__main__":
    main()
