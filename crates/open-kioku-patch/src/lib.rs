use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_config::OkConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_core::{
    AnalysisFact, EvidenceSourceType, PatchId, PatchPlan, PlanReport, SearchResult, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_impact::ImpactEngine;
use open_kioku_storage::{MetadataStore, OkStore, SearchIndex};
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct PatchPlanner<'a> {
    config: &'a OkConfig,
    store: &'a dyn OkStore,
}

impl<'a> PatchPlanner<'a> {
    pub fn new(config: &'a OkConfig, store: &'a dyn OkStore) -> Self {
        Self { config, store }
    }

    pub fn plan(&self, task: &str) -> Result<PatchPlan> {
        let context = ContextPackBuilder::new(self.store).build(task, 12)?;
        Ok(PatchPlan {
            id: PatchId::new(stable_id(task)),
            task: task.into(),
            allowed_files: context.recommended_change_boundary.allowed_files,
            caution_files: context.recommended_change_boundary.caution_files,
            forbidden_files: context.recommended_change_boundary.forbidden_files,
            change_steps: vec![
                "Inspect primary symbols and definitions from the context pack".into(),
                "Constrain edits to allowed files unless evidence justifies expansion".into(),
                "Run the recommended validation plan after approval".into(),
            ],
            risks: context.risk_report.reasons,
            assumptions: vec![
                "Generated and vendor files remain out of scope".into(),
                "Patch application requires explicit write mode and approval".into(),
            ],
            tests: context.test_candidates,
            rollback_notes: vec!["Revert the unified diff if validation fails".into()],
            unified_diff: None,
            requires_approval: self.config.security.approval_required,
            evidence: context.evidence,
        })
    }

    pub fn apply(&self, _patch: &PatchPlan, approved: bool) -> Result<()> {
        PolicyGate::new(self.config).ensure_allowed(ActionKind::ApplyPatch)?;
        if self.config.security.approval_required && !approved {
            return Err(OkError::PolicyDenied(
                "patch application requires explicit approval".into(),
            ));
        }
        Err(OkError::Unsupported(
            "patch application is intentionally not implemented without a diff applicator".into(),
        ))
    }
}

