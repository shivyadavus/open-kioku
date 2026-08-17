from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

# Shared type candidate semantics and proof-gated declared type use.
types = Path("crates/open-kioku-resolution/src/type_candidates.rs")
text = types.read_text()
text = replace_exact(
    text,
    'use crate::index::SymbolIndex;\nuse open_kioku_core::{FileId, ScopeId, SymbolId, SymbolKind};\n',
    '''use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome, ResolutionStrategy};
use open_kioku_core::{
    Binding, Confidence, EvidenceId, EvidenceSourceType, FileId, FileRange, GraphEdgeType,
    LineRange, RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
};
use std::path::Path;
''',
    "type candidate imports",
)
text = replace_exact(
    text,
    '    if value.contains(\'|\') || value.contains("->") || value.contains("=>") || value.contains(\',\') {\n',
    '    if value.contains(\'|\') || value.contains("->") || value.contains("=>") {\n',
    "generic comma normalization",
)
old_invalid = '''        || value.contains('{')
        || value.contains('}')
    {
'''
new_invalid = '''        || value.contains('{')
        || value.contains('}')
        || value.contains(',')
    {
'''
text = replace_exact(text, old_invalid, new_invalid, "top-level comma rejection")
old_discovery_prelude = '''    let simple_name = type_name
        .rsplit_once("::")
        .map(|(_, name)| name)
        .or_else(|| type_name.rsplit_once('.').map(|(_, name)| name))
        .unwrap_or(type_name.as_str());

    let mut candidates = BTreeMap::<String, TypeCandidate>::new();
'''
new_discovery_prelude = '''    let simple_name = type_name
        .rsplit_once("::")
        .map(|(_, name)| name)
        .or_else(|| type_name.rsplit_once('.').map(|(_, name)| name))
        .unwrap_or(type_name.as_str());
    let qualified_expression = type_name.contains("::") || type_name.contains('.');

    let mut candidates = BTreeMap::<String, TypeCandidate>::new();
'''
text = replace_exact(text, old_discovery_prelude, new_discovery_prelude, "qualified type detection")
old_local_import = '''    if let Some(file_symbols) = symbols.by_file.get(file_id) {
        for target in file_symbols {
            if is_named_type(symbols, target, simple_name) {
                add(target.clone(), TypeDiscovery::SameFile);
            }
        }
    }

    let mut imported = repository.imports.lookup(file_id, scope_id, simple_name);
    if scope_id.is_some() {
        imported.extend(repository.imports.lookup(file_id, None, simple_name));
    }
    for binding in imported {
        if let Some(target) = &binding.target_symbol {
            if is_type_symbol(symbols, target) {
                add(target.clone(), TypeDiscovery::ImportBinding);
            }
        }
        if let Some(target_file) = &binding.target_file {
            if let Some(file_symbols) = symbols.by_file.get(target_file) {
                for target in file_symbols {
                    if is_named_type(symbols, target, simple_name) {
                        add(target.clone(), TypeDiscovery::ImportBinding);
                    }
                }
            }
        }
    }

    for lookup in [type_name.as_str(), simple_name] {
        if let Some(qualified) = symbols.by_qualified.get(lookup) {
            for target in qualified {
                if is_type_symbol(symbols, target) {
                    add(target.clone(), TypeDiscovery::QualifiedName);
                }
            }
        }
    }
'''
new_local_import = '''    if !qualified_expression {
        if let Some(file_symbols) = symbols.by_file.get(file_id) {
            for target in file_symbols {
                if is_named_type(symbols, target, simple_name) {
                    add(target.clone(), TypeDiscovery::SameFile);
                }
            }
        }

        let mut imported = repository.imports.lookup(file_id, scope_id, simple_name);
        if scope_id.is_some() {
            imported.extend(repository.imports.lookup(file_id, None, simple_name));
        }
        for binding in imported {
            if let Some(target) = &binding.target_symbol {
                if is_type_symbol(symbols, target) {
                    add(target.clone(), TypeDiscovery::ImportBinding);
                }
            }
            if let Some(target_file) = &binding.target_file {
                if let Some(file_symbols) = symbols.by_file.get(target_file) {
                    for target in file_symbols {
                        if is_named_type(symbols, target, simple_name) {
                            add(target.clone(), TypeDiscovery::ImportBinding);
                        }
                    }
                }
            }
        }
    }

    let dotted_as_scoped = type_name.replace('.', "::");
    let mut qualified_lookups = vec![type_name.as_str()];
    if dotted_as_scoped != type_name {
        qualified_lookups.push(dotted_as_scoped.as_str());
    }
    if !qualified_expression {
        qualified_lookups.push(simple_name);
    }
    for lookup in qualified_lookups {
        if let Some(qualified) = symbols.by_qualified.get(lookup) {
            for target in qualified {
                if is_type_symbol(symbols, target) {
                    add(target.clone(), TypeDiscovery::QualifiedName);
                }
            }
        }
    }
'''
text = replace_exact(text, old_local_import, new_local_import, "qualified candidate precedence")
insert_anchor = '''fn is_named_type(symbols: &SymbolIndex, target: &SymbolId, name: &str) -> bool {
'''
new_functions = '''pub fn discovery_candidate_count(
    candidates: &[TypeCandidate],
    discovery: TypeDiscovery,
) -> usize {
    candidates
        .iter()
        .filter(|candidate| candidate.discoveries.contains(&discovery))
        .count()
}

pub fn resolve_declared_type_use(
    binding: &Binding,
    source_symbol_id: &SymbolId,
    file_path: &Path,
    repository: &SemanticRepository,
    symbols: &SymbolIndex,
) -> ResolutionOutcome {
    let Some(declared_type) = binding.declared_type.as_deref() else {
        return ResolutionOutcome::Unresolved {
            candidates: Vec::new(),
            reason: "binding has no explicit declared type".into(),
        };
    };
    let discovered = discover_type_candidates(
        &binding.file_id,
        Some(&binding.scope_id),
        declared_type,
        repository,
        symbols,
    );
    let same_file_count = discovery_candidate_count(&discovered, TypeDiscovery::SameFile);
    let import_count = discovery_candidate_count(&discovered, TypeDiscovery::ImportBinding);
    let qualified_count = discovery_candidate_count(&discovered, TypeDiscovery::QualifiedName);
    let range = FileRange {
        path: file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: binding.range.start_line,
            end: binding.range.end_line,
        }),
    };

    let candidates = discovered
        .into_iter()
        .map(|type_candidate| {
            let target = type_candidate.target;
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact)
                .with_strategy(ResolutionStrategy::TypedReceiver);
            for discovery in type_candidate.discoveries {
                let (kind, strategy, candidate_count) = match discovery {
                    TypeDiscovery::SameFile => (
                        RelationshipProofKind::SameScopeDefinition,
                        "declared_type_same_file",
                        same_file_count,
                    ),
                    TypeDiscovery::ImportBinding => (
                        RelationshipProofKind::ImportBinding,
                        "declared_type_import_binding",
                        import_count,
                    ),
                    TypeDiscovery::QualifiedName => (
                        RelationshipProofKind::QualifiedName,
                        "declared_type_qualified_name",
                        qualified_count,
                    ),
                };
                let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
                proof.source_range = Some(range.clone());
                proof.source_symbol_id = Some(source_symbol_id.clone());
                proof.target_symbol_id = Some(target.clone());
                proof.evidence_ids = vec![EvidenceId::new(binding.id.0.clone())];
                candidate.proofs.push(proof);
            }
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: Some(range.clone()),
                symbol_id: Some(target),
                message: format!("explicit declared type `{declared_type}` resolved structurally"),
            });
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::UsesType, candidates)
}

'''
text = replace_exact(text, insert_anchor, new_functions + insert_anchor, "declared type use functions")
# Strengthen normalization tests for real generic and fully-qualified cases.
old_test = '''        assert_eq!(normalize_outer_type_name("Repo<Foo>"), Some("Repo".into()));
        assert_eq!(
            normalize_outer_type_name("pkg::Repo<Foo>"),
            Some("pkg::Repo".into())
        );
'''
new_test = '''        assert_eq!(normalize_outer_type_name("Repo<Foo>"), Some("Repo".into()));
        assert_eq!(normalize_outer_type_name("Map<Key, Repo>"), Some("Map".into()));
        assert_eq!(
            normalize_outer_type_name("pkg::Repo<Foo>"),
            Some("pkg::Repo".into())
        );
'''
text = replace_exact(text, old_test, new_test, "generic normalization tests")
# Add proof-gating tests before the existing exact-qualified test.
anchor = '''    #[test]
    fn exact_qualified_name_can_disambiguate_same_simple_name() {
'''
proof_tests = '''    #[test]
    fn unique_declared_same_file_type_is_proven() {
        let file_id = FileId::new("file:main");
        let owner = SymbolId::new("symbol:owner");
        let symbols = SymbolIndex::build(vec![symbol(
            "symbol:Repo",
            "Repo",
            "pkg::Repo",
            "file:main",
        )]);
        let repository = SemanticRepository::new();
        let binding = Binding {
            id: open_kioku_core::BindingId::new("binding:repo"),
            file_id,
            scope_id: ScopeId::new("scope:method"),
            name: "repo".into(),
            declared_type: Some("Repo".into()),
            inferred_type: None,
            range: open_kioku_core::SourceRange {
                start_line: 4,
                start_column: 5,
                end_line: 4,
                end_column: 14,
            },
        };
        let outcome = resolve_declared_type_use(
            &binding,
            &owner,
            Path::new("src/main.rs"),
            &repository,
            &symbols,
        );
        match outcome {
            ResolutionOutcome::Proven { candidate } => {
                assert_eq!(candidate.target_symbol_id, SymbolId::new("symbol:Repo"));
                assert_eq!(
                    candidate.authority(&GraphEdgeType::UsesType),
                    open_kioku_core::RelationshipAuthority::Authoritative
                );
            }
            other => panic!("expected proven declared type, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_declared_same_file_type_stays_ambiguous() {
        let file_id = FileId::new("file:main");
        let symbols = SymbolIndex::build(vec![
            symbol("symbol:a", "Repo", "a::Repo", "file:main"),
            symbol("symbol:b", "Repo", "b::Repo", "file:main"),
        ]);
        let repository = SemanticRepository::new();
        let binding = Binding {
            id: open_kioku_core::BindingId::new("binding:repo"),
            file_id,
            scope_id: ScopeId::new("scope:method"),
            name: "repo".into(),
            declared_type: Some("Repo".into()),
            inferred_type: None,
            range: open_kioku_core::SourceRange {
                start_line: 4,
                start_column: 5,
                end_line: 4,
                end_column: 14,
            },
        };
        let outcome = resolve_declared_type_use(
            &binding,
            &SymbolId::new("symbol:owner"),
            Path::new("src/main.rs"),
            &repository,
            &symbols,
        );
        assert!(matches!(outcome, ResolutionOutcome::Ambiguous { .. }));
    }

'''
text = replace_exact(text, anchor, proof_tests + anchor, "declared type proof tests")
types.write_text(text)

