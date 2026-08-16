#[derive(Clone)]
pub struct CacheAuditRow {
    pub key: String,
    pub event: String,
    pub store_write: bool,
    pub eviction_metadata: Option<String>,
}

/// Read-only cache report. This does not execute get-or-load, mutate CacheStore, or emit traces.
pub struct CacheAuditReport {
    rows: Vec<CacheAuditRow>,
}

impl CacheAuditReport {
    pub fn new(rows: Vec<CacheAuditRow>) -> Self {
        Self { rows }
    }

    pub fn cache_miss_rows(&self) -> Vec<CacheAuditRow> {
        self.rows
            .iter()
            .filter(|row| row.event == "cache.miss")
            .cloned()
            .collect()
    }

    pub fn persisted_store_writes(&self) -> usize {
        self.rows.iter().filter(|row| row.store_write).count()
    }

    pub fn eviction_metadata_rows(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| row.eviction_metadata.clone())
            .collect()
    }

    pub fn render_get_or_load_summary(&self) -> String {
        self.rows
            .iter()
            .map(|row| format!("{}:{}:{}", row.key, row.event, row.store_write))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
