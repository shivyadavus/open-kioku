use chrono::{DateTime, Utc};
use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, LineRange, Symbol, SymbolId,
};
use open_kioku_errors::{OkError, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_RUNTIME_ARTIFACT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RUNTIME_RECORDS: usize = 10_000;
const MAX_SAMPLE_MESSAGES: usize = 3;
const STALE_RUNTIME_DAYS: i64 = 30;

#[derive(Debug, Clone)]
struct RuntimeRecord {
    file_id: FileId,
    file_path: PathBuf,
    symbol_id: Option<SymbolId>,
    line: Option<u32>,
    service: Option<String>,
    route: Option<String>,
    method: Option<String>,
    status_code: Option<u16>,
    duration_ms: Option<f64>,
    error: bool,
    timestamp: Option<String>,
    sql_statement: Option<String>,
    message: Option<String>,
    artifact: PathBuf,
    artifact_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct RuntimeAggregate {
    key: String,
    target: String,
    target_kind: GraphNodeType,
    edge_type: GraphEdgeType,
    file_id: FileId,
    symbol_id: Option<SymbolId>,
    count: usize,
    error_count: usize,
    error_rate: f32,
    p50_ms: Option<f64>,
    p95_ms: Option<f64>,
    p99_ms: Option<f64>,
    first_observed: Option<String>,
    last_observed: Option<String>,
    services: Vec<String>,
    status_codes: Vec<u16>,
    sample_messages: Vec<String>,
    confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeAggregateKind {
    Route,
    Table,
    Error,
    Symbol,
}

pub fn collect_runtime_analysis_facts(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
) -> Result<Vec<AnalysisFact>> {
    let files_by_path = files
        .iter()
        .map(|file| (normalize_path(&file.path.to_string_lossy()), file))
        .collect::<HashMap<_, _>>();
    let symbols_by_file =
        symbols
            .iter()
            .fold(HashMap::<FileId, Vec<&Symbol>>::new(), |mut acc, symbol| {
                acc.entry(symbol.file_id.clone()).or_default().push(symbol);
                acc
            });

    let mut records = Vec::new();
    let mut facts = Vec::new();
    for runtime_root in runtime_roots(root) {
        if !runtime_root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&runtime_root)
            .max_depth(3)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if records.len() >= MAX_RUNTIME_RECORDS {
                return Ok(dedupe_analysis_facts(add_aggregate_facts(facts, &records)));
            }
            if !entry.file_type().is_file() || !is_runtime_jsonl(entry.path()) {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > MAX_RUNTIME_ARTIFACT_BYTES {
                continue;
            }
            let content = fs::read_to_string(entry.path())?;
            for (idx, line) in content.lines().enumerate() {
                if records.len() >= MAX_RUNTIME_RECORDS {
                    return Ok(dedupe_analysis_facts(add_aggregate_facts(facts, &records)));
                }
                let Some(record) = parse_runtime_record(
                    root,
                    entry.path(),
                    idx + 1,
                    line,
                    &files_by_path,
                    &symbols_by_file,
                ) else {
                    continue;
                };
                facts.extend(record_facts(&record));
                records.push(record);
            }
        }
    }
    Ok(dedupe_analysis_facts(add_aggregate_facts(facts, &records)))
}

fn runtime_roots(root: &Path) -> [PathBuf; 3] {
    [
        root.join(".ok/runtime"),
        root.join(".ok/analysis/runtime"),
        root.join(".ok/analysis"),
    ]
}

fn is_runtime_jsonl(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower_name = file_name.to_ascii_lowercase();
    lower_name.ends_with(".jsonl")
        && [
            "span", "trace", "runtime", "otel", "log", "incident", "error", "failure",
        ]
        .iter()
        .any(|needle| lower_name.contains(needle))
}

fn parse_runtime_record(
    root: &Path,
    artifact: &Path,
    artifact_line: usize,
    line: &str,
    files_by_path: &HashMap<String, &File>,
    symbols_by_file: &HashMap<FileId, Vec<&Symbol>>,
) -> Option<RuntimeRecord> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = serde_json::from_str::<Value>(trimmed).ok()?;
    let source_file = json_string(
        &value,
        &[
            "file",
            "code.filepath",
            "source.file",
            "code.file.path",
            "source.path",
        ],
    )?;
    let normalized = normalize_runtime_file(root, &source_file);
    let file = files_by_path.get(&normalized).copied()?;
    let symbol_name = json_string(&value, &["symbol", "code.function", "function", "name"]);
    let symbol_id = symbol_name
        .as_deref()
        .and_then(|name| resolve_symbol(symbols_by_file.get(&file.id)?, name));
    let status_code = json_u64(
        &value,
        &[
            "http.response.status_code",
            "http.status_code",
            "status_code",
            "status",
        ],
    )
    .and_then(|value| u16::try_from(value).ok());
    let error = json_bool(&value, &["error", "error.flag", "exception", "failed"]).unwrap_or(false)
        || status_code.map(|code| code >= 500).unwrap_or(false)
        || json_string(
            &value,
            &[
                "error.message",
                "exception.message",
                "span.status.message",
                "failure.message",
            ],
        )
        .is_some();
    Some(RuntimeRecord {
        file_id: file.id.clone(),
        file_path: file.path.clone(),
        symbol_id,
        line: json_u64(&value, &["line", "code.lineno", "source.line"])
            .and_then(|value| u32::try_from(value).ok()),
        service: json_string(&value, &["service", "service.name", "service_name"]),
        route: json_string(
            &value,
            &[
                "http.route",
                "http.target",
                "url.path",
                "route",
                "span.name",
            ],
        )
        .filter(|route| route.contains('/')),
        method: json_string(
            &value,
            &[
                "http.request.method",
                "http.method",
                "method",
                "request.method",
            ],
        )
        .map(|method| method.to_ascii_uppercase()),
        status_code,
        duration_ms: json_f64(
            &value,
            &[
                "duration_ms",
                "duration.ms",
                "http.server.duration",
                "elapsed_ms",
                "latency_ms",
            ],
        )
        .or_else(|| {
            json_f64(&value, &["duration", "duration_ns"]).map(|duration| {
                if duration > 1_000_000.0 {
                    duration / 1_000_000.0
                } else {
                    duration
                }
            })
        }),
        error,
        timestamp: json_string(
            &value,
            &[
                "timestamp",
                "time",
                "@timestamp",
                "observed_at",
                "start_time",
            ],
        ),
        sql_statement: json_string(&value, &["db.statement", "sql", "database.statement"]),
        message: json_string(
            &value,
            &[
                "error.message",
                "exception.message",
                "log.message",
                "message",
                "event.message",
                "span.status.message",
                "failure.message",
            ],
        )
        .and_then(|message| compact_runtime_message(&message)),
        artifact: artifact.to_path_buf(),
        artifact_line,
    })
}

fn resolve_symbol(symbols: &[&Symbol], name: &str) -> Option<SymbolId> {
    symbols
        .iter()
        .find(|symbol| {
            symbol.name == name
                || symbol.qualified_name == name
                || symbol.qualified_name.ends_with(name)
        })
        .map(|symbol| symbol.id.clone())
}

fn record_facts(record: &RuntimeRecord) -> Vec<AnalysisFact> {
    let mut facts = Vec::new();
    if let Some(route) = &record.route {
        let method = record.method.as_deref().unwrap_or("HTTP");
        facts.push(runtime_fact(
            record,
            GraphEdgeType::ExposesEndpoint,
            GraphNodeType::Endpoint,
            format!("{method} {route}"),
            "runtime endpoint observed in local trace artifact",
            Confidence::High,
            "record",
        ));
    }
    if let Some(statement) = &record.sql_statement {
        if let Some(table) = extract_sql_table(statement) {
            facts.push(runtime_fact(
                record,
                GraphEdgeType::ReadsTable,
                GraphNodeType::DatabaseTable,
                table,
                "runtime database access observed in local trace artifact",
                Confidence::High,
                "record",
            ));
        }
    }
    if let Some(message) = &record.message {
        facts.push(runtime_fact(
            record,
            GraphEdgeType::FailedIn,
            GraphNodeType::RuntimeError,
            message.clone(),
            "runtime incident observed in local log or failure artifact",
            Confidence::High,
            "record",
        ));
    }
    facts
}

fn add_aggregate_facts(
    mut facts: Vec<AnalysisFact>,
    records: &[RuntimeRecord],
) -> Vec<AnalysisFact> {
    facts.extend(
        aggregate_runtime_records(records)
            .into_iter()
            .map(|aggregate| aggregate_fact(&aggregate)),
    );
    facts
}

fn aggregate_runtime_records(records: &[RuntimeRecord]) -> Vec<RuntimeAggregate> {
    let mut groups = BTreeMap::<String, (RuntimeAggregateKind, Vec<&RuntimeRecord>)>::new();
    for record in records {
        if let Some(route) = &record.route {
            let method = record.method.as_deref().unwrap_or("HTTP");
            push_runtime_group(
                &mut groups,
                format!("route:{}:{}:{}", record.file_id.0, method, route),
                RuntimeAggregateKind::Route,
                record,
            );
        }
        if let Some(statement) = &record.sql_statement {
            if let Some(table) = extract_sql_table(statement) {
                push_runtime_group(
                    &mut groups,
                    format!("table:{}:{}", record.file_id.0, table),
                    RuntimeAggregateKind::Table,
                    record,
                );
            }
        }
        if let Some(message) = &record.message {
            push_runtime_group(
                &mut groups,
                format!("error:{}:{}", record.file_id.0, message),
                RuntimeAggregateKind::Error,
                record,
            );
        }
        if let Some(symbol_id) = &record.symbol_id {
            push_runtime_group(
                &mut groups,
                format!("symbol:{}:{}", record.file_id.0, symbol_id.0),
                RuntimeAggregateKind::Symbol,
                record,
            );
        }
    }
    groups
        .into_iter()
        .filter_map(|(key, (kind, records))| runtime_aggregate(key, kind, &records))
        .collect()
}

fn push_runtime_group<'a>(
    groups: &mut BTreeMap<String, (RuntimeAggregateKind, Vec<&'a RuntimeRecord>)>,
    key: String,
    kind: RuntimeAggregateKind,
    record: &'a RuntimeRecord,
) {
    groups
        .entry(key)
        .or_insert_with(|| (kind, Vec::new()))
        .1
        .push(record);
}

fn runtime_aggregate(
    key: String,
    kind: RuntimeAggregateKind,
    records: &[&RuntimeRecord],
) -> Option<RuntimeAggregate> {
    let first = *records.first()?;
    let (edge_type, target_kind, target) = match kind {
        RuntimeAggregateKind::Route => {
            let route = first.route.as_ref()?;
            (
                GraphEdgeType::ExposesEndpoint,
                GraphNodeType::Endpoint,
                format!("{} {}", first.method.as_deref().unwrap_or("HTTP"), route),
            )
        }
        RuntimeAggregateKind::Table => {
            let statement = first.sql_statement.as_ref()?;
            (
                GraphEdgeType::ReadsTable,
                GraphNodeType::DatabaseTable,
                extract_sql_table(statement)?,
            )
        }
        RuntimeAggregateKind::Error => {
            let message = first.message.as_ref()?;
            (
                GraphEdgeType::FailedIn,
                GraphNodeType::RuntimeError,
                message.clone(),
            )
        }
        RuntimeAggregateKind::Symbol => {
            let symbol_id = first.symbol_id.as_ref()?;
            (
                GraphEdgeType::FailedIn,
                GraphNodeType::RuntimeError,
                format!("symbol:{}", symbol_id.0),
            )
        }
    };
    let mut durations = records
        .iter()
        .filter_map(|record| record.duration_ms)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .collect::<Vec<_>>();
    durations.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let count = records.len();
    let error_count = records.iter().filter(|record| record.error).count();
    let timestamps = records
        .iter()
        .filter_map(|record| record.timestamp.clone())
        .collect::<Vec<_>>();
    let sample_messages = records
        .iter()
        .filter_map(|record| record.message.clone())
        .take(MAX_SAMPLE_MESSAGES)
        .collect::<Vec<_>>();
    let mut services = records
        .iter()
        .filter_map(|record| record.service.clone())
        .collect::<Vec<_>>();
    services.sort();
    services.dedup();
    let mut status_codes = records
        .iter()
        .filter_map(|record| record.status_code)
        .collect::<Vec<_>>();
    status_codes.sort();
    status_codes.dedup();
    Some(RuntimeAggregate {
        key,
        target,
        target_kind,
        edge_type,
        file_id: first.file_id.clone(),
        symbol_id: first.symbol_id.clone(),
        count,
        error_count,
        error_rate: error_count as f32 / count as f32,
        p50_ms: percentile(&durations, 0.50),
        p95_ms: percentile(&durations, 0.95),
        p99_ms: percentile(&durations, 0.99),
        first_observed: timestamps.iter().min().cloned(),
        last_observed: timestamps.iter().max().cloned(),
        services,
        status_codes,
        sample_messages,
        confidence: if count >= 3 {
            Confidence::High
        } else {
            Confidence::Medium
        },
    })
}

fn aggregate_fact(aggregate: &RuntimeAggregate) -> AnalysisFact {
    AnalysisFact {
        id: identity::stable_hash(&format!("runtime-aggregate:{}", aggregate.key)),
        file_id: aggregate.file_id.clone(),
        symbol_id: aggregate.symbol_id.clone(),
        target: aggregate.target.clone(),
        target_kind: aggregate.target_kind.clone(),
        edge_type: aggregate.edge_type.clone(),
        range: None,
        confidence: aggregate.confidence,
        source: "open-kioku-runtime:aggregate".into(),
        source_type: EvidenceSourceType::Runtime,
        message: aggregate_message(aggregate),
    }
}

fn runtime_fact(
    record: &RuntimeRecord,
    edge_type: GraphEdgeType,
    target_kind: GraphNodeType,
    target: String,
    message: &'static str,
    confidence: Confidence,
    kind: &str,
) -> AnalysisFact {
    AnalysisFact {
        id: identity::stable_hash(&format!(
            "runtime:{kind}:{}:{:?}:{}:{}",
            record.file_path.display(),
            edge_type,
            target,
            record.artifact_line
        )),
        file_id: record.file_id.clone(),
        symbol_id: record.symbol_id.clone(),
        target,
        target_kind,
        edge_type,
        range: record.line.map(LineRange::single),
        confidence,
        source: format!("open-kioku-runtime:{}", record.artifact.display()),
        source_type: EvidenceSourceType::Runtime,
        message: message.into(),
    }
}

fn aggregate_message(aggregate: &RuntimeAggregate) -> String {
    let mut message = format!(
        "runtime aggregate observed: count {}, error_count {}, error_rate {:.2}",
        aggregate.count, aggregate.error_count, aggregate.error_rate
    );
    if let Some(p50) = aggregate.p50_ms {
        message.push_str(&format!(", p50_ms {:.1}", p50));
    }
    if let Some(p95) = aggregate.p95_ms {
        message.push_str(&format!(", p95_ms {:.1}", p95));
    }
    if let Some(p99) = aggregate.p99_ms {
        message.push_str(&format!(", p99_ms {:.1}", p99));
    }
    if let Some(first) = &aggregate.first_observed {
        message.push_str(&format!(", first_observed {first}"));
    }
    if let Some(last) = &aggregate.last_observed {
        message.push_str(&format!(", last_observed {last}"));
    }
    if let Some(freshness) = aggregate
        .last_observed
        .as_deref()
        .and_then(runtime_freshness)
    {
        message.push_str(&format!(", freshness {freshness}"));
    }
    if !aggregate.services.is_empty() {
        message.push_str(&format!(", services {}", aggregate.services.join("|")));
    }
    if !aggregate.status_codes.is_empty() {
        message.push_str(&format!(
            ", status_codes {}",
            aggregate
                .status_codes
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join("|")
        ));
    }
    if !aggregate.sample_messages.is_empty() {
        message.push_str(&format!(
            ", sample_messages {}",
            aggregate.sample_messages.join(" | ")
        ));
    }
    message
}

pub fn percentile(sorted_values: &[f64], percentile: f64) -> Option<f64> {
    if sorted_values.is_empty() {
        return None;
    }
    let rank = (percentile.clamp(0.0, 1.0) * sorted_values.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted_values.len() - 1);
    Some(sorted_values[index])
}

fn runtime_freshness(timestamp: &str) -> Option<&'static str> {
    let observed = DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&Utc);
    let age = Utc::now().signed_duration_since(observed);
    Some(if age.num_days() > STALE_RUNTIME_DAYS {
        "stale"
    } else {
        "recent"
    })
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
        if let Some(value) = value
            .get("attributes")
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(Value::as_str)
        {
            return Some(value.to_string());
        }
        if let Some(value) = value
            .get("resource")
            .and_then(|resource| resource.get("attributes"))
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(Value::as_str)
        {
            return Some(value.to_string());
        }
    }
    None
}

