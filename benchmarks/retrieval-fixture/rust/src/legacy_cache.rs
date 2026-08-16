use std::collections::BTreeMap;

/// Migration-only cache used to replay historical cache misses into an audit snapshot.
/// It intentionally shares cache, miss, store, put, load, and eviction vocabulary with live code.
#[derive(Default)]
pub struct LegacyCacheReplay {
    values: BTreeMap<String, String>,
    events: Vec<String>,
}

impl LegacyCacheReplay {
    pub fn replay_get_or_load(&mut self, key: &str, loaded: String) -> String {
        if let Some(value) = self.values.get(key) {
            return value.clone();
        }
        self.record_cache_miss(key);
        self.put_migrated_value(key.to_string(), loaded.clone());
        loaded
    }

    pub fn put_migrated_value(&mut self, key: String, value: String) {
        let previous = self.values.insert(key.clone(), value);
        if previous.is_some() {
            self.events.push(format!("eviction metadata replaced key={key}"));
        }
    }

    pub fn migration_store_value(&self, key: &str) -> Option<&String> {
        self.values.get(key)
    }

    fn record_cache_miss(&mut self, key: &str) {
        self.events
            .push(format!("cache.miss migration trace key={key}"));
    }

    pub fn render_store_audit(&self) -> String {
        self.events.join("\n")
    }
}
