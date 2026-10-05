use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, GraphEdgeType,
    GraphNodeType, Import, Language, LineRange, ScoreComponent, Symbol, SymbolId, SymbolKind,
    TestTarget,
};
use open_kioku_tree_sitter::TestRegistrationCall;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use test_discovery::TestFileDiscovery;

mod csharp_tests;
mod test_discovery;

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub syntax: open_kioku_core::SyntaxFacts,
    pub chunks: Vec<CodeChunk>,
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
        let mut syntax = open_kioku_tree_sitter::parse_file(file, content).unwrap_or_default();
        if syntax.symbols.is_empty() {
            syntax.symbols = extract_symbols(file, content);
        } else if file.language == Language::CSharp
            && syntax.symbols.iter().all(|symbol| {
                symbol.kind == SymbolKind::Package && symbol.confidence == Confidence::Medium
            })
        {
            // A C# file read through syntax errors where recovery kept only the namespace: its
            // types were all inside error nodes. Patterns name them at heuristic provenance.
            syntax.symbols.extend(pattern_symbols(file, content));
        }
        dedupe_symbols(&mut syntax.symbols);

        let analysis_facts = extract_analysis_facts(file, content, &syntax.symbols);
        let mut chunks = extract_chunks(file, content, &syntax.symbols);
        dedupe_chunks(&mut chunks);
        let tests = extract_tests(file, content, &syntax.symbols, build_hint);

        ParsedFile {
            syntax,
            chunks,
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
    pattern_symbols(file, content)
}

/// Declarations matched line by line, for a file tree-sitter could not read.
fn pattern_symbols(file: &File, content: &str) -> Vec<Symbol> {
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
        // Reached only when tree-sitter left no declaration whole. Type declarations alone: a
        // C# member line has no keyword a pattern could tell from a statement.
        Language::CSharp => extract_with_patterns(
            file,
            content,
            &[
                (
                    r"^\s*(?:[a-z]+\s+)*(?:class|struct|enum|record(?:\s+class|\s+struct)?)\s+([A-Za-z_][A-Za-z0-9_]*)",
                    SymbolKind::Class,
                    1,
                ),
                (
                    r"^\s*(?:[a-z]+\s+)*interface\s+([A-Za-z_][A-Za-z0-9_]*)",
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
                        module_id: None,
                        parent_symbol_id: None,
                        scope_id: None,
                        signature: None,
                        visibility: open_kioku_core::Visibility::Unknown,
                        alias_of: None,
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
        Language::Yaml | Language::Json | Language::Toml | Language::Text => {
            extract_infra_analysis_facts(file, content)
        }
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
    let http_client_re =
        Regex::new(r#"\b(?:getForObject|postForObject|put|delete|exchange)\(\s*["']([^"']+)["']"#)
            .expect("valid Java HTTP client regex");
    let kafka_listener_re = Regex::new(r#"@KafkaListener\([^)]*topics\s*=\s*["']([^"']+)["']"#)
        .expect("valid Kafka listener regex");
    let kafka_send_re = Regex::new(r#"\bkafkaTemplate\.send\(\s*["']([^"']+)["']"#)
        .expect("valid Kafka send regex");

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
        for captures in http_client_re.captures_iter(line) {
            let Some(route) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("HTTP {}", route.as_str()),
                line_number,
                ("open-kioku-static/java", "Java HTTP client call"),
            ));
        }
        for captures in kafka_listener_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_after(symbols, line_number, 3),
                GraphEdgeType::ConsumesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                ("open-kioku-static/java", "Java Kafka topic listener"),
            ));
        }
        for captures in kafka_send_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::PublishesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                ("open-kioku-static/java", "Java Kafka topic publish"),
            ));
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
    let client_re =
        Regex::new(r#"\b(?:axios|client|http)\.(get|post|put|delete|patch)\(\s*["']([^"']+)["']"#)
            .expect("valid JavaScript HTTP client regex");
    let fetch_re = Regex::new(r#"\bfetch\(\s*["']([^"']+)["']"#).expect("valid fetch regex");
    let publish_re = Regex::new(
        r#"\b(?:producer|publisher|pubsub|channel)\.(?:send|publish|emit)\(\s*(?:\{[^}]*topic\s*:\s*)?["']([^"']+)["']"#,
    )
    .expect("valid JavaScript publish regex");
    let subscribe_re = Regex::new(
        r#"\b(?:consumer|subscriber|pubsub|channel)\.(?:subscribe|on)\(\s*(?:\{[^}]*topic\s*:\s*)?["']([^"']+)["']"#,
    )
    .expect("valid JavaScript subscribe regex");
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
        for captures in client_re.captures_iter(line) {
            let method = captures
                .get(1)
                .map(|value| value.as_str().to_ascii_uppercase())
                .unwrap_or_else(|| "HTTP".into());
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                (
                    "open-kioku-static/javascript",
                    "JavaScript HTTP client call",
                ),
            ));
        }
        for captures in fetch_re.captures_iter(line) {
            let route = captures.get(1).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("HTTP {route}"),
                line_number,
                ("open-kioku-static/javascript", "JavaScript fetch call"),
            ));
        }
        for captures in publish_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::PublishesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                ("open-kioku-static/javascript", "JavaScript topic publish"),
            ));
        }
        for captures in subscribe_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::ConsumesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                (
                    "open-kioku-static/javascript",
                    "JavaScript topic subscription",
                ),
            ));
        }
    }
    dedupe_analysis_facts(&mut facts);
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
    let client_re =
        Regex::new(r#"\b(?:requests|httpx)\.(get|post|put|delete|patch)\(\s*["']([^"']+)["']"#)
            .expect("valid Python HTTP client regex");
    let publish_re = Regex::new(r#"\b(?:producer|publisher|client)\.send\(\s*["']([^"']+)["']"#)
        .expect("valid Python publish regex");
    let subscribe_re =
        Regex::new(r#"\b(?:consumer|subscriber)\.subscribe\(\s*(?:\[)?\s*["']([^"']+)["']"#)
            .expect("valid Python subscribe regex");
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
        for captures in client_re.captures_iter(line) {
            let method = captures
                .get(1)
                .map(|value| value.as_str().to_ascii_uppercase())
                .unwrap_or_else(|| "HTTP".into());
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/python", "Python HTTP client call"),
            ));
        }
        for captures in publish_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::PublishesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                ("open-kioku-static/python", "Python topic publish"),
            ));
        }
        for captures in subscribe_re.captures_iter(line) {
            let Some(topic) = captures.get(1) else {
                continue;
            };
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::ConsumesEvent,
                GraphNodeType::Topic,
                topic.as_str().to_string(),
                line_number,
                ("open-kioku-static/python", "Python topic subscription"),
            ));
        }
    }
    dedupe_analysis_facts(&mut facts);
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
    let client_re = Regex::new(r#"\breqwest::(get|post|put|delete|patch)\(\s*["']([^"']+)["']"#)
        .expect("valid Rust HTTP client regex");
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
        for captures in client_re.captures_iter(line) {
            let method = captures
                .get(1)
                .map(|value| value.as_str().to_ascii_uppercase())
                .unwrap_or_else(|| "HTTP".into());
            let route = captures.get(2).map(|value| value.as_str()).unwrap_or("/");
            facts.push(analysis_fact(
                file,
                symbol_at_or_before(symbols, line_number),
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("{method} {route}"),
                line_number,
                ("open-kioku-static/rust", "Rust HTTP client call"),
            ));
        }
    }
    dedupe_analysis_facts(&mut facts);
    facts
}

fn extract_infra_analysis_facts(file: &File, content: &str) -> Vec<AnalysisFact> {
    let path = file.path.to_string_lossy().to_ascii_lowercase();
    let mut facts = Vec::new();
    if path.ends_with("dockerfile") || path.contains("dockerfile.") {
        extract_dockerfile_facts(file, content, &mut facts);
    }
    if path.ends_with("docker-compose.yml")
        || path.ends_with("docker-compose.yaml")
        || path.ends_with("compose.yml")
        || path.ends_with("compose.yaml")
    {
        extract_compose_facts(file, content, &mut facts);
    }
    if matches!(file.language, Language::Yaml) {
        extract_kubernetes_facts(file, content, &mut facts);
    }
    if path.ends_with(".tf") || path.ends_with(".tfvars") || path.ends_with(".hcl") {
        extract_terraform_facts(file, content, &mut facts);
    }
    extract_url_binding_facts(file, content, &mut facts);
    dedupe_analysis_facts(&mut facts);
    facts
}

fn extract_dockerfile_facts(file: &File, content: &str, facts: &mut Vec<AnalysisFact>) {
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("EXPOSE ") {
            for port in rest.split_whitespace() {
                let port = port.split('/').next().unwrap_or(port);
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::ExposesEndpoint,
                    GraphNodeType::Endpoint,
                    format!("TCP :{port}"),
                    line_number,
                    ("open-kioku-static/dockerfile", "Dockerfile exposed port"),
                ));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("ENV ") {
            if let Some((key, _)) = rest.split_once('=') {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::WritesConfig,
                    GraphNodeType::ConfigKey,
                    key.trim().to_string(),
                    line_number,
                    (
                        "open-kioku-static/dockerfile",
                        "Dockerfile environment binding",
                    ),
                ));
            }
        }
    }
}

