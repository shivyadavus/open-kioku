//! `code_to_test` requires validation evidence. An index that cannot supply it, because it holds
//! no test target or none the runner would execute, reports a repository-level absence as a
//! caveat. Availability is decided from the indexed targets alone, so a stream that ran over a
//! usable target and matched nothing still blocks, wherever that target lives.

use chrono::Utc;
use open_kioku_context::ContextPackBuilder;
use open_kioku_core::{
    AnalysisSemanticsState, CodeChunk, Confidence, ContextPack, EvidenceSourceType, File, FileId,
    IndexManifest, IndexQuality, Language, LineRange, Repository, RepositoryId,
    RetrievalSourceKind, ScoreComponent, Symbol, SymbolId, SymbolKind, TaskFamily,
    TestSelectionTier, TestTarget, TestTargetOrigin, Visibility,
};
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::PathBuf;

const TASK: &str = "add tests for convertCurrency rounding";
const BLOCKER: &str =
    "context retrieval blocked because task-family required evidence was missing: validation";

/// Where the fixture's one test target lives. Every name shares no word with the task, so a
/// stream that runs over it stays silent.
#[derive(Clone, Copy)]
enum Targets {
    /// No test file and no target.
    None,
    /// `test/ledger_test.ts` holds the target.
    InTestFile,
    /// `test/ledger_test.ts` exists but holds nothing; `src/ledger.ts` holds a callable target.
    OnlyInSourceBesideTestFile,
    /// `test/ledger_test.ts` holds one registration target the runner skips.
    OnlyDisabled,
    /// No test file; `src/ledger.ts` holds the target, like a crate with only inline tests.
    OnlyInSource,
}

fn file(id: &str, path: &str) -> File {
    File {
        id: FileId::new(id),
        repository_id: RepositoryId::new("repo"),
        path: PathBuf::from(path),
        language: Language::TypeScript,
        size_bytes: 120,
        content_hash: format!("hash-{id}"),
        is_generated: false,
        is_vendor: false,
    }
}

fn function(id: &str, name: &str, file: &File, range: LineRange) -> Symbol {
    Symbol {
        id: SymbolId::new(id),
        name: name.into(),
        qualified_name: format!("src::{name}"),
        kind: SymbolKind::Function,
        file_id: file.id.clone(),
        range: Some(range),
        language: Language::TypeScript,
        confidence: Confidence::High,
        provenance: EvidenceSourceType::TreeSitter,
        module_id: None,
        parent_symbol_id: None,
        scope_id: None,
        signature: None,
        visibility: Visibility::Unknown,
    }
}

fn chunk(id: &str, symbol: &Symbol, text: &str) -> CodeChunk {
    CodeChunk {
        id: id.into(),
        file_id: symbol.file_id.clone(),
        range: symbol.range.clone().expect("fixture symbols carry a range"),
        language: Language::TypeScript,
        text: text.into(),
        symbol_id: Some(symbol.id.clone()),
    }
}

fn target(id: &str, symbol: &Symbol) -> TestTarget {
    typed_target(id, symbol, TestTargetOrigin::Symbol, Confidence::High)
}

fn typed_target(
    id: &str,
    symbol: &Symbol,
    origin: TestTargetOrigin,
    confidence: Confidence,
) -> TestTarget {
    TestTarget {
        selection_tier: TestSelectionTier::default(),
        tier_justification: Vec::new(),
        id: id.into(),
        name: symbol.name.clone(),
        file_id: symbol.file_id.clone(),
        range: symbol.range.clone(),
        command: None,
        confidence,
        reason: "fixture target".into(),
        evidence_refs: vec![id.into()],
        score_breakdown: vec![ScoreComponent::single(
            "indexed_test_confidence",
            confidence.score(),
            vec![id.into()],
            "fixture target",
        )],
        origin,
    }
}

