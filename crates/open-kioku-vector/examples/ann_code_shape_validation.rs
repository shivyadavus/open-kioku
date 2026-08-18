use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, HnswParameters, UsearchHnswVectorIndex, VectorHit,
    VectorId, VectorRecord, VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS,
};
use serde::Serialize;
use std::collections::HashSet;
use std::time::Instant;

const MAX_K: usize = 20;
const DEFAULT_SIZES: &[usize] = &[50_000, 100_000, 300_000, 1_000_000];
const DEFAULT_DIMS: &[usize] = &[384, 768];
const DEFAULT_SEARCH_EXPANSIONS: &[usize] = &[64, 128, 256, 512, 1_024];
const DEFAULT_QUERY_COUNT: usize = 64;

#[derive(Debug, Clone, Serialize)]
struct QualityMetrics {
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
    recall_at_20: f64,
    mrr: f64,
}

#[derive(Debug, Clone, Serialize)]
struct LatencyMetrics {
    mean_us: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
}

#[derive(Debug, Clone, Serialize)]
struct Measurement {
    vector_count: usize,
    dimensions: usize,
    query_count: usize,
    parameters: HnswParameters,
    quality: QualityMetrics,
    exact_build_ms: f64,
    ann_build_ms: f64,
    ann_vectors_per_second: f64,
    exact_query: LatencyMetrics,
    ann_query: LatencyMetrics,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    benchmark: &'static str,
    backend: &'static str,
    oracle: &'static str,
    distribution: &'static str,
    distribution_contract: &'static str,
    requested_sizes: Vec<usize>,
    requested_dimensions: Vec<usize>,
    requested_search_expansions: Vec<usize>,
    measurements: Vec<Measurement>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = parse_list("OK_ANN_CODE_SHAPE_SIZES", DEFAULT_SIZES)?;
    let dimensions = parse_list("OK_ANN_CODE_SHAPE_DIMS", DEFAULT_DIMS)?;
    let search_expansions = parse_list(
        "OK_ANN_CODE_SHAPE_SEARCH_EXPANSIONS",
        DEFAULT_SEARCH_EXPANSIONS,
    )?;
    let query_count = parse_usize("OK_ANN_CODE_SHAPE_QUERIES", DEFAULT_QUERY_COUNT)?;
    validate_positive_list("OK_ANN_CODE_SHAPE_SIZES", &sizes)?;
    validate_positive_list("OK_ANN_CODE_SHAPE_DIMS", &dimensions)?;
    validate_positive_list("OK_ANN_CODE_SHAPE_SEARCH_EXPANSIONS", &search_expansions)?;
    if query_count == 0 {
        return Err("OK_ANN_CODE_SHAPE_QUERIES must be greater than zero".into());
    }

    let mut measurements = Vec::new();
    for &dimensions in &dimensions {
        for &vector_count in &sizes {
            let fixture = ExactFixture::new(dimensions, vector_count, query_count)?;
            for &expansion_search in &search_expansions {
                let parameters = HnswParameters {
                    connectivity: PRODUCTION_HNSW_PARAMETERS.connectivity,
                    expansion_add: PRODUCTION_HNSW_PARAMETERS.expansion_add,
                    expansion_search,
                };
                measurements.push(fixture.measure_ann(parameters)?);
            }
        }
    }

