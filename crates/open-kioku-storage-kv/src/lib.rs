use open_kioku_core::{GraphEdge, GraphNode};
use open_kioku_errors::Result;
use open_kioku_graph::InMemoryGraph;

pub type KvGraphStore = InMemoryGraph;

pub fn build_graph(nodes: Vec<GraphNode>, edges: Vec<GraphEdge>) -> Result<KvGraphStore> {
    Ok(InMemoryGraph {
        nodes: nodes
            .into_iter()
            .map(|node| (node.id.0.clone(), node))
            .collect(),
        edges,
    })
}
