use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, IndexMode, LineRange, Symbol, SymbolId, SymbolKind,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const MAX_SYMBOLS_FOR_PAIRWISE: usize = 400;
const MAX_TRANSITIVE_CALL_DEPTH: usize = 8;
const SIMILARITY_THRESHOLD: f32 = 0.62;
const SEMANTIC_THRESHOLD: f32 = 0.55;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ComplexityMetrics {
    cyclomatic: u32,
    cognitive: u32,
    loop_count: u32,
    max_loop_depth: u32,
    transitive_loop_depth: u32,
    recursive: bool,
    linear_scan_in_loop: bool,
    allocation_in_loop: bool,
    recursion_in_loop: bool,
    unguarded_recursion: bool,
    parameter_count: u32,
    max_access_depth: u32,
    blocking_network_db_call_count: u32,
    cap_hit: bool,
}

#[derive(Debug, Clone)]
struct SymbolText<'a> {
    symbol: &'a Symbol,
    text: String,
    tokens: Vec<String>,
    shingles: BTreeSet<String>,
}

pub fn collect_relationship_analysis_facts(
    files: &[File],
    symbols: &[Symbol],
    chunks: &[CodeChunk],
    existing_facts: &[AnalysisFact],
    mode: IndexMode,
    semantic: &SemanticConfig,
) -> Vec<AnalysisFact> {
    let files_by_id = files
        .iter()
        .map(|file| (file.id.clone(), file))
        .collect::<HashMap<_, _>>();
    let symbol_texts = symbol_texts(symbols, chunks);
    let calls = calls_by_symbol(existing_facts, symbols);
    let mut metrics = symbol_texts
        .iter()
        .map(|entry| {
            (
                entry.symbol.id.clone(),
                compute_complexity(entry.symbol, &entry.text),
            )
        })
        .collect::<BTreeMap<_, _>>();
    apply_transitive_loop_depth(&mut metrics, &calls);

    let mut facts = Vec::new();
    for entry in &symbol_texts {
        if let Some(file) = files_by_id.get(&entry.symbol.file_id) {
            if let Some(metric) = metrics.get(&entry.symbol.id) {
                facts.push(complexity_fact(file.id.clone(), entry.symbol, metric));
            }
        }
    }

    if mode != IndexMode::Fast {
        facts.extend(similarity_facts(&symbol_texts, &files_by_id));
        facts.extend(semantic_facts(&symbol_texts, &files_by_id, semantic));
    }

    dedupe_analysis_facts(facts)
}

fn symbol_texts<'a>(symbols: &'a [Symbol], chunks: &[CodeChunk]) -> Vec<SymbolText<'a>> {
    let mut chunks_by_symbol: HashMap<SymbolId, Vec<&CodeChunk>> = HashMap::new();
    let mut chunks_by_file: HashMap<FileId, Vec<&CodeChunk>> = HashMap::new();
    for chunk in chunks {
        chunks_by_file
            .entry(chunk.file_id.clone())
            .or_default()
            .push(chunk);
        if let Some(symbol_id) = &chunk.symbol_id {
            chunks_by_symbol
                .entry(symbol_id.clone())
                .or_default()
                .push(chunk);
        }
    }

    let mut entries = Vec::new();
    for symbol in symbols.iter().filter(|symbol| {
        matches!(
            symbol.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
        )
    }) {
        let mut selected = chunks_by_symbol
            .get(&symbol.id)
            .cloned()
            .unwrap_or_else(|| chunks_for_symbol_range(symbol, &chunks_by_file));
        selected.sort_by_key(|chunk| (chunk.range.start, chunk.range.end, chunk.id.clone()));
        let text = selected
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            continue;
        }
        let tokens = normalize_tokens(&text);
        let shingles = structural_shingles(&tokens, 4);
        entries.push(SymbolText {
            symbol,
            text,
            tokens,
            shingles,
        });
    }
    entries.sort_by(|a, b| a.symbol.id.0.cmp(&b.symbol.id.0));
    entries.truncate(MAX_SYMBOLS_FOR_PAIRWISE);
    entries
}

