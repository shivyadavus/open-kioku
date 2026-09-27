use open_kioku_core::{
    identity, EdgeId, Evidence, EvidenceSourceType, GraphEdge, GraphEdgeType, GraphNode, NodeId,
    RELATIONSHIP_PROOFS_PROPERTY,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Default, Debug, Clone)]
pub struct GraphBufferMergeReport {
    pub nodes_merged: usize,
    pub edges_merged: usize,
    pub duplicates_collapsed: usize,
}

#[derive(Default)]
pub struct GraphBuffer {
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeSlot>,
    node_by_id: HashMap<NodeId, usize>,
    node_by_key: HashMap<String, NodeId>,
    edges_by_key: HashMap<(NodeId, NodeId, GraphEdgeType), usize>,
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

/// Whether `a` is a better representative evidence for an edge than `b`: the higher rank, then
/// the earliest site (path, then line range; evidence with a site before evidence without one),
/// then the smaller source and evidence id. A total order, so the representative does not depend
/// on the order the writes arrive in, and one the evidence id decides only between writes at the
/// same site: the id hashes the edge's target node, so choosing by it showed an arbitrary call
/// site that moved whenever node identity changed (#597).
fn evidence_precedes(a: &Evidence, b: &Evidence) -> bool {
    let (a_source, a_confidence) = evidence_rank(a);
    let (b_source, b_confidence) = evidence_rank(b);
    fn site(e: &Evidence) -> Option<(&std::ffi::OsStr, Option<(u32, u32)>)> {
        e.file_range.as_ref().map(|range| {
            (
                range.path.as_path().as_os_str(),
                range
                    .line_range
                    .as_ref()
                    .map(|lines| (lines.start, lines.end)),
            )
        })
    }
    let (a_site, b_site) = (site(a), site(b));
    b_source
        .cmp(&a_source)
        .then_with(|| b_confidence.total_cmp(&a_confidence))
        .then_with(|| a_site.is_none().cmp(&b_site.is_none()))
        .then_with(|| a_site.cmp(&b_site))
        .then_with(|| a.source.cmp(&b.source))
        .then_with(|| a.id.0.cmp(&b.id.0))
        == Ordering::Less
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
}

/// Properties merged as unions of every write rather than taken from one.
fn is_union_property(key: &str) -> bool {
    key == "call_sites" || key == "reference_sites" || key == RELATIONSHIP_PROOFS_PROPERTY
}

/// The writes of one edge folded so far. `edge` is the representative write, holding its own
/// scalar fields and every write's unions; `fallbacks` joins every write's scalar fields, for the
/// ones the representative leaves unset. Both halves are joins, so the edge a slot yields does not
/// depend on the order writes, or other buffers' slots, are folded into it.
struct EdgeSlot {
    edge: GraphEdge,
    /// `None` while the slot holds one write, whose own fields are its fallbacks; most edges
    /// never collide, so they carry no copy.
    fallbacks: Option<Box<EdgeFallbacks>>,
}

#[derive(Default)]
struct EdgeFallbacks {
    properties: BTreeMap<String, serde_json::Value>,
    schema_version: Option<String>,
    source_pass: Option<String>,
    index_mode: Option<String>,
    extractor_version: Option<String>,
}

impl EdgeFallbacks {
    fn of(edge: &GraphEdge) -> Self {
        Self {
            properties: edge
                .properties
                .iter()
                .filter(|(key, _)| !is_union_property(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            schema_version: edge.schema_version.clone(),
            source_pass: edge.source_pass.clone(),
            index_mode: edge.index_mode.clone(),
            extractor_version: edge.extractor_version.clone(),
        }
    }

    /// The smallest present value of each field, as `merge_node` joins a node's.
    fn join(&mut self, other: Self) {
        for (key, value) in other.properties {
            join_property(&mut self.properties, key, value);
        }
        join_min(&mut self.schema_version, other.schema_version);
        join_min(&mut self.source_pass, other.source_pass);
        join_min(&mut self.index_mode, other.index_mode);
        join_min(&mut self.extractor_version, other.extractor_version);
    }
}

impl EdgeSlot {
    fn fallbacks(&mut self) -> EdgeFallbacks {
        match self.fallbacks.take() {
            Some(fallbacks) => *fallbacks,
            None => EdgeFallbacks::of(&self.edge),
        }
    }

    fn fold(mut self, mut incoming: EdgeSlot) -> EdgeSlot {
        let mut fallbacks = self.fallbacks();
        fallbacks.join(incoming.fallbacks());
        let (mut winner, loser) = if evidence_precedes(&incoming.edge.evidence, &self.edge.evidence)
        {
            (incoming.edge, self.edge)
        } else {
            (self.edge, incoming.edge)
        };
        winner.evidence.message =
            merge_messages(&winner.evidence.message, &loser.evidence.message).into();
        merge_edge_metadata(&mut winner, loser);
        EdgeSlot {
            edge: winner,
            fallbacks: Some(Box::new(fallbacks)),
        }
    }

    fn into_edge(self) -> GraphEdge {
        let mut edge = self.edge;
        if let Some(fallbacks) = self.fallbacks {
            let fallbacks = *fallbacks;
            for (key, value) in fallbacks.properties {
                edge.properties.entry(key).or_insert(value);
            }
            edge.schema_version = edge.schema_version.or(fallbacks.schema_version);
            edge.source_pass = edge.source_pass.or(fallbacks.source_pass);
            edge.index_mode = edge.index_mode.or(fallbacks.index_mode);
            edge.extractor_version = edge.extractor_version.or(fallbacks.extractor_version);
        }
        edge
    }
}

/// Fold `incoming` into `existing`, a node of the same id, so that the result does not
/// depend on the order the writes arrive in: every field is a join (the smallest present
/// value, a sorted union), which is commutative and associative. A full build and an
/// incremental one visit the same writes in different orders, and a node whose content
/// followed its first write (a file node's `source_pass` naming whichever co-change commit
/// was read first) came out different in each (#591).
fn merge_node(existing: &mut GraphNode, incoming: GraphNode) {
    // The write that carries the file or symbol a node stands for names it; an analysis fact
    // that only points at the node (an import target, a co-changed file, whose spelling of a
    // path normalizes to the same id) names it only when no such write does. This stays a
    // join as long as a write carrying a file or symbol id carries a label, which every such
    // write the graph builder makes does.
    let label_key = |node: &GraphNode| {
        (
            !node.label.is_empty(),
            node.file_id.is_some() || node.symbol_id.is_some(),
        )
    };
    let (existing_key, incoming_key) = (label_key(existing), label_key(&incoming));
    if incoming_key > existing_key
        || (incoming_key == existing_key && incoming.label < existing.label)
    {
        existing.label = incoming.label;
    }
    join_min(&mut existing.file_id, incoming.file_id);
    join_min(&mut existing.symbol_id, incoming.symbol_id);
    join_min(&mut existing.schema_version, incoming.schema_version);
    join_min(&mut existing.source_pass, incoming.source_pass);
    join_min(&mut existing.index_mode, incoming.index_mode);
    join_min(&mut existing.extractor_version, incoming.extractor_version);
    for (key, value) in incoming.properties {
        join_property(&mut existing.properties, key, value);
    }
    join_sorted(&mut existing.ambiguity, incoming.ambiguity);
    join_sorted(&mut existing.quality_notes, incoming.quality_notes);
}

/// The smaller of two values of a property; a present value over an absent one.
fn join_property(
    properties: &mut BTreeMap<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    match properties.entry(key) {
        std::collections::btree_map::Entry::Vacant(slot) => {
            slot.insert(value);
        }
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            // `Value` has no order; its serialized form stands in for one, and is built only
            // when two writes disagree.
            if *slot.get() != value {
                let (incoming, stored) = (value.to_string(), slot.get().to_string());
                if incoming < stored {
                    slot.insert(value);
                }
            }
        }
    }
}

/// The smaller of two present values; a present value over an absent one.
fn join_min<T: Ord>(existing: &mut Option<T>, incoming: Option<T>) {
    if let Some(incoming) = incoming {
        if existing.as_ref().is_none_or(|current| incoming < *current) {
            *existing = Some(incoming);
        }
    }
}

fn join_sorted(existing: &mut Vec<String>, incoming: Vec<String>) {
    if incoming.is_empty() {
        return;
    }
    existing.extend(incoming);
    existing.sort();
    existing.dedup();
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
            merge_node(existing, node);
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
        edge.id = identity::edge_id(edge.edge_type.clone(), &edge.from, &edge.to, None);
        self.insert_slot(EdgeSlot {
            edge,
            fallbacks: None,
        })
    }

