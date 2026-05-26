use open_kioku_core::{
    CodeChunk, File, FileId, GraphEdge, GraphNode, Import, IndexManifest, SearchResult, Symbol,
    SymbolId, SymbolOccurrence, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(|err| OkError::Storage(err.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.initialize()?;
        Ok(store)
    }
}

fn storage_err(err: rusqlite::Error) -> OkError {
    OkError::Storage(err.to_string())
}

impl MetadataStore for SqliteStore {
    fn initialize(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.execute_batch(include_str!("schema.sql"))
            .map_err(storage_err)
    }

    fn put_manifest(&self, manifest: &IndexManifest) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let json =
            serde_json::to_string(manifest).map_err(|err| OkError::Storage(err.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO manifest (id, data) VALUES (1, ?1)",
            params![json],
        )
        .map_err(storage_err)?;
        Ok(())
    }

    fn manifest(&self) -> Result<Option<IndexManifest>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let result = conn.query_row("SELECT data FROM manifest WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        });
        match result {
            Ok(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))?,
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(storage_err(err)),
        }
    }

    fn replace_index(&self, data: IndexData<'_>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.execute_batch("DELETE FROM files; DELETE FROM symbols; DELETE FROM chunks; DELETE FROM tests; DELETE FROM imports; DELETE FROM occurrences;").map_err(storage_err)?;
        for file in data.files {
            let json =
                serde_json::to_string(file).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO files (id, data) VALUES (?1, ?2)",
                params![file.id.0, json],
            )
            .map_err(storage_err)?;
        }
        for symbol in data.symbols {
            let json =
                serde_json::to_string(symbol).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO symbols (id, name, data) VALUES (?1, ?2, ?3)",
                params![symbol.id.0, symbol.name, json],
            )
            .map_err(storage_err)?;
        }
        for chunk in data.chunks {
            let json =
                serde_json::to_string(chunk).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO chunks (id, file_id, data) VALUES (?1, ?2, ?3)",
                params![chunk.id, chunk.file_id.0, json],
            )
            .map_err(storage_err)?;
        }
        for test in data.tests {
            let json =
                serde_json::to_string(test).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO tests (id, data) VALUES (?1, ?2)",
                params![test.id.0, json],
            )
            .map_err(storage_err)?;
        }
        for import in data.imports {
            let json =
                serde_json::to_string(import).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO imports (id, data) VALUES (?1, ?2)",
                params![import.id.0, json],
            )
            .map_err(storage_err)?;
        }
        for occ in data.occurrences {
            let json =
                serde_json::to_string(occ).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO occurrences (id, data) VALUES (?1, ?2)",
                params![occ.id.0, json],
            )
            .map_err(storage_err)?;
        }
        self.put_manifest(data.manifest)
    }

    fn list_files(&self, limit: usize, offset: usize) -> Result<Vec<File>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM files LIMIT ?1 OFFSET ?2")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn get_file_by_path(&self, path: &std::path::Path) -> Result<Option<File>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let path_str = path.display().to_string();
        let result = conn.query_row(
            "SELECT data FROM files WHERE json_extract(data, '$.path') = ?1",
            params![path_str],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))?,
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(storage_err(err)),
        }
    }

    fn list_symbols(
        &self,
        query: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Symbol>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = if let Some(q) = query {
            let pattern = format!("%{}%", q);
            let mut s = conn
                .prepare("SELECT data FROM symbols WHERE name LIKE ?1 LIMIT ?2 OFFSET ?3")
                .map_err(storage_err)?;
            let rows = s
                .query_map(params![pattern, limit as i64, offset as i64], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(storage_err)?;
            return rows
                .map(|r| {
                    r.map_err(storage_err).and_then(|json| {
                        serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
                    })
                })
                .collect();
        } else {
            conn.prepare("SELECT data FROM symbols LIMIT ?1 OFFSET ?2")
                .map_err(storage_err)?
        };
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn symbol_by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let result = conn.query_row(
            "SELECT data FROM symbols WHERE id = ?1",
            params![id.0],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))?,
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(storage_err(err)),
        }
    }

    fn chunks_for_file(&self, file_id: &FileId) -> Result<Vec<CodeChunk>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM chunks WHERE file_id = ?1")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![file_id.0], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM chunks")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn tests(&self) -> Result<Vec<TestTarget>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM tests")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn imports(&self) -> Result<Vec<Import>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM imports")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM occurrences WHERE json_extract(data, '$.symbol_id') = ?1 LIMIT ?2")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![id.0, limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }

    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM occurrences WHERE json_extract(data, '$.file_id') = ?1")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![file_id.0], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        rows.map(|r| {
            r.map_err(storage_err).and_then(|json| {
                serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
            })
        })
        .collect()
    }
}

impl GraphStore for SqliteStore {
    fn replace_graph(&self, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.execute_batch("DELETE FROM graph_nodes; DELETE FROM graph_edges;")
            .map_err(storage_err)?;
        for node in nodes {
            let json =
                serde_json::to_string(node).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO graph_nodes (id, data) VALUES (?1, ?2)",
                params![node.id.0, json],
            )
            .map_err(storage_err)?;
        }
        for edge in edges {
            let json =
                serde_json::to_string(edge).map_err(|err| OkError::Storage(err.to_string()))?;
            conn.execute("INSERT OR REPLACE INTO graph_edges (id, from_id, to_id, data) VALUES (?1, ?2, ?3, ?4)", params![edge.id.0, edge.from.0, edge.to.0, json]).map_err(storage_err)?;
        }
        Ok(())
    }

    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM graph_edges WHERE from_id = ?1 OR to_id = ?1 LIMIT ?2")
            .map_err(storage_err)?;
        let edges: Vec<GraphEdge> = stmt
            .query_map(params![node, limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?
            .map(|r| {
                r.map_err(storage_err).and_then(|json| {
                    serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
                })
            })
            .collect::<Result<_>>()?;
        let node_ids: std::collections::HashSet<_> = edges
            .iter()
            .flat_map(|e| [e.from.0.clone(), e.to.0.clone()])
            .collect();
        let mut nodes = Vec::new();
        for node_id in node_ids {
            let result = conn.query_row(
                "SELECT data FROM graph_nodes WHERE id = ?1",
                params![node_id],
                |row| row.get::<_, String>(0),
            );
            match result {
                Ok(json) => nodes.push(
                    serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))?,
                ),
                Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(err) => return Err(storage_err(err)),
            }
        }
        Ok((nodes, edges))
    }

    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT data FROM graph_edges WHERE from_id = ?1 OR to_id = ?2 LIMIT ?3")
            .map_err(storage_err)?;
        let edges: Vec<GraphEdge> = stmt
            .query_map(params![from, to, max_depth as i64 * 2], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?
            .map(|r| {
                r.map_err(storage_err).and_then(|json| {
                    serde_json::from_str(&json).map_err(|err| OkError::Storage(err.to_string()))
                })
            })
            .collect::<Result<_>>()?;
        Ok(edges)
    }
}

impl open_kioku_storage::OkStore for SqliteStore {}
