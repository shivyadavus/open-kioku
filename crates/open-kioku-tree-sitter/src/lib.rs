use open_kioku_core::{
    Binding, BindingId, CallSite, CallSiteId, Confidence, EvidenceSourceType, ExportSite, File,
    ImportSite, ImportedName, InheritanceKind, InheritanceSite, Language, LineRange,
    ModuleDeclarationSite, PackageDeclarationSite, ReceiverKind, Scope, ScopeId, ScopeKind,
    SourceRange, Symbol, SymbolId, SymbolKind, SyntaxFacts, TypeAliasSite, Visibility,
};
use open_kioku_errors::{OkError, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tree_sitter::{Language as TsLanguage, Node, Parser, TreeCursor};

pub struct ParseContext {
    pub scope_stack: Vec<ScopeId>,
    pub symbol_stack: Vec<SymbolId>,
    pub callable_stack: Vec<SymbolId>,
    pub type_stack: Vec<SymbolId>,
    pub next_scope_counter: u32,
    /// Visibility of the Rust traits and types the file declares, by name; built on the first
    /// trait `impl` member, since only those need it. See [`rust_declared_visibility`].
    rust_declared_visibility: Option<HashMap<String, Option<Visibility>>>,
}

impl ParseContext {
    pub fn new() -> Self {
        Self {
            scope_stack: Vec::new(),
            symbol_stack: Vec::new(),
            callable_stack: Vec::new(),
            type_stack: Vec::new(),
            next_scope_counter: 0,
            rust_declared_visibility: None,
        }
    }

    pub fn current_scope(&self) -> Option<ScopeId> {
        self.scope_stack.last().cloned()
    }

    pub fn current_symbol(&self) -> Option<SymbolId> {
        self.symbol_stack.last().cloned()
    }

    pub fn current_callable(&self) -> Option<SymbolId> {
        self.callable_stack.last().cloned()
    }

    pub fn current_type(&self) -> Option<SymbolId> {
        self.type_stack.last().cloned()
    }
}

impl Default for ParseContext {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parser_for(language: &Language) -> Result<Parser> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language(language)?)
        .map_err(|err| OkError::Parse {
            path: "<language>".into(),
            message: err.to_string(),
        })?;
    Ok(parser)
}

pub fn parse_file(file: &File, content: &str) -> Result<SyntaxFacts> {
    let mut parser = parser_for(&file.language)?;
    let tree = parser.parse(content, None).ok_or_else(|| OkError::Parse {
        path: file.path.clone(),
        message: "tree-sitter returned no parse tree".into(),
    })?;
    if tree.root_node().has_error() {
        return Err(OkError::Parse {
            path: file.path.clone(),
            message: "tree-sitter parse contains syntax errors".into(),
        });
    }

    let mut out = SyntaxFacts::default();
    let mut ctx = ParseContext::new();

    let file_scope_id = ScopeId::new(format!("{}:scope:file:0", file.path.display()));
    let file_scope = Scope {
        id: file_scope_id.clone(),
        file_id: file.id.clone(),
        parent_id: None,
        owner_symbol_id: None,
        kind: ScopeKind::File,
        range: node_source_range(tree.root_node()),
    };
    out.scopes.push(file_scope);
    ctx.scope_stack.push(file_scope_id);

    walk(file, content, tree.root_node(), &mut ctx, &mut out);
    out.package_declaration = package_declaration(file, content, tree.root_node());
    out.invokes_item_macro =
        file.language == Language::Rust && invokes_item_macro(tree.root_node());

    // Reconcile Rust impl method parent_symbol_id and inheritance sites to the actual struct/trait symbol
    if file.language == Language::Rust {
        let type_symbols_by_name: std::collections::HashMap<String, SymbolId> = out
            .symbols
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
                )
            })
            .map(|s| (s.name.clone(), s.id.clone()))
            .collect();

        for sym in &mut out.symbols {
            if let Some(parent_id) = &sym.parent_symbol_id {
                if parent_id.0.contains(":impl_owner:") {
                    let type_name = parent_id
                        .0
                        .rsplit(":impl_owner:")
                        .next()
                        .unwrap_or(parent_id.0.as_str());
                    if let Some(actual_id) = type_symbols_by_name.get(type_name) {
                        sym.parent_symbol_id = Some((*actual_id).clone());
                    }
                }
            }
        }

        for inh in &mut out.inheritance {
            if inh.child_symbol_id.0.contains(":impl_owner:") {
                let type_name = inh
                    .child_symbol_id
                    .0
                    .rsplit(":impl_owner:")
                    .next()
                    .unwrap_or(inh.child_symbol_id.0.as_str());
                if let Some(actual_id) = type_symbols_by_name.get(type_name) {
                    inh.child_symbol_id = (*actual_id).clone();
                }
            }
        }
    }

    out.symbols
        .sort_by_key(|symbol| symbol.range.as_ref().map(|range| range.start).unwrap_or(0));
    out.symbols.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

pub fn parse_symbols(file: &File, content: &str) -> Result<Vec<Symbol>> {
    Ok(parse_file(file, content)?.symbols)
}

/// A test a JavaScript or TypeScript file registers by calling its runner: `test("parses rows",
/// fn)`, `it.skip(...)`, `test.each(table)(...)` or `Suite.test(...)`. It becomes a test target
/// only; it adds no symbol and no relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRegistrationCall {
    /// The first argument's literal text with whitespace runs collapsed to one space.
    pub name: String,
    /// The whole call expression.
    pub range: LineRange,
    /// The name is a template literal with a `${...}` substitution, so the registered name is
    /// only known at runtime.
    pub interpolated: bool,
    /// A modifier the runner honours by not executing the test: `skip`, `todo`, `failing`,
    /// `fails`. Such a test is written but never run, so it is not validation evidence.
    pub disabled: bool,
}

/// Modifiers whose test the runner does not execute.
const DISABLING_TEST_MODIFIERS: [&str; 4] = ["skip", "todo", "failing", "fails"];

/// What a registration callee resolved to.
#[derive(Debug, Clone, Copy)]
struct RegistrationCallee {
    /// `<identifier>.test` or `<identifier>.it`, which needs a callback argument to count.
    namespaced: bool,
    /// A disabling modifier appeared anywhere in the chain.
    disabled: bool,
}

/// Modifiers runners chain onto `test` and `it`. Suites (`describe`, `suite`) are not
/// registrations: a suite is not a runnable unit of its own, and its name repeats the vocabulary
/// its tests already carry.
const TEST_REGISTRATION_MODIFIERS: [&str; 8] = [
    "only",
    "skip",
    "todo",
    "concurrent",
    "failing",
    "fails",
    "sequential",
    "each",
];

/// Longest callee chain followed, so a pathological `test.only.only...` cannot recurse unbounded.
const TEST_REGISTRATION_CALLEE_DEPTH: usize = 8;

/// Every test registration call in a JavaScript or TypeScript file whose first argument is a
/// string or template literal. A file with syntax errors is an error, as in [`parse_file`], so
/// the caller can fall back to a pattern reading instead of trusting a partial tree.
pub fn test_registration_calls(file: &File, content: &str) -> Result<Vec<TestRegistrationCall>> {
    let grammar: TsLanguage = match file.language {
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript
            if file
                .path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tsx")) =>
        {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        }
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => {
            return Err(OkError::Unsupported(format!(
                "test registration calls are not read for {:?}",
                file.language
            )))
        }
    };
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|err| OkError::Parse {
            path: file.path.clone(),
            message: err.to_string(),
        })?;
    let tree = parser.parse(content, None).ok_or_else(|| OkError::Parse {
        path: file.path.clone(),
        message: "tree-sitter returned no parse tree".into(),
    })?;
    if tree.root_node().has_error() {
        return Err(OkError::Parse {
            path: file.path.clone(),
            message: "tree-sitter parse contains syntax errors".into(),
        });
    }
    let source = content.as_bytes();
    let mut calls = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" {
            calls.extend(registration_call(node, source));
        }
        pending.extend((0..node.named_child_count()).filter_map(|index| node.named_child(index)));
    }
    calls.sort_by(|left, right| {
        (left.range.start, left.range.end, &left.name).cmp(&(
            right.range.start,
            right.range.end,
            &right.name,
        ))
    });
    Ok(calls)
}

fn registration_call(node: Node<'_>, source: &[u8]) -> Option<TestRegistrationCall> {
    let callee = registration_callee(node.child_by_field_name("function")?, source, 0)?;
    let arguments = node.child_by_field_name("arguments")?;
    // `test.each`table`` passes a template, not an argument list; the call it returns registers.
    if arguments.kind() != "arguments" {
        return None;
    }
    let values = (0..arguments.named_child_count())
        .filter_map(|index| arguments.named_child(index))
        .filter(|value| value.kind() != "comment")
        .collect::<Vec<_>>();
    let (first, rest) = values.split_first()?;
    let text = first.utf8_text(source).ok()?;
    let inner = text.get(1..text.len().checked_sub(1)?)?;
    let interpolated = match first.kind() {
        "string" => false,
        "template_string" => (0..first.named_child_count())
            .filter_map(|index| first.named_child(index))
            .any(|child| child.kind() == "template_substitution"),
        _ => return None,
    };
    // `pattern.test("abc")` is a regular expression check, not a registration.
    if callee.namespaced
        && !rest.iter().any(|value| {
            matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "function"
            )
        })
    {
        return None;
    }
    let name = inner.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    Some(TestRegistrationCall {
        name,
        range: LineRange {
            start: u32::try_from(node.start_position().row + 1).ok()?,
            end: u32::try_from(node.end_position().row + 1).ok()?,
        },
        interpolated,
        disabled: callee.disabled,
    })
}

