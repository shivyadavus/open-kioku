use rust_tests_fixture::CacheEntry;

pub fn seeded_entries() -> Vec<CacheEntry> {
    vec![CacheEntry {
        key: "warm".to_string(),
        hits: 5,
    }]
}
