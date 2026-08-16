use super::store::CacheStore;
use super::trace::record_cache_miss;

pub struct CacheService { pub store: CacheStore }

impl CacheService {
    /// Get-or-load path: a cache miss is traced and the loaded value is written to CacheStore.
    pub fn get_or_load(&mut self, key: &str, loader: impl FnOnce() -> String) -> String {
        if let Some(value) = self.store.get(key) { return value.clone(); }
        record_cache_miss(key);
        let value = loader();
        self.store.put(key.to_string(), value.clone());
        value
    }
}
