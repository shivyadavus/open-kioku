fn is_live_relationship_observation_path(path: &Path) -> bool {
    path == Path::new("@live")
}

fn produce_live_relationship_observations(
    corpus: &RelationshipBenchCorpus,
) -> anyhow::Result<RelationshipBenchObservationSet> {
    validate_live_relationship_scenario_coverage(corpus)?;
    let mut cases = corpus.cases.iter().collect::<Vec<_>>();
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    let mut observations = Vec::with_capacity(cases.len());
    for case in cases {
        observations.push(produce_live_relationship_case(case)?);
    }
    Ok(RelationshipBenchObservationSet {
        metadata: live_relationship_run_metadata(corpus)?,
        observations,
    })
}

fn validate_live_relationship_scenario_coverage(
    corpus: &RelationshipBenchCorpus,
) -> anyhow::Result<()> {
    if corpus.status != RelationshipBenchCorpusStatus::Frozen {
        return Ok(());
    }
    const REQUIRED: [&str; 16] = [
        "same_simple_name",
        "unrelated_receiver",
        "alias_import",
        "nested_shadowing",
        "test_prod_collision",
        "constructor_function_collision",
        "static_instance_collision",
        "unknown_receiver",
        "dynamic_dispatch",
        "overload_ambiguity",
        "inheritance_collision",
        "local_import_shadowing",
        "multiple_exact_sites",
        "unresolved_external",
        "skipped_path",
        "malformed_partial",
    ];
    let scenarios = corpus
        .cases
        .iter()
        .map(|case| case.scenario.as_str())
        .collect::<BTreeSet<_>>();
    let missing = REQUIRED
        .iter()
        .copied()
        .filter(|scenario| !scenarios.contains(scenario))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "frozen relationship corpus is missing required adversarial scenario families: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

fn live_relationship_run_metadata(
    _corpus: &RelationshipBenchCorpus,
) -> anyhow::Result<RelationshipBenchRunMetadata> {
    let git_commit = ProcessCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("GITHUB_SHA").ok());
    let semantics_fingerprint = open_kioku_core::AnalysisSemanticsState::current().fingerprint;
    let adapter_versions = ["rust", "typescript", "javascript", "python", "java", "go"]
        .into_iter()
        .map(|language| {
            (
                language.to_string(),
                format!("open-kioku-tree-sitter@{}", env!("CARGO_PKG_VERSION")),
            )
        })
        .collect();
    Ok(RelationshipBenchRunMetadata {
        generated_at: None,
        git_commit,
        analysis_semantics_fingerprint: Some(semantics_fingerprint),
        adapter_versions,
        proof_policy_version: Some("ri3-proof-policy-v1".into()),
        index_mode: Some("full".into()),
        index_config: BTreeMap::from([
            ("resolution_mode".into(), serde_json::json!("shadow")),
            ("semantic".into(), serde_json::json!(false)),
            ("history".into(), serde_json::json!(false)),
            (
                "reference_exact_occurrence_source".into(),
                serde_json::json!("deterministic SCIP-equivalent fixture injection after parser symbolization"),
            ),
        ]),
    })
}