pub struct ChangeVerifier<'a> {
    store: &'a dyn OkStore,
    search_index: Option<&'a dyn SearchIndex>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyChangeInput {
    #[serde(default)]
    pub changed_files: Vec<PathBuf>,
    #[serde(default)]
    pub unified_diff: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub run_commands: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeVerificationReport {
    pub verdict: VerificationVerdict,
    pub changed_files: Vec<PathBuf>,
    pub changed_symbols: Vec<String>,
    pub boundary_violations: Vec<VerificationFinding>,
    pub warnings: Vec<VerificationFinding>,
    pub missing_tests: Vec<VerificationFinding>,
    pub changed_impact: Vec<VerificationFinding>,
    pub recommended_tests: Vec<TestTarget>,
    pub command_results: Vec<ValidationCommandResult>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFinding {
    pub path: Option<PathBuf>,
    pub kind: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCommandResult {
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl<'a> ChangeVerifier<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            search_index: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn verify(
        &self,
        repo: &Path,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ChangeVerificationReport> {
        let changed_files = changed_files_from_input(&input);
        if changed_files.is_empty() {
            return Err(OkError::Config(
                "verify requires at least one changed file or a non-empty unified diff".into(),
            ));
        }

        let boundary_violations = boundary_violations(plan, &changed_files, &input.evidence_refs);
        let changed_symbols = changed_symbols(self.store, &changed_files)?;
        let recommended_tests = recommended_tests(self.store, &changed_files)?;
        let missing_tests = missing_tests(plan, &recommended_tests);
        let changed_impact = changed_impact(self.store, self.search_index, plan, &changed_files)?;
        let command_results = if input.run_commands {
            run_validation_commands(repo, plan)
        } else {
            Vec::new()
        };
        let command_failures = command_results
            .iter()
            .filter(|result| result.status == "fail")
            .map(|result| VerificationFinding {
                path: None,
                kind: "command_failed".into(),
                reason: format!(
                    "validation command `{}` exited with {:?}",
                    result.command, result.exit_code
                ),
                evidence_refs: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut warnings = Vec::new();
        warnings.extend(caution_warnings(plan, &changed_files));
        warnings.extend(expansion_warnings(
            plan,
            &changed_files,
            &input.evidence_refs,
        ));
        warnings.extend(runtime_warnings(self.store, &changed_files)?);

        let verdict = if !boundary_violations.is_empty() || !command_failures.is_empty() {
            VerificationVerdict::Fail
        } else if !warnings.is_empty() || !missing_tests.is_empty() || !changed_impact.is_empty() {
            VerificationVerdict::Warn
        } else {
            VerificationVerdict::Pass
        };

        let mut all_boundary_violations = boundary_violations;
        all_boundary_violations.extend(command_failures);

        Ok(ChangeVerificationReport {
            verdict,
            changed_files,
            changed_symbols,
            boundary_violations: all_boundary_violations,
            warnings,
            missing_tests,
            changed_impact,
            recommended_tests,
            command_results,
            evidence_refs: input.evidence_refs,
        })
    }
}

pub fn changed_files_from_unified_diff(diff: &str) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut pending_old: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if let Some(path) = parts.get(1).and_then(|part| part.strip_prefix("b/")) {
                paths.insert(PathBuf::from(path));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("--- ") {
            pending_old = diff_path(path);
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = diff_path(path).or_else(|| pending_old.take()) {
                paths.insert(PathBuf::from(path));
            }
        }
    }
    paths.into_iter().collect()
}

fn changed_files_from_input(input: &VerifyChangeInput) -> Vec<PathBuf> {
    let mut paths = input
        .changed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    if let Some(diff) = &input.unified_diff {
        paths.extend(
            changed_files_from_unified_diff(diff)
                .into_iter()
                .map(|p| normalize_path(&p)),
        );
    }
    paths.into_iter().map(PathBuf::from).collect()
}

fn diff_path(raw: &str) -> Option<String> {
    let path = raw.split_whitespace().next().unwrap_or_default();
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_string(),
    )
}

fn boundary_violations(
    plan: &PlanReport,
    changed_files: &[PathBuf],
    evidence_refs: &[String],
) -> Vec<VerificationFinding> {
    let boundary = &plan.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let mut findings = Vec::new();
    for path in changed_files {
        let normalized = normalize_path(path);
        if let Some(rule) = boundary
            .forbidden_rules
            .iter()
            .find(|rule| boundary_pattern_matches(&rule.pattern, &normalized))
        {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "forbidden_boundary".into(),
                reason: format!(
                    "matches forbidden pattern `{}`: {}",
                    rule.pattern, rule.reason
                ),
                evidence_refs: rule.evidence_refs.clone(),
            });
            continue;
        }
        if allowed.contains(&normalized) || caution.contains(&normalized) {
            continue;
        }
        if evidence_refs.is_empty() {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "out_of_boundary".into(),
                reason:
                    "path is outside the saved plan boundary and no expansion evidence was supplied"
                        .into(),
                evidence_refs: Vec::new(),
            });
        }
    }
    findings
}

fn caution_warnings(plan: &PlanReport, changed_files: &[PathBuf]) -> Vec<VerificationFinding> {
    let boundary = &plan.recommended_change_boundary;
    changed_files
        .iter()
        .filter_map(|path| {
            let normalized = normalize_path(path);
            boundary
                .caution_rules
                .iter()
                .find(|rule| normalize_path(&rule.path) == normalized)
                .map(|rule| VerificationFinding {
                    path: Some(path.clone()),
                    kind: "caution_boundary".into(),
                    reason: rule.reason.clone(),
                    evidence_refs: rule.evidence_refs.clone(),
                })
        })
        .collect()
}

fn expansion_warnings(
    plan: &PlanReport,
    changed_files: &[PathBuf],
    evidence_refs: &[String],
) -> Vec<VerificationFinding> {
    if evidence_refs.is_empty() {
        return Vec::new();
    }
    let boundary = &plan.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    changed_files
        .iter()
        .filter_map(|path| {
            let normalized = normalize_path(path);
            if allowed.contains(&normalized)
                || caution.contains(&normalized)
                || boundary
                    .forbidden_rules
                    .iter()
                    .any(|rule| boundary_pattern_matches(&rule.pattern, &normalized))
            {
                return None;
            }
            Some(VerificationFinding {
                path: Some(path.clone()),
                kind: "boundary_expansion".into(),
                reason: "path is outside the saved boundary but explicit expansion evidence was supplied".into(),
                evidence_refs: evidence_refs.to_vec(),
            })
        })
        .collect()
}

