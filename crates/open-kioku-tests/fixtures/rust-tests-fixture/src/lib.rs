pub const CACHE_LIMIT: usize = 64;

pub struct CacheEntry {
    pub key: String,
    pub hits: u32,
}

pub fn clamp_hits(hits: u32) -> u32 {
    hits.min(CACHE_LIMIT as u32)
}

#[cfg(test)]
mod tests {
    #[test]
    fn clamp_keeps_small_counts() {
        assert_eq!(super::clamp_hits(3), 3);
    }

    #[rstest]
    #[case(0, 0)]
    #[case(64, 64)]
    #[case(65, 64)]
    #[case(1000, 64)]
    fn clamp_bounds_each_case(#[case] hits: u32, #[case] expected: u32) {
        assert_eq!(super::clamp_hits(hits), expected);
    }
}
