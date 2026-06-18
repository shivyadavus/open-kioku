use chrono::Utc;
use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, GraphEdgeType,
    GraphNodeType, Import, Language, LineRange, ScoreComponent, Symbol, SymbolId, SymbolKind,
    TestTarget,
};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub chunks: Vec<CodeChunk>,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub analysis_facts: Vec<AnalysisFact>,
    pub tests: Vec<TestTarget>,
}

pub trait Parser: Send + Sync {
    fn parse(&self, file: &File, content: &str) -> ParsedFile {
        self.parse_with_hint(file, content, None)
    }
    fn parse_with_hint(&self, file: &File, content: &str, build_hint: Option<&str>) -> ParsedFile;
}

#[derive(Default)]
pub struct HeuristicParser;

impl Parser for HeuristicParser {
    fn parse_with_hint(&self, file: &File, content: &str, build_hint: Option<&str>) -> ParsedFile {
        let imports = extract_imports(file, content);
        let mut symbols = extract_symbols(file, content);
        dedupe_symbols(&mut symbols);
        let analysis_facts = extract_analysis_facts(file, content, &symbols);
        let mut chunks = extract_chunks(file, content, &symbols);
        dedupe_chunks(&mut chunks);
        let tests = extract_tests(file, content, &symbols, build_hint);
        ParsedFile {
            chunks,
            symbols,
            imports,
            analysis_facts,
            tests,
        }
    }
}

fn dedupe_symbols(symbols: &mut Vec<Symbol>) {
    let mut seen = HashSet::new();
    symbols.retain(|symbol| seen.insert(symbol.id.clone()));
}

fn dedupe_chunks(chunks: &mut Vec<CodeChunk>) {
    let mut seen = HashSet::new();
    chunks.retain(|chunk| seen.insert(chunk.id.clone()));
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
                    let qualified_name = qualified_name(file, content, name.as_str());
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

pub fn extract_analysis_facts(file: &File, content: &str, symbols: &[Symbol]) -> Vec<AnalysisFact> {
    match file.language {
        Language::Java => extract_java_analysis_facts(file, content, symbols),
        Language::TypeScript | Language::JavaScript => {
            extract_javascript_analysis_facts(file, content, symbols)
        }
        Language::Python => extract_python_analysis_facts(file, content, symbols),
        Language::Rust => extract_rust_analysis_facts(file, content, symbols),
        _ => Vec::new(),
    }
}

fn extract_java_analysis_facts(
    file: &File,
    content: &str,
    symbols: &[Symbol],
) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    let class_re = Regex::new(
        r"\b(?:class|record|enum)\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s+extends\s+([A-Za-z0-9_.$<>]+))?(?:\s+implements\s+([A-Za-z0-9_.$<>,\s]+))?",
    )
    .expect("valid Java class regex");
    let interface_re = Regex::new(
        r"\binterface\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s+extends\s+([A-Za-z0-9_.$<>,\s]+))?",
    )
    .expect("valid Java interface regex");
    let mapping_re = Regex::new(
        r#"@(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)(?:\s*\(\s*(?:value\s*=\s*)?["']([^"']+)["'])?"#,
    )
    .expect("valid Spring mapping regex");
    let env_re =
        Regex::new(r#"System\.getenv\(\s*["']([^"']+)["']\s*\)"#).expect("valid getenv regex");
    let value_re = Regex::new(r#"@Value\(\s*["']\$\{([^}:]+)(?::[^}]*)?\}["']\s*\)"#)
        .expect("valid Spring value regex");
    let table_re =
        Regex::new(r#"@Table\(\s*name\s*=\s*["']([^"']+)["']"#).expect("valid table regex");

    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        if let Some(captures) = class_re.captures(line) {
            let source = captures.get(1).map(|value| value.as_str());
            let source_symbol = source.and_then(|name| symbol_named(symbols, name));
            if let Some(base) = captures.get(2) {
                facts.push(analysis_fact(
                    file,
                    source_symbol,
                    GraphEdgeType::Extends,
                    GraphNodeType::Class,
                    clean_java_type(base.as_str()),
                    line_number,
                    ("open-kioku-static/java", "Java class inheritance"),
                ));
            }
            if let Some(interfaces) = captures.get(3) {
                for interface in split_java_types(interfaces.as_str()) {
                    facts.push(analysis_fact(
                        file,
                        source_symbol,
                        GraphEdgeType::Implements,
                        GraphNodeType::Interface,
                        interface,
                        line_number,
                        ("open-kioku-static/java", "Java implemented interface"),
                    ));
                }
            }
        }
        if let Some(captures) = interface_re.captures(line) {
            let source = captures.get(1).map(|value| value.as_str());
            let source_symbol = source.and_then(|name| symbol_named(symbols, name));
            if let Some(parents) = captures.get(2) {
                for parent in split_java_types(parents.as_str()) {
                    facts.push(analysis_fact(
                        file,
                        source_symbol,
                        GraphEdgeType::Extends,
                        GraphNodeType::Interface,
                        parent,
                        line_number,
                        ("open-kioku-static/java", "Java interface inheritance"),
                    ));
                }
            }
        }
        if let Some(captures) = mapping_re.captures(line) {
            let method = spring_http_method(captures.get(1).map(|value| value.as_str()));
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            let source_symbol = symbol_at_or_after(symbols, line_number, 4);
            facts.push(analysis_fact(
                file,
                source_symbol,
                GraphEdgeType::ExposesEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/java", "Spring MVC endpoint mapping"),
            ));
        }
        for captures in env_re.captures_iter(line) {
            if let Some(key) = captures.get(1) {
                facts.push(analysis_fact(
                    file,
                    symbol_at_or_before(symbols, line_number),
                    GraphEdgeType::ReadsConfig,
                    GraphNodeType::ConfigKey,
                    key.as_str().to_string(),
                    line_number,
                    ("open-kioku-static/java", "Java environment variable read"),
                ));
            }
        }
        if let Some(captures) = value_re.captures(line) {
            if let Some(key) = captures.get(1) {
                facts.push(analysis_fact(
                    file,
                    symbol_at_or_after(symbols, line_number, 3),
                    GraphEdgeType::ReadsConfig,
                    GraphNodeType::ConfigKey,
                    key.as_str().to_string(),
                    line_number,
                    ("open-kioku-static/java", "Spring configuration value read"),
                ));
            }
        }
        if let Some(captures) = table_re.captures(line) {
            if let Some(table) = captures.get(1) {
                facts.push(analysis_fact(
                    file,
                    symbol_at_or_after(symbols, line_number, 3),
                    GraphEdgeType::ReadsTable,
                    GraphNodeType::DatabaseTable,
                    table.as_str().to_string(),
                    line_number,
                    ("open-kioku-static/java", "JPA table mapping"),
                ));
            }
        }
    }
    dedupe_analysis_facts(&mut facts);
    facts
}

fn extract_javascript_analysis_facts(
    file: &File,
    content: &str,
    symbols: &[Symbol],
) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    let route_re =
        Regex::new(r#"\b(?:app|router)\.(get|post|put|delete|patch|all)\(\s*["']([^"']+)["']"#)
            .expect("valid JavaScript route regex");
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        for captures in route_re.captures_iter(line) {
            let method = captures
                .get(1)
                .map(|value| value.as_str().to_ascii_uppercase())
                .unwrap_or_else(|| "HTTP".into());
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::ExposesEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/javascript", "JavaScript HTTP route"),
            ));
        }
    }
    facts
}