fn changed_symbols(store: &dyn MetadataStore, changed_files: &[PathBuf]) -> Result<Vec<String>> {
    let mut symbols = BTreeSet::new();
    for path in changed_files {
        if let Some(file) = store.get_file_by_path(path)? {
            for symbol in store.symbols_for_file(&file.id)? {
                symbols.insert(symbol.qualified_name);
            }
        }
    }
    Ok(symbols.into_iter().collect())
}

fn recommended_tests(store: &dyn OkStore, changed_files: &[PathBuf]) -> Result<Vec<TestTarget>> {
    let selector = TestSelector::new(store);
    let mut tests = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        for test in selector.for_changed_path_with_evidence(path, 8)? {
            if seen.insert(test.id.clone()) {
                tests.push(test);
            }
        }
    }
    Ok(tests)
}

fn missing_tests(plan: &PlanReport, recommended_tests: &[TestTarget]) -> Vec<VerificationFinding> {
    let planned = plan
        .validation
        .iter()
        .flat_map(|test| [test.id.clone(), test.name.clone()])
        .collect::<BTreeSet<_>>();
    recommended_tests
        .iter()
        .filter(|test| !planned.contains(&test.id) && !planned.contains(&test.name))
        .map(|test| VerificationFinding {
            path: Some(PathBuf::from(test.file_id.0.clone())),
            kind: "missing_test".into(),
            reason: format!("recommended test `{}` is not in the saved plan", test.name),
            evidence_refs: test.evidence_refs.clone(),
        })
        .collect()
}

fn changed_impact(
    store: &dyn OkStore,
    search_index: Option<&dyn SearchIndex>,
    plan: &PlanReport,
    changed_files: &[PathBuf],
) -> Result<Vec<VerificationFinding>> {
    let planned_impacts = plan
        .impact
        .direct_impacts
        .iter()
        .chain(plan.impact.indirect_impacts.iter())
        .map(|result| normalize_path(&result.path))
        .chain(
            plan.recommended_change_boundary
                .allowed_files
                .iter()
                .map(|path| normalize_path(path)),
        )
        .chain(
            plan.recommended_change_boundary
                .caution_files
                .iter()
                .map(|path| normalize_path(path)),
        )
        .collect::<BTreeSet<_>>();
    let impact_engine = ImpactEngine::new(store).with_search_index(search_index);
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let impact = impact_engine.for_file(path)?;
        for result in impact
            .direct_impacts
            .iter()
            .chain(impact.indirect_impacts.iter())
            .take(12)
        {
            let normalized = normalize_path(&result.path);
            if !planned_impacts.contains(&normalized) && seen.insert(normalized.clone()) {
                findings.push(impact_finding(result));
            }
        }
    }
    Ok(findings)
}

fn runtime_warnings(
    store: &dyn MetadataStore,
    changed_files: &[PathBuf],
) -> Result<Vec<VerificationFinding>> {
    let runtime_facts = store.analysis_facts(Some(EvidenceSourceType::Runtime), 500)?;
    if runtime_facts.is_empty() {
        return Ok(Vec::new());
    }
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let Some(file) = store.get_file_by_path(path)? else {
            continue;
        };
        for fact in runtime_facts
            .iter()
            .filter(|fact| fact.file_id == file.id)
            .take(5)
        {
            if seen.insert((normalize_path(path), fact.id.clone())) {
                findings.push(runtime_finding(path, fact));
            }
        }
    }
    Ok(findings)
}

fn runtime_finding(path: &Path, fact: &AnalysisFact) -> VerificationFinding {
    VerificationFinding {
        path: Some(path.to_path_buf()),
        kind: "nearby_runtime_signal".into(),
        reason: format!(
            "changed file has local runtime trace/log/incident evidence `{}`: {}",
            fact.target, fact.message
        ),
        evidence_refs: vec![fact.id.clone()],
    }
}

fn impact_finding(result: &SearchResult) -> VerificationFinding {
    VerificationFinding {
        path: Some(result.path.clone()),
        kind: "changed_impact".into(),
        reason: format!(
            "post-edit impact candidate was not present in the saved plan: {}",
            result.match_reason
        ),
        evidence_refs: result.derived_evidence_ids(),
    }
}

fn run_validation_commands(repo: &Path, plan: &PlanReport) -> Vec<ValidationCommandResult> {
    let mut seen = BTreeSet::new();
    let commands = plan
        .validation
        .iter()
        .filter_map(|test| test.command.clone())
        .filter(|command| seen.insert(command.clone()))
        .collect::<Vec<_>>();
    commands
        .into_iter()
        .map(|command| run_validation_command(repo, &command))
        .collect()
}

