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
        raise SystemExit(f"{path}: expected one occurrence, found {count}: {old[:100]!r}")
    write(path, text.replace(old, new, 1))


def wire_snapshot_metadata() -> None:
    path = "crates/open-kioku-cli/src/types.rs"
    old = """    open_kioku_version: String,\n    index_mode: String,\n"""
    new = """    open_kioku_version: String,\n    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n    analysis_semantics: Option<open_kioku_core::AnalysisSemanticsState>,\n    index_mode: String,\n"""
    replace_once(path, old, new)


def compatibility_expr(manifest: str) -> str:
    return (
        "open_kioku_core::classify_analysis_semantics("
        f"{manifest}.and_then(|manifest| manifest.analysis_semantics.as_ref()), "
        "&open_kioku_core::AnalysisSemanticsState::current())"
    )


def wire_snapshot_import() -> None:
    path = "crates/open-kioku-cli/src/commands/snapshot.rs"
    text = read(path)
    old = """        open_kioku_version: env!(\"CARGO_PKG_VERSION\").to_string(),\n        index_mode: manifest.index_mode.to_string(),\n"""
    new = """        open_kioku_version: env!(\"CARGO_PKG_VERSION\").to_string(),\n        analysis_semantics: manifest.analysis_semantics.clone(),\n        index_mode: manifest.index_mode.to_string(),\n"""
    if old not in text:
        raise SystemExit("snapshot metadata construction point missing")
    text = text.replace(old, new, 1)

    old = """    let temp_manifest = read_manifest_from_sqlite(&temp_db)?;\n    if temp_manifest.is_none() {\n        let _ = fs::remove_file(&temp_db);\n        anyhow::bail!(\"snapshot database has no index manifest\");\n    }\n\n    let index_path = index_sqlite_path(&repo);\n"""
    new = """    let temp_manifest = match read_manifest_from_sqlite(&temp_db)? {\n        Some(manifest) => manifest,\n        None => {\n            let _ = fs::remove_file(&temp_db);\n            anyhow::bail!(\"snapshot database has no index manifest\");\n        }\n    };\n    if metadata.analysis_semantics != temp_manifest.analysis_semantics {\n        let _ = fs::remove_file(&temp_db);\n        anyhow::bail!(\n            \"snapshot analysis-semantics metadata does not match the embedded index manifest\"\n        );\n    }\n    let current_semantics = open_kioku_core::AnalysisSemanticsState::current();\n    let compatibility = open_kioku_core::classify_analysis_semantics(\n        temp_manifest.analysis_semantics.as_ref(),\n        &current_semantics,\n    );\n    if !compatibility.status.allows_authoritative_relationships() {\n        let _ = fs::remove_file(&temp_db);\n        anyhow::bail!(\n            \"snapshot analysis semantics are {:?}: {}; stored={}, current={}; {}\",\n            compatibility.status,\n            compatibility.reasons.join(\"; \"),\n            compatibility.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n            compatibility.current_fingerprint,\n            compatibility.recommended_action\n        );\n    }\n\n    let index_path = index_sqlite_path(&repo);\n"""
    if old not in text:
        raise SystemExit("snapshot import manifest validation point missing")
    text = text.replace(old, new, 1)

    # Snapshot doctor validates semantic identity without mutating or promoting the artifact.
    old = """                            Ok(_) => {}\n                            Err(err) => errors.push(err.to_string()),\n                        }\n                    }\n                }\n"""
    new = """                            Ok(_) => {}\n                            Err(err) => errors.push(err.to_string()),\n                        }\n                    }\n                    match read_manifest_from_sqlite(&temp_db) {\n                        Ok(Some(manifest)) => {\n                            if let Some(metadata) = &metadata {\n                                if metadata.analysis_semantics != manifest.analysis_semantics {\n                                    errors.push(\"snapshot analysis-semantics metadata does not match the embedded index manifest\".into());\n                                }\n                            }\n                            let compatibility = open_kioku_core::classify_analysis_semantics(\n                                manifest.analysis_semantics.as_ref(),\n                                &open_kioku_core::AnalysisSemanticsState::current(),\n                            );\n                            if !compatibility.status.allows_authoritative_relationships() {\n                                errors.push(format!(\n                                    \"snapshot analysis semantics are {:?}: {}; {}\",\n                                    compatibility.status,\n                                    compatibility.reasons.join(\"; \"),\n                                    compatibility.recommended_action\n                                ));\n                            }\n                        }\n                        Ok(None) => errors.push(\"snapshot database has no index manifest\".into()),\n                        Err(err) => errors.push(err.to_string()),\n                    }\n                }\n"""
    if text.count(old) != 1:
        raise SystemExit(f"snapshot doctor validation point count={text.count(old)}")
    text = text.replace(old, new, 1)
    write(path, text)


