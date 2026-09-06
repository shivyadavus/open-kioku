use open_kioku_core::{
    identity, EdgeId, Evidence, EvidenceSourceType, GraphEdge, GraphEdgeType, GraphNode, NodeId,
    RELATIONSHIP_PROOFS_PROPERTY,
};
use std::collections::{BTreeSet, HashMap};

#[derive(Default, Debug, Clone)]
pub struct GraphBufferMergeReport {
    pub nodes_merged: usize,
    pub edges_merged: usize,
    pub duplicates_collapsed: usize,
}

#[derive(Default)]
pub struct GraphBuffer {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    node_by_id: HashMap<NodeId, usize>,
    node_by_key: HashMap<String, NodeId>,
    edges_by_key: HashMap<(NodeId, NodeId, GraphEdgeType), usize>,
    edges_by_source_type: HashMap<(NodeId, GraphEdgeType), Vec<usize>>,
    edges_by_target_type: HashMap<(NodeId, GraphEdgeType), Vec<usize>>,
}

pub struct WorkerGraphBuffer {
    pub worker_id: usize,
    pub inner: GraphBuffer,
}

impl WorkerGraphBuffer {
    pub fn new(worker_id: usize) -> Self {
        Self {
            worker_id,
            inner: GraphBuffer::new(),
        }
    }

    pub fn into_inner(self) -> GraphBuffer {
        self.inner
    }

    pub fn merge_into(self, target: &mut GraphBuffer) -> GraphBufferMergeReport {
        target.merge(self.inner)
    }
}

fn node_key(node: &GraphNode) -> String {
    format!(
        "{:?}|{}|{}|{}",
        node.node_type,
        node.label,
        node.file_id.as_ref().map(|v| v.0.as_str()).unwrap_or(""),
        node.symbol_id.as_ref().map(|v| v.0.as_str()).unwrap_or("")
    )
}

fn evidence_rank(e: &Evidence) -> (u8, f32) {
    let source_rank = match e.source_type {
        EvidenceSourceType::Scip => 100,
        EvidenceSourceType::Lsp => 95,
        EvidenceSourceType::TreeSitter => 90,
        EvidenceSourceType::StaticAnalysis => 85,
        EvidenceSourceType::Runtime => 80,
        EvidenceSourceType::GitHistory => 70,
        EvidenceSourceType::Regex => 55,
        EvidenceSourceType::Lexical => 45,
        EvidenceSourceType::Semantic => 35,
        EvidenceSourceType::ExternalIntegration => 30,
        EvidenceSourceType::Heuristic => 20,
    };
    (source_rank, e.confidence.score())
}

fn merge_messages(a: &str, b: &str) -> String {
    let mut messages = BTreeSet::new();
    for msg in [a, b] {
        for line in msg.lines() {
            let line = line.trim();
            if !line.is_empty() {
                messages.insert(line.to_string());
            }
        }
    }
    messages.into_iter().take(8).collect::<Vec<_>>().join("\n")
}

fn push_unique_site(sites: &mut Vec<serde_json::Value>, site: serde_json::Value) {
    if !sites.contains(&site) {
        sites.push(site);
    }
}

fn structured_site_key(site: &serde_json::Value) -> (String, u64, u64, u64, u64) {
    let number = |key: &str| {
        site.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(u64::MAX)
    };
    (
        site.get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        number("start_line"),
        number("start_column"),
        number("end_line"),
        number("end_column"),
    )
}

fn normalize_structured_sites(sites: &mut Vec<serde_json::Value>) {
    sites.sort_by_key(structured_site_key);
    sites.dedup();
}

fn file_range_call_site(file_range: &open_kioku_core::FileRange) -> serde_json::Value {
    let (start_line, end_line) = file_range
        .line_range
        .as_ref()
        .map(|range| (Some(range.start), Some(range.end)))
        .unwrap_or((None, None));
    serde_json::json!({
        "path": file_range.path.to_string_lossy(),
        "start_line": start_line,
        "start_column": serde_json::Value::Null,
        "end_line": end_line,
        "end_column": serde_json::Value::Null,
    })
}

