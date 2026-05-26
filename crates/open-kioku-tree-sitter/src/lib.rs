use open_kioku_core::{CodeChunk, File, FileId, Language, LineRange, Symbol, SymbolId, SymbolKind};
use open_kioku_errors::{OkError, Result};
use tree_sitter::Parser;

pub fn parse_file(file: &File, content: &str) -> Result<Vec<CodeChunk>> {
    let mut parser = Parser::new();
    let language = ts_language(file.language).ok_or_else(|| {
        OkError::Unsupported(format!("no tree-sitter grammar for {:?}", file.language))
    })?;
    parser
        .set_language(&language)
        .map_err(|err| OkError::Parse {
            path: file.path.clone(),
            message: err.to_string(),
        })?;
    let tree = parser.parse(content, None).ok_or_else(|| OkError::Parse {
        path: file.path.clone(),
        message: "tree-sitter returned no tree".into(),
    })?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(OkError::Parse {
            path: file.path.clone(),
            message: "tree-sitter parse tree contains errors".into(),
        });
    }
    let chunks = extract_chunks(file, content, &root);
    Ok(chunks)
}

fn extract_chunks(file: &File, content: &str, root: &tree_sitter::Node) -> Vec<CodeChunk> {
    let mut chunks = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let kind = node.kind();
        let is_decl = matches!(
            kind,
            "function_definition"
                | "function_item"
                | "impl_item"
                | "class_definition"
                | "method_definition"
                | "decorated_definition"
        );
        if !is_decl {
            continue;
        }
        let start = node.start_position().row as u32 + 1;
        let end = node.end_position().row as u32 + 1;
        let text = &content[node.byte_range()];
        let snippet: String = text.chars().take(500).collect();
        chunks.push(CodeChunk {
            id: format!("ts:{}:{}", file.path.display(), start),
            file_id: file.id.clone(),
            symbol_id: None,
            language: file.language,
            text: snippet,
            range: LineRange { start, end },
        });
    }
    chunks
}

pub fn extract_symbols(file: &File, content: &str) -> Result<Vec<Symbol>> {
    let mut parser = Parser::new();
    let language = ts_language(file.language).ok_or_else(|| {
        OkError::Unsupported(format!("no tree-sitter grammar for {:?}", file.language))
    })?;
    parser
        .set_language(&language)
        .map_err(|err| OkError::Parse {
            path: file.path.clone(),
            message: err.to_string(),
        })?;
    let tree = parser.parse(content, None).ok_or_else(|| OkError::Parse {
        path: file.path.clone(),
        message: "tree-sitter returned no tree".into(),
    })?;
    let root = tree.root_node();
    let symbols = extract_symbol_nodes(file, content, &root);
    Ok(symbols)
}

fn extract_symbol_nodes(file: &File, content: &str, root: &tree_sitter::Node) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let (name_node, kind) = match node.kind() {
            "function_item" => (node.child_by_field_name("name"), SymbolKind::Function),
            "impl_item" => (node.child_by_field_name("type"), SymbolKind::Class),
            "struct_item" => (node.child_by_field_name("name"), SymbolKind::Class),
            "enum_item" => (node.child_by_field_name("name"), SymbolKind::Constant),
            "trait_item" => (node.child_by_field_name("name"), SymbolKind::Interface),
            "function_definition" | "async_function_definition" => {
                (node.child_by_field_name("name"), SymbolKind::Function)
            }
            "class_definition" => (node.child_by_field_name("name"), SymbolKind::Class),
            _ => continue,
        };
        let Some(name_node) = name_node else { continue };
        let name = &content[name_node.byte_range()];
        let start = node.start_position().row as u32 + 1;
        let end = node.end_position().row as u32 + 1;
        symbols.push(Symbol {
            id: SymbolId::new(format!("{}:{}", file.path.display(), name)),
            name: name.into(),
            kind,
            file_id: FileId::new(file.path.display().to_string()),
            range: LineRange { start, end },
            signature: None,
            doc_comment: None,
        });
    }
    symbols
}

fn ts_language(lang: Language) -> Option<tree_sitter::Language> {
    match lang {
        Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Language::TypeScript | Language::TSX => {
            Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        }
        Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::Go => Some(tree_sitter_go::LANGUAGE.into()),
        _ => None,
    }
}
