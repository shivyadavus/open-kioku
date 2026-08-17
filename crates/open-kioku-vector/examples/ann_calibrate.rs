use open_kioku_vector::{
    ExactFlatVectorIndex, VectorHit, VectorId, VectorRecord, VectorSearchOptions,
};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::time::Instant;
use usearch::{new_index, IndexOptions, MetricKind, ScalarKind};

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
struct Measurement {
    dimensions: usize,
    vector_count: usize,
    query_count: usize,
    connectivity: usize,
    expansion_add: usize,
    expansion_search: usize,
    recall_at_10: f64,
    exact_build_ms: f64,
    ann_build_ms: f64,
    exact_query_mean_us: f64,
    exact_query_p95_us: f64,
    ann_query_mean_us: f64,
    ann_query_p95_us: f64,
    p95_query_speedup: f64,
    exact_index_bytes: u64,
    ann_index_bytes: u64,
    ann_memory_bytes: usize,
    process_peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct Recommendation {
    auto_min_rows: usize,
    connectivity: usize,
    expansion_add: usize,
    expansion_search: usize,
    worst_recall_at_10: f64,
    worst_p95_speedup: f64,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    backend: &'static str,
    recall_floor: f64,
    minimum_p95_speedup: f64,
    host: HostProfile,
    measurements: Vec<Measurement>,
    recommendation: Option<Recommendation>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = parse_list("OK_ANN_CALIBRATION_SIZES", &[5_000, 10_000, 25_000])?;
    let dimensions = parse_list("OK_ANN_CALIBRATION_DIMS", &[384, 768])?;
    let connectivities = parse_list("OK_ANN_CALIBRATION_CONNECTIVITIES", &[16, 32, 48])?;
    let expansion_adds = parse_list("OK_ANN_CALIBRATION_EXPANSION_ADDS", &[128, 256, 512])?;
    let expansion_searches = parse_list(
        "OK_ANN_CALIBRATION_SEARCH_EXPANSIONS",
        &[512, 1_024, 2_048, 4_096],
    )?;
    let query_count = std::env::var("OK_ANN_CALIBRATION_QUERIES")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(24usize);
    let recall_floor = 0.98;
    let minimum_p95_speedup = 1.5;
    let mut measurements = Vec::new();

    for &dims in &dimensions {
        for &size in &sizes {
            let fixture = Fixture::new(dims, size, query_count)?;
            for &connectivity in &connectivities {
                for &expansion_add in &expansion_adds {
                    for &expansion_search in &expansion_searches {
                        measurements.push(fixture.measure(
                            connectivity,
                            expansion_add,
                            expansion_search,
                        )?);
                    }
                }
            }
        }
    }

    let recommendation = recommend(
        &measurements,
        &dimensions,
        &sizes,
        recall_floor,
        minimum_p95_speedup,
    );

    let report = Report {
        schema_version: 3,
        backend: "usearch-hnsw-f32",
        recall_floor,
        minimum_p95_speedup,
        host: host_profile(),
        measurements,
        recommendation,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

struct Fixture {
    dimensions: usize,
    vector_count: usize,
    query_count: usize,
    records: Vec<VectorRecord>,
    query_vectors: Vec<Vec<f32>>,
    exact_hits: Vec<Vec<VectorHit>>,
    exact_build_ms: f64,
    exact_query_mean_us: f64,
    exact_query_p95_us: f64,
    exact_index_bytes: u64,
}

impl Fixture {
    fn new(
        dimensions: usize,
        vector_count: usize,
        query_count: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let records = (0..vector_count)
            .map(|id| record(id, dimensions))
            .collect::<Vec<_>>();

        let exact_started = Instant::now();
        let mut exact = ExactFlatVectorIndex::new(dimensions)?;
        for record in &records {
            exact.add(record.clone())?;
        }
        let exact_build_ms = exact_started.elapsed().as_secs_f64() * 1_000.0;

        let temp = tempfile::tempdir()?;
        let exact_path = temp.path().join("exact.json");
        exact.save(&exact_path)?;
        let exact_index_bytes = fs::metadata(&exact_path)?.len();

        let effective_queries = query_count.min(vector_count.max(1));
        let query_vectors = (0..effective_queries)
            .map(|query_index| {
                let target = query_target(query_index, vector_count);
                query_vector(target, dimensions)
            })
            .collect::<Vec<_>>();

        let mut exact_hits = Vec::with_capacity(effective_queries);
        let mut exact_latencies = Vec::with_capacity(effective_queries);
        for query in &query_vectors {
            let started = Instant::now();
            exact_hits.push(exact.search(query, options())?);
            exact_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        }

        Ok(Self {
            dimensions,
            vector_count,
            query_count: effective_queries,
            records,
            query_vectors,
            exact_hits,
            exact_build_ms,
            exact_query_mean_us: mean(&exact_latencies),
            exact_query_p95_us: percentile(&exact_latencies, 0.95),
            exact_index_bytes,
        })
    }

    fn measure(
        &self,
        connectivity: usize,
        expansion_add: usize,
        expansion_search: usize,
    ) -> Result<Measurement, Box<dyn std::error::Error>> {
        let ann_started = Instant::now();
        let ann = new_index(&IndexOptions {
            dimensions: self.dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            connectivity,
            expansion_add,
            expansion_search,
            multi: false,
        })?;
        ann.reserve(self.vector_count.max(1))?;
        for record in &self.records {
            ann.add(record.id.0, &record.vector)?;
        }
        let ann_build_ms = ann_started.elapsed().as_secs_f64() * 1_000.0;

        let temp = tempfile::tempdir()?;
        let ann_path = temp.path().join("ann.usearch");
        ann.save(ann_path.to_str().ok_or("non-UTF-8 temp path")?)?;
        let ann_index_bytes = fs::metadata(&ann_path)?.len();

        let mut ann_latencies = Vec::with_capacity(self.query_count);
        let mut recall_sum = 0.0f64;
        for (query, oracle_hits) in self.query_vectors.iter().zip(&self.exact_hits) {
            let started = Instant::now();
            let ann_hits = ann.search(query, 10)?;
            ann_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            recall_sum += recall_at_10_keys(oracle_hits, &ann_hits.keys);
        }
        let ann_query_mean_us = mean(&ann_latencies);
        let ann_query_p95_us = percentile(&ann_latencies, 0.95);

        Ok(Measurement {
            dimensions: self.dimensions,
            vector_count: self.vector_count,
            query_count: self.query_count,
            connectivity,
            expansion_add,
            expansion_search,
            recall_at_10: recall_sum / self.query_count.max(1) as f64,
            exact_build_ms: self.exact_build_ms,
            ann_build_ms,
            exact_query_mean_us: self.exact_query_mean_us,
            exact_query_p95_us: self.exact_query_p95_us,
            ann_query_mean_us,
            ann_query_p95_us,
            p95_query_speedup: self.exact_query_p95_us / ann_query_p95_us.max(f64::EPSILON),
            exact_index_bytes: self.exact_index_bytes,
            ann_index_bytes,
            ann_memory_bytes: ann.memory_usage(),
            process_peak_rss_bytes: process_peak_rss_bytes(),
        })
    }
}

fn recommend(
    measurements: &[Measurement],
    dimensions: &[usize],
    sizes: &[usize],
    recall_floor: f64,
    minimum_p95_speedup: f64,
) -> Option<Recommendation> {
    let mut parameter_sets = measurements
        .iter()
        .map(|row| (row.connectivity, row.expansion_add, row.expansion_search))
        .collect::<Vec<_>>();
    parameter_sets.sort_unstable();
    parameter_sets.dedup();

    let mut thresholds = sizes.to_vec();
    thresholds.sort_unstable();
    thresholds.dedup();

    let mut candidates = Vec::new();
    for (connectivity, expansion_add, expansion_search) in parameter_sets {
        for &threshold in &thresholds {
            let rows = measurements
                .iter()
                .filter(|row| {
                    row.connectivity == connectivity
                        && row.expansion_add == expansion_add
                        && row.expansion_search == expansion_search
                        && row.vector_count >= threshold
                })
                .collect::<Vec<_>>();
            if rows.is_empty() {
                continue;
            }
            let covers_every_dimension = dimensions
                .iter()
                .all(|dimension| rows.iter().any(|row| row.dimensions == *dimension));
            let clears_gate = rows.iter().all(|row| {
                row.recall_at_10 >= recall_floor && row.p95_query_speedup >= minimum_p95_speedup
            });
            if !covers_every_dimension || !clears_gate {
                continue;
            }
            let worst_recall_at_10 = rows
                .iter()
                .map(|row| row.recall_at_10)
                .fold(f64::INFINITY, f64::min);
            let worst_p95_speedup = rows
                .iter()
                .map(|row| row.p95_query_speedup)
                .fold(f64::INFINITY, f64::min);
            candidates.push(Recommendation {
                auto_min_rows: threshold,
                connectivity,
                expansion_add,
                expansion_search,
                worst_recall_at_10,
                worst_p95_speedup,
            });
        }
    }

    candidates.into_iter().min_by(|left, right| {
        left.auto_min_rows
            .cmp(&right.auto_min_rows)
            .then_with(|| left.connectivity.cmp(&right.connectivity))
            .then_with(|| left.expansion_add.cmp(&right.expansion_add))
            .then_with(|| left.expansion_search.cmp(&right.expansion_search))
            .then_with(|| {
                right
                    .worst_p95_speedup
                    .partial_cmp(&left.worst_p95_speedup)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })
}

fn options() -> VectorSearchOptions {
    VectorSearchOptions {
        limit: 10,
        allowlist: None,
        target_kind: None,
    }
}

fn recall_at_10_keys(exact_hits: &[VectorHit], ann_keys: &[u64]) -> f64 {
    let oracle = exact_hits
        .iter()
        .map(|hit| hit.id.0)
        .collect::<HashSet<_>>();
    let matched = ann_keys.iter().filter(|key| oracle.contains(key)).count();
    matched as f64 / exact_hits.len().max(1) as f64
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
        vector: deterministic_unit_vector(id as u64 + 1, dimensions),
    }
}

fn query_target(query_index: usize, vector_count: usize) -> usize {
    if vector_count == 0 {
        return 0;
    }
    ((query_index as u64 * 1_000_003 + 97) % vector_count as u64) as usize
}

fn query_vector(target: usize, dimensions: usize) -> Vec<f32> {
    let mut vector = deterministic_unit_vector(target as u64 + 1, dimensions);
    let mut noise = target as u64 ^ 0x9e37_79b9_7f4a_7c15;
    for value in &mut vector {
        noise = xorshift64(noise);
        let jitter = ((noise >> 40) as i32 - 32_768) as f32 / 32_768.0;
        *value += jitter * 0.0025;
    }
    normalize(&mut vector);
    vector
}

fn deterministic_unit_vector(seed: u64, dimensions: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0xd134_2543_de82_ef95).max(1);
    let mut vector = Vec::with_capacity(dimensions);
    for _ in 0..dimensions {
        state = xorshift64(state);
        let centered = ((state >> 40) as i32 - 32_768) as f32 / 32_768.0;
        vector.push(centered);
    }
    normalize(&mut vector);
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

fn process_peak_rss_bytes() -> Option<u64> {
    let content = fs::read_to_string("/proc/self/status").ok()?;
    content.lines().find_map(|line| {
        if !line.starts_with("VmHWM:") {
            return None;
        }
        line.split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|value| value * 1024)
    })
}

fn parse_list(name: &str, default: &[usize]) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let Some(raw) = std::env::var(name).ok() else {
        return Ok(default.to_vec());
    };
    raw.split(',')
        .map(|value| Ok(value.trim().parse::<usize>()?))
        .collect()
}