fn merge_edge_metadata(existing: &mut GraphEdge, incoming: GraphEdge) {
    let incoming_relationship_proofs = incoming.relationship_proofs();
    let mut call_sites: Vec<serde_json::Value> = existing
        .properties
        .get("call_sites")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    if call_sites.is_empty() {
        if let Some(existing_fr) = &existing.evidence.file_range {
            push_unique_site(&mut call_sites, file_range_call_site(existing_fr));
        }
    }

    if let Some(incoming_sites) = incoming
        .properties
        .get("call_sites")
        .and_then(|value| value.as_array())
    {
        for site in incoming_sites {
            push_unique_site(&mut call_sites, site.clone());
        }
    } else if let Some(incoming_fr) = &incoming.evidence.file_range {
        push_unique_site(&mut call_sites, file_range_call_site(incoming_fr));
    }

    if !call_sites.is_empty() {
        normalize_structured_sites(&mut call_sites);
        existing.properties.insert(
            "call_sites".to_string(),
            serde_json::Value::Array(call_sites),
        );
    }

    let mut reference_sites = existing
        .properties
        .get("reference_sites")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(incoming_sites) = incoming
        .properties
        .get("reference_sites")
        .and_then(|value| value.as_array())
    {
        for site in incoming_sites {
            push_unique_site(&mut reference_sites, site.clone());
        }
    }
    if !reference_sites.is_empty() {
        normalize_structured_sites(&mut reference_sites);
        existing.properties.insert(
            "reference_sites".to_string(),
            serde_json::Value::Array(reference_sites),
        );
    }

    for (k, v) in incoming.properties {
        if k != "call_sites" && k != "reference_sites" && k != RELATIONSHIP_PROOFS_PROPERTY {
            existing.properties.entry(k).or_insert(v);
        }
    }

    let mut merged_relationship_proofs = existing.relationship_proofs();
    merged_relationship_proofs.extend(incoming_relationship_proofs);
    if existing
        .set_relationship_proofs(merged_relationship_proofs)
        .is_err()
    {
        existing.properties.remove(RELATIONSHIP_PROOFS_PROPERTY);
    }

    existing.ambiguity.extend(incoming.ambiguity);
    existing.ambiguity.sort();
    existing.ambiguity.dedup();

    existing.quality_notes.extend(incoming.quality_notes);
    existing.quality_notes.sort();
    existing.quality_notes.dedup();

    if existing.schema_version.is_none() {
        existing.schema_version = incoming.schema_version;
    }
    if existing.source_pass.is_none() {
        existing.source_pass = incoming.source_pass;
    }
    if existing.index_mode.is_none() {
        existing.index_mode = incoming.index_mode;
    }
    if existing.extractor_version.is_none() {
        existing.extractor_version = incoming.extractor_version;
    }
}

impl GraphBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_node(&mut self, mut node: GraphNode) -> NodeId {
        let key = node_key(&node);
        if let Some(existing_id) = self.node_by_key.get(&key) {
            node.id = existing_id.clone();
        } else {
            self.node_by_key.insert(key.clone(), node.id.clone());
        }