# Scope ownership follows lexical parents so block-local bindings can produce symbol-to-type edges.
index = Path("crates/open-kioku-resolution/src/index.rs")
text = index.read_text()
anchor = '''    pub fn get(&self, id: &ScopeId) -> Option<&Scope> {
        self.scopes.get(id)
    }
'''
new = '''    pub fn get(&self, id: &ScopeId) -> Option<&Scope> {
        self.scopes.get(id)
    }

    pub fn nearest_owner_symbol(&self, id: &ScopeId) -> Option<SymbolId> {
        let mut current = Some(id.clone());
        let mut visited = std::collections::HashSet::new();
        while let Some(scope_id) = current {
            if !visited.insert(scope_id.clone()) {
                return None;
            }
            let scope = self.get(&scope_id)?;
            if let Some(owner) = &scope.owner_symbol_id {
                return Some(owner.clone());
            }
            current = scope.parent_id.clone();
        }
        None
    }
'''
text = replace_exact(text, anchor, new, "scope owner traversal")
index.write_text(text)

# Core authority: each exact declared-type binding strategy is sufficient only when its own
# candidate count is unique.
core = Path("crates/open-kioku-core/src/relationship.rs")
text = core.read_text()
old = '''        GraphEdgeType::UsesType => {
            exact_target
                || (receiver_type && (qualified_name || same_scope))
                || (import_binding && qualified_name)
        }
'''
new = '''        GraphEdgeType::UsesType => {
            exact_target
                || same_scope
                || import_binding
                || qualified_name
                || (receiver_type && (qualified_name || same_scope))
        }
'''
text = replace_exact(text, old, new, "USES_TYPE authority policy")
core.write_text(text)

