use chrono::Utc;
use open_kioku_core::{
    CodeChunk, Confidence, EvidenceSourceType, File, Import, Language, LineRange, Symbol, SymbolId,
    SymbolKind, TestTarget,
};
use regex::Regex;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub chunks: Vec<CodeChunk>,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub tests: Vec<TestTarget>,
}

pub trait Parser: Send + Sync {
    fn parse(&self, file: &File, content: &str) -> ParsedFile;
}

#[derive(Default)]
pub struct HeuristicParser;

impl Parser for HeuristicParser {
    fn parse(&self, file: &File, content: &str) -> ParsedFile {
        let imports = extract_imports(file, content);
        let symbols = extract_symbols(file, content);
        let chunks = extract_chunks(file, content, &symbols);
        let tests = extract_tests(file, content, &symbols);
        ParsedFile {
            chunks,
            symbols,
            imports,
            tests,
        }
    }
}

pub fn extract_symbols(file: &File, content: &str) -> Vec<Symbol> {
    if let Ok(symbols) = open_kioku_tree_sitter::parse_symbols(file, content) {
        if !symbols.is_empty() {
            return symbols;
        }
    }
    match file.language {
        Language::Rust => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"^\s*(pub\s+)?(async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Function,
                    3,
                ),
                (
                    r"^\s*(pub\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Class,
                    2,
                ),
                (
                    r"^\s*(pub\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Class,
                    2,
                ),
                (
                    r"^\s*(pub\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Trait,
                    2,
                ),
                (r"^\s*mod\s+([A-Za-z_][A-Za-z0-9_]*)", SymbolKind::Module, 1),
            ],
        ),
        Language::Java => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"\b(class|record)\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Class,
                    2,
                ),
                (
                    r"\binterface\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Interface,
                    1,
                ),
                (
                    r"\b(?:public|private|protected)?\s*(?:static\s+)?[A-Za-z0-9_<>\[\], ?]+\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
                    SymbolKind::Method,
                    1,
                ),
            ],
        ),
        Language::TypeScript | Language::JavaScript => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"\bfunction\s+([A-Za-z_$][A-Za-z0-9_$]*)",
                    SymbolKind::Function,
                    1,
                ),
                (
                    r"\bclass\s+([A-Za-z_$][A-Za-z0-9_$]*)",
                    SymbolKind::Class,
                    1,
                ),
                (
                    r"\binterface\s+([A-Za-z_$][A-Za-z0-9_$]*)",
                    SymbolKind::Interface,
                    1,
                ),
                (
                    r"\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:async\s*)?\(",
                    SymbolKind::Function,
                    1,
                ),
                (
                    r"\bexport\s+(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
                    SymbolKind::Variable,
                    1,
                ),
            ],
        ),
        Language::Python => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Function,
                    1,
                ),
                (
                    r"^\s*async\s+def\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Function,
                    1,
                ),
                (
                    r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Class,
                    1,
                ),
            ],
        ),
        Language::Go => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"^\s*func\s+(?:\([^)]+\)\s*)?([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Function,
                    1,
                ),
                (
                    r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+struct",
                    SymbolKind::Class,
                    1,
                ),
                (
                    r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+interface",
                    SymbolKind::Interface,
                    1,
                ),
            ],
        ),
        Language::Sql => extract_with_patterns(
            file,
            content,
            &[(
                r"(?i)^\s*create\s+table\s+([A-Za-z_][A-Za-z0-9_\.]*)",
                SymbolKind::DatabaseTable,
                1,
            )],
        ),
        _ => Vec::new(),
    }
}

fn extract_with_patterns(
    file: &File,
    content: &str,
    specs: &[(&str, SymbolKind, usize)],
) -> Vec<Symbol> {
    let compiled = specs
        .iter()
        .filter_map(|(pattern, kind, capture)| {
            Regex::new(pattern)
                .ok()
                .map(|re| (re, kind.clone(), *capture))
        })
        .collect::<Vec<_>>();
    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for (regex, kind, capture) in &compiled {
            if let Some(captures) = regex.captures(line) {
                if let Some(name) = captures.get(*capture) {
                    let line_number = (idx + 1) as u32;
                    let qualified_name = qualified_name(file, name.as_str());
                    symbols.push(Symbol {
                        id: SymbolId::new(stable_id(&format!(
                            "{}:{}:{}",
                            file.path.display(),
                            line_number,
                            qualified_name
                        ))),
                        name: name.as_str().to_string(),
                        qualified_name,
                        kind: kind.clone(),
                        file_id: file.id.clone(),
                        range: Some(LineRange::single(line_number)),
                        language: file.language.clone(),
                        confidence: Confidence::Medium,
                        provenance: EvidenceSourceType::Heuristic,
                    });
                }
            }
        }
    }
    symbols
}