    fn insert_slot(&mut self, incoming: EdgeSlot) -> EdgeId {
        let key = (
            incoming.edge.from.clone(),
            incoming.edge.to.clone(),
            incoming.edge.edge_type.clone(),
        );
        if let Some(&index) = self.edges_by_key.get(&key) {
            let placeholder = EdgeSlot {
                edge: GraphEdge::default(),
                fallbacks: None,
            };
            let existing = std::mem::replace(&mut self.edges[index], placeholder);
            self.edges[index] = existing.fold(incoming);
            self.edges[index].edge.id.clone()
        } else {
            let index = self.edges.len();
            self.edges_by_key.insert(key, index);
            let id = incoming.edge.id.clone();
            self.edges.push(incoming);
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

        for slot in other.edges {
            self.insert_slot(slot);
            report.edges_merged += 1;
        }
        let after_edges = self.edges.len();
        report.duplicates_collapsed += report.edges_merged - (after_edges - initial_edges);

        report
    }

    pub fn into_parts(mut self) -> (Vec<GraphNode>, Vec<GraphEdge>) {
        self.nodes.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        let mut edges: Vec<GraphEdge> = self.edges.into_iter().map(EdgeSlot::into_edge).collect();
        edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        (self.nodes, edges)
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

    /// #591: a file node written by the file itself and by analysis facts that point at it
    /// (co-changes naming different commits, an import resolved to it) must come out the same
    /// whatever order the writes arrive in. Every field used to follow the first write that
    /// set it, or the last for properties, so a full and an incremental build disagreed.
    #[test]
    fn upsert_node_merges_the_same_node_whatever_the_write_order() {
        let file_id = open_kioku_core::FileId::new("f1");
        let id = NodeId::new("file:src/lib.rs");
        let from_file = GraphNode {
            id: id.clone(),
            node_type: GraphNodeType::File,
            label: "src/lib.rs".into(),
            file_id: Some(file_id.clone()),
            ..Default::default()
        };
        let fact = |label: &str, source: &str, ambiguity: &[&str], property: i64| GraphNode {
            id: id.clone(),
            node_type: GraphNodeType::File,
            label: label.into(),
            source_pass: Some(source.into()),
            ambiguity: ambiguity.iter().map(|note| note.to_string()).collect(),
            properties: [("weight".to_string(), serde_json::json!(property))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let writes = [
            from_file,
            fact("./src/lib.rs", "git-history:bbbb", &["late"], 2),
            fact("src/lib.rs", "git-history:aaaa", &["early", "late"], 3),
            fact("src/lib.rs", "open-kioku-import-resolver/relative", &[], 1),
        ];
        let merged = |order: &[usize]| {
            let mut buffer = GraphBuffer::new();
            for &index in order {
                buffer.upsert_node(writes[index].clone());
            }
            let (nodes, _) = buffer.into_parts();
            assert_eq!(nodes.len(), 1, "{order:?}");
            nodes.into_iter().next().unwrap()
        };
        let expected = merged(&[0, 1, 2, 3]);
        assert_eq!(expected.label, "src/lib.rs");
        assert_eq!(expected.file_id, Some(file_id));
        assert_eq!(expected.source_pass.as_deref(), Some("git-history:aaaa"));
        assert_eq!(expected.ambiguity, vec!["early", "late"]);
        assert_eq!(expected.properties["weight"], serde_json::json!(1));
        for order in [
            [3, 2, 1, 0],
            [1, 0, 3, 2],
            [2, 3, 0, 1],
            [1, 2, 3, 0],
            [3, 1, 0, 2],
        ] {
            assert_eq!(
                serde_json::to_value(merged(&order)).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "write order {order:?}"
            );
        }
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

    fn call_site_write(path: &str, line: u32, evidence_id: &str, source: &str) -> GraphEdge {
        let mut edge = GraphEdge {
            from: NodeId::new("caller"),
            to: NodeId::new("callee"),
            edge_type: GraphEdgeType::Calls,
            source_pass: Some(source.into()),
            properties: [
                ("source_pass".to_string(), serde_json::json!(source)),
                ("confidence".to_string(), serde_json::json!("High")),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        edge.evidence.id = open_kioku_core::EvidenceId::new(evidence_id);
        edge.evidence.source = source.into();
        edge.evidence.source_type = EvidenceSourceType::StaticAnalysis;
        edge.evidence.confidence = Confidence::High;
        edge.evidence.message = format!("call at {path}:{line}").into();
        edge.evidence.file_range = Some(open_kioku_core::FileRange {
            path: std::path::Path::new(path).into(),
            line_range: Some(open_kioku_core::LineRange::single(line)),
        });
        edge
    }

    fn evidence_line(edge: &GraphEdge) -> (String, u32) {
        let range = edge.evidence.file_range.as_ref().expect("evidence site");
        (
            range.path.to_string_lossy().into_owned(),
            range.line_range.as_ref().expect("evidence lines").start,
        )
    }

    /// #597: writes of one edge with equal rank used to keep the evidence with the smallest id,
    /// a hash of the edge's target node, so the line shown was an arbitrary call site. The
    /// earliest site must be shown, and the edge must come out the same whatever order the
    /// writes, or the worker buffers holding them, are folded in.
    #[test]
    fn equal_rank_edge_writes_show_the_earliest_site_whatever_the_write_order() {
        // The smallest evidence id belongs to the latest site, and the earliest site's write
        // is neither first nor last in the base order.
        let writes = [
            call_site_write("src/b.rs", 3, "evid_A", "pass-b"),
            call_site_write("src/a.rs", 40, "evid_C", "pass-c"),
            call_site_write("src/a.rs", 12, "evid_D", "pass-a"),
            call_site_write("src/a.rs", 25, "evid_B", "pass-a"),
        ];
        let orders = [
            [0, 1, 2, 3],
            [3, 2, 1, 0],
            [0, 3, 1, 2],
            [2, 0, 3, 1],
            [1, 3, 0, 2],
            [3, 0, 2, 1],
        ];
        let folded = |order: &[usize]| {
            let mut buffer = GraphBuffer::new();
            for &index in order {
                buffer.insert_edge(writes[index].clone());
            }
            let (_, edges) = buffer.into_parts();
            assert_eq!(edges.len(), 1, "{order:?}");
            edges.into_iter().next().unwrap()
        };
        let expected = folded(&orders[0]);
        assert_eq!(evidence_line(&expected), ("src/a.rs".to_string(), 12));
        assert_eq!(expected.evidence.id.0, "evid_D");
        assert_eq!(expected.source_pass.as_deref(), Some("pass-a"));
        assert_eq!(
            expected.properties["source_pass"],
            serde_json::json!("pass-a")
        );
        let sites = expected.properties["call_sites"]
            .as_array()
            .expect("every site stays listed")
            .iter()
            .map(|site| {
                (
                    site["path"].as_str().unwrap().to_string(),
                    site["start_line"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sites,
            vec![
                ("src/a.rs".to_string(), 12),
                ("src/a.rs".to_string(), 25),
                ("src/a.rs".to_string(), 40),
                ("src/b.rs".to_string(), 3),
            ]
        );
        let expected = serde_json::to_value(&expected).unwrap();
        for order in &orders[1..] {
            assert_eq!(
                serde_json::to_value(folded(order)).unwrap(),
                expected,
                "write order {order:?}"
            );
        }
        // Worker buffers fold slots that already hold several writes.
        for (left, right) in [([0, 1], [2, 3]), ([2, 3], [0, 1]), ([3, 0], [1, 2])] {
            let mut first = GraphBuffer::new();
            let mut second = GraphBuffer::new();
            for index in left {
                first.insert_edge(writes[index].clone());
            }
            for index in right {
                second.insert_edge(writes[index].clone());
            }
            first.merge(second);
            let (_, edges) = first.into_parts();
            assert_eq!(
                serde_json::to_value(&edges[0]).unwrap(),
                expected,
                "buffers {left:?} + {right:?}"
            );
        }
    }

    /// Rank still decides before the site: an exact write at a later line is the
    /// representative over a heuristic one at an earlier line, and a field the representative
    /// leaves unset is the smallest any write holds, not the first one folded.
    #[test]
    fn a_higher_ranked_write_stays_representative_over_an_earlier_site() {
        let mut exact = call_site_write("src/a.rs", 30, "evid_Z", "scip");
        exact.evidence.source_type = EvidenceSourceType::Scip;
        exact.evidence.confidence = Confidence::Exact;
        exact.source_pass = None;
        exact.properties.clear();
        let mut heuristic_late = call_site_write("src/a.rs", 20, "evid_B", "pass-z");
        heuristic_late.evidence.source_type = EvidenceSourceType::Heuristic;
        let mut heuristic_early = call_site_write("src/a.rs", 10, "evid_A", "pass-m");
        heuristic_early.evidence.source_type = EvidenceSourceType::Heuristic;
        let writes = [exact, heuristic_late, heuristic_early];
        let mut results = Vec::new();
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
            let mut buffer = GraphBuffer::new();
            for index in order {
                buffer.insert_edge(writes[index].clone());
            }
            let (_, edges) = buffer.into_parts();
            let edge = edges.into_iter().next().unwrap();
            assert_eq!(
                edge.evidence.source_type,
                EvidenceSourceType::Scip,
                "{order:?}"
            );
            assert_eq!(evidence_line(&edge), ("src/a.rs".to_string(), 30));
            assert_eq!(edge.source_pass.as_deref(), Some("pass-m"), "{order:?}");
            assert_eq!(edge.properties["source_pass"], serde_json::json!("pass-m"));
            results.push(serde_json::to_value(edge).unwrap());
        }
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
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
