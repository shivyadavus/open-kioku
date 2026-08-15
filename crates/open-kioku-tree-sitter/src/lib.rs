use open_kioku_core::{
    Binding, BindingId, CallSite, CallSiteId, Confidence, EvidenceSourceType, ExportSite, File,
    ImportSite, ImportedName, InheritanceKind, InheritanceSite, Language, LineRange, ReceiverKind,
    Scope, ScopeId, ScopeKind, SourceRange, Symbol, SymbolId, SymbolKind, SyntaxFacts, Visibility,
};
use open_kioku_errors::{OkError, Result};
use sha2::{Digest, Sha256};
use tree_sitter::{Language as TsLanguage, Node, Parser, TreeCursor};

pub struct ParseContext {
    pub scope_stack: Vec<ScopeId>,
    pub symbol_stack: Vec<SymbolId>,
    pub callable_stack: Vec<SymbolId>,
    pub type_stack: Vec<SymbolId>,
    pub next_scope_counter: u32,
}

impl ParseContext {
    pub fn new() -> Self {
        Self {
            scope_stack: Vec::new(),
            symbol_stack: Vec::new(),
            callable_stack: Vec::new(),
            type_stack: Vec::new(),
            next_scope_counter: 0,
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

    out.symbols
        .sort_by_key(|symbol| symbol.range.as_ref().map(|range| range.start).unwrap_or(0));
    out.symbols.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

pub fn parse_symbols(file: &File, content: &str) -> Result<Vec<Symbol>> {
    Ok(parse_file(file, content)?.symbols)
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

    if let Some((name_node, symbol_kind)) = symbol_name_node(file, node) {
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
                let visibility = extract_symbol_visibility(file, content, node);

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
                };

                out.symbols.push(symbol);
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

fn extract_symbol_visibility(file: &File, content: &str, node: Node<'_>) -> Visibility {
    let source_bytes = content.as_bytes();
    let text = node.utf8_text(source_bytes).unwrap_or("");
    match file.language {
        Language::Java => {
            if text.starts_with("public ") || text.contains(" public ") {
                Visibility::Public
            } else if text.starts_with("private ") || text.contains(" private ") {
                Visibility::Private
            } else if text.starts_with("protected ") || text.contains(" protected ") {
                Visibility::Protected
            } else {
                Visibility::Package
            }
        }
        Language::Rust => {
            if text.starts_with("pub ") || text.contains("pub ") {
                Visibility::Public
            } else {
                Visibility::Private
            }
        }
        Language::Go => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source_bytes) {
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

fn symbol_name_node<'tree>(file: &File, node: Node<'tree>) -> Option<(Node<'tree>, SymbolKind)> {
    let kind = node.kind();
    let name = node.child_by_field_name("name");
    match file.language {
        Language::Rust => match kind {
            "function_item" => name.map(|node| (node, SymbolKind::Function)),
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
            "type_spec" => name.map(|node| {
                let symbol_kind =
                    if node.parent().map(|parent| parent.kind()) == Some("type_declaration") {
                        SymbolKind::Class
                    } else {
                        SymbolKind::Unknown
                    };
                (node, symbol_kind)
            }),
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
                        receiver_kind = classify_receiver_string(&recv);
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
                            receiver_kind = classify_receiver_string(&recv);
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
                            receiver_kind = classify_receiver_string(&recv);
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
                            receiver_kind = classify_receiver_string(&recv);
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
                            receiver_kind = classify_receiver_string(&recv);
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
                            receiver_kind = classify_receiver_string(&recv);
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
            "{}:call:{}:{}:{}",
            file.path.display(),
            range.start_line,
            range.start_column,
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

fn classify_receiver_string(recv: &str) -> ReceiverKind {
    let recv = recv.trim();
    if recv == "this"
        || recv == "self"
        || recv == "Self"
        || recv.starts_with("this.")
        || recv.starts_with("self.")
        || recv.starts_with("Self::")
    {
        ReceiverKind::Self_
    } else if recv == "super"
        || recv == "Super"
        || recv.starts_with("super.")
        || recv.starts_with("Super::")
    {
        ReceiverKind::Super
    } else if recv
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && !recv.contains('.')
    {
        ReceiverKind::Type
    } else {
        ReceiverKind::Value
    }
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
                    .map(|s| s.to_string());
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
                    .map(|s| s.trim_start_matches('&').trim().to_string());
                if let Some(n) = name {
                    extracted.push((n, declared_type, None));
                }
            } else if kind == "self_parameter" {
                extracted.push(("self".to_string(), None, None));
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

fn infer_type_from_expr(file: &File, source: &[u8], expr: Node<'_>) -> Option<String> {
    let kind = expr.kind();
    match file.language {
        Language::Java | Language::JavaScript | Language::TypeScript => {
            if kind == "new_expression" || kind == "object_creation_expression" {
                if let Some(type_node) = expr.child_by_field_name("type") {
                    return type_node.utf8_text(source).ok().map(|s| s.to_string());
                } else if let Some(constructor) = expr.child_by_field_name("constructor") {
                    return constructor.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
        }
        Language::Rust => {
            if kind == "call_expression" {
                if let Some(function) = expr.child_by_field_name("function") {
                    if function.kind() == "scoped_identifier" {
                        if let Some(path) = function.child_by_field_name("path") {
                            return path.utf8_text(source).ok().map(|s| s.to_string());
                        }
                    }
                }
            } else if kind == "struct_expression" {
                if let Some(name) = expr.child_by_field_name("name") {
                    return name.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
        }
        Language::Python => {
            if kind == "call" {
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
                    if let Some(last) = text.split('.').last() {
                        bindings.push(ImportedName {
                            imported: last.to_string(),
                            local: last.to_string(),
                        });
                    }
                }
            }
        }
        Language::Rust => {
            if let Ok(text) = node.utf8_text(source_bytes) {
                let text = text
                    .trim_start_matches("pub")
                    .trim_start_matches("use")
                    .trim_end_matches(';')
                    .trim();
                module_source = text.to_string();
                if text.ends_with("::*") {
                    is_glob = true;
                } else if let Some(last) = text.split("::").last() {
                    if last.contains(" as ") {
                        let parts: Vec<&str> = last.split(" as ").collect();
                        if parts.len() == 2 {
                            bindings.push(ImportedName {
                                imported: parts[0].trim().to_string(),
                                local: parts[1].trim().to_string(),
                            });
                        }
                    } else {
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
                    if let Some(pkg) = module_source.split('/').last() {
                        bindings.push(ImportedName {
                            imported: pkg.to_string(),
                            local: alias.to_string(),
                        });
                    }
                }
            } else if let Some(pkg) = module_source.split('/').last() {
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
            range,
        });
    }
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

    if file.language == Language::JavaScript || file.language == Language::TypeScript {
        if kind == "export_statement" {
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
        Language::Java | Language::JavaScript | Language::TypeScript => {
            if kind == "extends_clause" || kind == "implements_clause" {
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
    use open_kioku_core::{File, FileId, Language, ReceiverKind, RepositoryId};

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
