from pathlib import Path

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

# Add a regression at the end of the core tests module without depending on runtime timing values.
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
