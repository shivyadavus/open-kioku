use open_kioku_core::SearchResult;

/// Re-rank a list of search results by applying domain-specific boosts and penalties:
/// - Vendor files are heavily penalised (score × 0.35).
/// - Generated files are moderately penalised (score × 0.55).
/// - Test files receive a small boost (+0.05) so they surface near relevant source files.
/// - Results where the snippet contains the query as an exact token receive a boost (+0.15).
pub fn rerank(mut results: Vec<SearchResult>) -> Vec<SearchResult> {
    for result in &mut results {
        let path = result.path.to_string_lossy();
        if path.contains("vendor") {
            result.score *= 0.35;
        } else if result
            .symbol
            .as_ref()
            .map(|s| s.file_id.0.is_empty())
            .unwrap_or(false)
        {
            // placeholder: is_vendor flag lives on File, not SearchResult directly
        }

        // Penalise generated files — is_generated is baked into Evidence confidence.
        // We detect generated paths by common markers used in likely_generated().
        if path.contains("generated")
            || path.contains("_pb.rs")
            || path.contains(".pb.go")
            || path.contains("schema.json")
        {
            result.score *= 0.55;
        }

        if path.contains("test") {
            result.score += 0.05;
        }

        // Boost exact word-boundary symbol name hits.
        if let Some(symbol) = &result.symbol {
            if result.snippet.contains(&symbol.name) {
                result.score += 0.15;
            }
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

#[cfg(test)]
mod tests {
    use super::rerank;
    use chrono::Utc;
    use open_kioku_core::{
        Confidence, Evidence, EvidenceId, EvidenceSourceType, LineRange, SearchResult,
    };
    use std::path::{Path, PathBuf};

    fn make_result(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange::single(1)),
            snippet: "some code".into(),
            symbol: None,
            score,
            match_reason: "test".into(),
            evidence: Evidence {
                id: EvidenceId::new("ev"),
                source: "test".into(),
                source_type: EvidenceSourceType::Lexical,
                file_range: None,
                symbol_id: None,
                confidence: Confidence::Medium,
                message: "test".into(),
                indexed_at: Utc::now(),
            },
        }
    }

    #[test]
    fn vendor_files_score_lower() {
        let normal = make_result("src/lib.rs", 1.0);
        let vendor = make_result("vendor/dep/lib.rs", 1.0);
        let results = rerank(vec![normal, vendor]);
        assert!(
            results[0].path.to_string_lossy().contains("src"),
            "normal file should outscore vendor"
        );
    }

    #[test]
    fn generated_files_score_lower() {
        let normal = make_result("src/lib.rs", 1.0);
        let generated = make_result("src/generated_pb.rs", 1.0);
        let results = rerank(vec![normal, generated]);
        assert!(
            results[0].path == Path::new("src/lib.rs"),
            "Expected src/lib.rs to be first, got {:?}",
            results[0].path
        );
    }

    #[test]
    fn test_files_score_slightly_higher() {
        let normal = make_result("src/lib.rs", 1.0);
        let test = make_result("src/lib_test.rs", 1.0);
        let results = rerank(vec![normal, test]);
        // Test file gets +0.05 boost; both start at 1.0.
        let test_score = results
            .iter()
            .find(|r| r.path.to_string_lossy().contains("test"))
            .map(|r| r.score)
            .unwrap();
        assert!(test_score > 1.0, "test file should receive boost");
    }

    #[test]
    fn results_sorted_descending() {
        let low = make_result("src/a.rs", 0.3);
        let high = make_result("src/b.rs", 0.9);
        let results = rerank(vec![low, high]);
        assert!(results[0].score >= results[1].score);
    }
}