fn extract_compose_facts(file: &File, content: &str, facts: &mut Vec<AnalysisFact>) {
    let service_re =
        Regex::new(r#"^\s{2}([A-Za-z0-9_.-]+):\s*$"#).expect("valid compose service regex");
    let port_re = Regex::new(r#"^\s*-\s*["']?(?:\d+:)?(\d+)(?:/tcp|/udp)?["']?\s*$"#)
        .expect("valid compose port regex");
    let env_re = Regex::new(r#"^\s*([A-Z_][A-Z0-9_]+):"#).expect("valid compose env regex");
    let dep_re = Regex::new(r#"^\s*-\s*([A-Za-z0-9_.-]+)\s*$"#).expect("valid compose dep regex");
    let mut in_services = false;
    let mut in_environment = false;
    let mut in_depends = false;
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        if line.trim() == "services:" {
            in_services = true;
            continue;
        }
        if !in_services {
            continue;
        }
        if let Some(captures) = service_re.captures(line) {
            if let Some(service) = captures.get(1).map(|m| m.as_str()) {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::Defines,
                    GraphNodeType::Resource,
                    format!("compose:service:{service}"),
                    line_number,
                    ("open-kioku-static/compose", "Docker Compose service"),
                ));
            }
            in_environment = false;
            in_depends = false;
            continue;
        }
        let trimmed = line.trim();
        in_environment =
            trimmed == "environment:" || (in_environment && line.starts_with("      "));
        in_depends = trimmed == "depends_on:" || (in_depends && line.starts_with("      "));
        if let Some(captures) = port_re.captures(line) {
            if let Some(port) = captures.get(1).map(|m| m.as_str()) {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::ExposesEndpoint,
                    GraphNodeType::Endpoint,
                    format!("TCP :{port}"),
                    line_number,
                    ("open-kioku-static/compose", "Docker Compose published port"),
                ));
            }
        }
        if in_environment {
            if let Some(key) = env_re
                .captures(line)
                .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
            {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::WritesConfig,
                    GraphNodeType::ConfigKey,
                    key,
                    line_number,
                    (
                        "open-kioku-static/compose",
                        "Docker Compose environment binding",
                    ),
                ));
            }
        }
        if in_depends {
            if let Some(dependency) = dep_re
                .captures(line)
                .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
            {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::DependsOn,
                    GraphNodeType::Resource,
                    format!("compose:service:{dependency}"),
                    line_number,
                    (
                        "open-kioku-static/compose",
                        "Docker Compose service dependency",
                    ),
                ));
            }
        }
    }
}

fn extract_kubernetes_facts(file: &File, content: &str, facts: &mut Vec<AnalysisFact>) {
    let mut kind: Option<String> = None;
    let mut name: Option<String> = None;
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("kind:") {
            kind = Some(value.trim().to_string());
        } else if name.is_none() && trimmed.starts_with("name:") {
            name = Some(trimmed.trim_start_matches("name:").trim().to_string());
        } else if let Some(value) = trimmed
            .strip_prefix("port:")
            .or_else(|| trimmed.strip_prefix("- port:"))
        {
            let port = value.trim();
            if port.chars().all(|ch| ch.is_ascii_digit()) {
                facts.push(analysis_fact(
                    file,
                    None,
                    GraphEdgeType::ExposesEndpoint,
                    GraphNodeType::Endpoint,
                    format!("TCP :{port}"),
                    line_number,
                    ("open-kioku-static/kubernetes", "Kubernetes service port"),
                ));
            }
        }
        if let (Some(resource_kind), Some(resource_name)) = (&kind, &name) {
            facts.push(analysis_fact(
                file,
                None,
                GraphEdgeType::Defines,
                GraphNodeType::Resource,
                format!("kubernetes:{resource_kind}:{resource_name}"),
                line_number,
                ("open-kioku-static/kubernetes", "Kubernetes resource"),
            ));
            kind = None;
            name = None;
        }
    }
}