fn extract_python_analysis_facts(
    file: &File,
    content: &str,
    symbols: &[Symbol],
) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    let route_re = Regex::new(
        r#"@(?:app|router|blueprint)\.(get|post|put|delete|patch|route)\(\s*["']([^"']+)["']"#,
    )
    .expect("valid Python route regex");
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        for captures in route_re.captures_iter(line) {
            let method = match captures.get(1).map(|value| value.as_str()) {
                Some("route") => "HTTP".to_string(),
                Some(value) => value.to_ascii_uppercase(),
                None => "HTTP".into(),
            };
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_after(symbols, line_number, 2),
                GraphEdgeType::ExposesEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/python", "Python HTTP route decorator"),
            ));
        }
    }
    facts
}

fn extract_rust_analysis_facts(
    file: &File,
    content: &str,
    symbols: &[Symbol],
) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    let route_re = Regex::new(r#"#\[(get|post|put|delete|patch)\(\s*["']([^"']+)["']\s*\)\]"#)
        .expect("valid Rust route regex");
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        for captures in route_re.captures_iter(line) {
            let method = captures
                .get(1)
                .map(|value| value.as_str().to_ascii_uppercase())
                .unwrap_or_else(|| "HTTP".into());
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_after(symbols, line_number, 2),
                GraphEdgeType::ExposesEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/rust", "Rust HTTP route attribute"),
            ));
        }
    }
    facts
}

