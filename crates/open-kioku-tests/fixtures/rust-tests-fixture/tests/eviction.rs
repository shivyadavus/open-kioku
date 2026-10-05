mod common;

use rust_tests_fixture::eviction::evict_coldest;
use rust_tests_fixture::CacheEntry;

fn entry_with_hits(key: &str, hits: u32) -> CacheEntry {
    CacheEntry {
        key: key.to_string(),
        hits,
    }
}

#[test]
fn evict_coldest_drops_the_least_hit_entry() {
    let mut entries = common::seeded_entries();
    entries.push(entry_with_hits("cold", 0));
    evict_coldest(&mut entries);
    assert!(entries.iter().all(|entry| entry.key != "cold"));
}
