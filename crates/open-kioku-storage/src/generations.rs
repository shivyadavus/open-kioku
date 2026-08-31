//! RI3.6 index generations: coherent, atomically published index layouts.
//!
//! A generation is a directory under `.ok/generations/<id>/` holding every index
//! component (SQLite store, lexical search, semantic vectors) built together. The small
//! `.ok/generations/active` pointer file names the generation currently serving reads
//! and is only ever replaced atomically, so readers never observe a half-published
//! index. Legacy repositories (components directly under `.ok/`) keep working through
//! [`resolve_index_location`], and adopt the generation layout in place — a directory
//! move, not a data copy — via [`adopt_legacy_layout`] under the index write lock.
//!
//! Design: `docs/ri3-index-generations-design.md`. This module is phase 1: layout,
//! resolution, adoption, atomic pointer, and startup classification.

use crate::Result;
use open_kioku_errors::OkError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const GENERATIONS_DIR: &str = "generations";
pub const ACTIVE_POINTER_FILE: &str = "active";
pub const GENERATION_MANIFEST_FILE: &str = "generation.json";
pub const GENERATION_SCHEMA_VERSION: u32 = 1;
/// Generation id used when a legacy layout is adopted in place.
pub const LEGACY_ADOPTION_GENERATION_ID: &str = "g0-legacy";

/// Component states recorded in a generation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationComponentState {
    Complete,
    Staging,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationComponent {
    pub state: GenerationComponentState,
}

/// Manifest describing one generation. Written last inside a staging directory, so a
/// directory without a readable manifest is by definition not publishable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub schema_version: u32,
    pub generation_id: String,
    pub created_at: String,
    #[serde(default)]
    pub components: std::collections::BTreeMap<String, GenerationComponent>,
}

/// The atomically replaced pointer naming the active generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivePointer {
    pub schema_version: u32,
    pub generation_id: String,
}

/// Where the index components for a repository currently live.
///
/// `generation` is `None` for legacy layouts (components directly under `.ok/`).
#[derive(Debug, Clone, PartialEq)]
pub struct IndexLocation {
    root: PathBuf,
    generation: Option<String>,
}

impl IndexLocation {
    /// The directory holding the components (either `.ok` or `.ok/generations/<id>`).
    pub fn component_root(&self) -> &Path {
        &self.root
    }

    pub fn generation_id(&self) -> Option<&str> {
        self.generation.as_deref()
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.root.join("index.sqlite")
    }

    pub fn search_root(&self) -> PathBuf {
        self.root.join("search")
    }

    pub fn tantivy_dir(&self) -> PathBuf {
        self.search_root().join("tantivy")
    }

    pub fn vectors_root(&self) -> PathBuf {
        self.root.join("vectors")
    }
}

fn generations_root(repo: &Path) -> PathBuf {
    repo.join(".ok").join(GENERATIONS_DIR)
}

fn generation_dir(repo: &Path, generation_id: &str) -> PathBuf {
    generations_root(repo).join(generation_id)
}

/// Resolve where index components live for this repository.
///
/// A valid active pointer naming a generation with a readable, schema-compatible
/// manifest wins; anything else falls back to the legacy layout. The fallback is
/// deliberate fail-open-to-legacy: a corrupt pointer must degrade to the old behavior
/// (which may then report a missing index) rather than fail every command.
pub fn resolve_index_location(repo: &Path) -> IndexLocation {
    let legacy = IndexLocation {
        root: repo.join(".ok"),
        generation: None,
    };
    let Some(pointer) = read_active_pointer(repo) else {
        return legacy;
    };
    let dir = generation_dir(repo, &pointer.generation_id);
    match read_generation_manifest(&dir) {
        Some(manifest) if manifest.generation_id == pointer.generation_id => IndexLocation {
            root: dir,
            generation: Some(pointer.generation_id),
        },
        _ => legacy,
    }
}

fn read_active_pointer(repo: &Path) -> Option<ActivePointer> {
    let text = std::fs::read_to_string(generations_root(repo).join(ACTIVE_POINTER_FILE)).ok()?;
    let pointer = serde_json::from_str::<ActivePointer>(&text).ok()?;
    (pointer.schema_version == GENERATION_SCHEMA_VERSION
        && is_safe_generation_id(&pointer.generation_id))
    .then_some(pointer)
}

fn read_generation_manifest(dir: &Path) -> Option<GenerationManifest> {
    let text = std::fs::read_to_string(dir.join(GENERATION_MANIFEST_FILE)).ok()?;
    let manifest = serde_json::from_str::<GenerationManifest>(&text).ok()?;
    (manifest.schema_version == GENERATION_SCHEMA_VERSION).then_some(manifest)
}