/// A TypeScript repository with the rate conversion the task names and an unrelated ledger
/// module, plus a test file and one test target placed as `targets` says.
fn store(targets: Targets) -> SqliteStore {
    let store = SqliteStore::open(":memory:").unwrap();
    let rates = file("rates", "src/rates.ts");
    let ledger = file("ledger", "src/ledger.ts");
    let ledger_test = file("ledger-test", "test/ledger_test.ts");
    let convert = function(
        "convert-currency",
        "convertCurrency",
        &rates,
        LineRange { start: 1, end: 3 },
    );
    let post = function(
        "post-ledger-entry",
        "postLedgerEntry",
        &ledger,
        LineRange { start: 1, end: 3 },
    );
    let verify = function(
        "verify-ledger-balance",
        "verifyLedgerBalance",
        &ledger,
        LineRange { start: 5, end: 7 },
    );
    let parses_header = function(
        "parses-ledger-header",
        "parsesLedgerHeader",
        &ledger_test,
        LineRange { start: 1, end: 3 },
    );
    let mut files = vec![rates, ledger];
    let mut symbols = vec![convert.clone(), post.clone(), verify.clone()];
    let mut chunks = vec![
        chunk(
            "rates-convert",
            &convert,
            "export function convertCurrency(amount: number, rate: number): number {\n  return Math.round(amount * rate * 100) / 100;\n}",
        ),
        chunk(
            "ledger-post",
            &post,
            "export function postLedgerEntry(entry: LedgerEntry): void {\n  journal.append(entry);\n}",
        ),
    ];
    let mut tests = Vec::new();
    match targets {
        Targets::None => {}
        Targets::InTestFile => {
            chunks.push(chunk(
                "ledger-test-header",
                &parses_header,
                "function parsesLedgerHeader() {\n  expect(readHeader(sample)).toBeDefined();\n}",
            ));
            tests.push(target("ledger-header-target", &parses_header));
            files.push(ledger_test);
            symbols.push(parses_header);
        }
        Targets::OnlyInSourceBesideTestFile => {
            tests.push(target("ledger-balance-target", &verify));
            files.push(ledger_test);
        }
        Targets::OnlyInSource => {
            tests.push(target("ledger-balance-target", &verify));
        }
        Targets::OnlyDisabled => {
            tests.push(typed_target(
                "ledger-skipped-target",
                &parses_header,
                TestTargetOrigin::DisabledRegistrationCall,
                Confidence::Low,
            ));
            files.push(ledger_test);
        }
    }
    let quality = IndexQuality::default();
    let manifest = IndexManifest {
        analysis_semantics: Some(AnalysisSemanticsState::current()),
        repository: Repository {
            id: RepositoryId::new("repo"),
            name: "repo".into(),
            root: PathBuf::from("."),
            branch: None,
            commit: None,
            indexed_at: None,
        },
        file_count: files.len(),
        symbol_count: symbols.len(),
        chunk_count: chunks.len(),
        indexed_at: Utc::now(),
        schema_version: 1,
        index_mode: quality.index_mode,
        phase_reports: Vec::new(),
        quality,
        snapshot: None,
    };
    store
        .replace_index(IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &symbols,
            chunks: &chunks,
            tests: &tests,
            imports: &[],
            occurrences: &[],
            analysis_facts: &[],
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        })
        .unwrap();
    store
}

fn pack(targets: Targets) -> ContextPack {
    let store = store(targets);
    let pack = ContextPackBuilder::new(&store).build(TASK, 5).unwrap();
    assert_eq!(
        pack.retrieval_diagnostics.routing.task_family,
        TaskFamily::CodeToTest
    );
    pack
}

/// The pack keeps its exact primary result and reports validation as unavailable for `reason`.
fn assert_validation_unavailable(pack: &ContextPack, reason: &str) {
    let diagnostics = &pack.retrieval_diagnostics;
    assert_eq!(diagnostics.selection.abstention_reason, None);
    assert_eq!(
        pack.primary_files.first().map(|result| result.path.clone()),
        Some(PathBuf::from("src/rates.ts")),
        "{:?}",
        pack.primary_files
    );
    assert!(diagnostics
        .sources_attempted
        .contains(&RetrievalSourceKind::Validation));
    assert!(!diagnostics
        .sources_succeeded
        .contains(&RetrievalSourceKind::Validation));
    for caveat in [
        reason,
        "task-family required evidence: validation is unavailable in this repository",
    ] {
        assert!(
            diagnostics.caveats.iter().any(|item| item == caveat),
            "missing caveat `{caveat}` in {:?}",
            diagnostics.caveats
        );
    }
    assert!(pack
        .negative_evidence
        .iter()
        .any(|item| item.scope == "validation"));
    assert!(pack
        .confidence_breakdown
        .caveats
        .iter()
        .any(|caveat| caveat == "no validation target was selected"));
    assert!(!pack
        .confidence_breakdown
        .blockers
        .iter()
        .any(|blocker| blocker == BLOCKER));
    let validation = pack
        .confidence_breakdown
        .components
        .iter()
        .find(|component| component.signal == "validation_availability")
        .expect("validation_availability component");
    assert!((validation.raw_value - 0.2).abs() < f32::EPSILON);
}

fn assert_validation_blocks(pack: &ContextPack) {
    let diagnostics = &pack.retrieval_diagnostics;
    assert!(diagnostics
        .sources_succeeded
        .contains(&RetrievalSourceKind::Validation));
    assert_eq!(
        diagnostics.selection.abstention_reason.as_deref(),
        Some("missing_required_evidence:validation")
    );
    assert!(pack.primary_files.is_empty());
    assert!(pack
        .confidence_breakdown
        .blockers
        .iter()
        .any(|blocker| blocker == BLOCKER));
}

#[test]
fn code_to_test_without_indexed_test_targets_keeps_its_primary_result_with_a_caveat() {
    assert_validation_unavailable(
        &pack(Targets::None),
        "no test targets are indexed for this repository",
    );
}

#[test]
fn code_to_test_whose_only_targets_are_disabled_keeps_its_primary_result_with_a_caveat() {
    assert_validation_unavailable(
        &pack(Targets::OnlyDisabled),
        "every indexed test target is a disabled test the runner skips",
    );
}

/// Availability is a fact about targets, not a census of files: a repository whose test file
/// holds no target still has a usable target elsewhere, so the stream runs and the gate holds.
#[test]
fn code_to_test_with_a_test_file_holding_no_target_still_blocks() {
    assert_validation_blocks(&pack(Targets::OnlyInSourceBesideTestFile));
}

#[test]
fn code_to_test_whose_indexed_targets_match_nothing_still_blocks() {
    assert_validation_blocks(&pack(Targets::InTestFile));
}

#[test]
fn code_to_test_without_test_files_whose_source_targets_match_nothing_still_blocks() {
    assert_validation_blocks(&pack(Targets::OnlyInSource));
}