def wire_cli_status_and_doctor() -> None:
    path = "crates/open-kioku-cli/src/reports/status_setup_doctor.rs"
    text = read(path)
    marker = """fn render_status_markdown(\n"""
    helper = """fn analysis_semantics_compatibility_for_manifest(\n    manifest: Option<&IndexManifest>,\n) -> open_kioku_core::AnalysisSemanticsCompatibility {\n    open_kioku_core::classify_analysis_semantics(\n        manifest.and_then(|manifest| manifest.analysis_semantics.as_ref()),\n        &open_kioku_core::AnalysisSemanticsState::current(),\n    )\n}\n\nfn render_status_markdown(\n"""
    if "fn analysis_semantics_compatibility_for_manifest(" not in text:
        if marker not in text:
            raise SystemExit("status helper insertion point missing")
        text = text.replace(marker, helper, 1)

    old = """        out.push_str(&format!(\"| Mode | `{}` |\\n\", manifest.index_mode));\n        out.push_str(&format!(\"| Files | {} |\\n\", manifest.file_count));\n"""
    new = """        out.push_str(&format!(\"| Mode | `{}` |\\n\", manifest.index_mode));\n        let semantics = analysis_semantics_compatibility_for_manifest(Some(manifest));\n        out.push_str(&format!(\"| Analysis semantics | `{:?}` |\\n\", semantics.status));\n        out.push_str(&format!(\"| Stored semantics fingerprint | `{}` |\\n\", semantics.stored_fingerprint.as_deref().unwrap_or(\"missing\")));\n        out.push_str(&format!(\"| Current semantics fingerprint | `{}` |\\n\", semantics.current_fingerprint));\n        out.push_str(&format!(\"| Files | {} |\\n\", manifest.file_count));\n"""
    if old not in text:
        raise SystemExit("status markdown semantic insertion point missing")
    text = text.replace(old, new, 1)

    # Add a deterministic doctor check after basic index presence validation.
    marker = """    if index_path.exists() {\n        if let Ok(store) = SqliteStore::open(&index_path) {\n"""
    semantic_check = """    if let Ok(Some(manifest)) = load_index_manifest(&repo) {\n        let compatibility = analysis_semantics_compatibility_for_manifest(Some(&manifest));\n        let compatible = compatibility.status.allows_authoritative_relationships();\n        checks.push(DoctorCheck {\n            name: \"analysis-semantics\",\n            status: if compatible { CheckStatus::Pass } else { CheckStatus::Fail },\n            message: if compatible {\n                format!(\n                    \"compatible; fingerprint {}\",\n                    compatibility.current_fingerprint\n                )\n            } else {\n                format!(\n                    \"{:?}: {}; stored={}, current={}; affected components [{}], languages [{}]; {}\",\n                    compatibility.status,\n                    compatibility.reasons.join(\"; \"),\n                    compatibility.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n                    compatibility.current_fingerprint,\n                    compatibility.affected_components.join(\", \"),\n                    compatibility.affected_languages.join(\", \"),\n                    compatibility.recommended_action\n                )\n            },\n        });\n        if !compatible {\n            next_steps.push(compatibility.recommended_action.clone());\n        }\n    }\n\n    if index_path.exists() {\n        if let Ok(store) = SqliteStore::open(&index_path) {\n"""
    if "name: \"analysis-semantics\"" not in text:
        if marker not in text:
            raise SystemExit("doctor semantic check insertion point missing")
        text = text.replace(marker, semantic_check, 1)
    write(path, text)

    path = "crates/open-kioku-cli/src/commands/mod.rs"
    text = read(path)
    old = """            } else if cli.json {\n                println!(\"{}\", serde_json::to_string_pretty(&manifest)?);\n            } else if let Some(manifest) = manifest {\n"""
    new = """            } else if cli.json {\n                let compatibility = analysis_semantics_compatibility_for_manifest(manifest.as_ref());\n                let mut status = serde_json::to_value(&manifest)?;\n                if let Some(object) = status.as_object_mut() {\n                    object.insert(\"analysis_semantics_status\".into(), serde_json::to_value(compatibility)?);\n                }\n                println!(\"{}\", serde_json::to_string_pretty(&status)?);\n            } else if let Some(manifest) = manifest {\n"""
    if old not in text:
        raise SystemExit("CLI JSON status insertion point missing")
    text = text.replace(old, new, 1)
    old = """                if let Some(report) = manifest.quality.resolution_quality.as_ref() {\n"""
    new = """                let semantics = analysis_semantics_compatibility_for_manifest(Some(&manifest));\n                println!(\n                    \"Analysis semantics: {:?}; stored={}, current={}\",\n                    semantics.status,\n                    semantics.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n                    semantics.current_fingerprint\n                );\n                if !semantics.status.allows_authoritative_relationships() {\n                    println!(\"Relationship authority unavailable: {}\", semantics.reasons.join(\"; \"));\n                    println!(\"Recommended action: {}\", semantics.recommended_action);\n                }\n                if let Some(report) = manifest.quality.resolution_quality.as_ref() {\n"""
    # There are multiple resolution-quality renderers; target the first occurrence in status branch
    # after the healthy index line.
    idx = text.find('                println!(\n                    "Healthy index:')
    if idx < 0:
        raise SystemExit("CLI human status branch missing")
    tail = text[idx:]
    if old not in tail:
        raise SystemExit("CLI human status semantics insertion point missing")
    tail = tail.replace(old, new, 1)
    text = text[:idx] + tail
    write(path, text)