/// Generation ids are path components; restrict them so a corrupt or malicious pointer
/// can never traverse outside the generations root.
fn is_safe_generation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Atomically point readers at `generation_id` (write temp + rename).
pub fn publish_generation(repo: &Path, generation_id: &str) -> Result<()> {
    if !is_safe_generation_id(generation_id) {
        return Err(OkError::Storage(format!(
            "invalid generation id: {generation_id:?}"
        )));
    }
    let dir = generation_dir(repo, generation_id);
    if read_generation_manifest(&dir).is_none() {
        return Err(OkError::Storage(format!(
            "generation {generation_id} has no readable manifest; refusing to publish"
        )));
    }
    let root = generations_root(repo);
    std::fs::create_dir_all(&root).map_err(|err| OkError::Storage(err.to_string()))?;
    let pointer = ActivePointer {
        schema_version: GENERATION_SCHEMA_VERSION,
        generation_id: generation_id.to_string(),
    };
    let temp = root.join(format!(".{ACTIVE_POINTER_FILE}.tmp"));
    std::fs::write(
        &temp,
        serde_json::to_string_pretty(&pointer).map_err(|err| OkError::Storage(err.to_string()))?,
    )
    .map_err(|err| OkError::Storage(err.to_string()))?;
    std::fs::rename(&temp, root.join(ACTIVE_POINTER_FILE))
        .map_err(|err| OkError::Storage(err.to_string()))?;
    Ok(())
}

/// Adopt a legacy `.ok` layout into the generation layout, in place.
///
/// Moves `index.sqlite`, `search/`, and `vectors/` into `generations/g0-legacy/`,
/// writes the generation manifest, and publishes the pointer. Must be called under the
/// repository's index write lock; it never copies data and never touches shared caches
/// (models, embedding cache) that live outside generations by design.
///
/// Returns `Ok(None)` when there is nothing to adopt (already adopted, or no legacy
/// components exist).
pub fn adopt_legacy_layout(repo: &Path) -> Result<Option<String>> {
    if read_active_pointer(repo).is_some() {
        return Ok(None);
    }
    let ok_dir = repo.join(".ok");
    let legacy_sqlite = ok_dir.join("index.sqlite");
    let legacy_search = ok_dir.join("search");
    let legacy_vectors = ok_dir.join("vectors");
    if !legacy_sqlite.exists() && !legacy_search.exists() && !legacy_vectors.exists() {
        return Ok(None);
    }

    let dir = generation_dir(repo, LEGACY_ADOPTION_GENERATION_ID);
    if dir.exists() {
        // A previous adoption attempt was interrupted before the pointer was published.
        // The moves below are individually idempotent (skip components already moved),
        // so continuing completes the adoption.
        if read_generation_manifest(&dir).is_some() {
            publish_generation(repo, LEGACY_ADOPTION_GENERATION_ID)?;
            return Ok(Some(LEGACY_ADOPTION_GENERATION_ID.to_string()));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|err| OkError::Storage(err.to_string()))?;

    let mut components = std::collections::BTreeMap::new();
    for (name, source, target) in [
        ("structural", &legacy_sqlite, dir.join("index.sqlite")),
        ("search", &legacy_search, dir.join("search")),
        ("semantic", &legacy_vectors, dir.join("vectors")),
    ] {
        let state = if target.exists() {
            GenerationComponentState::Complete
        } else if source.exists() {
            std::fs::rename(source, &target).map_err(|err| {
                OkError::Storage(format!(
                    "adopting legacy {name} component failed ({} -> {}): {err}",
                    source.display(),
                    target.display()
                ))
            })?;
            GenerationComponentState::Complete
        } else {
            GenerationComponentState::Absent
        };
        components.insert(name.to_string(), GenerationComponent { state });
    }
    // SQLite sidecar files (WAL/SHM) must move with the database.
    for suffix in ["-wal", "-shm"] {
        let sidecar = ok_dir.join(format!("index.sqlite{suffix}"));
        if sidecar.exists() {
            let target = dir.join(format!("index.sqlite{suffix}"));
            let _ = std::fs::rename(&sidecar, target);
        }
    }

    let manifest = GenerationManifest {
        schema_version: GENERATION_SCHEMA_VERSION,
        generation_id: LEGACY_ADOPTION_GENERATION_ID.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        components,
    };
    let manifest_temp = dir.join(format!(".{GENERATION_MANIFEST_FILE}.tmp"));
    std::fs::write(
        &manifest_temp,
        serde_json::to_string_pretty(&manifest).map_err(|err| OkError::Storage(err.to_string()))?,
    )
    .map_err(|err| OkError::Storage(err.to_string()))?;
    std::fs::rename(&manifest_temp, dir.join(GENERATION_MANIFEST_FILE))
        .map_err(|err| OkError::Storage(err.to_string()))?;

    publish_generation(repo, LEGACY_ADOPTION_GENERATION_ID)?;
    Ok(Some(LEGACY_ADOPTION_GENERATION_ID.to_string()))
}

/// Startup classification of everything under the generations root, for status/doctor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationClassification {
    pub generation_id: String,
    pub state: GenerationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Active,
    ReadyUnpublished,
    Corrupt,
}