fn produce_live_relationship_case(
    case: &RelationshipBenchCase,
) -> anyhow::Result<RelationshipBenchObservation> {
    let root = live_case_root(&case.id);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    let result = (|| -> anyhow::Result<RelationshipBenchObservation> {
        write_live_relationship_fixture(case, &root)?;
        OkConfig::write_default(root.join("ok.toml"))?;
        let mut config = OkConfig::load_from_repo(&root)?;
        config.index.resolution_mode = open_kioku_config::ResolutionMode::Shadow;
        config.scip.enabled = false;
        config.history.enabled = false;
        config.semantic.enabled = false;
        let mut snapshot = Indexer::default().index_repo_with_mode(&root, &config, IndexMode::Full)?;
        inject_reference_fixture_occurrence(case, &mut snapshot)?;
        if case.scenario == "metamorphic_b" {
            // Exercise order independence after parsing/indexing: graph construction and proof
            // normalization must not depend on discovery/insertion order of persisted evidence.
            snapshot.files.reverse();
            snapshot.symbols.reverse();
            snapshot.chunks.reverse();
            snapshot.occurrences.reverse();
            snapshot.imports.reverse();
            snapshot.analysis_facts.reverse();
            snapshot.resolved_relationships.reverse();
        }
        let graph = InMemoryGraph::from_index_with_resolved_relationships(
            &snapshot.files,
            &snapshot.symbols,
            &snapshot.chunks,
            &snapshot.occurrences,
            &snapshot.imports,
            &snapshot.analysis_facts,
            &snapshot.resolved_relationships,
        );
        let source_node = resolve_live_endpoint(case, &case.source, &snapshot, &graph)?;
        let expected_target_node = case
            .expected_target
            .as_ref()
            .map(|target| resolve_live_endpoint(case, target, &snapshot, &graph))
            .transpose()?;
        let mut relationships = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == case.relationship && edge.from == source_node)
            .map(|edge| {
                observed_relationship_from_graph_edge(case, edge, expected_target_node.as_ref(), &graph)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        normalize_observed_relationships(&mut relationships);
        if case.scenario == "multiple_exact_sites"
            && case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit
        {
            let distinct_sites = relationships
                .iter()
                .filter(|relationship| {
                    relationship.authority
                        == open_kioku_core::RelationshipAuthority::Authoritative
                })
                .flat_map(|relationship| relationship.source_ranges.iter())
                .map(|range| {
                    (
                        range.start_line,
                        range.start_column,
                        range.end_line,
                        range.end_column,
                    )
                })
                .collect::<BTreeSet<_>>();
            if distinct_sites.len() < 2 {
                anyhow::bail!(
                    "case {} expected at least two exact reference sites, observed {}",
                    case.id,
                    distinct_sites.len()
                );
            }
        }
        let authoritative = relationships
            .iter()
            .filter(|relationship| {
                relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
            })
            .collect::<Vec<_>>();
        let outcome = if authoritative.is_empty() {
            if case.expected_outcome
                == RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge
            {
                RelationshipBenchObservedOutcome::Ambiguous
            } else {
                RelationshipBenchObservedOutcome::Unresolved
            }
        } else {
            RelationshipBenchObservedOutcome::Proven
        };
        let candidate_count = relationships
            .iter()
            .flat_map(|relationship| relationship_candidate_counts(relationship, &graph, &source_node))
            .max();
        Ok(RelationshipBenchObservation {
            case_id: case.id.clone(),
            outcome,
            candidate_count,
            relationships,
        })
    })();
    let _ = fs::remove_dir_all(&root);
    result
}

fn live_case_root(case_id: &str) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(case_id.as_bytes()));
    std::env::temp_dir().join(format!(
        "open-kioku-ri3-{}-{}",
        std::process::id(),
        &digest[..12]
    ))
}

fn resolve_live_endpoint(
    case: &RelationshipBenchCase,
    endpoint: &RelationshipBenchEndpoint,
    snapshot: &open_kioku_ingest::IndexSnapshot,
    graph: &InMemoryGraph,
) -> anyhow::Result<open_kioku_core::NodeId> {
    use open_kioku_core::GraphNodeType;
    match endpoint.kind {
        RelationshipBenchEndpointKind::Symbol => {
            let matches = snapshot
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.name == endpoint.selector || symbol.qualified_name == endpoint.selector
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                anyhow::bail!(
                    "case {} expected one symbol matching `{}`, found {}",
                    case.id,
                    endpoint.selector,
                    matches.len()
                );
            }
            Ok(open_kioku_core::identity::symbol_node_id(matches[0]))
        }
        RelationshipBenchEndpointKind::File => {
            let normalized = endpoint.selector.replace('\\', "/");
            let matches = snapshot
                .files
                .iter()
                .filter(|file| file.path.to_string_lossy().replace('\\', "/") == normalized)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                anyhow::bail!(
                    "case {} expected one file matching `{}`, found {}",
                    case.id,
                    endpoint.selector,
                    matches.len()
                );
            }
            Ok(open_kioku_core::identity::file_node_id(&matches[0].path))
        }
        RelationshipBenchEndpointKind::Module | RelationshipBenchEndpointKind::Package => {
            let expected_type = if endpoint.kind == RelationshipBenchEndpointKind::Module {
                GraphNodeType::Module
            } else {
                GraphNodeType::Package
            };
            let matches = graph
                .nodes
                .values()
                .filter(|node| node.node_type == expected_type && node.label == endpoint.selector)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                anyhow::bail!(
                    "case {} expected one {:?} node matching `{}`, found {}",
                    case.id,
                    endpoint.kind,
                    endpoint.selector,
                    matches.len()
                );
            }
            Ok(matches[0].id.clone())
        }
    }
}

