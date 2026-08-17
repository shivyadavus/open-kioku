from pathlib import Path

# Branch-local validator: separate nondeterministic wall-clock telemetry from durable semantic quality.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
old = '''#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LanguageResolutionQuality {
    pub occurrences: usize,
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub candidate_cap_hits: usize,
    pub enrichment_time_us: u64,
}
'''
new = '''#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LanguageResolutionQuality {
    pub occurrences: usize,
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub candidate_cap_hits: usize,
    /// Observational wall-clock telemetry for the current process only.
    ///
    /// Runtime timing is intentionally excluded from the durable quality contract and equality:
    /// identical repository evidence must produce identical manifests regardless of scheduler or
    /// machine noise. The value remains available to callers inspecting the live report.
    #[serde(skip, default)]
    #[schemars(skip)]
    pub enrichment_time_us: u64,
}

impl PartialEq for LanguageResolutionQuality {
    fn eq(&self, other: &Self) -> bool {
        self.occurrences == other.occurrences
            && self.candidates_considered == other.candidates_considered
            && self.proven == other.proven
            && self.ambiguous == other.ambiguous
            && self.unresolved == other.unresolved
            && self.external == other.external
            && self.candidate_cap_hits == other.candidate_cap_hits
    }
}

impl Eq for LanguageResolutionQuality {}
'''
if text.count(old) != 1:
    raise SystemExit(f'LanguageResolutionQuality marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '\n}\n'
pos = text.rfind(marker)
if pos < 0:
    raise SystemExit('could not locate final module terminator')
test = '''

    #[test]
    fn language_resolution_wall_clock_telemetry_is_not_durable_semantic_quality() {
        let mut left = LanguageResolutionQuality {
            occurrences: 4,
            candidates_considered: 9,
            proven: 3,
            ambiguous: 1,
            unresolved: 0,
            external: 0,
            candidate_cap_hits: 0,
            enrichment_time_us: 10,
        };
        let mut right = left.clone();
        right.enrichment_time_us = 99_999;

        assert_eq!(left, right, "wall-clock jitter must not alter semantic quality equality");
        let left_json = serde_json::to_value(&left).unwrap();
        let right_json = serde_json::to_value(&right).unwrap();
        assert_eq!(left_json, right_json, "durable quality serialization must be deterministic");
        assert!(left_json.get("enrichment_time_us").is_none());

        left.proven += 1;
        assert_ne!(left, right, "semantic evidence changes must remain observable");
    }
'''
text = text[:pos] + test + text[pos:]
path.write_text(text)

# The MCP tool-list snapshot is a public contract. PR #263 intentionally enriches the
# get_evidence_schema description, so keep the integration snapshot in lock-step rather than
# weakening the golden test or regenerating unrelated entries.
path = Path('crates/open-kioku-tests/snapshots/tools_list.json')
text = path.read_text()
old = '''Retrieve the versioned schema defining the supported graph node types, edge types, and query properties available in the repository's structural evidence graph. Use before query_evidence_graph to learn available graph node types, edge types, and properties. This is read-only and does not query graph data.'''
new = '''Retrieve the versioned schema defining supported graph types, query properties, and the Tier-1 relationship-semantic capability matrix. Use before query_evidence_graph to learn available graph node types, edge types, properties, and the versioned Tier-1 relationship-semantic capability matrix. This is read-only and does not query graph data.'''
if text.count(old) != 1:
    raise SystemExit(f'tools_list get_evidence_schema snapshot marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))