# Call resolution uses discovery-specific uniqueness instead of total candidate count for import
# and exact-qualified proof claims.
calls = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = calls.read_text()
old = 'use crate::type_candidates::{discover_type_candidates, TypeDiscovery};\n'
new = '''use crate::type_candidates::{
    discover_type_candidates, discovery_candidate_count, TypeDiscovery,
};
'''
text = replace_exact(text, old, new, "call type discovery count import")
old = '''    let receiver_candidate_count = type_candidates.len();
    let mut targets = BTreeMap::<String, (SymbolId, Vec<TypeDiscovery>)>::new();
'''
new = '''    let receiver_candidate_count = type_candidates.len();
    let import_candidate_count =
        discovery_candidate_count(&type_candidates, TypeDiscovery::ImportBinding);
    let qualified_candidate_count =
        discovery_candidate_count(&type_candidates, TypeDiscovery::QualifiedName);
    let mut targets = BTreeMap::<String, (SymbolId, Vec<TypeDiscovery>)>::new();
'''
text = replace_exact(text, old, new, "call discovery counts")
text = replace_exact(
    text,
    '''                        "receiver_type_import_binding",
                        receiver_candidate_count,
''',
    '''                        "receiver_type_import_binding",
                        import_candidate_count,
''',
    "call import proof count",
)
text = replace_exact(
    text,
    '''                        "receiver_type_qualified_name",
                        receiver_candidate_count,
''',
    '''                        "receiver_type_qualified_name",
                        qualified_candidate_count,
''',
    "call qualified proof count",
)
calls.write_text(text)

