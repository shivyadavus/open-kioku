use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, UsearchHnswVectorIndex, VectorHit, VectorId, VectorRecord,
    VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

const DEFAULT_INITIAL_VECTORS: usize = 2_000;
const DEFAULT_DIMENSIONS: usize = 384;
const DEFAULT_CYCLES: usize = 20;
const DEFAULT_MUTATIONS_PER_CYCLE: usize = 100;
const DEFAULT_QUERIES: usize = 8;
const MAX_K: usize = 10;

// Policy v1 is anchored to the existing bounded 2,000 x 384d GitHub-hosted ANN scale
// measurement, which observed Recall@10=1.0, about 5,000 vectors/s build throughput,
// and ANN p95 below exact-flat. These margins deliberately leave substantial headroom.
const DEFAULT_MIN_RECALL_AT_10: f64 = 0.99;
const DEFAULT_MIN_MRR: f64 = 0.99;
const DEFAULT_MIN_BUILD_VECTORS_PER_SECOND: f64 = 2_000.0;
const DEFAULT_MAX_ANN_TO_EXACT_P95_RATIO: f64 = 1.0;

#[derive(Debug, Clone)]
struct LiveVector {
    id: usize,
    content_generation: usize,
    path_generation: usize,
}

impl LiveVector {
    fn record(&self, dimensions: usize) -> VectorRecord {
        VectorRecord {
            id: VectorId((self.id + 1) as u64),
            target_id: self.target_id(),
            target_kind: "code".into(),
            vector: content_vector(self.id, self.content_generation, dimensions),
        }
    }

