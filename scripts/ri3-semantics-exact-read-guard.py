from pathlib import Path

path = Path('crates/open-kioku-storage-sqlite/src/lib.rs')
text = path.read_text()

assert 'require_authoritative_graph_semantics' in text
text = text.replace(
    'require_authoritative_graph_semantics',
    'require_authoritative_relationship_semantics',
)

replacements = [
    (
        '''    fn imports(&self) -> Result<Vec<Import>> {\n        let conn = self''',
        '''    fn imports(&self) -> Result<Vec<Import>> {\n        require_authoritative_relationship_semantics(self)?;\n        let conn = self''',
    ),
    (
        '''    fn implementation_facts_for_target(\n        &self,\n        target: &str,\n        limit: usize,\n    ) -> Result<Vec<AnalysisFact>> {\n        let target = target.trim();''',
        '''    fn implementation_facts_for_target(\n        &self,\n        target: &str,\n        limit: usize,\n    ) -> Result<Vec<AnalysisFact>> {\n        require_authoritative_relationship_semantics(self)?;\n        let target = target.trim();''',
    ),
    (
        '''    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>> {\n        let conn = self''',
        '''    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>> {\n        require_authoritative_relationship_semantics(self)?;\n        let conn = self''',
    ),
    (
        '''    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {\n        let conn = self''',
        '''    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {\n        require_authoritative_relationship_semantics(self)?;\n        let conn = self''',
    ),
]
for old, new in replacements:
    count = text.count(old)
    assert count == 1, f'marker count={count} for {old[:80]!r}'
    text = text.replace(old, new, 1)

old = '''            store\n                .graph_edges_between("file:src/lib.rs", "symbol:s1", 10)\n                .unwrap_err(),\n        ] {'''
new = '''            store\n                .graph_edges_between("file:src/lib.rs", "symbol:s1", 10)\n                .unwrap_err(),\n            store.imports().unwrap_err(),\n            store\n                .implementation_facts_for_target("worker", 10)\n                .unwrap_err(),\n            store.references_for_symbol(&SymbolId::new("s1"), 10).unwrap_err(),\n            store.occurrences_for_file(&FileId::new("f1")).unwrap_err(),\n        ] {'''
assert text.count(old) == 1, f'adversarial test marker count={text.count(old)}'
text = text.replace(old, new, 1)

path.write_text(text)
