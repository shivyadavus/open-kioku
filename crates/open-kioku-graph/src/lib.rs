use chrono::Utc;
use open_kioku_core::{
    AnalysisFact, CodeChunk, EdgeId, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,
    GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import, NodeId, Symbol, SymbolOccurrence,
};
use open_kioku_errors::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

pub mod buffer;
pub mod schema;
pub mod query;

pub use buffer::{GraphBuffer, GraphBufferMergeReport, WorkerGraphBuffer};
#[derive(Default, Clone)]
pub struct InMemoryGraph {
    pub nodes: HashMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl InMemoryGraph {
    pub fn from_index(files: &[File], symbols: &[Symbol], chunks: &[CodeChunk]) -> Self {
        Self::from_index_with_analysis(files, symbols, chunks, &[], &[], &[])
    }

    pub fn from_index_with_occurrences(
        files: &[File],
        symbols: &[Symbol],
        _chunks: &[CodeChunk],
        occurrences: &[SymbolOccurrence],
    ) -> Self {
        Self::from_index_with_analysis(files, symbols, _chunks, occurrences, &[], &[])
    }

    pub fn from_index_with_analysis(
        files: &[File],
        symbols: &[Symbol],
        _chunks: &[CodeChunk],
        occurrences: &[SymbolOccurrence],
        imports: &[Import],
        analysis_facts: &[AnalysisFact],
    ) -> Self {
        let mut buffer = GraphBuffer::new();
        let files_by_id = files
            .iter()
            .map(|file| (file.id.0.as_str(), file))
            .collect::<HashMap<_, _>>();
        let symbols_by_id = symbols
            .iter()
            .map(|symbol| (symbol.id.0.as_str(), symbol))
            .collect::<HashMap<_, _>>();
        for file in files {
            let node = GraphNode {
                id: NodeId::new(format!("file:{}", file.path.display())),
                node_type: GraphNodeType::File,
                label: file.path.display().to_string(),
                file_id: Some(file.id.clone()),
                symbol_id: None,
                ..Default::default()
            };
            buffer.upsert_node(node);
        }
        for symbol in symbols {
            let symbol_node = GraphNode {
                id: NodeId::new(format!("symbol:{}", symbol.id.0)),
                node_type: symbol_node_type(symbol),
                label: symbol.qualified_name.clone(),
                file_id: Some(symbol.file_id.clone()),
                symbol_id: Some(symbol.id.clone()),
                ..Default::default()
            };
            let Some(file) = files_by_id.get(symbol.file_id.0.as_str()) else {
                continue;
            };
            let edge = GraphEdge {
                id: EdgeId::new(stable_id(&format!("defines:{}:{}", file.id.0, symbol.id.0))),
                from: NodeId::new(format!("file:{}", file.path.display())),
                to: symbol_node.id.clone(),
                edge_type: GraphEdgeType::Defines,
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!(
                        "evidence:{}:{}",
                        file.path.display(),
                        symbol.name
                    ))),
                    source: "open-kioku-graph".into(),
                    source_type: symbol.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: symbol.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: symbol.confidence,
                    message: format!("{} defines {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            };
            buffer.upsert_node(symbol_node);
            buffer.insert_edge(edge);
        }
        for occurrence in occurrences
            .iter()
            .filter(|occurrence| !occurrence.is_definition)
        {
            let Some(file) = files_by_id.get(occurrence.file_id.0.as_str()) else {
                continue;
            };
            let Some(symbol) = symbols_by_id.get(occurrence.symbol_id.0.as_str()) else {
                continue;
            };
            buffer.insert_edge(GraphEdge {
                id: EdgeId::new(stable_id(&format!(
                    "occurrence:{}:{}:{}",
                    file.id.0,
                    symbol.id.0,
                    occurrence
                        .range
                        .as_ref()
                        .map(|range| range.start)
                        .unwrap_or_default()
                ))),
                from: NodeId::new(format!("file:{}", file.path.display())),
                to: NodeId::new(format!("symbol:{}", symbol.id.0)),
                edge_type: GraphEdgeType::References,
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!(
                        "occurrence-evidence:{}:{}",
                        file.id.0, symbol.id.0
                    ))),
                    source: "open-kioku-graph".into(),
                    source_type: occurrence.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: occurrence.confidence,
                    message: format!("{} references {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        for import in imports {
            let Some(file) = files_by_id.get(import.file_id.0.as_str()) else {
                continue;
            };
            let target_node = GraphNode {
                id: analysis_node_id(GraphNodeType::Module, &import.imported),
                node_type: GraphNodeType::Module,
                label: import.imported.clone(),
                file_id: None,
                symbol_id: None,
                ..Default::default()
            };
            buffer.upsert_node(target_node.clone());
            let edge_id = EdgeId::new(stable_id(&format!(
                "import:{}:{}",
                file.id.0, import.imported
            )));
            buffer.insert_edge(GraphEdge {
                id: edge_id.clone(),
                from: NodeId::new(format!("file:{}", file.path.display())),
                to: target_node.id,
                edge_type: GraphEdgeType::Imports,
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!("import-evidence:{}", edge_id.0))),
                    source: "open-kioku-static/imports".into(),
                    source_type: EvidenceSourceType::StaticAnalysis,
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: import.range.clone(),
                    }),
                    symbol_id: None,
                    confidence: import.confidence,
                    message: format!("{} imports {}", file.path.display(), import.imported),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        for fact in analysis_facts {
            let Some(file) = files_by_id.get(fact.file_id.0.as_str()) else {
                continue;
            };
            let source_node = fact
                .symbol_id
                .as_ref()
                .and_then(|symbol_id| {
                    symbols_by_id
                        .get(symbol_id.0.as_str())
                        .map(|symbol| NodeId::new(format!("symbol:{}", symbol.id.0)))
                })
                .unwrap_or_else(|| NodeId::new(format!("file:{}", file.path.display())));

            let target_node = GraphNode {
                id: analysis_node_id(fact.target_kind.clone(), &fact.target),
                node_type: fact.target_kind.clone(),
                label: fact.target.clone(),
                file_id: None,
                symbol_id: None,
                ..Default::default()
            };
            buffer.upsert_node(target_node.clone());

            let edge_id = EdgeId::new(stable_id(&format!(
                "analysis:{}:{}:{:?}:{:?}",
                source_node.0, fact.target, fact.edge_type, fact.source_type
            )));

            buffer.insert_edge(GraphEdge {
                id: edge_id.clone(),
                from: source_node,
                to: target_node.id,
                edge_type: fact.edge_type.clone(),
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!("analysis-evidence:{}", edge_id.0))),
                    source: fact.source.clone(),
                    source_type: fact.source_type.clone(),
                    file_range: fact.range.as_ref().map(|range| FileRange {
                        path: file.path.clone(),
                        line_range: Some(range.clone()),
                    }),
                    symbol_id: fact.symbol_id.clone(),
                    confidence: fact.confidence,
                    message: fact.message.clone(),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
        }

        let (nodes_vec, edges) = buffer.into_parts();
        let mut nodes = HashMap::new();
        for node in nodes_vec {
            nodes.insert(node.id.0.clone(), node);
        }

        Self { nodes, edges }
    }

    pub fn neighbors(&self, node: &str, limit: usize) -> (Vec<GraphNode>, Vec<GraphEdge>) {
        let mut edges = self
            .edges
            .iter()
            .filter(|edge| edge.from.0 == node || edge.to.0 == node)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let nodes = edges
            .iter()
            .flat_map(|edge| [edge.from.0.clone(), edge.to.0.clone()])
            .filter_map(|id| self.nodes.get(&id).cloned())
            .collect::<Vec<_>>();
        edges.truncate(limit);
        (nodes, edges)
    }

    pub fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Vec<GraphEdge> {
        let mut queue = VecDeque::from([(from.to_string(), Vec::<GraphEdge>::new())]);
        let mut seen = std::collections::HashSet::new();
        while let Some((node, path)) = queue.pop_front() {
            if node == to {
                return path;
            }
            if path.len() >= max_depth || !seen.insert(node.clone()) {
                continue;
            }
            for edge in self.edges.iter().filter(|edge| edge.from.0 == node) {
                let mut next_path = path.clone();
                next_path.push(edge.clone());
                queue.push_back((edge.to.0.clone(), next_path));
            }
        }
        Vec::new()
    }
}

fn symbol_node_type(symbol: &Symbol) -> GraphNodeType {
    use open_kioku_core::SymbolKind;
    match symbol.kind {
        SymbolKind::Class => GraphNodeType::Class,
        SymbolKind::Trait => GraphNodeType::Trait,
        SymbolKind::Interface => GraphNodeType::Interface,
        SymbolKind::Method => GraphNodeType::Method,
        SymbolKind::Field => GraphNodeType::Field,
        SymbolKind::Endpoint => GraphNodeType::Endpoint,
        SymbolKind::DatabaseTable => GraphNodeType::DatabaseTable,
        SymbolKind::Test => GraphNodeType::Test,
        SymbolKind::Module | SymbolKind::Package => GraphNodeType::Module,
        _ => GraphNodeType::Function,
    }
}

fn analysis_node_id(node_type: GraphNodeType, label: &str) -> NodeId {
    NodeId::new(format!(
        "analysis:{node_type:?}:{}",
        stable_id(&label.to_ascii_lowercase())
    ))
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

impl open_kioku_storage::GraphStore for InMemoryGraph {
    fn replace_graph(&self, _nodes: &[GraphNode], _edges: &[GraphEdge]) -> Result<()> {
        Ok(())
    }

    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        Ok(self.neighbors(node, limit))
    }

    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        Ok(self.shortest_path(from, to, max_depth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType, GraphNodeType,
        Import, Language, LineRange, RepositoryId, Symbol, SymbolId, SymbolKind, SymbolOccurrence,
    };
    use std::path::PathBuf;

    fn make_file(id: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: format!("{id}.rs").into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn make_symbol(id: &str, file_id: &str, name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Function,
            file_id: FileId::new(file_id),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    #[test]
    fn builds_graph_from_index() {
        let file_a = make_file("a");
        let sym_a = make_symbol("s1", "a", "foo");

        let graph = InMemoryGraph::from_index_with_occurrences(&[file_a], &[sym_a], &[], &[]);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1); // file defines symbol
    }

    #[test]
    fn shortest_path_finds_route() {
        let mut graph = InMemoryGraph::default();
        graph.nodes.insert(
            "A".into(),
            GraphNode {
                id: NodeId::new("A"),
                node_type: GraphNodeType::File,
                label: "A".into(),
                file_id: None,
                symbol_id: None,
                ..Default::default()
            },
        );
        graph.nodes.insert(
            "B".into(),
            GraphNode {
                id: NodeId::new("B"),
                node_type: GraphNodeType::File,
                label: "B".into(),
                file_id: None,
                symbol_id: None,
                ..Default::default()
            },
        );
        graph.nodes.insert(
            "C".into(),
            GraphNode {
                id: NodeId::new("C"),
                node_type: GraphNodeType::File,
                label: "C".into(),
                file_id: None,
                symbol_id: None,
                ..Default::default()
            },
        );

        graph.edges.push(GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("A"),
            to: NodeId::new("B"),
            edge_type: GraphEdgeType::References,
            evidence: Evidence {
                id: EvidenceId::new("ev1"),
                source: "".into(),
                source_type: EvidenceSourceType::Lexical,
                file_range: None,
                symbol_id: None,
                confidence: Confidence::High,
                message: "".into(),
                indexed_at: Utc::now(),
                ..Default::default()
            },
            ..Default::default()
        });
        graph.edges.push(GraphEdge {
            id: EdgeId::new("e2"),
            from: NodeId::new("B"),
            to: NodeId::new("C"),
            edge_type: GraphEdgeType::References,
            evidence: Evidence {
                id: EvidenceId::new("ev2"),
                source: "".into(),
                source_type: EvidenceSourceType::Lexical,
                file_range: None,
                symbol_id: None,
                confidence: Confidence::High,
                message: "".into(),
                indexed_at: Utc::now(),
                ..Default::default()
            },
            ..Default::default()
        });

        let path = graph.shortest_path("A", "C", 5);
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id.0, "e1");
        assert_eq!(path[1].id.0, "e2");
    }

    #[test]
    fn graph_includes_imports_and_language_analysis_facts() {
        let file = make_file("src/service");
        let symbol = make_symbol("sym-service", "src/service", "Service");
        let import = Import {
            file_id: file.id.clone(),
            imported: "com.acme.Client".into(),
            range: Some(LineRange::single(1)),
            confidence: Confidence::Medium,
        };
        let fact = AnalysisFact {
            id: "endpoint-fact".into(),
            file_id: file.id.clone(),
            symbol_id: Some(symbol.id.clone()),
            target: "GET /orders".into(),
            target_kind: GraphNodeType::Endpoint,
            edge_type: GraphEdgeType::ExposesEndpoint,
            range: Some(LineRange::single(3)),
            confidence: Confidence::Medium,
            source: "test-static".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "test endpoint".into(),
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            &[file],
            &[symbol],
            &[],
            &[],
            &[import],
            &[fact],
        );

        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.edge_type == GraphEdgeType::Imports));
        assert!(graph.edges.iter().any(|edge| {
            edge.edge_type == GraphEdgeType::ExposesEndpoint
                && graph
                    .nodes
                    .get(&edge.to.0)
                    .is_some_and(|node| node.label == "GET /orders")
        }));
    }

    #[test]
    fn test_duplicate_import_edges_are_collapsed() {
        let file = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let import1 = Import {
            file_id: file.id.clone(),
            imported: "std::collections::HashMap".into(),
            range: Some(LineRange { start: 1, end: 1 }),
            confidence: Confidence::Exact,
        };
        let import2 = Import {
            file_id: file.id.clone(),
            imported: "std::collections::HashMap".into(),
            range: Some(LineRange { start: 2, end: 2 }),
            confidence: Confidence::High,
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            &[file],
            &[],
            &[],
            &[],
            &[import1, import2],
            &[],
        );

        let imports = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == GraphEdgeType::Imports)
            .collect::<Vec<_>>();
        assert_eq!(imports.len(), 1);
    }

    #[test]
    fn test_duplicate_reference_edges_are_collapsed() {
        let file = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("s1"),
            file_id: file.id.clone(),
            language: Language::Rust,
            name: "foo".into(),
            qualified_name: "foo".into(),
            kind: SymbolKind::Function,
            range: Some(LineRange { start: 1, end: 5 }),
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };
        let occ1 = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange { start: 10, end: 10 }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };
        // Duplicate reference to same symbol from same file
        let occ2 = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange { start: 12, end: 12 }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            &[file],
            &[symbol],
            &[],
            &[occ1, occ2],
            &[],
            &[],
        );

        let refs = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == GraphEdgeType::References)
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn test_analysis_fact_edges_survive_buffering() {
        let file = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let fact = AnalysisFact {
            id: "fact1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "foo".into(),
            target_kind: GraphNodeType::Function,
            edge_type: GraphEdgeType::Calls,
            range: None,
            confidence: Confidence::High,
            source: "analyzer".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "msg".into(),
        };

        let graph = InMemoryGraph::from_index_with_analysis(&[file], &[], &[], &[], &[], &[fact]);
        let edges = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == GraphEdgeType::Calls)
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.message, "msg");
    }

    #[test]
    fn test_buffer_output_is_stable_across_repeated_runs() {
        let file1 = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("a.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let file2 = File {
            id: FileId::new("f2"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("b.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };

        let graph1 = InMemoryGraph::from_index_with_analysis(
            &[file1.clone(), file2.clone()],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let graph2 = InMemoryGraph::from_index_with_analysis(
            &[file2.clone(), file1.clone()],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let mut keys1 = graph1.nodes.keys().collect::<Vec<_>>();
        keys1.sort();
        let mut keys2 = graph2.nodes.keys().collect::<Vec<_>>();
        keys2.sort();
        assert_eq!(keys1, keys2);
    }

    #[test]
    fn test_from_index_with_analysis_preserves_files_symbols_occurrences_imports_analysis_facts() {
        let file = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("s1"),
            file_id: file.id.clone(),
            language: Language::Rust,
            name: "foo".into(),
            qualified_name: "foo".into(),
            kind: SymbolKind::Function,
            range: Some(LineRange { start: 1, end: 5 }),
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };
        let occ = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange { start: 10, end: 10 }),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Lsp,
        };
        let import = Import {
            file_id: file.id.clone(),
            imported: "std::collections::HashMap".into(),
            range: Some(LineRange { start: 1, end: 1 }),
            confidence: Confidence::Exact,
        };
        let fact = AnalysisFact {
            id: "fact1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "GET /api".into(),
            target_kind: GraphNodeType::Endpoint,
            edge_type: GraphEdgeType::ExposesEndpoint,
            range: None,
            confidence: Confidence::High,
            source: "analyzer".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "msg".into(),
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            &[file],
            &[symbol],
            &[],
            &[occ],
            &[import],
            &[fact],
        );

        // Assert files
        assert!(graph
            .nodes
            .values()
            .any(|n| n.node_type == GraphNodeType::File));
        // Assert symbols
        assert!(graph
            .nodes
            .values()
            .any(|n| n.node_type == GraphNodeType::Function));
        assert!(graph
            .edges
            .iter()
            .any(|e| e.edge_type == GraphEdgeType::Defines));
        // Assert occurrences
        assert!(graph
            .edges
            .iter()
            .any(|e| e.edge_type == GraphEdgeType::References));
        // Assert imports
        assert!(graph
            .edges
            .iter()
            .any(|e| e.edge_type == GraphEdgeType::Imports));
        assert!(graph
            .nodes
            .values()
            .any(|n| n.node_type == GraphNodeType::Module));
        // Assert facts
        assert!(graph
            .edges
            .iter()
            .any(|e| e.edge_type == GraphEdgeType::ExposesEndpoint));
        assert!(graph
            .nodes
            .values()
            .any(|n| n.node_type == GraphNodeType::Endpoint));
    }
}
