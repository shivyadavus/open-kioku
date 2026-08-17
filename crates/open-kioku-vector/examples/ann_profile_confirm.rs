use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, HnswParameters, UsearchHnswVectorIndex, VectorHit,
    VectorId, VectorRecord, VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS,
};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::time::Instant;

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
    recall_at_10: f64,
    mrr: f64,
    exact_build_ms: f64,
    ann_build_ms: f64,
    exact_query_p95_us: f64,
    ann_query_p95_us: f64,
    p95_query_speedup: f64,
    exact_index_bytes: u64,
    ann_index_bytes: u64,
    ann_memory_bytes: usize,
    process_peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    backend: &'static str,
    parameters: HnswParameters,
    recall_floor: f64,
    minimum_p95_speedup: f64,
    host: HostProfile,
    measurements: Vec<Measurement>,
    recommended_auto_min_rows: Option<usize>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = parse_list("OK_ANN_CONFIRM_SIZES", &[10_000, 25_000])?;
    let dimensions = parse_list("OK_ANN_CONFIRM_DIMS", &[384, 768])?;
    let query_count = parse_usize("OK_ANN_CONFIRM_QUERIES", 64)?;
    let parameters = HnswParameters {
        connectivity: parse_usize(
            "OK_ANN_CONFIRM_CONNECTIVITY",
            PRODUCTION_HNSW_PARAMETERS.connectivity,
        )?,
        expansion_add: parse_usize(
            "OK_ANN_CONFIRM_EXPANSION_ADD",
            PRODUCTION_HNSW_PARAMETERS.expansion_add,
        )?,
        expansion_search: parse_usize(
            "OK_ANN_CONFIRM_EXPANSION_SEARCH",
            PRODUCTION_HNSW_PARAMETERS.expansion_search,
        )?,
    };
    let recall_floor = 0.98;
    let minimum_p95_speedup = 1.5;
    let mut measurements = Vec::new();

    for &dims in &dimensions {
        for &size in &sizes {
            measurements.push(measure(dims, size, query_count, parameters)?);
        }
    }

    let mut thresholds = sizes.clone();
    thresholds.sort_unstable();
    thresholds.dedup();
    let recommended_auto_min_rows = thresholds.into_iter().find(|threshold| {
        dimensions.iter().all(|dimension| {
            measurements.iter().any(|row| {
                row.dimensions == *dimension
                    && row.vector_count == *threshold
                    && row.recall_at_10 >= recall_floor
                    && row.p95_query_speedup >= minimum_p95_speedup
            })
        }) && measurements
            .iter()
            .filter(|row| row.vector_count >= *threshold)
            .all(|row| {
                row.recall_at_10 >= recall_floor && row.p95_query_speedup >= minimum_p95_speedup
            })
    });

    let report = Report {
        schema_version: 1,
        backend: "usearch-hnsw-f32",
        parameters,
        recall_floor,
        minimum_p95_speedup,
        host: host_profile(),
        measurements,
        recommended_auto_min_rows,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn measure(
    dimensions: usize,
    vector_count: usize,
    query_count: usize,
    parameters: HnswParameters,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let records = (0..vector_count)
        .map(|id| record(id, dimensions))
        .collect::<Vec<_>>();

    let exact_started = Instant::now();
    let mut exact = ExactFlatVectorIndex::new(dimensions)?;
    for record in &records {
        exact.add(record.clone())?;
    }
    let exact_build_ms = exact_started.elapsed().as_secs_f64() * 1_000.0;

    let ann_started = Instant::now();
    let mut ann = UsearchHnswVectorIndex::with_parameters(
        dimensions,
        AnnScalarKind::F32,
        vector_count,
        parameters,
    )?;
    for record in records {
        ann.add(record)?;
    }
    let ann_build_ms = ann_started.elapsed().as_secs_f64() * 1_000.0;

    let temp = tempfile::tempdir()?;
    let exact_path = temp.path().join("exact.json");
    let ann_path = temp.path().join("ann.usearch");
    exact.save(&exact_path)?;
    ann.save(&ann_path)?;

    let effective_queries = query_count.min(vector_count.max(1));
    let mut exact_latencies = Vec::with_capacity(effective_queries);
    let mut ann_latencies = Vec::with_capacity(effective_queries);
    let mut recall_sum = 0.0;
    let mut reciprocal_rank_sum = 0.0;

    for query_index in 0..effective_queries {
        let target = query_target(query_index, vector_count);
        let query = query_vector(target, dimensions);
        let options = VectorSearchOptions {
            limit: 10,
            allowlist: None,
            target_kind: None,
        };

        let started = Instant::now();
        let exact_hits = exact.search(&query, options.clone())?;
        exact_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);

        let started = Instant::now();
        let ann_hits = ann.search(&query, options)?;
        ann_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);

        recall_sum += recall_at_10(&exact_hits, &ann_hits);
        reciprocal_rank_sum += reciprocal_rank(&exact_hits, &ann_hits);
    }

    let exact_query_p95_us = percentile(&exact_latencies, 0.95);
    let ann_query_p95_us = percentile(&ann_latencies, 0.95);
    Ok(Measurement {
        dimensions,
        vector_count,
        query_count: effective_queries,
        recall_at_10: recall_sum / effective_queries.max(1) as f64,
        mrr: reciprocal_rank_sum / effective_queries.max(1) as f64,
        exact_build_ms,
        ann_build_ms,
        exact_query_p95_us,
        ann_query_p95_us,
        p95_query_speedup: exact_query_p95_us / ann_query_p95_us.max(f64::EPSILON),
        exact_index_bytes: fs::metadata(&exact_path)?.len(),
        ann_index_bytes: fs::metadata(&ann_path)?.len(),
        ann_memory_bytes: ann.memory_usage_bytes(),
        process_peak_rss_bytes: process_peak_rss_bytes(),
    })
}

fn recall_at_10(exact: &[VectorHit], ann: &[VectorHit]) -> f64 {
    let oracle = exact.iter().map(|hit| hit.id).collect::<HashSet<_>>();
    ann.iter().filter(|hit| oracle.contains(&hit.id)).count() as f64 / exact.len().max(1) as f64
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
