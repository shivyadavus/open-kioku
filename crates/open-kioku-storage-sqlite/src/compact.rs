//! Compact row encoding for the graph tables.
//!
//! `graph_edges` and `call_sites` used to persist a self-contained JSON document per row
//! *in addition to* the query columns that already held the same values. On a 1,751-file
//! Java corpus that blob averaged 1,288 bytes per edge, of which 22% re-stated `id`,
//! `from`, `to` and `edge_type` — values the columns beside it already carried — and 48%
//! was the edge's own [`Evidence`], whose file path, pass name, message and timestamp are
//! drawn from a handful of distinct values repeated across the whole corpus.
//!
//! Two mechanisms replace it:
//!
//! * every scalar field gets a typed column, so nothing is stored twice; and
//! * every repeated string is written once into a per-table dictionary and referenced by
//!   integer id. Measured on that corpus: 2,388 distinct evidence paths across 153,856
//!   edges, 66,663 distinct messages across 156,515, and one distinct `indexed_at` per
//!   index run.
//!
//! Object-level deduplication was measured and rejected: evidence objects are 1.0x
//! distinct per edge (their `id` is a content hash and `indexed_at` is stamped per run),
//! so a normalised `evidence` table keyed by evidence id would add a join and a second
//! 64-character index for no saving. The redundancy is at field level, which is where the
//! dictionary sits.

use open_kioku_core::{
    CallSite, Confidence, Evidence, EvidenceId, EvidenceSourceType, FileRange, GraphEdge,
    GraphEdgeType, LineRange, NodeId, SymbolId,
};
#[cfg(test)]
use open_kioku_core::{CallSiteId, FileId, ReceiverKind, ScopeId, SourceRange};
use open_kioku_errors::{OkError, Result};
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use std::collections::{BTreeMap, HashMap};

/// Dictionary table backing `graph_edges`.
pub(crate) const GRAPH_STRINGS: &str = "graph_strings";
/// Dictionary table backing `call_sites`.
pub(crate) const CALL_SITE_STRINGS: &str = "call_site_strings";

