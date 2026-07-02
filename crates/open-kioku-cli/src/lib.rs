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
    ApiSurfaceConstraint, ChangeContractV1, ContractFile, ContractId, ContractStore,
    DependencyDeltaConstraint, FsContractStore, StoredContractRecord,
};
use open_kioku_core::{
    ChurnSummary, Confidence, ContextHandleId, EdgeId, EnforcedEdgeType, Evidence, EvidenceId,
    EvidenceSourceType, FileProvenance, GitChangeKind, GitCochangeEdge, GitCommitId,
    GitCommitRecord, GraphEdge, GraphEdgeType, GraphNode, HistoryRecordId, HistorySnapshot,
    HistorySummary, IndexManifest, IndexMode, NodeId, Owner, OwnerSuggestion, OwnershipEvidence,
    OwnershipReport, OwnershipSourceType, PlanReport, PolicyCheckReport, PolicyComponentMatch,
    PolicyExemptionEvidence, PolicyViolation, ProvenanceTouch, ReviewerAvailability,
    ReviewerEvidence, ReviewerRole, ReviewerSuggestionReport, ScoreComponent, SearchResult,
    SimilarChangeQuery, SimilarChangeReport, Symbol, SymbolId, SymbolProvenance,
};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::{IndexProgress, Indexer};
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{
    ChangeVerificationReport, ChangeVerifier, ContractVerificationReport, ContractVerifier,
    PatchPlanner, VerificationVerdict, VerifyChangeInput,
};
use open_kioku_plan::{ContractBuilder, PlanEngine, PlanFormat};
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

include!("types.rs");
include!("commands/mod.rs");
include!("commands/architecture.rs");
include!("reports/status_setup_doctor.rs");
include!("bench/mod.rs");
include!("commands/verification.rs");
include!("commands/contract.rs");
include!("reports/ranking.rs");
include!("reports/proof.rs");
include!("commands/context.rs");
include!("commands/index.rs");
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
}
