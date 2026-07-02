fn absolutize(path: &Path) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        Ok(path.canonicalize()?)
    } else {
        Ok(path)
    }
}

fn snapshot_export(repo: &Path, quality: SnapshotQuality) -> anyhow::Result<SnapshotExportReport> {
    let repo = absolutize(repo)?;
    let index_path = index_sqlite_path(&repo);
    if !index_path.exists() {
        anyhow::bail!(
            "index database is missing at {}; run `ok index` first",
            index_path.display()
        );
    }

    let artifact_dir = snapshot_artifact_dir(&repo);
    fs::create_dir_all(&artifact_dir)?;
    ensure_snapshot_gitattributes(&artifact_dir)?;
    checkpoint_sqlite(&index_path);

    let temp_db = unique_temp_path(&artifact_dir, "index.snapshot", "sqlite.tmp");
    match quality {
        SnapshotQuality::Best => {
            let conn = Connection::open_with_flags(&index_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("opening {} read-only", index_path.display()))?;
            let temp_db_string = temp_db.to_string_lossy().to_string();
            conn.execute("VACUUM INTO ?1", params![temp_db_string])
                .with_context(|| {
                    format!("compacting snapshot database to {}", temp_db.display())
                })?;
        }
        SnapshotQuality::Fast => {
            fs::copy(&index_path, &temp_db).with_context(|| {
                format!(
                    "copying index database from {} to {}",
                    index_path.display(),
                    temp_db.display()
                )
            })?;
        }
    }

    integrity_check_sqlite(&temp_db)?;
    ensure_required_snapshot_tables(&temp_db)?;

    let manifest = read_manifest_from_sqlite(&temp_db)?
        .ok_or_else(|| anyhow::anyhow!("snapshot source database has no index manifest"))?;
    let graph_counts = read_graph_counts_from_sqlite(&temp_db)?;
    let sqlite_user_version = sqlite_user_version(&temp_db)?;
    let original_size_bytes = fs::metadata(&temp_db)?.len();

    let artifact_path = snapshot_artifact_path(&repo);
    let artifact_tmp = unique_temp_path(&artifact_dir, "index.snapshot", "zst.tmp");
    compress_file(&temp_db, &artifact_tmp, quality.compression_level())?;
    fs::rename(&artifact_tmp, &artifact_path).with_context(|| {
        format!(
            "promoting snapshot artifact {} to {}",
            artifact_tmp.display(),
            artifact_path.display()
        )
    })?;
    let compressed_size_bytes = fs::metadata(&artifact_path)?.len();

    let metadata = SnapshotMetadata {
        schema_version: SNAPSHOT_SCHEMA_VERSION.to_string(),
        sqlite_user_version,
        open_kioku_version: env!("CARGO_PKG_VERSION").to_string(),
        index_mode: manifest.index_mode.to_string(),
        repo_commit: manifest
            .repository
            .commit
            .clone()
            .or_else(|| open_kioku_git::commit(&repo))
            .unwrap_or_else(|| "unknown".to_string()),
        indexed_at: manifest.indexed_at.to_rfc3339(),
        file_count: manifest.file_count,
        symbol_count: manifest.symbol_count,
        chunk_count: manifest.chunk_count,
        graph_node_count: graph_counts.0,
        graph_edge_count: graph_counts.1,
        original_size_bytes,
        compressed_size_bytes,
        compression_level: quality.compression_level(),
        source_root_hash: source_root_hash(&repo),
        artifact_kind: SNAPSHOT_ARTIFACT_KIND.to_string(),
    };
    atomic_write_json(&snapshot_metadata_path(&repo), &metadata)?;
    let _ = fs::remove_file(&temp_db);

    Ok(SnapshotExportReport {
        ok: true,
        quality,
        artifact_path,
        metadata_path: snapshot_metadata_path(&repo),
        metadata,
    })
}

