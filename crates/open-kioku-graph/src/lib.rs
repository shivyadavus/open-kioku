use chrono::Utc;
use open_kioku_core::{
    CodeChunk, Confidence, EdgeId, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,
    GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, NodeId, Symbol, SymbolOccurrence,
};
use open_kioku_errors::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

#[derive(Default, Clone)]
pub struct InMemoryGraph {
    pub nodes: HashMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl InMemoryGraph {
    pub fn from_index(files: &[File], symbols: &[Symbol], chunks: &[CodeChunk]) -> Self {
        Self::from_index_with_occurrences(files, symbols, chunks, &[])
    }

    pub fn from_index_with_occurrences(
        files: &[File],
        symbols: &[Symbol],
        chunks: &[CodeChunk],
        occurrences: &[SymbolOccurrence],
    ) -> Self {
        let mut graph = Self::default();
        for file in files {
            let node = GraphNode {
                id: NodeId::new(format!("file:{}", file.path.display())),
                node_type: GraphNodeType::File,
                label: file.path.display().to_string(),
                file_id: Some(file.id.clone()),
                symbol_id: None,
            };
            graph.nodes.insert(node.id.0.clone(), node);
        }
        for symbol in symbols {
            let symbol_node = GraphNode {
                id: NodeId::new(format!("symbol:{}", symbol.id.0)),
                node_type: symbol_node_type(symbol),
                label: symbol.qualified_name.clone(),
                file_id: Some(symbol.file_id.clone()),
                symbol_id: Some(symbol.id.clone()),
            };
            let Some(file) = files.iter().find(|file| file.id == symbol.file_id) else {
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
                },
            };
            graph.nodes.insert(symbol_node.id.0.clone(), symbol_node);
            graph.edges.push(edge);
        }
        for occurrence in occurrences
            .iter()
            .filter(|occurrence| !occurrence.is_definition)
        {
            let Some(file) = files.iter().find(|file| file.id == occurrence.file_id) else {
                continue;
            };
            let Some(symbol) = symbols
                .iter()
                .find(|symbol| symbol.id == occurrence.symbol_id)
            else {
                continue;
            };
            graph.edges.push(GraphEdge {
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
                },
            });
        }
        for chunk in chunks {
            let Some(symbol_id) = &chunk.symbol_id else {
                continue;
            };
            for other in symbols {
                if other.id == *symbol_id {
                    continue;
                }
                if chunk.text.contains(&other.name) {
                    graph.edges.push(GraphEdge {
                        id: EdgeId::new(stable_id(&format!("ref:{}:{}", chunk.id, other.id.0))),
                        from: NodeId::new(format!("symbol:{}", symbol_id.0)),
                        to: NodeId::new(format!("symbol:{}", other.id.0)),
                        edge_type: GraphEdgeType::References,
                        evidence: Evidence {
                            id: EvidenceId::new(stable_id(&format!(
                                "refev:{}:{}",
                                chunk.id, other.id.0
                            ))),
                            source: "open-kioku-graph".into(),
                            source_type: EvidenceSourceType::Heuristic,
                            file_range: None,
                            symbol_id: Some(other.id.clone()),
                            confidence: Confidence::Low,
                            message: format!("symbol text references {}", other.name),
                            indexed_at: Utc::now(),
                        },
                    });
                }
            }
        }
        graph
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
    use open_kioku_core::{FileId, Language, LineRange, RepositoryId, SymbolId, SymbolKind};

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
            },
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
            },
        });

        let path = graph.shortest_path("A", "C", 5);
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id.0, "e1");
        assert_eq!(path[1].id.0, "e2");
    }
}