fn observed_relationship_from_graph_edge(
    case: &RelationshipBenchCase,
    edge: &GraphEdge,
    expected_target_node: Option<&open_kioku_core::NodeId>,
    graph: &InMemoryGraph,
) -> anyhow::Result<RelationshipBenchObservedRelationship> {
    let proofs = edge.relationship_proofs();
    let target_identity = if expected_target_node.is_some_and(|target| target == &edge.to) {
        case.expected_target
            .as_ref()
            .expect("expected target node exists only for MustEmit case")
            .identity
            .clone()
    } else {
        graph
            .nodes
            .get(&edge.to.0)
            .map(canonical_live_node_identity)
            .unwrap_or_else(|| format!("node:{}", edge.to.0))
    };
    let proof_kinds = proofs.iter().map(|proof| proof.kind).collect();
    let resolver_strategies = proofs
        .iter()
        .filter(|proof| !proof.resolver_strategy.is_empty())
        .map(|proof| proof.resolver_strategy.clone())
        .collect();
    Ok(RelationshipBenchObservedRelationship {
        source_identity: case.source.identity.clone(),
        target_identity,
        relationship: edge.edge_type.clone(),
        authority: edge.relationship_authority(),
        proof_kinds,
        source_ranges: exact_ranges_from_graph_edge(edge),
        resolver_strategies,
    })
}

fn relationship_candidate_counts(
    relationship: &RelationshipBenchObservedRelationship,
    graph: &InMemoryGraph,
    source_node: &open_kioku_core::NodeId,
) -> Vec<usize> {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.from == *source_node
                && edge.edge_type == relationship.relationship
                && edge.relationship_authority() == relationship.authority
        })
        .flat_map(|edge| edge.relationship_proofs())
        .map(|proof| proof.candidate_count)
        .filter(|count| *count > 0)
        .collect()
}

fn canonical_live_node_identity(node: &GraphNode) -> String {
    use open_kioku_core::GraphNodeType;
    match node.node_type {
        GraphNodeType::File => format!("file:{}", node.label.replace('\\', "/")),
        GraphNodeType::Module => format!("module:{}", node.label),
        GraphNodeType::Package => format!("package:{}", node.label),
        _ if node.symbol_id.is_some() => format!("symbol:{}", node.label),
        _ => format!("{:?}:{}", node.node_type, node.label),
    }
}

fn exact_ranges_from_graph_edge(edge: &GraphEdge) -> Vec<open_kioku_core::SourceRange> {
    let mut ranges = Vec::new();
    for key in ["call_sites", "reference_sites"] {
        let Some(sites) = edge.properties.get(key).and_then(|value| value.as_array()) else {
            continue;
        };
        for site in sites {
            let get = |name: &str| {
                site.get(name)
                    .and_then(|value| value.as_u64())
                    .and_then(|value| u32::try_from(value).ok())
            };
            let (Some(start_line), Some(start_column), Some(end_line), Some(end_column)) = (
                get("start_line"),
                get("start_column"),
                get("end_line"),
                get("end_column"),
            ) else {
                continue;
            };
            ranges.push(open_kioku_core::SourceRange {
                start_line,
                start_column,
                end_line,
                end_column,
            });
        }
    }
    ranges.sort_by_key(|range| {
        (
            range.start_line,
            range.start_column,
            range.end_line,
            range.end_column,
        )
    });
    ranges.dedup();
    ranges
}

