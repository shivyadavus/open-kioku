use open_kioku_core::{
    Confidence, EvidenceSourceType, File, Language, LineRange, Symbol, SymbolId, SymbolKind,
};
use open_kioku_errors::{OcfError, Result};
use sha2::{Digest, Sha256};
use tree_sitter::{Language as TsLanguage, Node, Parser, TreeCursor};

pub fn parser_for(language: &Language) -> Result<Parser> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language(language)?)
        .map_err(|err| OcfError::Parse {
            path: "<language>".into(),
            message: err.to_string(),
        })?;
    Ok(parser)
}

pub fn parse_symbols(file: &File, content: &str) -> Result<Vec<Symbol>> {
    let mut parser = parser_for(&file.language)?;
    let tree = parser.parse(content, None).ok_or_else(|| OcfError::Parse {
        path: file.path.clone(),
        message: "tree-sitter returned no parse tree".into(),
    })?;
    if tree.root_node().has_error() {
        return Err(OcfError::Parse {
            path: file.path.clone(),
            message: "tree-sitter parse contains syntax errors".into(),
        });
    }
    let mut symbols = Vec::new();
    walk(file, content, tree.root_node(), &mut symbols);
    symbols.sort_by_key(|symbol| symbol.range.as_ref().map(|range| range.start).unwrap_or(0));
    symbols.dedup_by(|a, b| a.id == b.id);
    Ok(symbols)
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
        _ => Err(OcfError::Unsupported(format!(
            "tree-sitter parser not configured for {language:?}"
        ))),
    }
}

fn walk(file: &File, content: &str, node: Node<'_>, symbols: &mut Vec<Symbol>) {
    if let Some((name_node, kind)) = symbol_name_node(file, node) {
        if let Ok(name) = name_node.utf8_text(content.as_bytes()) {
            if !name.is_empty() {
                let line_range = LineRange {
                    start: (node.start_position().row + 1) as u32,
                    end: (node.end_position().row + 1) as u32,
                };
                let qualified_name = qualified_name(file, name);
                symbols.push(Symbol {
                    id: SymbolId::new(stable_id(&format!(
                        "{}:{}:{}",
                        file.path.display(),
                        line_range.start,
                        qualified_name
                    ))),
                    name: name.to_string(),
                    qualified_name,
                    kind,
                    file_id: file.id.clone(),
                    range: Some(line_range),
                    language: file.language.clone(),
                    confidence: Confidence::High,
                    provenance: EvidenceSourceType::TreeSitter,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in named_children(&mut cursor) {
        walk(file, content, child, symbols);
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
        Language::Json | Language::Yaml => match kind {
            "pair" | "block_mapping_pair" => node
                .child_by_field_name("key")
                .map(|node| (node, SymbolKind::Field)),
            _ => None,
        },
        _ => None,
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
    use super::parse_symbols;
    use open_kioku_core::{File, FileId, Language, RepositoryId};

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
}
