from pathlib import Path

semantic = Path("crates/open-kioku-semantic/src/lib.rs")
text = semantic.read_text()

def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one marker, found {count}")
    text = text.replace(old, new, 1)

replace_once(
    "    pub filter_selectivity: String,\n    pub path_scope_enforced: bool,\n    pub routing_reason: String,\n",
    "    pub filter_selectivity: String,\n    /// Whether any supported semantic candidate filter was enforced before top-k selection.\n    /// This is broader than `path_scope_enforced`: callers may supply a precomputed vector\n    /// allowlist for language, project, symbol, module, or another repository-validated scope.\n    #[serde(default)]\n    pub filter_scope_enforced: bool,\n    pub path_scope_enforced: bool,\n    pub routing_reason: String,\n",
    "routing telemetry field",
)
replace_once(
    "    pub fn search_with_allowlist(\n        &self,\n        query: &str,\n        limit: usize,\n        allowlist: Option<HashSet<VectorId>>,\n    ) -> Result<Vec<SearchResult>> {\n        Ok(self.search_routed(query, limit, allowlist, &[])?.results)\n    }\n",
    "    pub fn search_with_allowlist(\n        &self,\n        query: &str,\n        limit: usize,\n        allowlist: Option<HashSet<VectorId>>,\n    ) -> Result<Vec<SearchResult>> {\n        Ok(self\n            .search_with_allowlist_report(query, limit, allowlist)?\n            .results)\n    }\n\n    /// Search a repository-validated semantic candidate set and return routing diagnostics.\n    ///\n    /// The allowlist is intentionally generic: higher layers can derive it from language,\n    /// project/workspace, symbol/module, path, or another scope already represented in repository\n    /// metadata. The semantic layer treats the set as an enforced eligibility boundary and never\n    /// widens it while choosing between exact-flat and ANN.\n    pub fn search_with_allowlist_report(\n        &self,\n        query: &str,\n        limit: usize,\n        allowlist: Option<HashSet<VectorId>>,\n    ) -> Result<SemanticSearchReport> {\n        self.search_routed(query, limit, allowlist, &[])\n    }\n",
    "allowlist report API",
)
replace_once(
    "        let allowlist = intersect_allowlists(caller_allowlist, scoped_ids);\n        let eligible_candidate_count = allowlist\n",
    "        let allowlist = intersect_allowlists(caller_allowlist, scoped_ids);\n        // Any concrete allowlist is an enforced eligibility boundary, regardless of which\n        // higher-level filter produced it. Backend routing must depend on the effective candidate\n        // population, not on whether the filter happened to be a path prefix.\n        let filter_scope_enforced = allowlist.is_some();\n        let eligible_candidate_count = allowlist\n",
    "filter enforcement",
)
replace_once(
    "        let filter_selectivity = semantic_filter_selectivity(\n            total_vector_count,\n            eligible_candidate_count,\n            path_scope_enforced || allowlist.is_some(),\n        );\n",
    "        let filter_selectivity = semantic_filter_selectivity(\n            total_vector_count,\n            eligible_candidate_count,\n            filter_scope_enforced,\n        );\n",
    "selectivity input",
)
replace_once(
    "        let mut routing_reason = if path_scope_enforced {\n            format!(\n                \"validated path scope leaves {eligible_candidate_count} of {total_vector_count} semantic candidates\"\n            )\n        } else {\n            format!(\"semantic query uses the persisted {manifest_backend} backend\")\n        };\n",
    "        let mut routing_reason = if path_scope_enforced {\n            format!(\n                \"validated path scope leaves {eligible_candidate_count} of {total_vector_count} semantic candidates\"\n            )\n        } else if filter_scope_enforced {\n            format!(\n                \"validated semantic candidate filter leaves {eligible_candidate_count} of {total_vector_count} candidates\"\n            )\n        } else {\n            format!(\"semantic query uses the persisted {manifest_backend} backend\")\n        };\n",
    "routing reason",
)
replace_once(
    "        let should_use_scoped_exact = path_scope_enforced\n            && should_route_scoped_exact(&self.config, manifest_backend, eligible_candidate_count);\n",
    "        let should_use_scoped_exact = filter_scope_enforced\n            && should_route_scoped_exact(&self.config, manifest_backend, eligible_candidate_count);\n",
    "generic exact routing",
)
replace_once(
    "                      \"validated semantic path scope lost its candidate allowlist; refusing to widen retrieval\"\n",
    "                      \"validated semantic candidate filter lost its allowlist; refusing to widen retrieval\"\n",
    "fail-closed allowlist error",
)
replace_once(
    "                    routing_reason = format!(\n                        \"auto backend selected exact-flat because validated scope has {eligible_candidate_count} eligible candidates below ann_min_rows={} (total vectors {total_vector_count})\",\n                        self.config.ann_min_rows\n                    );\n",
    "                    routing_reason = format!(\n                        \"auto backend selected exact-flat because the enforced filter has {eligible_candidate_count} eligible candidates below ann_min_rows={} (total vectors {total_vector_count})\",\n                        self.config.ann_min_rows\n                    );\n",
    "generic exact reason",
)
replace_once(
    "                        \"scoped exact-flat routing could not reconstruct a complete exact subset from the local embedding cache; retained pre-filtered ANN search without widening scope\"\n",
    "                        \"filtered exact-flat routing could not reconstruct a complete exact subset from the local embedding cache; retained pre-filtered ANN search without widening scope\"\n",
    "generic cache caveat",
)
replace_once(
    "                        \"auto backend retained {manifest_backend} because the exact subset cache was incomplete; validated path scope still filters before ANN top-k\"\n",
    "                        \"auto backend retained {manifest_backend} because the exact subset cache was incomplete; the enforced candidate filter still applies before ANN top-k\"\n",
    "generic cache fallback reason",
)
replace_once(
    "            if path_scope_enforced && backend_is_ann(manifest_backend) {\n                routing_reason = if self.config.backend == \"auto\" {\n                    format!(\n                        \"auto backend retained {manifest_backend} because validated scope has {eligible_candidate_count} eligible candidates at or above ann_min_rows={} (total vectors {total_vector_count})\",\n                        self.config.ann_min_rows\n                    )\n                } else {\n                    format!(\n                        \"explicit backend `{}` retained {manifest_backend}; validated path scope filters candidates before ANN top-k\",\n                        self.config.backend\n                    )\n                };\n            }\n",
    "            if filter_scope_enforced && backend_is_ann(manifest_backend) {\n                routing_reason = if self.config.backend == \"auto\" {\n                    format!(\n                        \"auto backend retained {manifest_backend} because the enforced filter has {eligible_candidate_count} eligible candidates at or above ann_min_rows={} (total vectors {total_vector_count})\",\n                        self.config.ann_min_rows\n                    )\n                } else {\n                    format!(\n                        \"explicit backend `{}` retained {manifest_backend}; the enforced candidate filter applies before ANN top-k\",\n                        self.config.backend\n                    )\n                };\n            }\n",
    "generic ANN reason",
)
replace_once(
    "                filter_selectivity,\n                path_scope_enforced,\n                routing_reason,\n",
    "                filter_selectivity,\n                filter_scope_enforced,\n                path_scope_enforced,\n                routing_reason,\n",
    "routing telemetry output",
)

