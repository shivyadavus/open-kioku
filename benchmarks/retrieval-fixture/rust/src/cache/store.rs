use std::collections::HashMap;

#[derive(Default)]
pub struct CacheStore { values: HashMap<String, String> }

impl CacheStore {
    pub fn get(&self, key: &str) -> Option<&String> { self.values.get(key) }

    /// Writes a cache value. A future contract change may return eviction metadata.
    pub fn put(&mut self, key: String, value: String) { self.values.insert(key, value); }
}