# Export the declared-type resolver.
lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = lib.read_text()
old = '''pub use type_candidates::{
    discover_type_candidates, normalize_outer_type_name, TypeCandidate, TypeDiscovery,
};
'''
new = '''pub use type_candidates::{
    discover_type_candidates, discovery_candidate_count, normalize_outer_type_name,
    resolve_declared_type_use, TypeCandidate, TypeDiscovery,
};
'''
text = replace_exact(text, old, new, "declared type resolver export")
lib.write_text(text)

# Emit only proven declared type uses. Inferred types are intentionally excluded.
ingest = Path("crates/open-kioku-ingest/src/lib.rs")
text = ingest.read_text()
anchor = '''            quality_report.call_sites = call_sites.len();

            let symbols_by_qualified: HashMap<&str, &Symbol> = symbols
'''
insert = '''            quality_report.call_sites = call_sites.len();

            for binding in &bindings {
                if binding.declared_type.is_none() {
                    continue;
                }
                let Some(source_symbol_id) = scope_index.nearest_owner_symbol(&binding.scope_id)
                else {
                    continue;
                };
                let Some(file) = file_lookup.get(&binding.file_id) else {
                    continue;
                };
                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } =
                    open_kioku_resolution::resolve_declared_type_use(
                        binding,
                        &source_symbol_id,
                        &file.path,
                        &semantic_repo,
                        &symbol_index,
                    )
                {
                    resolved_relationships.push(open_kioku_resolution::ResolvedRelationship {
                        from: source_symbol_id,
                        to: candidate.target_symbol_id,
                        edge_type: GraphEdgeType::UsesType,
                        confidence: candidate.confidence,
                        call_site: None,
                        evidence: candidate.evidence,
                        proofs: candidate.proofs,
                    });
                }
            }

            let symbols_by_qualified: HashMap<&str, &Symbol> = symbols
'''
text = replace_exact(text, anchor, insert, "declared type ingest emission")
ingest.write_text(text)