fn chunks_for_symbol_range<'a>(
    symbol: &Symbol,
    chunks_by_file: &HashMap<FileId, Vec<&'a CodeChunk>>,
) -> Vec<&'a CodeChunk> {
    let Some(range) = &symbol.range else {
        return chunks_by_file
            .get(&symbol.file_id)
            .and_then(|chunks| chunks.first().copied().map(|chunk| vec![chunk]))
            .unwrap_or_default();
    };
    chunks_by_file
        .get(&symbol.file_id)
        .into_iter()
        .flatten()
        .filter(|chunk| ranges_overlap(&chunk.range, range))
        .copied()
        .collect()
}

fn ranges_overlap(left: &LineRange, right: &LineRange) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn compute_complexity(symbol: &Symbol, text: &str) -> ComplexityMetrics {
    let lexical = lexical_tokens(text);
    let lower = text.to_ascii_lowercase();
    let cyclomatic_keywords = [
        "if", "for", "while", "loop", "match", "case", "catch", "except", "elif", "&&", "||", "?",
    ];
    let cyclomatic = 1 + lexical
        .iter()
        .filter(|token| cyclomatic_keywords.contains(&token.as_str()))
        .count() as u32;
    let loop_count = lexical
        .iter()
        .filter(|token| matches!(token.as_str(), "for" | "while" | "loop"))
        .count() as u32;
    let max_loop_depth = max_loop_depth(&lexical);
    let recursive = calls_name(text, &symbol.name);
    let loop_segments = loop_segments(text);
    let linear_scan_in_loop = loop_segments.iter().any(|segment| {
        contains_any(
            segment,
            &[".contains(", ".find(", ".position(", ".iter().find"],
        )
    });
    let allocation_in_loop = loop_segments.iter().any(|segment| {
        contains_any(
            segment,
            &[
                "vec::new",
                "string::new",
                "box::new",
                "format!(",
                ".collect(",
                "new ",
            ],
        )
    });
    let recursion_in_loop = recursive
        && loop_segments
            .iter()
            .any(|segment| calls_name(segment, &symbol.name));
    let unguarded_recursion =
        recursive && !contains_any(&lower, &["if ", "match ", "return", "break"]);
    let parameter_count = parameter_count(text);
    let max_access_depth = max_access_depth(text);
    let blocking_network_db_call_count = count_any(
        &lower,
        &[
            "sleep(",
            "block_on",
            ".await",
            "reqwest",
            "http::",
            "fetch(",
            ".execute(",
            ".query(",
            "select ",
            "insert ",
            "update ",
            "delete ",
        ],
    );
    let cognitive = cyclomatic
        .saturating_add(max_loop_depth)
        .saturating_add(if recursive { 2 } else { 0 })
        .saturating_add(blocking_network_db_call_count);

    ComplexityMetrics {
        cyclomatic,
        cognitive,
        loop_count,
        max_loop_depth,
        transitive_loop_depth: max_loop_depth,
        recursive,
        linear_scan_in_loop,
        allocation_in_loop,
        recursion_in_loop,
        unguarded_recursion,
        parameter_count,
        max_access_depth,
        blocking_network_db_call_count,
        cap_hit: false,
    }
}

fn calls_by_symbol(
    facts: &[AnalysisFact],
    symbols: &[Symbol],
) -> HashMap<SymbolId, BTreeSet<SymbolId>> {
    let by_qualified_name = symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.id.clone()))
        .collect::<HashMap<_, _>>();
    let mut calls: HashMap<SymbolId, BTreeSet<SymbolId>> = HashMap::new();
    for fact in facts
        .iter()
        .filter(|fact| fact.edge_type == GraphEdgeType::Calls)
    {
        let Some(source) = &fact.symbol_id else {
            continue;
        };
        let Some(target) = by_qualified_name.get(fact.target.as_str()) else {
            continue;
        };
        calls
            .entry(source.clone())
            .or_default()
            .insert(target.clone());
    }
    calls
}

