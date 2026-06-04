use chrono::Utc;
use open_kioku_core::{Confidence, EntityLink, MemoryFact, MemoryFactId, MemorySearchResult};
use open_kioku_errors::{OkError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct RepoMemoryStore {
    connection: Mutex<Connection>,
}

impl RepoMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| OkError::Storage(format!("create memory dir: {err}")))?;
        }
        let connection = Connection::open(path).map_err(storage_err)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn open_repo(repo: impl AsRef<Path>) -> Result<Self> {
        Self::open(default_memory_path(repo))
    }

    pub fn remember(&self, text: &str, source: &str, confidence: Confidence) -> Result<MemoryFact> {
        let text = text.trim();
        if text.is_empty() {
            return Err(OkError::Config("memory fact text cannot be empty".into()));
        }
        let created_at = Utc::now();
        let fact = MemoryFact {
            id: MemoryFactId::new(memory_id(text, source, created_at.timestamp_micros())),
            text: text.into(),
            source: source.into(),
            confidence,
            entities: extract_entities(text),
            created_at,
        };
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("memory sqlite mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO memory_facts(id, created_at, source, text, json) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                &fact.id.0,
                fact.created_at.to_rfc3339(),
                &fact.source,
                &fact.text,
                serde_json::to_string(&fact)?
            ],
        )
        .map_err(storage_err)?;
        Ok(fact)
    }

    pub fn get(&self, id: &MemoryFactId) -> Result<Option<MemoryFact>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("memory sqlite mutex poisoned".into()))?;
        let raw = conn
            .query_row(
                "SELECT json FROM memory_facts WHERE id = ?1",
                params![&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err)?;
        raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let facts = self.recent(500)?;
        let query_terms = terms(query);
        let query_entities = extract_entities(query);
        let mut scored = facts
            .into_iter()
            .filter_map(|fact| score_fact(fact, &query_terms, &query_entities))
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.fact.created_at.cmp(&a.fact.created_at))
        });
        scored.truncate(limit.min(100));
        Ok(scored)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<MemoryFact>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("memory sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM memory_facts ORDER BY created_at DESC LIMIT ?1")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(serde_json::from_str(&row.map_err(storage_err)?)?);
        }
        Ok(facts)
    }

    fn initialize(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("memory sqlite mutex poisoned".into()))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memory_facts (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                source TEXT NOT NULL,
                text TEXT NOT NULL,
                json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_memory_created_at ON memory_facts(created_at);
            CREATE INDEX IF NOT EXISTS idx_memory_source ON memory_facts(source);
            ",
        )
        .map_err(storage_err)?;
        Ok(())
    }
}

pub fn default_memory_path(repo: impl AsRef<Path>) -> PathBuf {
    repo.as_ref().join(".ok/memory.sqlite")
}

pub fn extract_entities(text: &str) -> Vec<EntityLink> {
    let mut entities = Vec::new();
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|ch: char| {
            !(ch.is_ascii_alphanumeric()
                || ch == '_'
                || ch == '-'
                || ch == '/'
                || ch == '.'
                || ch == ':')
        });
        if cleaned.len() < 3 {
            continue;
        }
        let kind = if is_path_like(cleaned) {
            "file"
        } else if is_ticket_id(cleaned) {
            "ticket"
        } else if cleaned.starts_with("cargo")
            || cleaned.starts_with("./")
            || cleaned.starts_with("npm")
            || cleaned.starts_with("pytest")
        {
            "command"
        } else if is_identifier(cleaned) {
            "symbol"
        } else {
            continue;
        };
        if !entities
            .iter()
            .any(|entity: &EntityLink| entity.kind == kind && entity.value == cleaned)
        {
            entities.push(EntityLink {
                kind: kind.into(),
                value: cleaned.into(),
                file_range: None,
                confidence: Confidence::Medium,
            });
        }
    }
    entities
}

fn score_fact(
    fact: MemoryFact,
    query_terms: &[String],
    query_entities: &[EntityLink],
) -> Option<MemorySearchResult> {
    let lower = fact.text.to_ascii_lowercase();
    let mut score = 0.0;
    let mut evidence = Vec::new();
    let term_hits = query_terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count();
    if term_hits > 0 {
        score += 0.25 + term_hits as f32 * 0.08;
        evidence.push(format!("{term_hits} lexical term match(es)"));
    }
    let entity_hits = query_entities
        .iter()
        .filter(|query_entity| {
            fact.entities.iter().any(|fact_entity| {
                fact_entity.kind == query_entity.kind && fact_entity.value == query_entity.value
            })
        })
        .count();
    if entity_hits > 0 {
        score += 0.45 + entity_hits as f32 * 0.15;
        evidence.push(format!("{entity_hits} entity link match(es)"));
    }
    score += fact.confidence.score() * 0.1;
    if evidence.is_empty() {
        return None;
    }
    Some(MemorySearchResult {
        fact,
        score,
        match_reason: "repo memory lexical/entity match".into(),
        evidence,
    })
}

fn terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn memory_id(text: &str, source: &str, timestamp: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.update(source.as_bytes());
    hasher.update(timestamp.to_le_bytes());
    format!("mem:{}", hex_prefix(&hasher.finalize(), 16))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .take(len)
        .map(|nibble| char::from_digit(nibble as u32, 16).unwrap_or('0'))
        .collect()
}

fn is_path_like(value: &str) -> bool {
    value.contains('/')
        || value.ends_with(".rs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".js")
        || value.ends_with(".jsx")
        || value.ends_with(".java")
        || value.ends_with(".py")
        || value.ends_with(".go")
        || value.ends_with(".md")
}

fn is_ticket_id(value: &str) -> bool {
    let Some((prefix, number)) = value.split_once('-') else {
        return false;
    };
    prefix.len() >= 2
        && prefix.chars().all(|ch| ch.is_ascii_uppercase())
        && number.len() >= 2
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn is_identifier(value: &str) -> bool {
    let has_lower = value.chars().any(|ch| ch.is_ascii_lowercase());
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    let has_separator = value.contains('_') || value.contains("::");
    has_separator || (has_lower && has_upper)
}

fn storage_err(err: rusqlite::Error) -> OkError {
    OkError::Storage(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_searches_entity_linked_facts() {
        let dir = tempfile::tempdir().unwrap();
        let store = RepoMemoryStore::open_repo(dir.path()).unwrap();
        let fact = store
            .remember(
                "RATE-7031 maps PublishRestrictionsMutation to GqlPublishRestrictionsTest",
                "test",
                Confidence::High,
            )
            .unwrap();

        let results = store
            .search("PublishRestrictionsMutation RATE-7031", 5)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.id, fact.id);
        assert!(results[0]
            .evidence
            .iter()
            .any(|evidence| evidence.contains("entity link")));
    }
}