fn extract_terraform_facts(file: &File, content: &str, facts: &mut Vec<AnalysisFact>) {
    let resource_re =
        Regex::new(r#"resource\s+"([^"]+)"\s+"([^"]+)""#).expect("valid Terraform resource regex");
    let variable_re =
        Regex::new(r#"variable\s+"([^"]+)""#).expect("valid Terraform variable regex");
    for (idx, line) in content.lines().enumerate() {
        let line_number = (idx + 1) as u32;
        if let Some((kind, name)) = resource_re
            .captures(line)
            .and_then(|captures| Some((captures.get(1)?.as_str(), captures.get(2)?.as_str())))
        {
            let (target_kind, edge_type, target) = if kind.contains("sqs") || kind.contains("queue")
            {
                (
                    GraphNodeType::Queue,
                    GraphEdgeType::Defines,
                    name.to_string(),
                )
            } else if kind.contains("sns") || kind.contains("topic") {
                (
                    GraphNodeType::Topic,
                    GraphEdgeType::Defines,
                    name.to_string(),
                )
            } else {
                (
                    GraphNodeType::Resource,
                    GraphEdgeType::Defines,
                    format!("terraform:{kind}:{name}"),
                )
            };
            facts.push(analysis_fact(
                file,
                None,
                edge_type,
                target_kind,
                target,
                line_number,
                ("open-kioku-static/terraform", "Terraform resource"),
            ));
        }
        if let Some(variable) = variable_re
            .captures(line)
            .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
        {
            facts.push(analysis_fact(
                file,
                None,
                GraphEdgeType::ReadsConfig,
                GraphNodeType::ConfigKey,
                variable,
                line_number,
                ("open-kioku-static/terraform", "Terraform variable"),
            ));
        }
    }
}

fn extract_url_binding_facts(file: &File, content: &str, facts: &mut Vec<AnalysisFact>) {
    let url_re =
        Regex::new(r#"["']?(?:url|endpoint|base_url)["']?\s*[:=]\s*["'](https?://[^"']+)["']"#)
            .expect("valid URL binding regex");
    for (idx, line) in content.lines().enumerate() {
        for url in url_re
            .captures_iter(line)
            .filter_map(|captures| captures.get(1).map(|m| m.as_str().to_string()))
        {
            facts.push(analysis_fact(
                file,
                None,
                GraphEdgeType::CallsEndpoint,
                GraphNodeType::Endpoint,
                format!("HTTP {url}"),
                (idx + 1) as u32,
                ("open-kioku-static/config", "configuration URL binding"),
            ));
        }
    }
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
        target_symbol_id: None,
        ambiguity: Vec::new(),
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
            symbol.range.as_ref().map(|range| {
                (
                    chunk_start(file, &lines, range.start as usize),
                    symbol.id.clone(),
                )
            })
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

/// The line a symbol's chunk starts on: the symbol's first line or, in C#, the first line of the
/// `///` documentation comment right above it. C# attributes are inside the declaration, as Java
/// annotations are, but its documentation precedes it; without this, a symbol's documentation
/// would be searched as the tail of the chunk before it.
fn chunk_start(file: &File, lines: &[&str], start: usize) -> usize {
    if file.language != Language::CSharp {
        return start;
    }
    let mut first = start;
    while first > 1
        && lines
            .get(first - 2)
            .is_some_and(|line| line.trim_start().starts_with("///"))
    {
        first -= 1;
    }
    first
}

pub fn extract_tests(
    file: &File,
    content: &str,
    symbols: &[Symbol],
    build_hint: Option<&str>,
) -> Vec<TestTarget> {
    let is_test_file = open_kioku_core::is_test_code_path(&file.path.to_string_lossy());
    let lines = content.lines().collect::<Vec<_>>();
    let csharp = (file.language == Language::CSharp)
        .then(|| csharp_tests::CSharpTests::new(&lines, symbols));
    let discovery =
        is_test_file.then(|| TestFileDiscovery::new(file, &lines, symbols, csharp.as_ref()));
    let mut targets = symbols
        .iter()
        .filter(|symbol| {
            // Variables, constants, classes and modules are never targets, in a test file or not.
            // Outside a test path, a C# test is one by its attribute alone: a test project need
            // not follow a naming convention, and ingest confirms it from the project file.
            is_test_symbol_kind(&symbol.kind)
                && (is_test_file
                    || has_test_name_prefix(&symbol.name)
                    || has_adjacent_test_annotation(&lines, symbol)
                    || csharp.as_ref().is_some_and(|csharp| csharp.runs(symbol)))
        })
        .map(|symbol| {
            // Every callable of a test file is kept as test code, but only those its runner
            // discovers are tests; a helper or lifecycle hook beside them validates nothing. One
            // matched outside a test file is a test by annotation or name, and the surfaces that
            // filter say so differently.
            let origin = match &discovery {
                Some(discovery) if discovery.runs(symbol) => {
                    open_kioku_core::TestTargetOrigin::TestFileSymbol
                }
                Some(_) => open_kioku_core::TestTargetOrigin::TestFileHelper,
                None => open_kioku_core::TestTargetOrigin::Symbol,
            };
            match &csharp {
                Some(csharp) => csharp_target(file, csharp, symbol, origin),
                None => symbol_target(file, symbol, origin, build_hint),
            }
        })
        .collect::<Vec<_>>();
    // Most JavaScript and TypeScript tests are calls, not declarations, so they have no symbol.
    if is_test_file && matches!(file.language, Language::TypeScript | Language::JavaScript) {
        targets.extend(
            test_registration_calls(file, content)
                .into_iter()
                .map(|call| registration_target(file, call, build_hint)),
        );
    }
    targets
}

const SYMBOL_TEST_REASON: &str = "test-like path, annotation, or naming convention";
const HELPER_REASON: &str =
    "callable in a test-path file matching no default runner discovery rule";

fn symbol_target(
    file: &File,
    symbol: &Symbol,
    origin: open_kioku_core::TestTargetOrigin,
    build_hint: Option<&str>,
) -> TestTarget {
    let id = stable_id(&format!("test:{}:{}", file.path.display(), symbol.name));
    let command = recommended_command(&file.language, &file.path.to_string_lossy(), build_hint);
    test_target(file, symbol, origin, id, command)
}

/// A C# target is identified by its qualified name, since one test file often declares a method
/// of the same name in each of several nested classes (`WhenPosting.Rejects`,
/// `WhenVoiding.Rejects`). A test's command filters `dotnet test` to it; a helper's runs the
/// project, since it selects no test of its own. Ingest scopes both to the test project.
fn csharp_target(
    file: &File,
    csharp: &csharp_tests::CSharpTests<'_>,
    symbol: &Symbol,
    origin: open_kioku_core::TestTargetOrigin,
) -> TestTarget {
    let id = stable_id(&format!(
        "test:{}:{}",
        file.path.display(),
        symbol.qualified_name
    ));
    let command = if origin == open_kioku_core::TestTargetOrigin::TestFileHelper {
        "dotnet test".to_string()
    } else {
        csharp.command(symbol)
    };
    let mut target = test_target(file, symbol, origin, id, Some(command));
    // A method of a file read through syntax errors is what recovery left whole: still a test
    // its runner discovers, but no stronger than the medium-confidence symbol it rests on.
    if target.counts_as_validation_evidence() && symbol.confidence != Confidence::High {
        target.confidence = Confidence::Medium;
        target.reason = RECOVERED_CSHARP_TEST_REASON.into();
        target.score_breakdown = vec![ScoreComponent::single(
            "indexed_test_confidence",
            Confidence::Medium.score(),
            target.evidence_refs.clone(),
            RECOVERED_CSHARP_TEST_REASON,
        )];
    }
    target
}

const RECOVERED_CSHARP_TEST_REASON: &str =
    "test attribute on a method of a C# file read through syntax errors";

fn test_target(
    file: &File,
    symbol: &Symbol,
    origin: open_kioku_core::TestTargetOrigin,
    id: String,
    command: Option<String>,
) -> TestTarget {
    // A helper is test code, not a test, so it is the weakest target there is.
    let (confidence, reason) = match origin {
        open_kioku_core::TestTargetOrigin::TestFileSymbol => (Confidence::High, SYMBOL_TEST_REASON),
        open_kioku_core::TestTargetOrigin::TestFileHelper => (Confidence::Low, HELPER_REASON),
        _ => (Confidence::Medium, SYMBOL_TEST_REASON),
    };
    TestTarget {
        selection_tier: open_kioku_core::TestSelectionTier::default(),
        tier_justification: Vec::new(),
        id: id.clone(),
        name: symbol.name.clone(),
        file_id: file.id.clone(),
        range: symbol.range.clone(),
        command,
        confidence,
        reason: reason.into(),
        evidence_refs: vec![id.clone()],
        score_breakdown: vec![ScoreComponent::single(
            "indexed_test_confidence",
            confidence.score(),
            vec![id],
            reason,
        )],
        origin,
    }
}

const REGISTRATION_REASON: &str = "test registration call in a test-path file";
const DISABLED_REGISTRATION_REASON: &str = "disabled test registration call in a test-path file";

/// The tree-sitter reading of the file's registration calls, or the single-line pattern reading
/// when the file does not parse cleanly, the fallback symbol extraction takes as well.
fn test_registration_calls(file: &File, content: &str) -> Vec<TestRegistrationCall> {
    open_kioku_tree_sitter::test_registration_calls(file, content)
        .unwrap_or_else(|_| test_registration_calls_by_pattern(content))
}

/// `test("name", ...)`, `it.skip('name', ...)` or `Suite.test(`name`, () => ...)` opening on one
/// line. A namespaced callee needs a callback on that line, so `pattern.test("abc")` is not a test.
fn test_registration_calls_by_pattern(content: &str) -> Vec<TestRegistrationCall> {
    let Ok(pattern) = Regex::new(
        r#"^\s*(?:(?P<namespace>[A-Za-z_$][A-Za-z0-9_$]*)\.)?(?:test|it)(?P<modifiers>(?:\.(?:only|skip|todo|concurrent|failing|fails|sequential))*)\s*\(\s*(?:"(?P<double>[^"]*)"|'(?P<single>[^']*)'|`(?P<template>[^`]*)`)(?P<rest>.*)$"#,
    ) else {
        return Vec::new();
    };
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let captures = pattern.captures(line)?;
            let rest = captures.name("rest").map_or("", |rest| rest.as_str());
            if captures.name("namespace").is_some()
                && !(rest.contains("=>") || rest.contains("function"))
            {
                return None;
            }
            let (raw, interpolated) = match (
                captures.name("double"),
                captures.name("single"),
                captures.name("template"),
            ) {
                (Some(text), _, _) | (_, Some(text), _) => (text.as_str(), false),
                (_, _, Some(text)) => (text.as_str(), text.as_str().contains("${")),
                _ => return None,
            };
            let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            let line_number = u32::try_from(index + 1).ok()?;
            let modifiers = captures
                .name("modifiers")
                .map_or("", |value| value.as_str());
            let disabled = ["skip", "todo", "failing", "fails"]
                .iter()
                .any(|modifier| modifiers.contains(modifier));
            (!name.is_empty()).then_some(TestRegistrationCall {
                name,
                range: LineRange {
                    start: line_number,
                    end: line_number,
                },
                interpolated,
                disabled,
            })
        })
        .collect()
}

/// A registered test's target is named by its literal text. The id carries the line, because two
/// suites in one file often register tests with the same name; ids therefore churn when the calls
/// move, and a saved plan's `test:` evidence refs stop resolving after such a re-index.
fn registration_target(
    file: &File,
    call: TestRegistrationCall,
    build_hint: Option<&str>,
) -> TestTarget {
    let id = stable_id(&format!(
        "test:{}:{}:{}",
        file.path.display(),
        call.name,
        call.range.start
    ));
    // A disabled test is written but never run, so it is the weakest evidence there is. A
    // `${...}` substitution makes the registered name known only at runtime.
    let confidence = if call.disabled {
        Confidence::Low
    } else if call.interpolated {
        Confidence::Medium
    } else {
        Confidence::High
    };
    let reason = if call.disabled {
        DISABLED_REGISTRATION_REASON
    } else {
        REGISTRATION_REASON
    };
    let origin = if call.disabled {
        open_kioku_core::TestTargetOrigin::DisabledRegistrationCall
    } else {
        open_kioku_core::TestTargetOrigin::RegistrationCall
    };
    TestTarget {
        selection_tier: open_kioku_core::TestSelectionTier::default(),
        tier_justification: Vec::new(),
        id: id.clone(),
        name: call.name,
        file_id: file.id.clone(),
        range: Some(call.range),
        command: recommended_command(&file.language, &file.path.to_string_lossy(), build_hint),
        confidence,
        reason: reason.into(),
        evidence_refs: vec![id.clone()],
        score_breakdown: vec![ScoreComponent::single(
            "indexed_test_confidence",
            confidence.score(),
            vec![id],
            reason,
        )],
        origin,
    }
}

/// `test`, `test_rounds`, `testRounds` or `test2`, but not `testable` or `testimony`: the prefix
/// has to end at a word boundary.
fn has_test_name_prefix(name: &str) -> bool {
    name.strip_prefix("test").is_some_and(|rest| {
        matches!(
            rest.chars().next(),
            None | Some('_' | 'A'..='Z' | '0'..='9')
        )
    })
}

/// Upper bound on the attribute, annotation and doc-comment lines walked above a symbol, so a
/// pathological stack cannot turn one symbol into a scan of the file.
const TEST_ANNOTATION_STACK_LIMIT: usize = 64;

/// Attribute and annotation prefixes, after indentation, that mark the next callable as a test.
/// Matched as prefixes, not substrings.
const STACKED_TEST_ANNOTATIONS: &[&str] = &[
    "#[test]",
    "#[tokio::test",
    "#[async_std::test",
    "#[rstest",
    "#[test_case",
    "@Test",
    "@ParameterizedTest",
    "@RepeatedTest",
];

/// Declaration prefixes that make a symbol's own first line a test: a JS/TS `it(` or `test(`
/// call, or a Python `def test_`. Matched on that line only. Above a symbol they are the
/// previous test's code, not an annotation of this symbol, and `it(` inside `commit(` is never
/// one because matching is by prefix.
const DECLARATION_TEST_PREFIXES: &[&str] = &["it(", "test(", "def test_", "async def test_"];

fn is_test_symbol_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
    )
}

fn is_stacked_test_annotation(line: &str) -> bool {
    STACKED_TEST_ANNOTATIONS
        .iter()
        .any(|annotation| line.starts_with(annotation))
}

/// An attribute, annotation, decorator or comment line: the only single lines the walk above a
/// symbol passes over.
fn is_annotation_stack_line(line: &str) -> bool {
    ["#", "@", "//", "/*", "*"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn bracket_balance(line: &str) -> i64 {
    line.chars()
        .map(|character| match character {
            '(' | '[' | '{' => 1,
            ')' | ']' | '}' => -1,
            _ => 0,
        })
        .sum()
}

/// The first line of a multi-line attribute or annotation that ends at `close`: the nearest
/// `#[` or `@` line above whose brackets close exactly at `close`, with only argument lines in
/// between. `None` when `close` is code, such as the last line of the previous test's body.
fn multiline_attribute_opener(lines: &[&str], close: usize) -> Option<usize> {
    let closing = lines.get(close)?.trim();
    if !(closing.ends_with(')') || closing.ends_with(']')) {
        return None;
    }
    let mut balance = bracket_balance(closing);
    for opener in (0..close).rev().take(TEST_ANNOTATION_STACK_LIMIT) {
        let line = lines[opener].trim();
        balance += bracket_balance(line);
        if line.starts_with("#[") || line.starts_with('@') {
            return (balance == 0 && bracket_balance(line) > 0).then_some(opener);
        }
        let ends_code = line.is_empty()
            || line.ends_with(';')
            || line.ends_with('{')
            || line.ends_with('}')
            || line.ends_with(':');
        if ends_code {
            return None;
        }
    }
    None
}

/// Whether the symbol's first line declares a test, or a test attribute or annotation sits in
/// the contiguous stack of attribute, annotation, decorator and comment lines directly above
/// it, including the argument lines of a multi-line `#[should_panic(..)]` or
/// `@ValueSource({..})`. The walk never passes a line of code, so a helper declared right after
/// a test body is not a test. Scanned per symbol, not per file: a `#[cfg(test)] mod tests`
/// elsewhere in the file says nothing about the constant or struct three hundred lines earlier.
fn has_adjacent_test_annotation(lines: &[&str], symbol: &Symbol) -> bool {
    declares_registered_test(lines, symbol)
        || has_adjacent_annotation(lines, symbol, is_stacked_test_annotation)
}

/// Whether the symbol's own first line is a JS/TS `it(` or `test(` call or a Python `def test_`.
fn declares_registered_test(lines: &[&str], symbol: &Symbol) -> bool {
    symbol_first_line(lines, symbol).is_some_and(|first| {
        DECLARATION_TEST_PREFIXES
            .iter()
            .any(|prefix| first.starts_with(prefix))
    })
}

fn symbol_first_line<'a>(lines: &[&'a str], symbol: &Symbol) -> Option<&'a str> {
    let start = (symbol.range.as_ref()?.start as usize).checked_sub(1)?;
    lines.get(start).map(|line| line.trim_start())
}