fn apply_transitive_loop_depth(
    metrics: &mut BTreeMap<SymbolId, ComplexityMetrics>,
    calls: &HashMap<SymbolId, BTreeSet<SymbolId>>,
) {
    let ids = metrics.keys().cloned().collect::<Vec<_>>();
    for id in ids {
        let mut visiting = Vec::new();
        let (depth, recursive, cap_hit) =
            transitive_loop_depth(&id, metrics, calls, 0, &mut visiting);
        if let Some(metric) = metrics.get_mut(&id) {
            metric.transitive_loop_depth = metric.transitive_loop_depth.max(depth);
            metric.recursive |= recursive;
            metric.cap_hit |= cap_hit;
        }
    }
}

fn transitive_loop_depth(
    id: &SymbolId,
    metrics: &BTreeMap<SymbolId, ComplexityMetrics>,
    calls: &HashMap<SymbolId, BTreeSet<SymbolId>>,
    depth: usize,
    visiting: &mut Vec<SymbolId>,
) -> (u32, bool, bool) {
    if depth >= MAX_TRANSITIVE_CALL_DEPTH {
        return (
            metrics
                .get(id)
                .map(|metric| metric.max_loop_depth)
                .unwrap_or_default(),
            false,
            true,
        );
    }
    if visiting.iter().any(|seen| seen == id) {
        return (
            metrics
                .get(id)
                .map(|metric| metric.max_loop_depth)
                .unwrap_or_default(),
            true,
            false,
        );
    }
    visiting.push(id.clone());
    let mut max_depth = metrics
        .get(id)
        .map(|metric| metric.max_loop_depth)
        .unwrap_or_default();
    let mut recursive = false;
    let mut cap_hit = false;
    if let Some(targets) = calls.get(id) {
        for target in targets {
            let (target_depth, target_recursive, target_cap_hit) =
                transitive_loop_depth(target, metrics, calls, depth + 1, visiting);
            max_depth = max_depth.max(target_depth);
            recursive |= target_recursive;
            cap_hit |= target_cap_hit;
        }
    }
    visiting.pop();
    (max_depth, recursive, cap_hit)
}

fn complexity_fact(file_id: FileId, symbol: &Symbol, metrics: &ComplexityMetrics) -> AnalysisFact {
    let risk = if metrics.cyclomatic >= 12
        || metrics.cognitive >= 16
        || metrics.transitive_loop_depth >= 3
        || metrics.recursion_in_loop
        || metrics.unguarded_recursion
        || metrics.blocking_network_db_call_count >= 3
    {
        "high"
    } else if metrics.cyclomatic >= 6
        || metrics.cognitive >= 8
        || metrics.loop_count >= 2
        || metrics.max_loop_depth >= 2
        || metrics.linear_scan_in_loop
        || metrics.allocation_in_loop
        || metrics.blocking_network_db_call_count > 0
    {
        "medium"
    } else {
        "low"
    };
    let caveat = if metrics.cap_hit {
        "; caveat=transitive call-depth cap hit; risk signal, not proof of complexity"
    } else {
        "; caveat=risk signal, not proof of complexity"
    };
    AnalysisFact {
        id: identity::stable_hash(&format!("relationship-complexity:{}", symbol.id.0)),
        file_id,
        symbol_id: Some(symbol.id.clone()),
        target: format!("complexity:{}", symbol.qualified_name),
        target_kind: GraphNodeType::Resource,
        edge_type: GraphEdgeType::BelongsTo,
        range: symbol.range.clone(),
        confidence: Confidence::Medium,
        source: "open-kioku-relationships:complexity".into(),
        source_type: EvidenceSourceType::StaticAnalysis,
        message: format!(
            "complexity_risk={risk}; cyclomatic={}; cognitive={}; loop_count={}; max_loop_depth={}; transitive_loop_depth={}; recursive={}; linear_scan_in_loop={}; allocation_in_loop={}; recursion_in_loop={}; unguarded_recursion={}; parameter_count={}; max_access_depth={}; blocking_network_db_call_count={}{}",
            metrics.cyclomatic,
            metrics.cognitive,
            metrics.loop_count,
            metrics.max_loop_depth,
            metrics.transitive_loop_depth,
            metrics.recursive,
            metrics.linear_scan_in_loop,
            metrics.allocation_in_loop,
            metrics.recursion_in_loop,
            metrics.unguarded_recursion,
            metrics.parameter_count,
            metrics.max_access_depth,
            metrics.blocking_network_db_call_count,
            caveat
        ),
    }
}

