#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# CLI graph-query/search paths are direct authoritative relationship consumers and must obey
# the same generation boundary as MCP/context consumers.
path = ROOT / "crates/open-kioku-cli/src/commands/mod.rs"
text = path.read_text()
marker = '''fn relationship_resolution_summary_lines(
'''
helper = '''fn require_current_analysis_semantics(
    store: &SqliteStore,
) -> anyhow::Result<()> {
    let manifest = open_kioku_storage::MetadataStore::manifest(store)?;
    let compatibility = open_kioku_core::classify_analysis_semantics(
        manifest
            .as_ref()
            .and_then(|manifest| manifest.analysis_semantics.as_ref()),
        &open_kioku_core::AnalysisSemanticsState::current(),
    );
    if compatibility.status.allows_authoritative_relationships() {
        return Ok(());
    }
    anyhow::bail!(
        "authoritative relationship evidence unavailable: analysis semantics {:?}: {}; stored={}, current={}; affected components [{}], languages [{}]; {}",
        compatibility.status,
        compatibility.reasons.join("; "),
        compatibility.stored_fingerprint.as_deref().unwrap_or("missing"),
        compatibility.current_fingerprint,
        compatibility.affected_components.join(", "),
        compatibility.affected_languages.join(", "),
        compatibility.recommended_action
    )
}

fn relationship_resolution_summary_lines(
'''
if "fn require_current_analysis_semantics(" not in text:
    if marker not in text:
        raise SystemExit("CLI semantics helper insertion point missing")
    text = text.replace(marker, helper, 1)

old = '''            } => {
                let store = open_store(&repo)?;
                let ast = open_kioku_graph::query::parse_graph_query(&dsl)?;
'''
new = '''            } => {
                let store = open_store(&repo)?;
                require_current_analysis_semantics(&store)?;
                let ast = open_kioku_graph::query::parse_graph_query(&dsl)?;
'''
# Limit replacement to GraphCommand::Query section.
anchor = text.find("Command::Graph { command }")
if anchor < 0:
    raise SystemExit("CLI Graph command missing")
tail = text[anchor:]
if old not in tail:
    raise SystemExit("CLI graph query gate insertion point missing")
tail = tail.replace(old, new, 1)
text = text[:anchor] + tail

old = '''            let results = if matches!(kind, SearchKind::Graph) {
                graph_search(&repo, &query, limit)?
'''
new = '''            let results = if matches!(kind, SearchKind::Graph) {
                require_current_analysis_semantics(&store)?;
                graph_search(&repo, &query, limit)?
'''
if old not in text:
    raise SystemExit("CLI graph-search gate insertion point missing")
text = text.replace(old, new, 1)
path.write_text(text)

# Architecture outputs evaluate stored dependency/relationship edges. Config syntax validation stays
# usable, but architecture findings must not present stale graph truth as authoritative.
path = ROOT / "crates/open-kioku-mcp/src/lib.rs"
text = path.read_text()
replacements = [
    (
        '        "detect_architecture" => Ok(json!(ArchitectureDetector::new(store, None).detect()?)),\n',
        '''        "detect_architecture" => {
            require_authoritative_relationships(store)?;
            Ok(json!(ArchitectureDetector::new(store, None).detect()?))
        }
''',
    ),
    (
        '''        "architecture_boundaries" | "architecture_violations" => {
            architecture_summary_tool(repo, store)
        }
''',
        '''        "architecture_boundaries" | "architecture_violations" => {
            require_authoritative_relationships(store)?;
            architecture_summary_tool(repo, store)
        }
''',
    ),
    (
        '''        "architecture_policy_check" => {
            let Some(policy) = load_architecture_policy(repo)? else {
''',
        '''        "architecture_policy_check" => {
            require_authoritative_relationships(store)?;
            let Some(policy) = load_architecture_policy(repo)? else {
''',
    ),
    (
        '''        "architecture_policy_explain" => {
            let Some(policy) = load_architecture_policy(repo)? else {
''',
        '''        "architecture_policy_explain" => {
            require_authoritative_relationships(store)?;
            let Some(policy) = load_architecture_policy(repo)? else {
''',
    ),
    (
        '        "summarize_architecture" => architecture_summary_tool(repo, store),\n',
        '''        "summarize_architecture" => {
            require_authoritative_relationships(store)?;
            architecture_summary_tool(repo, store)
        }
''',
    ),
]
for old, new in replacements:
    if old not in text:
        raise SystemExit(f"MCP architecture gate point missing: {old.splitlines()[0].strip()}")
    text = text.replace(old, new, 1)
path.write_text(text)
print("RI3 final authoritative read gates staged")
