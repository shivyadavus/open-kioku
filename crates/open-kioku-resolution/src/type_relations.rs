use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::{RustModuleFiles, ScopeIndex, SymbolIndex};
use crate::pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
};
use open_kioku_core::{
    Binding, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, InheritanceKind,
    InheritanceSite, Language, LineRange, RelationshipProof, RelationshipProofKind, ScopeId,
    Symbol, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ParentBindingKind {
    SameFile,
    Import,
    QualifiedName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentTypeCandidate {
    pub target: SymbolId,
    pub bindings: BTreeSet<ParentBindingKind>,
}

/// With `scopes`, imports are looked up from the child's defining scope, since an inheritance site
/// has no scope of its own; without it, every import of the name in the file is one set.
pub(crate) fn collect_parent_type_candidates(
    child: &Symbol,
    parent_name: &str,
    symbols: &SymbolIndex,
    repository: &SemanticRepository,
    scopes: Option<&ScopeIndex>,
) -> Vec<ParentTypeCandidate> {
    collect_parent_type_candidate_set(child, parent_name, symbols, repository, scopes).0
}

/// [`collect_parent_type_candidates`], with the files of a module whose file configuration
/// selects when an import through it reached a candidate (#625).
fn collect_parent_type_candidate_set(
    child: &Symbol,
    parent_name: &str,
    symbols: &SymbolIndex,
    repository: &SemanticRepository,
    scopes: Option<&ScopeIndex>,
) -> (Vec<ParentTypeCandidate>, Option<RustModuleFiles>) {
    let mut configured = None::<RustModuleFiles>;
    let mut candidates = BTreeMap::<String, ParentTypeCandidate>::new();
    let mut add = |target: SymbolId, binding: ParentBindingKind| {
        let entry = candidates
            .entry(target.0.clone())
            .or_insert_with(|| ParentTypeCandidate {
                target,
                bindings: BTreeSet::new(),
            });
        entry.bindings.insert(binding);
    };

    if let Some(file_symbols) = symbols.by_file.get(&child.file_id) {
        for id in file_symbols {
            if symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == parent_name)
                .unwrap_or(false)
            {
                add(id.clone(), ParentBindingKind::SameFile);
            }
        }
    }

    // As for calls, the nearest import of the name in scope decides.
    if let crate::context::ScopedImport::Resolved(bindings) = crate::context::scoped_import(
        repository,
        scopes,
        &child.language,
        &child.file_id,
        child.scope_id.as_ref(),
        parent_name,
        |binding| {
            binding.target_symbol.is_some()
                || binding.target_file.is_some()
                || binding.configured_targets.is_some()
        },
    ) {
        for binding in bindings {
            // An import through a module whose file configuration selects names the type of each
            // file that may hold it; the placed tree's target is one of them (#625).
            if let Some(targets) = &binding.configured_targets {
                let mut found = targets
                    .items
                    .iter()
                    .filter(|item| {
                        symbols
                            .get(item)
                            .is_some_and(|symbol| is_type_symbol(&symbol.kind))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for file in &targets.module_files {
                    found.extend(
                        symbols
                            .by_file
                            .get(file)
                            .into_iter()
                            .flatten()
                            .filter(|id| {
                                symbols.get(id).is_some_and(|symbol| {
                                    is_type_symbol(&symbol.kind) && symbol.name == parent_name
                                })
                            })
                            .cloned(),
                    );
                }
                if !found.is_empty() {
                    for target in found {
                        add(target, ParentBindingKind::Import);
                    }
                    let files = crate::typed_calls::configured_import_files(targets);
                    let into = configured.get_or_insert_with(RustModuleFiles::default);
                    into.files.extend(files.files);
                    into.unread |= files.unread;
                }
                continue;
            }
            if let Some(target) = &binding.target_symbol {
                if symbols
                    .get(target)
                    .map(|symbol| is_type_symbol(&symbol.kind))
                    .unwrap_or(false)
                {
                    add(target.clone(), ParentBindingKind::Import);
                }
            }
            if let Some(target_file) = &binding.target_file {
                if let Some(file_symbols) = symbols.by_file.get(target_file) {
                    for id in file_symbols {
                        if symbols
                            .get(id)
                            .map(|symbol| {
                                is_type_symbol(&symbol.kind) && symbol.name == parent_name
                            })
                            .unwrap_or(false)
                        {
                            add(id.clone(), ParentBindingKind::Import);
                        }
                    }
                }
            }
        }
    }

    if let Some(qualified) = symbols.by_qualified.get(parent_name) {
        for id in qualified {
            if symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind))
                .unwrap_or(false)
            {
                add(id.clone(), ParentBindingKind::QualifiedName);
            }
        }
    }

    if let Some(files) = &mut configured {
        files.files.sort();
        files.files.dedup();
    }
    (candidates.into_values().collect(), configured)
}

pub fn resolve_inheritance_relationship_outcome(
    site: &InheritanceSite,
    ctx: &ResolutionContext<'_>,
) -> (GraphEdgeType, ResolutionOutcome) {
    let edge_type = inheritance_edge_type(&site.kind);
    let Some(child) = ctx.symbols.get(&site.child_symbol_id) else {
        return (
            edge_type.clone(),
            evaluate_candidates(&edge_type, Vec::new()),
        );
    };
    let (parent_candidates, configured) = collect_parent_type_candidate_set(
        child,
        &site.parent_name,
        ctx.symbols,
        ctx.repository,
        Some(ctx.scopes),
    );
    if parent_candidates.is_empty()
        && site.kind == InheritanceKind::TraitImpl
        && child.language == Language::Rust
    {
        if let Some((identity, via_import)) = rust_external_trait(child, &site.parent_name, ctx) {
            let evidence = ResolutionEvidence {
                kind: if via_import {
                    ResolutionEvidenceKind::ExplicitImport
                } else {
                    ResolutionEvidenceKind::LexicalScope
                },
                source_type: EvidenceSourceType::TreeSitter,
                file_range: syntax_file_range(ctx, &site.range),
                symbol_id: None,
                message: format!(
                    "implemented trait `{}` is `{identity}`, defined outside the repository",
                    site.parent_name
                ),
            };
            return (
                edge_type,
                ResolutionOutcome::External {
                    identity,
                    evidence: vec![evidence],
                },
            );
        }
    }
    let target_ids = parent_candidates
        .iter()
        .map(|candidate| candidate.target.clone())
        .collect::<Vec<_>>();
    let candidate_count = target_ids.len();
    // Through a module whose file configuration selects, every proof lists its files, so none is
    // unique: each candidate is kept, below authoritative (#625).
    let (confidence, ambiguity, message) = match &configured {
        Some(files) => (
            Confidence::High,
            crate::typed_calls::configured_file_names(files),
            crate::typed_calls::configured_type_message(
                &format!(
                    "explicit {:?} declaration of {} through a Rust import",
                    site.kind, site.parent_name
                ),
                files,
            ),
        ),
        None => (
            Confidence::Exact,
            ambiguity_strings(&target_ids),
            format!(
                "explicit {:?} declaration candidate for {}",
                site.kind, site.parent_name
            ),
        ),
    };
    let source_range = syntax_file_range(ctx, &site.range);

    let candidates = parent_candidates
        .into_iter()
        .map(|parent| {
            let mut candidate = ResolutionCandidate::new(parent.target.clone(), confidence);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::InheritanceGraph,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: source_range.clone(),
                symbol_id: Some(parent.target.clone()),
                message: message.clone(),
            });
            candidate.proofs.push(proof(
                RelationshipProofKind::InheritanceBinding,
                "explicit_inheritance_declaration",
                source_range.clone(),
                &site.child_symbol_id,
                &parent.target,
                candidate_count,
                &ambiguity,
            ));
            for binding in parent.bindings {
                let (kind, strategy) = match binding {
                    ParentBindingKind::SameFile => (
                        RelationshipProofKind::SameScopeDefinition,
                        "same_file_parent_type",
                    ),
                    ParentBindingKind::Import if configured.is_some() => (
                        RelationshipProofKind::ImportBinding,
                        "rust_configured_type_import",
                    ),
                    ParentBindingKind::Import => (
                        RelationshipProofKind::ImportBinding,
                        "import_bound_parent_type",
                    ),
                    ParentBindingKind::QualifiedName => (
                        RelationshipProofKind::QualifiedName,
                        "qualified_parent_type",
                    ),
                };
                candidate.proofs.push(proof(
                    kind,
                    strategy,
                    source_range.clone(),
                    &site.child_symbol_id,
                    &parent.target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            if matches!(
                site.kind,
                InheritanceKind::Implements | InheritanceKind::TraitImpl
            ) && ctx
                .symbols
                .get(&parent.target)
                .map(|target| matches!(target.kind, SymbolKind::Trait | SymbolKind::Interface))
                .unwrap_or(false)
            {
                candidate.proofs.push(proof(
                    RelationshipProofKind::TraitOrInterfaceBinding,
                    "trait_or_interface_target",
                    source_range.clone(),
                    &site.child_symbol_id,
                    &parent.target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();

    let outcome = match &configured {
        Some(files) => ResolutionOutcome::Alternatives {
            candidates: normalize_candidates(candidates),
            reason: crate::typed_calls::configured_type_message(
                &format!("the Rust import of {}", site.parent_name),
                files,
            ),
        },
        None => evaluate_candidates(&edge_type, candidates),
    };
    (edge_type, outcome)
}

/// Traits of the Rust standard prelude, in any edition, that code names without importing them.
const RUST_PRELUDE_TRAITS: &[&str] = &[
    "AsMut",
    "AsRef",
    "AsyncFn",
    "AsyncFnMut",
    "AsyncFnOnce",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Send",
    "Sized",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
];

/// The path of the trait a Rust `impl` names, when no repository symbol answers to it and the
/// index can show it is defined outside the repository, and whether an import showed it. The
/// path's first segment decides: an import binding it from the standard library or from a
/// dependency the package's manifest places outside the repository (`use std::fmt;` for
/// `fmt::Debug`), such a crate named directly (`std::error::Error`, `tokio::io::AsyncRead`), or,
/// for a bare name nothing in scope imports, a trait of the standard prelude (`Iterator`).
/// `None` whenever the path may name an item of the repository: through `crate`, `self` or
/// `super`, an item or glob import in scope, or an import the index cannot place outside.
fn rust_external_trait(
    child: &Symbol,
    trait_name: &str,
    ctx: &ResolutionContext<'_>,
) -> Option<(String, bool)> {
    let path = rust_trait_path(trait_name)?;
    let (absolute, path) = match path.strip_prefix("::") {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    let mut segments = path.split("::").map(str::trim);
    let first = segments.next()?;
    let rest = segments.collect::<Vec<_>>();
    if absolute {
        return ctx
            .scopes
            .rust_names_external_crate(&child.file_id, first)
            .then(|| (path.to_string(), false));
    }
    if matches!(first, "crate" | "self" | "super" | "Self") {
        return None;
    }
    let scope = child.scope_id.as_ref()?;
    if crate::context::nearest_lexical_items(ctx, scope, first, |_| true).is_some() {
        return None;
    }
    match crate::context::scoped_import(
        ctx.repository,
        Some(ctx.scopes),
        &child.language,
        &child.file_id,
        Some(scope),
        first,
        |_| true,
    ) {
        crate::context::ScopedImport::Resolved(bindings) => {
            let mut sources = bindings
                .iter()
                .map(|binding| rust_external_import_source(ctx, child, &binding.source_module))
                .collect::<Option<Vec<_>>>()?;
            sources.sort_unstable();
            sources.dedup();
            let [source] = sources.as_slice() else {
                return None;
            };
            let identity = std::iter::once(*source)
                .chain(rest.iter().copied())
                .collect::<Vec<_>>()
                .join("::");
            Some((identity, true))
        }
        crate::context::ScopedImport::Unresolved => None,
        crate::context::ScopedImport::NotImported if rest.is_empty() => RUST_PRELUDE_TRAITS
            .contains(&first)
            .then(|| (format!("std::prelude::{first}"), false)),
        crate::context::ScopedImport::NotImported => ctx
            .scopes
            .rust_names_external_crate(&child.file_id, first)
            .then(|| (path.to_string(), false)),
    }
}

/// A `use` path whose first segment names a crate outside the repository, without a trailing
/// `self`.
fn rust_external_import_source<'s>(
    ctx: &ResolutionContext<'_>,
    child: &Symbol,
    source: &'s str,
) -> Option<&'s str> {
    let source = source.trim();
    let source = source.strip_prefix("::").unwrap_or(source);
    let source = source.strip_suffix("::self").unwrap_or(source);
    let first = source.split("::").next()?.trim();
    ctx.scopes
        .rust_names_external_crate(&child.file_id, first)
        .then_some(source)
}

/// The path of the trait an `impl` header names, without generic arguments (`From<u8>` is a use
/// of `From`) or the parenthesized arguments of a closure trait. `None` for anything that is not
/// a plain path.
fn rust_trait_path(trait_name: &str) -> Option<&str> {
    let end = trait_name.find(['<', '(']).unwrap_or(trait_name.len());
    let path = trait_name[..end].trim();
    let body = path.strip_prefix("::").unwrap_or(path);
    let plain = !body.is_empty()
        && body.split("::").all(|segment| {
            let segment = segment.strip_prefix("r#").unwrap_or(segment);
            !segment.is_empty() && segment.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        });
    plain.then_some(path)
}

pub fn resolve_declared_type_use_outcome(
    binding: &Binding,
    ctx: &ResolutionContext<'_>,
) -> Option<(SymbolId, ResolutionOutcome)> {
    let type_name = binding.declared_type.as_deref()?.trim();
    if type_name.is_empty() {
        return None;
    }
    let source = scope_owner_symbol(&binding.scope_id, ctx.scopes)?;
    let found = crate::typed_calls::collect_type_candidate_set(ctx, &binding.scope_id, type_name);
    let origins = found.targets;
    let targets = origins
        .iter()
        .map(|(target, _)| target.clone())
        .collect::<Vec<_>>();
    let candidate_count = targets.len();
    // Through a module whose file configuration selects, the declared type names the type of each
    // file that may hold it: every proof lists those files, so none is unique and each candidate
    // is kept below authoritative (#625).
    let (confidence, ambiguity, message, import_strategy) = match &found.configured {
        Some(files) => (
            Confidence::High,
            crate::typed_calls::configured_file_names(files),
            crate::typed_calls::configured_type_message(
                &format!("the declared type `{type_name}` through a Rust import"),
                files,
            ),
            "rust_configured_type_import",
        ),
        None => (
            Confidence::Exact,
            ambiguity_strings(&targets),
            format!("explicit declared type `{type_name}` candidate"),
            "import_bound_declared_type",
        ),
    };
    let source_range = syntax_file_range(ctx, &binding.range);
    let candidates = origins
        .into_iter()
        .map(|(target, via_import)| {
            let mut candidate = ResolutionCandidate::new(target.clone(), confidence);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: source_range.clone(),
                symbol_id: Some(target.clone()),
                message: message.clone(),
            });
            candidate.proofs.push(proof(
                RelationshipProofKind::ExactReference,
                "explicit_declared_type",
                source_range.clone(),
                &source,
                &target,
                candidate_count,
                &ambiguity,
            ));
            // Keeps the import route visible on the edge; the exact reference alone decides its
            // authority.
            if via_import {
                candidate.proofs.push(proof(
                    RelationshipProofKind::ImportBinding,
                    import_strategy,
                    source_range.clone(),
                    &source,
                    &target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();
    let outcome = match &found.configured {
        Some(files) => ResolutionOutcome::Alternatives {
            candidates: normalize_candidates(candidates),
            reason: crate::typed_calls::configured_type_message(
                &format!("the Rust import of `{type_name}`"),
                files,
            ),
        },
        None => evaluate_candidates(&GraphEdgeType::UsesType, candidates),
    };
    Some((source, outcome))
}

pub(crate) fn scope_owner_symbol(scope_id: &ScopeId, scopes: &ScopeIndex) -> Option<SymbolId> {
    let mut current = Some(scope_id.clone());
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            return None;
        }
        let scope = scopes.get(&id)?;
        if let Some(owner) = &scope.owner_symbol_id {
            return Some(owner.clone());
        }
        current = scope.parent_id.clone();
    }
    None
}

fn inheritance_edge_type(kind: &InheritanceKind) -> GraphEdgeType {
    match kind {
        InheritanceKind::Extends => GraphEdgeType::Extends,
        InheritanceKind::Implements | InheritanceKind::TraitImpl => GraphEdgeType::Implements,
        InheritanceKind::Embeds => GraphEdgeType::UsesType,
    }
}

fn proof(
    kind: RelationshipProofKind,
    strategy: &str,
    source_range: Option<FileRange>,
    source: &SymbolId,
    target: &SymbolId,
    candidate_count: usize,
    ambiguity: &[String],
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = source_range;
    proof.source_symbol_id = Some(source.clone());
    proof.target_symbol_id = Some(target.clone());
    proof.ambiguity = ambiguity.to_vec();
    proof
}

fn syntax_file_range(
    ctx: &ResolutionContext<'_>,
    range: &open_kioku_core::SourceRange,
) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.into(),
        line_range: Some(LineRange {
            start: range.start_line,
            end: range.end_line,
        }),
    })
}

fn ambiguity_strings(ids: &[SymbolId]) -> Vec<String> {
    if ids.len() > 1 {
        ids.iter().map(|id| id.0.clone()).collect()
    } else {
        Vec::new()
    }
}

fn is_type_symbol(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        BindingId, FileId, Language, Scope, ScopeKind, SourceRange, Symbol, Visibility,
    };
    use open_kioku_semantic_model::SemanticRepository;

    fn symbol(id: &str, name: &str, kind: SymbolKind, parent: Option<&str>) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind,
            file_id: FileId::new("file:src/lib.rs"),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: parent.map(SymbolId::new),
            scope_id: Some(ScopeId::new("scope:fn")),
            signature: None,
            visibility: Visibility::Public,
            alias_of: None,
        }
    }

    fn range() -> SourceRange {
        SourceRange {
            start_line: 3,
            start_column: 1,
            end_line: 3,
            end_column: 20,
        }
    }

    #[test]
    fn duplicate_same_file_parent_names_remain_ambiguous() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class, None);
        let first = symbol("symbol:a", "Parent", SymbolKind::Class, None);
        let second = symbol("symbol:b", "Parent", SymbolKind::Class, None);
        let forward = SymbolIndex::build(vec![child.clone(), first.clone(), second.clone()]);
        let reversed = SymbolIndex::build(vec![second, first, child.clone()]);
        let repo = SemanticRepository::new();
        let left = collect_parent_type_candidates(&child, "Parent", &forward, &repo, None);
        let right = collect_parent_type_candidates(&child, "Parent", &reversed, &repo, None);
        assert_eq!(left, right);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].target.0, "symbol:a");
        assert_eq!(left[1].target.0, "symbol:b");
    }

    #[test]
    fn declared_type_requires_unique_exact_target() {
        let owner = symbol("symbol:owner", "owner", SymbolKind::Function, None);
        let ty_a = symbol("symbol:a", "Thing", SymbolKind::Class, None);
        let ty_b = symbol("symbol:b", "Thing", SymbolKind::Class, None);
        let scope = Scope {
            id: ScopeId::new("scope:fn"),
            file_id: FileId::new("file:src/lib.rs"),
            parent_id: None,
            owner_symbol_id: Some(owner.id.clone()),
            kind: ScopeKind::Function,
            range: range(),
        };
        let binding = Binding {
            id: BindingId::new("binding:value"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: scope.id.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: None,
            range: range(),
        };
        let symbols = SymbolIndex::build(vec![owner, ty_a, ty_b]);
        let scopes = ScopeIndex::build(vec![scope]);
        let bindings = BindingIndex::build(vec![binding.clone()]);
        let inheritance = InheritanceIndex::default();
        let repo = SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let file_id = FileId::new("file:src/lib.rs");
        let ctx = ResolutionContext::new(
            &file_id,
            std::path::Path::new("src/lib.rs"),
            None,
            Language::Rust,
            &repo,
            &symbols,
            &scopes,
            &bindings,
            &inheritance,
            semantics,
        );
        let (_, outcome) = resolve_declared_type_use_outcome(&binding, &ctx).unwrap();
        assert!(matches!(outcome, ResolutionOutcome::Ambiguous { .. }));
    }
}