fn similarity_facts(
    entries: &[SymbolText<'_>],
    files_by_id: &HashMap<FileId, &File>,
) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    for (left_index, left) in entries.iter().enumerate() {
        if !files_by_id.contains_key(&left.symbol.file_id) || left.shingles.is_empty() {
            continue;
        }
        for right in entries.iter().skip(left_index + 1) {
            if left.symbol.file_id == right.symbol.file_id || right.shingles.is_empty() {
                continue;
            }
            let score = jaccard(&left.shingles, &right.shingles);
            if score < SIMILARITY_THRESHOLD {
                continue;
            }
            facts.push(AnalysisFact {
                id: identity::stable_hash(&format!(
                    "relationship-similar:{}:{}:{score:.3}",
                    left.symbol.id.0, right.symbol.id.0
                )),
                file_id: left.symbol.file_id.clone(),
                symbol_id: Some(left.symbol.id.clone()),
                target: right.symbol.qualified_name.clone(),
                target_kind: graph_node_type(right.symbol),
                edge_type: GraphEdgeType::SimilarTo,
                range: left.symbol.range.clone(),
                confidence: Confidence::Low,
                source: "open-kioku-relationships:structural-shingles".into(),
                source_type: EvidenceSourceType::Heuristic,
                message: format!(
                    "similarity score={score:.3}; method=normalized_ast_tokens+structural_shingles+jaccard; threshold={SIMILARITY_THRESHOLD:.2}; caveat=fuzzy evidence never outranks exact symbol, SCIP, test, history, runtime, or policy evidence"
                ),
            });
        }
    }
    facts
}

