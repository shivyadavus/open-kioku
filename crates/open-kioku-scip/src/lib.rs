use open_kioku_core::{
    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, Symbol, SymbolId,
    SymbolKind, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
use protobuf::{Enum, Message};
use scip::types::{symbol_information, Index, SymbolRole};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScipImport {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<SymbolOccurrence>,
}

pub fn import_scip_file(
    path: impl AsRef<Path>,
    repository_id: &RepositoryId,
) -> Result<ScipImport> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ScipImport {
            symbols: Vec::new(),
            occurrences: Vec::new(),
        });
    }
    let bytes = fs::read(path)?;
    let index = Index::parse_from_bytes(&bytes).map_err(|err| OkError::Index(err.to_string()))?;
    Ok(convert_index(index, repository_id))
}

fn convert_index(index: Index, repository_id: &RepositoryId) -> ScipImport {
    let mut symbols = Vec::new();
    let mut occurrences = Vec::new();
    for document in index.documents {
        let file_id = FileId::new(stable_id(&document.relative_path));
        let language = language_from_scip(&document.language);
        for info in &document.symbols {
            symbols.push(Symbol {
                id: SymbolId::new(stable_id(&info.symbol)),
                name: display_name(info),
                qualified_name: info.symbol.clone(),
                kind: symbol_kind(
                    info.kind
                        .enum_value_or(symbol_information::Kind::UnspecifiedKind),
                ),
                file_id: file_id.clone(),
                range: definition_range(&document.occurrences, &info.symbol),
                language: language.clone(),
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            });
        }
        for occurrence in document.occurrences {
            if occurrence.symbol.is_empty() {
                continue;
            }
            occurrences.push(SymbolOccurrence {
                symbol_id: SymbolId::new(stable_id(&occurrence.symbol)),
                file_id: file_id.clone(),
                range: scip_range(&occurrence.range),
                is_definition: has_role(occurrence.symbol_roles, SymbolRole::Definition),
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            });
        }
    }
    symbols.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    symbols.dedup_by(|a, b| a.id == b.id);
    occurrences.sort_by(|a, b| {
        (
            &a.symbol_id.0,
            &a.file_id.0,
            a.range.as_ref().map(|range| range.start),
            a.is_definition,
        )
            .cmp(&(
                &b.symbol_id.0,
                &b.file_id.0,
                b.range.as_ref().map(|range| range.start),
                b.is_definition,
            ))
    });
    occurrences.dedup_by(|a, b| {
        a.symbol_id == b.symbol_id
            && a.file_id == b.file_id
            && a.range == b.range
            && a.is_definition == b.is_definition
    });
    let _ = repository_id;
    ScipImport {
        symbols,
        occurrences,
    }
}

fn display_name(info: &scip::types::SymbolInformation) -> String {
    if !info.display_name.is_empty() {
        return info.display_name.clone();
    }
    info.symbol
        .trim_end_matches('.')
        .rsplit(['/', '#', '.', ' '])
        .find(|part| !part.is_empty())
        .unwrap_or(&info.symbol)
        .to_string()
}

fn definition_range(occurrences: &[scip::types::Occurrence], symbol: &str) -> Option<LineRange> {
    occurrences
        .iter()
        .find(|occurrence| {
            occurrence.symbol == symbol && has_role(occurrence.symbol_roles, SymbolRole::Definition)
        })
        .and_then(|occurrence| scip_range(&occurrence.range))
}

fn scip_range(range: &[i32]) -> Option<LineRange> {
    match range {
        [start_line, _, end_line, _] => Some(LineRange {
            start: (*start_line + 1).max(1) as u32,
            end: (*end_line + 1).max(1) as u32,
        }),
        [start_line, _, _] => Some(LineRange::single((*start_line + 1).max(1) as u32)),
        _ => None,
    }
}

fn has_role(roles: i32, role: SymbolRole) -> bool {
    roles & role.value() != 0
}

fn language_from_scip(language: &str) -> Language {
    match language.to_ascii_lowercase().as_str() {
        "rust" => Language::Rust,
        "java" => Language::Java,
        "typescript" | "ts" => Language::TypeScript,
        "javascript" | "js" => Language::JavaScript,
        "python" | "py" => Language::Python,
        "go" => Language::Go,
        "json" => Language::Json,
        "yaml" | "yml" => Language::Yaml,
        "toml" => Language::Toml,
        "sql" => Language::Sql,
        _ => Language::Unknown,
    }
}

fn symbol_kind(kind: symbol_information::Kind) -> SymbolKind {
    match kind {
        symbol_information::Kind::Class
        | symbol_information::Kind::Struct
        | symbol_information::Kind::Enum
        | symbol_information::Kind::Object
        | symbol_information::Kind::Type => SymbolKind::Class,
        symbol_information::Kind::Interface => SymbolKind::Interface,
        symbol_information::Kind::Method
        | symbol_information::Kind::Constructor
        | symbol_information::Kind::StaticMethod
        | symbol_information::Kind::TraitMethod => SymbolKind::Method,
        symbol_information::Kind::Function => SymbolKind::Function,
        symbol_information::Kind::Field
        | symbol_information::Kind::StaticField
        | symbol_information::Kind::Property => SymbolKind::Field,
        symbol_information::Kind::Constant => SymbolKind::Constant,
        symbol_information::Kind::Module => SymbolKind::Module,
        symbol_information::Kind::Package => SymbolKind::Package,
        symbol_information::Kind::Variable
        | symbol_information::Kind::StaticVariable
        | symbol_information::Kind::Value => SymbolKind::Variable,
        symbol_information::Kind::Trait => SymbolKind::Trait,
        symbol_information::Kind::TypeParameter => SymbolKind::Class,
        _ => SymbolKind::Unknown,
    }
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
fn _project_path(root: &Path, relative: &str) -> PathBuf {
    root.join(relative)
}

#[cfg(test)]
mod tests {
    use super::import_scip_file;
    use open_kioku_core::RepositoryId;
    use protobuf::Enum;
    use scip::types::{
        symbol_information, Document, Index, Occurrence, SymbolInformation, SymbolRole,
    };

    #[test]
    fn imports_binary_scip_index() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.scip");
        let symbol_name = "scip rust test src/lib.rs/ retry_import().";

        let mut index = Index::new();
        let mut document = Document::new();
        document.relative_path = "src/lib.rs".into();
        document.language = "rust".into();

        let mut info = SymbolInformation::new();
        info.symbol = symbol_name.into();
        info.display_name = "retry_import".into();
        info.kind = symbol_information::Kind::Function.into();
        document.symbols.push(info);

        let mut definition = Occurrence::new();
        definition.symbol = symbol_name.into();
        definition.range = vec![8, 0, 8, 12];
        definition.symbol_roles = SymbolRole::Definition.value();
        document.occurrences.push(definition);

        let mut reference = Occurrence::new();
        reference.symbol = symbol_name.into();
        reference.range = vec![3, 8, 3, 20];
        document.occurrences.push(reference);

        index.documents.push(document);
        scip::write_message_to_file(&path, index).unwrap();

        let imported = import_scip_file(&path, &RepositoryId::new("repo")).unwrap();
        assert_eq!(imported.symbols.len(), 1);
        assert_eq!(imported.symbols[0].name, "retry_import");
        assert_eq!(imported.occurrences.len(), 2);
        assert!(imported
            .occurrences
            .iter()
            .any(|occurrence| occurrence.is_definition));
        assert!(imported
            .occurrences
            .iter()
            .any(|occurrence| !occurrence.is_definition));
    }
}
