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
    // The probe every read surface makes: an index a live writer has not published yet is
    // refused with `indexing in progress`, and a database without a manifest is not an index.
    let Some(store) = SqliteStore::open_repo_index(&repo)? else {
        anyhow::bail!(
            "{}",
            open_kioku_storage::generations::not_indexed_message(&repo)
        );
    };
    let index_path = index_sqlite_path(&repo);

    // A store whose edges were discarded still has an edge table, so the export would count
    // it, write `graph_edge_count: 0` into the metadata, and pass the required-table check —
    // a clean-looking measurement of a repository that has not been measured. The artifact is
    // effect-safe (it carries the marker, and the importer's first open raises the rebuild
    // error), but the metadata file is read by people, so refuse rather than publish the zero.
    if store.graph_rebuild_required()? {
        anyhow::bail!(
            "index at {} has no graph edges: they were built by an older index format and \
             were discarded on open; run `ok index` before exporting a snapshot",
            index_path.display()
        );
    }
    // Two different states, and only one of them `--quality best` can fix. An index written
    // before secret-value redaction holds the values in live rows, and `VACUUM INTO` copies
    // rows faithfully, so no export mode is safe until `ok index` rewrites them. An index that
    // has been re-indexed owes only the clearing of free pages, which `best` drops on the way
    // out and `fast` carries along with the file.
    match open_kioku_storage::MetadataStore::manifest(&store) {
        // Withdrawn between the probe above and this read: the copy below refuses it with the
        // unpublished-index message, so there is nothing for these checks to decide.
        Ok(None) => {}
        // Unreadable: whether this index can be exported safely is unknown, and `ok index`
        // treats the same unknown as "assume the clearing is owed" rather than as "nothing
        // owed". An export is a distribution point, so it refuses too.
        Err(err) => anyhow::bail!(
            "index at {} cannot be read to check whether it still holds values stored before \
             secret-value redaction ({err}); run `ok index`, then export",
            index_path.display()
        ),
        Ok(Some(manifest)) => {
            if manifest.predates_secret_redaction() {
                anyhow::bail!(
                    "index at {} was written before secret-value redaction: its rows still \
                     hold those values as read, and every export mode copies rows. Run `ok \
                     index` to rebuild it with redaction, then export.",
                    index_path.display()
                );
            }
            if manifest.quality.pending_pre_redaction_compaction
                && matches!(quality, SnapshotQuality::Fast)
            {
                anyhow::bail!(
                    "index at {} still owes the clearing of bytes an earlier index stored as \
                     read; `--quality fast` copies the database file as it is, free pages \
                     included. Run `ok index` to clear them, or export with `--quality best`, \
                     which rewrites the database and leaves those pages behind.",
                    index_path.display()
                );
            }
        }
    }
    drop(store);

    let artifact_dir = snapshot_artifact_dir(&repo);
    fs::create_dir_all(&artifact_dir)?;
    ensure_snapshot_gitattributes(&artifact_dir)?;

    let temp_db = unique_temp_path(&artifact_dir, "index.snapshot", "sqlite.tmp");
    let manifest = match copy_published_index(&repo, &index_path, &temp_db, quality) {
        Ok(manifest) => manifest,
        Err(err) => {
            let _ = fs::remove_file(&temp_db);
            return Err(err);
        }
    };
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
        analysis_semantics: manifest.analysis_semantics.clone(),
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

fn snapshot_import(repo: &Path, allow_foreign: bool) -> anyhow::Result<SnapshotImportReport> {
    let repo = absolutize(repo)?;
    // An import replaces every index component, so it takes the writer lock `ok index` and
    // `ok watch` take, for the whole run: it never interleaves with either, and readers report
    // an index being built, not an unindexed repository, until the manifest is published.
    if open_kioku_storage::generations::index_write_in_progress(&repo) {
        eprintln!(
            "snapshot import: waiting for exclusive index writer lock {}",
            open_kioku_storage::generations::index_lock_path(&repo).display()
        );
    }
    let _lock = IndexWriteLock::acquire(&repo, IndexWriteLock::DEFAULT_WAIT)?;
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
    let temp_manifest = match read_manifest_from_sqlite(&temp_db)? {
        Some(manifest) => manifest,
        None => {
            let _ = fs::remove_file(&temp_db);
            anyhow::bail!("snapshot database has no index manifest");
        }
    };
    if metadata.analysis_semantics != temp_manifest.analysis_semantics {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!(
            "snapshot analysis-semantics metadata does not match the embedded index manifest"
        );
    }
    let current_semantics = open_kioku_core::AnalysisSemanticsState::current();
    let compatibility = open_kioku_core::classify_analysis_semantics(
        temp_manifest.analysis_semantics.as_ref(),
        &current_semantics,
    );
    if !compatibility.status.allows_authoritative_relationships() {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!(
            "snapshot analysis semantics are {:?}: {}; stored={}, current={}; {}",
            compatibility.status,
            compatibility.reasons.join("; "),
            compatibility.stored_fingerprint.as_deref().unwrap_or("missing"),
            compatibility.current_fingerprint,
            compatibility.recommended_action
        );
    }

    // Both checks run on the staged copy, before the current index is touched: a refusal
    // leaves it published, and the rows the local policy excludes are gone before any reader
    // can see the imported database.
    let artifact_commit = match snapshot_artifact_commit(&metadata, &temp_manifest) {
        Ok(commit) => commit,
        Err(err) => {
            let _ = fs::remove_file(&temp_db);
            return Err(err);
        }
    };
    let revision = match assess_snapshot_revision(&repo, &artifact_commit, allow_foreign) {
        Ok(revision) => revision,
        Err(err) => {
            let _ = fs::remove_file(&temp_db);
            return Err(err);
        }
    };
    let mut temp_manifest = temp_manifest;
    let filtered = match apply_local_policy_to_snapshot(&repo, &temp_db, &mut temp_manifest) {
        Ok(filtered) => filtered,
        Err(err) => {
            remove_staged_db(&temp_db);
            return Err(err);
        }
    };
    let provenance = revision.into_provenance(filtered.paths_removed);
    temp_manifest.snapshot = Some(provenance.clone());

    // The manifest is the publication marker and the artifact carries one. The database is
    // moved into place without it, and it is put back once the search index has been rebuilt
    // from the imported rows, as `ok index` publishes its own.
    if let Err(err) = withhold_snapshot_manifest(&temp_db) {
        let _ = fs::remove_file(&temp_db);
        return Err(err);
    }
    let index_path = index_sqlite_path(&repo);
    if let Err(err) = promote_snapshot_db(&repo, &temp_db) {
        let _ = fs::remove_file(&temp_db);
        return Err(err);
    }
    let publish = || -> anyhow::Result<()> {
        let store = open_store_for_write(&repo)?;
        rebuild_search_from_store(&repo, &store)?;
        store.put_manifest(&temp_manifest)?;
        Ok(())
    };
    // The previous database is gone from here on. Without a manifest the imported rows read
    // as unindexed, never as an index whose search side is missing.
    publish().with_context(|| {
        format!(
            "the imported index was not published, so reads report the repository as \
             unindexed; run `ok snapshot import` again or `ok index {}`",
            repo.display()
        )
    })?;

    let mut caveats = Vec::new();
    caveats.extend(provenance.caveat());
    if filtered.paths_removed > 0 {
        caveats.push(format!(
            "{} path(s) in the snapshot are excluded by this repository's index policy and \
             were removed from the imported index",
            filtered.paths_removed
        ));
    }
    Ok(SnapshotImportReport {
        ok: true,
        imported: true,
        rebuilt_search: true,
        artifact_path,
        metadata_path,
        index_path,
        metadata,
        snapshot: provenance,
        policy_filtered_by_source: filtered.by_source,
        caveats,
        warnings,
    })
}