/// `Some(false)` for `test`, `it` and their modifier chains, `Some(true)` for
/// `<identifier>.test`, `<identifier>.it` and theirs. A `test.each(table)` call in callee
/// position counts as the chain it was called on.
fn registration_callee(
    callee: Node<'_>,
    source: &[u8],
    depth: usize,
) -> Option<RegistrationCallee> {
    if depth > TEST_REGISTRATION_CALLEE_DEPTH {
        return None;
    }
    match callee.kind() {
        "identifier" => {
            matches!(callee.utf8_text(source).ok()?, "test" | "it").then_some(RegistrationCallee {
                namespaced: false,
                disabled: false,
            })
        }
        "member_expression" => {
            let object = callee.child_by_field_name("object")?;
            let property = callee
                .child_by_field_name("property")?
                .utf8_text(source)
                .ok()?;
            if matches!(property, "test" | "it") && object.kind() == "identifier" {
                return Some(RegistrationCallee {
                    namespaced: true,
                    disabled: false,
                });
            }
            if TEST_REGISTRATION_MODIFIERS.contains(&property) {
                let mut inner = registration_callee(object, source, depth + 1)?;
                inner.disabled |= DISABLING_TEST_MODIFIERS.contains(&property);
                return Some(inner);
            }
            None
        }
        "call_expression" => {
            let table = callee.child_by_field_name("function")?;
            let is_each = table.kind() == "member_expression"
                && table
                    .child_by_field_name("property")?
                    .utf8_text(source)
                    .ok()?
                    == "each";
            if is_each {
                registration_callee(table, source, depth + 1)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn tree_sitter_language(language: &Language) -> Result<TsLanguage> {
    match language {
        Language::Rust => Ok(tree_sitter_rust::LANGUAGE.into()),
        Language::Java => Ok(tree_sitter_java::LANGUAGE.into()),
        Language::TypeScript => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Language::JavaScript => Ok(tree_sitter_javascript::LANGUAGE.into()),
        Language::Python => Ok(tree_sitter_python::LANGUAGE.into()),
        Language::Go => Ok(tree_sitter_go::LANGUAGE.into()),
        Language::Yaml => Ok(tree_sitter_yaml::LANGUAGE.into()),
        Language::Json => Ok(tree_sitter_json::LANGUAGE.into()),
        _ => Err(OkError::Unsupported(format!(
            "tree-sitter parser not configured for {language:?}"
        ))),
    }
}

fn node_source_range(node: Node<'_>) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start_line: (start.row + 1) as u32,
        start_column: (start.column + 1) as u32,
        end_line: (end.row + 1) as u32,
        end_column: (end.column + 1) as u32,
    }
}

fn is_scope_node(file: &File, node: Node<'_>) -> Option<ScopeKind> {
    let kind = node.kind();
    match file.language {
        Language::Rust => match kind {
            "mod_item" => Some(ScopeKind::Module),
            "struct_item" | "enum_item" | "union_item" => Some(ScopeKind::Class),
            "trait_item" | "impl_item" => Some(ScopeKind::Trait),
            "function_item" => Some(ScopeKind::Function),
            // A closure's typed parameters are bindings of the closure alone: `|ctx: &mut Ring|`
            // must not type a `ctx` the enclosing function uses after the closure.
            "closure_expression" => Some(ScopeKind::Closure),
            "block" => Some(ScopeKind::Block),
            _ => None,
        },
        Language::Python => match kind {
            "class_definition" => Some(ScopeKind::Class),
            "function_definition" => Some(ScopeKind::Function),
            "block" => Some(ScopeKind::Block),
            _ => None,
        },
        Language::JavaScript | Language::TypeScript => match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "interface_declaration" => Some(ScopeKind::Interface),
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                Some(ScopeKind::Function)
            }
            "arrow_function" => Some(ScopeKind::Closure),
            "statement_block" => Some(ScopeKind::Block),
            _ => None,
        },
        Language::Java => match kind {
            "class_declaration" | "record_declaration" | "enum_declaration" => {
                Some(ScopeKind::Class)
            }
            "interface_declaration" => Some(ScopeKind::Interface),
            "method_declaration" | "constructor_declaration" => Some(ScopeKind::Method),
            "block" => Some(ScopeKind::Block),
            _ => None,
        },
        Language::Go => match kind {
            "function_declaration" | "method_declaration" => Some(ScopeKind::Function),
            "block" => Some(ScopeKind::Block),
            _ => None,
        },
        _ => None,
    }
}

fn walk(file: &File, content: &str, node: Node<'_>, ctx: &mut ParseContext, out: &mut SyntaxFacts) {
    let mut pushed_symbol: Option<SymbolId> = None;
    let mut pushed_scope: Option<ScopeId> = None;
    let mut pushed_callable: Option<SymbolId> = None;
    let mut pushed_type: Option<SymbolId> = None;

    if let Some((name_node, symbol_kind)) = symbol_name_node(file, node, ctx) {
        if let Ok(name) = name_node.utf8_text(content.as_bytes()) {
            if !name.is_empty() {
                let line_range = LineRange {
                    start: (node.start_position().row + 1) as u32,
                    end: (node.end_position().row + 1) as u32,
                };
                let qualified_name = qualified_name(file, name);
                let symbol_id = SymbolId::new(stable_id(&format!(
                    "{}:{}:{}",
                    file.path.display(),
                    line_range.start,
                    qualified_name
                )));

                let signature = extract_symbol_signature(file, content, node);
                let visibility = extract_symbol_visibility(file, content, node, ctx);

                let symbol = Symbol {
                    id: symbol_id.clone(),
                    name: name.to_string(),
                    qualified_name,
                    kind: symbol_kind.clone(),
                    file_id: file.id.clone(),
                    range: Some(line_range),
                    language: file.language.clone(),
                    confidence: Confidence::High,
                    provenance: EvidenceSourceType::TreeSitter,
                    module_id: None,
                    parent_symbol_id: ctx.current_type().or_else(|| ctx.current_symbol()),
                    scope_id: ctx.current_scope(),
                    signature,
                    visibility,
                    alias_of: None,
                };

                out.symbols.push(symbol);
                if file.language == Language::Go && node.kind() == "type_alias" {
                    out.type_aliases
                        .push(go_type_alias_site(content, node, symbol_id.clone()));
                }
                ctx.symbol_stack.push(symbol_id.clone());
                pushed_symbol = Some(symbol_id.clone());

                let is_callable = matches!(symbol_kind, SymbolKind::Function | SymbolKind::Method);
                let is_type = matches!(
                    symbol_kind,
                    SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait
                );

                if is_callable {
                    ctx.callable_stack.push(symbol_id.clone());
                    pushed_callable = Some(symbol_id.clone());
                }
                if is_type {
                    ctx.type_stack.push(symbol_id.clone());
                    pushed_type = Some(symbol_id);
                }
            }
        }
    }

    if file.language == Language::Rust && node.kind() == "impl_item" && pushed_type.is_none() {
        let source_bytes = content.as_bytes();
        if let Some(type_node) = node.child_by_field_name("type") {
            if let Some(type_name) = rust_impl_owner_name(content, type_node) {
                let type_sym_id =
                    SymbolId::new(format!("{}:impl_owner:{}", file.path.display(), type_name));

                if let Some(trait_node) = node.child_by_field_name("trait") {
                    if let Ok(trait_name) = trait_node.utf8_text(source_bytes) {
                        let trait_name = trait_name.trim().to_string();
                        if !trait_name.is_empty() {
                            out.inheritance.push(InheritanceSite {
                                child_symbol_id: type_sym_id.clone(),
                                parent_name: trait_name,
                                kind: InheritanceKind::TraitImpl,
                                order: 0,
                                range: node_source_range(node),
                            });
                        }
                    }
                }

                ctx.type_stack.push(type_sym_id.clone());
                pushed_type = Some(type_sym_id);
            }
        }
    }

    // Recorded before the item's own scope is pushed, so the declaration carries its enclosing
    // scope.
    if file.language == Language::Rust && node.kind() == "mod_item" {
        extract_rust_module_declaration(file, content.as_bytes(), node, ctx, out);
    }

    if let Some(scope_kind) = is_scope_node(file, node) {
        ctx.next_scope_counter += 1;
        let scope_id = ScopeId::new(format!(
            "{}:scope:{}:{}",
            file.path.display(),
            node.start_position().row + 1,
            ctx.next_scope_counter
        ));
        let scope = Scope {
            id: scope_id.clone(),
            file_id: file.id.clone(),
            parent_id: ctx.current_scope(),
            owner_symbol_id: ctx.current_symbol(),
            kind: scope_kind,
            range: node_source_range(node),
        };
        out.scopes.push(scope);
        ctx.scope_stack.push(scope_id.clone());
        pushed_scope = Some(scope_id);
    }

    extract_import(file, content, node, ctx, out);
    extract_export(file, content, node, ctx, out);
    extract_binding(file, content, node, ctx, out);
    extract_call(file, content, node, ctx, out);
    extract_inheritance(file, content, node, ctx, out);

    let mut cursor = node.walk();
    for child in named_children(&mut cursor) {
        walk(file, content, child, ctx, out);
    }

    if pushed_scope.is_some() {
        ctx.scope_stack.pop();
    }
    if pushed_type.is_some() {
        ctx.type_stack.pop();
    }
    if pushed_callable.is_some() {
        ctx.callable_stack.pop();
    }
    if pushed_symbol.is_some() {
        ctx.symbol_stack.pop();
    }
}

/// The package a Java or Go file declares, read from its top-level `package` declaration or
/// clause. Java package annotations (`package-info.java`) are skipped.
fn package_declaration(
    file: &File,
    content: &str,
    root: Node<'_>,
) -> Option<PackageDeclarationSite> {
    let (declaration, names) = match file.language {
        Language::Java => ("package_declaration", ["scoped_identifier", "identifier"]),
        Language::Go => ("package_clause", ["package_identifier", "identifier"]),
        _ => return None,
    };
    let mut cursor = root.walk();
    let node = named_children(&mut cursor)
        .into_iter()
        .find(|node| node.kind() == declaration)?;
    let mut cursor = node.walk();
    let name = named_children(&mut cursor)
        .into_iter()
        .find(|child| names.contains(&child.kind()))?;
    let name = name.utf8_text(content.as_bytes()).ok()?;
    // A Java name may be spaced or span lines: `org . example`.
    let name = name
        .split(|ch: char| ch.is_whitespace())
        .collect::<String>();
    (!name.is_empty()).then(|| PackageDeclarationSite {
        file_id: file.id.clone(),
        name,
    })
}

/// What a Go `type_alias` node names: `store.Entry`, `Entry` and `Page[int]` name a declared
/// type; a pointer, slice, map, function or literal type names none.
fn go_type_alias_site(content: &str, node: Node<'_>, symbol_id: SymbolId) -> TypeAliasSite {
    let source_bytes = content.as_bytes();
    let text = |node: Node<'_>| node.utf8_text(source_bytes).ok().map(str::to_string);
    let mut target = node.child_by_field_name("type");
    if let Some(generic) = target.filter(|node| node.kind() == "generic_type") {
        target = generic.child_by_field_name("type");
    }
    let (target_package, target_name) = match target {
        Some(node) if node.kind() == "type_identifier" => (None, text(node)),
        Some(node) if node.kind() == "qualified_type" => {
            let package = node.child_by_field_name("package").and_then(text);
            let name = node.child_by_field_name("name").and_then(text);
            match (package, name) {
                (Some(package), Some(name)) => (Some(package), Some(name)),
                _ => (None, None),
            }
        }
        _ => (None, None),
    };
    TypeAliasSite {
        symbol_id,
        target_package,
        target_name,
    }
}

fn extract_symbol_signature(file: &File, content: &str, node: Node<'_>) -> Option<String> {
    let source_bytes = content.as_bytes();
    match file.language {
        Language::Java => {
            if let Some(params) = node.child_by_field_name("parameters") {
                let text = params.utf8_text(source_bytes).ok()?;
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .unwrap_or("");
                Some(format!("{name}{text}"))
            } else {
                None
            }
        }
        Language::Rust => {
            if let Some(params) = node.child_by_field_name("parameters") {
                let text = params.utf8_text(source_bytes).ok()?;
                let return_type = node
                    .child_by_field_name("return_type")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .unwrap_or("");
                Some(format!("fn{text} {return_type}").trim().to_string())
            } else {
                None
            }
        }
        Language::TypeScript | Language::JavaScript => {
            if let Some(params) = node.child_by_field_name("parameters") {
                let text = params.utf8_text(source_bytes).ok()?;
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .unwrap_or("");
                Some(format!("{name}{text}"))
            } else {
                None
            }
        }
        Language::Python => {
            if let Some(params) = node.child_by_field_name("parameters") {
                let text = params.utf8_text(source_bytes).ok()?;
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .unwrap_or("");
                Some(format!("def {name}{text}"))
            } else {
                None
            }
        }
        Language::Go => {
            if node.kind() == "type_alias" {
                // Says what the alias stands for wherever the symbol is listed, so it does not
                // read as a second declaration of the type it names.
                let name = node
                    .child_by_field_name("name")?
                    .utf8_text(source_bytes)
                    .ok()?;
                let target = node
                    .child_by_field_name("type")?
                    .utf8_text(source_bytes)
                    .ok()?;
                let target = target.split_whitespace().collect::<Vec<_>>().join(" ");
                return Some(format!("type {name} = {target}"));
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                let text = params.utf8_text(source_bytes).ok()?;
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .unwrap_or("");
                Some(format!("func {name}{text}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_symbol_visibility(
    file: &File,
    content: &str,
    node: Node<'_>,
    ctx: &mut ParseContext,
) -> Visibility {
    match file.language {
        Language::Java => java_visibility(node),
        Language::Rust => match rust_associated_owner(node) {
            Some(owner) if owner.kind() == "trait_item" => rust_visibility(owner),
            Some(owner) if owner.child_by_field_name("trait").is_some() => {
                rust_trait_impl_visibility(file, content, owner, ctx)
            }
            _ => rust_visibility(node),
        },
        Language::Go => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(content.as_bytes()) {
                    if name
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    {
                        return Visibility::Public;
                    }
                }
            }
            Visibility::Private
        }
        _ => Visibility::Public,
    }
}

/// Reads the item's own `visibility_modifier` child, never its text: a private function whose
/// body spells `pub fn` is still private. `pub(super)` and `pub(in path)` reach no further than
/// the crate, so they record `Crate` alongside `pub(crate)`; `pub(self)` is private.
fn rust_visibility(node: Node<'_>) -> Visibility {
    let mut cursor = node.walk();
    let Some(modifier) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier")
    else {
        return Visibility::Private;
    };
    let mut cursor = modifier.walk();
    let mut is_pub = false;
    let mut restriction = None;
    for child in modifier.children(&mut cursor) {
        match child.kind() {
            "pub" => is_pub = true,
            "(" | ")" => {}
            kind => {
                restriction.get_or_insert(kind);
            }
        }
    }
    match (is_pub, restriction) {
        (true, None) => Visibility::Public,
        (true, Some("self")) => Visibility::Private,
        // `pub(crate)`, `pub(super)`, `pub(in path)`, and the bare `crate` modifier.
        _ => Visibility::Crate,
    }
}

/// Whether a Rust file invokes a macro at its top level, as an item: such a macro may expand to
/// items and `use` declarations the parser does not see. A macro inside a function, an inline
/// `mod` or another item is not at the top level.
fn invokes_item_macro(root: Node<'_>) -> bool {
    let mut cursor = root.walk();
    let found = root.children(&mut cursor).any(|child| match child.kind() {
        "macro_invocation" => true,
        "expression_statement" => child
            .named_child(0)
            .is_some_and(|inner| inner.kind() == "macro_invocation"),
        _ => false,
    });
    found
}

/// The `trait` or `impl` whose body directly holds this item. An item nested deeper, such as a
/// function declared inside a default method's body, has an owner of its own and returns `None`.
fn rust_associated_owner(node: Node<'_>) -> Option<Node<'_>> {
    let body = node
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")?;
    body.parent()
        .filter(|owner| matches!(owner.kind(), "trait_item" | "impl_item"))
}

/// A trait `impl` member carries no modifier of its own: it can be called wherever the trait is
/// in scope and the implementing type can be named, so it records the narrower of the two.
///
/// The type bound is a nameability heuristic, not a reachability proof: a private type handed
/// out as `impl Trait` or `Box<dyn Trait>` from a public function has its trait methods called
/// from outside the module that declares it. The record states who can name the `impl`, which is
/// what the impact test-scope reads it for.
///
/// The parser sees one file, so a bound comes only from a declaration in it:
/// - a bare name (`Store`, `Store<T>`) the file declares once, or always with the same
///   visibility, anywhere in its tree;
/// - a `self::`, `super::` or `crate::` path whose last segment is declared directly in the module
///   the path names, when that module is in this file: `super::` past the file's top level and
///   `crate::` outside a crate root name another file;
/// - an implementing type seen through `&T`, `&mut T`, `Box<T>`, `Rc<T>` and `Arc<T>`, which
///   can be named only where `T` can.
///
/// Anything else, such as `fmt::Display`, a prelude trait, a trait imported from another file, a
/// longer module path, or a name the file declares twice with different visibility, is not
/// bounded: a trait from elsewhere is callable wherever it is in scope, so it reads `Public`
/// rather than claiming a narrower reach the evidence does not show.
fn rust_trait_impl_visibility(
    file: &File,
    content: &str,
    impl_node: Node<'_>,
    ctx: &mut ParseContext,
) -> Visibility {
    let declared = ctx
        .rust_declared_visibility
        .get_or_insert_with(|| rust_declared_visibility(content, impl_node));
    let bound = |type_node: Option<Node<'_>>| {
        type_node
            .and_then(|type_node| {
                rust_declared_type_visibility(file, content, impl_node, type_node, declared)
            })
            .unwrap_or(Visibility::Public)
    };
    let implementing_type = impl_node
        .child_by_field_name("type")
        .map(|type_node| rust_pointee_type(content, type_node, declared));
    narrower_rust_visibility(
        bound(impl_node.child_by_field_name("trait")),
        bound(implementing_type),
    )
}

/// `Hidden` for `&Hidden`, `&mut Hidden`, `Box<Hidden>`, `Rc<Hidden>`, `Arc<Hidden>`,
/// `Pin<Hidden>` and any nesting of them. A file that declares its own `Box`, `Rc`, `Arc` or `Pin`
/// keeps that type.
fn rust_pointee_type<'tree>(
    content: &str,
    mut node: Node<'tree>,
    declared: &HashMap<String, Option<Visibility>>,
) -> Node<'tree> {
    const POINTERS: [&str; 12] = [
        "Box",
        "Rc",
        "Arc",
        "Pin",
        "std::pin::Pin",
        "core::pin::Pin",
        "std::boxed::Box",
        "std::rc::Rc",
        "std::sync::Arc",
        "alloc::boxed::Box",
        "alloc::rc::Rc",
        "alloc::sync::Arc",
    ];
    loop {
        let inner = match node.kind() {
            "reference_type" => node.child_by_field_name("type"),
            "generic_type" => node
                .child_by_field_name("type")
                .and_then(|head| head.utf8_text(content.as_bytes()).ok())
                .filter(|head| POINTERS.contains(head) && !declared.contains_key(*head))
                .and_then(|_| node.child_by_field_name("type_arguments"))
                .and_then(|arguments| {
                    let mut cursor = arguments.walk();
                    let mut types = arguments.named_children(&mut cursor);
                    match (types.next(), types.next()) {
                        (Some(only), None) => Some(only),
                        _ => None,
                    }
                }),
            _ => None,
        };
        match inner {
            Some(inner) => node = inner,
            None => return node,
        }
    }
}

/// The visibility this file declares for the trait or type `node` names, per the rules on
/// [`rust_trait_impl_visibility`]; `None` when the name is not bounded here.
fn rust_declared_type_visibility(
    file: &File,
    content: &str,
    impl_node: Node<'_>,
    node: Node<'_>,
    declared: &HashMap<String, Option<Visibility>>,
) -> Option<Visibility> {
    let node = if node.kind() == "generic_type" {
        node.child_by_field_name("type")?
    } else {
        node
    };
    match node.kind() {
        "type_identifier" => {
            let name = node.utf8_text(content.as_bytes()).ok()?;
            // `impl<Hidden> Cache for Box<Hidden>` is a blanket impl over any type, whatever the
            // file declares under the parameter's name.
            if rust_impl_declares_type_parameter(content, impl_node, name) {
                return None;
            }
            declared.get(name).copied().flatten()
        }
        "scoped_type_identifier" => {
            let path = node.child_by_field_name("path")?;
            let name = node
                .child_by_field_name("name")?
                .utf8_text(content.as_bytes())
                .ok()?;
            let module = rust_path_module(file, content, impl_node, path)?;
            rust_module_item_visibility(content, module, name)
        }
        _ => None,
    }
}

fn rust_impl_declares_type_parameter(content: &str, impl_node: Node<'_>, name: &str) -> bool {
    let Some(parameters) = impl_node.child_by_field_name("type_parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    let declares = parameters.named_children(&mut cursor).any(|parameter| {
        parameter.kind() == "type_parameter"
            && parameter
                .child_by_field_name("name")
                .and_then(|parameter_name| parameter_name.utf8_text(content.as_bytes()).ok())
                == Some(name)
    });
    declares
}

/// The in-file module a `self`, `super` or `crate` path names from `from`, as the node holding
/// that module's items; `None` for any other path or a module in another file.
fn rust_path_module<'tree>(
    file: &File,
    content: &str,
    from: Node<'tree>,
    path: Node<'_>,
) -> Option<Node<'tree>> {
    let text = path.utf8_text(content.as_bytes()).ok()?;
    let mut module = rust_enclosing_module(from)?;
    for (index, segment) in text.split("::").map(str::trim).enumerate() {
        match segment {
            "self" if index == 0 => {}
            "super" => {
                // A file's top level is the child of a module declared in another file.
                let declaring_mod = module
                    .parent()
                    .filter(|parent| parent.kind() == "mod_item")?;
                module = rust_enclosing_module(declaring_mod)?;
            }
            "crate" if index == 0 && is_rust_crate_root(&file.path) => {
                while let Some(parent) = module.parent() {
                    module = parent;
                }
            }
            _ => return None,
        }
    }
    Some(module)
}