    let report = Report {
        schema_version: 1,
        benchmark: "cc5-ann-code-shape-validation",
        backend: "usearch-hnsw-f32",
        oracle: "exact-flat",
        distribution: "deterministic-code-shaped-v1",
        distribution_contract: "hierarchical topic/module/symbol-family clusters with near-duplicate revisions, mixed code/test/doc/config targets, deterministic query perturbations",
        requested_sizes: sizes,
        requested_dimensions: dimensions,
        requested_search_expansions: search_expansions,
        measurements,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

struct ExactFixture {
    dimensions: usize,
    vector_count: usize,
    query_count: usize,
    queries: Vec<Vec<f32>>,
    oracle_hits: Vec<Vec<VectorHit>>,
    exact_build_ms: f64,
    exact_query: LatencyMetrics,
}

impl ExactFixture {
    fn new(
        dimensions: usize,
        vector_count: usize,
        query_count: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let exact_started = Instant::now();
        let mut exact = ExactFlatVectorIndex::new(dimensions)?;
        for id in 0..vector_count {
            exact.add(record(id, dimensions))?;
        }
        let exact_build_ms = elapsed_ms(exact_started);

        let effective_queries = query_count.min(vector_count);
        let queries = (0..effective_queries)
            .map(|query_index| {
                let target = query_target(query_index, vector_count);
                query_vector(target, dimensions)
            })
            .collect::<Vec<_>>();
        let mut oracle_hits = Vec::with_capacity(effective_queries);
        let mut exact_latencies = Vec::with_capacity(effective_queries);
        for query in &queries {
            let started = Instant::now();
            oracle_hits.push(exact.search(query, search_options())?);
            exact_latencies.push(elapsed_us(started));
        }
        drop(exact);

        Ok(Self {
            dimensions,
            vector_count,
            query_count: effective_queries,
            queries,
            oracle_hits,
            exact_build_ms,
            exact_query: latency_metrics(&exact_latencies),
        })
    }

    fn measure_ann(
        &self,
        parameters: HnswParameters,
    ) -> Result<Measurement, Box<dyn std::error::Error>> {
        let ann_started = Instant::now();
        let mut ann = UsearchHnswVectorIndex::with_parameters(
            self.dimensions,
            AnnScalarKind::F32,
            self.vector_count,
            parameters,
        )?;
        for id in 0..self.vector_count {
            ann.add(record(id, self.dimensions))?;
        }
        let ann_build_ms = elapsed_ms(ann_started);

        let mut ann_latencies = Vec::with_capacity(self.query_count);
        let mut recall_at_1_sum = 0.0;
        let mut recall_at_5_sum = 0.0;
        let mut recall_at_10_sum = 0.0;
        let mut recall_at_20_sum = 0.0;
        let mut reciprocal_rank_sum = 0.0;
        for (query, oracle) in self.queries.iter().zip(&self.oracle_hits) {
            let started = Instant::now();
            let hits = ann.search(query, search_options())?;
            ann_latencies.push(elapsed_us(started));
            recall_at_1_sum += recall_at_k(oracle, &hits, 1);
            recall_at_5_sum += recall_at_k(oracle, &hits, 5);
            recall_at_10_sum += recall_at_k(oracle, &hits, 10);
            recall_at_20_sum += recall_at_k(oracle, &hits, 20);
            reciprocal_rank_sum += reciprocal_rank(oracle, &hits);
        }

        let denominator = self.query_count.max(1) as f64;
        Ok(Measurement {
            vector_count: self.vector_count,
            dimensions: self.dimensions,
            query_count: self.query_count,
            parameters,
            quality: QualityMetrics {
                recall_at_1: recall_at_1_sum / denominator,
                recall_at_5: recall_at_5_sum / denominator,
                recall_at_10: recall_at_10_sum / denominator,
                recall_at_20: recall_at_20_sum / denominator,
                mrr: reciprocal_rank_sum / denominator,
            },
            exact_build_ms: self.exact_build_ms,
            ann_build_ms,
            ann_vectors_per_second: self.vector_count as f64
                / (ann_build_ms / 1_000.0).max(f64::EPSILON),
            exact_query: self.exact_query.clone(),
            ann_query: latency_metrics(&ann_latencies),
        })
    }
}

fn record(id: usize, dimensions: usize) -> VectorRecord {
    VectorRecord {
        id: VectorId((id + 1) as u64),
        target_id: format!("code-shape-{id}"),
        target_kind: target_kind(id).into(),
        vector: code_shaped_unit_vector(id, dimensions),
    }
}

fn target_kind(id: usize) -> &'static str {
    match id % 16 {
        0 => "test",
        1 | 2 => "doc",
        3 => "config",
        _ => "code",
    }
}

fn query_target(query_index: usize, vector_count: usize) -> usize {
    if vector_count == 0 {
        return 0;
    }
    ((query_index as u64 * 1_000_003 + 97) % vector_count as u64) as usize
}

fn query_vector(target: usize, dimensions: usize) -> Vec<f32> {
    let mut vector = code_shaped_unit_vector(target, dimensions);
    let mut noise = (target as u64 + 1) ^ 0x517c_c1b7_2722_0a95;
    for value in &mut vector {
        noise = xorshift64(noise);
        let jitter = ((noise >> 40) as i32 - 32_768) as f32 / 32_768.0;
        *value += jitter * 0.003;
    }
    normalize(&mut vector);
    vector
}

fn code_shaped_unit_vector(id: usize, dimensions: usize) -> Vec<f32> {
    let topic = id / 32_768;
    let module = id / 512;
    let symbol_family = id / 8;
    let revision = id % 8;

    let topic_vector = deterministic_vector(
        (topic as u64 + 1).wrapping_mul(0xa24b_aed4_963e_e407),
        dimensions,
    );
    let module_vector = deterministic_vector(
        (module as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        dimensions,
    );
    let family_vector = deterministic_vector(
        (symbol_family as u64 + 1).wrapping_mul(0xd134_2543_de82_ef95),
        dimensions,
    );
    let revision_vector = deterministic_vector(
        ((symbol_family as u64 + 1) << 4 | revision as u64).wrapping_mul(0x94d0_49bb_1331_11eb),
        dimensions,
    );

    let mut vector = Vec::with_capacity(dimensions);
    for index in 0..dimensions {
        vector.push(
            topic_vector[index] * 0.30
                + module_vector[index] * 0.28
                + family_vector[index] * 0.36
                + revision_vector[index] * 0.06,
        );
    }
    normalize(&mut vector);
    vector
}

fn deterministic_vector(seed: u64, dimensions: usize) -> Vec<f32> {
    let mut state = seed.max(1);
    let mut vector = Vec::with_capacity(dimensions);
    for _ in 0..dimensions {
        state = xorshift64(state);
        let centered = ((state >> 40) as i32 - 32_768) as f32 / 32_768.0;
        vector.push(centered);
    }
    vector
}

fn search_options() -> VectorSearchOptions {
    VectorSearchOptions {
        limit: MAX_K,
        allowlist: None,
        target_kind: None,
    }
}

fn recall_at_k(exact: &[VectorHit], ann: &[VectorHit], k: usize) -> f64 {
    let effective_k = k.min(exact.len());
    if effective_k == 0 {
        return 0.0;
    }
    let oracle = exact
        .iter()
        .take(effective_k)
        .map(|hit| hit.id)
        .collect::<HashSet<_>>();
    let matched = ann
        .iter()
        .take(effective_k)
        .filter(|hit| oracle.contains(&hit.id))
        .count();
    matched as f64 / effective_k as f64
}

fn reciprocal_rank(exact: &[VectorHit], ann: &[VectorHit]) -> f64 {
    let Some(oracle_top) = exact.first().map(|hit| hit.id) else {
        return 0.0;
    };
    ann.iter()
        .position(|hit| hit.id == oracle_top)
        .map(|rank| 1.0 / (rank + 1) as f64)
        .unwrap_or(0.0)
}

fn latency_metrics(values: &[f64]) -> LatencyMetrics {
    LatencyMetrics {
        mean_us: mean(values),
        p50_us: percentile(values, 0.50),
        p95_us: percentile(values, 0.95),
        p99_us: percentile(values, 0.99),
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in vector {
            *value /= norm;
        }
    }
}

fn xorshift64(mut value: u64) -> u64 {
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    value
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn elapsed_us(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000_000.0
}

fn parse_usize(name: &str, default: usize) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(std::env::var(name)
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(default))
}

fn parse_list(name: &str, default: &[usize]) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let Some(raw) = std::env::var(name).ok() else {
        return Ok(default.to_vec());
    };
    raw.split(',')
        .map(|value| Ok(value.trim().parse::<usize>()?))
        .collect()
}

fn validate_positive_list(name: &str, values: &[usize]) -> Result<(), Box<dyn std::error::Error>> {
    if values.is_empty() || values.contains(&0) {
        return Err(format!("{name} must contain one or more positive integers").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_shaped_fixture_is_deterministic_and_normalized() {
        let first = code_shaped_unit_vector(42, 64);
        let second = code_shaped_unit_vector(42, 64);
        assert_eq!(first, second);
        let norm = first.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn sibling_revisions_are_closer_than_unrelated_symbols() {
        let anchor = code_shaped_unit_vector(80, 128);
        let sibling = code_shaped_unit_vector(81, 128);
        let unrelated = code_shaped_unit_vector(8_000, 128);
        assert!(dot(&anchor, &sibling) > dot(&anchor, &unrelated));
    }

    #[test]
    fn target_mix_includes_repository_artifact_families() {
        let kinds = (0..32).map(target_kind).collect::<HashSet<_>>();
        assert_eq!(kinds, HashSet::from(["code", "test", "doc", "config"]));
    }

    fn dot(left: &[f32], right: &[f32]) -> f32 {
        left.iter()
            .zip(right)
            .map(|(left, right)| left * right)
            .sum()
    }
}
