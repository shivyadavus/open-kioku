from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


# Preserve full occurrence coordinates additively while keeping the legacy line-only range readable.
replace_exact(
    "crates/open-kioku-core/src/lib.rs",
    '''pub struct SymbolOccurrence {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    pub is_definition: bool,
''',
    '''pub struct SymbolOccurrence {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<SourceRange>,
    pub is_definition: bool,
''',
    "SymbolOccurrence exact source range",
)

# Heuristic occurrence generation is definition-only and cannot claim occurrence-level columns.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''            range: symbol.range.clone(),
            is_definition: true,
''',
    '''            range: symbol.range.clone(),
            source_range: None,
            is_definition: true,
''',
    "heuristic occurrence exact range",
)

# SCIP is an exact occurrence source. Retain its 0-based coordinates as Open Kioku 1-based SourceRange.
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, Symbol, SymbolId,
    SymbolKind, SymbolOccurrence,
''',
    '''    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, SourceRange,
    Symbol, SymbolId, SymbolKind, SymbolOccurrence,
''',
    "SCIP SourceRange import",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''                range: scip_range(&occurrence.range),
                is_definition: has_role(occurrence.symbol_roles, SymbolRole::Definition),
''',
    '''                range: scip_range(&occurrence.range),
                source_range: scip_source_range(&occurrence.range),
                is_definition: has_role(occurrence.symbol_roles, SymbolRole::Definition),
''',
    "SCIP exact occurrence range",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''    occurrences.sort_by(|a, b| {
        (
            &a.symbol_id.0,
            &a.file_id.0,
            a.range.as_ref().map(|range| range.start),
            a.is_definition,
        )
            .cmp(&(
                &b.symbol_id.0,
                &b.file_id.0,
                b.range.as_ref().map(|range| range.start),
                b.is_definition,
            ))
    });
    occurrences.dedup_by(|a, b| {
        a.symbol_id == b.symbol_id
            && a.file_id == b.file_id
            && a.range == b.range
            && a.is_definition == b.is_definition
    });
''',
    '''    occurrences.sort_by(|a, b| {
        (
            &a.symbol_id.0,
            &a.file_id.0,
            source_range_key(a.source_range.as_ref()),
            a.range.as_ref().map(|range| (range.start, range.end)),
            a.is_definition,
        )
            .cmp(&(
                &b.symbol_id.0,
                &b.file_id.0,
                source_range_key(b.source_range.as_ref()),
                b.range.as_ref().map(|range| (range.start, range.end)),
                b.is_definition,
            ))
    });
    occurrences.dedup_by(|a, b| {
        a.symbol_id == b.symbol_id
            && a.file_id == b.file_id
            && a.source_range == b.source_range
            && a.range == b.range
            && a.is_definition == b.is_definition
    });
''',
    "SCIP exact occurrence dedup",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''fn scip_range(range: &[i32]) -> Option<LineRange> {
    match range {
        [start_line, _, end_line, _] => Some(LineRange {
            start: (*start_line + 1).max(1) as u32,
            end: (*end_line + 1).max(1) as u32,
        }),
        [start_line, _, _] => Some(LineRange::single((*start_line + 1).max(1) as u32)),
        _ => None,
    }
}
''',
    '''fn source_range_key(range: Option<&SourceRange>) -> Option<(u32, u32, u32, u32)> {
    range.map(|range| {
        (
            range.start_line,
            range.start_column,
            range.end_line,
            range.end_column,
        )
    })
}

fn scip_source_range(range: &[i32]) -> Option<SourceRange> {
    let one_based = |value: i32| (value + 1).max(1) as u32;
    match range {
        [start_line, start_column, end_line, end_column] => Some(SourceRange {
            start_line: one_based(*start_line),
            start_column: one_based(*start_column),
            end_line: one_based(*end_line),
            end_column: one_based(*end_column),
        }),
        [line, start_column, end_column] => Some(SourceRange {
            start_line: one_based(*line),
            start_column: one_based(*start_column),
            end_line: one_based(*line),
            end_column: one_based(*end_column),
        }),
        _ => None,
    }
}

fn scip_range(range: &[i32]) -> Option<LineRange> {
    scip_source_range(range).map(|range| LineRange {
        start: range.start_line,
        end: range.end_line,
    })
}
''',
    "SCIP SourceRange conversion",
)

# Add focused SCIP regressions for column preservation and same-line deduplication.
scip = Path("crates/open-kioku-scip/src/lib.rs")
text = scip.read_text()
text += '''

#[cfg(test)]
mod ri3_exact_reference_tests {
    use super::*;