marker = "    #[test]\n    fn explicit_ann_is_not_overridden_by_selective_scope() {\n"
test = r'''    #[test]
    fn auto_ann_routes_selective_precomputed_allowlist_to_exact_flat() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let manifest = open_kioku_core::IndexManifest {
            repository: open_kioku_core::Repository {
                id: RepositoryId("repo".into()),
                name: "repo".into(),
                root: temp.path().to_path_buf(),
                branch: Some("main".into()),
                commit: Some("abc".into()),
                indexed_at: Some(Utc::now()),
            },
            file_count: 3,
            symbol_count: 3,
            chunk_count: 3,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: Default::default(),
        };
        let files = vec![
            file("file_auth", "src/auth.rs"),
            file("file_billing", "src/billing.rs"),
            file("file_profile", "src/profile.rs"),
        ];
        let symbols = vec![
            symbol("symbol_auth", "issue_token", "file_auth"),
            symbol("symbol_billing", "issue_invoice", "file_billing"),
            symbol("symbol_profile", "load_profile", "file_profile"),
        ];
        let chunks = vec![
            chunk("chunk_auth", "file_auth", "pub fn issue_token() { create session token }", Some("symbol_auth")),
            chunk("chunk_billing", "file_billing", "pub fn issue_invoice() { billing invoice }", Some("symbol_billing")),
            chunk("chunk_profile", "file_profile", "pub fn load_profile() { user profile }", Some("symbol_profile")),
        ];
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
                symbols: &symbols,
                chunks: &chunks,
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let mut config = semantic_config();
        config.backend = "auto".into();
        config.ann_min_rows = 5;
        let manager = SemanticIndexManager::new(temp.path(), &store, &config);
        let indexed = manager.index().unwrap();
        assert_eq!(indexed.status.backend, "usearch-hnsw-f32");

        let targets = read_targets(&manager.current_dir().join("ids.json")).unwrap();
        let auth_ids = targets
            .values()
            .filter(|target| target.path == std::path::Path::new("src/auth.rs"))
            .map(|target| target.vector_id)
            .collect::<HashSet<_>>();
        assert_eq!(auth_ids.len(), 2);

        let report = manager
            .search_with_allowlist_report("issue token", 5, Some(auth_ids))
            .unwrap();
        assert_eq!(report.routing.selected_backend, "exact-flat");
        assert!(report.routing.filter_scope_enforced);
        assert!(!report.routing.path_scope_enforced);
        assert_eq!(report.routing.total_vector_count, 6);
        assert_eq!(report.routing.eligible_candidate_count, 2);
        assert_eq!(report.routing.filter_selectivity, "medium-selectivity");
        assert!(report
            .results
            .iter()
            .all(|result| result.path.as_path() == std::path::Path::new("src/auth.rs")));
    }

'''
count = text.count(marker)
if count != 1:
    raise SystemExit(f"generic allowlist test marker count={count}")
