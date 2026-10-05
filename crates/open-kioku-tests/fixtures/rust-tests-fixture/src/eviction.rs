use crate::CacheEntry;

pub fn evict_coldest(entries: &mut Vec<CacheEntry>) {
    let coldest = entries
        .iter()
        .enumerate()
        .min_by_key(|(_, entry)| entry.hits)
        .map(|(index, _)| index);
    if let Some(index) = coldest {
        entries.remove(index);
    }
}
