use open_kioku_core::{
    graph_edge_authority, graph_route_authorities, GraphEdge, RelationshipAuthority, RouteSearch,
};
use open_kioku_errors::Result;
use open_kioku_storage::GraphStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The most hops `ok path` and MCP `dependency_path` follow between two nodes.
pub const DEPENDENCY_ROUTE_MAX_HOPS: usize = 12;

/// The caveat a route carries when a containment hop after a relationship hop is capped.
pub const ROUTE_CONTAINMENT_DESCENT_CAVEAT: &str = "this route descends through CONTAINS or DEFINES after a relationship hop; containment is not transitive across a relationship (a file importing the file that defines X does not establish a relation to X), so hop_route_authority caps that hop at corroborating and route_authority reflects it, while edge_authority still reports what each edge establishes on its own";

/// The caveat a route report carries when the search found no route within `max_hops` and
/// stopped there with nodes left to expand, so the empty route is not read as "not connected".
pub fn route_hop_limit_caveat(max_hops: usize) -> String {
    format!(
        "no route within {max_hops} hops: the search stopped at the {max_hops}-hop limit with nodes it had reached still unexpanded, so a longer route may exist; this empty route does not show that `from` and `to` are unconnected"
    )
}

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
    /// The hop limit the search stopped at, when it found no route and nodes it had reached at
    /// that depth were left unexpanded: a longer route may exist, and [`route_hop_limit_caveat`]
    /// says so in `caveats`. Absent when a route was found, and when the search read every node
    /// `from` reaches forward without meeting `to`, so no route of any length exists over the
    /// edges the walk follows: from source to target, and never `DERIVED_FROM`, `SIMILAR_TO` or
    /// `SEMANTICALLY_RELATED` ([`open_kioku_core::UNTYPED_WALK_EXCLUDED_EDGE_TYPES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at_hop_limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
}

impl DependencyRoute {
    /// Report what a search for a forward route from `from` to `to` of at most `max_hops` hops
    /// found: the route, or, when there is none and the search stopped at `max_hops`, that a
    /// longer one may exist.
    pub fn from_search(from: String, to: String, search: RouteSearch, max_hops: usize) -> Self {
        let RouteSearch {
            edges,
            stopped_at_hop_limit,
        } = search;
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
        let mut caveats = Vec::new();
        if !hop_route_authority.is_empty() {
            caveats.push(ROUTE_CONTAINMENT_DESCENT_CAVEAT.to_string());
        }
        // The walk sets this only when it found no route, so it never sits beside edges.
        let stopped_at_hop_limit = stopped_at_hop_limit.then_some(max_hops);
        if let Some(limit) = stopped_at_hop_limit {
            caveats.push(route_hop_limit_caveat(limit));
        }
        Self {
            from,
            to,
            edge_authority,
            route_authority: route.into_iter().min(),
            edges,
            hop_route_authority,
            stopped_at_hop_limit,
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
    let search = store.shortest_path(&from, &to, DEPENDENCY_ROUTE_MAX_HOPS)?;
    Ok(DependencyRoute::from_search(
        from,
        to,
        search,
        DEPENDENCY_ROUTE_MAX_HOPS,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryGraph;
    use open_kioku_core::{EdgeId, GraphEdgeType, NodeId};

    /// `n0` calls `n1`, which calls `n2`, and so on to `n{len}`; nothing calls `n0`.
    fn chain(len: usize) -> InMemoryGraph {
        InMemoryGraph {
            edges: (0..len)
                .map(|i| GraphEdge {
                    id: EdgeId::new(format!("n{i}-n{}", i + 1)),
                    from: NodeId::new(format!("n{i}")),
                    to: NodeId::new(format!("n{}", i + 1)),
                    edge_type: GraphEdgeType::Calls,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// An empty route at the hop limit reads differently from "not connected" (#666).
    #[test]
    fn a_route_beyond_the_hop_limit_names_the_limit() {
        let graph = chain(DEPENDENCY_ROUTE_MAX_HOPS + 1);
        let far = format!("n{}", DEPENDENCY_ROUTE_MAX_HOPS + 1);
        let route = dependency_route(&graph, "n0".into(), far).unwrap();
        assert!(route.edges.is_empty());
        assert_eq!(route.stopped_at_hop_limit, Some(DEPENDENCY_ROUTE_MAX_HOPS));
        assert_eq!(
            route.caveats,
            [route_hop_limit_caveat(DEPENDENCY_ROUTE_MAX_HOPS)]
        );
        assert!(route.caveats[0].contains("no route within 12 hops"));
        assert!(route.route_authority.is_none());
        let json = serde_json::to_value(&route).unwrap();
        assert_eq!(json["stopped_at_hop_limit"], 12, "{json}");

        // A route of exactly the limit is found, with no hop-limit caveat.
        let near = format!("n{DEPENDENCY_ROUTE_MAX_HOPS}");
        let route = dependency_route(&graph, "n0".into(), near).unwrap();
        assert_eq!(route.edges.len(), DEPENDENCY_ROUTE_MAX_HOPS);
        assert_eq!(route.stopped_at_hop_limit, None);
        assert!(route.caveats.is_empty(), "{:?}", route.caveats);
    }

    /// A pair the walk proves unconnected (it read everything `from` reaches) carries no
    /// hop-limit field or caveat, and neither appears in its JSON.
    #[test]
    fn an_unconnected_pair_carries_no_hop_limit_caveat() {
        let graph = chain(DEPENDENCY_ROUTE_MAX_HOPS + 1);
        let route = dependency_route(&graph, "n3".into(), "n0".into()).unwrap();
        assert!(route.edges.is_empty());
        assert_eq!(route.stopped_at_hop_limit, None);
        assert!(route.caveats.is_empty(), "{:?}", route.caveats);
        let json = serde_json::to_value(&route).unwrap();
        assert!(json.get("stopped_at_hop_limit").is_none(), "{json}");
        assert!(json.get("caveats").is_none(), "{json}");
    }
}