fn print_snapshot_import_outcome(report: &SnapshotImportReport) {
    let snapshot = &report.snapshot;
    let revision = match snapshot.relation {
        open_kioku_core::SnapshotRevisionRelation::SameCommit => "the checked-out commit".into(),
        open_kioku_core::SnapshotRevisionRelation::Related => format!(
            "{} commit(s) behind and {} ahead of HEAD",
            snapshot.commits_behind.unwrap_or_default(),
            snapshot.commits_ahead.unwrap_or_default()
        ),
        open_kioku_core::SnapshotRevisionRelation::Foreign => {
            "an unverified revision (--allow-foreign)".into()
        }
    };
    println!(
        "Snapshot revision: {} ({revision})",
        snapshot.imported_from_commit
    );
    println!(
        "Removed by local index policy: {} path(s)",
        snapshot.policy_filtered
    );
    for (source, count) in &report.policy_filtered_by_source {
        println!("  {source}: {count}");
    }
    for caveat in &report.caveats {
        println!("caveat: {caveat}");
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

/// The commit the artifact's rows were built from. The embedded manifest records it at index
/// time; the metadata records it at export, falling back to `HEAD` then. Two different values
/// mean the metadata does not describe this database.
fn snapshot_artifact_commit(
    metadata: &SnapshotMetadata,
    manifest: &IndexManifest,
) -> anyhow::Result<Option<String>> {
    let recorded = (metadata.repo_commit != "unknown").then(|| metadata.repo_commit.clone());
    match (&manifest.repository.commit, recorded) {
        (Some(embedded), Some(recorded)) if *embedded != recorded => anyhow::bail!(
            "snapshot metadata names commit {recorded}, but the embedded index manifest was \
             built from {embedded}"
        ),
        (Some(embedded), _) => Ok(Some(embedded.clone())),
        (None, recorded) => Ok(recorded),
    }
}

/// The revision half of [`open_kioku_core::SnapshotProvenance`].
#[derive(Debug)]
struct SnapshotRevision {
    imported_from_commit: String,
    local_commit: Option<String>,
    relation: open_kioku_core::SnapshotRevisionRelation,
    commits_behind: Option<usize>,
    commits_ahead: Option<usize>,
    changed_files: Option<usize>,
}

impl SnapshotRevision {
    fn into_provenance(self, policy_filtered: usize) -> open_kioku_core::SnapshotProvenance {
        open_kioku_core::SnapshotProvenance {
            imported_from_commit: self.imported_from_commit,
            local_commit: self.local_commit,
            relation: self.relation,
            commits_behind: self.commits_behind,
            commits_ahead: self.commits_ahead,
            changed_files: self.changed_files,
            policy_filtered,
        }
    }
}

/// Relate the artifact's commit to this checkout. An artifact that shares history with
/// `HEAD` is imported, stale or not, because the provenance it gets says exactly how far it
/// is from the checkout. One whose relation cannot be established — its commit is absent
/// here, unrelated, or unrecorded, or there is no `HEAD` — is refused unless the caller
/// passes `--allow-foreign`: importing it silently would serve another tree's code as this
/// one's, and nothing downstream could tell.
fn assess_snapshot_revision(
    repo: &Path,
    artifact_commit: &Option<String>,
    allow_foreign: bool,
) -> anyhow::Result<SnapshotRevision> {
    use open_kioku_core::SnapshotRevisionRelation as Relation;
    use open_kioku_git::RevisionRelation;

    let comparison = match artifact_commit {
        Some(commit) => open_kioku_git::compare_with_head(repo, commit)?,
        None => None,
    };
    let local_commit = comparison
        .as_ref()
        .map(|comparison| comparison.head.clone())
        .or_else(|| open_kioku_git::commit(repo));
    let imported_from_commit = comparison
        .as_ref()
        .map(|comparison| comparison.commit.clone())
        .or_else(|| artifact_commit.clone())
        .unwrap_or_else(|| "unknown".into());
    // Paths discovery never reaches (`.ok` itself, `.git`, build and dependency directories)
    // do not make the index differ from the checkout.
    let changed_files = |commit: &str| {
        open_kioku_git::changed_paths_since_commit(repo, commit)
            .ok()
            .map(|paths| {
                paths
                    .iter()
                    .filter(|path| {
                        !open_kioku_ingest::path_policy::is_pruned_by_discovery(path)
                    })
                    .count()
            })
    };
    let refusal = match (artifact_commit, &comparison) {
        (None, _) => "the snapshot does not record the commit it was built from".to_string(),
        (Some(_), None) => format!(
            "{} has no Git HEAD, so the snapshot's commit {imported_from_commit} cannot be \
             related to this checkout",
            repo.display()
        ),
        (Some(_), Some(comparison)) => match comparison.relation {
            RevisionRelation::Same => {
                return Ok(SnapshotRevision {
                    changed_files: changed_files(&comparison.commit),
                    imported_from_commit,
                    local_commit,
                    relation: Relation::SameCommit,
                    commits_behind: Some(0),
                    commits_ahead: Some(0),
                })
            }
            RevisionRelation::Related { ahead, behind } => {
                return Ok(SnapshotRevision {
                    changed_files: changed_files(&comparison.commit),
                    imported_from_commit,
                    local_commit,
                    relation: Relation::Related,
                    commits_behind: Some(behind),
                    commits_ahead: Some(ahead),
                })
            }
            RevisionRelation::Unrelated => format!(
                "the snapshot's commit {imported_from_commit} shares no history with HEAD {}",
                comparison.head
            ),
            RevisionRelation::Unknown => format!(
                "the snapshot's commit {imported_from_commit} is not in this repository; \
                 fetch it if the snapshot comes from this repository's remote"
            ),
        },
    };
    if !allow_foreign {
        anyhow::bail!(
            "snapshot import refused: {refusal}. The imported index would describe a tree this \
             checkout cannot be compared with. Run `ok index` to build the index from source, \
             or pass `--allow-foreign` to import it marked as foreign."
        );
    }
    Ok(SnapshotRevision {
        imported_from_commit,
        local_commit,
        relation: Relation::Foreign,
        commits_behind: None,
        commits_ahead: None,
        changed_files: None,
    })
}

#[derive(Debug, Default)]
struct SnapshotPolicyFilter {
    paths_removed: usize,
    by_source: BTreeMap<String, usize>,
}

/// Check the staged database and apply this repository's index policy to it, with the
/// ingest crate's own rules, so an imported index holds what `ok index` here would admit.
///
/// First, the rows must agree with each other where readers rely on it: the path a row's
/// column holds is the one its JSON serves, every row belongs to an indexed file, and the
/// graph dictionary is keyed by its values. The policy below is decided on columns, so a
/// row that disagrees could carry a path past it; such an artifact is refused outright,
/// whatever `--allow-foreign` says, because no writer produces one.
///
/// Then:
/// - every indexed file and document the policy excludes (secret-like or denied, hidden,
///   `[index] exclude`, `.gitignore`, `.okignore`) is removed with every row derived from it,
///   and recorded in the manifest's coverage and skipped paths as discovery records a skip;
/// - every Git history row, and every graph node no file owns, naming a secret-like or
///   denied path is removed too. `ok index` records history for every touched path, so this
///   is stricter than a local index, and it is what keeps an artifact from serving a path
///   the security policy denies;
/// - secret-like paths the exporter recorded as skipped are withheld under this repository's
///   `redact_secrets`.
///
/// Rules that do not depend on local configuration — vendor detection, pruning of build and
/// dependency directories, the size limit, symlinks — are not applied again. The search index
/// is rebuilt from the database afterwards, so it never sees what was removed.
fn apply_local_policy_to_snapshot(
    repo: &Path,
    temp_db: &Path,
    manifest: &mut IndexManifest,
) -> anyhow::Result<SnapshotPolicyFilter> {
    let config = OkConfig::load_from_repo(repo)
        .with_context(|| format!("loading the index policy of {}", repo.display()))?;
    let store = SqliteStore::open(temp_db)
        .with_context(|| format!("opening staged snapshot {}", temp_db.display()))?;
    let violations = store.consistency_violations()?;
    if !violations.is_empty() {
        anyhow::bail!(
            "snapshot import refused: the artifact's rows are inconsistent in ways no Open \
             Kioku writer produces, so the paths it would serve cannot be checked against \
             this repository's index policy ({}). Re-export it with `ok snapshot export`, or \
             run `ok index`.",
            violations.join("; ")
        );
    }
    let stored = store.stored_paths()?;
    // Indexed and history paths are repository paths, and Git is asked about them. Graph node
    // labels are not all paths (import specifiers, routes), so they get the security rules
    // alone, below, and never reach Git.
    let candidates = stored
        .indexed
        .union(&stored.history)
        .cloned()
        .collect::<Vec<_>>();
    let policy = open_kioku_ingest::path_policy::IndexPathPolicy::for_paths(
        repo,
        &config,
        &candidates,
    )?;
    let mut filter = SnapshotPolicyFilter::default();
    let mut indexed = BTreeSet::new();
    let mut history = BTreeSet::new();
    let mut excluded_files = Vec::new();
    for path in &candidates {
        let Some(exclusion) = policy.exclusion(path) else {
            continue;
        };
        let security = exclusion.source == open_kioku_core::SkipSource::SecurityPolicy;
        let in_index = stored.indexed.contains(path);
        if !in_index && !security {
            continue;
        }
        *filter
            .by_source
            .entry(skip_source_key(exclusion.source))
            .or_default() += 1;
        if in_index {
            if let Some(file) = store.get_file_by_path(path)? {
                excluded_files.push((file, exclusion));
            }
            indexed.insert(path.clone());
        }
        if security {
            history.insert(path.clone());
        }
    }
    let mut nodes = BTreeSet::new();
    let mut node_labels = BTreeSet::new();
    for (id, label) in &stored.unanchored_nodes {
        let Some(exclusion) = policy.security_exclusion(Path::new(label)) else {
            continue;
        };
        nodes.insert(id.clone());
        // A label that is also an indexed or history path was counted above.
        if !history.contains(Path::new(label)) && node_labels.insert(label.clone()) {
            *filter
                .by_source
                .entry(skip_source_key(exclusion.source))
                .or_default() += 1;
        }
    }
    filter.paths_removed = filter.by_source.values().sum();
    let purge = store.purge_paths(&indexed, &history, &nodes, manifest)?;
    drop(store);

    manifest.file_count = manifest.file_count.saturating_sub(purge.files_removed);
    manifest.symbol_count = manifest.symbol_count.saturating_sub(purge.symbols_removed);
    manifest.chunk_count = manifest.chunk_count.saturating_sub(purge.chunks_removed);
    for (file, exclusion) in &excluded_files {
        open_kioku_ingest::path_policy::record_excluded_indexed_file(
            &mut manifest.quality,
            file,
            *exclusion,
        );
    }
    open_kioku_ingest::path_policy::redact_recorded_skips(&mut manifest.quality, &config);
    Ok(filter)
}

/// Remove a staged database and the sidecar files an open connection left beside it.
fn remove_staged_db(temp_db: &Path) {
    let _ = fs::remove_file(temp_db);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = temp_db.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = fs::remove_file(PathBuf::from(sidecar));
    }
}

fn skip_source_key(source: open_kioku_core::SkipSource) -> String {
    serde_json::to_value(source)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{source:?}"))
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
                    match read_manifest_from_sqlite(&temp_db) {
                        Ok(Some(manifest)) => {
                            if let Some(metadata) = &metadata {
                                if metadata.analysis_semantics != manifest.analysis_semantics {
                                    errors.push("snapshot analysis-semantics metadata does not match the embedded index manifest".into());
                                }
                            }
                            let compatibility = open_kioku_core::classify_analysis_semantics(
                                manifest.analysis_semantics.as_ref(),
                                &open_kioku_core::AnalysisSemanticsState::current(),
                            );
                            if !compatibility.status.allows_authoritative_relationships() {
                                errors.push(format!(
                                    "snapshot analysis semantics are {:?}: {}; {}",
                                    compatibility.status,
                                    compatibility.reasons.join("; "),
                                    compatibility.recommended_action
                                ));
                            }
                            // The import's own revision check, so `importable` means what
                            // a plain `ok snapshot import` would do with this artifact here.
                            if let Some(metadata) = &metadata {
                                match snapshot_artifact_commit(metadata, &manifest).and_then(
                                    |commit| assess_snapshot_revision(&repo, &commit, false),
                                ) {
                                    Ok(revision) => {
                                        warnings.extend(revision.into_provenance(0).caveat())
                                    }
                                    Err(err) => errors.push(err.to_string()),
                                }
                            }
                            // The import's consistency check, on this throwaway copy.
                            match SqliteStore::open(&temp_db)
                                .and_then(|store| store.consistency_violations())
                            {
                                Ok(violations) => errors.extend(violations.into_iter().map(
                                    |violation| {
                                        format!(
                                            "snapshot rows are inconsistent and would be \
                                             refused on import: {violation}"
                                        )
                                    },
                                )),
                                Err(err) => errors.push(err.to_string()),
                            }
                        }
                        Ok(None) => errors.push("snapshot database has no index manifest".into()),
                        Err(err) => errors.push(err.to_string()),
                    }
                }
                Err(err) => errors.push(err.to_string()),
            }
            remove_staged_db(&temp_db);
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
    // Resolves through the active index generation when one is published (RI3.6).
    open_kioku_storage::generations::resolve_index_location(repo).sqlite_path()
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
    // The linker reads each member index read-only, so nothing here runs the store's rebuild
    // gate. Without this check a project whose edges are awaiting `ok index` would contribute
    // zero boundary edges and the workspace report would state, cleanly and wrongly, that it
    // makes no cross-project calls — which `replace_graph` would then persist.
    if open_kioku_storage_sqlite::graph_rebuild_required(&conn)? {
        anyhow::bail!(
            "project `{}` at {} has no graph edges: they were built by an older index format \
             and were discarded on open; run `ok index` in that project first",
            project.name,
            index_path.display()
        );
    }

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
    let mut edges = Vec::new();
    for edge in open_kioku_storage_sqlite::read_graph_edges_by_type(conn, edge_type)? {
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
                    ).into(),
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
    let mut links = Vec::new();
    for edge in open_kioku_storage_sqlite::read_graph_edges_by_source_type(
        conn,
        open_kioku_core::EvidenceSourceType::StaticAnalysis,
    )? {
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

/// Copy one committed state of the index into `dest` and return the manifest that state
/// carries. Export is a reader: it takes no writer lock, and a writer may commit, withdraw the
/// manifest or checkpoint while the copy runs. The manifest is read from the copy rather than
/// the live file, so it and the rows beside it come from the same state; a state without one
/// is a full run between staging and publication, or no index, and is refused as such.
fn copy_published_index(
    repo: &Path,
    index_path: &Path,
    dest: &Path,
    quality: SnapshotQuality,
) -> anyhow::Result<IndexManifest> {
    copy_index_database(index_path, dest, quality)?;
    integrity_check_sqlite(dest)?;
    ensure_required_snapshot_tables(dest)?;
    read_manifest_from_sqlite(dest)?
        .ok_or_else(|| anyhow::anyhow!("{}", unpublished_index_message(repo)))
}

/// How long a copy keeps retrying a source that reports busy before the export fails.
const SNAPSHOT_COPY_BUSY_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Copy the database at `index_path` into `dest` from inside a single read transaction, so the
/// copy is one committed state however writers interleave. The store runs in WAL mode, where
/// a reader's snapshot includes committed WAL frames and is not disturbed by later commits or
/// checkpoints; copying the file bytes instead can miss committed frames still in the WAL and
/// capture a page a checkpoint is rewriting. Neither quality checkpoints the live index first:
/// neither needs it, and export does not write to the index it reads.
///
/// `Best` rebuilds the database with `VACUUM INTO`. `Fast` copies its pages with the online
/// backup API in one step, which holds one read transaction for the whole copy; a copy in
/// several steps would restart whenever another connection commits, and could never finish
/// against a writer that keeps committing.
fn copy_index_database(
    index_path: &Path,
    dest: &Path,
    quality: SnapshotQuality,
) -> anyhow::Result<()> {
    let source = Connection::open_with_flags(
        index_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {} read-only", index_path.display()))?;
    source.busy_timeout(SNAPSHOT_COPY_BUSY_WAIT)?;
    match quality {
        SnapshotQuality::Best => {
            let dest_string = dest.to_string_lossy().to_string();
            source
                .execute("VACUUM INTO ?1", params![dest_string])
                .with_context(|| format!("compacting snapshot database to {}", dest.display()))?;
        }
        SnapshotQuality::Fast => {
            let mut target = Connection::open(dest)
                .with_context(|| format!("creating snapshot database {}", dest.display()))?;
            {
                let backup = rusqlite::backup::Backup::new(&source, &mut target)
                    .with_context(|| format!("starting backup to {}", dest.display()))?;
                let started = std::time::Instant::now();
                loop {
                    // -1: every page in this step, under one read transaction.
                    match backup.step(-1).with_context(|| {
                        format!(
                            "copying index database from {} to {}",
                            index_path.display(),
                            dest.display()
                        )
                    })? {
                        rusqlite::backup::StepResult::Done => break,
                        // Nothing was copied; the next step starts over from a new snapshot.
                        _ if started.elapsed() < SNAPSHOT_COPY_BUSY_WAIT => {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        other => anyhow::bail!(
                            "copying index database from {}: source stayed busy for {}s \
                             ({other:?}); retry the export",
                            index_path.display(),
                            SNAPSHOT_COPY_BUSY_WAIT.as_secs()
                        ),
                    }
                }
            }
            // The copied header still marks the file as WAL. The artifact is this one file,
            // so switch it to a rollback journal: nothing it holds can live in a sidecar.
            let mode: String = target
                .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
                .with_context(|| format!("finalizing snapshot database {}", dest.display()))?;
            if !mode.eq_ignore_ascii_case("delete") {
                anyhow::bail!(
                    "snapshot database {} stayed in journal mode {mode}",
                    dest.display()
                );
            }
        }
    }
    Ok(())
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
    // Importing an older store would open cleanly and then answer relationship questions from
    // graph tables the compact reader had to discard. Refuse it here, where the message can
    // name the fix, rather than importing something that only looks complete.
    if metadata.sqlite_user_version < SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "snapshot sqlite user_version {} predates the compact graph storage introduced in \
             user_version {}; re-export the snapshot from a rebuilt index (`ok index`)",
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
    // The same gate the store applies: an artifact from a newer Open Kioku is refused with
    // the upgrade-or-reindex message rather than imported and then failing on every read.
    raw.as_deref()
        .map(open_kioku_storage_sqlite::decode_index_manifest)
        .transpose()
        .map_err(Into::into)
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
    let backup_path = unique_temp_path(&ok_dir, "index.sqlite", "backup");
    let had_existing = index_path.exists();
    // A session that already holds the previous database (an MCP server) keeps reading that
    // file after it is moved aside, for as long as it has a manifest. Withdrawn first, the
    // session probes the index path again: the import in progress, then the imported index.
    let previous_manifest = if had_existing {
        withdraw_replaced_manifest(&index_path)?
    } else {
        None
    };
    checkpoint_sqlite(&index_path);
    if had_existing {
        if let Err(err) = fs::rename(&index_path, &backup_path) {
            let err = anyhow::Error::new(err).context(format!(
                "moving existing index {} to rollback backup {}",
                index_path.display(),
                backup_path.display()
            ));
            return Err(restore_replaced_manifest(
                err,
                &index_path,
                previous_manifest.as_deref(),
            ));
        }
    }

    if let Err(err) = fs::rename(temp_db, &index_path) {
        let err = anyhow::Error::new(err).context(format!(
            "promoting imported snapshot {} to {}",
            temp_db.display(),
            index_path.display()
        ));
        return Err(if had_existing {
            roll_back_to_previous_index(
                err,
                &index_path,
                &backup_path,
                previous_manifest.as_deref(),
            )
        } else {
            err
        });
    }
    remove_sqlite_sidecars(&index_path);

    match SqliteStore::open(&index_path) {
        Ok(_) => {
            if had_existing {
                if let Err(err) = fs::remove_file(&backup_path) {
                    eprintln!(
                        "warning: could not remove the previous index database at {} after \
                         the import ({err}); it is not read and can be deleted",
                        backup_path.display()
                    );
                }
            }
            Ok(())
        }
        Err(err) => {
            let err = anyhow::Error::from(err).context("imported snapshot failed SQLite open");
            // The sidecars at the index path are the imported database's now.
            remove_sqlite_sidecars(&index_path);
            if had_existing {
                return Err(roll_back_to_previous_index(
                    err,
                    &index_path,
                    &backup_path,
                    previous_manifest.as_deref(),
                ));
            }
            // Nothing to move back. The imported database has no manifest, so the repository
            // reads as unindexed whether or not it can be removed.
            Err(match fs::remove_file(&index_path) {
                Ok(()) => err,
                Err(remove_err) => err.context(format!(
                    "removing the imported database {} also failed ({remove_err}); it has no \
                     manifest, so the repository reads as unindexed; run `ok index`",
                    index_path.display()
                )),
            })
        }
    }
}

/// Move the previous database back from `backup_path` after an import failed with it moved
/// aside, and put its manifest back. `rename` replaces whatever the import left at the index
/// path (on unix, and on Windows through `MOVEFILE_REPLACE_EXISTING`), so that file is never
/// removed first and the path is never left empty by the rollback itself. The manifest is
/// restored only once the move has succeeded: if the move fails, the file at the index path,
/// if any, is the imported one, and writing the previous manifest into it would publish the
/// imported rows as the previous index.
fn roll_back_to_previous_index(
    err: anyhow::Error,
    index_path: &Path,
    backup_path: &Path,
    manifest: Option<&str>,
) -> anyhow::Error {
    if let Err(rename_err) = fs::rename(backup_path, index_path) {
        return err.context(format!(
            "moving the previous index database back from {backup} to {index} also failed \
             ({rename_err}); the previous database, without its manifest, is still at \
             {backup}, and the repository reads as unindexed; run `ok index`",
            backup = backup_path.display(),
            index = index_path.display()
        ));
    }
    restore_replaced_manifest(err, index_path, manifest)
}

/// Remove the manifest an artifact carries before its database is moved into place; the
/// import puts it back once the search index is rebuilt. The rename moves the database file
/// alone, so the delete is committed in rollback-journal mode, where it is in that file when
/// the statement returns; a WAL frame left beside the file would be lost with the manifest
/// still in it.
fn withhold_snapshot_manifest(db: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("opening {} to withhold its manifest", db.display()))?;
    conn.execute_batch("PRAGMA journal_mode = DELETE; DELETE FROM manifests;")
        .with_context(|| format!("withholding the index manifest of {}", db.display()))?;
    Ok(())
}

/// Withdraw the manifest of the database an import is about to replace, returning it so a
/// failed import can put it back.
///
/// Only a file that is not a readable index is replaced without a withdrawal: one SQLite
/// reports as not a database, or a database with no `manifests` table. No reader can serve
/// either as an index. Every other failure to open the database, read its schema or manifest,
/// or delete the manifest fails the import here, before the file is moved, so the previous
/// index stays published rather than being replaced while a session may still read it.
fn withdraw_replaced_manifest(index_path: &Path) -> anyhow::Result<Option<String>> {
    let left_in_place = |stage: &str| {
        format!(
            "{stage} the index database {} before replacing it; it was left in place, and if \
             it is damaged, remove it and run the import again",
            index_path.display()
        )
    };
    let conn = match Connection::open_with_flags(index_path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Ok(conn) => conn,
        Err(err) if is_not_a_database(&err) => return Ok(None),
        Err(err) => return Err(anyhow::Error::new(err).context(left_in_place("opening"))),
    };
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let manifests_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'manifests'",
            [],
            |_| Ok(()),
        )
        .optional();
    match manifests_table {
        Ok(Some(())) => {}
        Ok(None) => return Ok(None),
        Err(err) if is_not_a_database(&err) => return Ok(None),
        Err(err) => {
            return Err(anyhow::Error::new(err).context(left_in_place("reading the schema of")))
        }
    }
    let manifest = conn
        .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .with_context(|| left_in_place("reading the manifest of"))?;
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    conn.execute("DELETE FROM manifests", [])
        .with_context(|| left_in_place("withdrawing the manifest of"))?;
    Ok(Some(manifest))
}

fn is_not_a_database(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(error, _) if error.code == rusqlite::ErrorCode::NotADatabase
    )
}