fn snapshot_import(repo: &Path) -> anyhow::Result<SnapshotImportReport> {
    let repo = absolutize(repo)?;
    let artifact_path = snapshot_artifact_path(&repo);
    let metadata_path = snapshot_metadata_path(&repo);
    let metadata = read_snapshot_metadata(&metadata_path)?;
    let warnings = validate_snapshot_metadata(&metadata)?;
    if !artifact_path.exists() {
        anyhow::bail!("snapshot artifact is missing: {}", artifact_path.display());
    }
    validate_compressed_snapshot_size(&artifact_path, &metadata)?;

    let artifact_dir = snapshot_artifact_dir(&repo);
    fs::create_dir_all(&artifact_dir)?;
    let temp_db = unique_temp_path(&artifact_dir, "index.snapshot.import", "sqlite.tmp");
    decompress_file(&artifact_path, &temp_db)?;
    validate_decompressed_snapshot_size(&temp_db, &metadata)?;
    integrity_check_sqlite(&temp_db)?;
    ensure_required_snapshot_tables(&temp_db)?;
    let temp_user_version = sqlite_user_version(&temp_db)?;
    if temp_user_version != metadata.sqlite_user_version {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!(
            "snapshot metadata user_version {} does not match database user_version {}",
            metadata.sqlite_user_version,
            temp_user_version
        );
    }
    let temp_manifest = read_manifest_from_sqlite(&temp_db)?;
    if temp_manifest.is_none() {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!("snapshot database has no index manifest");
    }

    let index_path = index_sqlite_path(&repo);
    promote_snapshot_db(&repo, &temp_db)?;
    let store = open_store(&repo)?;
    rebuild_search_from_store(&repo, &store)?;

    Ok(SnapshotImportReport {
        ok: true,
        imported: true,
        rebuilt_search: true,
        artifact_path,
        metadata_path,
        index_path,
        metadata,
        warnings,
    })
}

fn snapshot_doctor(repo: &Path) -> SnapshotDoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let artifact_path = snapshot_artifact_path(&repo);
    let metadata_path = snapshot_metadata_path(&repo);
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let metadata = match read_snapshot_metadata(&metadata_path) {
        Ok(metadata) => {
            match validate_snapshot_metadata(&metadata) {
                Ok(metadata_warnings) => warnings.extend(metadata_warnings),
                Err(err) => errors.push(err.to_string()),
            }
            Some(metadata)
        }
        Err(err) => {
            errors.push(err.to_string());
            None
        }
    };

    if !artifact_path.exists() {
        errors.push(format!(
            "snapshot artifact is missing: {}",
            artifact_path.display()
        ));
    } else {
        if let Some(metadata) = &metadata {
            if let Err(err) = validate_compressed_snapshot_size(&artifact_path, metadata) {
                errors.push(err.to_string());
            }
        }
        let artifact_dir = snapshot_artifact_dir(&repo);
        if let Err(err) = fs::create_dir_all(&artifact_dir) {
            errors.push(format!(
                "cannot create artifact temp directory {}: {err}",
                artifact_dir.display()
            ));
        } else {
            let temp_db = unique_temp_path(&artifact_dir, "index.snapshot.doctor", "sqlite.tmp");
            match decompress_file(&artifact_path, &temp_db)
                .and_then(|_| {
                    if let Some(metadata) = &metadata {
                        validate_decompressed_snapshot_size(&temp_db, metadata)?;
                    }
                    Ok(())
                })
                .and_then(|_| integrity_check_sqlite(&temp_db))
                .and_then(|_| ensure_required_snapshot_tables(&temp_db))
            {
                Ok(()) => {
                    if let Some(metadata) = &metadata {
                        match sqlite_user_version(&temp_db) {
                            Ok(user_version) if user_version != metadata.sqlite_user_version => {
                                errors.push(format!(
                                    "metadata user_version {} does not match artifact user_version {}",
                                    metadata.sqlite_user_version, user_version
                                ));
                            }
                            Ok(_) => {}
                            Err(err) => errors.push(err.to_string()),
                        }
                    }
                }
                Err(err) => errors.push(err.to_string()),
            }
            let _ = fs::remove_file(&temp_db);
        }
    }

    SnapshotDoctorReport {
        ok: errors.is_empty(),
        artifact_path,
        metadata_path,
        metadata,
        warnings,
        errors,
    }
}

fn print_snapshot_doctor_report(report: &SnapshotDoctorReport) {
    println!("Open Kioku snapshot doctor");
    println!(
        "{} metadata: {}",
        if report.metadata.is_some() {
            "[ok]  "
        } else {
            "[fail]"
        },
        report.metadata_path.display()
    );
    println!(
        "{} artifact: {}",
        if report.artifact_path.exists() {
            "[ok]  "
        } else {
            "[fail]"
        },
        report.artifact_path.display()
    );
    for warning in &report.warnings {
        println!("[warn] {warning}");
    }
    for error in &report.errors {
        println!("[fail] {error}");
    }
    if report.ok {
        println!("[ok]   snapshot is importable");
    }
}