/// FNV-1a. Chosen over `DefaultHasher` because the value is persisted: `DefaultHasher`'s
/// output is explicitly not stable across Rust releases, and a hash that silently changed
/// would make the incremental writer stop finding existing entries.
fn fnv1a64(value: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

fn storage_err(err: rusqlite::Error) -> OkError {
    OkError::Storage(err.to_string())
}

/// Interns strings into one dictionary table for the duration of a write.
///
/// `bulk` is set by the paths that just emptied the table: they cannot hit an existing row,
/// so the per-string `SELECT` is skipped. The incremental paths keep it, because they add
/// rows to a dictionary that already has content.
pub(crate) struct StringWriter {
    table: &'static str,
    cache: HashMap<String, i64>,
    next_id: i64,
    bulk: bool,
}

impl StringWriter {
    pub(crate) fn bulk(table: &'static str) -> Self {
        Self {
            table,
            cache: HashMap::new(),
            next_id: 1,
            bulk: true,
        }
    }

    pub(crate) fn incremental(tx: &Transaction<'_>, table: &'static str) -> Result<Self> {
        let max: i64 = tx
            .query_row(
                &format!("SELECT COALESCE(MAX(sid), 0) FROM {table}"),
                [],
                |row| row.get(0),
            )
            .map_err(storage_err)?;
        Ok(Self {
            table,
            cache: HashMap::new(),
            next_id: max + 1,
            bulk: false,
        })
    }

    pub(crate) fn intern(&mut self, tx: &Transaction<'_>, value: &str) -> Result<i64> {
        if let Some(sid) = self.cache.get(value) {
            return Ok(*sid);
        }
        let hash = fnv1a64(value);
        if !self.bulk {
            let existing: Option<i64> = tx
                .prepare_cached(&format!(
                    "SELECT sid FROM {} WHERE vhash = ?1 AND value = ?2",
                    self.table
                ))
                .map_err(storage_err)?
                .query_row(rusqlite::params![hash, value], |row| row.get(0))
                .optional()
                .map_err(storage_err)?;
            if let Some(sid) = existing {
                self.cache.insert(value.to_string(), sid);
                return Ok(sid);
            }
        }
        let sid = self.next_id;
        self.next_id += 1;
        tx.prepare_cached(&format!(
            "INSERT INTO {}(sid, vhash, value) VALUES(?1, ?2, ?3)",
            self.table
        ))
        .map_err(storage_err)?
        .execute(rusqlite::params![sid, hash, value])
        .map_err(storage_err)?;
        self.cache.insert(value.to_string(), sid);
        Ok(sid)
    }

    pub(crate) fn intern_opt(
        &mut self,
        tx: &Transaction<'_>,
        value: Option<&str>,
    ) -> Result<Option<i64>> {
        match value {
            Some(value) => self.intern(tx, value).map(Some),
            None => Ok(None),
        }
    }
}

/// Resolve a caller-supplied string to its dictionary id.
///
/// `Ok(None)` means the string was never stored, so every query keyed on it is empty by
/// construction — the callers turn that into an empty result rather than a scan.
pub(crate) fn lookup_sid(conn: &Connection, table: &str, value: &str) -> Result<Option<i64>> {
    conn.prepare_cached(&format!(
        "SELECT sid FROM {table} WHERE vhash = ?1 AND value = ?2"
    ))
    .map_err(storage_err)?
    .query_row(rusqlite::params![fnv1a64(value), value], |row| row.get(0))
    .optional()
    .map_err(storage_err)
}

// -- enum column round-tripping -------------------------------------------------------
//
// The type columns hold the `Debug` name (`DependsOn`, `StaticAnalysis`, `Medium`) because
// that spelling is what `edge_type_stats` reports and what existing callers compare
// against. Parsing derives the serde spelling from the `Debug` name rather than
// hand-listing variants, so a new variant needs no change here.

fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_enum<T: serde::de::DeserializeOwned>(
    kind: &str,
    name: &str,
    screaming: bool,
) -> Result<T> {
    let mut spelling = pascal_to_snake(name);
    if screaming {
        spelling = spelling.to_ascii_uppercase();
    }
    serde_json::from_value(serde_json::Value::String(spelling))
        .map_err(|_| OkError::Storage(format!("unknown {kind} `{name}` in the index")))
}

pub(crate) fn parse_edge_type(name: &str) -> Result<GraphEdgeType> {
    parse_enum("graph edge type", name, true)
}

pub(crate) fn parse_confidence(name: &str) -> Result<Confidence> {
    parse_enum("confidence", name, false)
}

pub(crate) fn parse_source_type(name: &str) -> Result<EvidenceSourceType> {
    parse_enum("evidence source type", name, false)
}

#[cfg(test)]
fn parse_receiver_kind(name: &str) -> Result<ReceiverKind> {
    parse_enum("receiver kind", name, false)
}

// -- residual edge fields --------------------------------------------------------------

/// Everything on a [`GraphEdge`] that has no column of its own.
///
/// Serialized only when non-empty and interned like any other string: the four
/// low-cardinality property keys the graph builder writes on most edges make the residual
/// document 2.1x duplicated across edges on the measured corpus.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct EdgeExtra {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    properties: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_pass: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extractor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ambiguity: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    quality_notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confidence_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confidence_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_freshness: Option<String>,
}

impl EdgeExtra {
    fn is_empty(&self) -> bool {
        self.properties.is_empty()
            && self.schema_version.is_none()
            && self.source_pass.is_none()
            && self.index_mode.is_none()
            && self.extractor_version.is_none()
            && self.ambiguity.is_empty()
            && self.quality_notes.is_empty()
            && self.confidence_score.is_none()
            && self.confidence_reason.is_none()
            && self.evidence_freshness.is_none()
    }
}

/// A `graph_edges` row, ready to bind.
pub(crate) struct EdgeRow {
    pub(crate) id: String,
    pub(crate) from_sid: i64,
    pub(crate) to_sid: i64,
    pub(crate) edge_type: String,
    pub(crate) confidence: String,
    pub(crate) source_type: String,
    pub(crate) source_sid: Option<i64>,
    pub(crate) freshness: i64,
    pub(crate) ev_id: String,
    pub(crate) ev_path_sid: Option<i64>,
    pub(crate) ev_line_start: Option<i64>,
    pub(crate) ev_line_end: Option<i64>,
    pub(crate) ev_symbol_sid: Option<i64>,
    pub(crate) ev_message_sid: Option<i64>,
    pub(crate) ev_indexed_at_sid: i64,
    pub(crate) extra_sid: Option<i64>,
}

