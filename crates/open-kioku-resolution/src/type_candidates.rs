use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use crate::pipeline::{
    evaluate_candidates, ResolutionCandidate, ResolutionOutcome, ResolutionStrategy,
};
use open_kioku_core::{
    Binding, Confidence, EvidenceId, EvidenceSourceType, FileId, FileRange, GraphEdgeType,
    LineRange, RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeDiscovery {
    SameFile,
    ImportBinding,
    QualifiedName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeCandidate {
    pub target: SymbolId,
    pub discoveries: Vec<TypeDiscovery>,
}

/// Normalize the outer type named by a declared/receiver type expression without guessing through
/// unions, tuples, function types, or other multi-target syntax.
pub fn normalize_outer_type_name(raw: &str) -> Option<String> {
    let mut value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if value.contains('|') || value.contains("->") || value.contains("=>") {
        return None;
    }

    loop {
        let trimmed = value.trim();
        let next = trimmed
            .strip_prefix("mut ")
            .or_else(|| trimmed.strip_prefix("const "))
            .or_else(|| trimmed.strip_prefix("dyn "))
            .or_else(|| trimmed.strip_prefix("impl "))
            .or_else(|| trimmed.strip_prefix('&'))
            .or_else(|| trimmed.strip_prefix('*'));
        match next {
            Some(rest) => value = rest,
            None => {
                value = trimmed;
                break;
            }
        }
    }

    while let Some(rest) = value.strip_suffix("[]") {
        value = rest.trim();
    }
    value = value.trim_end_matches('?').trim();
    if value.is_empty()
        || value.starts_with('(')
        || value.starts_with('[')
        || value.starts_with('{')
    {
        return None;
    }

    if let Some(index) = value.find('<') {
        let suffix = &value[index..];
        if !balanced_angles(suffix) {
            return None;
        }
        value = value[..index].trim();
    }

    let value = value
        .trim_start_matches("::")
        .trim_start_matches('.')
        .trim();
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.contains('(')
        || value.contains(')')
        || value.contains('[')
        || value.contains(']')
        || value.contains('{')
        || value.contains('}')
        || value.contains(',')
    {
        return None;
    }
    Some(value.to_string())
}

pub fn discover_type_candidates(
    file_id: &FileId,
    scope_id: Option<&ScopeId>,
    raw_type_name: &str,
    repository: &SemanticRepository,
    symbols: &SymbolIndex,
) -> Vec<TypeCandidate> {
    let Some(type_name) = normalize_outer_type_name(raw_type_name) else {
        return Vec::new();
    };
    let simple_name = type_name
        .rsplit_once("::")
        .map(|(_, name)| name)
        .or_else(|| type_name.rsplit_once('.').map(|(_, name)| name))
        .unwrap_or(type_name.as_str());
    let qualified_expression = type_name.contains("::") || type_name.contains('.');

    let mut candidates = BTreeMap::<String, TypeCandidate>::new();
    let mut add = |target: SymbolId, discovery: TypeDiscovery| {
        let entry = candidates
            .entry(target.0.clone())
            .or_insert_with(|| TypeCandidate {
                target: target.clone(),
                discoveries: Vec::new(),
            });
        entry.discoveries.push(discovery);
    };

    if !qualified_expression {
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

    candidates
        .into_values()
        .map(|mut candidate| {
            candidate.discoveries.sort();
            candidate.discoveries.dedup();
            candidate
        })
        .collect()
}

pub fn discovery_candidate_count(candidates: &[TypeCandidate], discovery: TypeDiscovery) -> usize {
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

fn is_named_type(symbols: &SymbolIndex, target: &SymbolId, name: &str) -> bool {
    symbols
        .get(target)
        .map(|symbol| symbol.name == name && is_type_kind(&symbol.kind))
        .unwrap_or(false)
}

fn is_type_symbol(symbols: &SymbolIndex, target: &SymbolId) -> bool {
    symbols
        .get(target)
        .map(|symbol| is_type_kind(&symbol.kind))
        .unwrap_or(false)
}

fn is_type_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
}

fn balanced_angles(value: &str) -> bool {
    let mut depth = 0_i32;
    for ch in value.chars() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, Language, LineRange, Symbol, Visibility,
    };

    fn symbol(id: &str, name: &str, qualified: &str, file: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified.into(),
            kind: SymbolKind::Class,
            file_id: FileId::new(file),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Java,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        }
    }

    #[test]
    fn outer_type_normalization_is_conservative_and_cross_language() {
        assert_eq!(normalize_outer_type_name("&mut Repo"), Some("Repo".into()));
        assert_eq!(normalize_outer_type_name("*Repo"), Some("Repo".into()));
        assert_eq!(normalize_outer_type_name("Repo[]"), Some("Repo".into()));
        assert_eq!(normalize_outer_type_name("Repo<Foo>"), Some("Repo".into()));
        assert_eq!(
            normalize_outer_type_name("Map<Key, Repo>"),
            Some("Map".into())
        );
        assert_eq!(
            normalize_outer_type_name("pkg::Repo<Foo>"),
            Some("pkg::Repo".into())
        );
        assert_eq!(normalize_outer_type_name("Repo | MockRepo"), None);
        assert_eq!(normalize_outer_type_name("(Repo, Foo)"), None);
        assert_eq!(normalize_outer_type_name("Repo -> Foo"), None);
    }

    #[test]
    fn same_file_candidates_are_deterministic_and_deduplicated() {
        let file_id = FileId::new("file:main");
        let first = symbol("symbol:b", "Repo", "pkg::Repo", "file:main");
        let second = symbol("symbol:a", "Repo", "other::Repo", "file:main");
        let symbols = SymbolIndex::build(vec![first, second]);
        let repository = SemanticRepository::new();

        let candidates = discover_type_candidates(&file_id, None, "Repo", &repository, &symbols);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.target.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
        assert!(candidates
            .iter()
            .all(|candidate| { candidate.discoveries == vec![TypeDiscovery::SameFile] }));
    }

    #[test]
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

    #[test]
    fn exact_qualified_name_can_disambiguate_same_simple_name() {
        let file_id = FileId::new("file:main");
        let first = symbol("symbol:a", "Repo", "pkg::Repo", "file:a");
        let second = symbol("symbol:b", "Repo", "other::Repo", "file:b");
        let symbols = SymbolIndex::build(vec![first, second]);
        let repository = SemanticRepository::new();

        let candidates =
            discover_type_candidates(&file_id, None, "pkg::Repo", &repository, &symbols);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].target, SymbolId::new("symbol:a"));
        assert_eq!(
            candidates[0].discoveries,
            vec![TypeDiscovery::QualifiedName]
        );
    }
}