def wire_context_fail_closed() -> None:
    path = "crates/open-kioku-context/src/candidates/builtins.rs"
    text = read(path)
    helper_marker = """impl<'a> BuiltinCandidateContext<'a> {\n"""
    helper = """impl<'a> BuiltinCandidateContext<'a> {\n    fn authoritative_relationships_available(&self) -> Result<(), String> {\n        let manifest = self\n            .store\n            .manifest()\n            .map_err(|err| format!(\"index manifest unavailable: {err}\"))?;\n        let compatibility = open_kioku_core::classify_analysis_semantics(\n            manifest\n                .as_ref()\n                .and_then(|manifest| manifest.analysis_semantics.as_ref()),\n            &open_kioku_core::AnalysisSemanticsState::current(),\n        );\n        if compatibility.status.allows_authoritative_relationships() {\n            Ok(())\n        } else {\n            Err(format!(\n                \"analysis semantics {:?}: {}; stored={}, current={}; {}\",\n                compatibility.status,\n                compatibility.reasons.join(\"; \"),\n                compatibility.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n                compatibility.current_fingerprint,\n                compatibility.recommended_action\n            ))\n        }\n    }\n\n"""
    if "fn authoritative_relationships_available(&self)" not in text:
        if helper_marker not in text:
            raise SystemExit("context semantic helper insertion point missing")
        text = text.replace(helper_marker, helper, 1)

    old = """    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {\n        let keys = symbol_anchor_keys(&request.task, &request.search_terms);\n"""
    new = """    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {\n        if let Err(reason) = self.authoritative_relationships_available() {\n            return CandidateStream::unavailable(\n                RetrievalSourceKind::ExactSemantic,\n                format!(\"exact semantic retrieval unavailable: {reason}\"),\n            );\n        }\n        let keys = symbol_anchor_keys(&request.task, &request.search_terms);\n"""
    if old not in text:
        raise SystemExit("exact stream gate insertion point missing")
    text = text.replace(old, new, 1)

    old = """    fn graph_stream(\n        &self,\n        request: &CandidateRequest,\n        anchor_symbols: &[Symbol],\n    ) -> CandidateStream {\n        if anchor_symbols.is_empty() {\n"""
    new = """    fn graph_stream(\n        &self,\n        request: &CandidateRequest,\n        anchor_symbols: &[Symbol],\n    ) -> CandidateStream {\n        if let Err(reason) = self.authoritative_relationships_available() {\n            return CandidateStream::unavailable(\n                RetrievalSourceKind::Graph,\n                format!(\"graph retrieval unavailable: {reason}\"),\n            );\n        }\n        if anchor_symbols.is_empty() {\n"""
    if old not in text:
        raise SystemExit("graph stream gate insertion point missing")
    text = text.replace(old, new, 1)
    write(path, text)


