use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, FileId, GraphEdgeType,
    GraphNodeType, ImportResolution, ResolutionStatus, Symbol, SymbolId, SymbolKind,
};
use std::collections::{HashMap, HashSet};

const COMMON_NAME_CAP: usize = 32;
const MAX_TOKENS_PER_CHUNK: usize = 80;
const MAX_UNRESOLVED_NOTES: usize = 64;

#[derive(Debug, Clone)]
pub struct SymbolRegistry {
    pub by_id: HashMap<SymbolId, Symbol>,
    pub by_qualified_name: HashMap<String, Vec<SymbolId>>,
    pub by_simple_name: HashMap<String, Vec<SymbolId>>,
    pub by_file: HashMap<FileId, Vec<SymbolId>>,
    pub by_module: HashMap<String, Vec<SymbolId>>,
    pub import_resolutions: Vec<ImportResolution>,
}

#[derive(Debug, Clone, Default)]
pub struct RegistryReport {
    pub analysis_facts: Vec<AnalysisFact>,
    pub quality_notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct Resolution {
    symbol: Option<Symbol>,
    strategy: &'static str,
    candidates: usize,
    confidence: Confidence,
    ambiguity_reason: Option<String>,
    speculative: bool,
}

#[derive(Debug, Clone)]
struct TokenUse {
    token: String,
    line: u32,
    is_call: bool,
}

impl SymbolRegistry {
    pub fn new(symbols: &[Symbol], import_resolutions: &[ImportResolution]) -> Self {
        let mut registry = Self {
            by_id: HashMap::new(),
            by_qualified_name: HashMap::new(),
            by_simple_name: HashMap::new(),
            by_file: HashMap::new(),
            by_module: HashMap::new(),
            import_resolutions: import_resolutions.to_vec(),
        };
        for symbol in symbols {
            registry.by_id.insert(symbol.id.clone(), symbol.clone());
            registry
                .by_qualified_name
                .entry(symbol.qualified_name.clone())
                .or_default()
                .push(symbol.id.clone());
            registry
                .by_simple_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id.clone());
            registry
                .by_file
                .entry(symbol.file_id.clone())
                .or_default()
                .push(symbol.id.clone());
            registry
                .by_module
                .entry(module_name(&symbol.qualified_name))
                .or_default()
                .push(symbol.id.clone());
        }
        registry
    }

    fn resolve(&self, chunk: &CodeChunk, token: &str) -> Resolution {
        if let Some(resolution) = self.resolve_import_target(chunk, token) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_same_file(chunk, token) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_same_module(chunk, token) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_unique_project_name(token) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_suffix_with_import_reachability(chunk, token) {
            return resolution;
        }
        self.resolve_fuzzy(token).unwrap_or_else(|| Resolution {
            symbol: None,
            strategy: "unresolved",
            candidates: 0,
            confidence: Confidence::Low,
            ambiguity_reason: Some("no registry candidate matched".into()),
            speculative: true,
        })
    }

    fn resolve_import_target(&self, chunk: &CodeChunk, token: &str) -> Option<Resolution> {
        let mut candidates = Vec::new();
        for import in self
            .import_resolutions
            .iter()
            .filter(|resolution| resolution.import.file_id == chunk.file_id)
        {
            if !matches!(import.status, ResolutionStatus::Resolved) {
                continue;
            }
            if let Some(symbol_id) = &import.target_symbol {
                if let Some(symbol) = self.by_id.get(symbol_id) {
                    if symbol_matches_token(symbol, token) || import_mentions_token(import, token) {
                        candidates.push(symbol.clone());
                    }
                }
            } else if let Some(file_id) = &import.target_file {
                candidates.extend(
                    self.by_file
                        .get(file_id)
                        .into_iter()
                        .flatten()
                        .filter_map(|id| self.by_id.get(id))
                        .filter(|symbol| symbol_matches_token(symbol, token))
                        .cloned(),
                );
            }
        }
        resolution_from_candidates("direct-import", candidates, Confidence::High, false)
    }

    fn resolve_same_file(&self, chunk: &CodeChunk, token: &str) -> Option<Resolution> {
        let candidates = self
            .by_file
            .get(&chunk.file_id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.by_id.get(id))
            .filter(|symbol| symbol_matches_token(symbol, token))
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates("same-file", candidates, Confidence::High, false)
    }

    fn resolve_same_module(&self, chunk: &CodeChunk, token: &str) -> Option<Resolution> {
        let current = chunk
            .symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .map(|symbol| module_name(&symbol.qualified_name))?;
        let candidates = self
            .by_module
            .get(&current)
            .into_iter()
            .flatten()
            .filter_map(|id| self.by_id.get(id))
            .filter(|symbol| symbol_matches_token(symbol, token))
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates("same-module", candidates, Confidence::Medium, false)
    }