fn inject_reference_fixture_occurrence(
    case: &RelationshipBenchCase,
    snapshot: &mut open_kioku_ingest::IndexSnapshot,
) -> anyhow::Result<()> {
    if case.relationship != GraphEdgeType::References {
        return Ok(());
    }
    let should_inject_exact = case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit;
    let should_inject_heuristic = case.scenario == "heuristic_reference";
    if !should_inject_exact && !should_inject_heuristic {
        return Ok(());
    }
    let target_selector = case
        .expected_target
        .as_ref()
        .map(|target| target.selector.as_str())
        .unwrap_or("target_fn");
    let target = snapshot
        .symbols
        .iter()
        .find(|symbol| symbol.name == target_selector || symbol.qualified_name == target_selector)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "case {} reference fixture target `{}` was not parsed",
                case.id,
                target_selector
            )
        })?
        .clone();
    let source_file = snapshot
        .files
        .iter()
        .find(|file| {
            file.path.to_string_lossy().replace('\\', "/") == case.source.selector.replace('\\', "/")
        })
        .ok_or_else(|| anyhow::anyhow!("case {} reference source file was not parsed", case.id))?
        .clone();
    let range = case.expected_source_range.clone().unwrap_or(open_kioku_core::SourceRange {
        start_line: 2,
        start_column: 1,
        end_line: 2,
        end_column: 10,
    });
    snapshot.occurrences.push(open_kioku_core::SymbolOccurrence {
        symbol_id: target.id.clone(),
        file_id: source_file.id.clone(),
        range: Some(open_kioku_core::LineRange {
            start: range.start_line,
            end: range.end_line,
        }),
        source_range: Some(range.clone()),
        is_definition: false,
        confidence: if should_inject_exact {
            Confidence::Exact
        } else {
            Confidence::High
        },
        provenance: if should_inject_exact {
            EvidenceSourceType::Scip
        } else {
            EvidenceSourceType::TreeSitter
        },
    });
    if should_inject_exact && case.scenario == "multiple_exact_sites" {
        let second = open_kioku_core::SourceRange {
            start_line: range.end_line.saturating_add(1),
            start_column: 1,
            end_line: range.end_line.saturating_add(1),
            end_column: 10,
        };
        snapshot.occurrences.push(open_kioku_core::SymbolOccurrence {
            symbol_id: target.id,
            file_id: source_file.id,
            range: Some(open_kioku_core::LineRange {
                start: second.start_line,
                end: second.end_line,
            }),
            source_range: Some(second),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        });
    }
    Ok(())
}

fn write_live_relationship_fixture(case: &RelationshipBenchCase, root: &Path) -> anyhow::Result<()> {
    let files = live_fixture_files(case)?;
    for (path, content) in files {
        let absolute = root.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(absolute, content)?;
    }
    if case.scenario == "metamorphic_b" || case.scenario == "unrelated_file" {
        let (path, content) = unrelated_live_fixture_file(case.language);
        let absolute = root.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(absolute, content)?;
    }
    if case.scenario == "skipped_path" {
        // Put real relationship-shaped source in a vendor/generated path. If secure ingest ever
        // leaks skipped source, endpoint identity or structural precision will change.
        let relative = PathBuf::from("vendor/generated").join(main_path(case.language));
        let skipped = root.join(relative);
        fs::create_dir_all(skipped.parent().expect("skipped fixture has parent"))?;
        fs::write(skipped, positive_call_source(case.language))?;
    }
    if case.scenario == "malformed_partial" {
        let (path, content) = malformed_live_fixture_file(case.language);
        let absolute = root.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(absolute, content)?;
    }
    Ok(())
}

fn malformed_live_fixture_file(language: RelationshipBenchLanguage) -> (PathBuf, &'static str) {
    match language {
        RelationshipBenchLanguage::Rust => (PathBuf::from("src/broken.rs"), "pub fn broken( {"),
        RelationshipBenchLanguage::TypeScript => (PathBuf::from("src/broken.ts"), "export function broken( {"),
        RelationshipBenchLanguage::JavaScript => (PathBuf::from("src/broken.js"), "export function broken( {"),
        RelationshipBenchLanguage::Python => (PathBuf::from("src/broken.py"), "def broken(:\n    pass\n"),
        RelationshipBenchLanguage::Java => (PathBuf::from("src/Broken.java"), "class Broken { void broken( { }"),
        RelationshipBenchLanguage::Go => (PathBuf::from("broken.go"), "package bench\nfunc Broken( {\n"),
    }
}

fn live_fixture_files(case: &RelationshipBenchCase) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let positive_syntax = case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit
        || (case.capability_state != RelationshipBenchCapabilityState::Authoritative
            && matches!(case.scenario.as_str(), "metamorphic_a" | "metamorphic_b" | "unsupported_feature"));
    let adversarial = !positive_syntax;
    let files = match &case.relationship {
        GraphEdgeType::Calls => live_call_fixture(case.language, adversarial, &case.scenario),
        GraphEdgeType::References => live_reference_fixture(case.language),
        GraphEdgeType::UsesType => live_type_fixture(case.language, positive_syntax),
        GraphEdgeType::Implements => live_implements_fixture(case.language, positive_syntax),
        GraphEdgeType::Extends => live_extends_fixture(case.language, positive_syntax),
        GraphEdgeType::Imports => live_import_fixture(case.language, positive_syntax, false),
        GraphEdgeType::DependsOn => live_import_fixture(case.language, positive_syntax, true),
        other => anyhow::bail!("unsupported live relationship fixture family: {other:?}"),
    };
    Ok(files)
}

