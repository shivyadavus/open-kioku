use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, UsearchHnswVectorIndex, VectorHit, VectorId, VectorRecord,
    VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS,
};
use serde::Serialize;
use std::collections::HashSet;
use std::time::Instant;

const MAX_K: usize = 20;
const DEFAULT_SIZES: &[usize] = &[50_000, 100_000, 300_000, 1_000_000];
const DEFAULT_DIMS: &[usize] = &[384, 768];
const DEFAULT_QUERY_COUNT: usize = 64;
const DEFAULT_MUTATION_BPS: usize = 100; // 1%

#[derive(Debug, Serialize)]
struct QualityMetrics {
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
    recall_at_20: f64,
    mrr: f64,
}

#[derive(Debug, Serialize)]
struct Measurement {
    vector_count: usize,
    dimensions: usize,
    query_count: usize,
    mutation_count: usize,
    mutation_ratio: f64,
    baseline_build_ms: f64,
    fresh_generation_build_ms: f64,
    fresh_generation_vectors_per_second: f64,
    rebuild_to_baseline_ratio: f64,
    quality: QualityMetrics,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    benchmark: &'static str,
    backend: &'static str,
    oracle: &'static str,
    distribution: &'static str,
    lifecycle_policy: &'static str,
    requested_sizes: Vec<usize>,
    requested_dimensions: Vec<usize>,
    mutation_basis_points: usize,
    measurements: Vec<Measurement>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = parse_list("OK_ANN_UPDATE_SIZES", DEFAULT_SIZES)?;
    let dimensions = parse_list("OK_ANN_UPDATE_DIMS", DEFAULT_DIMS)?;
    let query_count = parse_usize("OK_ANN_UPDATE_QUERIES", DEFAULT_QUERY_COUNT)?;
    let mutation_bps = parse_usize("OK_ANN_UPDATE_MUTATION_BPS", DEFAULT_MUTATION_BPS)?;

    validate_nonzero("OK_ANN_UPDATE_SIZES", &sizes)?;
    validate_nonzero("OK_ANN_UPDATE_DIMS", &dimensions)?;
    if query_count == 0 {
        return Err("OK_ANN_UPDATE_QUERIES must be greater than zero".into());
    }
    if mutation_bps == 0 || mutation_bps > 10_000 {
        return Err("OK_ANN_UPDATE_MUTATION_BPS must be in 1..=10000".into());
    }

    let mut measurements = Vec::new();
    for &dims in &dimensions {
        for &size in &sizes {
            measurements.push(measure(size, dims, query_count, mutation_bps)?);
        }
    }