pub fn classify_generations(repo: &Path) -> Vec<GenerationClassification> {
    let active = read_active_pointer(repo).map(|pointer| pointer.generation_id);
    let root = generations_root(repo);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let state = if Some(&id) == active.as_ref() {
            GenerationState::Active
        } else if read_generation_manifest(&entry.path()).is_some() {
            GenerationState::ReadyUnpublished
        } else {
            GenerationState::Corrupt
        };
        out.push(GenerationClassification {
            generation_id: id,
            state,
        });
    }
    out.sort_by(|a, b| a.generation_id.cmp(&b.generation_id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_layout_resolves_without_generations() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let location = resolve_index_location(repo);
        assert_eq!(location.generation_id(), None);
        assert_eq!(location.sqlite_path(), repo.join(".ok/index.sqlite"));
        assert_eq!(location.tantivy_dir(), repo.join(".ok/search/tantivy"));
        assert_eq!(location.vectors_root(), repo.join(".ok/vectors"));
    }

    #[test]
    fn adoption_moves_components_and_publishes_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        std::fs::create_dir_all(repo.join(".ok/search/tantivy")).unwrap();
        std::fs::create_dir_all(repo.join(".ok/vectors/current")).unwrap();
        std::fs::write(repo.join(".ok/index.sqlite"), b"sqlite-bytes").unwrap();
        std::fs::write(repo.join(".ok/search/tantivy/meta.json"), b"{}").unwrap();
        std::fs::write(repo.join(".ok/vectors/current/manifest.json"), b"{}").unwrap();

        let adopted = adopt_legacy_layout(repo).unwrap();
        assert_eq!(adopted.as_deref(), Some(LEGACY_ADOPTION_GENERATION_ID));

        let location = resolve_index_location(repo);
        assert_eq!(
            location.generation_id(),
            Some(LEGACY_ADOPTION_GENERATION_ID)
        );
        assert_eq!(
            std::fs::read(location.sqlite_path()).unwrap(),
            b"sqlite-bytes"
        );
        assert!(location.tantivy_dir().join("meta.json").exists());
        assert!(location
            .vectors_root()
            .join("current/manifest.json")
            .exists());
        assert!(!repo.join(".ok/index.sqlite").exists());

        // Idempotent: adopting again is a no-op.
        assert_eq!(adopt_legacy_layout(repo).unwrap(), None);

        let classes = classify_generations(repo);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].state, GenerationState::Active);
    }

    #[test]
    fn adoption_with_nothing_to_adopt_is_a_noop() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(adopt_legacy_layout(temp.path()).unwrap(), None);
        assert_eq!(resolve_index_location(temp.path()).generation_id(), None);
    }

    #[test]
    fn corrupt_pointer_falls_back_to_legacy_and_never_escapes_root() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let root = repo.join(".ok/generations");
        std::fs::create_dir_all(&root).unwrap();

        std::fs::write(root.join("active"), "{not json").unwrap();
        assert_eq!(resolve_index_location(repo).generation_id(), None);

        // A traversal-shaped id is rejected outright.
        std::fs::write(
            root.join("active"),
            serde_json::to_string(&ActivePointer {
                schema_version: GENERATION_SCHEMA_VERSION,
                generation_id: "../../etc".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(resolve_index_location(repo).generation_id(), None);

        // A pointer at a generation without a manifest also falls back.
        std::fs::write(
            root.join("active"),
            serde_json::to_string(&ActivePointer {
                schema_version: GENERATION_SCHEMA_VERSION,
                generation_id: "g-missing".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(resolve_index_location(repo).generation_id(), None);
    }

    #[test]
    fn publish_refuses_manifestless_generations() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        std::fs::create_dir_all(repo.join(".ok/generations/g-empty")).unwrap();
        assert!(publish_generation(repo, "g-empty").is_err());
        assert!(publish_generation(repo, "../escape").is_err());
    }
}