fn main_path(language: RelationshipBenchLanguage) -> &'static str {
    match language {
        RelationshipBenchLanguage::Rust => "src/main.rs",
        RelationshipBenchLanguage::TypeScript => "src/main.ts",
        RelationshipBenchLanguage::JavaScript => "src/main.js",
        RelationshipBenchLanguage::Python => "src/main.py",
        RelationshipBenchLanguage::Java => "src/Bench.java",
        RelationshipBenchLanguage::Go => "main.go",
    }
}

fn live_call_fixture(
    language: RelationshipBenchLanguage,
    adversarial: bool,
    scenario: &str,
) -> Vec<(PathBuf, String)> {
    let content = if adversarial {
        adversarial_call_source(language, scenario)
    } else {
        positive_call_source(language)
    };
    vec![(PathBuf::from(main_path(language)), content)]
}

fn positive_call_source(language: RelationshipBenchLanguage) -> String {
    match language {
        RelationshipBenchLanguage::Rust => {
            "pub fn target_fn() {}\npub fn caller_fn() { target_fn(); }\n".into()
        }
        RelationshipBenchLanguage::TypeScript => {
            "export function targetFn() {}\nexport function callerFn() { targetFn(); }\n".into()
        }
        RelationshipBenchLanguage::JavaScript => {
            "export function targetFn() {}\nexport function callerFn() { targetFn(); }\n".into()
        }
        RelationshipBenchLanguage::Python => {
            "def target_fn():\n    pass\n\ndef caller_fn():\n    target_fn()\n".into()
        }
        RelationshipBenchLanguage::Java => {
            "class Bench {\n  static void targetFn() {}\n  static void callerFn() { targetFn(); }\n}\n".into()
        }
        RelationshipBenchLanguage::Go => {
            "package bench\nfunc TargetFn() {}\nfunc CallerFn() { TargetFn() }\n".into()
        }
    }
}

fn adversarial_call_source(language: RelationshipBenchLanguage, scenario: &str) -> String {
    match (language, scenario) {
        (RelationshipBenchLanguage::Rust, "same_simple_name") => "mod a { pub fn target_fn() {} }\nmod b { pub fn target_fn() {} }\npub fn caller_fn() { target_fn(); }\n".into(),
        (RelationshipBenchLanguage::TypeScript | RelationshipBenchLanguage::JavaScript, "same_simple_name") => "const a = { targetFn() {} };\nconst b = { targetFn() {} };\nexport function callerFn(value) { value.targetFn(); }\n".into(),
        (RelationshipBenchLanguage::Python, "same_simple_name") => "class A:\n    def target_fn(self): pass\nclass B:\n    def target_fn(self): pass\ndef caller_fn(value):\n    value.target_fn()\n".into(),
        (RelationshipBenchLanguage::Java, "same_simple_name") => "class A { void targetFn() {} }\nclass B { void targetFn() {} }\nclass Bench { void callerFn(Object value) {} }\n".into(),
        (RelationshipBenchLanguage::Go, "same_simple_name") => "package bench\ntype A struct{}\ntype B struct{}\nfunc (A) TargetFn() {}\nfunc (B) TargetFn() {}\nfunc CallerFn(value interface{}) {}\n".into(),
        (RelationshipBenchLanguage::Rust, "nested_shadowing" | "local_import_shadowing") => "pub fn target_fn() {}\npub fn caller_fn() { let target_fn = || {}; target_fn(); }\n".into(),
        (RelationshipBenchLanguage::TypeScript | RelationshipBenchLanguage::JavaScript, "nested_shadowing" | "local_import_shadowing") => "export function targetFn() {}\nexport function callerFn() { const targetFn = () => {}; targetFn(); }\n".into(),
        (RelationshipBenchLanguage::Python, "nested_shadowing" | "local_import_shadowing") => "def target_fn(): pass\ndef caller_fn():\n    target_fn = lambda: None\n    target_fn()\n".into(),
        (RelationshipBenchLanguage::Java, "static_instance_collision" | "overload_ambiguity") => "class Bench {\n  static void targetFn() {}\n  void targetFn(int value) {}\n  void callerFn(Object unknown) {}\n}\n".into(),
        (RelationshipBenchLanguage::Go, "static_instance_collision" | "overload_ambiguity") => "package bench\ntype TargetType struct{}\nfunc (TargetType) TargetFn() {}\nfunc CallerFn(value interface{}) {}\n".into(),
        (RelationshipBenchLanguage::Rust, "unknown_receiver" | "dynamic_dispatch" | "unrelated_receiver") => "pub trait Dyn { fn target_fn(&self); }\npub fn caller_fn<T: Dyn>(value: T) { value.target_fn(); }\n".into(),
        (RelationshipBenchLanguage::TypeScript | RelationshipBenchLanguage::JavaScript, "unknown_receiver" | "dynamic_dispatch" | "unrelated_receiver") => "export function callerFn(value) { value.targetFn(); }\n".into(),
        (RelationshipBenchLanguage::Python, "unknown_receiver" | "dynamic_dispatch" | "unrelated_receiver") => "def caller_fn(value):\n    value.target_fn()\n".into(),
        (RelationshipBenchLanguage::Java, "unknown_receiver" | "dynamic_dispatch" | "unrelated_receiver") => "class Bench { void callerFn(Object value) {} }\n".into(),
        (RelationshipBenchLanguage::Go, "unknown_receiver" | "dynamic_dispatch" | "unrelated_receiver") => "package bench\nfunc CallerFn(value interface{}) {}\n".into(),
        _ => missing_call_source(language),
    }
}

