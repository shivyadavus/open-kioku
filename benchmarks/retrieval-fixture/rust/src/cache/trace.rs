/// TODO: attach cache.miss trace attributes before the store write in get_or_load.
pub fn record_cache_miss(key: &str) { eprintln!("cache.miss key={key}"); }