/// Put back the manifest [`withdraw_replaced_manifest`] removed, once a failed import has
/// moved the previous database back to `index_path`. If that fails too, the previous index
/// stays unpublished and reads as unindexed, and the returned error says so.
fn restore_replaced_manifest(
    err: anyhow::Error,
    index_path: &Path,
    manifest: Option<&str>,
) -> anyhow::Error {
    let Some(manifest) = manifest else {
        return err;
    };
    let restored = Connection::open_with_flags(index_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .and_then(|conn| {
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            conn.execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1) \
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                params![manifest],
            )
        });
    match restored {
        Ok(_) => err,
        Err(restore_err) => err.context(format!(
            "restoring the previous index manifest at {} also failed ({restore_err}), so the \
             repository reads as unindexed; run `ok index`",
            index_path.display()
        )),
    }
}

/// Why there is no published index to read: a live writer is building one, or there is none.
fn unpublished_index_message(repo: &Path) -> String {
    if open_kioku_storage::generations::index_write_in_progress(repo) {
        open_kioku_storage::generations::indexing_in_progress_message(repo)
    } else {
        open_kioku_storage::generations::not_indexed_message(repo)
    }
}

fn remove_sqlite_sidecars(index_path: &Path) {
    let wal = index_path.with_extension("sqlite-wal");
    let shm = index_path.with_extension("sqlite-shm");
    let _ = fs::remove_file(wal);
    let _ = fs::remove_file(shm);
}