fn missing_call_source(language: RelationshipBenchLanguage) -> String {
    match language {
        RelationshipBenchLanguage::Rust => "pub fn caller_fn() { missing_target(); }\n".into(),
        RelationshipBenchLanguage::TypeScript => "export function callerFn() { missingTarget(); }\n".into(),
        RelationshipBenchLanguage::JavaScript => "export function callerFn() { missingTarget(); }\n".into(),
        RelationshipBenchLanguage::Python => "def caller_fn():\n    missing_target()\n".into(),
        RelationshipBenchLanguage::Java => "class Bench { void callerFn() {} }\n".into(),
        RelationshipBenchLanguage::Go => "package bench\nfunc CallerFn() {}\n".into(),
    }
}

fn live_reference_fixture(language: RelationshipBenchLanguage) -> Vec<(PathBuf, String)> {
    vec![(PathBuf::from(main_path(language)), reference_source(language))]
}

fn reference_source(language: RelationshipBenchLanguage) -> String {
    match language {
        RelationshipBenchLanguage::Rust => "pub fn target_fn() {}\npub fn caller_fn() {}\n".into(),
        RelationshipBenchLanguage::TypeScript => "export function targetFn() {}\nexport function callerFn() {}\n".into(),
        RelationshipBenchLanguage::JavaScript => "export function targetFn() {}\nexport function callerFn() {}\n".into(),
        RelationshipBenchLanguage::Python => "def target_fn(): pass\ndef caller_fn(): pass\n".into(),
        RelationshipBenchLanguage::Java => "class Bench { static void targetFn() {} static void callerFn() {} }\n".into(),
        RelationshipBenchLanguage::Go => "package bench\nfunc TargetFn() {}\nfunc CallerFn() {}\n".into(),
    }
}