    fn resolve_unique_project_name(&self, token: &str) -> Option<Resolution> {
        let candidates = self.by_simple_name.get(token)?;
        if candidates.len() > COMMON_NAME_CAP {
            return Some(Resolution {
                symbol: None,
                strategy: "common-name-cap",
                candidates: candidates.len(),
                confidence: Confidence::Low,
                ambiguity_reason: Some(format!(
                    "common name `{token}` has {} candidates; resolver cap is {COMMON_NAME_CAP}",
                    candidates.len()
                )),
                speculative: true,
            });
        }
        let symbols = candidates
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates("unique-project-name", symbols, Confidence::Medium, true)
    }

    fn resolve_suffix_with_import_reachability(
        &self,
        chunk: &CodeChunk,
        token: &str,
    ) -> Option<Resolution> {
        let imported_suffixes = self
            .import_resolutions
            .iter()
            .filter(|resolution| resolution.import.file_id == chunk.file_id)
            .map(|resolution| resolution.import.imported.as_str())
            .collect::<Vec<_>>();
        let candidates = self
            .by_qualified_name
            .iter()
            .filter(|(qualified_name, _)| {
                qualified_name.ends_with(token)
                    && imported_suffixes
                        .iter()
                        .any(|imported| qualified_name.replace("::", ".").ends_with(*imported))
            })
            .flat_map(|(_, ids)| ids)
            .filter_map(|id| self.by_id.get(id))
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates(
            "suffix-import-reachability",
            candidates,
            Confidence::Low,
            true,
        )
    }

    fn resolve_fuzzy(&self, token: &str) -> Option<Resolution> {
        let candidates = self
            .by_simple_name
            .iter()
            .filter(|(name, _)| {
                name.len() > 3
                    && token.len() > 3
                    && (name.contains(token) || token.contains(name.as_str()))
            })
            .flat_map(|(_, ids)| ids)
            .filter_map(|id| self.by_id.get(id))
            .take(COMMON_NAME_CAP + 1)
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates("fuzzy-fallback", candidates, Confidence::Low, true)
    }
}

pub fn resolve_symbol_edges(
    chunks: &[CodeChunk],
    symbols: &[Symbol],
    import_resolutions: &[ImportResolution],
    scip_available: bool,
) -> RegistryReport {
    let registry = SymbolRegistry::new(symbols, import_resolutions);
    let mut report = RegistryReport::default();
    let mut seen = HashSet::new();
    let mut unresolved_notes = 0usize;

    for chunk in chunks {
        for token_use in token_uses(&chunk.text)
            .into_iter()
            .take(MAX_TOKENS_PER_CHUNK)
        {
            if chunk
                .symbol_id
                .as_ref()
                .and_then(|id| registry.by_id.get(id))
                .is_some_and(|symbol| symbol.name == token_use.token)
            {
                continue;
            }
            let resolution = registry.resolve(chunk, &token_use.token);
            let key = (
                chunk.id.as_str(),
                token_use.token.as_str(),
                token_use.line,
                resolution
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.id.0.as_str())
                    .unwrap_or("<unresolved>"),
            );
            if !seen.insert(format!("{}:{}:{}:{}", key.0, key.1, key.2, key.3)) {
                continue;
            }

            if let Some(note) = quality_note(&token_use.token, &resolution) {
                report.quality_notes.push(note);
            }
            if let Some(fact) = fact_for_resolution(chunk, &token_use, &resolution, scip_available)
            {
                report.analysis_facts.push(fact);
            } else if unresolved_notes < MAX_UNRESOLVED_NOTES {
                unresolved_notes += 1;
                report.quality_notes.push(format!(
                    "symbol registry unresolved `{}` in chunk {}",
                    token_use.token, chunk.id
                ));
            }
        }
    }

    report.quality_notes.sort();
    report.quality_notes.dedup();
    report.analysis_facts.sort_by(|a, b| a.id.cmp(&b.id));
    report.analysis_facts.dedup_by(|a, b| a.id == b.id);
    report
}

fn resolution_from_candidates(
    strategy: &'static str,
    mut candidates: Vec<Symbol>,
    confidence: Confidence,
    speculative: bool,
) -> Option<Resolution> {
    candidates.sort_by(|left, right| {
        symbol_rank(left)
            .cmp(&symbol_rank(right))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
    });
    candidates.dedup_by(|a, b| a.id == b.id);
    match candidates.len() {
        0 => None,
        1 => Some(Resolution {
            symbol: candidates.pop(),
            strategy,
            candidates: 1,
            confidence,
            ambiguity_reason: None,
            speculative,
        }),
        count => Some(Resolution {
            symbol: None,
            strategy,
            candidates: count,
            confidence: Confidence::Low,
            ambiguity_reason: Some(format!("{count} candidates matched via {strategy}")),
            speculative: true,
        }),
    }
}