    fn target_id(&self) -> String {
        format!("src/churn/{}/path-{}", self.id, self.path_generation)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct MutationCounts {
    added: usize,
    updated: usize,
    renamed: usize,
    deleted: usize,
}

#[derive(Debug, Clone, Serialize)]
struct QualityMetrics {
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
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
struct CycleMeasurement {
    cycle: usize,
    operations: usize,
    mutations: MutationCounts,
    live_vectors: usize,
    stored_vectors: usize,
    stored_to_live_ratio: f64,
    stale_or_deleted_hits: usize,
    stale_identity_hits: usize,
    duplicate_hit_ids: usize,
    quality: QualityMetrics,
    mutation_apply_ms: f64,
    exact_build_ms: f64,
    ann_build_ms: f64,
    ann_build_vectors_per_second: f64,
    exact_query: LatencyMetrics,
    ann_query: LatencyMetrics,
    ann_to_exact_p95_ratio: f64,
    ann_reload_ms: f64,
    ann_index_bytes: u64,
    ann_metadata_bytes: u64,
    ann_memory_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyThresholds {
    min_recall_at_10: f64,
    min_mrr: f64,
    min_ann_build_vectors_per_second: f64,
    max_ann_to_exact_p95_ratio: f64,
    max_stale_or_deleted_ratio: f64,
    required_stored_to_live_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyDecision {
    strategy: &'static str,
    compaction_policy: &'static str,
    rebuild_trigger: &'static str,
    thresholds: PolicyThresholds,
    passed: bool,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct OverallMetrics {
    total_mutations: usize,
    min_recall_at_10: f64,
    min_mrr: f64,
    min_ann_build_vectors_per_second: f64,
    max_ann_to_exact_p95_ratio: f64,
    max_stale_or_deleted_ratio: f64,
    max_stored_to_live_ratio: f64,
    ann_build_p50_ms: f64,
    ann_build_p95_ms: f64,
    ann_build_p99_ms: f64,
    ann_query_p95_us: f64,
    max_index_bytes: u64,
    max_memory_bytes: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    benchmark: &'static str,
    backend: &'static str,
    oracle: &'static str,
    lifecycle_model: &'static str,
    dimensions: usize,
    initial_vectors: usize,
    cycles: usize,
    mutations_per_cycle: usize,
    query_count_per_cycle: usize,
    measurements: Vec<CycleMeasurement>,
    overall: OverallMetrics,
    policy: PolicyDecision,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let initial_vectors = parse_usize("OK_ANN_CHURN_INITIAL_VECTORS", DEFAULT_INITIAL_VECTORS)?;
    let dimensions = parse_usize("OK_ANN_CHURN_DIMS", DEFAULT_DIMENSIONS)?;
    let cycles = parse_usize("OK_ANN_CHURN_CYCLES", DEFAULT_CYCLES)?;
    let mutations_per_cycle = parse_usize(
        "OK_ANN_CHURN_MUTATIONS_PER_CYCLE",
        DEFAULT_MUTATIONS_PER_CYCLE,
    )?;
    let query_count = parse_usize("OK_ANN_CHURN_QUERIES", DEFAULT_QUERIES)?;
    let thresholds = PolicyThresholds {
        min_recall_at_10: parse_f64("OK_ANN_CHURN_MIN_RECALL_AT_10", DEFAULT_MIN_RECALL_AT_10)?,
        min_mrr: parse_f64("OK_ANN_CHURN_MIN_MRR", DEFAULT_MIN_MRR)?,
        min_ann_build_vectors_per_second: parse_f64(
            "OK_ANN_CHURN_MIN_BUILD_VECTORS_PER_SECOND",
            DEFAULT_MIN_BUILD_VECTORS_PER_SECOND,
        )?,
        max_ann_to_exact_p95_ratio: parse_f64(
            "OK_ANN_CHURN_MAX_ANN_TO_EXACT_P95_RATIO",
            DEFAULT_MAX_ANN_TO_EXACT_P95_RATIO,
        )?,
        max_stale_or_deleted_ratio: 0.0,
        required_stored_to_live_ratio: 1.0,
    };

    if initial_vectors == 0
        || dimensions == 0
        || cycles == 0
        || mutations_per_cycle == 0
        || query_count == 0
    {
        return Err("ANN churn benchmark inputs must all be positive".into());
    }

    let mut live = (0..initial_vectors)
        .map(|id| {
            (
                id,
                LiveVector {
                    id,
                    content_generation: 0,
                    path_generation: 0,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut next_id = initial_vectors;
    let mut measurements = Vec::with_capacity(cycles);

    for cycle in 1..=cycles {
        let mutation_started = Instant::now();
        let mutations = apply_mutations(
            &mut live,
            &mut next_id,
            cycle,
            mutations_per_cycle,
            initial_vectors,
        );
        let mutation_apply_ms = elapsed_ms(mutation_started);
        measurements.push(measure_cycle(
            cycle,
            mutations_per_cycle,
            mutations,
            &live,
            dimensions,
            query_count,
            mutation_apply_ms,
        )?);
    }

    let overall = overall_metrics(&measurements);
    let policy = policy_decision(&overall, &thresholds);
    let passed = policy.passed;
    let report = Report {
        schema_version: 1,
        benchmark: "cc5-ann-churn-matrix",
        backend: "usearch-hnsw-f32",
        oracle: "exact-flat",
        lifecycle_model: "fresh-generation-from-live-set",
        dimensions,
        initial_vectors,
        cycles,
        mutations_per_cycle,
        query_count_per_cycle: query_count,
        measurements,
        overall,
        policy,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !passed {
        return Err("ANN churn policy gate failed; inspect the emitted report".into());
    }
    Ok(())
}

fn apply_mutations(
    live: &mut BTreeMap<usize, LiveVector>,
    next_id: &mut usize,
    cycle: usize,
    count: usize,
    initial_vectors: usize,
) -> MutationCounts {
    let mut mutations = MutationCounts::default();
    let minimum_live = (initial_vectors / 2).max(1);
    for operation in 0..count {
        match operation % 4 {
            0 => {
                let id = *next_id;
                *next_id += 1;
                live.insert(
                    id,
                    LiveVector {
                        id,
                        content_generation: cycle,
                        path_generation: 0,
                    },
                );
                mutations.added += 1;
            }
            1 => {
                if let Some(id) = selected_id(live, cycle, operation, operation % 20 == 1) {
                    if let Some(entry) = live.get_mut(&id) {
                        entry.content_generation += 1;
                        mutations.updated += 1;
                    }
                }
            }
            2 => {
                if let Some(id) = selected_id(live, cycle, operation, false) {
                    if let Some(entry) = live.get_mut(&id) {
                        entry.path_generation += 1;
                        mutations.renamed += 1;
                    }
                }
            }
            _ => {
                if live.len() > minimum_live {
                    if let Some(id) = selected_id(live, cycle, operation, false) {
                        live.remove(&id);
                        mutations.deleted += 1;
                    }
                }
            }
        }
    }
    mutations
}

fn selected_id(
    live: &BTreeMap<usize, LiveVector>,
    cycle: usize,
    operation: usize,
    hot_rewrite: bool,
) -> Option<usize> {
    if live.is_empty() {
        return None;
    }
    if hot_rewrite {
        return live.keys().next().copied();
    }
    let index = (cycle.wrapping_mul(1_009) + operation.wrapping_mul(97)) % live.len();
    live.keys().nth(index).copied()
}

fn measure_cycle(
    cycle: usize,
    operations: usize,
    mutations: MutationCounts,
    live: &BTreeMap<usize, LiveVector>,
    dimensions: usize,
    query_count: usize,
    mutation_apply_ms: f64,
) -> Result<CycleMeasurement, Box<dyn std::error::Error>> {
    let exact_started = Instant::now();
    let mut exact = ExactFlatVectorIndex::new(dimensions)?;
    for entry in live.values() {
        exact.add(entry.record(dimensions))?;
    }
    let exact_build_ms = elapsed_ms(exact_started);

    let ann_started = Instant::now();
    let mut ann = UsearchHnswVectorIndex::with_parameters(
        dimensions,
        AnnScalarKind::F32,
        live.len(),
        PRODUCTION_HNSW_PARAMETERS,
    )?;
    for entry in live.values() {
        ann.add(entry.record(dimensions))?;
    }
    let ann_build_ms = elapsed_ms(ann_started);
    let ann_build_vectors_per_second =
        live.len() as f64 / (ann_build_ms / 1_000.0).max(f64::EPSILON);

    let temp = tempfile::tempdir()?;
    let ann_path = temp.path().join("ann.usearch");
    ann.save(&ann_path)?;
    let ann_index_bytes = fs::metadata(&ann_path)?.len();
    let ann_metadata_bytes = fs::metadata(metadata_path(&ann_path))?.len();
    drop(ann);

    let reload_started = Instant::now();
    let loaded = UsearchHnswVectorIndex::load(&ann_path)?;
    let ann_reload_ms = elapsed_ms(reload_started);
    let stored_vectors = loaded.stats().vector_count;

    let current_targets = live
        .values()
        .map(|entry| (VectorId((entry.id + 1) as u64), entry.target_id()))
        .collect::<BTreeMap<_, _>>();
    let query_entries = sample_entries(live, query_count, cycle);
    let mut exact_latencies = Vec::with_capacity(query_entries.len());
    let mut ann_latencies = Vec::with_capacity(query_entries.len());
    let mut recall_at_1 = 0.0;
    let mut recall_at_5 = 0.0;
    let mut recall_at_10 = 0.0;
    let mut reciprocal_rank = 0.0;
    let mut stale_or_deleted_hits = 0;
    let mut stale_identity_hits = 0;
    let mut duplicate_hit_ids = 0;

    for entry in query_entries {
        let query = query_vector(entry, dimensions);
        let exact_started = Instant::now();
        let oracle = exact.search(&query, search_options())?;
        exact_latencies.push(elapsed_us(exact_started));

        let ann_started = Instant::now();
        let hits = loaded.search(&query, search_options())?;
        ann_latencies.push(elapsed_us(ann_started));

        recall_at_1 += recall_at_k(&oracle, &hits, 1);
        recall_at_5 += recall_at_k(&oracle, &hits, 5);
        recall_at_10 += recall_at_k(&oracle, &hits, 10);
        reciprocal_rank += reciprocal_rank_for_top(&oracle, &hits);

        let mut seen = HashSet::new();
        for hit in &hits {
            if !seen.insert(hit.id) {
                duplicate_hit_ids += 1;
            }
            match current_targets.get(&hit.id) {
                Some(target_id) if target_id == &hit.target_id => {}
                Some(_) => stale_identity_hits += 1,
                None => stale_or_deleted_hits += 1,
            }
        }
    }

    let denominator = query_count.min(live.len()).max(1) as f64;
    let exact_query = latency_metrics(&exact_latencies);
    let ann_query = latency_metrics(&ann_latencies);
    let ann_to_exact_p95_ratio = ann_query.p95_us / exact_query.p95_us.max(f64::EPSILON);

    Ok(CycleMeasurement {
        cycle,
        operations,
        mutations,
        live_vectors: live.len(),
        stored_vectors,
        stored_to_live_ratio: stored_vectors as f64 / live.len().max(1) as f64,
        stale_or_deleted_hits,
        stale_identity_hits,
        duplicate_hit_ids,
        quality: QualityMetrics {
            recall_at_1: recall_at_1 / denominator,
            recall_at_5: recall_at_5 / denominator,
            recall_at_10: recall_at_10 / denominator,
            mrr: reciprocal_rank / denominator,
        },
        mutation_apply_ms,
        exact_build_ms,
        ann_build_ms,
        ann_build_vectors_per_second,
        exact_query,
        ann_query,
        ann_to_exact_p95_ratio,
        ann_reload_ms,
        ann_index_bytes,
        ann_metadata_bytes,
        ann_memory_bytes: loaded.memory_usage_bytes(),
    })
}

fn sample_entries<'a>(
    live: &'a BTreeMap<usize, LiveVector>,
    query_count: usize,
    cycle: usize,
) -> Vec<&'a LiveVector> {
    let count = query_count.min(live.len());
    if count == 0 {
        return Vec::new();
    }
    (0..count)
        .filter_map(|index| {
            let selected = (cycle.wrapping_mul(131) + index.wrapping_mul(1_003)) % live.len();
            live.values().nth(selected)
        })
        .collect()
}

fn query_vector(entry: &LiveVector, dimensions: usize) -> Vec<f32> {
    let mut vector = content_vector(entry.id, entry.content_generation, dimensions);
    let mut noise = (entry.id as u64 + 1)
        ^ (entry.content_generation as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for value in &mut vector {
        noise = xorshift64(noise.max(1));
        let jitter = ((noise >> 40) as i32 - 32_768) as f32 / 32_768.0;
        *value += jitter * 0.0025;
    }
    normalize(&mut vector);
    vector
}

fn content_vector(id: usize, generation: usize, dimensions: usize) -> Vec<f32> {
    let cluster = (id % 256) as u64 + 1;
    let mut centroid =
        deterministic_vector(cluster.wrapping_mul(0xa24b_aed4_963e_e407), dimensions);
    let seed = (id as u64 + 1)
        .wrapping_mul(0xd134_2543_de82_ef95)
        .wrapping_add((generation as u64).wrapping_mul(0x94d0_49bb_1331_11eb));
    let member = deterministic_vector(seed, dimensions);
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

fn reciprocal_rank_for_top(exact: &[VectorHit], ann: &[VectorHit]) -> f64 {
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

fn overall_metrics(measurements: &[CycleMeasurement]) -> OverallMetrics {
    let build_ms = measurements
        .iter()
        .map(|measurement| measurement.ann_build_ms)
        .collect::<Vec<_>>();
    OverallMetrics {
        total_mutations: measurements
            .iter()
            .map(|measurement| measurement.operations)
            .sum(),
        min_recall_at_10: measurements
            .iter()
            .map(|measurement| measurement.quality.recall_at_10)
            .fold(1.0, f64::min),
        min_mrr: measurements
            .iter()
            .map(|measurement| measurement.quality.mrr)
            .fold(1.0, f64::min),
        min_ann_build_vectors_per_second: measurements
            .iter()
            .map(|measurement| measurement.ann_build_vectors_per_second)
            .fold(f64::INFINITY, f64::min),
        max_ann_to_exact_p95_ratio: measurements
            .iter()
            .map(|measurement| measurement.ann_to_exact_p95_ratio)
            .fold(0.0, f64::max),
        max_stale_or_deleted_ratio: measurements
            .iter()
            .map(|measurement| {
                let observed = measurement
                    .stale_or_deleted_hits
                    .saturating_add(measurement.stale_identity_hits);
                observed as f64 / (measurement.live_vectors.max(1) * MAX_K).max(1) as f64
            })
            .fold(0.0, f64::max),
        max_stored_to_live_ratio: measurements
            .iter()
            .map(|measurement| measurement.stored_to_live_ratio)
            .fold(0.0, f64::max),
        ann_build_p50_ms: percentile(&build_ms, 0.50),
        ann_build_p95_ms: percentile(&build_ms, 0.95),
        ann_build_p99_ms: percentile(&build_ms, 0.99),
        ann_query_p95_us: measurements
            .iter()
            .map(|measurement| measurement.ann_query.p95_us)
            .fold(0.0, f64::max),
        max_index_bytes: measurements
            .iter()
            .map(|measurement| measurement.ann_index_bytes)
            .max()
            .unwrap_or_default(),
        max_memory_bytes: measurements
            .iter()
            .map(|measurement| measurement.ann_memory_bytes)
            .max()
            .unwrap_or_default(),
    }
}

fn policy_decision(overall: &OverallMetrics, thresholds: &PolicyThresholds) -> PolicyDecision {
    let mut reasons = Vec::new();
    if overall.min_recall_at_10 < thresholds.min_recall_at_10 {
        reasons.push(format!(
            "Recall@10 {:.4} fell below {:.4}",
            overall.min_recall_at_10, thresholds.min_recall_at_10
        ));
    }
    if overall.min_mrr < thresholds.min_mrr {
        reasons.push(format!(
            "MRR {:.4} fell below {:.4}",
            overall.min_mrr, thresholds.min_mrr
        ));
    }
    if overall.min_ann_build_vectors_per_second < thresholds.min_ann_build_vectors_per_second {
        reasons.push(format!(
            "ANN rebuild throughput {:.1} vectors/s fell below {:.1}",
            overall.min_ann_build_vectors_per_second, thresholds.min_ann_build_vectors_per_second
        ));
    }
    if overall.max_ann_to_exact_p95_ratio > thresholds.max_ann_to_exact_p95_ratio {
        reasons.push(format!(
            "ANN/exact p95 ratio {:.3} exceeded {:.3}",
            overall.max_ann_to_exact_p95_ratio, thresholds.max_ann_to_exact_p95_ratio
        ));
    }
    if overall.max_stale_or_deleted_ratio > thresholds.max_stale_or_deleted_ratio {
        reasons.push(format!(
            "stale/deleted ratio {:.6} exceeded {:.6}",
            overall.max_stale_or_deleted_ratio, thresholds.max_stale_or_deleted_ratio
        ));
    }
    if (overall.max_stored_to_live_ratio - thresholds.required_stored_to_live_ratio).abs() > 1e-9 {
        reasons.push(format!(
            "stored/live ratio {:.6} did not equal required {:.6}",
            overall.max_stored_to_live_ratio, thresholds.required_stored_to_live_ratio
        ));
    }
    if reasons.is_empty() {
        reasons.push(
            "fresh-generation rebuild preserved exact-oracle quality, zero stale entries, and measured latency/build bounds"
                .into(),
        );
    }
    PolicyDecision {
        strategy: "rebuild a staged ANN generation from the authoritative live set",
        compaction_policy: "no separate compaction: successful publication has stored/live=1 and stale/deleted=0",
        rebuild_trigger: "authoritative semantic source generation changed or artifact/profile health is incompatible",
        thresholds: thresholds.clone(),
        passed: reasons.len() == 1 && reasons[0].starts_with("fresh-generation rebuild"),
        reasons,
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

fn metadata_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("meta.json")
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

fn parse_f64(name: &str, default: f64) -> Result<f64, Box<dyn std::error::Error>> {
    Ok(std::env::var(name)
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(default))
}