fn snapshot_artifact_dir(repo: &Path) -> PathBuf {
    repo.join(".ok/artifacts")
}

fn snapshot_artifact_path(repo: &Path) -> PathBuf {
    snapshot_artifact_dir(repo).join("index.snapshot.zst")
}

fn snapshot_metadata_path(repo: &Path) -> PathBuf {
    snapshot_artifact_dir(repo).join("index.snapshot.json")
}

fn index_sqlite_path(repo: &Path) -> PathBuf {
    repo.join(".ok/index.sqlite")
}

fn workspace_graph_path(workspace: &Path) -> PathBuf {
    workspace.join(".ok/workspace.sqlite")
}

fn build_cross_project_workspace(workspace: &Path) -> anyhow::Result<WorkspaceLinkReport> {
    let (workspace_root, config_path, config) = load_workspace_config(workspace)?;
    let mut warnings = Vec::new();
    let mut projects = Vec::new();
    for project in &config.projects {
        projects.push(load_workspace_project(&workspace_root, project)?);
    }

    let mut workspace_nodes = HashMap::<String, GraphNode>::new();
    let mut workspace_edges = Vec::<GraphEdge>::new();
    let mut links = Vec::<WorkspaceLinkSummary>::new();
    let mut cap_hit = false;

    for source in &projects {
        for target in &projects {
            if source.name == target.name {
                continue;
            }
            link_boundary_edges(
                source,
                target,
                &source.calls,
                &target.exposes,
                GraphEdgeType::CallsEndpoint,
                "endpoint_path_protocol",
                &mut workspace_nodes,
                &mut workspace_edges,
                &mut links,
                &mut warnings,
                &mut cap_hit,
            );
            link_boundary_edges(
                source,
                target,
                &source.publishes,
                &target.consumes,
                GraphEdgeType::PublishesEvent,
                "topic_name",
                &mut workspace_nodes,
                &mut workspace_edges,
                &mut links,
                &mut warnings,
                &mut cap_hit,
            );
        }
    }

    let graph_path = workspace_graph_path(&workspace_root);
    let store = SqliteStore::open(&graph_path)?;
    let mut nodes = workspace_nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    workspace_edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    store.replace_graph(&nodes, &workspace_edges)?;

    let project_reports = projects
        .into_iter()
        .map(|project| WorkspaceProjectReport {
            name: project.name,
            repo: project.repo,
            index_path: project.index_path,
            graph_nodes: project.graph_node_count,
            graph_edges: project.graph_edge_count,
        })
        .collect::<Vec<_>>();

    Ok(WorkspaceLinkReport {
        ok: !cap_hit,
        workspace: workspace_root,
        config_path,
        graph_path,
        project_count: project_reports.len(),
        projects: project_reports,
        link_count: links.len(),
        links,
        cap: WORKSPACE_LINK_CAP,
        cap_hit,
        warnings,
    })
}

fn load_fleet_architecture_report(workspace: &Path) -> anyhow::Result<FleetArchitectureReport> {
    let (workspace_root, _config_path, config) = load_workspace_config(workspace)?;
    let graph_path = workspace_graph_path(&workspace_root);
    if !graph_path.exists() {
        anyhow::bail!(
            "workspace graph is missing at {}; run `ok index --mode cross-project --workspace {}`",
            graph_path.display(),
            workspace_root.display()
        );
    }
    let conn = Connection::open_with_flags(
        &graph_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening workspace graph {}", graph_path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    let links = load_workspace_link_summaries(&conn)?;
    Ok(FleetArchitectureReport {
        ok: true,
        workspace: workspace_root,
        graph_path,
        project_count: config.projects.len(),
        link_count: links.len(),
        links,
        warnings: Vec::new(),
    })
}

fn load_workspace_config(workspace: &Path) -> anyhow::Result<(PathBuf, PathBuf, WorkspaceConfig)> {
    let workspace = absolutize(workspace)?;
    let (workspace_root, config_path) = if workspace.is_file() {
        let root = workspace
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (root, workspace)
    } else {
        let candidates = [
            workspace.join("ok-workspace.toml"),
            workspace.join("workspace.toml"),
            workspace.join("ok.toml"),
        ];
        let config_path = candidates
            .into_iter()
            .find(|path| path.exists())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "workspace config not found in {}; expected ok-workspace.toml, workspace.toml, or ok.toml",
                    workspace.display()
                )
            })?;
        (workspace, config_path)
    };
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading workspace config {}", config_path.display()))?;
    let parsed: WorkspaceToml = toml::from_str(&raw)
        .with_context(|| format!("parsing workspace config {}", config_path.display()))?;
    if parsed.workspace.projects.is_empty() {
        anyhow::bail!("workspace config {} has no projects", config_path.display());
    }
    let mut names = std::collections::HashSet::new();
    for project in &parsed.workspace.projects {
        if project.name.trim().is_empty() {
            anyhow::bail!("workspace project names must not be empty");
        }
        if !names.insert(project.name.clone()) {
            anyhow::bail!("duplicate workspace project name `{}`", project.name);
        }
    }
    Ok((workspace_root, config_path, parsed.workspace))
}