fn fact_for_resolution(
    chunk: &CodeChunk,
    token_use: &TokenUse,
    resolution: &Resolution,
    scip_available: bool,
) -> Option<AnalysisFact> {
    let symbol = resolution.symbol.as_ref()?;
    let edge_type = if token_use.is_call {
        GraphEdgeType::Calls
    } else {
        GraphEdgeType::References
    };
    let mut message = format!(
        "symbol registry resolved `{}` to `{}` via {}; candidates={}; scip_available={}; speculative={}",
        token_use.token,
        symbol.qualified_name,
        resolution.strategy,
        resolution.candidates,
        scip_available,
        resolution.speculative
    );
    if let Some(reason) = &resolution.ambiguity_reason {
        message.push_str("; ambiguity: ");
        message.push_str(reason);
    }
    Some(AnalysisFact {
        id: identity::stable_hash(&format!(
            "symbol-registry:{}:{}:{}:{}",
            chunk.id, token_use.token, token_use.line, symbol.id.0
        )),
        file_id: chunk.file_id.clone(),
        symbol_id: chunk.symbol_id.clone(),
        target: symbol.qualified_name.clone(),
        target_kind: graph_node_type(symbol),
        edge_type,
        range: Some(open_kioku_core::LineRange::single(
            chunk
                .range
                .start
                .saturating_add(token_use.line)
                .saturating_sub(1),
        )),
        confidence: resolution.confidence,
        source: format!("open-kioku-symbol-registry/{}", resolution.strategy),
        source_type: EvidenceSourceType::StaticAnalysis,
        message,
    })
}

fn quality_note(token: &str, resolution: &Resolution) -> Option<String> {
    resolution.ambiguity_reason.as_ref().map(|reason| {
        format!(
            "symbol registry caveat for `{token}` via {}: {reason}",
            resolution.strategy
        )
    })
}

fn token_uses(text: &str) -> Vec<TokenUse> {
    let mut uses = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let mut current = String::new();
        let mut token_end = 0usize;
        for (idx, ch) in line.char_indices() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                current.push(ch);
                token_end = idx + ch.len_utf8();
            } else if !current.is_empty() {
                push_token_use(&mut uses, &current, line, token_end, line_index);
                current.clear();
            }
        }
        if !current.is_empty() {
            push_token_use(&mut uses, &current, line, token_end, line_index);
        }
    }
    uses
}

fn push_token_use(
    uses: &mut Vec<TokenUse>,
    token: &str,
    line: &str,
    token_end: usize,
    line_index: usize,
) {
    if is_keyword_or_literal(token) || token.len() < 2 {
        return;
    }
    let is_call = line[token_end..]
        .chars()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == '(');
    uses.push(TokenUse {
        token: token.to_string(),
        line: line_index as u32 + 1,
        is_call,
    });
}

fn symbol_matches_token(symbol: &Symbol, token: &str) -> bool {
    symbol.name == token || symbol.qualified_name.ends_with(&format!("::{token}"))
}

fn import_mentions_token(import: &ImportResolution, token: &str) -> bool {
    import
        .import
        .imported
        .rsplit(['/', '.', ':'])
        .next()
        .is_some_and(|last| last == token)
}

fn module_name(qualified_name: &str) -> String {
    qualified_name
        .rsplit_once("::")
        .map(|(module, _)| module.to_string())
        .unwrap_or_default()
}

fn graph_node_type(symbol: &Symbol) -> GraphNodeType {
    match symbol.kind {
        SymbolKind::Class => GraphNodeType::Class,
        SymbolKind::Trait => GraphNodeType::Trait,
        SymbolKind::Interface => GraphNodeType::Interface,
        SymbolKind::Method => GraphNodeType::Method,
        SymbolKind::Field => GraphNodeType::Field,
        SymbolKind::Endpoint => GraphNodeType::Endpoint,
        SymbolKind::DatabaseTable => GraphNodeType::DatabaseTable,
        SymbolKind::Test => GraphNodeType::Test,
        SymbolKind::Module | SymbolKind::Package => GraphNodeType::Module,
        _ => GraphNodeType::Function,
    }
}

fn symbol_rank(symbol: &Symbol) -> (u8, usize) {
    let kind_rank = match symbol.kind {
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface => 0,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Endpoint => 1,
        SymbolKind::Module | SymbolKind::Package => 2,
        SymbolKind::Variable | SymbolKind::Constant | SymbolKind::Field => 3,
        SymbolKind::DatabaseTable | SymbolKind::Test | SymbolKind::Unknown => 4,
    };
    (kind_rank, symbol.qualified_name.len())
}