text = text.replace(marker, test + marker, 1)
semantic.write_text(text)

vector = Path("crates/open-kioku-vector/src/lib.rs")
vtext = vector.read_text()
marker = "    #[test]\n    fn hnsw_preserves_filters_parameters_and_persistence_without_duplicate_vectors() {\n"
test = r'''    #[test]
    fn filtered_hnsw_matches_exact_oracle_across_selectivity_and_top_k() {
        let records = [
            record(1, "a", "chunk", &[1.0, 0.0, 0.0]),
            record(2, "b", "chunk", &[0.98, 0.2, 0.0]),
            record(3, "c", "symbol", &[0.8, 0.6, 0.0]),
            record(4, "d", "chunk", &[0.6, 0.8, 0.0]),
            record(5, "e", "symbol", &[0.2, 0.98, 0.0]),
            record(6, "f", "chunk", &[0.0, 1.0, 0.0]),
            record(7, "g", "chunk", &[0.0, 0.8, 0.6]),
            record(8, "h", "symbol", &[0.0, 0.2, 0.98]),
            record(9, "i", "chunk", &[0.0, 0.0, 1.0]),
            record(10, "j", "chunk", &[0.5, 0.5, 0.707]),
        ];
        let mut exact = ExactFlatVectorIndex::new(3).unwrap();
        let mut hnsw = UsearchHnswVectorIndex::new(3, AnnScalarKind::F32, records.len()).unwrap();
        for value in records {
            exact.add(value.clone()).unwrap();
            hnsw.add(value).unwrap();
        }

        let scopes = [
            HashSet::from([VectorId(1)]),
            HashSet::from([VectorId(1), VectorId(2), VectorId(3)]),
            HashSet::from([
                VectorId(1), VectorId(2), VectorId(3), VectorId(4), VectorId(5),
                VectorId(6), VectorId(7), VectorId(8),
            ]),
        ];
        for allowlist in scopes {
            for limit in [1usize, 3, 5] {
                let options = VectorSearchOptions {
                    limit,
                    allowlist: Some(allowlist.clone()),
                    target_kind: None,
                };
                let exact_hits = exact.search(&[1.0, 0.0, 0.0], options.clone()).unwrap();
                let hnsw_hits = hnsw.search(&[1.0, 0.0, 0.0], options).unwrap();
                assert_eq!(
                    exact_hits.iter().map(|hit| &hit.target_id).collect::<Vec<_>>(),
                    hnsw_hits.iter().map(|hit| &hit.target_id).collect::<Vec<_>>(),
                    "filtered ANN must preserve exact-oracle top-k for allowlist size {} and k={limit}",
                    allowlist.len()
                );
            }
        }
    }

'''
count = vtext.count(marker)
if count != 1:
    raise SystemExit(f"filtered parity test marker count={count}")
vtext = vtext.replace(marker, test + marker, 1)
vector.write_text(vtext)
