use open_kioku_errors::{OkError, Result};
use open_kioku_storage::{GraphStore, MetadataStore};
use std::path::Path;

/// Resolve a path, symbol name, or explicit `file:`/`symbol:` node id to a graph node id.
///
/// `ok path` and MCP `dependency_path` both resolve through here, so the two surfaces cannot
/// disagree about which node a name means. A name that resolves to nothing is an error about
/// index content (`OkError::Index`), as a symbol lookup that finds nothing is, not about the
/// caller's arguments: passing it through produced an empty edge list that read as "these two
/// are unconnected" when the truth was "this is not in the index". A store failure is returned
/// as the store reported it.
pub fn resolve_graph_node<S>(store: &S, query: &str) -> Result<String>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    if query.starts_with("file:") || query.starts_with("symbol:") {
        return match store.node_by_id(query)? {
            Some(_) => Ok(query.to_string()),
            None => Err(OkError::Index(format!(
                "`{query}` is not a node in the indexed dependency graph; it may be excluded, unsupported, or added since the last `ok index`"
            ))),
        };
    }
    if let Some(file) = store.get_file_by_path(Path::new(query))? {
        return Ok(format!("file:{}", file.path.display()));
    }
    if let Some(symbol) = store
        .list_symbols(Some(query), 10, 0)?
        .into_iter()
        .find(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
    {
        return Ok(format!("symbol:{}", symbol.id.0));
    }
    Err(OkError::Index(format!(
        "`{query}` is not an indexed file path or symbol name; it may be excluded, unsupported, or added since the last `ok index`"
    )))
}