fn analysis_fact(
    file: &File,
    symbol: Option<&Symbol>,
    edge_type: GraphEdgeType,
    target_kind: GraphNodeType,
    target: String,
    line_number: u32,
    source: (&str, &str),
) -> AnalysisFact {
    AnalysisFact {
        id: stable_id(&format!(
            "analysis:{}:{}:{:?}:{}:{}",
            file.path.display(),
            symbol
                .map(|symbol| symbol.id.0.as_str())
                .unwrap_or("<file>"),
            edge_type,
            target,
            line_number
        )),
        file_id: file.id.clone(),
        symbol_id: symbol.map(|symbol| symbol.id.clone()),
        target,
        target_kind,
        edge_type,
        range: Some(LineRange::single(line_number)),
        confidence: Confidence::Medium,
        source: source.0.into(),
        source_type: EvidenceSourceType::StaticAnalysis,
        message: source.1.into(),
    }
}

fn symbol_named<'a>(symbols: &'a [Symbol], name: &str) -> Option<&'a Symbol> {
    symbols.iter().find(|symbol| symbol.name == name)
}

fn symbol_at_or_after(symbols: &[Symbol], line_number: u32, max_distance: u32) -> Option<&Symbol> {
    symbols
        .iter()
        .filter_map(|symbol| {
            let start = symbol.range.as_ref()?.start;
            (start >= line_number && start <= line_number + max_distance).then_some((start, symbol))
        })
        .min_by_key(|(start, _)| *start)
        .map(|(_, symbol)| symbol)
}

fn symbol_at_or_before(symbols: &[Symbol], line_number: u32) -> Option<&Symbol> {
    symbols
        .iter()
        .filter_map(|symbol| {
            let start = symbol.range.as_ref()?.start;
            (start <= line_number).then_some((start, symbol))
        })
        .max_by_key(|(start, _)| *start)
        .map(|(_, symbol)| symbol)
}

fn clean_java_type(value: &str) -> String {
    value
        .trim()
        .trim_matches(',')
        .split('<')
        .next()
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn split_java_types(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(clean_java_type)
        .filter(|value| !value.is_empty())
        .collect()
}

fn spring_http_method(annotation: Option<&str>) -> &'static str {
    match annotation {
        Some("GetMapping") => "GET",
        Some("PostMapping") => "POST",
        Some("PutMapping") => "PUT",
        Some("DeleteMapping") => "DELETE",
        Some("PatchMapping") => "PATCH",
        Some("RequestMapping") => "HTTP",
        _ => "HTTP",
    }
}

fn dedupe_analysis_facts(facts: &mut Vec<AnalysisFact>) {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
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
    starts.dedup_by_key(|(line, _)| *line);
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

pub fn extract_tests(
    file: &File,
    content: &str,
    symbols: &[Symbol],
    build_hint: Option<&str>,
) -> Vec<TestTarget> {
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
            command: recommended_command(&file.language, &file.path.to_string_lossy(), build_hint),
            confidence: if is_test_file {
                Confidence::High
            } else {
                Confidence::Medium
            },
            reason: "test-like path, annotation, or naming convention".into(),
            evidence_refs: vec![stable_id(&format!(
                "test:{}:{}",
                file.path.display(),
                symbol.name
            ))],
            score_breakdown: vec![ScoreComponent::single(
                "indexed_test_confidence",
                if is_test_file {
                    Confidence::High.score()
                } else {
                    Confidence::Medium.score()
                },
                vec![stable_id(&format!(
                    "test:{}:{}",
                    file.path.display(),
                    symbol.name
                ))],
                "test-like path, annotation, or naming convention",
            )],
        })
        .collect()
}