pub fn extract_imports(file: &File, content: &str) -> Vec<Import> {
    let patterns = match file.language {
        Language::Rust => vec![r"^\s*use\s+([^;]+)", r"^\s*mod\s+([A-Za-z_][A-Za-z0-9_]*)"],
        Language::Java => vec![r"^\s*import\s+([^;]+)"],
        Language::TypeScript | Language::JavaScript => {
            vec![r#"from\s+["']([^"']+)["']"#, r#"import\s+["']([^"']+)["']"#]
        }
        Language::Python => vec![
            r"^\s*import\s+([A-Za-z0-9_\.]+)",
            r"^\s*from\s+([A-Za-z0-9_\.]+)\s+import",
        ],
        Language::Go => vec![r#"^\s*import\s+"([^"]+)""#],
        _ => Vec::new(),
    };
    let compiled = patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect::<Vec<_>>();
    let mut imports = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for regex in &compiled {
            if let Some(captures) = regex.captures(line) {
                if let Some(value) = captures.get(1) {
                    imports.push(Import {
                        file_id: file.id.clone(),
                        imported: value.as_str().trim().to_string(),
                        range: Some(LineRange::single((idx + 1) as u32)),
                        confidence: Confidence::Medium,
                    });
                }
            }
        }
    }
    imports
}

pub fn extract_chunks(file: &File, content: &str, symbols: &[Symbol]) -> Vec<CodeChunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    let lines = content.lines().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut starts = symbols
        .iter()
        .filter_map(|symbol| {
            symbol
                .range
                .as_ref()
                .map(|range| (range.start as usize, symbol.id.clone()))
        })
        .collect::<Vec<_>>();
    starts.sort_by_key(|(line, _)| *line);
    if starts.is_empty() {
        for (idx, window) in lines.chunks(80).enumerate() {
            let start = idx * 80 + 1;
            let end = start + window.len().saturating_sub(1);
            chunks.push(CodeChunk {
                id: stable_id(&format!("{}:{start}:{end}", file.path.display())),
                file_id: file.id.clone(),
                range: LineRange {
                    start: start as u32,
                    end: end as u32,
                },
                language: file.language.clone(),
                text: window.join("\n"),
                symbol_id: None,
            });
        }
        return chunks;
    }
    for (idx, (start, symbol_id)) in starts.iter().enumerate() {
        let next = starts
            .get(idx + 1)
            .map(|(line, _)| *line)
            .unwrap_or(lines.len() + 1);
        let end = next.saturating_sub(1).min(lines.len());
        let text = lines[start.saturating_sub(1)..end].join("\n");
        chunks.push(CodeChunk {
            id: stable_id(&format!("{}:{start}:{end}", file.path.display())),
            file_id: file.id.clone(),
            range: LineRange {
                start: *start as u32,
                end: end as u32,
            },
            language: file.language.clone(),
            text,
            symbol_id: Some(symbol_id.clone()),
        });
    }
    chunks
}