/// Whether `matches` accepts the symbol's first line or an annotation opener in the contiguous
/// attribute, annotation, decorator and comment stack directly above it. See
/// [`has_adjacent_test_annotation`] for how the walk stops.
fn has_adjacent_annotation(
    lines: &[&str],
    symbol: &Symbol,
    matches: impl Fn(&str) -> bool,
) -> bool {
    let Some(range) = &symbol.range else {
        return false;
    };
    let start = (range.start as usize).saturating_sub(1);
    let Some(first) = lines.get(start) else {
        return false;
    };
    if matches(first.trim_start()) {
        return true;
    }
    let mut index = start;
    let mut walked = 0;
    while index > 0 && walked < TEST_ANNOTATION_STACK_LIMIT {
        let above = index - 1;
        let line = lines[above].trim();
        if is_annotation_stack_line(line) {
            if matches(line) {
                return true;
            }
            index = above;
            walked += 1;
            continue;
        }
        let Some(opener) = multiline_attribute_opener(lines, above) else {
            return false;
        };
        if matches(lines[opener].trim()) {
            return true;
        }
        walked += index - opener;
        index = opener;
    }
    false
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
    use std::collections::BTreeMap;

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

    #[test]
    fn extracts_service_boundary_facts_for_clients_channels_and_infra() {
        let ts = ts_file();
        let ts_src = r#"
router.post("/v1/orders", handler);
await fetch("https://billing.example.com/v1/orders");
producer.send({ topic: "orders.created" });
consumer.subscribe({ topic: "orders.created" });
"#;
        let ts_facts = extract_analysis_facts(&ts, ts_src, &extract_symbols(&ts, ts_src));
        assert!(ts_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::CallsEndpoint
                && fact.target == "HTTP https://billing.example.com/v1/orders"
        }));
        assert!(ts_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::PublishesEvent
                && fact.target_kind == GraphNodeType::Topic
                && fact.target == "orders.created"
        }));
        assert!(ts_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ConsumesEvent
                && fact.target_kind == GraphNodeType::Topic
                && fact.target == "orders.created"
        }));

        let docker = File {
            language: Language::Text,
            path: "Dockerfile".into(),
            ..rust_file()
        };
        let docker_facts =
            extract_analysis_facts(&docker, "ENV SERVICE_PORT=8080\nEXPOSE 8080\n", &[]);
        assert!(docker_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ExposesEndpoint && fact.target == "TCP :8080"
        }));
        assert!(docker_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::WritesConfig && fact.target == "SERVICE_PORT"
        }));

        let compose = File {
            language: Language::Yaml,
            path: "docker-compose.yml".into(),
            ..rust_file()
        };
        let compose_src = r#"
services:
  api:
    ports:
      - "8080:8080"
    environment:
      DATABASE_URL: postgres://db/app
    depends_on:
      - db
  db:
    image: postgres
"#;
        let compose_facts = extract_analysis_facts(&compose, compose_src, &[]);
        assert!(compose_facts.iter().any(|fact| {
            fact.target_kind == GraphNodeType::Resource && fact.target == "compose:service:api"
        }));
        assert!(compose_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::DependsOn && fact.target == "compose:service:db"
        }));
        assert!(compose_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::WritesConfig && fact.target == "DATABASE_URL"
        }));

        let k8s = File {
            language: Language::Yaml,
            path: "k8s/service.yaml".into(),
            ..rust_file()
        };
        let k8s_src =
            "kind: Service\nmetadata:\n  name: orders-api\nspec:\n  ports:\n    - port: 80\n";
        let k8s_facts = extract_analysis_facts(&k8s, k8s_src, &[]);
        assert!(k8s_facts.iter().any(|fact| {
            fact.target_kind == GraphNodeType::Resource
                && fact.target == "kubernetes:Service:orders-api"
        }));
        assert!(k8s_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::ExposesEndpoint && fact.target == "TCP :80"
        }));

        let terraform = File {
            language: Language::Text,
            path: "infra/main.tf".into(),
            ..rust_file()
        };
        let terraform_src = r#"