fn load_workspace_project(
    workspace_root: &Path,
    project: &WorkspaceProjectConfig,
) -> anyhow::Result<WorkspaceProjectGraph> {
    let repo = if project.repo.is_absolute() {
        project.repo.clone()
    } else {
        workspace_root.join(&project.repo)
    };
    let repo = absolutize(&repo)?;
    let index_path = index_sqlite_path(&repo);
    if !index_path.exists() {
        anyhow::bail!(
            "missing project index for `{}` at {}; run `ok index` in that project first",
            project.name,
            index_path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &index_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening project index {}", index_path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;

    let nodes = load_graph_nodes_by_id(&conn)?;
    let graph_node_count = nodes.len();
    let graph_edge_count = graph_row_count(&conn, "graph_edges")?;
    let exposes = load_boundary_edges(&conn, &nodes, GraphEdgeType::ExposesEndpoint)?;
    let calls = load_boundary_edges(&conn, &nodes, GraphEdgeType::CallsEndpoint)?;
    let publishes = load_boundary_edges(&conn, &nodes, GraphEdgeType::PublishesEvent)?;
    let consumes = load_boundary_edges(&conn, &nodes, GraphEdgeType::ConsumesEvent)?;

    Ok(WorkspaceProjectGraph {
        name: project.name.clone(),
        repo,
        index_path,
        graph_node_count,
        graph_edge_count,
        exposes,
        calls,
        publishes,
        consumes,
    })
}

fn load_graph_nodes_by_id(conn: &Connection) -> anyhow::Result<HashMap<String, GraphNode>> {
    let mut stmt = conn.prepare("SELECT json FROM graph_nodes ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut nodes = HashMap::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let node: GraphNode = serde_json::from_str(&raw)?;
        nodes.insert(node.id.0.clone(), node);
    }
    Ok(nodes)
}

fn load_boundary_edges(
    conn: &Connection,
    nodes: &HashMap<String, GraphNode>,
    edge_type: GraphEdgeType,
) -> anyhow::Result<Vec<ProjectBoundaryEdge>> {
    let mut stmt = conn.prepare("SELECT json FROM graph_edges WHERE edge_type = ?1 ORDER BY id")?;
    let mut rows = stmt.query(params![format!("{:?}", edge_type)])?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let edge: GraphEdge = serde_json::from_str(&raw)?;
        let Some(source) = nodes.get(&edge.from.0).cloned() else {
            continue;
        };
        let Some(target) = nodes.get(&edge.to.0).cloned() else {
            continue;
        };
        edges.push(ProjectBoundaryEdge {
            edge,
            source,
            target,
        });
    }
    Ok(edges)
}

fn graph_row_count(conn: &Connection, table: &str) -> anyhow::Result<usize> {
    let sql = match table {
        "graph_edges" => "SELECT COUNT(*) FROM graph_edges",
        "graph_nodes" => "SELECT COUNT(*) FROM graph_nodes",
        _ => anyhow::bail!("unsupported graph count table {table}"),
    };
    let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(count as usize)
}

#[allow(clippy::too_many_arguments)]
fn link_boundary_edges(
    source_project: &WorkspaceProjectGraph,
    target_project: &WorkspaceProjectGraph,
    sources: &[ProjectBoundaryEdge],
    targets: &[ProjectBoundaryEdge],
    edge_type: GraphEdgeType,
    strategy: &str,
    workspace_nodes: &mut HashMap<String, GraphNode>,
    workspace_edges: &mut Vec<GraphEdge>,
    links: &mut Vec<WorkspaceLinkSummary>,
    warnings: &mut Vec<String>,
    cap_hit: &mut bool,
) {
    for source in sources {
        if workspace_edges.len() >= WORKSPACE_LINK_CAP {
            *cap_hit = true;
            warnings.push(format!(
                "workspace link cap {} reached; additional cross-project edges were skipped",
                WORKSPACE_LINK_CAP
            ));
            return;
        }
        let matches = targets
            .iter()
            .filter(|candidate| boundary_targets_match(source, candidate, &edge_type))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }
        for candidate in matches
            .iter()
            .take(WORKSPACE_LINK_CAP - workspace_edges.len())
        {
            let ambiguity = workspace_ambiguity(source, matches.len());
            let confidence = if ambiguity.is_empty() {
                Confidence::High
            } else {
                Confidence::Medium
            };
            let source_node = upsert_workspace_node(
                workspace_nodes,
                &source_project.name,
                &source.source,
                &source_project.repo,
            );
            let target_node = upsert_workspace_node(
                workspace_nodes,
                &target_project.name,
                &candidate.source,
                &target_project.repo,
            );
            let target_label = boundary_target_label(source);
            let edge_id = workspace_edge_id(
                &edge_type,
                &source_project.name,
                &target_project.name,
                &source.edge.id,
                &candidate.edge.id,
                &source_node,
                &target_node,
                &target_label,
                strategy,
            );
            let mut properties = BTreeMap::new();
            properties.insert(
                "source_project".into(),
                serde_json::json!(source_project.name),
            );
            properties.insert(
                "target_project".into(),
                serde_json::json!(target_project.name),
            );
            properties.insert("target".into(), serde_json::json!(target_label.clone()));
            properties.insert("source_node".into(), serde_json::json!(source.edge.from.0));
            properties.insert(
                "target_node".into(),
                serde_json::json!(candidate.edge.from.0),
            );
            properties.insert(
                "source_endpoint_node".into(),
                serde_json::json!(source.edge.to.0),
            );
            properties.insert(
                "target_endpoint_node".into(),
                serde_json::json!(candidate.edge.to.0),
            );
            properties.insert("matching_strategy".into(), serde_json::json!(strategy));
            properties.insert(
                "confidence".into(),
                serde_json::json!(format!("{:?}", confidence)),
            );
            if let Some(file_id) = &candidate.source.file_id {
                properties.insert("target_file".into(), serde_json::json!(file_id.0));
            }
            if let Some(symbol_id) = &candidate.source.symbol_id {
                properties.insert("target_symbol".into(), serde_json::json!(symbol_id.0));
            }
            workspace_edges.push(GraphEdge {
                id: edge_id.clone(),
                from: source_node.clone(),
                to: target_node.clone(),
                edge_type: edge_type.clone(),
                properties,
                source_pass: Some("workspace_linker".into()),
                index_mode: Some(IndexMode::CrossProject.to_string()),
                ambiguity: ambiguity.clone(),
                evidence: Evidence {
                    id: EvidenceId::new(open_kioku_core::identity::stable_hash(&format!(
                        "workspace-link-evidence:{}",
                        edge_id.0
                    ))),
                    source: "open-kioku-workspace".into(),
                    source_type: EvidenceSourceType::StaticAnalysis,
                    file_range: source.edge.evidence.file_range.clone(),
                    symbol_id: source.edge.evidence.symbol_id.clone(),
                    confidence,
                    message: format!(
                        "{} links {} to {} via {}",
                        source_project.name, target_label, target_project.name, strategy
                    ),
                    indexed_at: chrono::Utc::now(),
                    confidence_score: None,
                    confidence_reason: Some(if ambiguity.is_empty() {
                        "single cross-project boundary match".into()
                    } else {
                        "ambiguous cross-project boundary match".into()
                    }),
                    freshness: None,
                },
                ..Default::default()
            });
            links.push(WorkspaceLinkSummary {
                source_project: source_project.name.clone(),
                target_project: target_project.name.clone(),
                source_node: source_node.0,
                target_node: target_node.0,
                target: target_label,
                edge_type: edge_type.clone(),
                matching_strategy: strategy.into(),
                confidence,
                ambiguity,
            });
        }
    }
}

