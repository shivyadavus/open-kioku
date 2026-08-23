use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use open_kioku_architecture::{
    evaluate_policy, evaluate_public_api_boundary, ArchitectureDetector, PolicyResolver,
};
use open_kioku_config::{
    load_architecture_policy, load_architecture_policy_from_path, ArchitecturePolicy, OkConfig,
    PolicySource, RankingConfig, ScipMode,
};
use open_kioku_context::{expanded_task_search_terms, ContextPackBuilder, ContextPackFormat};
use open_kioku_context_compress::ContextHandleStore;
use open_kioku_contract::{
    ApiSurfaceConstraint, ArchitectureConstraint, ChangeContractV1, ConstraintSeverity,
    ContractFile, ContractId, ContractStore, DependencyDeltaConstraint, EvidenceRef,
    FsContractStore, StoredContractRecord,
};
use open_kioku_core::{
    ChurnSummary, Confidence, ContextHandleId, EdgeId, EnforcedEdgeType, Evidence, EvidenceId,
    EvidenceSourceType, FileProvenance, GitChangeKind, GitCochangeEdge, GitCommitId,
    GitCommitRecord, GraphEdge, GraphEdgeType, GraphNode, HistoryRecordId, HistorySnapshot,
    HistorySummary, IndexManifest, IndexMode, NodeId, Owner, OwnerSuggestion, OwnershipEvidence,
    OwnershipReport, OwnershipSourceType, PlanReport, PolicyCheckReport, PolicyComponentMatch,
    PolicyExemptionEvidence, PolicyViolation, ProvenanceTouch, ReviewerAvailability,
    ReviewerEvidence, ReviewerRole, ReviewerSuggestionReport, ScoreComponent, SearchResult,
    SimilarChangeQuery, SimilarChangeReport, Symbol, SymbolId, SymbolProvenance, TestTarget,
};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::{IndexProgress, Indexer};
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{
    ChangeVerificationReport, ChangeVerifier, ContractVerificationReport, ContractVerifier,
    PatchPlanner, VerificationFinding, VerificationVerdict, VerifyChangeInput,
};
use open_kioku_plan::{ContractBuilder, PlanEngine, PlanFormat, PreflightFormat, PreflightReport};
use open_kioku_ranking::{
    rerank_baseline, rerank_with_options, top_score_signals, RankingMode, RankingOptions,
    RankingSignal, RankingWeights,
};
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{
    default_index_dir, rebuild_disk_index_with_graph, TantivySearchIndex,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{
    GraphStore, HistoryStore, IndexData, MetadataStore, OkStore, SearchIndex,
};
use open_kioku_storage_sqlite::{SqliteStore, SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION};
use open_kioku_symbols::SymbolEngine;
use open_kioku_tests::TestSelector;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

include!("bench/retrieval.rs");
include!("types.rs");
include!("commands/mod.rs");
include!("commands/architecture.rs");
include!("commands/adr.rs");
include!("reports/trust.rs");
include!("reports/status_setup_doctor.rs");
include!("bench/mod.rs");
include!("bench/relationship.rs");
include!("commands/verification.rs");
include!("commands/contract.rs");
include!("reports/ranking.rs");
include!("reports/proof.rs");
include!("commands/context.rs");
include!("commands/index.rs");
include!("commands/onboarding.rs");
include!("commands/snapshot.rs");
include!("search.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_repo_prefers_command_path_over_global_default() {
        assert_eq!(
            resolve_repo(Path::new("."), PathBuf::from("/tmp/open-kioku-target")),
            PathBuf::from("/tmp/open-kioku-target")
        );
    }

    #[test]
    fn resolve_repo_uses_global_path_when_command_path_is_default() {
        assert_eq!(
            resolve_repo(Path::new("/tmp/open-kioku-global"), PathBuf::from(".")),
            PathBuf::from("/tmp/open-kioku-global")
        );
    }

    #[test]
    fn status_markdown_bounds_quality_notes_without_hiding_the_total() {
        let notes = (0..105)
            .map(|index| format!("quality note {index:03}"))
            .collect::<Vec<_>>();
        let mut output = String::new();

        append_status_quality_notes(&mut output, &notes);

        assert!(output.contains("quality note 099"));
        assert!(!output.contains("quality note 100"));
        assert!(output.contains("5 additional quality notes omitted"));
        assert!(output.contains("ok status --json"));
    }

    #[test]
    fn document_corpus_mode_benchmark_preserves_context_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        fs::write(
            repo.join("docs/architecture.md"),
            "# Runtime\nintro\n## Rotation protocol\nThe quasar token rotates nightly.\nRun the verifier.\n## Failure\nEscalate safely.\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();

        for mode in [IndexMode::Fast, IndexMode::Balanced, IndexMode::Full] {
            let mut config = OkConfig::load_from_repo(repo).unwrap();
            config.history.enabled = false;
            config.scip.enabled = false;
            config.semantic.enabled = false;
            let snapshot = index_repo_with_config(repo, config, mode).unwrap();
            assert!(!snapshot
                .files
                .iter()
                .any(|file| file.path == Path::new("docs/architecture.md")));
            let report = snapshot
                .phase_reports
                .iter()
                .find(|report| report.phase == "document_corpus")
                .unwrap();
            assert_eq!(report.document_files, Some(1));
            assert!(report.document_sections.is_some_and(|count| count >= 3));
            assert!(report.duration_ms.is_some());

            let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
            let section = store
                .document_sections()
                .unwrap()
                .into_iter()
                .find(|section| {
                    section.path == Path::new("docs/architecture.md")
                        && section.heading_path == ["Runtime", "Rotation protocol"]
                })
                .unwrap();
            assert_eq!(
                section.line_range,
                open_kioku_core::LineRange { start: 3, end: 5 }
            );
            assert!(!section.content_hash.is_empty());
            assert!(store
                .get_file_by_path(Path::new("docs/architecture.md"))
                .unwrap()
                .is_none());

            let pack = build_context_pack(
                repo,
                &store,
                "Find the quasar token rotation protocol in the architecture documentation",
                10,
            )
            .unwrap();
            let result = pack
                .primary_files
                .iter()
                .find(|result| result.path == Path::new("docs/architecture.md"))
                .unwrap();
            assert_eq!(
                result.line_range,
                Some(open_kioku_core::LineRange { start: 3, end: 5 })
            );
            assert!(result
                .evidence
                .iter()
                .any(|value| { value == "document heading path: Runtime > Rotation protocol" }));
        }
    }
}

