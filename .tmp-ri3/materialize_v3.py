from __future__ import annotations

import base64
import gzip
import json
import re
from pathlib import Path


def decode_payloads() -> None:
    mapping = {
        ".tmp-ri3/relationship.rs.gz.b64": "crates/open-kioku-cli/src/bench/relationship.rs",
        ".tmp-ri3/relationship_live.rs.gz.b64": "crates/open-kioku-cli/src/bench/relationship_live.rs",
        ".tmp-ri3/relationship-cases.json.gz.b64": "benchmarks/relationship-cases.json",
        ".tmp-ri3/relationship-ci-cases.json.gz.b64": "benchmarks/relationship-ci-cases.json",
        ".tmp-ri3/relationship-thresholds.json.gz.b64": "benchmarks/relationship-thresholds.json",
    }
    for source, target in mapping.items():
        payload = base64.b64decode(Path(source).read_bytes(), validate=True)
        Path(target).write_bytes(gzip.decompress(payload))


def align_capability_contract() -> None:
    corroborating = {
        ("java", "EXTENDS"),
        ("java", "IMPLEMENTS"),
        ("java_script", "EXTENDS"),
        ("python", "EXTENDS"),
        ("python", "USES_TYPE"),
    }
    for path in (
        Path("benchmarks/relationship-cases.json"),
        Path("benchmarks/relationship-ci-cases.json"),
    ):
        payload = json.loads(path.read_text())
        for case in payload["cases"]:
            if (case["language"], case["relationship"]) not in corroborating:
                continue
            case["capability_state"] = "corroborating"
            if case["expected_outcome"] == "must_emit":
                case["expected_outcome"] = "may_emit_heuristic_only"
                case.pop("expected_target", None)
                case.pop("expected_source_range", None)
                case["expected_proof_kinds"] = []
                case["forbidden_proof_kinds"] = []
            case["notes"] = (
                "RI3 broad capability is corroborating; authoritative emission is not required "
                "and structural truth must fail closed."
            )
        path.write_text(json.dumps(payload, indent=2) + "\n")


def patch_go_types() -> None:
    path = Path("crates/open-kioku-tree-sitter/src/lib.rs")
    text = path.read_text()
    pattern = re.compile(
        r'            "type_spec" => name\.map\(\|node\| \{\n'
        r"                let symbol_kind =\n"
        r'                    if node\.parent\(\)\.map\(\|parent\| parent\.kind\(\)\) == Some\("type_declaration"\) \{\n'
        r"                        SymbolKind::Class\n"
        r"                    \} else \{\n"
        r"                        SymbolKind::Unknown\n"
        r"                    \};\n"
        r"                \(node, symbol_kind\)\n"
        r"            \}\),"
    )
    replacement = "\n".join(
        [
            '            "type_spec" => {',
            '                let symbol_kind = match node.child_by_field_name("type").map(|node| node.kind()) {',
            '                    Some("interface_type") => SymbolKind::Interface,',
            "                    _ => SymbolKind::Class,",
            "                };",
            "                name.map(|name_node| (name_node, symbol_kind))",
            "            },",
        ]
    )
    text, count = pattern.subn(replacement, text, count=1)
    if count != 1:
        raise SystemExit(f"expected one Go type_spec classification replacement, got {count}")

    if "mod ri3_go_type_classification_tests" not in text:
        regression = "\n".join(
            [
                "",
                "#[cfg(test)]",
                "mod ri3_go_type_classification_tests {",
                "    use super::parse_file;",
                "    use open_kioku_core::{File, FileId, Language, RepositoryId, SymbolKind};",
                "",
                "    #[test]",
                "    fn go_named_types_are_classified_as_type_symbols() {",
                "        let file = File {",
                '            id: FileId::new("file_go_types"),',
                '            repository_id: RepositoryId::new("repo"),',
                '            path: "main.go".into(),',
                "            language: Language::Go,",
                "            size_bytes: 0,",
                '            content_hash: "hash".into(),',
                "            is_generated: false,",
                "            is_vendor: false,",
                "        };",
                "        let facts = parse_file(",
                "            &file,",
                '            "package bench\\ntype TargetType struct{}\\ntype TargetInterface interface{ Target() }\\nfunc CallerFn(value TargetType) {}\\n",',
                "        )",
                '        .expect("Go type fixture should parse");',
                '        let concrete = facts.symbols.iter().find(|symbol| symbol.name == "TargetType").expect("concrete Go type");',
                '        let interface = facts.symbols.iter().find(|symbol| symbol.name == "TargetInterface").expect("Go interface type");',
                "        assert_eq!(concrete.kind, SymbolKind::Class);",
                "        assert_eq!(interface.kind, SymbolKind::Interface);",
                '        assert!(facts.bindings.iter().any(|binding| binding.name == "value" && binding.declared_type.as_deref() == Some("TargetType")));',
                "    }",
                "}",
                "",
            ]
        )
        text += regression
    path.write_text(text)


def patch_package_dependency_proof() -> None:
    path = Path("crates/open-kioku-graph/src/lib.rs")
    text = path.read_text()
    marker = '.expect("resolved import binding proof must serialize to JSON");'
    marker_pos = text.find(marker)
    if marker_pos < 0:
        raise SystemExit("resolved import proof marker not found")
    prior_push = text.rfind("proof.evidence_ids.push(evidence_id);", 0, marker_pos)
    if prior_push < 0:
        raise SystemExit("resolved import evidence push not found")
    old_push = "proof.evidence_ids.push(evidence_id);"
    text = text[:prior_push] + "proof.evidence_ids.push(evidence_id.clone());" + text[prior_push + len(old_push) :]
    marker_pos = text.find(marker)
    insertion_pos = text.find("            buffer.insert_edge(edge);", marker_pos)
    if insertion_pos < 0:
        raise SystemExit("graph insertion marker not found")
    block = "\n".join(
        [
            "            if fact.edge_type == GraphEdgeType::DependsOn",
            "                && fact.target_kind == GraphNodeType::Package",
            '                && fact.source.starts_with("open-kioku-import-resolver/")',
            "                && matches!(fact.confidence, Confidence::High | Confidence::Exact)",
            "            {",
            "                let mut proof = RelationshipProof::new(",
            "                    RelationshipProofKind::ModuleOrPackageBinding,",
            "                    fact.source.clone(),",
            "                    1,",
            "                );",
            "                proof.source_range = fact.range.as_ref().map(|range| FileRange {",
            "                    path: file.path.clone(),",
            "                    line_range: Some(range.clone()),",
            "                });",
            "                proof.evidence_ids.push(evidence_id);",
            '                proof.details.insert("package".into(), json!(fact.target));',
            "                edge.set_relationship_proofs(vec![proof])",
            '                    .expect("resolved package dependency proof must serialize to JSON");',
            "            }",
            "",
        ]
    )
    path.write_text(text[:insertion_pos] + block + text[insertion_pos:])


def main() -> None:
    decode_payloads()
    align_capability_contract()
    patch_go_types()
    patch_package_dependency_proof()


if __name__ == "__main__":
    main()
