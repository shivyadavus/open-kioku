use open_kioku_core::{
    graph_edge_authority, graph_route_authorities, GraphEdge, RelationshipAuthority,
};
use open_kioku_errors::Result;
use open_kioku_storage::GraphStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The most hops `ok path` and MCP `dependency_path` follow between two nodes.
pub const DEPENDENCY_ROUTE_MAX_HOPS: usize = 12;

/// The caveat a route carries when a containment hop after a relationship hop is capped.
pub const ROUTE_CONTAINMENT_DESCENT_CAVEAT: &str = "this route descends through CONTAINS or DEFINES after a relationship hop; containment is not transitive across a relationship (a file importing the file that defines X does not establish a relation to X), so hop_route_authority caps that hop at corroborating and route_authority reflects it, while edge_authority still reports what each edge establishes on its own";

/// A route between two graph nodes with what it establishes: the answer `ok path` and MCP
/// `dependency_path` (with `to`) both give, built in one place so the two surfaces cannot report
/// the same edges with different authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyRoute {
    pub from: String,
    pub to: String,
    /// What each edge establishes on its own ([`graph_edge_authority`]), by edge id.
    pub edge_authority: BTreeMap<String, RelationshipAuthority>,
    pub edges: Vec<GraphEdge>,
    /// The lowest contribution any hop makes to the route ([`graph_route_authorities`]): a
    /// route is only as established as its weakest hop. Absent when there is no route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_authority: Option<RelationshipAuthority>,
    /// Each hop whose contribution to the route is below what its edge establishes on its own
    /// (containment after a relationship hop), by edge id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hop_route_authority: BTreeMap<String, RelationshipAuthority>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
}

impl DependencyRoute {
    /// Report `edges`, a forward route from `from` to `to`.
    pub fn from_edges(from: String, to: String, edges: Vec<GraphEdge>) -> Self {
        let route = graph_route_authorities(&edges);
        let edge_authority = edges
            .iter()
            .map(|edge| (edge.id.0.clone(), graph_edge_authority(edge)))
            .collect::<BTreeMap<_, _>>();
        let hop_route_authority = edges
            .iter()
            .zip(&route)
            .filter(|(edge, contribution)| edge_authority[&edge.id.0] != **contribution)
            .map(|(edge, contribution)| (edge.id.0.clone(), *contribution))
            .collect::<BTreeMap<_, _>>();
        let caveats = if hop_route_authority.is_empty() {
            Vec::new()
        } else {
            vec![ROUTE_CONTAINMENT_DESCENT_CAVEAT.to_string()]
        };
        Self {
            from,
            to,
            edge_authority,
            route_authority: route.into_iter().min(),
            edges,
            hop_route_authority,
            caveats,
        }
    }
}

/// The route between two resolved graph node ids ([`crate::resolve_graph_node`]): the shortest
/// one, and of those the one whose weakest hop is strongest ([`GraphStore::shortest_path`]).
pub fn dependency_route<S>(store: &S, from: String, to: String) -> Result<DependencyRoute>
where
    S: GraphStore + ?Sized,
{
    let edges = store.shortest_path(&from, &to, DEPENDENCY_ROUTE_MAX_HOPS)?;
    Ok(DependencyRoute::from_edges(from, to, edges))
}