#[cfg(test)]
mod snapshot_publication_tests {
    use super::*;

    const PREVIOUS_MANIFEST: &str = r#"{"published_by":"the index before the import"}"#;

    /// A repository whose index holds a manifest, as `ok index` leaves it.
    fn indexed_repo() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let index_path = index_sqlite_path(temp.path());
        drop(SqliteStore::open(&index_path).unwrap());
        Connection::open(&index_path)
            .unwrap()
            .execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1)",
                params![PREVIOUS_MANIFEST],
            )
            .unwrap();
        (temp, index_path)
    }

    fn manifest_rows(db: &Path) -> Vec<String> {
        let conn = Connection::open(db).unwrap();
        let mut stmt = conn.prepare("SELECT json FROM manifests").unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    }

    #[test]
    fn a_withdrawn_manifest_is_restored_when_the_import_fails() {
        let (temp, index_path) = indexed_repo();
        let withdrawn = withdraw_replaced_manifest(&index_path).unwrap();
        assert_eq!(withdrawn.as_deref(), Some(PREVIOUS_MANIFEST));
        assert!(manifest_rows(&index_path).is_empty());
        assert!(
            matches!(SqliteStore::open_repo_index(temp.path()), Ok(None)),
            "a replaced index with its manifest withdrawn is not served"
        );

        let err = restore_replaced_manifest(
            anyhow::anyhow!("promoting imported snapshot failed"),
            &index_path,
            withdrawn.as_deref(),
        );
        assert_eq!(err.to_string(), "promoting imported snapshot failed");
        assert_eq!(manifest_rows(&index_path), vec![PREVIOUS_MANIFEST]);
    }

    /// The previous database did not come back to the index path, so its manifest cannot be
    /// put back: the error must say the repository reads as unindexed, and nothing may be
    /// created at the index path in the attempt.
    #[test]
    fn a_failed_restore_leaves_the_repository_unindexed_and_says_so() {
        let (temp, index_path) = indexed_repo();
        let withdrawn = withdraw_replaced_manifest(&index_path).unwrap();
        let aside = temp.path().join("moved-aside.sqlite");
        fs::rename(&index_path, &aside).unwrap();

        let err = restore_replaced_manifest(
            anyhow::anyhow!("promoting imported snapshot failed"),
            &index_path,
            withdrawn.as_deref(),
        );
        let message = format!("{err:#}");
        assert!(
            message.contains("reads as unindexed; run `ok index`"),
            "{message}"
        );
        assert!(
            message.contains("promoting imported snapshot failed"),
            "{message}"
        );
        assert!(!index_path.exists(), "restore must not create a database");

        fs::rename(&aside, &index_path).unwrap();
        assert!(
            matches!(SqliteStore::open_repo_index(temp.path()), Ok(None)),
            "without its manifest the previous database reads as unindexed"
        );
    }

    #[test]
    fn an_index_that_is_not_a_database_is_replaced_without_withdrawal() {
        let temp = tempfile::tempdir().unwrap();
        let index_path = index_sqlite_path(temp.path());
        fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        fs::write(&index_path, b"not a database").unwrap();
        assert_eq!(withdraw_replaced_manifest(&index_path).unwrap(), None);
        assert_eq!(fs::read(&index_path).unwrap(), b"not a database");
    }

    #[test]
    fn a_database_without_a_manifests_table_is_replaced_without_withdrawal() {
        let temp = tempfile::tempdir().unwrap();
        let index_path = index_sqlite_path(temp.path());
        fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        Connection::open(&index_path)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated(x);")
            .unwrap();
        assert_eq!(withdraw_replaced_manifest(&index_path).unwrap(), None);
    }

    /// The import failed with the previous database moved aside, and moving it back fails. The
    /// file at the index path is the imported database: the previous manifest must not be
    /// written into it, and the error must say where the previous database is.
    #[test]
    fn a_failed_move_back_does_not_publish_the_previous_manifest_into_the_imported_database() {
        let temp = tempfile::tempdir().unwrap();
        let index_path = index_sqlite_path(temp.path());
        drop(SqliteStore::open(&index_path).unwrap());
        // Nothing is at the backup path, so the move back fails.
        let backup_path = unique_temp_path(&temp.path().join(".ok"), "index.sqlite", "backup");

        let err = roll_back_to_previous_index(
            anyhow::anyhow!("imported snapshot failed SQLite open"),
            &index_path,
            &backup_path,
            Some(PREVIOUS_MANIFEST),
        );
        let message = format!("{err:#}");
        assert!(
            message.contains(&format!(
                "without its manifest, is still at {}",
                backup_path.display()
            )),
            "{message}"
        );
        assert!(
            message.contains("reads as unindexed; run `ok index`"),
            "{message}"
        );
        assert!(
            manifest_rows(&index_path).is_empty(),
            "the imported database must not receive the previous manifest"
        );
    }

    #[test]
    fn a_moved_back_database_replaces_the_imported_one_and_gets_its_manifest_again() {
        let (temp, index_path) = indexed_repo();
        let withdrawn = withdraw_replaced_manifest(&index_path).unwrap();
        let backup_path = unique_temp_path(&temp.path().join(".ok"), "index.sqlite", "backup");
        fs::rename(&index_path, &backup_path).unwrap();
        drop(SqliteStore::open(&index_path).unwrap());
        Connection::open(&index_path)
            .unwrap()
            .execute_batch("CREATE TABLE imported_marker(x);")
            .unwrap();

        let err = roll_back_to_previous_index(
            anyhow::anyhow!("imported snapshot failed SQLite open"),
            &index_path,
            &backup_path,
            withdrawn.as_deref(),
        );
        assert_eq!(err.to_string(), "imported snapshot failed SQLite open");
        assert!(!backup_path.exists());
        assert_eq!(manifest_rows(&index_path), vec![PREVIOUS_MANIFEST]);
        let imported_tables: i64 = Connection::open(&index_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'imported_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(imported_tables, 0, "the previous database is back in place");
    }

    /// The name an import gives its backup is the one read surfaces recognise, so a database
    /// an interrupted import left behind is named when the repository reads as unindexed.
    #[test]
    fn a_backup_left_by_an_interrupted_import_is_named_by_the_not_indexed_message() {
        let temp = tempfile::tempdir().unwrap();
        let ok_dir = temp.path().join(".ok");
        fs::create_dir_all(&ok_dir).unwrap();
        let backup_path = unique_temp_path(&ok_dir, "index.sqlite", "backup");
        fs::write(&backup_path, b"").unwrap();
        let message = open_kioku_storage::generations::not_indexed_message(temp.path());
        assert!(
            message.contains(&backup_path.display().to_string()),
            "{message}"
        );
    }
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

#[cfg(test)]
mod snapshot_export_copy_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const ROWS_PER_BATCH: i64 = 10;
    /// Batches the writer keeps; older ones are deleted, so later commits reuse freed pages
    /// and checkpoints rewrite pages already in the database file.
    const KEPT_BATCHES: i64 = 400;

    /// A ledger whose rows are written all-or-nothing per transaction, with a totals row
    /// updated in the same transaction. A copy mixing two committed states breaks one of the
    /// relations [`assert_one_committed_state`] checks.
    fn create_ledger(db: &Path) {
        Connection::open(db)
            .unwrap()
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE ledger (
                   batch INTEGER NOT NULL, slot INTEGER NOT NULL, payload BLOB NOT NULL,
                   PRIMARY KEY (batch, slot));
                 CREATE TABLE ledger_totals (
                   id INTEGER PRIMARY KEY CHECK (id = 1),
                   batches INTEGER NOT NULL, rows INTEGER NOT NULL);
                 INSERT INTO ledger_totals VALUES (1, 0, 0);",
            )
            .unwrap();
    }

    fn spawn_ledger_writer(
        db: PathBuf,
        stop: Arc<AtomicBool>,
        committed: Arc<AtomicU64>,
    ) -> thread::JoinHandle<rusqlite::Result<()>> {
        thread::spawn(move || {
            let mut conn = Connection::open(&db)?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            // Checkpoint after a few pages, so the database file is being rewritten while a
            // copy reads it: the state a file-byte copy tears.
            conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA wal_autocheckpoint = 16;")?;
            let mut batch = 0_i64;
            while !stop.load(Ordering::SeqCst) {
                let tx = conn.transaction()?;
                for slot in 0..ROWS_PER_BATCH {
                    tx.execute(
                        "INSERT INTO ledger (batch, slot, payload) VALUES (?1, ?2, zeroblob(1024))",
                        params![batch, slot],
                    )?;
                }
                let deleted = tx.execute(
                    "DELETE FROM ledger WHERE batch = ?1",
                    params![batch - KEPT_BATCHES],
                )? as i64;
                tx.execute(
                    "UPDATE ledger_totals SET batches = batches + 1 - ?1, rows = rows + ?2 - ?3",
                    params![i64::from(deleted > 0), ROWS_PER_BATCH, deleted],
                )?;
                tx.commit()?;
                committed.fetch_add(1, Ordering::SeqCst);
                batch += 1;
            }
            Ok(())
        })
    }

    fn assert_one_committed_state(db: &Path) {
        integrity_check_sqlite(db).unwrap();
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let partial_batches: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM (SELECT batch FROM ledger GROUP BY batch \
                 HAVING COUNT(*) <> ?1)",
                params![ROWS_PER_BATCH],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(partial_batches, 0, "a batch was copied partway through");
        let (batches, rows, span): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(DISTINCT batch), COUNT(*), \
                 COALESCE(MAX(batch) - MIN(batch) + 1, 0) FROM ledger",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let totals: (i64, i64) = conn
            .query_row("SELECT batches, rows FROM ledger_totals", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(totals, (batches, rows), "totals from another commit");
        assert_eq!(span, batches, "the kept batches are not one window");
    }

    /// Both qualities read one committed state from inside a read transaction, so each copy
    /// passes `integrity_check` and every relation the writer keeps per transaction holds,
    /// however the copy interleaves with the writer's commits and checkpoints.
    #[test]
    fn a_copy_is_one_committed_state_while_a_writer_commits() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("index.sqlite");
        create_ledger(&db);
        let stop = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicU64::new(0));
        let writer = spawn_ledger_writer(db.clone(), Arc::clone(&stop), Arc::clone(&committed));
        let waiting_since = std::time::Instant::now();
        while committed.load(Ordering::SeqCst) < KEPT_BATCHES as u64 {
            assert!(
                !writer.is_finished(),
                "the writer stopped before filling the ledger"
            );
            assert!(waiting_since.elapsed() < std::time::Duration::from_secs(60));
            thread::sleep(std::time::Duration::from_millis(10));
        }

        // A commit must land during copies of each quality, or the test proves nothing.
        let mut overlapped = [0_u32; 2];
        let started = std::time::Instant::now();
        let mut copies = 0_usize;
        while copies < 8
            || (overlapped.iter().any(|count| *count < 2)
                && started.elapsed() < std::time::Duration::from_secs(30))
        {
            let quality = [SnapshotQuality::Fast, SnapshotQuality::Best][copies % 2];
            let dest = temp.path().join(format!("copy-{copies}.sqlite"));
            let before = committed.load(Ordering::SeqCst);
            copy_index_database(&db, &dest, quality).unwrap();
            if committed.load(Ordering::SeqCst) > before {
                overlapped[copies % 2] += 1;
            }
            assert_one_committed_state(&dest);
            if quality == SnapshotQuality::Fast {
                let mode: String =
                    Connection::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .unwrap()
                        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                        .unwrap();
                assert_eq!(
                    mode, "delete",
                    "the artifact must not depend on a WAL sidecar"
                );
                assert!(!dest.with_extension("sqlite-wal").exists());
            }
            fs::remove_file(&dest).unwrap();
            copies += 1;
        }
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap().unwrap();
        assert!(
            overlapped.iter().all(|count| *count > 0),
            "no commit landed during a copy of each quality: {overlapped:?}"
        );
    }

    /// A copied state with no manifest is a full run between staging and publication, or a
    /// database that never had one. Export says which, as every read surface does, and the
    /// check runs on the copy, so a manifest withdrawn after the probe is caught too.
    #[test]
    fn a_copied_state_without_a_manifest_is_refused_with_the_repository_state() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let index_path = index_sqlite_path(repo);
        drop(SqliteStore::open(&index_path).unwrap());
        for quality in [SnapshotQuality::Fast, SnapshotQuality::Best] {
            let dest = repo.join(format!("copy-{quality}.sqlite"));
            let err = copy_published_index(repo, &index_path, &dest, quality).unwrap_err();
            assert_eq!(
                err.to_string(),
                open_kioku_storage::generations::not_indexed_message(repo)
            );
            fs::remove_file(&dest).unwrap();

            let lock = IndexWriteLock::acquire(repo, IndexWriteLock::DEFAULT_WAIT).unwrap();
            let err = copy_published_index(repo, &index_path, &dest, quality).unwrap_err();
            assert_eq!(
                err.to_string(),
                open_kioku_storage::generations::indexing_in_progress_message(repo)
            );
            drop(lock);
            fs::remove_file(&dest).unwrap();
        }
    }
}