    let report = Report {
        schema_version: 1,
        benchmark: "cc5-ann-fresh-generation-update",
        backend: "usearch-hnsw-f32",
        oracle: "exact-flat",
        distribution: "deterministic-clustered-synthetic-v1",
        lifecycle_policy:
            "fresh generation from authoritative live vectors; no in-place tombstone compaction",
        requested_sizes: sizes,
        requested_dimensions: dimensions,
        mutation_basis_points: mutation_bps,
        measurements,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn measure(
    vector_count: usize,
    dimensions: usize,
    query_count: usize,
    mutation_bps: usize,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let baseline_started = Instant::now();
    let mut baseline = UsearchHnswVectorIndex::with_parameters(
        dimensions,
        AnnScalarKind::F32,
        vector_count,
        PRODUCTION_HNSW_PARAMETERS,
    )?;
    for id in 0..vector_count {
        baseline.add(record(id, dimensions, false))?;
    }
    let baseline_build_ms = elapsed_ms(baseline_started);
    drop(baseline);

    let effective_queries = query_count.min(vector_count);
    let mut exact = ExactFlatVectorIndex::new(dimensions)?;
    let mut mutation_count = 0usize;
    for id in 0..vector_count {
        let mutated = is_mutated(id, mutation_bps);
        mutation_count += usize::from(mutated);
        exact.add(record(id, dimensions, mutated))?;
    }

    let queries = (0..effective_queries)
        .map(|index| {
            let target = query_target(index, vector_count);
            query_vector(target, dimensions, is_mutated(target, mutation_bps))
        })
        .collect::<Vec<_>>();
    let mut oracle_hits = Vec::with_capacity(effective_queries);
    for query in &queries {
        oracle_hits.push(exact.search(query, search_options())?);
    }
    drop(exact);

    let rebuild_started = Instant::now();
    let mut rebuilt = UsearchHnswVectorIndex::with_parameters(
        dimensions,
        AnnScalarKind::F32,
        vector_count,
        PRODUCTION_HNSW_PARAMETERS,
    )?;
    for id in 0..vector_count {
        rebuilt.add(record(id, dimensions, is_mutated(id, mutation_bps)))?;
    }
    let fresh_generation_build_ms = elapsed_ms(rebuild_started);

    let mut recall_at_1_sum = 0.0;
    let mut recall_at_5_sum = 0.0;
    let mut recall_at_10_sum = 0.0;
    let mut recall_at_20_sum = 0.0;
    let mut reciprocal_rank_sum = 0.0;
    for (query, oracle) in queries.iter().zip(&oracle_hits) {
        let hits = rebuilt.search(query, search_options())?;
        recall_at_1_sum += recall_at_k(oracle, &hits, 1);
        recall_at_5_sum += recall_at_k(oracle, &hits, 5);
        recall_at_10_sum += recall_at_k(oracle, &hits, 10);
        recall_at_20_sum += recall_at_k(oracle, &hits, 20);
        reciprocal_rank_sum += reciprocal_rank(oracle, &hits);
    }

    let denominator = effective_queries.max(1) as f64;
    Ok(Measurement {
        vector_count,
        dimensions,
        query_count: effective_queries,
        mutation_count,
        mutation_ratio: mutation_count as f64 / vector_count.max(1) as f64,
        baseline_build_ms,
        fresh_generation_build_ms,
        fresh_generation_vectors_per_second: vector_count as f64
            / (fresh_generation_build_ms / 1_000.0).max(f64::EPSILON),
        rebuild_to_baseline_ratio: fresh_generation_build_ms / baseline_build_ms.max(f64::EPSILON),
        quality: QualityMetrics {
            recall_at_1: recall_at_1_sum / denominator,
            recall_at_5: recall_at_5_sum / denominator,
            recall_at_10: recall_at_10_sum / denominator,
            recall_at_20: recall_at_20_sum / denominator,
            mrr: reciprocal_rank_sum / denominator,
        },
    })
}

fn record(id: usize, dimensions: usize, mutated: bool) -> VectorRecord {
    VectorRecord {
        id: VectorId((id + 1) as u64),
        target_id: format!("synthetic-{id}"),
        target_kind: if id % 5 == 0 { "doc" } else { "code" }.into(),
        vector: clustered_unit_vector(id, dimensions, mutated),
    }
}

fn is_mutated(id: usize, mutation_bps: usize) -> bool {
    let bucket = xorshift64((id as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)) % 10_000;
    bucket < mutation_bps as u64
}

fn query_target(query_index: usize, vector_count: usize) -> usize {
    ((query_index as u64 * 1_000_003 + 97) % vector_count.max(1) as u64) as usize
}

fn query_vector(target: usize, dimensions: usize, mutated: bool) -> Vec<f32> {
    let mut vector = clustered_unit_vector(target, dimensions, mutated);
    let mut noise = target as u64 ^ 0x517c_c1b7_2722_0a95;
    for value in &mut vector {
        noise = xorshift64(noise);
        let jitter = ((noise >> 40) as i32 - 32_768) as f32 / 32_768.0;
        *value += jitter * 0.0025;
    }
    normalize(&mut vector);
    vector
}

fn clustered_unit_vector(id: usize, dimensions: usize, mutated: bool) -> Vec<f32> {
    let cluster = (id % 256) as u64 + 1;
    let mut centroid =
        deterministic_vector(cluster.wrapping_mul(0xa24b_aed4_963e_e407), dimensions);
    let member_seed = (id as u64 + 1).wrapping_mul(if mutated {
        0x94d0_49bb_1331_11eb
    } else {
        0xd134_2543_de82_ef95
    });
    let member = deterministic_vector(member_seed, dimensions);
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

fn parse_list(name: &str, default: &[usize]) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default.to_vec());
    };
    let parsed = raw
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().parse::<usize>())
        .collect::<Result<Vec<_>, _>>()?;
    if parsed.is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(parsed)
}

fn parse_usize(name: &str, default: usize) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(std::env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(default))
}

fn validate_nonzero(name: &str, values: &[usize]) -> Result<(), Box<dyn std::error::Error>> {
    if values.iter().any(|value| *value == 0) {
        return Err(format!("{name} values must be greater than zero").into());
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}
