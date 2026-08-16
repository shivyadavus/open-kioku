use super::service::CacheService;
use super::store::CacheStore;

#[test]
fn get_or_load_writes_misses_to_store() {
    let mut service = CacheService { store: CacheStore::default() };
    assert_eq!(service.get_or_load("a", || "value".into()), "value");
    assert_eq!(service.store.get("a").map(String::as_str), Some("value"));
}