fn live_type_fixture(
    language: RelationshipBenchLanguage,
    positive_syntax: bool,
) -> Vec<(PathBuf, String)> {
    let content = if positive_syntax {
        match language {
            RelationshipBenchLanguage::Rust => "pub struct TargetType;\npub fn caller_fn(value: TargetType) { let _ = value; }\n".into(),
            RelationshipBenchLanguage::TypeScript => "export class TargetType {}\nexport function callerFn(value: TargetType) { void value; }\n".into(),
            RelationshipBenchLanguage::JavaScript => "/** @typedef {{value:number}} TargetType */\nexport function callerFn(value) { void value; }\n".into(),
            RelationshipBenchLanguage::Python => "class TargetType: pass\ndef caller_fn(value: TargetType): pass\n".into(),
            RelationshipBenchLanguage::Java => "class TargetType {}\nclass Bench { void callerFn(TargetType value) {} }\n".into(),
            RelationshipBenchLanguage::Go => "package bench\ntype TargetType struct{}\nfunc CallerFn(value TargetType) {}\n".into(),
        }
    } else {
        match language {
            RelationshipBenchLanguage::Rust => "pub fn caller_fn<T>(value: T) { let _ = value; }\n".into(),
            RelationshipBenchLanguage::TypeScript => "export function callerFn(value: unknown) { void value; }\n".into(),
            RelationshipBenchLanguage::JavaScript => "export function callerFn(value) { void value; }\n".into(),
            RelationshipBenchLanguage::Python => "def caller_fn(value): pass\n".into(),
            RelationshipBenchLanguage::Java => "class Bench { void callerFn(Object value) {} }\n".into(),
            RelationshipBenchLanguage::Go => "package bench\nfunc CallerFn(value interface{}) {}\n".into(),
        }
    };
    vec![(PathBuf::from(main_path(language)), content)]
}

fn live_implements_fixture(
    language: RelationshipBenchLanguage,
    positive_syntax: bool,
) -> Vec<(PathBuf, String)> {
    let content = if positive_syntax {
        match language {
            RelationshipBenchLanguage::Rust => "pub trait TargetType {}\npub struct SourceType;\nimpl TargetType for SourceType {}\n".into(),
            RelationshipBenchLanguage::TypeScript => "interface TargetType {}\nclass SourceType implements TargetType {}\n".into(),
            RelationshipBenchLanguage::JavaScript => "class SourceType {}\n".into(),
            RelationshipBenchLanguage::Python => "class SourceType: pass\n".into(),
            RelationshipBenchLanguage::Java => "interface TargetType {}\nclass SourceType implements TargetType {}\n".into(),
            RelationshipBenchLanguage::Go => "package bench\ntype TargetType interface{ Target() }\ntype SourceType struct{}\nfunc (SourceType) Target() {}\n".into(),
        }
    } else {
        source_type_only(language)
    };
    vec![(PathBuf::from(main_path(language)), content)]
}

fn live_extends_fixture(
    language: RelationshipBenchLanguage,
    positive_syntax: bool,
) -> Vec<(PathBuf, String)> {
    let content = if positive_syntax {
        match language {
            RelationshipBenchLanguage::Rust => "pub struct TargetType;\npub struct SourceType;\n".into(),
            RelationshipBenchLanguage::TypeScript => "class TargetType {}\nclass SourceType extends TargetType {}\n".into(),
            RelationshipBenchLanguage::JavaScript => "class TargetType {}\nclass SourceType extends TargetType {}\n".into(),
            RelationshipBenchLanguage::Python => "class TargetType: pass\nclass SourceType(TargetType): pass\n".into(),
            RelationshipBenchLanguage::Java => "class TargetType {}\nclass SourceType extends TargetType {}\n".into(),
            RelationshipBenchLanguage::Go => "package bench\ntype TargetType struct{}\ntype SourceType struct{}\n".into(),
        }
    } else {
        source_type_only(language)
    };
    vec![(PathBuf::from(main_path(language)), content)]
}

fn source_type_only(language: RelationshipBenchLanguage) -> String {
    match language {
        RelationshipBenchLanguage::Rust => "pub struct SourceType;\n".into(),
        RelationshipBenchLanguage::TypeScript => "class SourceType {}\n".into(),
        RelationshipBenchLanguage::JavaScript => "class SourceType {}\n".into(),
        RelationshipBenchLanguage::Python => "class SourceType: pass\n".into(),
        RelationshipBenchLanguage::Java => "class SourceType {}\n".into(),
        RelationshipBenchLanguage::Go => "package bench\ntype SourceType struct{}\n".into(),
    }
}

fn live_import_fixture(
    language: RelationshipBenchLanguage,
    positive_syntax: bool,
    dependency: bool,
) -> Vec<(PathBuf, String)> {
    let content = if positive_syntax {
        import_source(language, dependency)
    } else {
        no_import_source(language)
    };
    vec![(PathBuf::from(main_path(language)), content)]
}