fn json_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key)
            .and_then(Value::as_u64)
            .or_else(|| {
                nested_json_value(value, key)
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<u64>().ok())
            })
        {
            return Some(value);
        }
        if let Some(value) = value
            .get("attributes")
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str()?.parse::<u64>().ok())
            })
        {
            return Some(value);
        }
    }
    None
}

fn json_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key)
            .and_then(Value::as_f64)
            .or_else(|| {
                nested_json_value(value, key)
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<f64>().ok())
            })
        {
            return Some(value);
        }
        if let Some(value) = value
            .get("attributes")
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(|value| {
                value
                    .as_f64()
                    .or_else(|| value.as_str()?.parse::<f64>().ok())
            })
        {
            return Some(value);
        }
    }
    None
}

fn json_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key)
            .and_then(Value::as_bool)
            .or_else(|| {
                nested_json_value(value, key)
                    .and_then(Value::as_str)
                    .and_then(|value| match value.to_ascii_lowercase().as_str() {
                        "true" | "1" | "yes" => Some(true),
                        "false" | "0" | "no" => Some(false),
                        _ => None,
                    })
            })
        {
            return Some(value);
        }
    }
    None
}

fn nested_json_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(exact) = value.get(key) {
        return Some(exact);
    }
    let mut current = value;
    for segment in key.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn normalize_runtime_file(root: &Path, value: &str) -> String {
    let path = Path::new(value);
    let rel = if path.is_absolute() {
        path.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    normalize_path(&rel.to_string_lossy())
}

fn normalize_path(value: &str) -> String {
    value.trim_start_matches("./").replace('\\', "/")
}

fn extract_sql_table(statement: &str) -> Option<String> {
    let lower = statement.to_ascii_lowercase();
    for keyword in [" from ", " join ", " update ", " into "] {
        if let Some(index) = lower.find(keyword) {
            let start = index + keyword.len();
            let table = statement[start..]
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '.')
                .find(|part| !part.is_empty())?;
            return Some(table.to_string());
        }
    }
    None
}