fn semantic_facts(
    entries: &[SymbolText<'_>],
    files_by_id: &HashMap<FileId, &File>,
    semantic: &SemanticConfig,
) -> Vec<AnalysisFact> {
    if !semantic.enabled || semantic.provider != "local" {
        return Vec::new();
    }
    let mut facts = Vec::new();
    for (left_index, left) in entries.iter().enumerate() {
        if !files_by_id.contains_key(&left.symbol.file_id) || left.tokens.is_empty() {
            continue;
        }
        for right in entries.iter().skip(left_index + 1) {
            if left.symbol.file_id == right.symbol.file_id || right.tokens.is_empty() {
                continue;
            }
            let score = token_overlap(&left.tokens, &right.tokens);
            if score < SEMANTIC_THRESHOLD {
                continue;
            }
            facts.push(AnalysisFact {
                id: identity::stable_hash(&format!(
                    "relationship-semantic:{}:{}:{score:.3}:{}:{}",
                    left.symbol.id.0, right.symbol.id.0, semantic.provider, semantic.model
                )),
                file_id: left.symbol.file_id.clone(),
                symbol_id: Some(left.symbol.id.clone()),
                target: right.symbol.qualified_name.clone(),
                target_kind: graph_node_type(right.symbol),
                edge_type: GraphEdgeType::SemanticallyRelated,
                range: left.symbol.range.clone(),
                confidence: Confidence::Low,
                source: "open-kioku-relationships:semantic-local".into(),
                source_type: EvidenceSourceType::Semantic,
                message: format!(
                    "semantic score={score:.3}; provider={}; model={}; version=relationship-pass-v1; local_only=true; threshold={SEMANTIC_THRESHOLD:.2}; caveat=fuzzy semantic evidence never outranks exact symbol, SCIP, test, history, runtime, or policy evidence",
                    semantic.provider,
                    semantic.model
                ),
            });
        }
    }
    facts
}

fn graph_node_type(symbol: &Symbol) -> GraphNodeType {
    match symbol.kind {
        SymbolKind::Class => GraphNodeType::Class,
        SymbolKind::Trait => GraphNodeType::Trait,
        SymbolKind::Interface => GraphNodeType::Interface,
        SymbolKind::Function => GraphNodeType::Function,
        SymbolKind::Method => GraphNodeType::Method,
        SymbolKind::Field => GraphNodeType::Field,
        SymbolKind::Endpoint => GraphNodeType::Endpoint,
        SymbolKind::DatabaseTable => GraphNodeType::DatabaseTable,
        SymbolKind::Test => GraphNodeType::Test,
        SymbolKind::Module => GraphNodeType::Module,
        SymbolKind::Package => GraphNodeType::Package,
        _ => GraphNodeType::Resource,
    }
}

fn lexical_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if matches!(ch, '{' | '}' | '(' | ')' | '?' | '&' | '|') {
                tokens.push(ch.to_string());
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn normalize_tokens(text: &str) -> Vec<String> {
    lexical_tokens(text)
        .into_iter()
        .filter(|token| !matches!(token.as_str(), "{" | "}" | "(" | ")"))
        .map(|token| {
            if token.chars().all(|ch| ch.is_ascii_digit()) {
                "lit".into()
            } else if is_structural_keyword(&token) {
                token
            } else if matches!(token.as_str(), "&&" | "||" | "?" | "&" | "|") {
                "op".into()
            } else {
                "id".into()
            }
        })
        .collect()
}

fn is_structural_keyword(token: &str) -> bool {
    matches!(
        token,
        "fn" | "function"
            | "def"
            | "if"
            | "else"
            | "match"
            | "case"
            | "for"
            | "while"
            | "loop"
            | "return"
            | "await"
            | "async"
            | "try"
            | "catch"
            | "map"
            | "filter"
            | "fold"
            | "collect"
            | "select"
            | "insert"
            | "update"
            | "delete"
    )
}

fn structural_shingles(tokens: &[String], width: usize) -> BTreeSet<String> {
    if tokens.len() < width {
        return tokens.iter().cloned().collect();
    }
    tokens
        .windows(width)
        .map(|window| window.join(" "))
        .collect::<BTreeSet<_>>()
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f32;
    let union = left.union(right).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn token_overlap(left: &[String], right: &[String]) -> f32 {
    let left = left.iter().collect::<BTreeSet<_>>();
    let right = right.iter().collect::<BTreeSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f32;
    let min_len = left.len().min(right.len()) as f32;
    intersection / min_len
}

fn max_loop_depth(tokens: &[String]) -> u32 {
    let mut pending_loop = false;
    let mut loop_stack: Vec<usize> = Vec::new();
    let mut brace_depth = 0usize;
    let mut max_depth = 0u32;
    for token in tokens {
        match token.as_str() {
            "for" | "while" | "loop" => pending_loop = true,
            "{" => {
                brace_depth += 1;
                if pending_loop {
                    loop_stack.push(brace_depth);
                    max_depth = max_depth.max(loop_stack.len() as u32);
                    pending_loop = false;
                }
            }
            "}" => {
                loop_stack.retain(|loop_depth| *loop_depth < brace_depth);
                brace_depth = brace_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    max_depth
}

fn loop_segments(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut segments = Vec::new();
    for marker in ["for ", "while ", "loop "] {
        let mut offset = 0;
        while let Some(index) = lower[offset..].find(marker) {
            let start = offset + index;
            let end = (start + 500).min(lower.len());
            segments.push(lower[start..end].to_string());
            offset = start + marker.len();
        }
    }
    segments
}

fn calls_name(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let direct_compact = format!("{name}(");
    let direct_spaced = format!("{name} (");
    let method = format!(".{name}(");
    lower.matches(&direct_compact).count() + lower.matches(&direct_spaced).count() > 1
        || lower.contains(&method)
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn count_any(text: &str, needles: &[&str]) -> u32 {
    needles
        .iter()
        .map(|needle| text.matches(needle).count() as u32)
        .sum()
}

fn parameter_count(text: &str) -> u32 {
    let Some(open) = text.find('(') else {
        return 0;
    };
    let Some(close) = text[open + 1..].find(')') else {
        return 0;
    };
    let params = &text[open + 1..open + 1 + close];
    let params = params.trim();
    if params.is_empty() {
        return 0;
    }
    params
        .split(',')
        .map(str::trim)
        .filter(|param| !param.is_empty() && *param != "self" && *param != "&self")
        .count() as u32
}

fn max_access_depth(text: &str) -> u32 {
    let mut max_depth = 0u32;
    let mut current = 0u32;
    for ch in text.chars() {
        if ch == '.' {
            current += 1;
            max_depth = max_depth.max(current);
        } else if !(ch.is_ascii_alphanumeric() || ch == '_') {
            current = 0;
        }
    }
    max_depth
}

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts.sort_by(|left, right| left.id.cmp(&right.id));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Language, RepositoryId};
    use std::path::PathBuf;

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(id: &str, file_id: &str, name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("crate::{name}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file_id),
            range: Some(LineRange { start: 1, end: 20 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn chunk(id: &str, file_id: &str, symbol_id: &str, text: &str) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file_id: FileId::new(file_id),
            range: LineRange { start: 1, end: 20 },
            language: Language::Rust,
            text: text.into(),
            symbol_id: Some(SymbolId::new(symbol_id)),
        }
    }

    fn semantic_config(enabled: bool) -> SemanticConfig {
        SemanticConfig {
            enabled,
            backend: "exact-flat".into(),
            provider: "local".into(),
            model: "local-hash".into(),
            dimensions: 384,
            distance: "cosine".into(),
            batch_size: 64,
            index_symbols: true,
            index_chunks: true,
            index_docs: true,
            index_memory: true,
            external_provider_allowed: false,
        }
    }

    #[test]
    fn computes_complexity_metrics() {
        let sym = symbol("a", "a", "scan");
        let metrics = compute_complexity(
            &sym,
            "fn scan(values: Vec<i32>, needle: i32) { for value in values { if values.contains(&needle) { Vec::new(); return; } } }",
        );

        assert!(metrics.cyclomatic >= 3);
        assert_eq!(metrics.loop_count, 1);
        assert!(metrics.linear_scan_in_loop);
        assert!(metrics.allocation_in_loop);
        assert!(!metrics.recursive);
        assert_eq!(metrics.parameter_count, 2);
    }

    #[test]
    fn detects_recursive_call_cycle_from_calls_edges() {
        let symbols = vec![symbol("a", "a", "a"), symbol("b", "b", "b")];
        let files = vec![file("a", "src/a.rs"), file("b", "src/b.rs")];
        let chunks = vec![
            chunk("a", "a", "a", "fn a() { b(); }"),
            chunk("b", "b", "b", "fn b() { for item in items { a(); } }"),
        ];
        let calls = vec![
            AnalysisFact {
                id: "a-calls-b".into(),
                file_id: FileId::new("a"),
                symbol_id: Some(SymbolId::new("a")),
                target: "crate::b".into(),
                target_kind: GraphNodeType::Function,
                edge_type: GraphEdgeType::Calls,
                range: None,
                confidence: Confidence::Medium,
                source: "test".into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                message: String::new(),
            },
            AnalysisFact {
                id: "b-calls-a".into(),
                file_id: FileId::new("b"),
                symbol_id: Some(SymbolId::new("b")),
                target: "crate::a".into(),
                target_kind: GraphNodeType::Function,
                edge_type: GraphEdgeType::Calls,
                range: None,
                confidence: Confidence::Medium,
                source: "test".into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                message: String::new(),
            },
        ];

        let facts = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &calls,
            IndexMode::Full,
            &semantic_config(false),
        );
        let complexity = facts
            .iter()
            .find(|fact| {
                fact.source == "open-kioku-relationships:complexity"
                    && fact.symbol_id == Some(SymbolId::new("a"))
            })
            .unwrap();

        assert!(complexity.message.contains("recursive=true"));
        assert!(complexity.message.contains("transitive_loop_depth=1"));
    }

    #[test]
    fn propagates_nested_loop_depth_over_calls() {
        let symbols = vec![symbol("a", "a", "a"), symbol("b", "b", "b")];
        let files = vec![file("a", "src/a.rs"), file("b", "src/b.rs")];
        let chunks = vec![
            chunk("a", "a", "a", "fn a() { b(); }"),
            chunk(
                "b",
                "b",
                "b",
                "fn b() { for x in xs { while ready { loop { work(); } } } }",
            ),
        ];
        let calls = vec![AnalysisFact {
            id: "a-calls-b".into(),
            file_id: FileId::new("a"),
            symbol_id: Some(SymbolId::new("a")),
            target: "crate::b".into(),
            target_kind: GraphNodeType::Function,
            edge_type: GraphEdgeType::Calls,
            range: None,
            confidence: Confidence::Medium,
            source: "test".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: String::new(),
        }];

        let facts = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &calls,
            IndexMode::Full,
            &semantic_config(false),
        );
        let complexity = facts
            .iter()
            .find(|fact| {
                fact.symbol_id == Some(SymbolId::new("a")) && fact.source.ends_with(":complexity")
            })
            .unwrap();

        assert!(complexity.message.contains("transitive_loop_depth=3"));
    }

    #[test]
    fn emits_similarity_for_true_positive_and_skips_fast_mode() {
        let files = vec![file("a", "src/a.rs"), file("b", "src/b.rs")];
        let symbols = vec![
            symbol("a", "a", "sum_orders"),
            symbol("b", "b", "sum_invoices"),
        ];
        let chunks = vec![
            chunk("a", "a", "a", "fn sum_orders(items: Vec<i32>) -> i32 { let mut total = 0; for item in items { if item > 0 { total += item; } } total }"),
            chunk("b", "b", "b", "fn sum_invoices(rows: Vec<i32>) -> i32 { let mut amount = 0; for row in rows { if row > 0 { amount += row; } } amount }"),
        ];

        let full = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &[],
            IndexMode::Full,
            &semantic_config(false),
        );
        assert!(full
            .iter()
            .any(|fact| fact.edge_type == GraphEdgeType::SimilarTo));

        let fast = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &[],
            IndexMode::Fast,
            &semantic_config(false),
        );
        assert!(!fast
            .iter()
            .any(|fact| fact.edge_type == GraphEdgeType::SimilarTo));
    }

    #[test]
    fn similarity_threshold_filters_false_positive() {
        let files = vec![file("a", "src/a.rs"), file("b", "src/b.rs")];
        let symbols = vec![symbol("a", "a", "parse"), symbol("b", "b", "render")];
        let chunks = vec![
            chunk("a", "a", "a", "fn parse(input: &str) { match input { \"a\" => return, _ => panic!() } }"),
            chunk("b", "b", "b", "fn render(template: Html) { let socket = connect(); socket.write(template); socket.flush(); }"),
        ];

        let facts = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &[],
            IndexMode::Full,
            &semantic_config(false),
        );

        assert!(!facts
            .iter()
            .any(|fact| fact.edge_type == GraphEdgeType::SimilarTo));
    }

    #[test]
    fn semantic_relationships_are_disabled_by_default() {
        let files = vec![file("a", "src/a.rs"), file("b", "src/b.rs")];
        let symbols = vec![
            symbol("a", "a", "sum_orders"),
            symbol("b", "b", "sum_invoices"),
        ];
        let chunks = vec![
            chunk(
                "a",
                "a",
                "a",
                "fn sum_orders(items: Vec<i32>) -> i32 { items.iter().sum() }",
            ),
            chunk(
                "b",
                "b",
                "b",
                "fn sum_invoices(rows: Vec<i32>) -> i32 { rows.iter().sum() }",
            ),
        ];

        let facts = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &[],
            IndexMode::Full,
            &semantic_config(false),
        );

        assert!(!facts
            .iter()
            .any(|fact| fact.edge_type == GraphEdgeType::SemanticallyRelated));
    }
}
