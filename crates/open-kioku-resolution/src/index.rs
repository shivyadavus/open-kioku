use open_kioku_core::{
    Binding, FileId, ModuleDeclarationSite, ModuleId, Scope, ScopeId, ScopeKind, SourceRange,
    Symbol, SymbolId,
};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub by_id: HashMap<SymbolId, Symbol>,
    pub by_name: HashMap<String, SmallVec<[SymbolId; 4]>>,
    pub by_qualified: HashMap<String, SmallVec<[SymbolId; 2]>>,
    pub by_file: HashMap<FileId, Vec<SymbolId>>,
    pub by_file_name: HashMap<(FileId, String), SmallVec<[SymbolId; 2]>>,
    pub by_file_scope_name: HashMap<(FileId, ScopeId, String), SmallVec<[SymbolId; 2]>>,
    pub by_module: HashMap<ModuleId, Vec<SymbolId>>,
    pub by_parent: HashMap<SymbolId, Vec<SymbolId>>,
}

impl SymbolIndex {
    pub fn build(symbols: Vec<Symbol>) -> Self {
        let mut index = Self::default();
        for sym in symbols {
            index
                .by_name
                .entry(sym.name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_qualified
                .entry(sym.qualified_name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_file
                .entry(sym.file_id.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_file_name
                .entry((sym.file_id.clone(), sym.name.clone()))
                .or_default()
                .push(sym.id.clone());
            if let Some(scope_id) = &sym.scope_id {
                index
                    .by_file_scope_name
                    .entry((sym.file_id.clone(), scope_id.clone(), sym.name.clone()))
                    .or_default()
                    .push(sym.id.clone());
            }
            if let Some(mod_id) = &sym.module_id {
                index
                    .by_module
                    .entry(mod_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            if let Some(parent_id) = &sym.parent_symbol_id {
                index
                    .by_parent
                    .entry(parent_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            index.by_id.insert(sym.id.clone(), sym);
        }
        for values in index.by_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_qualified.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file_scope_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_module.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_parent.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        index
    }

    pub fn get(&self, id: &SymbolId) -> Option<&Symbol> {
        self.by_id.get(id)
    }

    pub fn lookup_name(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn lookup_file_scope_name(
        &self,
        file_id: &FileId,
        scope_id: &ScopeId,
        name: &str,
    ) -> &[SymbolId] {
        self.by_file_scope_name
            .get(&(file_id.clone(), scope_id.clone(), name.to_owned()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn lookup_file_name(&self, file_id: &FileId, name: &str) -> &[SymbolId] {
        self.by_file_name
            .get(&(file_id.clone(), name.to_owned()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScopeIndex {
    pub scopes: HashMap<ScopeId, Scope>,
    /// The scope of each Rust `mod` item, by the module symbol that owns it. The parser gives a
    /// bodiless `mod name;` a scope too, so only [`ScopeIndex::module_body`] says which is a block.
    modules_by_owner: HashMap<SymbolId, ScopeId>,
    /// Whether each `mod` scope has a body, from the declarations the parser recorded.
    module_has_body: HashMap<ScopeId, bool>,
    /// `mod` scopes that enclose another scope, which only a block can.
    modules_with_children: HashSet<ScopeId>,
    /// Rust files that the declared module tree shows are not the module their path spells.
    misplaced_rust_module_files: HashSet<FileId>,
}

/// What the `mod` item a module symbol names turned out to be.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModuleBody<'s> {
    /// An inline `mod name { .. }` block, and its scope.
    Inline(&'s Scope),
    /// `mod name;`, whose items live in another file.
    OutOfLine,
    /// The index cannot tell.
    Unknown,
}

impl ScopeIndex {
    pub fn build(scopes: Vec<Scope>) -> Self {
        let mut index = Self::default();
        for scope in &scopes {
            if scope.kind == ScopeKind::Module {
                if let Some(owner) = &scope.owner_symbol_id {
                    index
                        .modules_by_owner
                        .insert(owner.clone(), scope.id.clone());
                }
            }
        }
        let module_scopes = index.modules_by_owner.values().collect::<HashSet<_>>();
        let modules_with_children = scopes
            .iter()
            .filter_map(|scope| scope.parent_id.as_ref())
            .filter(|parent| module_scopes.contains(parent))
            .cloned()
            .collect();
        index.modules_with_children = modules_with_children;
        for scope in scopes {
            index.scopes.insert(scope.id.clone(), scope);
        }
        index
    }

    /// Records which `mod` scopes have a body. A declaration and the scope of its item share
    /// the enclosing scope and the item's range.
    pub fn record_module_declarations(&mut self, declarations: &[ModuleDeclarationSite]) {
        let by_site = self
            .modules_by_owner
            .values()
            .filter_map(|id| self.scopes.get(id))
            .map(|scope| {
                (
                    (scope.parent_id.as_ref(), range_key(&scope.range)),
                    &scope.id,
                )
            })
            .collect::<HashMap<_, _>>();
        let mut has_body = HashMap::new();
        for declaration in declarations {
            let key = (declaration.scope_id.as_ref(), range_key(&declaration.range));
            if let Some(scope_id) = by_site.get(&key) {
                has_body.insert((*scope_id).clone(), declaration.has_body);
            }
        }
        self.module_has_body.extend(has_body);
    }

    /// Records the Rust files that the declared module tree shows are not the module their path
    /// spells: the default location of a `#[path]` module, a file a `#[path]` mounts, and any other
    /// file of a crate's source tree that is not declared as a file by the module above it. A Rust
    /// path whose target the resolver spells from file paths neither starts nor ends in one.
    pub fn record_misplaced_rust_module_files(&mut self, files: HashSet<FileId>) {
        self.misplaced_rust_module_files.extend(files);
    }

    /// Whether `file` may be the module its path spells: nothing recorded says otherwise. A file
    /// the module tree cannot place either way keeps that reading.
    pub(crate) fn may_be_module_at_its_path(&self, file: &FileId) -> bool {
        !self.misplaced_rust_module_files.contains(file)
    }

    pub fn get(&self, id: &ScopeId) -> Option<&Scope> {
        self.scopes.get(id)
    }

    /// What the `mod` item of `module` is. Without its declaration, a `mod` scope enclosing
    /// another scope is a block, and an empty one is either.
    pub(crate) fn module_body(&self, module: &SymbolId) -> ModuleBody<'_> {
        let Some(scope) = self
            .modules_by_owner
            .get(module)
            .and_then(|id| self.scopes.get(id))
        else {
            return ModuleBody::Unknown;
        };
        match self.module_has_body.get(&scope.id) {
            Some(true) => ModuleBody::Inline(scope),
            Some(false) => ModuleBody::OutOfLine,
            None if self.modules_with_children.contains(&scope.id) => ModuleBody::Inline(scope),
            None => ModuleBody::Unknown,
        }
    }
}

fn range_key(range: &SourceRange) -> (u32, u32, u32, u32) {
    (
        range.start_line,
        range.start_column,
        range.end_line,
        range.end_column,
    )
}

#[derive(Debug, Clone, Default)]
pub struct BindingIndex {
    pub bindings_by_scope_name: HashMap<(ScopeId, String), Vec<Binding>>,
}

impl BindingIndex {
    pub fn build(bindings: Vec<Binding>) -> Self {
        let mut index = Self::default();
        for binding in bindings {
            index
                .bindings_by_scope_name
                .entry((binding.scope_id.clone(), binding.name.clone()))
                .or_default()
                .push(binding);
        }
        for values in index.bindings_by_scope_name.values_mut() {
            values.sort_by(|left, right| {
                (
                    left.range.start_line,
                    left.range.start_column,
                    left.range.end_line,
                    left.range.end_column,
                )
                    .cmp(&(
                        right.range.start_line,
                        right.range.start_column,
                        right.range.end_line,
                        right.range.end_column,
                    ))
                    .then_with(|| left.id.0.cmp(&right.id.0))
            });
        }
        index
    }

    pub fn resolve_before(
        &self,
        scope_id: &ScopeId,
        name: &str,
        call_range: &SourceRange,
        scopes: &ScopeIndex,
    ) -> Option<&Binding> {
        let mut current = Some(scope_id.clone());
        let mut visited = std::collections::HashSet::new();
        let call_pos = (call_range.start_line, call_range.start_column);
        while let Some(sid) = current {
            if !visited.insert(sid.clone()) {
                break;
            }
            if let Some(list) = self
                .bindings_by_scope_name
                .get(&(sid.clone(), name.to_string()))
            {
                if let Some(binding) = list.iter().rev().find(|binding| {
                    (binding.range.start_line, binding.range.start_column) <= call_pos
                }) {
                    return Some(binding);
                }
            }
            current = scopes.get(&sid).and_then(|scope| scope.parent_id.clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        BindingId, Confidence, EvidenceSourceType, Language, LineRange, SymbolKind, Visibility,
    };

    fn symbol(id: &str, name: &str, file: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Rust,
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
    fn symbol_candidate_indexes_do_not_depend_on_insertion_order() {
        let first = symbol("symbol:a", "run", "file:lib.rs");
        let second = symbol("symbol:b", "run", "file:lib.rs");

        let forward = SymbolIndex::build(vec![first.clone(), second.clone()]);
        let reversed = SymbolIndex::build(vec![second, first]);

        assert_eq!(forward.lookup_name("run"), reversed.lookup_name("run"));
        assert_eq!(
            forward.by_file.get(&FileId::new("file:lib.rs")),
            reversed.by_file.get(&FileId::new("file:lib.rs"))
        );
        assert_eq!(
            forward
                .lookup_name("run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
        assert_eq!(
            forward
                .lookup_file_scope_name(
                    &FileId::new("file:lib.rs"),
                    &ScopeId::new("scope:file"),
                    "run"
                )
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
        assert_eq!(
            forward
                .lookup_file_name(&FileId::new("file:lib.rs"), "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
    }

    #[test]
    fn file_scope_name_lookup_is_deterministic_and_exact() {
        let mut first = symbol("symbol:a", "run", "file:lib.rs");
        first.scope_id = Some(ScopeId::new("scope:inner"));
        let mut second = symbol("symbol:b", "run", "file:lib.rs");
        second.scope_id = Some(ScopeId::new("scope:inner"));
        let mut third = symbol("symbol:c", "run", "file:lib.rs");
        third.scope_id = Some(ScopeId::new("scope:outer"));

        let forward = SymbolIndex::build(vec![first.clone(), second.clone(), third.clone()]);
        let reversed = SymbolIndex::build(vec![third, second, first]);
        let file_id = FileId::new("file:lib.rs");
        let inner = ScopeId::new("scope:inner");

        assert_eq!(
            forward.lookup_file_scope_name(&file_id, &inner, "run"),
            reversed.lookup_file_scope_name(&file_id, &inner, "run")
        );
        assert_eq!(
            forward
                .lookup_file_scope_name(&file_id, &inner, "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
        assert!(forward
            .lookup_file_scope_name(&file_id, &inner, "other")
            .is_empty());
        assert_eq!(
            forward
                .lookup_file_name(&file_id, "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b", "symbol:c"]
        );
    }

    #[test]
    fn binding_ties_have_a_deterministic_order() {
        let scope = ScopeId::new("scope:file");
        let range = SourceRange {
            start_line: 4,
            start_column: 2,
            end_line: 4,
            end_column: 8,
        };
        let make = |id: &str| Binding {
            id: BindingId::new(id),
            file_id: FileId::new("file:lib.rs"),
            scope_id: scope.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: None,
            range: range.clone(),
        };

        let index = BindingIndex::build(vec![make("binding:z"), make("binding:a")]);
        let ids = index
            .bindings_by_scope_name
            .get(&(scope, "value".into()))
            .unwrap()
            .iter()
            .map(|binding| binding.id.0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["binding:a", "binding:z"]);
    }
}