fn is_keyword_or_literal(token: &str) -> bool {
    matches!(
        token,
        "if" | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "let"
            | "const"
            | "var"
            | "function"
            | "fn"
            | "class"
            | "struct"
            | "enum"
            | "trait"
            | "interface"
            | "impl"
            | "pub"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "new"
            | "true"
            | "false"
            | "null"
            | "None"
            | "Some"
            | "Ok"
            | "Err"
            | "self"
            | "this"
            | "super"
            | "crate"
            | "import"
            | "from"
            | "use"
            | "package"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Import, LineRange};

    fn symbol(id: &str, file: &str, name: &str, qualified: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified.into(),
            kind,
            file_id: FileId::new(file),
            range: Some(LineRange::single(1)),
            language: open_kioku_core::Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn chunk(id: &str, file: &str, symbol_id: Option<&str>, text: &str) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file_id: FileId::new(file),
            range: LineRange::single(1),
            language: open_kioku_core::Language::TypeScript,
            text: text.into(),
            symbol_id: symbol_id.map(SymbolId::new),
        }
    }

    fn import_resolution(file: &str, imported: &str, target_file: &str) -> ImportResolution {
        ImportResolution {
            import: Import {
                file_id: FileId::new(file),
                imported: imported.into(),
                range: Some(LineRange::single(1)),
                confidence: Confidence::Medium,
            },
            status: ResolutionStatus::Resolved,
            target_file: Some(FileId::new(target_file)),
            target_symbol: None,
            confidence: Confidence::High,
            strategy: "test-import".into(),
            caveats: vec![],
        }
    }

    #[test]
    fn direct_import_resolves_call() {
        let symbols = vec![
            symbol("caller", "entry", "main", "src::main", SymbolKind::Function),
            symbol(
                "target",
                "util",
                "helper",
                "src::util::helper",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "helper();")],
            &symbols,
            &[import_resolution("entry", "./util", "util")],
            false,
        );
        let fact = report
            .analysis_facts
            .iter()
            .find(|fact| fact.target == "src::util::helper")
            .unwrap();
        assert_eq!(fact.edge_type, GraphEdgeType::Calls);
        assert_eq!(fact.confidence, Confidence::High);
        assert!(fact.source.contains("direct-import"));
    }

    #[test]
    fn same_file_and_same_module_resolution() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "local",
                "entry",
                "local",
                "app::local",
                SymbolKind::Function,
            ),
            symbol(
                "neighbor",
                "other",
                "neighbor",
                "app::neighbor",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "local(); neighbor();")],
            &symbols,
            &[],
            false,
        );
        assert!(report
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "app::local" && fact.source.contains("same-file")));
        assert!(report
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "app::neighbor" && fact.source.contains("same-module")));
    }

    #[test]
    fn unique_project_name_is_medium_confidence() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "unique",
                "other",
                "unique",
                "lib::unique",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "unique();")],
            &symbols,
            &[],
            false,
        );
        let fact = report
            .analysis_facts
            .iter()
            .find(|fact| fact.target == "lib::unique")
            .unwrap();
        assert_eq!(fact.confidence, Confidence::Medium);
        assert!(fact.message.contains("speculative=true"));
    }

    #[test]
    fn suffix_ambiguity_and_common_name_caps_surface_caveats() {
        let mut symbols = vec![symbol(
            "caller",
            "entry",
            "main",
            "app::main",
            SymbolKind::Function,
        )];
        for index in 0..(COMMON_NAME_CAP + 1) {
            symbols.push(symbol(
                &format!("common-{index}"),
                &format!("file-{index}"),
                "render",
                &format!("pkg{index}::render"),
                SymbolKind::Function,
            ));
        }
        symbols.push(symbol(
            "amb-a",
            "a",
            "Session",
            "pkg::a::Session",
            SymbolKind::Class,
        ));
        symbols.push(symbol(
            "amb-b",
            "b",
            "Session",
            "pkg::b::Session",
            SymbolKind::Class,
        ));
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "render(); Session;")],
            &symbols,
            &[],
            false,
        );
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.contains("common name `render`")));
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.contains("2 candidates matched")));
    }

    #[test]
    fn unresolved_calls_surface_low_confidence_notes() {
        let symbols = vec![symbol(
            "caller",
            "entry",
            "main",
            "app::main",
            SymbolKind::Function,
        )];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "missingCall();")],
            &symbols,
            &[],
            false,
        );
        assert!(report.analysis_facts.is_empty());
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.contains("missingCall")));
    }

    #[test]
    fn scip_availability_is_recorded_without_claiming_exactness() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "other",
                "target",
                "lib::target",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "target();")],
            &symbols,
            &[],
            true,
        );
        let fact = report.analysis_facts.first().unwrap();
        assert_eq!(fact.confidence, Confidence::Medium);
        assert!(fact.message.contains("scip_available=true"));
    }
}