    fn occurrence(column: u32) -> SymbolOccurrence {
        SymbolOccurrence {
            symbol_id: SymbolId::new("symbol:target"),
            file_id: FileId::new("file:src/lib.rs"),
            range: Some(LineRange::single(10)),
            source_range: Some(SourceRange {
                start_line: 10,
                start_column: column,
                end_line: 10,
                end_column: column + 3,
            }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        }
    }

    #[test]
    fn scip_source_range_preserves_same_line_columns() {
        assert_eq!(
            scip_source_range(&[9, 4, 8]),
            Some(SourceRange {
                start_line: 10,
                start_column: 5,
                end_line: 10,
                end_column: 9,
            })
        );
        assert_eq!(
            scip_source_range(&[9, 4, 11, 2]),
            Some(SourceRange {
                start_line: 10,
                start_column: 5,
                end_line: 12,
                end_column: 3,
            })
        );
    }

    #[test]
    fn dedup_keeps_distinct_same_line_reference_occurrences() {
        let mut symbols = Vec::new();
        let mut occurrences = vec![occurrence(5), occurrence(20), occurrence(5)];
        dedup_import(&mut symbols, &mut occurrences);
        assert_eq!(occurrences.len(), 2);
        assert_eq!(occurrences[0].source_range.as_ref().unwrap().start_column, 5);
        assert_eq!(occurrences[1].source_range.as_ref().unwrap().start_column, 20);
    }
}
'''
scip.write_text(text)

# Exact SCIP occurrences become typed authoritative REFERENCES; exact columns stay structured.
graph = "crates/open-kioku-graph/src/lib.rs"
replace_exact(
    graph,
    '''            let from = identity::file_node_id(&file.path);
            let to = identity::symbol_node_id(symbol);
            buffer.insert_edge(GraphEdge {
                id: identity::edge_id(GraphEdgeType::References, &from, &to, None),
                from,
                to,
                edge_type: GraphEdgeType::References,
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!(
                        "occurrence-evidence:{}:{}",
                        file.id.0, symbol.id.0
                    ))),
                    source: "open-kioku-graph".into(),
                    source_type: occurrence.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: occurrence.confidence,
                    message: format!("{} references {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
''',
    '''            let from = identity::file_node_id(&file.path);
            let to = identity::symbol_node_id(symbol);
            let occurrence_key = occurrence
                .source_range
                .as_ref()
                .map(|range| {
                    format!(
                        "{}:{}:{}:{}",
                        range.start_line,
                        range.start_column,
                        range.end_line,
                        range.end_column
                    )
                })
                .unwrap_or_else(|| {
                    occurrence
                        .range
                        .as_ref()
                        .map(|range| format!("{}:{}", range.start, range.end))
                        .unwrap_or_else(|| "unknown".into())
                });
            let evidence_id = EvidenceId::new(stable_id(&format!(
                "occurrence-evidence:{}:{}:{}",
                file.id.0, symbol.id.0, occurrence_key
            )));
            let mut properties = BTreeMap::new();
            if let Some(range) = &occurrence.source_range {
                properties.insert(
                    "reference_sites".into(),
                    json!([{
                        "path": file.path.to_string_lossy(),
                        "start_line": range.start_line,
                        "start_column": range.start_column,
                        "end_line": range.end_line,
                        "end_column": range.end_column,
                    }]),
                );
            }
            let mut edge = GraphEdge {
                id: identity::edge_id(GraphEdgeType::References, &from, &to, None),
                from,
                to,
                edge_type: GraphEdgeType::References,
                properties,
                evidence: Evidence {
                    id: evidence_id.clone(),
                    source: "open-kioku-graph".into(),
                    source_type: occurrence.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: occurrence.confidence,
                    message: format!("{} references {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            };
            if occurrence.provenance == EvidenceSourceType::Scip
                && occurrence.confidence == Confidence::Exact
            {
                if let Some(range) = &occurrence.source_range {
                    let mut proof = RelationshipProof::new(
                        RelationshipProofKind::ExactOccurrence,
                        "scip_exact_occurrence",
                        1,
                    );
                    proof.source_range = Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    });
                    proof.target_symbol_id = Some(symbol.id.clone());
                    proof.evidence_ids.push(evidence_id);
                    proof.details.insert("start_line".into(), json!(range.start_line));
                    proof
                        .details
                        .insert("start_column".into(), json!(range.start_column));
                    proof.details.insert("end_line".into(), json!(range.end_line));
                    proof
                        .details
                        .insert("end_column".into(), json!(range.end_column));
                    edge.set_relationship_proofs(vec![proof])
                        .expect("SCIP occurrence proof must serialize to JSON");
                }
            }
            buffer.insert_edge(edge);
''',
    "exact SCIP reference graph emission",
)

# Existing graph occurrence fixtures are explicitly non-exact until their source range is supplied.
p = Path(graph)
text = p.read_text()
needle = '''            range: Some(LineRange { start: 10, end: 10 }),
            is_definition: false,
'''
if text.count(needle) < 1:
    raise SystemExit("graph occurrence fixture seam changed")
text = text.replace(
    needle,
    '''            range: Some(LineRange { start: 10, end: 10 }),
            source_range: None,
            is_definition: false,
''',
)
needle12 = '''            range: Some(LineRange { start: 12, end: 12 }),
            is_definition: false,
'''
text = text.replace(
    needle12,
    '''            range: Some(LineRange { start: 12, end: 12 }),
            source_range: None,
            is_definition: false,
''',
)
p.write_text(text)

# Add exact-reference authority/adversarial regression after the existing duplicate-reference test.
p = Path(graph)
text = p.read_text()
anchor = '''        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn test_analysis_fact_edges_survive_buffering() {
'''
insert = '''        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].relationship_authority(),
            RelationshipAuthority::Heuristic
        );
    }