/// The `source_file`, or the body of the inline `mod`, whose items include `node`.
fn rust_enclosing_module(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        let is_module = candidate.kind() == "source_file"
            || (candidate.kind() == "declaration_list"
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.kind() == "mod_item"));
        if is_module {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

/// Cargo's default crate roots: `lib.rs`, `main.rs`, and a binary directly under `bin/`. A root
/// set elsewhere by a manifest `path` is not recognised, so its `crate::` paths stay unbounded.
fn is_rust_crate_root(path: &std::path::Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str());
    matches!(file_name, Some("lib.rs" | "main.rs"))
        || (path.extension().is_some_and(|extension| extension == "rs")
            && path
                .parent()
                .and_then(|parent| parent.file_name())
                .is_some_and(|parent| parent == "bin"))
}

/// The visibility of the trait or type `name` declared directly in `module`; `None` when it is
/// not, or is declared there more than once with different visibility.
fn rust_module_item_visibility(content: &str, module: Node<'_>, name: &str) -> Option<Visibility> {
    let mut cursor = module.walk();
    let mut found = None;
    for item in module.named_children(&mut cursor) {
        let declares_name = matches!(
            item.kind(),
            "trait_item" | "struct_item" | "enum_item" | "union_item" | "type_item"
        ) && item
            .child_by_field_name("name")
            .and_then(|item_name| item_name.utf8_text(content.as_bytes()).ok())
            == Some(name);
        if declares_name {
            let visibility = rust_visibility(item);
            match found {
                None => found = Some(visibility),
                Some(seen) if seen != visibility => return None,
                Some(_) => {}
            }
        }
    }
    found
}

fn narrower_rust_visibility(left: Visibility, right: Visibility) -> Visibility {
    let reach = |visibility: Visibility| match visibility {
        Visibility::Public => 2,
        Visibility::Crate => 1,
        _ => 0,
    };
    if reach(right) < reach(left) {
        right
    } else {
        left
    }
}

/// The type an `impl` block's members belong to: `Store` for `impl<'a> Store<'a>` and
/// `a::b::Store` for `impl<T> a::b::Store<T>`. Generic arguments pick an instantiation of the
/// type, not another type, so every `impl` of `Store<..>` owns members of `Store`. Any other form,
/// such as `&T`, `dyn Trait`, a tuple or a slice, keeps its written text and so names no declared
/// type: an `impl` for a reference or a trait object is not an `impl` of the referent.
fn rust_impl_owner_name<'a>(content: &'a str, type_node: Node<'_>) -> Option<&'a str> {
    let node = if type_node.kind() == "generic_type" {
        type_node.child_by_field_name("type").unwrap_or(type_node)
    } else {
        type_node
    };
    node.utf8_text(content.as_bytes()).ok().map(str::trim)
}

/// Every trait, struct, enum, union and type alias the file declares, anywhere in its tree, by
/// name, with its own visibility; `None` when the name is declared more than once with different
/// visibility. Associated types of an `impl` are `type_item`s too and are skipped: they name no
/// type outside it. Traits and types share one namespace, so one table serves both lookups.
fn rust_declared_visibility(
    content: &str,
    any_node: Node<'_>,
) -> HashMap<String, Option<Visibility>> {
    let mut root = any_node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut declared = HashMap::<String, Option<Visibility>>::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let is_declaration = matches!(
            node.kind(),
            "trait_item" | "struct_item" | "enum_item" | "union_item" | "type_item"
        ) && rust_associated_owner(node).is_none();
        if is_declaration {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(content.as_bytes()).ok())
            {
                let visibility = rust_visibility(node);
                declared
                    .entry(name.to_string())
                    .and_modify(|seen| {
                        if *seen != Some(visibility) {
                            *seen = None;
                        }
                    })
                    .or_insert(Some(visibility));
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    declared
}

/// Reads the declaration's own `modifiers` child, so a package-private class holding a public
/// method, or a method whose body spells ` public `, keeps its own access level.
///
/// A member of an interface or annotation type with no access keyword is implicitly `public`
/// (JLS 9.3, 9.4, 9.5, 9.6); an explicit `private` interface method (Java 9+) stays private. The
/// member records its own access, not its reach: a public method of a package-private interface
/// is `Public`, as a public method of a package-private class is.
fn java_visibility(node: Node<'_>) -> Visibility {
    let implicit = if node
        .parent()
        .is_some_and(|parent| matches!(parent.kind(), "interface_body" | "annotation_type_body"))
    {
        Visibility::Public
    } else {
        Visibility::Package
    };
    let mut cursor = node.walk();
    let Some(modifiers) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "modifiers")
    else {
        return implicit;
    };
    let mut cursor = modifiers.walk();
    let visibility = modifiers
        .children(&mut cursor)
        .find_map(|child| match child.kind() {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            "protected" => Some(Visibility::Protected),
            _ => None,
        });
    visibility.unwrap_or(implicit)
}

fn symbol_name_node<'tree>(
    file: &File,
    node: Node<'tree>,
    ctx: &ParseContext,
) -> Option<(Node<'tree>, SymbolKind)> {
    let kind = node.kind();
    let name = node.child_by_field_name("name");
    match file.language {
        Language::Rust => match kind {
            "function_item" => {
                let sym_kind = if ctx.current_type().is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                name.map(|node| (node, sym_kind))
            }
            "struct_item" | "enum_item" | "union_item" => {
                name.map(|node| (node, SymbolKind::Class))
            }
            "trait_item" => name.map(|node| (node, SymbolKind::Trait)),
            "mod_item" => name.map(|node| (node, SymbolKind::Module)),
            "const_item" => name.map(|node| (node, SymbolKind::Constant)),
            "type_item" => name.map(|node| (node, SymbolKind::Class)),
            _ => None,
        },
        Language::Python => match kind {
            "function_definition" => name.map(|node| (node, SymbolKind::Function)),
            "class_definition" => name.map(|node| (node, SymbolKind::Class)),
            _ => None,
        },
        Language::JavaScript | Language::TypeScript => match kind {
            "function_declaration" | "generator_function_declaration" => {
                name.map(|node| (node, SymbolKind::Function))
            }
            "class_declaration" => name.map(|node| (node, SymbolKind::Class)),
            "interface_declaration" => name.map(|node| (node, SymbolKind::Interface)),
            "method_definition" | "public_field_definition" => {
                name.map(|node| (node, SymbolKind::Method))
            }
            "lexical_declaration" | "variable_declaration" => {
                variable_name(node).map(|node| (node, SymbolKind::Variable))
            }
            _ => None,
        },
        Language::Java => match kind {
            "class_declaration" | "record_declaration" | "enum_declaration" => {
                name.map(|node| (node, SymbolKind::Class))
            }
            "interface_declaration" => name.map(|node| (node, SymbolKind::Interface)),
            "method_declaration" | "constructor_declaration" => {
                name.map(|node| (node, SymbolKind::Method))
            }
            "field_declaration" => variable_name(node).map(|node| (node, SymbolKind::Field)),
            _ => None,
        },
        Language::Go => match kind {
            "function_declaration" => name.map(|node| (node, SymbolKind::Function)),
            "method_declaration" => name.map(|node| (node, SymbolKind::Method)),
            // An alias is a type declaration too: the registry reads through it to its target.
            "type_spec" | "type_alias" => {
                let symbol_kind = match node.child_by_field_name("type").map(|node| node.kind()) {
                    Some("interface_type") => SymbolKind::Interface,
                    _ => SymbolKind::Class,
                };
                name.map(|name_node| (name_node, symbol_kind))
            }
            _ => None,
        },
        Language::Json | Language::Yaml => None,
        _ => None,
    }
}

