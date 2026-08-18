use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, HnswParameters, UsearchHnswVectorIndex, VectorHit,
    VectorId, VectorRecord, VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS,
};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

const MAX_K: usize = 20;
const DEFAULT_SIZES: &[usize] = &[50_000, 100_000, 300_000, 1_000_000];
const DEFAULT_DIMS: &[usize] = &[384, 768];
const DEFAULT_SEARCH_EXPANSIONS: &[usize] = &[64, 128, 256, 512, 1_024];

#[derive(Debug, Clone, Serialize)]
struct HostProfile {
    os: String,
    arch: String,
    logical_cpus: usize,
    total_memory_bytes: Option<u64>,
    cpu_model: Option<String>,
    profile_label: Option<String>,
}

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
    dimensions: usize,
    vector_count: usize,
    query_count: usize,
    parameters: HnswParameters,
    quality: QualityMetrics,
    exact_build_ms: f64,
    ann_build_ms: f64,
    ann_vectors_per_second: f64,
    exact_query: LatencyMetrics,
    ann_query: LatencyMetrics,
    ann_reload_ms: f64,
    ann_first_query_after_reload_us: f64,
    exact_index_bytes: u64,
    ann_index_bytes: u64,
    ann_metadata_bytes: u64,
    ann_memory_bytes: usize,
    process_rss_bytes: Option<u64>,
    process_peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    benchmark: &'static str,
    backend: &'static str,
    oracle: &'static str,
    distribution: &'static str,
    host: HostProfile,
    requested_sizes: Vec<usize>,
    requested_dimensions: Vec<usize>,
    requested_search_expansions: Vec<usize>,
    measurements: Vec<Measurement>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = parse_list("OK_ANN_SCALE_SIZES", DEFAULT_SIZES)?;
    let dimensions = parse_list("OK_ANN_SCALE_DIMS", DEFAULT_DIMS)?;
    let search_expansions = parse_list(
        "OK_ANN_SCALE_SEARCH_EXPANSIONS",
        DEFAULT_SEARCH_EXPANSIONS,
    )?;
    let query_count = parse_usize("OK_ANN_SCALE_QUERIES", 64)?;
    let connectivity = parse_usize(
        "OK_ANN_SCALE_CONNECTIVITY",
        PRODUCTION_HNSW_PARAMETERS.connectivity,
    )?;
    let expansion_add = parse_usize(
        "OK_ANN_SCALE_EXPANSION_ADD",
        PRODUCTION_HNSW_PARAMETERS.expansion_add,
    )?;

    validate_nonzero("OK_ANN_SCALE_SIZES", &sizes)?;
    validate_nonzero("OK_ANN_SCALE_DIMS", &dimensions)?;
    validate_nonzero("OK_ANN_SCALE_SEARCH_EXPANSIONS", &search_expansions)?;
    if query_count == 0 {
        return Err("OK_ANN_SCALE_QUERIES must be greater than zero".into());
    }
    if connectivity == 0 || expansion_add == 0 {
        return Err("HNSW connectivity and expansion_add must be greater than zero".into());
    }

    let mut measurements = Vec::new();
    for &dims in &dimensions {
        for &size in &sizes {
            let fixture = ExactFixture::new(dims, size, query_count)?;
            for &expansion_search in &search_expansions {
                let parameters = HnswParameters {
                    connectivity,
                    expansion_add,
                    expansion_search,
                };
                measurements.push(fixture.measure_ann(parameters)?);
            }
        }
    }

    let report = Report {
        schema_version: 1,
        benchmark: "cc5-ann-scale-matrix",
        backend: "usearch-hnsw-f32",
        oracle: "exact-flat",
        distribution: "deterministic-clustered-synthetic-v1",
        host: host_profile(),
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
    _exact_oracle: ExactFlatVectorIndex,
    queries: Vec<Vec<f32>>,
    oracle_hits: Vec<Vec<VectorHit>>,
    exact_build_ms: f64,
    exact_query: LatencyMetrics,
    exact_index_bytes: u64,
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

        let temp = tempfile::tempdir()?;
        let exact_path = temp.path().join("exact.json");
        exact.save(&exact_path)?;
        let exact_index_bytes = fs::metadata(&exact_path)?.len();

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

        Ok(Self {
            dimensions,
            vector_count,
            query_count: effective_queries,
            _exact_oracle: exact,
            queries,
            oracle_hits,
            exact_build_ms,
            exact_query: latency_metrics(&exact_latencies),
            exact_index_bytes,
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
        let ann_vectors_per_second = self.vector_count as f64
            / (ann_build_ms / 1_000.0).max(f64::EPSILON);

        let temp = tempfile::tempdir()?;
        let ann_path = temp.path().join("ann.usearch");
        ann.save(&ann_path)?;
        let ann_index_bytes = fs::metadata(&ann_path)?.len();
        let ann_metadata_bytes = fs::metadata(metadata_path(&ann_path))?.len();

        let load_started = Instant::now();
        let loaded = UsearchHnswVectorIndex::load(&ann_path)?;
        let ann_reload_ms = elapsed_ms(load_started);

        let ann_first_query_after_reload_us = if let Some(query) = self.queries.first() {
            let started = Instant::now();
            let _ = loaded.search(query, search_options())?;
            elapsed_us(started)
        } else {
            0.0
        };

        let mut ann_latencies = Vec::with_capacity(self.query_count);
        let mut recall_at_1_sum = 0.0;
        let mut recall_at_5_sum = 0.0;
        let mut recall_at_10_sum = 0.0;
        let mut recall_at_20_sum = 0.0;
        let mut reciprocal_rank_sum = 0.0;

        for (query, oracle) in self.queries.iter().zip(&self.oracle_hits) {
            let started = Instant::now();
            let hits = loaded.search(query, search_options())?;
            ann_latencies.push(elapsed_us(started));

            recall_at_1_sum += recall_at_k(oracle, &hits, 1);
            recall_at_5_sum += recall_at_k(oracle, &hits, 5);
            recall_at_10_sum += recall_at_k(oracle, &hits, 10);
            recall_at_20_sum += recall_at_k(oracle, &hits, 20);
            reciprocal_rank_sum += reciprocal_rank(oracle, &hits);
        }

        let denominator = self.query_count.max(1) as f64;
        Ok(Measurement {
            dimensions: self.dimensions,
            vector_count: self.vector_count,
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
            ann_vectors_per_second,
            exact_query: self.exact_query.clone(),
            ann_query: latency_metrics(&ann_latencies),
            ann_reload_ms,
            ann_first_query_after_reload_us,
            exact_index_bytes: self.exact_index_bytes,
            ann_index_bytes,
            ann_metadata_bytes,
            ann_memory_bytes: loaded.memory_usage_bytes(),
            process_rss_bytes: process_rss_bytes(),
            process_peak_rss_bytes: process_peak_rss_bytes(),
        })
    }
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

fn record(id: usize, dimensions: usize) -> VectorRecord {
    VectorRecord {
        id: VectorId((id + 1) as u64),
        target_id: format!("synthetic-{id}"),
        target_kind: if id % 5 == 0 { "doc" } else { "code" }.into(),
        vector: clustered_unit_vector(id, dimensions),
    }
}

fn query_target(query_index: usize, vector_count: usize) -> usize {
    if vector_count == 0 {
        return 0;
    }
    ((query_index as u64 * 1_000_003 + 97) % vector_count as u64) as usize
}

fn query_vector(target: usize, dimensions: usize) -> Vec<f32> {
    let mut vector = clustered_unit_vector(target, dimensions);
    let mut noise = target as u64 ^ 0x9e37_79b9_7f4a_7c15;
    for value in &mut vector {
        noise = xorshift64(noise);
        let jitter = ((noise >> 40) as i32 - 32_768) as f32 / 32_768.0;
        *value += jitter * 0.0025;
    }
    normalize(&mut vector);
    vector
}

fn clustered_unit_vector(id: usize, dimensions: usize) -> Vec<f32> {
    let cluster = (id % 256) as u64 + 1;
    let mut centroid =
        deterministic_vector(cluster.wrapping_mul(0xa24b_aed4_963e_e407), dimensions);
    let member = deterministic_vector(
        (id as u64 + 1).wrapping_mul(0xd134_2543_de82_ef95),
        dimensions,
    );
    for (centroid_value, member_value) in centroid.iter_mut().zip(member) {
        *centroid_value = *centroid_value * 0.94 + member_value * 0.06;
    }
    normalize(&mut centroid);
    centroid
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

fn xorshift64(mut value: u64) -> u64 {
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    value
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in vector {
            *value /= norm;
        }
    }
}

fn metadata_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("meta.json")
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn elapsed_us(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000_000.0
}

fn host_profile() -> HostProfile {
    HostProfile {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        total_memory_bytes: linux_meminfo_kib("MemTotal:").map(|value| value * 1024),
        cpu_model: linux_cpu_model(),
        profile_label: std::env::var("OK_CC5_HOST_PROFILE").ok(),
    }
}

fn linux_cpu_model() -> Option<String> {
    let content = fs::read_to_string("/proc/cpuinfo").ok()?;
    content.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim().to_owned())
    })
}