    #[test]
    fn exact_scip_references_preserve_columns_and_become_authoritative() {
        let file = make_file("src/reference");
        let symbol = make_symbol("target", "src/reference", "target");
        let make_occurrence = |start_column| SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange::single(10)),
            source_range: Some(SourceRange {
                start_line: 10,
                start_column,
                end_line: 10,
                end_column: start_column + 4,
            }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        };
        let graph = InMemoryGraph::from_index_with_occurrences(
            &[file],
            &[symbol],
            &[],
            &[make_occurrence(5), make_occurrence(20)],
        );
        let refs = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == GraphEdgeType::References)
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].relationship_authority(),
            RelationshipAuthority::Authoritative
        );
        let sites = refs[0]
            .properties
            .get("reference_sites")
            .and_then(serde_json::Value::as_array)
            .expect("exact reference edge should retain structured sites");
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["start_column"], 5);
        assert_eq!(sites[1]["start_column"], 20);
        let proofs = refs[0].relationship_proofs();
        assert_eq!(proofs.len(), 2);
        assert!(proofs
            .iter()
            .all(|proof| proof.kind == RelationshipProofKind::ExactOccurrence));
    }

    #[test]
    fn exact_non_scip_reference_range_does_not_create_structural_authority() {
        let file = make_file("src/reference_lsp");
        let symbol = make_symbol("target_lsp", "src/reference_lsp", "target");
        let occurrence = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange::single(10)),
            source_range: Some(SourceRange {
                start_line: 10,
                start_column: 5,
                end_line: 10,
                end_column: 9,
            }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };
        let graph =
            InMemoryGraph::from_index_with_occurrences(&[file], &[symbol], &[], &[occurrence]);
        let reference = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == GraphEdgeType::References)
            .expect("reference edge should exist as non-authoritative evidence");
        assert_eq!(
            reference.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
        assert!(reference.relationship_proofs().is_empty());
    }

    #[test]
    fn test_analysis_fact_edges_survive_buffering() {
'''
if text.count(anchor) != 1:
    raise SystemExit(f"reference test insertion seam changed: {text.count(anchor)}")
p.write_text(text.replace(anchor, insert, 1))

# Merge exact reference sites deterministically alongside call sites.
buffer = "crates/open-kioku-graph/src/buffer.rs"
replace_exact(
    buffer,
    '''fn push_unique_call_site(call_sites: &mut Vec<serde_json::Value>, site: serde_json::Value) {
    if !call_sites.contains(&site) {
        call_sites.push(site);
    }
}
''',
    '''fn push_unique_site(sites: &mut Vec<serde_json::Value>, site: serde_json::Value) {
    if !sites.contains(&site) {
        sites.push(site);
    }
}

fn structured_site_key(site: &serde_json::Value) -> (String, u64, u64, u64, u64) {
    let number = |key: &str| site.get(key).and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX);
    (
        site.get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        number("start_line"),
        number("start_column"),
        number("end_line"),
        number("end_column"),
    )
}

fn normalize_structured_sites(sites: &mut Vec<serde_json::Value>) {
    sites.sort_by_key(structured_site_key);
    sites.dedup();
}
''',
    "generic structured site helper",
)
# Rename existing helper calls in call-site logic.
p = Path(buffer)
text = p.read_text().replace("push_unique_call_site(", "push_unique_site(")
p.write_text(text)
replace_exact(
    buffer,
    '''    if !call_sites.is_empty() {
        existing.properties.insert(
            "call_sites".to_string(),
            serde_json::Value::Array(call_sites),
        );
    }

    for (k, v) in incoming.properties {
        if k != "call_sites" && k != RELATIONSHIP_PROOFS_PROPERTY {
''',
    '''    if !call_sites.is_empty() {
        normalize_structured_sites(&mut call_sites);
        existing.properties.insert(
            "call_sites".to_string(),
            serde_json::Value::Array(call_sites),
        );
    }

    let mut reference_sites = existing
        .properties
        .get("reference_sites")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(incoming_sites) = incoming
        .properties
        .get("reference_sites")
        .and_then(|value| value.as_array())
    {
        for site in incoming_sites {
            push_unique_site(&mut reference_sites, site.clone());
        }
    }
    if !reference_sites.is_empty() {
        normalize_structured_sites(&mut reference_sites);
        existing.properties.insert(
            "reference_sites".to_string(),
            serde_json::Value::Array(reference_sites),
        );
    }

    for (k, v) in incoming.properties {
        if k != "call_sites"
            && k != "reference_sites"
            && k != RELATIONSHIP_PROOFS_PROPERTY
        {
''',
    "reference site merge",
)