fn boundary_targets_match(
    source: &ProjectBoundaryEdge,
    target: &ProjectBoundaryEdge,
    edge_type: &GraphEdgeType,
) -> bool {
    match edge_type {
        GraphEdgeType::CallsEndpoint => {
            let source_path = normalized_boundary_value(&source.target);
            let target_path = normalized_boundary_value(&target.target);
            if source_path.is_empty() || source_path != target_path {
                return false;
            }
            let source_protocol = string_property(&source.target, "protocol").unwrap_or("http");
            let target_protocol = string_property(&target.target, "protocol").unwrap_or("http");
            if source_protocol != target_protocol {
                return false;
            }
            let source_method = string_property(&source.target, "method");
            let target_method = string_property(&target.target, "method");
            source_method.is_none() || target_method.is_none() || source_method == target_method
        }
        GraphEdgeType::PublishesEvent => {
            let source_topic = normalized_boundary_value(&source.target);
            !source_topic.is_empty() && source_topic == normalized_boundary_value(&target.target)
        }
        _ => false,
    }
}

fn workspace_ambiguity(source: &ProjectBoundaryEdge, match_count: usize) -> Vec<String> {
    let mut ambiguity = source.edge.ambiguity.clone();
    if match_count > 1 {
        ambiguity.push(format!(
            "{match_count} candidate cross-project targets matched this boundary"
        ));
    }
    ambiguity.sort();
    ambiguity.dedup();
    ambiguity
}

