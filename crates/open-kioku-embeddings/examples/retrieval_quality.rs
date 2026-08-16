use open_kioku_embeddings::{
    neural_model_cache_dir, EmbeddingProvider, FastEmbedEmbeddingProvider,
    LocalHashEmbeddingProvider, LocalNeuralModel,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Deserialize)]
struct RetrievalSuite {
    cases: Vec<RetrievalCase>,
}

#[derive(Debug, Deserialize)]
struct RetrievalCase {
    query: String,
    repo_fixture: String,
    #[serde(default)]
    gold_files: Vec<String>,
}

#[derive(Debug)]
struct Document {
    path: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct HostProfile {
    os: String,
    arch: String,
    logical_cpus: usize,
    total_memory_bytes: Option<u64>,
    cpu_model: Option<String>,
    profile_label: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProviderResult {
    label: String,
    provider: String,
    model: String,
    dimensions: usize,
    evaluated_cases: usize,
    recall_at_5: f64,
    recall_at_10: f64,
    mean_reciprocal_rank: f64,
    index_build_ms: f64,
    query_mean_ms: f64,
    query_p95_ms: f64,
    vector_bytes: usize,
    model_cache_bytes: u64,
    process_peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    schema_version: u32,
    corpus: String,
    host: HostProfile,
    providers: Vec<ProviderResult>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = std::env::current_dir()?;
    let suite_path = repo.join("benchmarks/retrieval-cases.json");
    let suite: RetrievalSuite = serde_json::from_slice(&fs::read(&suite_path)?)?;
    let fixture_rel = suite
        .cases
        .first()
        .map(|case| case.repo_fixture.clone())
        .ok_or("retrieval benchmark suite has no cases")?;
    if suite
        .cases
        .iter()
        .any(|case| case.repo_fixture != fixture_rel)
    {
        return Err("retrieval quality harness currently requires one shared fixture".into());
    }
    let fixture = repo.join(&fixture_rel);
    let documents = collect_documents(&fixture)?;
    if documents.is_empty() {
        return Err("retrieval fixture contains no documents".into());
    }

    let cache_root = std::env::var_os("OK_CC5_MODEL_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join(".ok/bench-models"));
    fs::create_dir_all(&cache_root)?;
    let only = std::env::var("OK_CC5_BENCH_ONLY").unwrap_or_else(|_| "all".into());

    let mut providers = Vec::new();
    if selected(&only, "local-hash-384") {
        providers.push(benchmark_provider(
            "local-hash-384",
            Box::new(LocalHashEmbeddingProvider::new(384)?),
            &suite.cases,
            &documents,
            0,
        )?);
    }

    let neural_profiles = [
        (
            "qwen3-embedding-0.6b-768",
            LocalNeuralModel::Qwen3Embedding06B,
            768usize,
        ),
        (
            "jina-embeddings-v2-base-code-768",
            LocalNeuralModel::JinaEmbeddingsV2BaseCode,
            768usize,
        ),
    ];
    let neural_selected = neural_profiles
        .iter()
        .any(|(label, _, _)| selected(&only, label));
    if neural_selected && std::env::var("OK_CC5_ALLOW_MODEL_DOWNLOAD").as_deref() != Ok("1") {
        return Err(
            "neural benchmark requires explicit OK_CC5_ALLOW_MODEL_DOWNLOAD=1; no model download was attempted"
                .into(),
        );
    }

    for (label, model, dimensions) in neural_profiles {
        if !selected(&only, label) {
            continue;
        }
        let cache = neural_model_cache_dir(&cache_root, model);
        let provider = FastEmbedEmbeddingProvider::new(model, dimensions, 16, &cache)?;
        let result = benchmark_provider(
            label,
            Box::new(provider),
            &suite.cases,
            &documents,
            dir_size(&cache),
        )?;
        providers.push(result);
    }

    if providers.is_empty() {
        return Err(format!(
            "OK_CC5_BENCH_ONLY={only:?} selected no provider; use all, local-hash-384, qwen3-embedding-0.6b-768, or jina-embeddings-v2-base-code-768"
        )
        .into());
    }

    let report = BenchmarkReport {
        schema_version: 3,
        corpus: fixture_rel,
        host: host_profile(),
        providers,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn selected(filter: &str, label: &str) -> bool {
    filter == "all" || filter == label
}

fn benchmark_provider(
    label: &str,
    provider: Box<dyn EmbeddingProvider>,
    cases: &[RetrievalCase],
    documents: &[Document],
    model_cache_bytes: u64,
) -> Result<ProviderResult, Box<dyn std::error::Error>> {
    let descriptor = provider.descriptor();
    let document_inputs = documents
        .iter()
        .map(|document| document.content.clone())
        .collect::<Vec<_>>();
    let build_started = Instant::now();
    let document_vectors = provider.embed_document_batch(&document_inputs, 16)?;
    let index_build_ms = build_started.elapsed().as_secs_f64() * 1_000.0;
    if document_vectors.len() != documents.len() {
        return Err(format!(
            "provider {label} returned {} document vectors for {} documents",
            document_vectors.len(),
            documents.len()
        )
        .into());
    }

    let evaluable = cases
        .iter()
        .filter(|case| !case.gold_files.is_empty())
        .collect::<Vec<_>>();
    let mut recall5 = 0.0;
    let mut recall10 = 0.0;
    let mut mrr = 0.0;
    let mut query_latencies_ms = Vec::with_capacity(evaluable.len());

    for case in &evaluable {
        let query_started = Instant::now();
        let query = provider.embed_query(&case.query)?;
        let mut scored = documents
            .iter()
            .zip(document_vectors.iter())
            .map(|(document, vector)| (document.path.as_str(), dot(&query, vector)))
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        query_latencies_ms.push(query_started.elapsed().as_secs_f64() * 1_000.0);

        let gold = case
            .gold_files
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let hits5 = scored
            .iter()
            .take(5)
            .filter(|(path, _)| gold.contains(path))
            .count();
        let hits10 = scored
            .iter()
            .take(10)
            .filter(|(path, _)| gold.contains(path))
            .count();
        recall5 += hits5 as f64 / gold.len() as f64;
        recall10 += hits10 as f64 / gold.len() as f64;
        if let Some(rank) = scored
            .iter()
            .position(|(path, _)| gold.contains(path))
            .map(|index| index + 1)
        {
            mrr += 1.0 / rank as f64;
        }
    }
    let count = evaluable.len().max(1) as f64;
    let query_mean_ms = query_latencies_ms.iter().sum::<f64>() / count;
    let query_p95_ms = percentile(&query_latencies_ms, 0.95);

    Ok(ProviderResult {
        label: label.into(),
        provider: descriptor.provider,
        model: descriptor.model,
        dimensions: descriptor.dimensions,
        evaluated_cases: evaluable.len(),
        recall_at_5: recall5 / count,
        recall_at_10: recall10 / count,
        mean_reciprocal_rank: mrr / count,
        index_build_ms,
        query_mean_ms,
        query_p95_ms,
        vector_bytes: documents.len() * descriptor.dimensions * std::mem::size_of::<f32>(),
        model_cache_bytes,
        process_peak_rss_bytes: process_peak_rss_bytes(),
    })
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

fn collect_documents(root: &Path) -> Result<Vec<Document>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_paths(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn collect_paths(
    root: &Path,
    current: &Path,
    documents: &mut Vec<Document>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_paths(root, &path, documents)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        documents.push(Document {
            path: relative,
            content,
        });
    }
    Ok(())
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn dir_size(path: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_symlink() || metadata.is_file() {
            *total = total.saturating_add(metadata.len());
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(&entry.path(), total);
        }
    }
    let mut total = 0;
    walk(path, &mut total);
    total
}