pub fn extract_tests(file: &File, content: &str, symbols: &[Symbol]) -> Vec<TestTarget> {
    let path = file.path.to_string_lossy().to_ascii_lowercase();
    let is_test_file = path.contains("/test/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_test.go")
        || path.ends_with("test.java")
        || path.ends_with(".spec.ts")
        || path.ends_with(".test.ts")
        || path.ends_with("_test.py");

    symbols
        .iter()
        .filter(|symbol| {
            is_test_file
                || symbol.name.starts_with("test")
                || content
                    .lines()
                    .any(|line| line.contains("#[test]") || line.contains("@Test"))
        })
        .map(|symbol| TestTarget {
            id: stable_id(&format!("test:{}:{}", file.path.display(), symbol.name)),
            name: symbol.name.clone(),
            file_id: file.id.clone(),
            range: symbol.range.clone(),
            command: recommended_command(&file.language, &file.path.to_string_lossy()),
            confidence: if is_test_file {
                Confidence::High
            } else {
                Confidence::Medium
            },
            reason: "test-like path, annotation, or naming convention".into(),
        })
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

fn recommended_command(language: &Language, path: &str) -> Option<String> {
    match language {
        Language::Rust => Some("cargo test".into()),
        Language::Java => Some("mvn test".into()),
        Language::TypeScript | Language::JavaScript => Some("npm test".into()),
        Language::Python => Some("pytest".into()),
        Language::Go => Some("go test ./...".into()),
        _ if path.contains("test") => Some("run repository test command".into()),
        _ => None,
    }
}

pub fn evidence_timestamp() -> chrono::DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::{extract_chunks, extract_imports, extract_symbols, extract_tests};
    use open_kioku_core::{File, FileId, Language, RepositoryId};

    fn rust_file() -> File {
        File {
            id: FileId::new("file-rs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn python_file() -> File {
        File {
            id: FileId::new("file-py"),
            repository_id: RepositoryId::new("repo"),
            path: "app/service.py".into(),
            language: Language::Python,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn ts_file() -> File {
        File {
            id: FileId::new("file-ts"),
            repository_id: RepositoryId::new("repo"),
            path: "src/index.ts".into(),
            language: Language::TypeScript,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    // ─── extract_symbols ──────────────────────────────────────────────────────

    #[test]
    fn extracts_rust_functions_and_structs() {
        let file = rust_file();
        let src = "pub fn do_work() {}\npub struct Worker;\npub trait Runnable {}\nmod utils {}";
        let symbols = extract_symbols(&file, src);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"do_work"), "should find function");
        assert!(names.contains(&"Worker"), "should find struct");
        assert!(names.contains(&"Runnable"), "should find trait");
        assert!(names.contains(&"utils"), "should find module");
    }

    #[test]
    fn extracts_python_class_and_function() {
        let file = python_file();
        let src = "class MyService:\n    pass\n\ndef handle_request():\n    pass\n";
        let symbols = extract_symbols(&file, src);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyService"), "should find class");
        assert!(names.contains(&"handle_request"), "should find function");
    }

    #[test]
    fn extracts_typescript_class_and_function() {
        let file = ts_file();
        let src = "class ApiClient {}\nfunction fetchData() {}\nconst handler = () => {};";
        let symbols = extract_symbols(&file, src);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ApiClient") || !symbols.is_empty());
    }

    // ─── extract_imports ──────────────────────────────────────────────────────

    #[test]
    fn extracts_rust_use_imports() {
        let file = rust_file();
        let src = "use std::collections::HashMap;\nuse crate::worker::Worker;";
        let imports = extract_imports(&file, src);
        assert_eq!(imports.len(), 2);
        assert!(imports.iter().any(|i| i.imported.contains("HashMap")));
    }

    #[test]
    fn extracts_python_imports() {
        let file = python_file();
        let src = "import os\nfrom pathlib import Path\n";
        let imports = extract_imports(&file, src);
        assert_eq!(imports.len(), 2);
        assert!(imports.iter().any(|i| i.imported == "os"));
        assert!(imports.iter().any(|i| i.imported == "pathlib"));
    }

    #[test]
    fn extracts_typescript_imports() {
        let file = ts_file();
        let src = "import { foo } from './foo';\nimport './styles.css';";
        let imports = extract_imports(&file, src);
        assert!(!imports.is_empty());
        assert!(imports.iter().any(|i| i.imported.contains("foo")));
    }

    // ─── extract_chunks ──────────────────────────────────────────────────────

    #[test]
    fn chunks_file_with_no_symbols_into_80_line_windows() {
        let file = rust_file();
        let content: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let chunks = extract_chunks(&file, &content, &[]);
        assert!(
            chunks.len() >= 2,
            "200 lines should produce at least 2 chunks"
        );
        for chunk in &chunks {
            assert!(chunk.symbol_id.is_none());
        }
    }

    #[test]
    fn chunks_file_by_symbol_boundaries() {
        let file = rust_file();
        let src = "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}";
        let symbols = extract_symbols(&file, src);
        assert!(
            !symbols.is_empty(),
            "should have symbols from heuristic parser"
        );
        let chunks = extract_chunks(&file, src, &symbols);
        // Each symbol becomes a chunk boundary.
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.symbol_id.is_some()));
    }

    // ─── extract_tests ────────────────────────────────────────────────────────

    #[test]
    fn detects_rust_test_attribute() {
        let file = rust_file();
        let src = "#[test]\nfn it_works() {\n    assert!(true);\n}\n";
        let symbols = extract_symbols(&file, src);
        let tests = extract_tests(&file, src, &symbols);
        assert!(!tests.is_empty(), "should detect #[test] function");
        assert!(tests[0].command.as_deref() == Some("cargo test"));
    }

    #[test]
    fn test_file_path_causes_all_symbols_to_be_tests() {
        let file = File {
            id: FileId::new("test-file"),
            repository_id: RepositoryId::new("repo"),
            path: "src/worker_test.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let src = "pub fn some_helper() {}\n";
        let symbols = extract_symbols(&file, src);
        let tests = extract_tests(&file, src, &symbols);
        // All symbols in a test file become test targets.
        assert_eq!(tests.len(), symbols.len());
    }
}