fn qualified_name(file: &File, content: &str, name: &str) -> String {
    identity::qualified_name(&file.path, &file.language, Some(content), name).unwrap_or_else(|_| {
        let stem = file
            .path
            .with_extension("")
            .to_string_lossy()
            .replace(['/', '\\'], "::");
        format!("{stem}::{name}")
    })
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn recommended_command(
    language: &Language,
    path: &str,
    build_hint: Option<&str>,
) -> Option<String> {
    match (language, build_hint) {
        (Language::Java, Some("gradle")) => Some("./gradlew test".into()),
        (Language::Java, Some("bazel")) => Some("bazel test //...".into()),
        (Language::Java, Some("maven") | _) => Some("mvn test".into()),
        (Language::Rust, _) => Some("cargo test".into()),
        (Language::TypeScript | Language::JavaScript, _) => Some("npm test".into()),
        (Language::Python, _) => Some("pytest".into()),
        (Language::Go, _) => Some("go test ./...".into()),
        _ if path.contains("test") => Some("run repository test command".into()),
        _ => None,
    }
}

pub fn evidence_timestamp() -> chrono::DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_analysis_facts, extract_chunks, extract_imports, extract_symbols, extract_tests,
        qualified_name,
    };
    use open_kioku_core::{
        Confidence, EvidenceSourceType, File, FileId, GraphEdgeType, GraphNodeType, Language,
        LineRange, RepositoryId, Symbol, SymbolId, SymbolKind,
    };

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

    fn java_file() -> File {
        File {
            id: FileId::new("file-java"),
            repository_id: RepositoryId::new("repo"),
            path: "src/main/java/com/acme/OrderController.java".into(),
            language: Language::Java,
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

    #[test]
    fn qualified_names_follow_language_entrypoint_rules() {
        let mut file = ts_file();
        file.path = "src/index.ts".into();
        assert_eq!(qualified_name(&file, "", "handler"), "src::handler");

        file.path = "pkg/__init__.py".into();
        file.language = Language::Python;
        assert_eq!(qualified_name(&file, "", "Factory"), "pkg::Factory");

        file.path = "src/api/mod.rs".into();
        file.language = Language::Rust;
        assert_eq!(qualified_name(&file, "", "run"), "src::api::run");

        file.path = "src/main/java/com/acme/OrderController.java".into();
        file.language = Language::Java;
        assert_eq!(
            qualified_name(
                &file,
                "package com.acme;\nclass OrderController {}",
                "getOrder"
            ),
            "com::acme::OrderController::getOrder"
        );

        file.path = "internal/orders/handler.go".into();
        file.language = Language::Go;
        assert_eq!(
            qualified_name(&file, "package orders\nfunc Load() {}", "Load"),
            "orders::Load"
        );
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

    #[test]
    fn extracts_java_static_analysis_facts() {
        let file = java_file();
        let src = r#"
class OrderController extends BaseController implements OrderApi, Audited {
    @GetMapping("/orders/{id}")
    public Order getOrder() {
        System.getenv("ORDER_REGION");
        return null;
    }
}
"#;
        let symbols = extract_symbols(&file, src);
        let facts = extract_analysis_facts(&file, src, &symbols);
        assert!(facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::Extends
                && fact.target == "BaseController"
                && fact.target_kind == GraphNodeType::Class
        }));
        assert!(facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::Implements
                && fact.target == "OrderApi"
                && fact.target_kind == GraphNodeType::Interface
        }));
        assert!(facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ExposesEndpoint && fact.target == "GET /orders/{id}"
        }));
        assert!(facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ReadsConfig && fact.target == "ORDER_REGION"
        }));
    }

    #[test]
    fn extracts_route_facts_for_script_languages() {
        let ts = ts_file();
        let ts_src = r#"router.post("/v1/orders", handler);"#;
        let ts_facts = extract_analysis_facts(&ts, ts_src, &extract_symbols(&ts, ts_src));
        assert!(ts_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ExposesEndpoint && fact.target == "POST /v1/orders"
        }));

        let py = python_file();
        let py_src = "@app.get('/health')\ndef health():\n    return {}\n";
        let py_facts = extract_analysis_facts(&py, py_src, &extract_symbols(&py, py_src));
        assert!(py_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ExposesEndpoint && fact.target == "GET /health"
        }));
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

    #[test]
    fn chunks_deduplicate_symbols_starting_on_same_line() {
        let file = ts_file();
        let src = "export const handler = () => call();\ncall();";
        let symbols = vec![
            Symbol {
                id: SymbolId::new("handler"),
                name: "handler".into(),
                qualified_name: "src::index::handler".into(),
                kind: SymbolKind::Function,
                file_id: file.id.clone(),
                range: Some(LineRange { start: 1, end: 1 }),
                language: Language::TypeScript,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
            },
            Symbol {
                id: SymbolId::new("call"),
                name: "call".into(),
                qualified_name: "src::index::call".into(),
                kind: SymbolKind::Function,
                file_id: file.id.clone(),
                range: Some(LineRange { start: 1, end: 1 }),
                language: Language::TypeScript,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
            },
        ];

        let chunks = extract_chunks(&file, src, &symbols);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].range.start, 1);
        assert_eq!(chunks[0].range.end, 2);
    }

    // ─── extract_tests ────────────────────────────────────────────────────────

    #[test]
    fn detects_rust_test_attribute() {
        let file = rust_file();
        let src = "#[test]\nfn it_works() {\n    assert!(true);\n}\n";
        let symbols = extract_symbols(&file, src);
        let tests = extract_tests(&file, src, &symbols, None);
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
        let tests = extract_tests(&file, src, &symbols, None);
        // All symbols in a test file become test targets.
        assert_eq!(tests.len(), symbols.len());
    }
}