fn upsert_workspace_node(
    nodes: &mut HashMap<String, GraphNode>,
    project: &str,
    node: &GraphNode,
    repo: &Path,
) -> NodeId {
    let workspace_id = workspace_node_id(project, &node.id);
    nodes.entry(workspace_id.0.clone()).or_insert_with(|| {
        let mut cloned = node.clone();
        cloned.id = workspace_id.clone();
        cloned
            .properties
            .insert("project".into(), serde_json::json!(project));
        cloned
            .properties
            .insert("repo".into(), serde_json::json!(repo.display().to_string()));
        cloned.index_mode = Some(IndexMode::CrossProject.to_string());
        cloned
    });
    workspace_id
}

fn workspace_node_id(project: &str, node_id: &NodeId) -> NodeId {
    NodeId::new(format!(
        "workspace:{}:{}",
        open_kioku_core::identity::stable_hash(project),
        node_id.0
    ))
}

#[allow(clippy::too_many_arguments)]
fn workspace_edge_id(
    edge_type: &GraphEdgeType,
    source_project: &str,
    target_project: &str,
    source_edge: &EdgeId,
    target_edge: &EdgeId,
    source_node: &NodeId,
    target_node: &NodeId,
    target: &str,
    strategy: &str,
) -> EdgeId {
    EdgeId::new(format!(
        "workspace-link:{}",
        open_kioku_core::identity::stable_hash(&format!(
            "{edge_type:?}:{source_project}:{target_project}:{}:{}:{}:{}:{target}:{strategy}",
            source_edge.0, target_edge.0, source_node.0, target_node.0
        ))
    ))
}

fn boundary_target_label(edge: &ProjectBoundaryEdge) -> String {
    let normalized = normalized_boundary_value(&edge.target);
    if normalized.is_empty() {
        edge.target.label.clone()
    } else {
        normalized
    }
}

fn normalized_boundary_value(node: &GraphNode) -> String {
    string_property(node, "normalized_path")
        .unwrap_or(node.label.as_str())
        .trim()
        .to_string()
}

fn string_property<'a>(node: &'a GraphNode, key: &str) -> Option<&'a str> {
    node.properties.get(key).and_then(|value| value.as_str())
}

fn load_workspace_link_summaries(conn: &Connection) -> anyhow::Result<Vec<WorkspaceLinkSummary>> {
    let mut stmt = conn
        .prepare("SELECT json FROM graph_edges WHERE source_type = 'StaticAnalysis' ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut links = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let edge: GraphEdge = serde_json::from_str(&raw)?;
        if edge.source_pass.as_deref() != Some("workspace_linker") {
            continue;
        }
        links.push(WorkspaceLinkSummary {
            source_project: edge
                .properties
                .get("source_project")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            target_project: edge
                .properties
                .get("target_project")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            source_node: edge.from.0,
            target_node: edge.to.0,
            target: edge
                .properties
                .get("target")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            edge_type: edge.edge_type,
            matching_strategy: edge
                .properties
                .get("matching_strategy")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            confidence: edge.evidence.confidence,
            ambiguity: edge.ambiguity,
        });
    }
    Ok(links)
}