        if let Some(&index) = self.node_by_id.get(&node.id) {
            let existing = &mut self.nodes[index];
            if existing.label.is_empty() && !node.label.is_empty() {
                existing.label = node.label.clone();
            }
            if existing.file_id.is_none() && node.file_id.is_some() {
                existing.file_id = node.file_id.clone();
            }
            if existing.symbol_id.is_none() && node.symbol_id.is_some() {
                existing.symbol_id = node.symbol_id.clone();
            }
            if existing.schema_version.is_none() && node.schema_version.is_some() {
                existing.schema_version = node.schema_version.clone();
            }
            if existing.source_pass.is_none() && node.source_pass.is_some() {
                existing.source_pass = node.source_pass.clone();
            }
            if existing.index_mode.is_none() && node.index_mode.is_some() {
                existing.index_mode = node.index_mode.clone();
            }
            if existing.extractor_version.is_none() && node.extractor_version.is_some() {
                existing.extractor_version = node.extractor_version.clone();
            }
            for (k, v) in node.properties {
                existing.properties.insert(k, v);
            }
            for amb in node.ambiguity {
                if !existing.ambiguity.contains(&amb) {
                    existing.ambiguity.push(amb);
                }
            }
            for qn in node.quality_notes {
                if !existing.quality_notes.contains(&qn) {
                    existing.quality_notes.push(qn);
                }
            }
            existing.id.clone()
        } else {
            let index = self.nodes.len();
            self.node_by_id.insert(node.id.clone(), index);
            let id = node.id.clone();
            self.nodes.push(node);
            id
        }
    }

    pub fn insert_edge(&mut self, mut edge: GraphEdge) -> EdgeId {
        let key = (edge.from.clone(), edge.to.clone(), edge.edge_type.clone());
        let expected_edge_id =
            identity::edge_id(edge.edge_type.clone(), &edge.from, &edge.to, None);
        edge.id = expected_edge_id.clone();

        if let Some(&index) = self.edges_by_key.get(&key) {
            let existing = &mut self.edges[index];
            let existing_rank = evidence_rank(&existing.evidence);
            let new_rank = evidence_rank(&edge.evidence);

            if new_rank > existing_rank
                || (new_rank == existing_rank && edge.evidence.id.0 < existing.evidence.id.0)
            {
                let merged_msg = merge_messages(&edge.evidence.message, &existing.evidence.message);
                edge.evidence.message = merged_msg.into();
                merge_edge_metadata(&mut edge, existing.clone());
                self.edges[index] = edge.clone();
            } else {
                existing.evidence.message =
                    merge_messages(&existing.evidence.message, &edge.evidence.message).into();
                merge_edge_metadata(existing, edge);
            }
            self.edges[index].id.clone()
        } else {
            let index = self.edges.len();
            self.edges_by_key.insert(key.clone(), index);
            self.edges_by_source_type
                .entry((edge.from.clone(), edge.edge_type.clone()))
                .or_default()
                .push(index);
            self.edges_by_target_type
                .entry((edge.to.clone(), edge.edge_type.clone()))
                .or_default()
                .push(index);
            let id = edge.id.clone();
            self.edges.push(edge);
            id
        }
    }

    pub fn merge(&mut self, other: GraphBuffer) -> GraphBufferMergeReport {
        let mut report = GraphBufferMergeReport::default();
        let initial_nodes = self.nodes.len();
        let initial_edges = self.edges.len();

        for node in other.nodes {
            self.upsert_node(node);
            report.nodes_merged += 1;
        }
        let after_nodes = self.nodes.len();
        report.duplicates_collapsed += report.nodes_merged - (after_nodes - initial_nodes);

        for edge in other.edges {
            self.insert_edge(edge);
            report.edges_merged += 1;
        }
        let after_edges = self.edges.len();
        report.duplicates_collapsed += report.edges_merged - (after_edges - initial_edges);

        report
    }

    pub fn into_parts(mut self) -> (Vec<GraphNode>, Vec<GraphEdge>) {
        self.nodes.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        self.edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        (self.nodes, self.edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Confidence, GraphNodeType};

    #[test]
    fn test_node_dedupe() {
        let mut buffer = GraphBuffer::new();
        let node1 = GraphNode {
            id: NodeId::new("1"),
            node_type: GraphNodeType::Function,
            label: "funcA".into(),
            file_id: None,
            symbol_id: None,
            ..Default::default()
        };
        let mut node2 = node1.clone();
        node2.label = "funcA_updated".into();

        let id1 = buffer.upsert_node(node1);
        let id2 = buffer.upsert_node(node2);

        assert_eq!(id1, id2);
        let (nodes, _) = buffer.into_parts();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "funcA");
    }

    #[test]
    fn test_edge_dedupe_keeps_highest_confidence() {
        let mut buffer = GraphBuffer::new();
        let mut edge1 = GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        edge1.evidence.source_type = EvidenceSourceType::Heuristic;
        edge1.evidence.confidence = Confidence::Low;
        edge1.evidence.message = "heuristic call".into();

        let mut edge2 = edge1.clone();
        edge2.id = EdgeId::new("e2");
        edge2.evidence.source_type = EvidenceSourceType::Lsp;
        edge2.evidence.confidence = Confidence::Exact;
        edge2.evidence.message = "lsp call".into();

        buffer.insert_edge(edge1);
        buffer.insert_edge(edge2);

        let (_, edges) = buffer.into_parts();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.source_type, EvidenceSourceType::Lsp);
        assert!(edges[0].evidence.message.contains("heuristic call"));
        assert!(edges[0].evidence.message.contains("lsp call"));
    }

    #[test]
    fn test_edge_dedupe_tie_breaker() {
        let mut edge1 = GraphEdge {
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        edge1.evidence.source_type = EvidenceSourceType::Lsp;
        edge1.evidence.confidence = Confidence::Exact;
        edge1.evidence.id = open_kioku_core::EvidenceId::new("evid_B");

        let mut edge2 = edge1.clone();
        edge2.evidence.id = open_kioku_core::EvidenceId::new("evid_A");

        let mut buffer = GraphBuffer::new();
        buffer.insert_edge(edge1.clone());
        buffer.insert_edge(edge2.clone());

        let (_, edges) = buffer.into_parts();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.id.0, "evid_A");

        let mut buffer2 = GraphBuffer::new();
        buffer2.insert_edge(edge2.clone());
        buffer2.insert_edge(edge1.clone());

        let (_, edges2) = buffer2.into_parts();
        assert_eq!(edges2.len(), 1);
        assert_eq!(edges2[0].evidence.id.0, "evid_A");

        assert_eq!(
            edges[0].id,
            identity::edge_id(
                GraphEdgeType::Calls,
                &NodeId::new("n1"),
                &NodeId::new("n2"),
                None
            )
        );
        assert_eq!(edges[0].id.0, edges2[0].id.0);
    }

    #[test]
    fn test_edge_dedupe_merges_structured_call_sites() {
        let mut edge1 = GraphEdge {
            from: NodeId::new("caller"),
            to: NodeId::new("callee"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        edge1.properties.insert(
            "call_sites".into(),
            serde_json::json!([{
                "path": "src/lib.rs",
                "start_line": 10,
                "start_column": 5,
                "end_line": 10,
                "end_column": 11
            }]),
        );
        let mut edge2 = edge1.clone();
        edge2.properties.insert(
            "call_sites".into(),
            serde_json::json!([{
                "path": "src/lib.rs",
                "start_line": 10,
                "start_column": 20,
                "end_line": 10,
                "end_column": 26
            }]),
        );

        let mut buffer = GraphBuffer::new();
        buffer.insert_edge(edge1);
        buffer.insert_edge(edge2);
        let (_, edges) = buffer.into_parts();

        assert_eq!(edges.len(), 1);
        let call_sites = edges[0]
            .properties
            .get("call_sites")
            .and_then(|value| value.as_array())
            .expect("merged Calls edge should retain call_sites");
        assert_eq!(call_sites.len(), 2);
        assert_eq!(call_sites[0]["start_column"], 5);
        assert_eq!(call_sites[1]["start_column"], 20);
    }

    #[test]
    fn test_deterministic_ordering() {
        let mut buffer = GraphBuffer::new();
        let node1 = GraphNode {
            id: NodeId::new("B"),
            label: "B".into(),
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("A"),
            label: "A".into(),
            ..Default::default()
        };
        let node3 = GraphNode {
            id: NodeId::new("C"),
            label: "C".into(),
            ..Default::default()
        };

        buffer.upsert_node(node1);
        buffer.upsert_node(node2);
        buffer.upsert_node(node3);

        let (nodes, _) = buffer.into_parts();
        assert_eq!(nodes[0].id.0, "A");
        assert_eq!(nodes[1].id.0, "B");
        assert_eq!(nodes[2].id.0, "C");
    }

    #[test]
    fn test_worker_merge() {
        let mut buffer1 = GraphBuffer::new();
        buffer1.upsert_node(GraphNode {
            id: NodeId::new("1"),
            ..Default::default()
        });
        buffer1.insert_edge(GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("1"),
            to: NodeId::new("1"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        });

        let mut buffer2 = GraphBuffer::new();
        buffer2.upsert_node(GraphNode {
            id: NodeId::new("1"),
            ..Default::default()
        });
        buffer2.insert_edge(GraphEdge {
            id: EdgeId::new("e2"),
            from: NodeId::new("1"),
            to: NodeId::new("1"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        });

        let report = buffer1.merge(buffer2);
        assert_eq!(report.nodes_merged, 1);
        assert_eq!(report.edges_merged, 1);
        assert_eq!(report.duplicates_collapsed, 2);

        let (nodes, edges) = buffer1.into_parts();
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
    }
}