pub(crate) fn encode_edge(
    tx: &Transaction<'_>,
    strings: &mut StringWriter,
    edge: &GraphEdge,
) -> Result<EdgeRow> {
    let evidence = &edge.evidence;
    let extra = EdgeExtra {
        properties: edge.properties.clone(),
        schema_version: edge.schema_version.clone(),
        source_pass: edge.source_pass.clone(),
        index_mode: edge.index_mode.clone(),
        extractor_version: edge.extractor_version.clone(),
        ambiguity: edge.ambiguity.clone(),
        quality_notes: edge.quality_notes.clone(),
        confidence_score: evidence.confidence_score,
        confidence_reason: evidence.confidence_reason.clone(),
        evidence_freshness: evidence.freshness.clone(),
    };
    let extra_sid = if extra.is_empty() {
        None
    } else {
        Some(strings.intern(tx, &serde_json::to_string(&extra)?)?)
    };
    let (path_sid, line_start, line_end) = match &evidence.file_range {
        Some(range) => (
            // `SharedPath`'s serializer refuses non-UTF-8 rather than emitting something a
            // reader cannot round-trip; the stored form matches it instead of quietly
            // substituting replacement characters.
            Some(strings.intern(
                tx,
                range.path.to_str().ok_or_else(|| {
                    OkError::Storage(
                        "evidence file range path contains invalid UTF-8 characters".into(),
                    )
                })?,
            )?),
            range.line_range.as_ref().map(|r| r.start as i64),
            range.line_range.as_ref().map(|r| r.end as i64),
        ),
        None => (None, None, None),
    };
    Ok(EdgeRow {
        id: edge.id.0.clone(),
        from_sid: strings.intern(tx, &edge.from.0)?,
        to_sid: strings.intern(tx, &edge.to.0)?,
        edge_type: format!("{:?}", edge.edge_type),
        confidence: format!("{:?}", evidence.confidence),
        source_type: format!("{:?}", evidence.source_type),
        source_sid: strings.intern_opt(tx, Some(evidence.source.as_str()))?,
        freshness: evidence.indexed_at.timestamp(),
        ev_id: evidence.id.0.clone(),
        ev_path_sid: path_sid,
        ev_line_start: line_start,
        ev_line_end: line_end,
        ev_symbol_sid: strings
            .intern_opt(tx, evidence.symbol_id.as_ref().map(|id| id.0.as_str()))?,
        ev_message_sid: strings.intern_opt(tx, Some(evidence.message.as_str()))?,
        // One distinct value per index run, so this reference costs 8 bytes per edge and
        // keeps the sub-second precision a unix `freshness` timestamp would drop. Nanoseconds,
        // not microseconds: `Utc::now()` resolves to nanoseconds on Linux, and truncating here
        // would silently round every edge's evidence timestamp on the way through the store.
        ev_indexed_at_sid: strings.intern(
            tx,
            &evidence
                .indexed_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        )?,
        extra_sid,
    })
}

/// Columns every edge read selects, in the order [`edge_from_row`] expects.
///
/// Every join is a LEFT JOIN, including the two endpoints. An inner join would make an edge
/// whose endpoint dictionary entry went missing disappear from every read with no error —
/// silently dropping a relationship, which is the one failure this format must not have.
/// [`edge_from_row`] turns the resulting `NULL` into an explicit error instead.
pub(crate) const EDGE_SELECT: &str = "\
SELECT e.id, sf.value, st.value, e.edge_type, e.confidence, e.source_type, ss.value, \
e.ev_id, ep.value, e.ev_line_start, e.ev_line_end, esy.value, em.value, ei.value, ex.value \
FROM graph_edges e \
LEFT JOIN graph_strings sf ON sf.sid = e.from_sid \
LEFT JOIN graph_strings st ON st.sid = e.to_sid \
LEFT JOIN graph_strings ss ON ss.sid = e.source_sid \
LEFT JOIN graph_strings ep ON ep.sid = e.ev_path_sid \
LEFT JOIN graph_strings esy ON esy.sid = e.ev_symbol_sid \
LEFT JOIN graph_strings em ON em.sid = e.ev_message_sid \
LEFT JOIN graph_strings ei ON ei.sid = e.ev_indexed_at_sid \
LEFT JOIN graph_strings ex ON ex.sid = e.extra_sid";