fn import_source(language: RelationshipBenchLanguage, _dependency: bool) -> String {
    match language {
        RelationshipBenchLanguage::Rust => "use std::fmt;\npub fn caller_fn() { let _ = fmt::Error; }\n".into(),
        RelationshipBenchLanguage::TypeScript => "import * as fs from \"node:fs\";\nexport function callerFn() { void fs; }\n".into(),
        RelationshipBenchLanguage::JavaScript => "import * as fs from \"node:fs\";\nexport function callerFn() { void fs; }\n".into(),
        RelationshipBenchLanguage::Python => "import os\ndef caller_fn():\n    return os.name\n".into(),
        RelationshipBenchLanguage::Java => "import java.util.List;\nclass Bench { List<String> value; }\n".into(),
        RelationshipBenchLanguage::Go => "package bench\nimport \"fmt\"\nfunc CallerFn() { fmt.Println(\"ok\") }\n".into(),
    }
}

fn no_import_source(language: RelationshipBenchLanguage) -> String {
    match language {
        RelationshipBenchLanguage::Rust => "pub fn caller_fn() {}\n".into(),
        RelationshipBenchLanguage::TypeScript => "export function callerFn() {}\n".into(),
        RelationshipBenchLanguage::JavaScript => "export function callerFn() {}\n".into(),
        RelationshipBenchLanguage::Python => "def caller_fn(): pass\n".into(),
        RelationshipBenchLanguage::Java => "class Bench {}\n".into(),
        RelationshipBenchLanguage::Go => "package bench\nfunc CallerFn() {}\n".into(),
    }
}

fn unrelated_live_fixture_file(language: RelationshipBenchLanguage) -> (PathBuf, String) {
    let path = match language {
        RelationshipBenchLanguage::Rust => "src/unrelated.rs",
        RelationshipBenchLanguage::TypeScript => "src/unrelated.ts",
        RelationshipBenchLanguage::JavaScript => "src/unrelated.js",
        RelationshipBenchLanguage::Python => "src/unrelated.py",
        RelationshipBenchLanguage::Java => "src/Unrelated.java",
        RelationshipBenchLanguage::Go => "unrelated.go",
    };
    let content = match language {
        RelationshipBenchLanguage::Rust => "pub fn unrelated_noise() {}\n",
        RelationshipBenchLanguage::TypeScript => "export function unrelatedNoise() {}\n",
        RelationshipBenchLanguage::JavaScript => "export function unrelatedNoise() {}\n",
        RelationshipBenchLanguage::Python => "def unrelated_noise(): pass\n",
        RelationshipBenchLanguage::Java => "class Unrelated {}\n",
        RelationshipBenchLanguage::Go => "package bench\nfunc UnrelatedNoise() {}\n",
    };
    (PathBuf::from(path), content.into())
}

#[cfg(test)]
mod relationship_live_tests {
    use super::*;

    #[test]
    fn live_marker_is_explicit_and_not_a_real_path() {
        assert!(is_live_relationship_observation_path(Path::new("@live")));
        assert!(!is_live_relationship_observation_path(Path::new("observations.json")));
    }

    #[test]
    fn frozen_corpus_requires_all_adversarial_families() {
        let endpoint = RelationshipBenchEndpoint {
            kind: RelationshipBenchEndpointKind::Symbol,
            selector: "caller_fn".into(),
            identity: "symbol:caller".into(),
        };
        let corpus = RelationshipBenchCorpus {
            schema_version: RELATIONSHIP_BENCH_SCHEMA_VERSION.into(),
            corpus_version: "test".into(),
            status: RelationshipBenchCorpusStatus::Frozen,
            cases: vec![RelationshipBenchCase {
                id: "only-one".into(),
                fixture_id: "fixture".into(),
                split: RelationshipBenchSplit::Holdout,
                language: RelationshipBenchLanguage::Rust,
                relationship: GraphEdgeType::Calls,
                capability_state: RelationshipBenchCapabilityState::Authoritative,
                source: endpoint,
                expected_outcome: RelationshipBenchExpectedOutcome::MustNotEmit,
                expected_target: None,
                expected_source_range: None,
                expected_proof_kinds: BTreeSet::new(),
                forbidden_proof_kinds: BTreeSet::new(),
                candidate_count_expected: None,
                metamorphic_group: None,
                scenario: "same_simple_name".into(),
                notes: None,
            }],
        };
        assert!(validate_live_relationship_scenario_coverage(&corpus).is_err());
    }
}