def wire_mcp_fail_closed() -> None:
    path = "crates/open-kioku-mcp/src/lib.rs"
    text = read(path)
    marker = """async fn dispatch(\n"""
    helper = """fn analysis_semantics_compatibility_for_store(\n    store: &SqliteStore,\n) -> anyhow::Result<open_kioku_core::AnalysisSemanticsCompatibility> {\n    let manifest = store.manifest()?;\n    Ok(open_kioku_core::classify_analysis_semantics(\n        manifest\n            .as_ref()\n            .and_then(|manifest| manifest.analysis_semantics.as_ref()),\n        &open_kioku_core::AnalysisSemanticsState::current(),\n    ))\n}\n\nfn require_authoritative_relationships(store: &SqliteStore) -> anyhow::Result<()> {\n    let compatibility = analysis_semantics_compatibility_for_store(store)?;\n    if compatibility.status.allows_authoritative_relationships() {\n        return Ok(());\n    }\n    anyhow::bail!(\n        \"authoritative relationship evidence unavailable: analysis semantics {:?}: {}; stored={}, current={}; affected components [{}], languages [{}]; {}\",\n        compatibility.status,\n        compatibility.reasons.join(\"; \"),\n        compatibility.stored_fingerprint.as_deref().unwrap_or(\"missing\"),\n        compatibility.current_fingerprint,\n        compatibility.affected_components.join(\", \"),\n        compatibility.affected_languages.join(\", \"),\n        compatibility.recommended_action\n    )\n}\n\nasync fn dispatch(\n"""
    if "fn analysis_semantics_compatibility_for_store(" not in text:
        if marker not in text:
            raise SystemExit("MCP semantic helper insertion point missing")
        text = text.replace(marker, helper, 1)

    old = '        "repo_status" => Ok(json!(store.manifest()?)),\n'
    new = '''        "repo_status" => {\n            let manifest = store.manifest()?;\n            let compatibility = analysis_semantics_compatibility_for_store(store)?;\n            let mut status = serde_json::to_value(&manifest)?;\n            if let Some(object) = status.as_object_mut() {\n                object.insert(\n                    "analysis_semantics_status".into(),\n                    serde_json::to_value(compatibility)?,\n                );\n            }\n            Ok(status)\n        }\n'''
    if old not in text:
        raise SystemExit("MCP repo_status insertion point missing")
    text = text.replace(old, new, 1)

    # Direct MCP surfaces that depend on persisted relationship/exact-reference truth.
    gates = [
        ('        "impact_analysis" => {\n', '        "impact_analysis" => {\n            require_authoritative_relationships(store)?;\n'),
        ('        "get_references" => {\n', '        "get_references" => {\n            require_authoritative_relationships(store)?;\n'),
        ('        "get_callers" | "get_callees" => {\n', '        "get_callers" | "get_callees" => {\n            require_authoritative_relationships(store)?;\n'),
        ('        "dependency_path" => {\n', '        "dependency_path" => {\n            require_authoritative_relationships(store)?;\n'),
        ('        "module_dependencies" => {\n', '        "module_dependencies" => {\n            require_authoritative_relationships(store)?;\n'),
        ('        "query_evidence_graph" => {\n', '        "query_evidence_graph" => {\n            require_authoritative_relationships(store)?;\n'),
    ]
    for old, new in gates:
        if old not in text:
            raise SystemExit(f"MCP gate insertion point missing: {old.strip()}")
        text = text.replace(old, new, 1)

    old = """            object.insert(\n                \"relationship_semantic_capabilities\".into(),\n                json!(capabilities),\n            );\n            Ok(schema)\n"""
    new = """            object.insert(\n                \"relationship_semantic_capabilities\".into(),\n                json!(capabilities),\n            );\n            object.insert(\n                \"analysis_semantics_status\".into(),\n                serde_json::to_value(analysis_semantics_compatibility_for_store(store)?)?,\n            );\n            Ok(schema)\n"""
    if old not in text:
        raise SystemExit("MCP evidence schema semantics insertion point missing")
    text = text.replace(old, new, 1)
    write(path, text)


wire_snapshot_metadata()
wire_snapshot_import()
wire_cli_status_and_doctor()
wire_context_fail_closed()
wire_mcp_fail_closed()
print("RI3 semantic read-safety integration staged")