/// Rebuild a [`GraphEdge`] from the columns [`EDGE_SELECT`] projects.
pub(crate) fn edge_from_row(row: &Row<'_>) -> Result<GraphEdge> {
    let get_string = |index: usize| -> Result<String> { row.get(index).map_err(storage_err) };
    let get_opt = |index: usize| -> Result<Option<String>> { row.get(index).map_err(storage_err) };
    let get_opt_i64 = |index: usize| -> Result<Option<i64>> { row.get(index).map_err(storage_err) };

    let extra: EdgeExtra = match get_opt(14)? {
        Some(raw) => serde_json::from_str(&raw)?,
        None => EdgeExtra::default(),
    };
    let file_range = match get_opt(8)? {
        Some(path) => {
            let line_range = match (get_opt_i64(9)?, get_opt_i64(10)?) {
                (Some(start), Some(end)) => Some(LineRange {
                    start: start as u32,
                    end: end as u32,
                }),
                _ => None,
            };
            Some(FileRange {
                path: path.as_str().into(),
                line_range,
            })
        }
        None => None,
    };
    // Written for every edge, so an absent or unparseable value is a damaged row rather than
    // an edge without a timestamp; defaulting it would date the evidence to 1970 silently.
    let raw_indexed_at = get_opt(13)?.ok_or_else(|| {
        OkError::Storage(
            "graph edge row has no evidence timestamp; re-index this repository".into(),
        )
    })?;
    let indexed_at = chrono::DateTime::parse_from_rfc3339(&raw_indexed_at)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|err| OkError::Storage(format!("unreadable evidence timestamp: {err}")))?;
    let evidence = Evidence {
        id: EvidenceId::new(get_string(7)?),
        source: get_opt(6)?.unwrap_or_default().as_str().into(),
        source_type: parse_source_type(&get_string(5)?)?,
        file_range,
        symbol_id: get_opt(11)?.map(SymbolId::new),
        confidence: parse_confidence(&get_string(4)?)?,
        message: get_opt(12)?.unwrap_or_default().as_str().into(),
        indexed_at,
        confidence_score: extra.confidence_score,
        confidence_reason: extra.confidence_reason,
        freshness: extra.evidence_freshness,
    };
    // An endpoint with no dictionary entry is a damaged row. Reporting it is the point: the
    // alternative is an edge that quietly stops existing.
    let endpoint = |index: usize, field: &str| -> Result<String> {
        get_opt(index)?.ok_or_else(|| {
            OkError::Storage(format!(
                "graph edge row has an unresolvable `{field}` endpoint; re-index this repository"
            ))
        })
    };
    Ok(GraphEdge {
        id: open_kioku_core::EdgeId::new(get_string(0)?),
        from: NodeId::new(endpoint(1, "from")?),
        to: NodeId::new(endpoint(2, "to")?),
        edge_type: parse_edge_type(&get_string(3)?)?,
        evidence,
        properties: extra.properties,
        schema_version: extra.schema_version,
        source_pass: extra.source_pass,
        index_mode: extra.index_mode,
        extractor_version: extra.extractor_version,
        ambiguity: extra.ambiguity,
        quality_notes: extra.quality_notes,
    })
}

// -- call sites --------------------------------------------------------------------------

/// A `call_sites` row, ready to bind.
pub(crate) struct CallSiteRow {
    pub(crate) id_sid: i64,
    pub(crate) file_sid: i64,
    pub(crate) scope_sid: i64,
    pub(crate) caller_sid: Option<i64>,
    pub(crate) callee_sid: i64,
    pub(crate) receiver_sid: Option<i64>,
    pub(crate) receiver_kind: String,
    pub(crate) start_line: i64,
    pub(crate) start_column: i64,
    pub(crate) end_line: i64,
    pub(crate) end_column: i64,
}

pub(crate) fn encode_call_site(
    tx: &Transaction<'_>,
    strings: &mut StringWriter,
    call_site: &CallSite,
) -> Result<CallSiteRow> {
    Ok(CallSiteRow {
        id_sid: strings.intern(tx, &call_site.id.0)?,
        file_sid: strings.intern(tx, &call_site.file_id.0)?,
        scope_sid: strings.intern(tx, &call_site.scope_id.0)?,
        caller_sid: strings.intern_opt(
            tx,
            call_site.caller_symbol_id.as_ref().map(|id| id.0.as_str()),
        )?,
        callee_sid: strings.intern(tx, &call_site.callee_name)?,
        receiver_sid: strings.intern_opt(tx, call_site.receiver.as_deref())?,
        receiver_kind: format!("{:?}", call_site.receiver_kind),
        start_line: call_site.range.start_line as i64,
        start_column: call_site.range.start_column as i64,
        end_line: call_site.range.end_line as i64,
        end_column: call_site.range.end_column as i64,
    })
}