fn extract_call(
    file: &File,
    content: &str,
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let kind = node.kind();
    let is_call = match file.language {
        Language::Rust => kind == "call_expression" || kind == "macro_invocation",
        Language::Java => kind == "method_invocation" || kind == "object_creation_expression",
        Language::JavaScript | Language::TypeScript => {
            kind == "call_expression" || kind == "new_expression"
        }
        Language::Python => kind == "call",
        Language::Go => kind == "call_expression",
        _ => false,
    };

    if !is_call {
        return;
    }

    let scope_id = match ctx.current_scope() {
        Some(id) => id,
        None => return,
    };

    let mut callee_name = String::new();
    let mut receiver_text: Option<String> = None;
    let mut receiver_kind = ReceiverKind::None;

    let source_bytes = content.as_bytes();

    match file.language {
        Language::Java => {
            if kind == "method_invocation" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    callee_name = name_node.utf8_text(source_bytes).unwrap_or("").to_string();
                }
                if let Some(object_node) = node.child_by_field_name("object") {
                    let recv = object_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                    if !recv.is_empty() {
                        receiver_kind = classify_receiver_string(&file.language, &recv);
                        receiver_text = Some(recv);
                    }
                }
            } else if kind == "object_creation_expression" {
                if let Some(type_node) = node.child_by_field_name("type") {
                    callee_name = type_node.utf8_text(source_bytes).unwrap_or("").to_string();
                    receiver_kind = ReceiverKind::Type;
                    receiver_text = Some(callee_name.clone());
                }
            }
        }
        Language::JavaScript | Language::TypeScript => {
            if let Some(function_node) = node.child_by_field_name("function") {
                if function_node.kind() == "member_expression" {
                    if let Some(property) = function_node.child_by_field_name("property") {
                        callee_name = property.utf8_text(source_bytes).unwrap_or("").to_string();
                    }
                    if let Some(object) = function_node.child_by_field_name("object") {
                        let recv = object.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_receiver_string(&file.language, &recv);
                            receiver_text = Some(recv);
                        }
                    }
                } else {
                    callee_name = function_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        Language::Python => {
            if let Some(function_node) = node.child_by_field_name("function") {
                if function_node.kind() == "attribute" {
                    if let Some(attribute) = function_node.child_by_field_name("attribute") {
                        callee_name = attribute.utf8_text(source_bytes).unwrap_or("").to_string();
                    }
                    if let Some(object) = function_node.child_by_field_name("object") {
                        let recv = object.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_receiver_string(&file.language, &recv);
                            receiver_text = Some(recv);
                        }
                    }
                } else {
                    callee_name = function_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        Language::Go => {
            if let Some(function_node) = node.child_by_field_name("function") {
                if function_node.kind() == "selector_expression" {
                    if let Some(field) = function_node.child_by_field_name("field") {
                        callee_name = field.utf8_text(source_bytes).unwrap_or("").to_string();
                    }
                    if let Some(operand) = function_node.child_by_field_name("operand") {
                        let recv = operand.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_receiver_string(&file.language, &recv);
                            receiver_text = Some(recv);
                        }
                    }
                } else {
                    callee_name = function_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        Language::Rust => {
            if let Some(function_node) = node.child_by_field_name("function") {
                if function_node.kind() == "field_expression" {
                    if let Some(field) = function_node.child_by_field_name("field") {
                        callee_name = field.utf8_text(source_bytes).unwrap_or("").to_string();
                    }
                    if let Some(value) = function_node.child_by_field_name("value") {
                        let recv = value.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_receiver_string(&file.language, &recv);
                            receiver_text = Some(recv);
                        }
                    }
                } else if function_node.kind() == "scoped_identifier" {
                    if let Some(name_node) = function_node.child_by_field_name("name") {
                        callee_name = name_node.utf8_text(source_bytes).unwrap_or("").to_string();
                    }
                    if let Some(path_node) = function_node.child_by_field_name("path") {
                        let recv = path_node.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_rust_path_receiver(&recv);
                            receiver_text = Some(recv);
                        }
                    }
                } else {
                    callee_name = function_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        _ => {}
    }

    if !callee_name.is_empty() {
        let range = node_source_range(node);
        let call_id = CallSiteId::new(format!(
            "{}:call:{}:{}:{}:{}:{}",
            file.path.display(),
            range.start_line,
            range.start_column,
            range.end_line,
            range.end_column,
            callee_name
        ));
        out.calls.push(CallSite {
            id: call_id,
            file_id: file.id.clone(),
            scope_id,
            caller_symbol_id: ctx.current_callable(),
            callee_name,
            receiver: receiver_text,
            receiver_kind,
            range,
        });
    }
}

fn classify_receiver_string(language: &Language, recv: &str) -> ReceiverKind {
    open_kioku_languages::semantics_for(language)
        .map(|semantics| semantics.classify_receiver(recv))
        .unwrap_or(ReceiverKind::Value)
}

/// The receiver of a Rust `path::name()` call: the path before the last `::`. Unlike the value of
/// a `receiver.name()` call, which shares its text, a path is never a local binding. A lowercase
/// head is a module or crate (`engine::run()`, `fs::read()`), except a primitive type
/// (`u32::from()`, `str::from_utf8()`); an uppercase one is a type.
fn classify_rust_path_receiver(recv: &str) -> ReceiverKind {
    let recv = recv.trim();
    let head = recv.split("::").next().unwrap_or(recv);
    if matches!(recv, "crate" | "self" | "super")
        || recv.starts_with("crate::")
        || recv.starts_with("self::")
        || recv.starts_with("super::")
    {
        ReceiverKind::Module
    } else if is_rust_primitive_type(head) {
        ReceiverKind::Type
    } else if head.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_')
        && head.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
    {
        ReceiverKind::Module
    } else {
        classify_receiver_string(&Language::Rust, recv)
    }
}

fn is_rust_primitive_type(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}

fn extract_binding(
    file: &File,
    content: &str,
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let kind = node.kind();
    let scope_id = match ctx.current_scope() {
        Some(id) => id,
        None => return,
    };
    let source_bytes = content.as_bytes();

    let mut extracted: Vec<(String, Option<String>, Option<String>)> = Vec::new();

    match file.language {
        Language::Java => {
            if kind == "local_variable_declaration" || kind == "field_declaration" {
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());

                let mut cursor = node.walk();
                for child in named_children(&mut cursor) {
                    if child.kind() == "variable_declarator" {
                        let name = child
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source_bytes).ok())
                            .map(|s| s.to_string());
                        let inferred = child
                            .child_by_field_name("value")
                            .and_then(|v| infer_type_from_expr(file, source_bytes, v));
                        if let Some(n) = name {
                            extracted.push((n, declared_type.clone(), inferred));
                        }
                    }
                }
            } else if kind == "formal_parameter" || kind == "spread_parameter" {
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            }
        }
        Language::JavaScript | Language::TypeScript => {
            if kind == "variable_declarator" {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let inferred_type = node
                    .child_by_field_name("value")
                    .and_then(|v| infer_type_from_expr(file, source_bytes, v));
                if let Some(n) = name {
                    extracted.push((n, declared_type, inferred_type));
                }
            } else if kind == "required_parameter" || kind == "optional_parameter" {
                let name = node
                    .child_by_field_name("pattern")
                    .or_else(|| node.child_by_field_name("name"))
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.trim_start_matches(':').trim().to_string());
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            } else if kind == "public_field_definition" || kind == "property_definition" {
                let name = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("property"))
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.trim_start_matches(':').trim().to_string());
                let inferred_type = node
                    .child_by_field_name("value")
                    .and_then(|v| infer_type_from_expr(file, source_bytes, v));
                if let Some(n) = name {
                    extracted.push((n, declared_type, inferred_type));
                }
            }
        }
        Language::Python => {
            if kind == "assignment" {
                let left_name = node
                    .child_by_field_name("left")
                    .and_then(|l| {
                        if l.kind() == "identifier" {
                            l.utf8_text(source_bytes).ok()
                        } else {
                            None
                        }
                    })
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let inferred_type = node
                    .child_by_field_name("right")
                    .and_then(|r| infer_type_from_expr(file, source_bytes, r));
                if let Some(n) = left_name {
                    extracted.push((n, declared_type, inferred_type));
                }
            } else if kind == "typed_parameter" || kind == "default_parameter" {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            }
        }
        Language::Rust => {
            if kind == "let_declaration" {
                let name = node
                    .child_by_field_name("pattern")
                    .and_then(|p| p.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .and_then(|text| rust_declared_type(node, text, source_bytes));
                let inferred_type = node
                    .child_by_field_name("value")
                    .and_then(|v| infer_type_from_expr(file, source_bytes, v));
                if let Some(n) = name {
                    extracted.push((n, declared_type, inferred_type));
                }
            } else if kind == "parameter" {
                let name = node
                    .child_by_field_name("pattern")
                    .and_then(|p| p.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .and_then(|text| {
                        rust_declared_type(node, text.trim_start_matches('&').trim(), source_bytes)
                    });
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            } else if kind == "self_parameter" {
                let inferred = ctx
                    .type_stack
                    .last()
                    .and_then(|tid| tid.0.split(':').next_back().map(|s| s.to_string()));
                extracted.push(("self".to_string(), None, inferred));
            } else if kind == "field_declaration" && rust_is_struct_field(node) {
                // A named field of a struct, in the struct's scope, which no function body is
                // inside: the resolver reads `self.field` and `value.field` through it (#630).
                // A field typed by a type parameter of the struct has no declared type.
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .and_then(|text| rust_declared_type(node, text, source_bytes));
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            }
        }
        Language::Go => {
            if kind == "short_var_declaration" {
                let name = node
                    .child_by_field_name("left")
                    .and_then(|l| l.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                let inferred_type = node
                    .child_by_field_name("right")
                    .and_then(|r| infer_type_from_expr(file, source_bytes, r));
                if let Some(n) = name {
                    extracted.push((n, None, inferred_type));
                }
            } else if kind == "parameter_declaration" {
                let declared_type = node
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source_bytes).ok())
                    .map(|s| s.trim_start_matches('*').to_string());
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string());
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            }
        }
        _ => {}
    }

    for (name_str, declared_type, inferred_type) in extracted {
        if !name_str.is_empty() {
            let range = node_source_range(node);
            let binding_id = BindingId::new(format!(
                "{}:binding:{}:{}:{}",
                file.path.display(),
                range.start_line,
                range.start_column,
                name_str
            ));
            out.bindings.push(Binding {
                id: binding_id,
                file_id: file.id.clone(),
                scope_id: scope_id.clone(),
                name: name_str,
                declared_type,
                inferred_type,
                range,
            });
        }
    }
}

/// Whether a Rust `field_declaration` is a named field of a `struct` item, rather than of an enum
/// variant or a union.
fn rust_is_struct_field(field: Node<'_>) -> bool {
    field
        .parent()
        .filter(|list| list.kind() == "field_declaration_list")
        .and_then(|list| list.parent())
        .is_some_and(|item| item.kind() == "struct_item")
}

/// A Rust binding's written type, unless it names a type parameter of an enclosing function, impl
/// or trait: in `fn f<Token: Parse>(t: Token)` the type of `t` is generic, not an item named
/// `Token`.
fn rust_declared_type(binding: Node<'_>, type_text: &str, source: &[u8]) -> Option<String> {
    let base = type_text.trim_start_matches('&').trim();
    let base = base.strip_prefix("mut ").unwrap_or(base).trim();
    let base = base.split('<').next().unwrap_or(base).trim();
    if rust_enclosing_type_parameters(binding, source)
        .iter()
        .any(|name| name == base)
    {
        None
    } else {
        Some(type_text.to_string())
    }
}

fn rust_enclosing_type_parameters(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if let Some(parameters) = ancestor.child_by_field_name("type_parameters") {
            let mut cursor = parameters.walk();
            for parameter in named_children(&mut cursor) {
                if parameter.kind() != "type_parameter" {
                    continue;
                }
                if let Some(name) = parameter
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                {
                    names.push(name.to_string());
                }
            }
        }
        current = ancestor.parent();
    }
    names
}