resource "aws_sns_topic" "orders_created" {}
variable "DATABASE_URL" {}
endpoint = "https://orders.example.com/v1/orders"
"#;
        let terraform_facts = extract_analysis_facts(&terraform, terraform_src, &[]);
        assert!(terraform_facts.iter().any(|fact| {
            fact.target_kind == GraphNodeType::Topic && fact.target == "orders_created"
        }));
        assert!(terraform_facts.iter().any(|fact| {
            fact.target_kind == GraphNodeType::ConfigKey && fact.target == "DATABASE_URL"
        }));
        assert!(terraform_facts.iter().any(|fact| {
            fact.edge_type == GraphEdgeType::CallsEndpoint
                && fact.target == "HTTP https://orders.example.com/v1/orders"
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

    fn csharp_file() -> File {
        File {
            id: FileId::new("file-cs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/Ledger/Entry.cs".into(),
            language: Language::CSharp,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    #[test]
    fn a_csharp_symbol_chunk_carries_its_documentation_comment() {
        let file = csharp_file();
        let src = "namespace Acme.Ledger;\n\n/// <summary>Settles a reconciled ledger.</summary>\n/// <remarks>Idempotent.</remarks>\n[Serializable]\npublic class Entry\n{\n    /// Books the entry.\n    public void Post() { }\n}\n";
        let symbols = extract_symbols(&file, src);
        let chunks = extract_chunks(&file, src, &symbols);
        let chunk_of = |name: &str| {
            let symbol = symbols.iter().find(|symbol| symbol.name == name).unwrap();
            chunks
                .iter()
                .find(|chunk| chunk.symbol_id.as_ref() == Some(&symbol.id))
                .unwrap()
        };
        let entry = chunk_of("Entry");
        assert_eq!((entry.range.start, entry.range.end), (3, 7));
        assert!(entry
            .text
            .starts_with("/// <summary>Settles a reconciled ledger."));
        let post = chunk_of("Post");
        assert_eq!((post.range.start, post.range.end), (8, 10));
        assert!(post.text.contains("Books the entry."));
        // The namespace chunk ends where the documentation starts.
        let namespace = chunk_of("Acme.Ledger");
        assert!(!namespace.text.contains("Settles"));
        // Symbol ranges stay the declaration's own.
        let symbol = symbols
            .iter()
            .find(|symbol| symbol.name == "Entry")
            .unwrap();
        assert_eq!(symbol.range.as_ref().map(|range| range.start), Some(5));
    }

    #[test]
    fn rust_chunks_still_start_at_the_symbol() {
        let file = rust_file();
        let src = "/// Adds.\npub fn alpha() {}\n/// Subtracts.\npub fn beta() {}\n";
        let symbols = extract_symbols(&file, src);
        let chunks = extract_chunks(&file, src, &symbols);
        let starts = chunks
            .iter()
            .map(|chunk| chunk.range.start)
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![2, 4]);
    }

    #[test]
    fn csharp_types_recovery_could_not_keep_are_named_by_pattern() {
        use crate::{HeuristicParser, Parser};
        let file = csharp_file();
        // The unbalanced brace leaves the class inside an error node, keeping only the namespace.
        let src = "namespace Acme;\npublic sealed class Broken\n{\n    public void Lost() { if (x { }\n    interface IHidden { }\n";
        let parsed = HeuristicParser.parse_with_hint(&file, src, None);
        let found = parsed
            .syntax
            .symbols
            .iter()
            .map(|symbol| {
                (
                    symbol.qualified_name.as_str(),
                    symbol.kind.clone(),
                    symbol.provenance.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            vec![
                ("Acme", SymbolKind::Package, EvidenceSourceType::TreeSitter),
                (
                    "Acme::Broken",
                    SymbolKind::Class,
                    EvidenceSourceType::Heuristic
                ),
                (
                    "Acme::IHidden",
                    SymbolKind::Interface,
                    EvidenceSourceType::Heuristic
                ),
            ]
        );
        // A file read whole keeps tree-sitter's symbols alone.
        let whole = HeuristicParser.parse_with_hint(&file, "namespace Acme;\n", None);
        assert_eq!(whole.syntax.symbols.len(), 1);
    }

    /// Each C# test target of `src` at `path` as `(name, origin, command)`, in source order.
    fn csharp_targets(
        path: &str,
        src: &str,
    ) -> Vec<(String, open_kioku_core::TestTargetOrigin, String)> {
        let file = file_at(path, Language::CSharp);
        let symbols = extract_symbols(&file, src);
        let mut tests = extract_tests(&file, src, &symbols, None);
        tests.sort_by_key(|test| test.range.as_ref().map(|range| range.start));
        tests
            .into_iter()
            .map(|test| (test.name, test.origin, test.command.unwrap_or_default()))
            .collect()
    }

    fn csharp_filter(name: &str) -> String {
        format!("dotnet test --filter \"FullyQualifiedName~{name}\"")
    }

    /// xUnit runs `[Fact]` and `[Theory]` methods, however the attribute is spelled; the class
    /// constructor, `IAsyncLifetime` and `IDisposable` methods are its setup and teardown, and a
    /// test of an abstract class runs under each class deriving from it.
    #[test]
    fn xunit_runs_fact_and_theory_methods_not_lifecycle_or_helpers() {
        use open_kioku_core::TestTargetOrigin::{TestFileHelper, TestFileSymbol};
        let src = r#"using Check = Xunit.FactAttribute;

namespace Acme.Ledger.Tests;

public class EntryTests : IAsyncLifetime, IDisposable
{
    public EntryTests() { }

    public Task InitializeAsync() => Task.CompletedTask;

    public Task DisposeAsync() => Task.CompletedTask;

    public void Dispose() { }

    [Fact]
    public void Posts() { }

    [Theory]
    [InlineData(1)]
    [MemberData(nameof(Amounts))]
    public void Rounds(int value) { }

    [FactAttribute] public void Suffixed() { }

    [Xunit.Fact(DisplayName = "posts [twice]")]
    public void Qualified() { }

    [Check]
    public void Aliased() { }

    [SkippableFact, Trait("kind", "slow")]
    public async Task Derived() { }

    private static Entry MakeEntry() => new Entry();

    public class WhenVoided
    {
        [Fact]
        public void Rejects() { }
    }
}

public abstract class LedgerContract
{
    [Fact]
    public void Balances() { }
}
"#;
        assert_eq!(
            csharp_targets("tests/Acme.Ledger.Tests/EntryTests.cs", src),
            vec![
                ("EntryTests".into(), TestFileHelper, "dotnet test".into()),
                (
                    "InitializeAsync".into(),
                    TestFileHelper,
                    "dotnet test".into()
                ),
                ("DisposeAsync".into(), TestFileHelper, "dotnet test".into()),
                ("Dispose".into(), TestFileHelper, "dotnet test".into()),
                (
                    "Posts".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Posts")
                ),
                (
                    "Rounds".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Rounds")
                ),
                (
                    "Suffixed".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Suffixed")
                ),
                (
                    "Qualified".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Qualified")
                ),
                (
                    "Aliased".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Aliased")
                ),
                (
                    "Derived".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests.Derived")
                ),
                ("MakeEntry".into(), TestFileHelper, "dotnet test".into()),
                (
                    "Rejects".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.EntryTests+WhenVoided.Rejects")
                ),
                (
                    "Balances".into(),
                    TestFileSymbol,
                    csharp_filter(".Balances")
                ),
            ]
        );
    }

    /// NUnit runs `[Test]`, `[TestCase]`, `[TestCaseSource]` and `[Theory]` methods; its setup
    /// and teardown attributes mark lifecycle methods, which are helpers.
    #[test]
    fn nunit_runs_test_and_case_methods_not_setup_or_teardown() {
        use open_kioku_core::TestTargetOrigin::{TestFileHelper, TestFileSymbol};
        let src = r#"using NUnit.Framework;

namespace Acme.Ledger.Tests
{
    [TestFixture]
    public class StatementTests
    {
        [OneTimeSetUp]
        public void OpenLedger() { }

        [SetUp]
        public void Reset() { }

        [Test]
        public void Totals() { }

        [TestCase(1, 2)]
        [TestCase(3, 4)]
        public void Sums(int left, int right) { }

        [TestCaseSource(nameof(Cases))]
        public void Sourced(int amount) { }

        [Theory]
        public void Holds(int amount) { }

        [NUnit.Framework.TestAttribute]
        public void Qualified() { }

        [TearDown]
        public void Clean() { }

        [OneTimeTearDown]
        public void CloseLedger() { }

        private static int[] Cases() => new[] { 1 };
    }
}
"#;
        let names = |origin| {
            csharp_targets("tests/Ledger/StatementTests.cs", src)
                .into_iter()
                .filter(|(_, target_origin, _)| *target_origin == origin)
                .map(|(name, _, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(TestFileSymbol),
            vec!["Totals", "Sums", "Sourced", "Holds", "Qualified"]
        );
        assert_eq!(
            names(TestFileHelper),
            vec!["OpenLedger", "Reset", "Clean", "CloseLedger", "Cases"]
        );
        assert!(csharp_targets("tests/Ledger/StatementTests.cs", src)
            .iter()
            .any(|(name, _, command)| name == "Sums"
                && *command == csharp_filter("Acme.Ledger.Tests.StatementTests.Sums")));
    }

    /// MSTest runs a `[TestMethod]` or `[DataTestMethod]` only in a `[TestClass]` class, or
    /// through a class deriving from an abstract one.
    #[test]
    fn mstest_runs_test_methods_of_test_classes_only() {
        use open_kioku_core::TestTargetOrigin::{TestFileHelper, TestFileSymbol};
        let src = r#"namespace Acme.Ledger.Tests;

[TestClass]
public sealed class JournalTests
{
    [ClassInitialize]
    public static void Open(TestContext context) { }

    [TestInitialize]
    public void Reset() { }

    [TestMethod]
    public void Replays() { }

    [DataTestMethod]
    [DataRow(1)]
    [DataRow(2)]
    public void Rounds(int value) { }

    [TestCleanup]
    public void Clean() { }
}

public class Unmarked
{
    [TestMethod]
    public void NeverRuns() { }
}

public abstract class JournalContract
{
    [TestMethod]
    public void Balances() { }
}
"#;
        assert_eq!(
            csharp_targets("tests/Ledger/JournalTests.cs", src),
            vec![
                ("Open".into(), TestFileHelper, "dotnet test".into()),
                ("Reset".into(), TestFileHelper, "dotnet test".into()),
                (
                    "Replays".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.JournalTests.Replays")
                ),
                (
                    "Rounds".into(),
                    TestFileSymbol,
                    csharp_filter("Acme.Ledger.Tests.JournalTests.Rounds")
                ),
                ("Clean".into(), TestFileHelper, "dotnet test".into()),
                ("NeverRuns".into(), TestFileHelper, "dotnet test".into()),
                (
                    "Balances".into(),
                    TestFileSymbol,
                    csharp_filter(".Balances")
                ),
            ]
        );
    }

    /// A `partial` class may carry `[TestClass]` on a part in another file, so its test methods
    /// run. A test of a file read through syntax errors rests on a medium-confidence symbol, and
    /// is no stronger than it.
    #[test]
    fn csharp_partial_and_recovered_tests_say_what_they_rest_on() {
        use open_kioku_core::TestTargetOrigin::TestFileSymbol;
        let partial = "namespace Acme.Tests;\n\npublic partial class GuardTests\n{\n    [TestMethod]\n    public void Rejects() { }\n}\n";
        assert_eq!(
            csharp_targets("tests/Acme.Tests/GuardTests.Array.cs", partial),
            vec![(
                "Rejects".into(),
                TestFileSymbol,
                csharp_filter("Acme.Tests.GuardTests.Rejects")
            )]
        );

        // This grammar version cannot read `async` as a name, which C# allows.
        let recovered = "namespace Acme.Tests;\npublic class BatchTests\n{\n    [Fact]\n    public void Loads()\n    {\n        Assert.True(Load(async: true));\n    }\n    public bool Load(bool async) => async;\n}\n";
        let file = file_at("tests/Acme.Tests/BatchTests.cs", Language::CSharp);
        let symbols = extract_symbols(&file, recovered);
        assert!(symbols
            .iter()
            .all(|symbol| symbol.confidence == Confidence::Medium));
        let tests = extract_tests(&file, recovered, &symbols, None);
        let loads = tests.iter().find(|test| test.name == "Loads").unwrap();
        assert_eq!(loads.origin, TestFileSymbol);
        assert_eq!(loads.confidence, Confidence::Medium);
        assert_eq!(loads.reason, super::RECOVERED_CSHARP_TEST_REASON);
    }

    /// Outside a test path a C# test is one by its attribute alone, and its helpers are no
    /// targets at all. Same-named tests of sibling nested classes stay distinct targets.
    #[test]
    fn csharp_tests_outside_test_paths_are_matched_by_attribute() {
        let src = "namespace Acme.Checks;\n\npublic class Balances\n{\n    [Fact]\n    public void Holds() { }\n\n    public void Helper() { }\n\n    public class WhenPosted { [Fact] public void Holds() { } }\n}\n";
        let file = file_at("src/Acme.Checks/Balances.cs", Language::CSharp);
        let symbols = extract_symbols(&file, src);
        let tests = extract_tests(&file, src, &symbols, None);
        assert_eq!(tests.len(), 2, "{tests:?}");
        assert!(tests.iter().all(
            |test| test.origin == open_kioku_core::TestTargetOrigin::Symbol && test.name == "Holds"
        ));
        assert_ne!(tests[0].id, tests[1].id);
        let commands = tests
            .iter()
            .map(|test| test.command.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        assert!(commands.contains(&csharp_filter("Acme.Checks.Balances.Holds")));
        assert!(commands.contains(&csharp_filter("Acme.Checks.Balances+WhenPosted.Holds")));
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
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
                alias_of: None,
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
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
                alias_of: None,
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
    fn constants_and_structs_beside_an_inline_test_module_are_not_test_targets() {
        let file = rust_file();
        let src = "pub const LATTICE_ANCHOR_BOOST: f32 = 0.4;\n\npub struct ScoreComponent;\n\npub fn boost() -> f32 {\n    LATTICE_ANCHOR_BOOST\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn boost_is_positive() {\n        assert!(boost() > 0.0);\n    }\n}\n";
        use crate::{HeuristicParser, Parser};
        let parsed = HeuristicParser.parse_with_hint(&file, src, None);
        let names = parsed
            .tests
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["boost_is_positive"]);
    }

    #[test]
    fn annotation_text_inside_another_call_does_not_mark_a_function() {
        let file = rust_file();
        let src = "fn flush(store: &Store) {\n    store.commit(latest());\n}\nfn helper() {}\n";
        let symbols = extract_symbols(&file, src);
        assert!(extract_tests(&file, src, &symbols, None).is_empty());
    }

    fn function_symbol(name: &str, start: u32) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("symbol-{name}")),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Function,
            file_id: FileId::new("file-rs"),
            range: Some(LineRange {
                start,
                end: start + 1,
            }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
            alias_of: None,
        }
    }

    /// Whether the symbol declared on the line naming `name` counts as annotated.
    fn annotated(src: &str, name: &str) -> bool {
        let lines = src.lines().collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|line| {
                line.contains(&format!("fn {name}(")) || line.contains(&format!(" {name}("))
            })
            .expect("the source declares the symbol") as u32
            + 1;
        super::has_adjacent_test_annotation(&lines, &function_symbol(name, start))
    }

    #[test]
    fn a_test_annotation_above_a_stack_of_attributes_and_comments_marks_the_function() {
        let rstest = "#[rstest]\n#[case(0, 0)]\n#[case(1, 1)]\n#[case(2, 4)]\n#[case(3, 9)]\nfn squares(#[case] n: u32, #[case] expected: u32) {}\n";
        assert!(annotated(rstest, "squares"));
        let should_panic =
            "#[test]\n#[should_panic(\n    expected = \"boom\"\n)]\nfn panics() {}\n";
        assert!(annotated(should_panic, "panics"));
        let tokio = "/// Serialised because it binds a port.\n#[tokio::test(flavor = \"multi_thread\")]\n#[serial]\n#[allow(clippy::unwrap_used)]\n#[cfg_attr(miri, ignore)]\nasync fn binds_port() {}\n";
        assert!(annotated(tokio, "binds_port"));
        let java = "@ParameterizedTest\n@ValueSource(strings = {\n    \"\",\n    \" \"\n})\n@DisplayName(\"blank input\")\n@Tag(\"fast\")\nvoid rejectsBlank(String value) {}\n";
        assert!(annotated(java, "rejectsBlank"));
    }

    #[test]
    fn a_helper_declared_right_after_a_test_body_is_not_a_test() {
        let javascript =
            "test('rounds', () => {\n  expect(round(1.5)).toBe(2)\n})\nexport function helper() {}\n";
        assert!(!annotated(javascript, "helper"));
        let python =
            "def test_rounds():\n    assert round_half(1.5) == round(2)\ndef helper():\n    pass\n";
        assert!(!annotated(python, "helper"));
        assert!(annotated(python, "test_rounds"));
    }

    #[test]
    fn the_annotation_stack_ends_at_code_and_blank_lines() {
        assert!(!annotated(
            "#[test]\nfn first() {}\nfn second() {}\n",
            "second"
        ));
        assert!(!annotated("#[test]\n\nfn detached() {}\n", "detached"));
        assert!(!annotated(
            "let value = compute(input)\nfn after_call() {}\n",
            "after_call"
        ));
    }

    #[test]
    fn a_repository_root_tests_directory_is_a_test_path() {
        let file = File {
            id: FileId::new("root-tests"),
            repository_id: RepositoryId::new("repo"),
            path: "tests/cli.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let src = "fn helper() {}\n";
        let symbols = extract_symbols(&file, src);
        assert!(!symbols.is_empty());
        assert_eq!(
            extract_tests(&file, src, &symbols, None).len(),
            symbols.len()
        );
    }

    #[test]
    fn every_callable_in_a_test_path_file_is_a_target_and_nothing_else_is() {
        let file = file_at("src/worker_test.rs", Language::Rust);
        let src = "pub const RETRY_LIMIT: u32 = 3;\npub struct Probe;\npub fn some_helper() {}\n";
        assert_eq!(target_names(&file, src), vec!["some_helper"]);
    }

    fn file_at(path: &str, language: Language) -> File {
        File {
            id: FileId::new(format!("file-{path}")),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn target_names(file: &File, src: &str) -> Vec<String> {
        let symbols = extract_symbols(file, src);
        let mut names = extract_tests(file, src, &symbols, None)
            .into_iter()
            .map(|test| test.name)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// A JS/TS runner executes registration calls, never a declared function, so every callable
    /// of a test file is kept as test code and none of them is a test.
    #[test]
    fn javascript_and_typescript_test_layouts_keep_every_callable_as_a_helper() {
        let src = "import { convert } from \"../rates\";\n\nconst SAMPLE_RATE = 1.25;\n\nfunction roundsHalfUp() {\n  expect(convert(2, SAMPLE_RATE)).toBe(2.5);\n}\n\nexport async function loadsRateTable() {\n  await loadTable();\n}\n";
        for (path, language) in [
            ("src/rates_test.ts", Language::TypeScript),
            ("src/rates_test.js", Language::JavaScript),
            ("src/rates.test.js", Language::JavaScript),
            ("src/rates.spec.js", Language::JavaScript),
            ("src/RateTable.test.tsx", Language::TypeScript),
            ("src/__tests__/rates.ts", Language::TypeScript),
            ("test/rates.ts", Language::TypeScript),
        ] {
            let file = file_at(path, language);
            assert_eq!(
                target_names(&file, src),
                vec!["loadsRateTable", "roundsHalfUp"],
                "{path}"
            );
            let symbols = extract_symbols(&file, src);
            for test in extract_tests(&file, src, &symbols, None) {
                assert_eq!(
                    test.origin,
                    open_kioku_core::TestTargetOrigin::TestFileHelper,
                    "{path}: {}",
                    test.name
                );
                assert!(test.has_test_provenance(), "{path}: {}", test.name);
                assert!(
                    !test.counts_as_validation_evidence(),
                    "{path}: {}",
                    test.name
                );
                assert!(matches!(test.confidence, Confidence::Low), "{path}");
            }
        }
    }

    #[test]
    fn a_const_bound_function_in_a_test_file_is_a_target_only_when_extracted_as_callable() {
        let file = file_at("src/rates.test.ts", Language::TypeScript);
        let src = "const parsesRates = () => {};\nconst SAMPLE_TABLE = [1, 2];\n";
        let callable = function_symbol("parsesRates", 1);
        let mut variable = function_symbol("SAMPLE_TABLE", 2);
        variable.kind = SymbolKind::Variable;
        let names = extract_tests(&file, src, &[callable, variable], None)
            .into_iter()
            .map(|test| test.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["parsesRates"]);
    }

    #[test]
    fn a_production_function_named_testable_is_not_a_test_target() {
        let file = file_at("src/rates.ts", Language::TypeScript);
        let src = "export function testable(rate: number): boolean {\n  return rate > 0;\n}\n\nexport function convert(amount: number, rate: number): number {\n  return amount * rate;\n}\n";
        assert!(!extract_symbols(&file, src).is_empty());
        assert!(target_names(&file, src).is_empty());
        for name in ["test", "test_rounds", "testRounds", "test2"] {
            assert!(super::has_test_name_prefix(name), "{name}");
        }
        for name in ["testable", "testimony", "Testable", "attest"] {
            assert!(!super::has_test_name_prefix(name), "{name}");
        }
    }

    /// Each registration form, a namespaced member form, a template name, a non-literal name, a
    /// suite, and a namespaced `.test` call that is a regular expression check.
    const REGISTRATIONS: &str = "describe(\"rate table\", () => {\n  test(\"converts at the posted rate\", () => {});\n  it('rounds half up', () => {});\n  test.only(\"reads   the header\", () => {});\n  it.skip(\"skips stale rows\", () => {});\n  test.each([[1, 2]])(\"adds %i to %i\", (left, right) => {});\n  Suite.test(\"parses rows\", async () => {});\n  it(`formats ${currency} totals`, () => {});\n  test(caseName, () => {});\n  expect(RowPattern.test(\"not a registration\")).toBe(true);\n});\n";

    const REGISTERED_NAMES: [&str; 7] = [
        "adds %i to %i",
        "converts at the posted rate",
        "formats ${currency} totals",
        "parses rows",
        "reads the header",
        "rounds half up",
        "skips stale rows",
    ];

    #[test]
    fn registration_calls_in_test_files_are_targets_in_ts_tsx_and_js() {
        for (path, language) in [
            ("src/rates.test.ts", Language::TypeScript),
            ("src/RateTable.test.tsx", Language::TypeScript),
            ("src/rates.spec.js", Language::JavaScript),
        ] {
            let file = file_at(path, language);
            assert_eq!(
                target_names(&file, REGISTRATIONS),
                REGISTERED_NAMES,
                "{path}"
            );
            let symbols = extract_symbols(&file, REGISTRATIONS);
            let targets = extract_tests(&file, REGISTRATIONS, &symbols, None);
            let converts = targets
                .iter()
                .find(|test| test.name == "converts at the posted rate")
                .expect("the plain registration is a target");
            assert_eq!(
                converts
                    .range
                    .as_ref()
                    .map(|range| (range.start, range.end)),
                Some((2, 2)),
                "{path}"
            );
            assert!(matches!(converts.confidence, Confidence::High), "{path}");
            let formats = targets
                .iter()
                .find(|test| test.name == "formats ${currency} totals")
                .expect("the template registration is a target");
            assert!(matches!(formats.confidence, Confidence::Medium), "{path}");
        }
    }

    #[test]
    fn a_disabled_registration_call_is_low_confidence_and_not_validation_evidence() {
        let src = "test.skip(\"skips the ledger\", () => {});\nit.skip(\"skips stale rows\", () => {});\ntest.todo(\"writes the receipt\");\ntest.failing(\"fails for now\", () => {});\nit.fails(\"fails as well\", () => {});\ntest.only(\"runs alone\", () => {});\n";
        let file = file_at("src/ledger.test.ts", Language::TypeScript);
        let symbols = extract_symbols(&file, src);
        let targets = extract_tests(&file, src, &symbols, None);
        let named = |name: &str| {
            targets
                .iter()
                .find(|target| target.name == name)
                .unwrap_or_else(|| panic!("no target named {name}"))
        };
        for name in [
            "skips the ledger",
            "skips stale rows",
            "writes the receipt",
            "fails for now",
            "fails as well",
        ] {
            let target = named(name);
            assert!(matches!(target.confidence, Confidence::Low), "{name}");
            assert!(!target.counts_as_validation_evidence(), "{name}");
            assert!(target.is_registration_call(), "{name}");
            assert_eq!(target.reason, super::DISABLED_REGISTRATION_REASON, "{name}");
        }
        let enabled = named("runs alone");
        assert!(matches!(enabled.confidence, Confidence::High));
        assert!(enabled.counts_as_validation_evidence());
        assert!(enabled.is_registration_call());
        assert_eq!(enabled.reason, super::REGISTRATION_REASON);
    }

    #[test]
    fn the_pattern_fallback_also_marks_a_disabled_registration() {
        let calls = super::test_registration_calls_by_pattern(
            "it.skip('skips stale rows', () => {});\ntest('keeps totals', () => {});\n",
        );
        assert_eq!(calls.len(), 2);
        assert!(calls[0].disabled, "it.skip is disabled");
        assert!(!calls[1].disabled, "a plain test is not");
    }

    #[test]
    fn a_registration_call_in_a_tsx_file_with_jsx_is_a_target() {
        let src =
            "it(\"renders the rate table\", () => {\n  render(<RateTable rows={rows} />);\n});\n";
        assert_eq!(
            target_names(
                &file_at("src/RateTable.test.tsx", Language::TypeScript),
                src
            ),
            vec!["renders the rate table"]
        );
    }

    #[test]
    fn registration_calls_in_a_production_file_are_not_targets() {
        for (path, language) in [
            ("src/rateTable.ts", Language::TypeScript),
            ("src/rateTable.js", Language::JavaScript),
        ] {
            assert!(
                target_names(&file_at(path, language), REGISTRATIONS).is_empty(),
                "{path}"
            );
        }
    }

    #[test]
    fn a_test_file_with_a_syntax_error_falls_back_to_single_line_registrations() {
        let src = "test(\"keeps totals\", () => {\n  const = ;\n});\nSuite.it('parses rows', function () {});\nRowPattern.test(\"abc\");\ntest(caseName, () => {});\n";
        assert_eq!(
            target_names(&file_at("src/rates.test.ts", Language::TypeScript), src),
            vec!["keeps totals", "parses rows"]
        );
    }

    /// A JUnit class declares its tests as `@Test` methods named for behaviour, which no name
    /// heuristic recognises, so the target carries the runner's verdict instead of leaving later
    /// surfaces to guess from the name. A `unittest.TestCase` runs only its `test*` methods: any
    /// other method beside them is kept as test code but is a helper, not validation.
    #[test]
    fn junit_and_unittest_methods_are_targets_by_test_file_provenance() {
        let java = file_at("src/test/java/com/acme/RatesTest.java", Language::Java);
        let java_src = "package com.acme;\n\npublic class RatesTest {\n  @Test\n  void shouldRoundHalfUp() {\n    assertEquals(3, Rates.convert(2, 1.5));\n  }\n\n  @Test\n  void roundsTowardsEven() {\n    assertEquals(2, Rates.convert(2, 1.25));\n  }\n}\n";
        let java_targets = extract_tests(&java, java_src, &extract_symbols(&java, java_src), None);
        let java_names = java_targets
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert!(java_names.contains(&"shouldRoundHalfUp"), "{java_names:?}");
        assert!(java_names.contains(&"roundsTowardsEven"), "{java_names:?}");
        assert!(
            !java_names.contains(&"RatesTest"),
            "a test class is not a target"
        );
        for target in &java_targets {
            assert!(target.has_test_provenance(), "{}", target.name);
            assert!(target.counts_as_validation_evidence(), "{}", target.name);
        }

        let python = file_at("tests/test_rates.py", Language::Python);
        let python_src = "import unittest\n\n\nclass RatesTest(unittest.TestCase):\n    def test_rounds_half_up(self):\n        self.assertEqual(convert(2, 1.5), 3)\n\n    def rounds_towards_even(self):\n        self.assertEqual(convert(2, 1.25), 2)\n";
        let python_targets = extract_tests(
            &python,
            python_src,
            &extract_symbols(&python, python_src),
            None,
        );
        let python_names = python_targets
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            python_names.contains(&"test_rounds_half_up"),
            "{python_names:?}"
        );
        assert!(
            python_names.contains(&"rounds_towards_even"),
            "a helper in a test file is still test code: {python_names:?}"
        );
        assert!(
            !python_names.contains(&"RatesTest"),
            "a test class is not a target"
        );
        for target in &python_targets {
            assert!(target.has_test_provenance(), "{}", target.name);
            assert_eq!(
                target.counts_as_validation_evidence(),
                target.name == "test_rounds_half_up",
                "{}",
                target.name
            );
        }
    }

    /// Each target of `src` at `path`, by name, with whether it is validation evidence.
    fn runnable_by_name(path: &str, language: Language, src: &str) -> BTreeMap<String, bool> {
        use crate::{HeuristicParser, Parser};
        let file = file_at(path, language);
        HeuristicParser
            .parse_with_hint(&file, src, None)
            .tests
            .into_iter()
            .map(|test| {
                assert!(
                    test.has_test_provenance() || !open_kioku_core::is_test_code_path(path),
                    "{path}: {}",
                    test.name
                );
                (test.name.clone(), test.counts_as_validation_evidence())
            })
            .collect()
    }

    fn assert_runnable(path: &str, language: Language, src: &str, expected: &[(&str, bool)]) {
        let expected = expected
            .iter()
            .map(|(name, runs)| ((*name).to_string(), *runs))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(runnable_by_name(path, language, src), expected, "{path}");
    }

    #[test]
    fn a_rust_test_file_runs_only_its_attributed_functions() {
        let src = "mod common;\n\nfn make_client() -> Client {\n    Client::default()\n}\n\nfn with_temp_repo<F: FnOnce(&Path)>(run: F) {\n    run(Path::new(\".\"));\n}\n\n#[test]\nfn posts_an_entry() {\n    with_temp_repo(|_| make_client().post());\n}\n\n#[tokio::test]\nasync fn replays_the_journal() {}\n\n#[sqlx::test]\nasync fn stores_a_row() {}\n";
        assert_runnable(
            "tests/ledger.rs",
            Language::Rust,
            src,
            &[
                ("make_client", false),
                ("with_temp_repo", false),
                ("posts_an_entry", true),
                ("replays_the_journal", true),
                ("stores_a_row", true),
            ],
        );
        // A shared helper module of an integration-test directory holds no test.
        assert_runnable(
            "tests/common/mod.rs",
            Language::Rust,
            "pub fn setup() -> Ledger {\n    Ledger::open()\n}\n",
            &[("setup", false)],
        );
    }

    #[test]
    fn a_python_test_module_runs_test_functions_and_test_class_methods_only() {
        let src = "import pytest\nimport unittest\n\n\n@pytest.fixture\ndef test_ledger():\n    return Ledger()\n\n\ndef make_client():\n    return Client()\n\n\ndef test_posts_entry(test_ledger):\n    assert make_client().post(test_ledger)\n\n\nclass LedgerTest(unittest.TestCase):\n    def setUp(self):\n        self.ledger = Ledger()\n\n    def tearDown(self):\n        self.ledger.close()\n\n    def test_balance(self):\n        self.assertEqual(self.ledger.balance(), 0)\n\n    def rounds_towards_even(self):\n        pass\n\n\nclass TestJournal:\n    def setup_method(self):\n        pass\n\n    def test_replays(self):\n        pass\n\n\nclass FakeJournal:\n    def test_stub(self):\n        pass\n";
        assert_runnable(
            "tests/test_ledger.py",
            Language::Python,
            src,
            &[
                ("test_ledger", false),
                ("make_client", false),
                ("test_posts_entry", true),
                ("setUp", false),
                ("tearDown", false),
                ("test_balance", true),
                ("rounds_towards_even", false),
                ("setup_method", false),
                ("test_replays", true),
                ("test_stub", false),
            ],
        );
        // pytest collects no test from `conftest.py` or a helper module beside the tests.
        for path in [
            "tests/conftest.py",
            "tests/helpers.py",
            "testutil/ledger.py",
        ] {
            assert_runnable(
                path,
                Language::Python,
                "def test_data():\n    return []\n\n\ndef make_ledger():\n    return Ledger()\n",
                &[("test_data", false), ("make_ledger", false)],
            );
        }
    }

    #[test]
    fn a_go_test_file_runs_test_fuzz_and_output_examples_only() {
        let src = "package ledger\n\nimport \"testing\"\n\nfunc TestMain(m *testing.M) {\n\tos.Exit(m.Run())\n}\n\nfunc newServer(t *testing.T) *Server {\n\treturn &Server{}\n}\n\nfunc TestPostsEntry(t *testing.T) {\n\tnewServer(t)\n}\n\nfunc FuzzParse(f *testing.F) {}\n\nfunc BenchmarkPost(b *testing.B) {}\n\nfunc Testify() {}\n\nfunc ExampleLedger() {\n\tfmt.Println(1)\n\t// Output: 1\n}\n\nfunc ExampleLedger_quiet() {\n\tfmt.Println(1)\n}\n";
        assert_runnable(
            "ledger/ledger_test.go",
            Language::Go,
            src,
            &[
                ("TestMain", false),
                ("newServer", false),
                ("TestPostsEntry", true),
                ("FuzzParse", true),
                ("BenchmarkPost", false),
                ("Testify", false),
                ("ExampleLedger", true),
                ("ExampleLedger_quiet", false),
            ],
        );
        // `go test` compiles only `_test.go` files as tests: a `testutil` package's exported
        // helpers are no test, whatever they are called.
        assert_runnable(
            "internal/testutil/server.go",
            Language::Go,
            "package testutil\n\nfunc NewServer() *Server {\n\treturn &Server{}\n}\n\nfunc TestServer() *Server {\n\treturn NewServer()\n}\n",
            &[("NewServer", false), ("TestServer", false)],
        );
    }

    #[test]
    fn a_java_test_class_runs_annotated_methods_junit3_tests_and_testng_class_methods() {
        let junit5 = "package com.acme;\n\nclass LedgerTest {\n  @BeforeEach\n  void setUp() {\n    ledger = new Ledger();\n  }\n\n  @AfterEach\n  void tearDown() {}\n\n  private Client makeClient() {\n    return new Client();\n  }\n\n  @Test\n  void shouldPostEntry() {\n    makeClient().post(ledger);\n  }\n\n  @ParameterizedTest\n  @ValueSource(ints = {1, 2})\n  void roundsTowardsEven(int value) {}\n}\n";
        assert_runnable(
            "src/test/java/com/acme/LedgerTest.java",
            Language::Java,
            junit5,
            &[
                ("setUp", false),
                ("tearDown", false),
                ("makeClient", false),
                ("shouldPostEntry", true),
                ("roundsTowardsEven", true),
            ],
        );
        let junit3 = "package com.acme;\n\npublic class JournalTest extends TestCase {\n  protected void setUp() {}\n\n  public void testReplays() {}\n\n  private Journal journal() {\n    return new Journal();\n  }\n}\n";
        assert_runnable(
            "src/test/java/com/acme/JournalTest.java",
            Language::Java,
            junit3,
            &[("setUp", false), ("testReplays", true), ("journal", false)],
        );
        let testng = "package com.acme;\n\n@Test\npublic class BalanceTest {\n  @BeforeMethod\n  public void reset() {}\n\n  @DataProvider\n  public Object[][] amounts() {\n    return new Object[][] {};\n  }\n\n  public void sumsEntries() {}\n\n  private Ledger ledger() {\n    return new Ledger();\n  }\n}\n";
        assert_runnable(
            "src/test/java/com/acme/BalanceTest.java",
            Language::Java,
            testng,
            &[
                ("reset", false),
                ("amounts", false),
                ("sumsEntries", true),
                ("ledger", false),
            ],
        );
        let support = "package com.acme.testing;\n\npublic final class LedgerFixtures {\n  public static Ledger emptyLedger() {\n    return new Ledger();\n  }\n\n  public static Client makeClient() {\n    return new Client();\n  }\n}\n";
        assert_runnable(
            "src/test/java/com/acme/testing/LedgerFixtures.java",
            Language::Java,
            support,
            &[("emptyLedger", false), ("makeClient", false)],
        );
    }

    #[test]
    fn a_typescript_test_file_runs_its_registrations_and_none_of_its_helpers() {
        let src = "import { Ledger } from \"../src/ledger\";\n\nfunction makeClient() {\n  return new Client();\n}\n\nasync function withTempRepo(run: (dir: string) => Promise<void>) {\n  await run(\"/tmp\");\n}\n\nbeforeEach(() => {\n  makeClient();\n});\n\ndescribe(\"ledger\", () => {\n  it(\"posts an entry\", async () => {\n    await withTempRepo(async () => {});\n  });\n  test.skip(\"replays the journal\", () => {});\n});\n";
        assert_runnable(
            "test/ledger.test.ts",
            Language::TypeScript,
            src,
            &[
                ("makeClient", false),
                ("withTempRepo", false),
                ("posts an entry", true),
                ("replays the journal", false),
            ],
        );
        assert_runnable(
            "test-utils/repo.ts",
            Language::TypeScript,
            "export async function withTempRepo(run: () => Promise<void>) {\n  await run();\n}\n\nexport function makeClient() {\n  return new Client();\n}\n",
            &[("withTempRepo", false), ("makeClient", false)],
        );
    }

    #[test]
    fn a_symbol_matched_outside_a_test_file_keeps_plain_symbol_provenance() {
        let file = file_at("src/rates.rs", Language::Rust);
        let src = "#[test]\nfn rounds_half_up() {\n    assert_eq!(convert(2, 1.5), 3);\n}\n";
        let targets = extract_tests(&file, src, &extract_symbols(&file, src), None);
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert!(!targets[0].has_test_provenance());
        assert!(targets[0].counts_as_validation_evidence());
    }

    #[test]
    fn data_only_directories_under_a_test_path_do_not_make_every_callable_a_target() {
        let go = "package lexer\n\nfunc Scan() {}\n";
        assert_eq!(
            target_names(&file_at("internal/lexer/lexer_test.go", Language::Go), go),
            vec!["Scan"]
        );
        for path in [
            "internal/lexer/testdata/input.go",
            "internal/lexer/testdata/input_test.go",
        ] {
            assert!(
                target_names(&file_at(path, Language::Go), go).is_empty(),
                "{path}"
            );
        }
        let ts = "export function renderInvoice() {}\n";
        for path in [
            "tests/fixtures/invoice.ts",
            "src/__fixtures__/invoice.ts",
            "src/__snapshots__/invoice.test.ts",
        ] {
            assert!(
                target_names(&file_at(path, Language::TypeScript), ts).is_empty(),
                "{path}"
            );
        }
    }

    /// The attribute forms common test crates generate tests with, qualified and bare, through
    /// `cfg_attr`, split over lines, and with doc comments and other attributes between them. An
    /// rstest_reuse `#[template]` carries `#[rstest]` but is a case list, not a test.
    const RUST_TEST_ATTRIBUTES: &str = r##"use ledger::{post_entry, rounds_half_up};

#[test_strategy::proptest]
fn strategy_proptest_rounds(#[strategy(0.0f64..10.0)] value: f64) {
    assert!(rounds_half_up(value) >= 0);
}

#[proptest]
fn bare_proptest_rounds(value: u8) {
    assert!(rounds_half_up(value as f64) >= 0);
}

#[rstest::rstest]
fn qualified_rstest_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[test_case::test_case(1, 1 ; "one")]
fn qualified_test_case_posts(amount: i64, expected: i64) {
    assert_eq!(post_entry(&mut vec![], amount), expected);
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn qualified_wasm_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[quickcheck_macros::quickcheck]
fn qualified_quickcheck_rounds(value: u8) -> bool {
    rounds_half_up(value as f64) >= 0
}

#[template]
#[rstest::rstest]
#[case(1)]
fn amounts(#[case] amount: i64) {}

#[apply(amounts)]
fn reused_template_posts(#[case] amount: i64) {
    assert_eq!(post_entry(&mut vec![], amount), amount);
}

#[pg_test]
fn pgrx_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[googletest::test]
fn googletest_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[cfg_attr(not(miri), test)]
fn cfg_attr_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[tokio::test(
    flavor = "multi_thread",
    worker_threads = 2
)]
async fn multiline_tokio_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

#[test]
/// Documented after the attribute.
#[allow(clippy::unit_cmp)]
fn attributed_through_a_stack_posts() {
    assert_eq!(post_entry(&mut vec![], 1), 1);
}

fn plain_helper() -> Vec<i64> {
    Vec::new()
}
"##;

    #[test]
    fn rust_test_crate_attributes_make_tests_and_an_rstest_template_does_not() {
        assert_runnable(
            "tests/attrs.rs",
            Language::Rust,
            RUST_TEST_ATTRIBUTES,
            &[
                ("strategy_proptest_rounds", true),
                ("bare_proptest_rounds", true),
                ("qualified_rstest_posts", true),
                ("qualified_test_case_posts", true),
                ("qualified_wasm_posts", true),
                ("qualified_quickcheck_rounds", true),
                ("amounts", false),
                ("reused_template_posts", true),
                ("pgrx_posts", true),
                ("googletest_posts", true),
                ("cfg_attr_posts", true),
                ("multiline_tokio_posts", true),
                ("attributed_through_a_stack_posts", true),
                ("plain_helper", false),
            ],
        );
    }

    /// JUnit 3 runs `public void test*()` of a `TestCase` subclass, and the `TestCase` is often
    /// behind an abstract base in another file, as `unittest`'s is behind a shared base class.
    #[test]
    fn junit3_and_unittest_tests_inherited_through_a_base_in_another_file_are_tests() {
        let java = "package com.acme;\n\npublic class LedgerLegacyTest extends AbstractLedgerTest {\n  public void testPostsLegacy() {\n    assertEquals(1, new Ledger().post(1));\n  }\n\n  public Ledger testLedger(long opening) {\n    return new Ledger();\n  }\n\n  void testPackagePrivate() {}\n}\n";
        assert_runnable(
            "src/test/java/com/acme/LedgerLegacyTest.java",
            Language::Java,
            java,
            &[
                ("testPostsLegacy", true),
                ("testLedger", false),
                ("testPackagePrivate", false),
            ],
        );
        // JUnit 4 and 5 ignore JUnit 3 naming: in a file written for them, an unannotated
        // `public void test*()` in a subclass is a helper.
        let jupiter = "package com.acme;\n\nimport org.junit.jupiter.api.Test;\n\npublic class LedgerJupiterTest extends LedgerFixtures {\n  @Test\n  void postsEntry() {}\n\n  public void testFixtureLoads() {}\n}\n";
        assert_runnable(
            "src/test/java/com/acme/LedgerJupiterTest.java",
            Language::Java,
            jupiter,
            &[("postsEntry", true), ("testFixtureLoads", false)],
        );
        let python = "from tests.base import LedgerCase\n\n\nclass PostTests(LedgerCase):\n    def test_posts(self):\n        pass\n\n\nclass TestWithInit:\n    def __init__(self):\n        self.ledger = []\n\n    def test_skipped_by_pytest(self):\n        pass\n\n\nclass Plain(object):\n    def test_not_collected(self):\n        pass\n";
        assert_runnable(
            "tests/test_posts.py",
            Language::Python,
            python,
            &[
                ("test_posts", true),
                ("__init__", false),
                ("test_skipped_by_pytest", false),
                ("test_not_collected", false),
            ],
        );
    }
}