/// Columns a call-site read selects, in the order the decoder expects.
#[cfg(test)]
pub(crate) const CALL_SITE_SELECT: &str = "\
SELECT si.value, sf.value, ss.value, sc.value, sn.value, sr.value, c.receiver_kind, \
c.start_line, c.start_column, c.end_line, c.end_column \
FROM call_sites c \
JOIN call_site_strings si ON si.sid = c.id_sid \
JOIN call_site_strings sf ON sf.sid = c.file_sid \
JOIN call_site_strings ss ON ss.sid = c.scope_sid \
LEFT JOIN call_site_strings sc ON sc.sid = c.caller_sid \
JOIN call_site_strings sn ON sn.sid = c.callee_sid \
LEFT JOIN call_site_strings sr ON sr.sid = c.receiver_sid";

/// Rebuild a [`CallSite`] from the columns `CALL_SITE_SELECT` projects.
///
/// Nothing in the product reads call sites back out of the index today — resolution
/// consumes them from the in-memory snapshot — so this exists to prove the columns are a
/// faithful, lossless replacement for the JSON document they took over from.
#[cfg(test)]
pub(crate) fn call_site_from_row(row: &Row<'_>) -> Result<CallSite> {
    let get_string = |index: usize| -> Result<String> { row.get(index).map_err(storage_err) };
    let get_opt = |index: usize| -> Result<Option<String>> { row.get(index).map_err(storage_err) };
    let get_u32 = |index: usize| -> Result<u32> {
        row.get::<_, i64>(index)
            .map(|value| value as u32)
            .map_err(storage_err)
    };
    let receiver_kind = parse_receiver_kind(&get_string(6)?)?;
    Ok(CallSite {
        id: CallSiteId::new(get_string(0)?),
        file_id: FileId::new(get_string(1)?),
        scope_id: ScopeId::new(get_string(2)?),
        caller_symbol_id: get_opt(3)?.map(SymbolId::new),
        callee_name: get_string(4)?,
        receiver: get_opt(5)?,
        receiver_kind,
        range: SourceRange {
            start_line: get_u32(7)?,
            start_column: get_u32(8)?,
            end_line: get_u32(9)?,
            end_column: get_u32(10)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_columns_round_trip_every_variant() {
        for edge_type in [
            GraphEdgeType::Contains,
            GraphEdgeType::DependsOn,
            GraphEdgeType::ExposesEndpoint,
            GraphEdgeType::SemanticallyRelated,
            GraphEdgeType::RelatedToTicket,
        ] {
            let name = format!("{edge_type:?}");
            assert_eq!(parse_edge_type(&name).unwrap(), edge_type);
        }
        for confidence in [
            Confidence::Low,
            Confidence::Medium,
            Confidence::High,
            Confidence::Exact,
        ] {
            let name = format!("{confidence:?}");
            assert_eq!(parse_confidence(&name).unwrap(), confidence);
        }
        for source_type in [
            EvidenceSourceType::TreeSitter,
            EvidenceSourceType::StaticAnalysis,
            EvidenceSourceType::ExternalIntegration,
            EvidenceSourceType::GitHistory,
        ] {
            let name = format!("{source_type:?}");
            assert_eq!(parse_source_type(&name).unwrap(), source_type);
        }
    }

    #[test]
    fn unknown_type_column_is_an_error_not_a_default() {
        assert!(parse_edge_type("NotAnEdgeType").is_err());
        assert!(parse_confidence("Certain").is_err());
        assert!(parse_source_type("Vibes").is_err());
    }

    #[test]
    fn fnv_hash_is_stable_for_known_inputs() {
        // Pinned so a refactor cannot silently change the persisted `vhash` column and
        // strand every dictionary entry written by an earlier build.
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325_u64 as i64);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
        assert_eq!(fnv1a64("foobar"), 0x85944171_f73967e8_u64 as i64);
    }
}