fn infer_type_from_expr(file: &File, source: &[u8], expr: Node<'_>) -> Option<String> {
    let kind = expr.kind();
    match file.language {
        Language::Java | Language::JavaScript | Language::TypeScript
            if kind == "new_expression" || kind == "object_creation_expression" =>
        {
            if let Some(type_node) = expr.child_by_field_name("type") {
                return type_node.utf8_text(source).ok().map(|s| s.to_string());
            } else if let Some(constructor) = expr.child_by_field_name("constructor") {
                return constructor.utf8_text(source).ok().map(|s| s.to_string());
            }
        }
        Language::Rust => {
            if kind == "call_expression" {
                if let Some(function) = expr.child_by_field_name("function") {
                    // Recorded as the whole call path, `Foo::bar()`: the call names `Foo` but may
                    // return anything, so resolution proves the type from `bar`'s signature.
                    if function.kind() == "scoped_identifier" {
                        return function
                            .utf8_text(source)
                            .ok()
                            .map(|path| format!("{path}()"));
                    }
                }
            } else if kind == "struct_expression" {
                if let Some(name) = expr.child_by_field_name("name") {
                    return name.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
        }
        Language::Python if kind == "call" => {
            if let Some(function) = expr.child_by_field_name("function") {
                if function.kind() == "identifier" {
                    let callee = function.utf8_text(source).ok().unwrap_or("");
                    if callee
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    {
                        return Some(callee.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    None
}

fn extract_import(
    file: &File,
    content: &str,
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let kind = node.kind();
    let source_bytes = content.as_bytes();

    let is_import = match file.language {
        Language::Rust => kind == "use_declaration",
        Language::Java => kind == "import_declaration",
        Language::JavaScript | Language::TypeScript => kind == "import_statement",
        Language::Python => kind == "import_statement" || kind == "import_from_statement",
        Language::Go => kind == "import_spec",
        _ => false,
    };

    if !is_import {
        return;
    }
    if file.language == Language::Rust {
        extract_rust_use(file, source_bytes, node, ctx, out);
        return;
    }

    let range = node_source_range(node);
    let mut module_source = String::new();
    let mut bindings = Vec::new();
    let mut is_glob = false;

    match file.language {
        Language::Java => {
            if let Ok(text) = node.utf8_text(source_bytes) {
                let text = text
                    .trim_start_matches("import")
                    .trim_start_matches("static")
                    .trim_end_matches(';')
                    .trim();
                if text.ends_with(".*") {
                    is_glob = true;
                    module_source = text.trim_end_matches(".*").to_string();
                } else {
                    module_source = text.to_string();
                    if let Some(last) = text.split('.').next_back() {
                        bindings.push(ImportedName {
                            imported: last.to_string(),
                            local: last.to_string(),
                        });
                    }
                }
            }
        }
        Language::Python => {
            if kind == "import_from_statement" {
                if let Some(module_node) = node.child_by_field_name("module_name") {
                    module_source = module_node
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .to_string();
                }
                let text = node.utf8_text(source_bytes).unwrap_or("");
                if text.contains("import *") {
                    is_glob = true;
                } else {
                    let mut cursor = node.walk();
                    for child in named_children(&mut cursor) {
                        if child.kind() == "dotted_name" || child.kind() == "aliased_import" {
                            if let Ok(item_text) = child.utf8_text(source_bytes) {
                                if item_text != module_source {
                                    if item_text.contains(" as ") {
                                        let parts: Vec<&str> = item_text.split(" as ").collect();
                                        if parts.len() == 2 {
                                            bindings.push(ImportedName {
                                                imported: parts[0].trim().to_string(),
                                                local: parts[1].trim().to_string(),
                                            });
                                        }
                                    } else {
                                        bindings.push(ImportedName {
                                            imported: item_text.trim().to_string(),
                                            local: item_text.trim().to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            } else if kind == "import_statement" {
                let text = node
                    .utf8_text(source_bytes)
                    .unwrap_or("")
                    .trim_start_matches("import ")
                    .trim();
                if text.contains(" as ") {
                    let parts: Vec<&str> = text.split(" as ").collect();
                    if parts.len() == 2 {
                        module_source = parts[0].trim().to_string();
                        bindings.push(ImportedName {
                            imported: module_source.clone(),
                            local: parts[1].trim().to_string(),
                        });
                    }
                } else {
                    module_source = text.to_string();
                    bindings.push(ImportedName {
                        imported: text.to_string(),
                        local: text.to_string(),
                    });
                }
            }
        }
        Language::JavaScript | Language::TypeScript => {
            if let Some(source_node) = node.child_by_field_name("source") {
                module_source = source_node
                    .utf8_text(source_bytes)
                    .unwrap_or("")
                    .trim_matches(&['\'', '"'][..])
                    .to_string();
            }
            let mut cursor = node.walk();
            for child in named_children(&mut cursor) {
                if child.kind() == "import_clause" || child.kind() == "named_imports" {
                    let mut clause_cursor = child.walk();
                    for spec in named_children(&mut clause_cursor) {
                        if spec.kind() == "import_specifier" {
                            let name = spec
                                .child_by_field_name("name")
                                .and_then(|n| n.utf8_text(source_bytes).ok());
                            let alias = spec
                                .child_by_field_name("alias")
                                .and_then(|a| a.utf8_text(source_bytes).ok());
                            if let Some(imported) = name {
                                bindings.push(ImportedName {
                                    imported: imported.to_string(),
                                    local: alias.unwrap_or(imported).to_string(),
                                });
                            }
                        } else if spec.kind() == "identifier" {
                            if let Ok(name) = spec.utf8_text(source_bytes) {
                                bindings.push(ImportedName {
                                    imported: "default".to_string(),
                                    local: name.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Language::Go => {
            if let Some(path_node) = node.child_by_field_name("path") {
                module_source = path_node
                    .utf8_text(source_bytes)
                    .unwrap_or("")
                    .trim_matches(&['\'', '"', '`'][..])
                    .to_string();
            } else {
                module_source = node
                    .utf8_text(source_bytes)
                    .unwrap_or("")
                    .trim_matches(&['\'', '"', '`'][..])
                    .to_string();
            }
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(alias) = name_node.utf8_text(source_bytes) {
                    if let Some(pkg) = module_source.split('/').next_back() {
                        bindings.push(ImportedName {
                            imported: pkg.to_string(),
                            local: alias.to_string(),
                        });
                    }
                }
            } else if let Some(pkg) = module_source.split('/').next_back() {
                bindings.push(ImportedName {
                    imported: pkg.to_string(),
                    local: pkg.to_string(),
                });
            }
        }
        _ => {}
    }

    if !module_source.is_empty() || !bindings.is_empty() || is_glob {
        out.imports.push(ImportSite {
            file_id: file.id.clone(),
            scope_id: ctx.current_scope(),
            source: module_source,
            bindings,
            is_glob,
            is_type_only: false,
            reexported: false,
            range,
        });
    }
}

fn extract_rust_module_declaration(
    file: &File,
    source: &[u8],
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
    else {
        return;
    };
    let path_attribute_texts = rust_item_path_attribute_texts(node, source);
    out.module_declarations.push(ModuleDeclarationSite {
        file_id: file.id.clone(),
        scope_id: ctx.current_scope(),
        name: name.to_string(),
        has_body: node.child_by_field_name("body").is_some(),
        has_path_attribute: !path_attribute_texts.is_empty(),
        path_attributes: rust_item_path_attributes(node, source),
        path_is_conditional: rust_path_attributes_are_conditional(&path_attribute_texts),
        range: node_source_range(node),
    });
}

/// Whether the `path` attributes of an item, as [`rust_item_path_attribute_texts`] gives them,
/// leave the module at its default location on some build: each is a `cfg_attr`, and their
/// conditions are not ones that hold on every build (`all()`, or `X` beside `not(X)`, #613).
fn rust_path_attributes_are_conditional(texts: &[String]) -> bool {
    if texts.is_empty()
        || !texts
            .iter()
            .all(|text| without_whitespace(text).starts_with("#[cfg_attr("))
    {
        return false;
    }
    let conditions = texts
        .iter()
        .filter_map(|text| open_kioku_languages::rust::cfg_attr_condition(text))
        .collect::<Vec<_>>();
    !open_kioku_languages::rust::cfg_conditions_hold_on_every_build(&conditions)
}

/// The outer attributes before an item that set `path`, including through `cfg_attr`, each as
/// written. Outer attributes precede an item as siblings in tree-sitter-rust. Any
/// of them moves the module file off its default location, always or when its `cfg_attr`
/// condition holds; a false match only withholds a binding.
fn rust_item_path_attribute_texts(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut texts = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => {
                let text = previous.utf8_text(source).unwrap_or_default();
                if without_whitespace(text).contains("path=") {
                    texts.push(text.to_string());
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    texts
}

fn without_whitespace(text: &str) -> String {
    text.split_whitespace().collect()
}

/// The string literals the `path` attributes before an item set, as written. A crate other than
/// the declaring one may compile the file a `path` names, which the module tree reads to tell a
/// file shared by two crates from one crate's own.
fn rust_item_path_attributes(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => {
                paths.extend(attribute_path_literals(
                    previous.utf8_text(source).unwrap_or_default(),
                ));
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    paths.reverse();
    paths
}

/// The literals of each `path = "..."` in an attribute's text. A literal with an escape is
/// skipped: `\\` is the only one a path needs, and a skipped literal only leaves the path unknown.
fn attribute_path_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("path") {
        let before = rest[..at].chars().next_back();
        let after = rest[at + "path".len()..].trim_start();
        rest = &rest[at + "path".len()..];
        if before.is_some_and(|ch| ch == '_' || ch.is_alphanumeric()) {
            continue;
        }
        let Some(value) = after.strip_prefix('=').map(str::trim_start) else {
            continue;
        };
        let Some(literal) = value.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = literal.find('"') {
            let literal = &literal[..end];
            if !literal.contains('\\') {
                literals.push(literal.to_string());
            }
        }
    }
    literals
}

/// One path a Rust `use` declaration imports, with its `as` alias.
struct RustUseLeaf {
    path: String,
    alias: Option<String>,
    is_glob: bool,
}

/// Emits one import site per path a Rust `use` declaration imports. `use a::{b, c as d}` binds
/// two names from two paths, and import resolution reads a site's `source` as the full path of
/// what it binds, so a grouped declaration cannot share one site.
fn extract_rust_use(
    file: &File,
    source: &[u8],
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let Some(argument) = node.child_by_field_name("argument") else {
        return;
    };
    let mut leaves = Vec::new();
    collect_rust_use_leaves(argument, source, "", &mut leaves);
    let range = node_source_range(node);
    let reexported = rust_visibility(node) == Visibility::Public;
    for leaf in leaves {
        let bindings = if leaf.is_glob {
            Vec::new()
        } else {
            rust_use_binding(&leaf).into_iter().collect()
        };
        let imported_path = if leaf.is_glob {
            join_rust_use_path(&leaf.path, "*")
        } else {
            leaf.path
        };
        out.imports.push(ImportSite {
            file_id: file.id.clone(),
            scope_id: ctx.current_scope(),
            source: imported_path,
            bindings,
            is_glob: leaf.is_glob,
            is_type_only: false,
            reexported,
            range: range.clone(),
        });
    }
}

fn collect_rust_use_leaves(
    node: Node<'_>,
    source: &[u8],
    prefix: &str,
    out: &mut Vec<RustUseLeaf>,
) {
    let path_of = |field: &str| {
        node.child_by_field_name(field)
            .and_then(|child| rust_use_path_text(child, source))
    };
    match node.kind() {
        "use_as_clause" => {
            if let (Some(path), Some(alias)) = (path_of("path"), path_of("alias")) {
                out.push(RustUseLeaf {
                    path: join_rust_use_path(prefix, &path),
                    alias: Some(alias),
                    is_glob: false,
                });
            }
        }
        "use_wildcard" => {
            let path = node
                .named_child(0)
                .and_then(|child| rust_use_path_text(child, source))
                .unwrap_or_default();
            out.push(RustUseLeaf {
                path: join_rust_use_path(prefix, &path),
                alias: None,
                is_glob: true,
            });
        }
        "scoped_use_list" => {
            let prefix = join_rust_use_path(prefix, &path_of("path").unwrap_or_default());
            if let Some(list) = node.child_by_field_name("list") {
                collect_rust_use_leaves(list, source, &prefix, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in named_children(&mut cursor) {
                collect_rust_use_leaves(child, source, prefix, out);
            }
        }
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" => {
            if let Some(path) = rust_use_path_text(node, source) {
                out.push(RustUseLeaf {
                    path: join_rust_use_path(prefix, &path),
                    alias: None,
                    is_glob: false,
                });
            }
        }
        _ => {}
    }
}

fn rust_use_path_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node
        .utf8_text(source)
        .ok()?
        .split_whitespace()
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

/// Joins a `use` list prefix and a path inside the list; `self` in a list names the prefix.
fn join_rust_use_path(prefix: &str, path: &str) -> String {
    match (prefix, path) {
        ("", path) => path.to_string(),
        (prefix, "" | "self") => prefix.to_string(),
        (prefix, path) => format!("{prefix}::{path}"),
    }
}

fn rust_use_binding(leaf: &RustUseLeaf) -> Option<ImportedName> {
    let imported = leaf.path.rsplit("::").next()?;
    let local = leaf.alias.as_deref().unwrap_or(imported);
    // `as _` brings a trait into scope without naming it, and a bare `crate`, `self` or `super`
    // names no item.
    if matches!(local, "_" | "crate" | "self" | "super") {
        return None;
    }
    Some(ImportedName {
        imported: imported.to_string(),
        local: local.to_string(),
    })
}

fn extract_export(
    file: &File,
    content: &str,
    node: Node<'_>,
    _ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let kind = node.kind();
    let source_bytes = content.as_bytes();

    if (file.language == Language::JavaScript || file.language == Language::TypeScript)
        && kind == "export_statement"
    {
        let range = node_source_range(node);
        let mut cursor = node.walk();
        for child in named_children(&mut cursor) {
            if child.kind() == "export_clause" {
                let mut clause_cursor = child.walk();
                for spec in named_children(&mut clause_cursor) {
                    if spec.kind() == "export_specifier" {
                        let name = spec
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source_bytes).ok());
                        let alias = spec
                            .child_by_field_name("alias")
                            .and_then(|a| a.utf8_text(source_bytes).ok());
                        if let Some(n) = name {
                            out.exports.push(ExportSite {
                                file_id: file.id.clone(),
                                exported_name: alias.unwrap_or(n).to_string(),
                                local_name: Some(n.to_string()),
                                source_module: None,
                                is_glob: false,
                                range: range.clone(),
                            });
                        }
                    }
                }
            } else if child.kind() == "function_declaration"
                || child.kind() == "class_declaration"
                || child.kind() == "interface_declaration"
            {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source_bytes) {
                        out.exports.push(ExportSite {
                            file_id: file.id.clone(),
                            exported_name: name.to_string(),
                            local_name: Some(name.to_string()),
                            source_module: None,
                            is_glob: false,
                            range: range.clone(),
                        });
                    }
                }
            }
        }
    }
}

fn extract_inheritance(
    file: &File,
    content: &str,
    node: Node<'_>,
    ctx: &ParseContext,
    out: &mut SyntaxFacts,
) {
    let kind = node.kind();
    let source_bytes = content.as_bytes();

    match file.language {
        Language::Java | Language::JavaScript | Language::TypeScript
            if kind == "extends_clause" || kind == "implements_clause" =>
        {
            if let Some(child_symbol_id) = ctx.current_type().or_else(|| ctx.current_symbol()) {
                let inheritance_kind = if kind == "extends_clause" {
                    InheritanceKind::Extends
                } else {
                    InheritanceKind::Implements
                };
                let text = node.utf8_text(source_bytes).unwrap_or("");
                let parent_name = text
                    .trim_start_matches("extends")
                    .trim_start_matches("implements")
                    .trim()
                    .to_string();
                if !parent_name.is_empty() {
                    out.inheritance.push(InheritanceSite {
                        child_symbol_id,
                        parent_name,
                        kind: inheritance_kind,
                        order: 0,
                        range: node_source_range(node),
                    });
                }
            }
        }
        _ => {}
    }
}

fn variable_name<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for child in named_children(&mut cursor) {
        match child.kind() {
            "variable_declarator" | "variable_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    return Some(name);
                }
                if let Some(name) = variable_name(child) {
                    return Some(name);
                }
            }
            "identifier" | "property_identifier" => return Some(child),
            _ => {
                if let Some(name) = variable_name(child) {
                    return Some(name);
                }
            }
        }
    }
    None
}

fn named_children<'tree>(cursor: &mut TreeCursor<'tree>) -> Vec<Node<'tree>> {
    let node = cursor.node();
    (0..node.named_child_count())
        .filter_map(|index| node.named_child(index))
        .collect()
}

fn qualified_name(file: &File, name: &str) -> String {
    let stem = file
        .path
        .with_extension("")
        .to_string_lossy()
        .replace(['/', '\\'], "::");
    format!("{stem}::{name}")
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{parse_file, parse_symbols};
    use open_kioku_core::{File, FileId, Language, ReceiverKind, RepositoryId, Visibility};

    #[test]
    fn extracts_rust_symbols_from_tree_sitter() {
        let file = File {
            id: FileId::new("file"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbols = parse_symbols(&file, "pub struct Worker;\npub fn run() {}\n").unwrap();
        assert!(symbols.iter().any(|symbol| symbol.name == "Worker"));
        assert!(symbols.iter().any(|symbol| symbol.name == "run"));
        assert!(symbols
            .iter()
            .all(|symbol| symbol.provenance == open_kioku_core::EvidenceSourceType::TreeSitter));
    }

    #[test]
    fn rust_generic_impl_members_belong_to_the_type_without_its_generic_arguments() {
        let file = File {
            id: FileId::new("file_generic_impl"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            concat!(
                "pub struct PlanEngine<'a> { store: &'a str }\n",
                "impl<'a> PlanEngine<'a> {\n",
                "    pub fn new(store: &'a str) -> Self { PlanEngine { store } }\n",
                "}\n",
                "pub struct Wrapper<T>(T);\n",
                "impl<T> Wrapper<T> { pub fn get(&self) -> &T { &self.0 } }\n",
                "impl<T: Clone> Clone for Wrapper<T> { fn clone(&self) -> Self { Wrapper(self.0.clone()) } }\n",
                "pub trait Render { fn paint(&self); }\n",
                "impl<'a> Render for &'a Wrapper<u8> { fn paint(&self) {} }\n",
                "impl dyn Render { pub fn draw(&self) {} }\n",
            ),
        )
        .unwrap();
        let id_of = |name: &str| {
            facts
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .map(|symbol| symbol.id.clone())
                .unwrap()
        };
        let parent_of = |name: &str| {
            facts
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .and_then(|symbol| symbol.parent_symbol_id.clone())
                .unwrap()
        };
        assert_eq!(parent_of("new"), id_of("PlanEngine"));
        assert_eq!(parent_of("get"), id_of("Wrapper"));
        assert_eq!(parent_of("clone"), id_of("Wrapper"));
        let clone_impl = facts
            .inheritance
            .iter()
            .find(|site| site.parent_name == "Clone")
            .unwrap();
        assert_eq!(clone_impl.child_symbol_id, id_of("Wrapper"));
        // An `impl` for a reference or a trait object is not an `impl` of the referent or the
        // trait: its members stay off both.
        let paint_impl = facts
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "paint" && symbol.parent_symbol_id != Some(id_of("Render"))
            })
            .and_then(|symbol| symbol.parent_symbol_id.clone())
            .unwrap();
        assert_ne!(paint_impl, id_of("Wrapper"));
        assert_ne!(parent_of("draw"), id_of("Render"));
    }

    fn visibility_of(language: Language, path: &str, source: &str) -> Vec<(String, Visibility)> {
        let file = File {
            id: FileId::new("file_visibility"),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        parse_symbols(&file, source)
            .expect("visibility fixture should parse")
            .into_iter()
            .map(|symbol| (symbol.name, symbol.visibility))
            .collect()
    }

    #[test]
    fn rust_visibility_comes_from_the_items_own_modifier_not_its_text() {
        let symbols = visibility_of(
            Language::Rust,
            "src/lib.rs",
            concat!(
                "#[test]\n",
                "fn renders_helper() {\n",
                "    assert_eq!(render(), \"pub fn helper() {}\");\n",
                "}\n",
                "fn expands() {\n",
                "    macro_rules! make { () => { pub fn made() {} } }\n",
                "}\n",
                "struct Holder { pub field: u8 }\n",
                "/// Docs mention pub items.\n",
                "#[inline]\n",
                "#[cfg_attr(test, allow(dead_code))]\n",
                "pub fn exported() {}\n",
                "pub(crate) fn crate_wide() {}\n",
                "pub(super) fn parent_only() {}\n",
                "pub(in crate::outer) fn scoped() {}\n",
                "pub(self) fn module_only() {}\n",
                "pub mod api { fn inner() { let _ = \"pub \"; } }\n",
                "pub(crate) struct Registry;\n",
                "pub trait Store { fn load(&self); }\n",
                "pub const LIMIT: u8 = 1;\n",
                "pub type Alias = u8;\n",
            ),
        );
        let expected = [
            ("renders_helper", Visibility::Private),
            ("expands", Visibility::Private),
            ("Holder", Visibility::Private),
            ("exported", Visibility::Public),
            ("crate_wide", Visibility::Crate),
            ("parent_only", Visibility::Crate),
            ("scoped", Visibility::Crate),
            ("module_only", Visibility::Private),
            ("api", Visibility::Public),
            ("inner", Visibility::Private),
            ("Registry", Visibility::Crate),
            ("Store", Visibility::Public),
            ("LIMIT", Visibility::Public),
            ("Alias", Visibility::Public),
        ];
        for (name, visibility) in expected {
            assert!(
                symbols.contains(&(name.to_string(), visibility)),
                "{name} should be {visibility:?}; got {symbols:?}"
            );
        }
    }

    #[test]
    fn java_visibility_comes_from_the_declarations_own_modifiers_not_its_text() {
        let symbols = visibility_of(
            Language::Java,
            "src/main/java/app/Service.java",
            concat!(
                "class Service {\n",
                "    public void run() {}\n",
                "    void describe() { String s = \" public \"; }\n",
                "    @Override protected String name() { return \"x private y\"; }\n",
                "    private static final int LIMIT = 1;\n",
                "}\n",
                "public final class Api {}\n",
            ),
        );
        let expected = [
            ("Service", Visibility::Package),
            ("run", Visibility::Public),
            ("describe", Visibility::Package),
            ("name", Visibility::Protected),
            ("LIMIT", Visibility::Private),
            ("Api", Visibility::Public),
        ];
        for (name, visibility) in expected {
            assert!(
                symbols.contains(&(name.to_string(), visibility)),
                "{name} should be {visibility:?}; got {symbols:?}"
            );
        }
    }

    /// Pairs of `(name, visibility)` at a line: several items here share a name.
    fn visibility_at(
        language: Language,
        path: &str,
        source: &str,
    ) -> Vec<(u32, String, Visibility)> {
        let file = File {
            id: FileId::new("file_visibility"),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        parse_symbols(&file, source)
            .expect("visibility fixture should parse")
            .into_iter()
            .map(|symbol| {
                let line = symbol.range.map(|range| range.start).unwrap_or(0);
                (line, symbol.name, symbol.visibility)
            })
            .collect()
    }

    #[test]
    fn rust_trait_items_and_trait_impl_members_take_the_traits_visibility() {
        let symbols = visibility_at(
            Language::Rust,
            "src/lib.rs",
            concat!(
                "pub trait Store {
",
                "    const KIND: u8 = 0;
",
                "    fn load(&self) {
",
                "        fn scratch() {}
",
                "    }
",
                "}
",
                "pub(crate) trait Cache { fn evict(&self) {} }
",
                "trait Local { fn tidy(&self) {} }
",
                "pub struct Disk;
",
                "impl Store for Disk { fn load(&self) {} }
",
                "impl Cache for Disk { fn evict(&self) {} }
",
                "impl Local for Disk { fn tidy(&self) {} }
",
                "impl std::fmt::Display for Disk {
",
                "    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { Ok(()) }
",
                "}
",
                "impl From<u8> for Disk { fn from(_: u8) -> Self { Disk } }
",
                "struct Hidden;
",
                "impl Store for Hidden { fn load(&self) {} }
",
                "impl Disk {
",
                "    pub fn open() {}
",
                "    fn helper() {}
",
                "    pub(crate) fn shared() {}
",
                "}
",
            ),
        );
        let expected = [
            (2, "KIND", Visibility::Public),
            (3, "load", Visibility::Public),
            // Declared inside a default method's body, not in the trait: its own modifier.
            (4, "scratch", Visibility::Private),
            (7, "evict", Visibility::Crate),
            (8, "tidy", Visibility::Private),
            (10, "load", Visibility::Public),
            (11, "evict", Visibility::Crate),
            (12, "tidy", Visibility::Private),
            // External traits: callable wherever the trait is in scope.
            (14, "fmt", Visibility::Public),
            (16, "from", Visibility::Public),
            // A public trait implemented by a private type reaches no further than the type.
            (18, "load", Visibility::Private),
            // Inherent `impl` members keep their own modifier.
            (20, "open", Visibility::Public),
            (21, "helper", Visibility::Private),
            (22, "shared", Visibility::Crate),
        ];
        for (line, name, visibility) in expected {
            assert!(
                symbols.contains(&(line, name.to_string(), visibility)),
                "{name} at line {line} should be {visibility:?}; got {symbols:?}"
            );
        }
    }

    #[test]
    fn rust_trait_impl_of_an_ambiguous_or_unknown_trait_is_not_narrowed() {
        let symbols = visibility_at(
            Language::Rust,
            "src/lib.rs",
            concat!(
                "mod a { pub trait Probe { fn check(&self); } }
",
                "mod b { trait Probe { fn check(&self); } }
",
                "pub struct Target;
",
                "impl Probe for Target { fn check(&self) {} }
",
                "impl super::Sink for Target { fn drain(&self) {} }
",
            ),
        );
        for (line, name) in [(4, "check"), (5, "drain")] {
            assert!(
                symbols.contains(&(line, name.to_string(), Visibility::Public)),
                "{name} at line {line} should be Public; got {symbols:?}"
            );
        }
    }

    #[test]
    fn rust_trait_impl_reads_relative_trait_paths_and_looks_through_pointers() {
        let symbols = visibility_at(
            Language::Rust,
            "src/lib.rs",
            concat!(
                "pub trait Store { fn load(&self); }\n",
                "pub(crate) trait Cache { fn evict(&self); }\n",
                "struct Hidden;\n",
                "pub struct Gen<T>(T);\n",
                "impl Store for &Hidden { fn load(&self) {} }\n",
                "impl Store for &mut Hidden { fn load(&self) {} }\n",
                "impl Store for Box<Hidden> { fn load(&self) {} }\n",
                "impl Store for std::rc::Rc<Hidden> { fn load(&self) {} }\n",
                "impl<'a> Store for &'a Arc<Hidden> { fn load(&self) {} }\n",
                "impl crate::Cache for Gen<u8> { fn evict(&self) {} }\n",
                "impl self::Cache for Gen<u16> { fn evict(&self) {} }\n",
                "impl Store for crate::Hidden { fn load(&self) {} }\n",
                "mod inner {\n",
                "    pub struct Inner;\n",
                "    impl super::Cache for Inner { fn evict(&self) {} }\n",
                "    impl crate::Store for Inner { fn load(&self) {} }\n",
                "}\n",
                "impl Store for std::pin::Pin<Box<Hidden>> { fn load(&self) {} }\n",
                // A type parameter named like a private type is any type: a blanket impl.
                "impl<Hidden> Cache for Box<Hidden> { fn evict(&self) {} }\n",
                "impl<Hidden: Clone> Store for Hidden { fn load(&self) {} }\n",
            ),
        );
        let expected = [
            (5, "load", Visibility::Private),
            (6, "load", Visibility::Private),
            (7, "load", Visibility::Private),
            (8, "load", Visibility::Private),
            (9, "load", Visibility::Private),
            (10, "evict", Visibility::Crate),
            (11, "evict", Visibility::Crate),
            (12, "load", Visibility::Private),
            (15, "evict", Visibility::Crate),
            (16, "load", Visibility::Public),
            (18, "load", Visibility::Private),
            (19, "evict", Visibility::Crate),
            (20, "load", Visibility::Public),
        ];
        for (line, name, visibility) in expected {
            assert!(
                symbols.contains(&(line, name.to_string(), visibility)),
                "{name} at line {line} should be {visibility:?}; got {symbols:?}"
            );
        }
    }

    #[test]
    fn rust_trait_impl_paths_into_another_file_are_not_narrowed() {
        let symbols = visibility_at(
            Language::Rust,
            "src/store.rs",
            concat!(
                "pub(crate) trait Cache { fn evict(&self); }\n",
                "pub trait Store { fn load(&self); }\n",
                "struct Hidden;\n",
                "pub struct Box<T>(T);\n",
                "pub struct Disk;\n",
                // Outside a crate root, `crate::` and a top-level `super::` name another file.
                "impl crate::Cache for Disk { fn evict(&self) {} }\n",
                "impl super::Cache for Disk { fn evict(&self) {} }\n",
                // This file's own `Box` is not the standard pointer.
                "impl Store for Box<Hidden> { fn load(&self) {} }\n",
                // A module path longer than `self`/`super`/`crate` is not followed.
                "impl crate::store::Cache for Disk { fn evict(&self) {} }\n",
                "impl Store for Vec<Hidden> { fn load(&self) {} }\n",
            ),
        );
        for (line, name) in [
            (6, "evict"),
            (7, "evict"),
            (8, "load"),
            (9, "evict"),
            (10, "load"),
        ] {
            assert!(
                symbols.contains(&(line, name.to_string(), Visibility::Public)),
                "{name} at line {line} should be Public; got {symbols:?}"
            );
        }
    }

    #[test]
    fn java_interface_members_without_an_access_keyword_are_public() {
        let symbols = visibility_at(
            Language::Java,
            "src/main/java/app/Store.java",
            concat!(
                "interface Store {
",
                "    void load();
",
                "    default String name() { return \"store\"; }
",
                "    static Store empty() { return null; }
",
                "    private void audit() {}
",
                "    class Entry {}
",
                "    interface Listener { void changed(); }
",
                "}
",
                "@interface Marker { class Holder {} }
",
                "class Service {
",
                "    void describe() {}
",
                "    interface Callback { void done(); }
",
                "    private interface Hidden { void run(); }
",
                "}
",
            ),
        );
        let expected = [
            (1, "Store", Visibility::Package),
            (2, "load", Visibility::Public),
            (3, "name", Visibility::Public),
            (4, "empty", Visibility::Public),
            (5, "audit", Visibility::Private),
            (6, "Entry", Visibility::Public),
            (7, "Listener", Visibility::Public),
            (7, "changed", Visibility::Public),
            (9, "Holder", Visibility::Public),
            // Class members keep the package default.
            (11, "describe", Visibility::Package),
            (12, "Callback", Visibility::Package),
            (12, "done", Visibility::Public),
            // A member records its own access, not its reach through a private enclosing type.
            (13, "Hidden", Visibility::Private),
            (13, "run", Visibility::Public),
        ];
        for (line, name, visibility) in expected {
            assert!(
                symbols.contains(&(line, name.to_string(), visibility)),
                "{name} at line {line} should be {visibility:?}; got {symbols:?}"
            );
        }
    }

    #[test]
    fn rust_scoped_paths_distinguish_modules_from_instance_self() {
        let file = File {
            id: FileId::new("file_rust_paths"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "fn run() { crate::crate_target(); self::self_target(); super::super_target(); Self::type_target(); self.instance_target(); }",
        )
        .expect("Rust qualified-call fixture should parse");
        let kind_for = |callee: &str| {
            facts
                .calls
                .iter()
                .find(|call| call.callee_name == callee)
                .map(|call| (call.receiver.as_deref(), call.receiver_kind))
                .expect("qualified call")
        };

        assert_eq!(
            kind_for("crate_target"),
            (Some("crate"), ReceiverKind::Module)
        );
        assert_eq!(
            kind_for("self_target"),
            (Some("self"), ReceiverKind::Module)
        );
        assert_eq!(
            kind_for("super_target"),
            (Some("super"), ReceiverKind::Module)
        );
        assert_eq!(kind_for("type_target"), (Some("Self"), ReceiverKind::Self_));
        assert_eq!(
            kind_for("instance_target"),
            (Some("self"), ReceiverKind::Self_)
        );
    }

    #[test]
    fn rust_path_receivers_are_told_apart_from_values_that_share_their_text() {
        let file = File {
            id: FileId::new("file_rust_paths"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "fn run(items: Vec<Thing>) { engine::path_call(); engine::inner::nested_call(); u32::primitive_call(); Engine::type_call(); items.iter().for_each(|engine| engine.value_call()); }",
        )
        .expect("Rust call fixture should parse");
        let kind_for = |callee: &str| {
            facts
                .calls
                .iter()
                .find(|call| call.callee_name == callee)
                .map(|call| (call.receiver.as_deref(), call.receiver_kind))
                .expect("call")
        };
        // `engine::f()` and `engine.f()` share the receiver text `engine`; only the path is a
        // module, and only a module receiver may name a crate.
        assert_eq!(
            kind_for("path_call"),
            (Some("engine"), ReceiverKind::Module)
        );
        assert_eq!(
            kind_for("nested_call"),
            (Some("engine::inner"), ReceiverKind::Module)
        );
        assert_eq!(
            kind_for("value_call"),
            (Some("engine"), ReceiverKind::Value)
        );
        assert_eq!(
            kind_for("primitive_call"),
            (Some("u32"), ReceiverKind::Type)
        );
        assert_eq!(kind_for("type_call"), (Some("Engine"), ReceiverKind::Type));
    }

    #[test]
    fn does_not_emit_json_keys_as_symbols() {
        let file = File {
            id: FileId::new("file"),
            repository_id: RepositoryId::new("repo"),
            path: "config/settings.json".into(),
            language: Language::Json,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbols = parse_symbols(&file, r#"{"cluster": {"name": "local"}}"#).unwrap();
        assert!(symbols.is_empty());
    }

    #[test]
    fn extracts_java_contextual_syntax_facts() {
        let file = File {
            id: FileId::new("file_java"),
            repository_id: RepositoryId::new("repo"),
            path: "com/acme/Service.java".into(),
            language: Language::Java,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let code = r#"
            package com.acme;
            import com.acme.repo.Repository;

            public class Service {
                private Repository repo;

                public void process() {
                    Repository localRepo = new Repository();
                    this.repo.save(x);
                    Repo.save(x);
                }
            }
        "#;
        let facts = parse_file(&file, code).unwrap();
        assert!(!facts.symbols.is_empty());
        assert!(!facts.scopes.is_empty());
        assert!(!facts.calls.is_empty());
        assert!(facts
            .calls
            .iter()
            .any(|c| c.callee_name == "save" && c.receiver_kind == ReceiverKind::Self_));
        assert!(facts
            .calls
            .iter()
            .any(|c| c.callee_name == "save" && c.receiver_kind == ReceiverKind::Type));
        assert!(facts.calls.iter().all(|c| c.caller_symbol_id.is_some()));
        assert!(facts
            .bindings
            .iter()
            .any(|b| b.name == "repo" && b.declared_type == Some("Repository".into())));
    }
}

#[cfg(test)]
mod ri3_rust_module_receiver_tests {
    use super::parse_file;
    use open_kioku_core::{File, FileId, Language, ReceiverKind, RepositoryId};
    use std::path::PathBuf;

    #[test]
    fn crate_qualified_rust_call_is_classified_as_module_receiver() {
        let file = File {
            id: FileId::new("file:src/domain/call_violation.rs"),
            repository_id: RepositoryId::new("repo:test"),
            path: PathBuf::from("src/domain/call_violation.rs"),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(&file, "pub fn write() { crate::storage::persist(); }")
            .expect("Rust fixture should parse");
        let call = facts
            .calls
            .iter()
            .find(|call| call.callee_name == "persist")
            .expect("qualified persist call should be extracted");

        assert_eq!(call.receiver.as_deref(), Some("crate::storage"));
        assert_eq!(call.receiver_kind, ReceiverKind::Module);
    }
}

#[cfg(test)]
mod ri3_rust_use_import_site_tests {
    use super::{attribute_path_literals, parse_file};
    use open_kioku_core::{File, FileId, ImportSite, ImportedName, Language, RepositoryId};

    #[test]
    fn rust_files_record_a_macro_invoked_at_their_top_level() {
        let invokes = |source: &str| {
            let file = File {
                id: FileId::new("file:src/facade.rs"),
                repository_id: RepositoryId::new("repo"),
                path: "src/facade.rs".into(),
                language: Language::Rust,
                size_bytes: 0,
                content_hash: "hash".into(),
                is_generated: false,
                is_vendor: false,
            };
            parse_file(&file, source)
                .expect("Rust macro fixture should parse")
                .invokes_item_macro
        };
        assert!(invokes("pub use crate::store::*;\nmake_items!();\n"));
        assert!(invokes(
            "cfg_select! {\n    unix => { pub use unix::*; }\n}\n"
        ));
        assert!(invokes("crate::local_fn!();\n"));
        assert!(!invokes(
            "macro_rules! m { () => {} }\npub fn f() { println!(\"x\"); }\nmod inner { m!(); }\n"
        ));
    }

    fn rust_import_sites(source: &str) -> Vec<ImportSite> {
        let file = File {
            id: FileId::new("file:src/session.rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/session.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        parse_file(&file, source)
            .expect("Rust import fixture should parse")
            .imports
    }

    fn name(imported: &str, local: &str) -> ImportedName {
        ImportedName {
            imported: imported.into(),
            local: local.into(),
        }
    }

    #[test]
    fn rust_item_import_site_keeps_the_full_path_and_binds_the_last_segment() {
        let sites = rust_import_sites("use crate::auth::issue_token;\n");
        assert_eq!(sites.len(), 1, "{sites:?}");
        assert_eq!(sites[0].source, "crate::auth::issue_token");
        assert_eq!(sites[0].bindings, vec![name("issue_token", "issue_token")]);
        assert!(!sites[0].is_glob);
    }

    #[test]
    fn rust_grouped_import_emits_one_site_per_imported_path() {
        let sites = rust_import_sites(
            "use crate::auth::{issue_token, Token as AuthToken, keys::{rotate}, self};\n",
        );
        let sources = sites
            .iter()
            .map(|site| site.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            vec![
                "crate::auth::issue_token",
                "crate::auth::Token",
                "crate::auth::keys::rotate",
                "crate::auth",
            ]
        );
        let bindings = sites
            .iter()
            .map(|site| site.bindings.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            bindings,
            vec![
                vec![name("issue_token", "issue_token")],
                vec![name("Token", "AuthToken")],
                vec![name("rotate", "rotate")],
                vec![name("auth", "auth")],
            ]
        );
    }

    #[test]
    fn rust_module_declarations_record_body_path_attribute_and_enclosing_scope() {
        let file = File {
            id: FileId::new("file:src/lib.rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "pub mod auth;\n#[cfg(test)]\nmod tests {\n    mod nested;\n}\n#[path = \"store_v2.rs\"]\nmod store;\n/// Platform glue.\n#[cfg_attr(unix, path = \"unix.rs\")]\nmod platform;\n#[cfg_attr(unix, path = \"a.rs\")]\n#[cfg_attr( windows ,\n  path = \"b.rs\")]\nmod both;\n#[cfg_attr(unix, path = \"a.rs\")]\n#[path = \"c.rs\"]\nmod mixed;\n#[cfg_attr(unix, path = r\"raw.rs\")]\nmod unread;\n#[cfg_attr( all( ), path = \"x.rs\")]\nmod always;\n#[cfg_attr(unix, path = \"u.rs\")]\n#[cfg_attr(not( unix ), path = \"o.rs\")]\nmod paired;\n#[cfg_attr(unix, path = \"u.rs\")]\n#[cfg_attr(not(windows), path = \"o.rs\")]\nmod unpaired;\n#[cfg_attr(feature = \"a b\", path = \"u.rs\")]\n#[cfg_attr(not(feature = \"ab\"), path = \"o.rs\")]\nmod spaced;\n#[cfg_attr(unix, cfg_attr(feature = \"x\", path = \"a.rs\"))]\n#[cfg_attr(not(unix), path = \"b.rs\")]\nmod layered;\n",
        )
        .expect("Rust module fixture should parse");
        let declaration = |name: &str| {
            facts
                .module_declarations
                .iter()
                .find(|declaration| declaration.name == name)
                .unwrap_or_else(|| {
                    panic!("`mod {name}` declaration: {:?}", facts.module_declarations)
                })
        };

        let auth = declaration("auth");
        assert!(!auth.has_body && !auth.has_path_attribute);
        let tests = declaration("tests");
        assert!(tests.has_body && !tests.has_path_attribute);
        let nested = declaration("nested");
        assert!(!nested.has_body);
        assert_ne!(
            nested.scope_id, auth.scope_id,
            "nested sits in the inline module's scope"
        );
        assert!(declaration("store").has_path_attribute);
        assert!(declaration("platform").has_path_attribute);
        assert!(auth.path_attributes.is_empty());
        assert_eq!(declaration("store").path_attributes, vec!["store_v2.rs"]);
        assert_eq!(declaration("platform").path_attributes, vec!["unix.rs"]);
        // A module whose every `path` is set through `cfg_attr` also compiles from its default
        // location; one `path` that always applies moves it off that location for good (#608).
        for conditional in [
            "platform", "both", "unread", "unpaired", "spaced", "layered",
        ] {
            assert!(
                declaration(conditional).path_is_conditional,
                "{conditional}"
            );
        }
        // `all()`, and a condition beside its own `not(..)`, hold on every build, so the default
        // location is never compiled (#613).
        for unconditional in ["auth", "tests", "store", "mixed", "always", "paired"] {
            assert!(
                !declaration(unconditional).path_is_conditional,
                "{unconditional}"
            );
        }
        assert!(declaration("unread").path_attributes.is_empty());
        assert_eq!(declaration("paired").path_attributes, vec!["u.rs", "o.rs"]);
    }

    #[test]
    fn attribute_path_literals_read_each_path_value() {
        assert_eq!(
            attribute_path_literals("#[path = \"a/b.rs\"]"),
            vec!["a/b.rs"]
        );
        assert_eq!(
            attribute_path_literals(
                "#[cfg_attr(unix, path=\"unix.rs\")] #[cfg_attr(windows, path = \"win.rs\")]"
            ),
            vec!["unix.rs", "win.rs"]
        );
        assert!(attribute_path_literals("#[doc = \"xpath = \\\"x\\\"\"]").is_empty());
        assert!(attribute_path_literals("#[path = \"a\\\\b.rs\"]").is_empty());
        assert!(attribute_path_literals("#[path = concat!(\"a\", \".rs\")]").is_empty());
    }

    #[test]
    fn rust_aliased_import_binds_the_alias_to_the_item() {
        let sites =
            rust_import_sites("use crate::auth::issue_token as mint;\nuse std::io::Write as _;\n");
        assert_eq!(sites.len(), 2, "{sites:?}");
        assert_eq!(sites[0].source, "crate::auth::issue_token");
        assert_eq!(sites[0].bindings, vec![name("issue_token", "mint")]);
        assert_eq!(sites[1].source, "std::io::Write");
        assert!(sites[1].bindings.is_empty(), "{:?}", sites[1].bindings);
    }

    #[test]
    fn rust_glob_and_visibility_qualified_imports_keep_only_their_paths() {
        let sites =
            rust_import_sites("pub(crate) use super::*;\npub use self::auth::issue_token;\n");
        assert_eq!(sites.len(), 2, "{sites:?}");
        assert_eq!(sites[0].source, "super::*");
        assert!(sites[0].is_glob);
        assert!(sites[0].bindings.is_empty());
        assert_eq!(sites[1].source, "self::auth::issue_token");
        assert_eq!(sites[1].bindings, vec![name("issue_token", "issue_token")]);
        // Only an unrestricted `pub use` lets other crates name what it imports.
        assert!(!sites[0].reexported);
        assert!(sites[1].reexported);
        assert!(!rust_import_sites("use crate::auth::issue_token;\n")[0].reexported);
    }
}

#[cfg(test)]
mod ri3_rust_binding_type_tests {
    use super::parse_file;
    use open_kioku_core::{Binding, File, FileId, Language, RepositoryId, ScopeKind};

    fn rust_bindings(source: &str) -> Vec<Binding> {
        let file = File {
            id: FileId::new("file:src/caller.rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/caller.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        parse_file(&file, source)
            .expect("Rust binding fixture should parse")
            .bindings
    }

    fn binding<'b>(bindings: &'b [Binding], name: &str) -> &'b Binding {
        bindings
            .iter()
            .find(|binding| binding.name == name)
            .unwrap_or_else(|| panic!("binding `{name}`: {bindings:?}"))
    }

    #[test]
    fn rust_path_call_initializer_is_recorded_as_the_whole_call_path() {
        let bindings = rust_bindings(
            "pub fn run() {\n    let handle = Server::spawn();\n    let config = Config { port: 1 };\n}\n",
        );
        let handle = binding(&bindings, "handle");
        assert_eq!(handle.declared_type, None);
        assert_eq!(handle.inferred_type.as_deref(), Some("Server::spawn()"));
        assert_eq!(
            binding(&bindings, "config").inferred_type.as_deref(),
            Some("Config")
        );
    }

    #[test]
    fn rust_type_naming_an_enclosing_type_parameter_is_not_recorded() {
        let bindings = rust_bindings(
            "pub fn parse<Token: Parse>(token: Token, raw: &Raw) {\n    let copy: Token = token;\n}\n\nimpl<Item> Queue<Item> {\n    pub fn push(&mut self, item: Item) {}\n}\n",
        );
        assert_eq!(binding(&bindings, "token").declared_type, None);
        assert_eq!(binding(&bindings, "copy").declared_type, None);
        assert_eq!(binding(&bindings, "item").declared_type, None);
        assert_eq!(
            binding(&bindings, "raw").declared_type.as_deref(),
            Some("Raw")
        );
    }

    #[test]
    fn rust_closure_parameters_bind_in_the_closure_alone() {
        let file = File {
            id: FileId::new("file:src/caller.rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/caller.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "pub fn run(ctx: Ring) {\n    let submit = |ctx: &mut Ring| ctx.submit();\n    ctx.flush();\n}\n",
        )
        .expect("Rust closure fixture should parse");
        let scope_of = |binding: &Binding| {
            facts
                .scopes
                .iter()
                .find(|scope| scope.id == binding.scope_id)
                .map(|scope| scope.kind)
        };
        let ctx = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "ctx")
            .collect::<Vec<_>>();
        assert_eq!(ctx.len(), 2, "{ctx:?}");
        assert_eq!(scope_of(ctx[0]), Some(ScopeKind::Function));
        assert_eq!(ctx[1].declared_type.as_deref(), Some("mut Ring"));
        assert_eq!(scope_of(ctx[1]), Some(ScopeKind::Closure));
    }

    #[test]
    fn rust_struct_fields_bind_in_the_struct_with_their_declared_types() {
        let file = File {
            id: FileId::new("file:src/caller.rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/caller.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "pub struct Holder<T> {\n    pub store: &'static Store,\n    item: T,\n    boxed: Box<T>,\n}\npub enum Shape {\n    Named { side: Side },\n}\npub union Bits {\n    raw: Raw,\n}\npub struct Pair(Left, Right);\n",
        )
        .expect("Rust struct fixture should parse");
        let owner_of = |binding: &Binding| {
            let scope = facts
                .scopes
                .iter()
                .find(|scope| scope.id == binding.scope_id)?;
            let owner = facts
                .symbols
                .iter()
                .find(|symbol| Some(&symbol.id) == scope.owner_symbol_id.as_ref())?;
            Some((scope.kind, owner.name.clone()))
        };
        let fields = facts
            .bindings
            .iter()
            .map(|binding| {
                (
                    binding.name.as_str(),
                    binding.declared_type.as_deref(),
                    owner_of(binding),
                )
            })
            .collect::<Vec<_>>();
        let holder = Some((ScopeKind::Class, "Holder".to_string()));
        // A field typed by the struct's own type parameter has no declared type; the fields of an
        // enum variant, a union or a tuple struct are not recorded.
        assert_eq!(
            fields,
            vec![
                ("store", Some("&'static Store"), holder.clone()),
                ("item", None, holder.clone()),
                ("boxed", Some("Box<T>"), holder),
            ]
        );
    }
}

#[cfg(test)]
mod ri3_go_type_classification_tests {
    use super::parse_file;
    use open_kioku_core::{File, FileId, Language, RepositoryId, SymbolKind};

    #[test]
    fn go_named_types_are_classified_as_type_symbols() {
        let file = File {
            id: FileId::new("file_go_types"),
            repository_id: RepositoryId::new("repo"),
            path: "main.go".into(),
            language: Language::Go,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "package bench\ntype TargetType struct{}\ntype TargetInterface interface{ Target() }\nfunc CallerFn(value TargetType) {}\n",
        )
        .expect("Go type fixture should parse");
        let concrete = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "TargetType")
            .expect("concrete Go type");
        let interface = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "TargetInterface")
            .expect("Go interface type");
        assert_eq!(concrete.kind, SymbolKind::Class);
        assert_eq!(interface.kind, SymbolKind::Interface);
        assert!(facts.bindings.iter().any(|binding| binding.name == "value"
            && binding.declared_type.as_deref() == Some("TargetType")));
    }

    #[test]
    fn go_type_aliases_are_type_symbols_that_record_their_target() {
        let file = File {
            id: FileId::new("file_go_aliases"),
            repository_id: RepositoryId::new("repo"),
            path: "ledger/aliases.go".into(),
            language: Language::Go,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "package ledger\n\nimport \"example.com/app/store\"\n\ntype Entry = store.Entry\n\ntype (\n\tBatch = store.Batch\n\tLocal = Record\n\tRows  = store.Page[Record]\n\tRaw   = []byte\n\tRef   = *store.Entry\n)\n\ntype Record struct{}\n",
        )
        .expect("Go alias fixture should parse");
        let alias = |name: &str| {
            let symbol = facts
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap_or_else(|| panic!("alias `{name}` is a symbol: {:?}", facts.symbols));
            let site = facts
                .type_aliases
                .iter()
                .find(|site| site.symbol_id == symbol.id)
                .unwrap_or_else(|| panic!("alias `{name}` records its target"));
            (
                symbol.kind.clone(),
                symbol.signature.clone(),
                site.target_package.clone(),
                site.target_name.clone(),
            )
        };
        let target = |package: Option<&str>, name: Option<&str>| {
            (package.map(str::to_string), name.map(str::to_string))
        };
        let (kind, signature, package, name) = alias("Entry");
        assert_eq!(kind, SymbolKind::Class);
        assert_eq!(signature.as_deref(), Some("type Entry = store.Entry"));
        assert_eq!((package, name), target(Some("store"), Some("Entry")));
        let (_, _, package, name) = alias("Batch");
        assert_eq!((package, name), target(Some("store"), Some("Batch")));
        let (_, _, package, name) = alias("Local");
        assert_eq!((package, name), target(None, Some("Record")));
        let (_, _, package, name) = alias("Rows");
        assert_eq!((package, name), target(Some("store"), Some("Page")));
        let (_, _, package, name) = alias("Raw");
        assert_eq!((package, name), target(None, None));
        let (_, _, package, name) = alias("Ref");
        assert_eq!((package, name), target(None, None));
        // A defined type is not an alias.
        assert_eq!(facts.type_aliases.len(), 6, "{:?}", facts.type_aliases);
    }

    #[test]
    fn java_and_go_files_record_the_package_they_declare() {
        let declared = |path: &str, language: Language, content: &str| {
            let file = File {
                id: FileId::new(path),
                repository_id: RepositoryId::new("repo"),
                path: path.into(),
                language,
                size_bytes: 0,
                content_hash: "hash".into(),
                is_generated: false,
                is_vendor: false,
            };
            let facts = parse_file(&file, content).expect("package fixture should parse");
            facts.package_declaration.map(|site| {
                assert_eq!(site.file_id, file.id);
                site.name
            })
        };
        // The declaration, not the directory, names a Java file's package.
        assert_eq!(
            declared(
                "src/Constants.java",
                Language::Java,
                "package org.example;\npublic class Constants {}\n",
            )
            .as_deref(),
            Some("org.example")
        );
        assert_eq!(
            declared(
                "src/org/example/package-info.java",
                Language::Java,
                "@Deprecated\npackage org . example\n    .app;\n",
            )
            .as_deref(),
            Some("org.example.app")
        );
        assert_eq!(
            declared("Main.java", Language::Java, "public class Main {}\n"),
            None
        );
        // A Go test file may declare the external test package beside the package it tests.
        assert_eq!(
            declared(
                "store/store_test.go",
                Language::Go,
                "package store_test\n\nimport \"testing\"\n\nfunc TestEntry(t *testing.T) {}\n",
            )
            .as_deref(),
            Some("store_test")
        );
        assert_eq!(
            declared("src/lib.rs", Language::Rust, "pub fn run() {}\n"),
            None
        );
    }
}