fn compact_runtime_message(message: &str) -> Option<String> {
    let value = redact_secrets(message.trim());
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(160).collect())
}

pub fn redact_secrets(message: &str) -> String {
    message
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if ["password=", "token=", "secret=", "api_key=", "apikey="]
                .iter()
                .any(|prefix| lower.contains(prefix))
            {
                token
                    .split_once('=')
                    .map(|(key, _)| format!("{key}=[REDACTED]"))
                    .unwrap_or_else(|| "[REDACTED]".into())
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, File, Language, RepositoryId, Symbol, SymbolKind,
    };

    fn file(path: &str) -> File {
        File {
            id: FileId::new(path),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::TypeScript,
            size_bytes: 10,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(file: &File, name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("sym:{name}")),
            name: name.into(),
            qualified_name: format!("src::{name}"),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange::single(3)),
            language: Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = vec![10.0, 20.0, 30.0, 40.0];
        assert_eq!(percentile(&values, 0.50), Some(20.0));
        assert_eq!(percentile(&values, 0.95), Some(40.0));
        assert_eq!(percentile(&[], 0.95), None);
    }

    #[test]
    fn redacts_secret_like_message_tokens() {
        assert_eq!(
            redact_secrets("failed password=hunter2 token=abc123 ok"),
            "failed password=[REDACTED] token=[REDACTED] ok"
        );
    }

    #[test]
    fn runtime_freshness_marks_old_observations_stale() {
        assert_eq!(runtime_freshness("2020-01-01T00:00:00Z"), Some("stale"));
    }

    #[test]
    fn parses_runtime_jsonl_and_aggregates_routes_symbols_and_sql() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".ok/runtime")).unwrap();
        let source_file = file("src/handler.ts");
        fs::write(
            root.join("src/handler.ts"),
            "export function handler() {}\n",
        )
        .unwrap();
        fs::write(
            root.join(".ok/runtime/spans.jsonl"),
            r#"{"file":"src/handler.ts","line":3,"symbol":"handler","timestamp":"2026-01-01T00:00:00Z","attributes":{"http.route":"/checkout","http.request.method":"POST","http.response.status_code":500,"duration_ms":120,"db.statement":"select * from orders"},"message":"failed token=abc"}
not-json
{"file":"src/handler.ts","line":3,"symbol":"handler","timestamp":"2026-01-01T00:01:00Z","attributes":{"http.route":"/checkout","http.request.method":"POST","http.response.status_code":200,"duration_ms":60}}
{"file":"src/handler.ts","line":3,"symbol":"handler","timestamp":"2026-01-01T00:02:00Z","attributes":{"http.route":"/checkout","http.request.method":"POST","http.response.status_code":200,"duration_ms":30}}
"#,
        )
        .unwrap();

        let facts = collect_runtime_analysis_facts(
            root,
            std::slice::from_ref(&source_file),
            &[symbol(&source_file, "handler")],
        )
        .unwrap();

        assert!(facts
            .iter()
            .any(|fact| fact.target == "POST /checkout" && fact.message.contains("p95_ms 120.0")));
        assert!(facts.iter().any(|fact| {
            fact.target == "POST /checkout" && fact.message.contains("freshness ")
        }));
        assert!(facts
            .iter()
            .any(|fact| fact.target == "orders"
                && fact.message.contains("runtime aggregate observed")));
        assert!(facts
            .iter()
            .any(|fact| fact.target == "failed token=[REDACTED]"
                && fact.message.contains("runtime aggregate observed")));
        assert!(facts
            .iter()
            .any(|fact| fact.symbol_id.as_ref().map(|id| id.0.as_str()) == Some("sym:handler")));
        assert!(facts
            .iter()
            .all(|fact| !fact.message.contains("abc") && !fact.target.contains("abc")));
    }

    #[test]
    fn caps_large_runtime_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".ok/runtime")).unwrap();
        let source_file = file("src/handler.ts");
        let mut jsonl = String::new();
        for index in 0..(MAX_RUNTIME_RECORDS + 100) {
            jsonl.push_str(&format!(
                r#"{{"file":"src/handler.ts","attributes":{{"http.route":"/r{index}","duration_ms":1}}}}"#
            ));
            jsonl.push('\n');
        }
        fs::write(root.join(".ok/runtime/spans.jsonl"), jsonl).unwrap();
        let facts = collect_runtime_analysis_facts(root, &[source_file], &[]).unwrap();
        assert!(facts.len() <= MAX_RUNTIME_RECORDS * 2);
    }
}