fn run_validation_command(repo: &Path, command: &str) -> ValidationCommandResult {
    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(repo)
        .output();
    match output {
        Ok(output) => ValidationCommandResult {
            command: command.into(),
            status: if output.status.success() {
                "pass".into()
            } else {
                "fail".into()
            },
            exit_code: output.status.code(),
            stdout: truncate_output(&String::from_utf8_lossy(&output.stdout)),
            stderr: truncate_output(&String::from_utf8_lossy(&output.stderr)),
        },
        Err(err) => ValidationCommandResult {
            command: command.into(),
            status: "fail".into(),
            exit_code: None,
            stdout: String::new(),
            stderr: truncate_output(&err.to_string()),
        },
    }
}

fn truncate_output(value: &str) -> String {
    const MAX: usize = 4000;
    if value.len() <= MAX {
        value.into()
    } else {
        format!("{}... <truncated>", &value[..MAX])
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn boundary_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").replace('\\', "/");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if let Some(middle) = prefix.strip_prefix("**/") {
            return path == middle
                || path.starts_with(&format!("{middle}/"))
                || path.contains(&format!("/{middle}/"));
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern.contains('*') {
        let mut remainder = path;
        for part in pattern.split('*').filter(|part| !part.is_empty()) {
            if let Some(index) = remainder.find(part) {
                remainder = &remainder[index + part.len()..];
            } else {
                return false;
            }
        }
        return true;
    }
    false
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        CodeChunk, Confidence, File, FileId, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType,
        Import, IndexManifest, Language, LineRange, RepositoryId, Symbol, SymbolId,
        SymbolOccurrence,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{GraphStore, IndexData};

    struct RuntimeStore {
        file: File,
        fact: AnalysisFact,
    }

    impl RuntimeStore {
        fn new() -> Self {
            let file = File {
                id: FileId::new("handler"),
                repository_id: RepositoryId::new("repo"),
                path: PathBuf::from("src/handler.rs"),
                language: Language::Rust,
                size_bytes: 100,
                content_hash: "handler".into(),
                is_generated: false,
                is_vendor: false,
            };
            let fact = AnalysisFact {
                id: "runtime-incident".into(),
                file_id: file.id.clone(),
                symbol_id: None,
                target: "panic in checkout flow".into(),
                target_kind: GraphNodeType::RuntimeError,
                edge_type: GraphEdgeType::FailedIn,
                range: Some(LineRange::single(9)),
                confidence: Confidence::High,
                source: "open-kioku-runtime:.ok/runtime/incidents.jsonl".into(),
                source_type: EvidenceSourceType::Runtime,
                message: "runtime incident observed in local log or failure artifact".into(),
            };
            Self { file, fact }
        }
    }

    impl MetadataStore for RuntimeStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, _limit: usize, _offset: usize) -> Result<Vec<File>> {
            Ok(vec![self.file.clone()])
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok((path == self.file.path).then(|| self.file.clone()))
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, _file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(Vec::new())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn analysis_facts(
            &self,
            source_type: Option<EvidenceSourceType>,
            _limit: usize,
        ) -> Result<Vec<AnalysisFact>> {
            if source_type == Some(EvidenceSourceType::Runtime) {
                Ok(vec![self.fact.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        fn references_for_symbol(
            &self,
            _id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }

        fn occurrences_for_file(&self, _file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }
    }

    impl GraphStore for RuntimeStore {
        fn replace_graph(&self, _nodes: &[GraphNode], _edges: &[GraphEdge]) -> Result<()> {
            Ok(())
        }

        fn neighbors(
            &self,
            _node: &str,
            _limit: usize,
        ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
            Ok((Vec::new(), Vec::new()))
        }

        fn shortest_path(
            &self,
            _from: &str,
            _to: &str,
            _max_depth: usize,
        ) -> Result<Vec<GraphEdge>> {
            Ok(Vec::new())
        }

        fn node_type_stats(
            &self,
        ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
            Ok(std::collections::HashMap::new())
        }

        fn edge_type_stats(
            &self,
        ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
            Ok(std::collections::HashMap::new())
        }
    }

    #[test]
    fn runtime_warnings_surface_nearby_incidents() {
        let store = RuntimeStore::new();
        let warnings = runtime_warnings(&store, &[PathBuf::from("src/handler.rs")]).unwrap();

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, "nearby_runtime_signal");
        assert!(warnings[0].reason.contains("panic in checkout flow"));
        assert_eq!(warnings[0].evidence_refs, vec!["runtime-incident"]);
    }
}