#[cfg(test)]
mod ri3_resolution_diagnostics_tests {
    use super::*;

    #[test]
    fn relationship_quality_persists_into_index_and_status_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ri3-diagnostics-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn callee() {}\npub fn caller() { callee(); }\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        let mut config = OkConfig::load_from_repo(repo).unwrap();
        config.index.resolution_mode = open_kioku_config::ResolutionMode::V2;
        config.scip.enabled = false;
        config.history.enabled = false;
        config.semantic.enabled = false;

        let snapshot = index_repo_with_config(repo, config, IndexMode::Full).unwrap();
        let report = snapshot
            .manifest
            .quality
            .resolution_quality
            .as_ref()
            .expect("V2 indexing should expose relationship resolution diagnostics");
        let persisted = load_index_manifest(repo)
            .unwrap()
            .expect("index manifest should be persisted for status");
        let persisted_report = persisted
            .quality
            .resolution_quality
            .as_ref()
            .expect("status manifest should retain relationship diagnostics");
        assert_eq!(persisted_report, report);

        let json = serde_json::to_value(&persisted).unwrap();
        assert!(json.pointer("/quality/resolution_quality").is_some());

        let mut display_report = open_kioku_core::ResolutionQualityReport::default();
        display_report.by_relationship.insert(
            "calls".into(),
            open_kioku_core::RelationshipResolutionQuality {
                candidates_considered: 3,
                proven: 1,
                ambiguous: 1,
                unresolved: 1,
                heuristic_candidates_retained: 2,
                ..open_kioku_core::RelationshipResolutionQuality::default()
            },
        );
        let lines = relationship_resolution_summary_lines(&display_report);
        assert!(lines.iter().any(|line| {
            line.contains("calls:")
                && line.contains("1 proven / 3 candidates")
                && line.contains("1 ambiguous")
                && line.contains("1 unresolved")
                && line.contains("2 heuristic candidates retained")
        }));
    }
}