fn linux_meminfo_kib(key: &str) -> Option<u64> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    content.lines().find_map(|line| {
        if !line.starts_with(key) {
            return None;
        }
        line.split_whitespace().nth(1)?.parse().ok()
    })
}

fn process_rss_bytes() -> Option<u64> {
    linux_status_kib("VmRSS:").map(|value| value * 1024)
}

fn process_peak_rss_bytes() -> Option<u64> {
    linux_status_kib("VmHWM:").map(|value| value * 1024)
}

fn linux_status_kib(key: &str) -> Option<u64> {
    let content = fs::read_to_string("/proc/self/status").ok()?;
    content.lines().find_map(|line| {
        if !line.starts_with(key) {
            return None;
        }
        line.split_whitespace().nth(1)?.parse().ok()
    })
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

fn validate_nonzero(name: &str, values: &[usize]) -> Result<(), Box<dyn std::error::Error>> {
    if values.is_empty() || values.contains(&0) {
        return Err(format!("{name} must contain one or more positive integers").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_at_k_uses_the_same_cutoff_for_oracle_and_candidate() {
        let exact = hits(&[1, 2, 3, 4, 5]);
        let ann = hits(&[1, 9, 3, 8, 5]);
        assert_eq!(recall_at_k(&exact, &ann, 1), 1.0);
        assert!((recall_at_k(&exact, &ann, 5) - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn reciprocal_rank_tracks_the_exact_oracle_top_hit() {
        let exact = hits(&[4, 2, 1]);
        let ann = hits(&[9, 4, 2]);
        assert!((reciprocal_rank(&exact, &ann) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clustered_fixture_is_deterministic_and_normalized() {
        let first = clustered_unit_vector(42, 32);
        let second = clustered_unit_vector(42, 32);
        assert_eq!(first, second);
        let norm = first.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    fn hits(ids: &[u64]) -> Vec<VectorHit> {
        ids.iter()
            .map(|id| VectorHit {
                id: VectorId(*id),
                target_id: format!("target-{id}"),
                target_kind: "code".into(),
                score: 1.0,
            })
            .collect()
    }
}