fn print_workspace_link_report(report: &WorkspaceLinkReport) {
    println!(
        "Linked {} cross-project boundary edge(s) across {} project(s)",
        report.link_count, report.project_count
    );
    println!("Workspace graph: {}", report.graph_path.display());
    for project in &report.projects {
        println!(
            "- {}: {} nodes, {} edges ({})",
            project.name,
            project.graph_nodes,
            project.graph_edges,
            project.repo.display()
        );
    }
    for link in &report.links {
        println!(
            "- {} -> {} {} via {} ({:?})",
            link.source_project,
            link.target_project,
            link.target,
            link.matching_strategy,
            link.confidence
        );
        for ambiguity in &link.ambiguity {
            println!("  caveat: {ambiguity}");
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn print_fleet_architecture_report(report: &FleetArchitectureReport) {
    println!(
        "Fleet graph: {} cross-project link(s) across {} project(s)",
        report.link_count, report.project_count
    );
    println!("Workspace graph: {}", report.graph_path.display());
    for link in &report.links {
        println!(
            "- {} -> {} {} via {} ({:?})",
            link.source_project,
            link.target_project,
            link.target,
            link.matching_strategy,
            link.confidence
        );
        for ambiguity in &link.ambiguity {
            println!("  caveat: {ambiguity}");
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn unique_temp_path(dir: &Path, stem: &str, suffix: &str) -> PathBuf {
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    dir.join(format!(
        ".{stem}.{}.{}.{}",
        std::process::id(),
        nanos,
        suffix
    ))
}

fn ensure_snapshot_gitattributes(artifact_dir: &Path) -> anyhow::Result<()> {
    let path = artifact_dir.join(".gitattributes");
    if path.exists() {
        return Ok(());
    }
    fs::write(
        &path,
        "*.snapshot.zst binary -merge\n*.snapshot.json text\n",
    )
    .with_context(|| format!("writing {}", path.display()))
}

fn checkpoint_sqlite(path: &Path) {
    if !path.exists() {
        return;
    }
    if let Ok(conn) = Connection::open(path) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

fn read_snapshot_metadata(path: &Path) -> anyhow::Result<SnapshotMetadata> {
    let file = File::open(path)
        .with_context(|| format!("snapshot metadata is missing: {}", path.display()))?;
    serde_json::from_reader(file)
        .with_context(|| format!("reading snapshot metadata {}", path.display()))
}

fn validate_snapshot_metadata(metadata: &SnapshotMetadata) -> anyhow::Result<Vec<String>> {
    if metadata.artifact_kind != SNAPSHOT_ARTIFACT_KIND {
        anyhow::bail!(
            "unsupported snapshot artifact kind {}; expected {}",
            metadata.artifact_kind,
            SNAPSHOT_ARTIFACT_KIND
        );
    }
    if metadata.schema_version != SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported snapshot schema version {}; expected {}",
            metadata.schema_version,
            SNAPSHOT_SCHEMA_VERSION
        );
    }
    if metadata.sqlite_user_version > SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "snapshot sqlite user_version {} is newer than supported version {}",
            metadata.sqlite_user_version,
            SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION
        );
    }
    let mut warnings = Vec::new();
    if metadata.open_kioku_version != env!("CARGO_PKG_VERSION") {
        warnings.push(format!(
            "snapshot was exported by Open Kioku {}; current binary is {}",
            metadata.open_kioku_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(warnings)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = unique_temp_path(parent, "index.snapshot.metadata", "json.tmp");
    {
        let mut file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("promoting metadata {} to {}", tmp.display(), path.display()))
}

fn compress_file(input: &Path, output: &Path, level: i32) -> anyhow::Result<()> {
    let mut reader = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let writer = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    zstd::stream::copy_encode(&mut reader, writer, level)
        .with_context(|| format!("compressing {}", input.display()))?;
    Ok(())
}

fn decompress_file(input: &Path, output: &Path) -> anyhow::Result<()> {
    let mut reader = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let writer = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    zstd::stream::copy_decode(&mut reader, writer)
        .with_context(|| format!("decompressing {}", input.display()))?;
    Ok(())
}

fn validate_compressed_snapshot_size(
    artifact_path: &Path,
    metadata: &SnapshotMetadata,
) -> anyhow::Result<()> {
    let actual = fs::metadata(artifact_path)
        .with_context(|| format!("reading metadata for {}", artifact_path.display()))?
        .len();
    if actual != metadata.compressed_size_bytes {
        anyhow::bail!(
            "snapshot compressed size mismatch: metadata says {} bytes, artifact is {} bytes",
            metadata.compressed_size_bytes,
            actual
        );
    }
    Ok(())
}

fn validate_decompressed_snapshot_size(
    db_path: &Path,
    metadata: &SnapshotMetadata,
) -> anyhow::Result<()> {
    let actual = fs::metadata(db_path)
        .with_context(|| format!("reading metadata for {}", db_path.display()))?
        .len();
    if actual != metadata.original_size_bytes {
        anyhow::bail!(
            "snapshot original size mismatch: metadata says {} bytes, artifact expands to {} bytes",
            metadata.original_size_bytes,
            actual
        );
    }
    Ok(())
}

fn integrity_check_sqlite(path: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for integrity check", path.display()))?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .with_context(|| format!("running integrity_check on {}", path.display()))?;
    if result != "ok" {
        anyhow::bail!(
            "sqlite integrity_check failed for {}: {result}",
            path.display()
        );
    }
    Ok(())
}

fn sqlite_user_version(path: &Path) -> anyhow::Result<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for user_version", path.display()))?;
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .with_context(|| format!("reading sqlite user_version from {}", path.display()))
}

fn read_manifest_from_sqlite(path: &Path) -> anyhow::Result<Option<IndexManifest>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for manifest read", path.display()))?;
    let raw: Option<String> = conn
        .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .with_context(|| format!("reading index manifest from {}", path.display()))?;
    raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

fn read_graph_counts_from_sqlite(path: &Path) -> anyhow::Result<(usize, usize)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for graph counts", path.display()))?;
    let nodes: usize = conn
        .query_row("SELECT COUNT(*) FROM graph_nodes", [], |row| row.get(0))
        .with_context(|| format!("counting graph nodes in {}", path.display()))?;
    let edges: usize = conn
        .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
        .with_context(|| format!("counting graph edges in {}", path.display()))?;
    Ok((nodes, edges))
}

fn ensure_required_snapshot_tables(path: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for schema check", path.display()))?;
    let required = [
        "manifests",
        "files",
        "symbols",
        "chunks",
        "tests",
        "imports",
        "occurrences",
        "analysis_facts",
        "graph_nodes",
        "graph_edges",
    ];
    for table in required {
        let exists: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("checking for required table {table}"))?;
        if exists.is_none() {
            anyhow::bail!("snapshot database is missing required table `{table}`");
        }
    }
    Ok(())
}

fn promote_snapshot_db(repo: &Path, temp_db: &Path) -> anyhow::Result<()> {
    let ok_dir = repo.join(".ok");
    fs::create_dir_all(&ok_dir)?;
    let index_path = index_sqlite_path(repo);
    checkpoint_sqlite(&index_path);
    let backup_path = unique_temp_path(&ok_dir, "index.sqlite", "backup");
    let had_existing = index_path.exists();
    if had_existing {
        fs::rename(&index_path, &backup_path).with_context(|| {
            format!(
                "moving existing index {} to rollback backup {}",
                index_path.display(),
                backup_path.display()
            )
        })?;
    }

    if let Err(err) = fs::rename(temp_db, &index_path) {
        if had_existing {
            let _ = fs::rename(&backup_path, &index_path);
        }
        return Err(err).with_context(|| {
            format!(
                "promoting imported snapshot {} to {}",
                temp_db.display(),
                index_path.display()
            )
        });
    }
    remove_sqlite_sidecars(&index_path);

    match SqliteStore::open(&index_path) {
        Ok(_) => {
            if had_existing {
                let _ = fs::remove_file(&backup_path);
            }
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(&index_path);
            if had_existing {
                let _ = fs::rename(&backup_path, &index_path);
            }
            remove_sqlite_sidecars(&index_path);
            Err(anyhow::Error::from(err)).context("imported snapshot failed SQLite open")
        }
    }
}

fn remove_sqlite_sidecars(index_path: &Path) {
    let wal = index_path.with_extension("sqlite-wal");
    let shm = index_path.with_extension("sqlite-shm");
    let _ = fs::remove_file(wal);
    let _ = fs::remove_file(shm);
}

fn rebuild_search_from_store(repo: &Path, store: &SqliteStore) -> anyhow::Result<()> {
    let chunks = store.all_chunks()?;
    let files = store.list_files(usize::MAX, 0)?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let graph_nodes = store.all_graph_nodes()?;
    rebuild_disk_index_with_graph(
        default_index_dir(repo),
        &chunks,
        &files,
        &symbols,
        &graph_nodes,
    )?;
    Ok(())
}