#[cfg(test)]
mod ri3_relationship_outcome_tests {
    use super::*;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        BindingId, FileId, Language, Scope, ScopeKind, SourceRange, Symbol, Visibility,
    };
    use open_kioku_semantic_model::SemanticRepository;

    fn symbol(id: &str, name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{name}"),
            kind,
            file_id: FileId::new("file:src/lib.rs"),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: Some(ScopeId::new("scope:fn")),
            signature: None,
            visibility: Visibility::Public,
            alias_of: None,
        }
    }

    fn range() -> SourceRange {
        SourceRange {
            start_line: 7,
            start_column: 3,
            end_line: 7,
            end_column: 21,
        }
    }

    struct Fixture {
        symbols: SymbolIndex,
        scopes: ScopeIndex,
        bindings: BindingIndex,
        inheritance: InheritanceIndex,
        repo: SemanticRepository,
        file_id: FileId,
    }

    impl Fixture {
        fn new(symbols: Vec<Symbol>, scopes: Vec<Scope>, bindings: Vec<Binding>) -> Self {
            Self {
                symbols: SymbolIndex::build(symbols),
                scopes: ScopeIndex::build(scopes),
                bindings: BindingIndex::build(bindings),
                inheritance: InheritanceIndex::default(),
                repo: SemanticRepository::new(),
                file_id: FileId::new("file:src/lib.rs"),
            }
        }

        fn context(&self) -> ResolutionContext<'_> {
            ResolutionContext::new(
                &self.file_id,
                std::path::Path::new("src/lib.rs"),
                None,
                Language::Rust,
                &self.repo,
                &self.symbols,
                &self.scopes,
                &self.bindings,
                &self.inheritance,
                open_kioku_languages::semantics_for(&Language::Rust).unwrap(),
            )
        }
    }

    fn proof_kinds(candidate: &ResolutionCandidate) -> BTreeSet<RelationshipProofKind> {
        candidate.proofs.iter().map(|proof| proof.kind).collect()
    }

    #[test]
    fn unique_same_file_extends_is_proven_with_declaration_and_binding_proofs() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let parent = symbol("symbol:parent", "Parent", SymbolKind::Class);
        let fixture = Fixture::new(vec![child.clone(), parent.clone()], Vec::new(), Vec::new());
        let site = InheritanceSite {
            child_symbol_id: child.id.clone(),
            parent_name: "Parent".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(),
        };

        let (edge_type, outcome) =
            resolve_inheritance_relationship_outcome(&site, &fixture.context());
        assert_eq!(edge_type, GraphEdgeType::Extends);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique exact parent should prove EXTENDS");
        };
        assert_eq!(candidate.target_symbol_id, parent.id);
        let kinds = proof_kinds(&candidate);
        assert!(kinds.contains(&RelationshipProofKind::InheritanceBinding));
        assert!(kinds.contains(&RelationshipProofKind::SameScopeDefinition));
        assert!(candidate
            .proofs
            .iter()
            .all(|proof| proof.source_range.is_some()));
    }

    #[test]
    fn unique_trait_implements_is_proven_with_trait_binding() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let tr = symbol("symbol:trait", "Runnable", SymbolKind::Trait);
        let fixture = Fixture::new(vec![child.clone(), tr.clone()], Vec::new(), Vec::new());
        let site = InheritanceSite {
            child_symbol_id: child.id.clone(),
            parent_name: "Runnable".into(),
            kind: InheritanceKind::Implements,
            order: 0,
            range: range(),
        };

        let (edge_type, outcome) =
            resolve_inheritance_relationship_outcome(&site, &fixture.context());
        assert_eq!(edge_type, GraphEdgeType::Implements);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique exact trait should prove IMPLEMENTS");
        };
        assert_eq!(candidate.target_symbol_id, tr.id);
        let kinds = proof_kinds(&candidate);
        assert!(kinds.contains(&RelationshipProofKind::InheritanceBinding));
        assert!(kinds.contains(&RelationshipProofKind::TraitOrInterfaceBinding));
    }

    #[test]
    fn unique_explicit_declared_type_is_proven_as_uses_type() {
        let owner = symbol("symbol:owner", "owner", SymbolKind::Function);
        let ty = symbol("symbol:thing", "Thing", SymbolKind::Class);
        let scope = Scope {
            id: ScopeId::new("scope:fn"),
            file_id: FileId::new("file:src/lib.rs"),
            parent_id: None,
            owner_symbol_id: Some(owner.id.clone()),
            kind: ScopeKind::Function,
            range: range(),
        };
        let binding = Binding {
            id: BindingId::new("binding:value"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: scope.id.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: Some("WrongInferredType".into()),
            range: range(),
        };
        let fixture = Fixture::new(
            vec![owner.clone(), ty.clone()],
            vec![scope],
            vec![binding.clone()],
        );

        let (source, outcome) =
            resolve_declared_type_use_outcome(&binding, &fixture.context()).unwrap();
        assert_eq!(source, owner.id);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique explicit declared type should prove USES_TYPE");
        };
        assert_eq!(candidate.target_symbol_id, ty.id);
        assert_eq!(
            proof_kinds(&candidate),
            BTreeSet::from([RelationshipProofKind::ExactReference])
        );
        assert!(candidate
            .proofs
            .iter()
            .all(|proof| proof.source_range.is_some()));
    }

    /// A Rust struct `Child` declared at file scope of `src/lib.rs`, with the file's `use`
    /// bindings `(local name, source path, glob)` and the crates outside the repository its
    /// package declares, and the outcome of `impl <trait_name> for Child`.
    fn rust_trait_impl_outcome(
        trait_name: &str,
        imports: &[(&str, &str, bool)],
        external_crates: &[&str],
    ) -> ResolutionOutcome {
        let mut child = symbol("symbol:child", "Child", SymbolKind::Class);
        child.scope_id = Some(ScopeId::new("scope:file"));
        let file_scope = Scope {
            id: ScopeId::new("scope:file"),
            file_id: FileId::new("file:src/lib.rs"),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::File,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 40,
                end_column: 1,
            },
        };
        let mut fixture = Fixture::new(vec![child.clone()], vec![file_scope], Vec::new());
        for (local_name, source, is_glob) in imports {
            fixture
                .repo
                .imports
                .insert(open_kioku_semantic_model::ImportBinding {
                    file_id: fixture.file_id.clone(),
                    scope_id: ScopeId::new("scope:file"),
                    local_name: (*local_name).into(),
                    imported_name: (*local_name).into(),
                    source_module: (*source).into(),
                    resolved_module: None,
                    target_file: None,
                    target_symbol: None,
                    origin: open_kioku_semantic_model::ImportOrigin::Unknown,
                    is_type_only: false,
                    is_glob: *is_glob,
                    evidence: Vec::new(),
                    rule: open_kioku_semantic_model::ImportBindingRule::ModuleKey,
                    configured_targets: None,
                });
        }
        fixture.scopes.record_rust_external_crates(
            [(
                fixture.file_id.clone(),
                std::sync::Arc::new(
                    external_crates
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect::<BTreeSet<_>>(),
                ),
            )]
            .into_iter()
            .collect(),
        );
        let site = InheritanceSite {
            child_symbol_id: child.id,
            parent_name: trait_name.into(),
            kind: InheritanceKind::TraitImpl,
            order: 0,
            range: range(),
        };
        let (edge_type, outcome) =
            resolve_inheritance_relationship_outcome(&site, &fixture.context());
        assert_eq!(edge_type, GraphEdgeType::Implements);
        outcome
    }

    fn external_identity(outcome: &ResolutionOutcome) -> Option<&str> {
        match outcome {
            ResolutionOutcome::External { identity, .. } => Some(identity),
            _ => None,
        }
    }

    #[test]
    fn rust_impl_of_a_standard_library_trait_is_external() {
        let through_module =
            rust_trait_impl_outcome("fmt::Debug", &[("fmt", "std::fmt", false)], &[]);
        assert_eq!(external_identity(&through_module), Some("std::fmt::Debug"));
        let direct = rust_trait_impl_outcome("std::error::Error", &[], &[]);
        assert_eq!(external_identity(&direct), Some("std::error::Error"));
        let imported =
            rust_trait_impl_outcome("Deref", &[("Deref", "core::ops::Deref", false)], &[]);
        assert_eq!(external_identity(&imported), Some("core::ops::Deref"));
        let generic = rust_trait_impl_outcome("From<u8>", &[], &[]);
        assert_eq!(external_identity(&generic), Some("std::prelude::From"));
        let prelude = rust_trait_impl_outcome("Iterator", &[], &[]);
        assert_eq!(external_identity(&prelude), Some("std::prelude::Iterator"));
    }

    #[test]
    fn rust_impl_of_a_trait_from_a_dependency_outside_the_repository_is_external() {
        let imported = rust_trait_impl_outcome(
            "AsyncRead",
            &[("AsyncRead", "tokio::io::AsyncRead", false)],
            &["tokio"],
        );
        assert_eq!(external_identity(&imported), Some("tokio::io::AsyncRead"));
        let direct = rust_trait_impl_outcome("bytes::Buf", &[], &["bytes"]);
        assert_eq!(external_identity(&direct), Some("bytes::Buf"));
    }

    #[test]
    fn rust_impl_of_a_trait_the_index_cannot_place_outside_stays_unresolved() {
        let unresolved = |outcome: ResolutionOutcome| {
            assert!(
                matches!(outcome, ResolutionOutcome::Unresolved { .. }),
                "expected unresolved, got {outcome:?}"
            );
        };
        // A crate the package's manifest does not declare may be one of the repository.
        unresolved(rust_trait_impl_outcome(
            "AsyncRead",
            &[("AsyncRead", "tokio::io::AsyncRead", false)],
            &[],
        ));
        unresolved(rust_trait_impl_outcome("bytes::Buf", &[], &[]));
        // A path through this crate names an item of the repository the index did not bind.
        unresolved(rust_trait_impl_outcome(
            "AsyncWrite",
            &[("AsyncWrite", "crate::io::AsyncWrite", false)],
            &["tokio"],
        ));
        unresolved(rust_trait_impl_outcome("crate::Link", &[], &[]));
        // A glob in scope may supply a prelude name, and a bare name outside the prelude comes
        // from somewhere the index does not see.
        unresolved(rust_trait_impl_outcome(
            "Iterator",
            &[("*", "crate::iter::*", true)],
            &[],
        ));
        unresolved(rust_trait_impl_outcome("Debug", &[], &[]));
    }

    #[test]
    fn inheritance_outcome_is_identical_under_reversed_symbol_insertion_order() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let first = symbol("symbol:a", "Parent", SymbolKind::Class);
        let second = symbol("symbol:b", "Parent", SymbolKind::Class);
        let forward = Fixture::new(
            vec![child.clone(), first.clone(), second.clone()],
            Vec::new(),
            Vec::new(),
        );
        let reversed = Fixture::new(vec![second, first, child.clone()], Vec::new(), Vec::new());
        let site = InheritanceSite {
            child_symbol_id: child.id,
            parent_name: "Parent".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(),
        };

        let (_, left) = resolve_inheritance_relationship_outcome(&site, &forward.context());
        let (_, right) = resolve_inheritance_relationship_outcome(&site, &reversed.context());
        match (left, right) {
            (
                ResolutionOutcome::Ambiguous {
                    candidates: left, ..
                },
                ResolutionOutcome::Ambiguous {
                    candidates: right, ..
                },
            ) => {
                let left_ids = left
                    .into_iter()
                    .map(|candidate| candidate.target_symbol_id)
                    .collect::<Vec<_>>();
                let right_ids = right
                    .into_iter()
                    .map(|candidate| candidate.target_symbol_id)
                    .collect::<Vec<_>>();
                assert_eq!(left_ids, right_ids);
                assert_eq!(
                    left_ids,
                    vec![SymbolId::new("symbol:a"), SymbolId::new("symbol:b")]
                );
            }
            other => panic!("expected deterministic ambiguity, got {other:?}"),
        }
    }
}
