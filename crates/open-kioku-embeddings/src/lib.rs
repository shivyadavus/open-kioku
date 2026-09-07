use candle_core::{DType, Device};
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, Qwen3TextEmbedding, TextEmbedding,
    TextInitOptions, TokenizerFiles, UserDefinedEmbeddingModel,
};
use open_kioku_errors::{OkError, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const FASTEMBED_PROVIDER_VERSION: &str = "fastembed-5.17.4";
/// The candle Qwen3 path materializes a `(batch, 1, seq, seq)` f32 attention mask padded to the
/// longest text in the batch; at the model's 32k/8k context that is tens of GB for a batch of
/// whole-symbol chunks (the first CI runs were OOM-killed). 2048 tokens covers the path/symbol
/// header plus the body of any chunk that matters for file-level retrieval.
pub const QWEN3_MAX_LENGTH: usize = 2_048;
const QWEN3_MAX_BATCH: usize = 8;
const QWEN3_QUERY_INSTRUCTION: &str = "Given a code search query, retrieve relevant code and documentation passages that help implement, explain, debug, or verify the requested change.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingProviderDescriptor {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub native_dimensions: usize,
    pub implementation: String,
}

pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, input: &str) -> Result<Vec<f32>> {
        self.embed_document(input)
    }

    fn embed_query(&self, input: &str) -> Result<Vec<f32>> {
        self.embed_document(input)
    }

    fn embed_document(&self, input: &str) -> Result<Vec<f32>>;

    fn embed_document_batch(&self, inputs: &[String], _batch_size: usize) -> Result<Vec<Vec<f32>>> {
        inputs
            .iter()
            .map(|input| self.embed_document(input))
            .collect()
    }

    fn descriptor(&self) -> EmbeddingProviderDescriptor;
}

#[derive(Debug, Clone)]
pub struct LocalHashEmbeddingProvider {
    dimensions: usize,
}

impl LocalHashEmbeddingProvider {
    pub fn new(dimensions: usize) -> Result<Self> {
        if dimensions == 0 {
            return Err(OkError::Unsupported(
                "local hash embeddings require at least one dimension".into(),
            ));
        }
        Ok(Self { dimensions })
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }
}

impl Default for LocalHashEmbeddingProvider {
    fn default() -> Self {
        Self { dimensions: 384 }
    }
}

impl EmbeddingProvider for LocalHashEmbeddingProvider {
    fn embed_document(&self, input: &str) -> Result<Vec<f32>> {
        let mut vector = vec![0.0; self.dimensions];
        for token in tokenize(input) {
            let hash = stable_hash(&token);
            let index = (hash as usize) % self.dimensions;
            let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
            vector[index] += sign;
        }
        normalize(&mut vector);
        Ok(vector)
    }

    fn descriptor(&self) -> EmbeddingProviderDescriptor {
        EmbeddingProviderDescriptor {
            provider: "local".into(),
            model: "local-hash".into(),
            dimensions: self.dimensions,
            native_dimensions: self.dimensions,
            implementation: "open-kioku-local-hash-v1".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalNeuralModel {
    Qwen3Embedding06B,
    Qwen3Embedding4B,
    Qwen3Embedding8B,
    JinaEmbeddingsV2BaseCode,
    /// Alibaba-NLP/gte-modernbert-base: 149M parameters, Apache-2.0, CoIR 79.3 — the same size
    /// class as the Jina code model with code-retrieval quality in the range of 2B-7B models.
    /// Loaded as a user-defined ONNX model (int8 export, CLS pooling) because fastembed has no
    /// built-in entry for it.
    GteModernBertBase,
}

const GTE_MODERNBERT_REPO: &str = "Alibaba-NLP/gte-modernbert-base";
const GTE_MODERNBERT_ONNX: &str = "onnx/model_int8.onnx";
/// Pinned Hugging Face revision. Upstream `main` can be re-exported or deleted; a pinned commit
/// plus per-file digests means the model either loads bit-identically or fails loudly.
const GTE_MODERNBERT_REVISION: &str = "e7f32e3c00f91d699e8c43b53106206bcc72bb22";
/// SHA-256 of every file fetched at that revision (name, digest).
const GTE_MODERNBERT_FILES: &[(&str, &str)] = &[
    (
        GTE_MODERNBERT_ONNX,
        "bae96b276d342bf86eeee07c1bdbc0c75bb82bf4033941aab7fabc1e33ee3b44",
    ),
    (
        "tokenizer.json",
        "6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30",
    ),
    (
        "config.json",
        "8ba54dc3d35d7194f5178a4194b649f146753e02dabd22bdca5c5cbac15069ed",
    ),
    (
        "special_tokens_map.json",
        "ea97ecdbcc73713039d8d64dbb05e3689495c96657fbd9a18f5bed381be81049",
    ),
    (
        "tokenizer_config.json",
        "9654072f7c873161814043cf08cb5ed72f71d0b935abcd4e267935cb34352c21",
    ),
];
/// ModernBERT's ONNX export runs full O(n^2) attention with no memory-efficient kernel, so
/// the model's 8k context is not a usable embedding length: at 8192 tokens x batch 64 the
/// attention scores alone asked ORT for 55 GB. Chunks are far shorter than 1024 tokens.
const GTE_MODERNBERT_MAX_LENGTH: usize = 1_024;
const GTE_MODERNBERT_MAX_BATCH: usize = 8;

impl LocalNeuralModel {
    /// The neural profile a configuration gets when it names none. Chosen on commit-derived
    /// corpora (2026-09-07, hosted 4-core runners): gte-modernbert-base beat jina-v2-code on
    /// every metric on hugo and deno_std (holdout MRR +0.025/+0.027 vs +0.007/+0.005) at a
    /// third of the peak memory, and Qwen3-0.6B on CPU could not index an 18k-chunk repository
    /// inside three hours where gte took 25 minutes. Apache-2.0, pinned by revision and digest.
    pub const DEFAULT: Self = Self::GteModernBertBase;

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "default" => Ok(Self::DEFAULT),
            "qwen3-embedding-0.6b" | "Qwen/Qwen3-Embedding-0.6B" => {
                Ok(Self::Qwen3Embedding06B)
            }
            "qwen3-embedding-4b" | "Qwen/Qwen3-Embedding-4B" => Ok(Self::Qwen3Embedding4B),
            "qwen3-embedding-8b" | "Qwen/Qwen3-Embedding-8B" => Ok(Self::Qwen3Embedding8B),
            "jina-embeddings-v2-base-code" | "jina-v2-base-code"
            | "jinaai/jina-embeddings-v2-base-code" => Ok(Self::JinaEmbeddingsV2BaseCode),
            "gte-modernbert-base" | "Alibaba-NLP/gte-modernbert-base" => {
                Ok(Self::GteModernBertBase)
            }
            other => Err(OkError::Unsupported(format!(
                "local neural embedding model `{other}` is unsupported; supported models: Alibaba-NLP/gte-modernbert-base (default), jinaai/jina-embeddings-v2-base-code, Qwen/Qwen3-Embedding-0.6B, Qwen/Qwen3-Embedding-4B, Qwen/Qwen3-Embedding-8B"
            ))),
        }
    }

    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::Qwen3Embedding06B => "Qwen/Qwen3-Embedding-0.6B",
            Self::Qwen3Embedding4B => "Qwen/Qwen3-Embedding-4B",
            Self::Qwen3Embedding8B => "Qwen/Qwen3-Embedding-8B",
            Self::JinaEmbeddingsV2BaseCode => "jinaai/jina-embeddings-v2-base-code",
            Self::GteModernBertBase => GTE_MODERNBERT_REPO,
        }
    }

    pub fn native_dimensions(self) -> usize {
        match self {
            Self::Qwen3Embedding06B => 1_024,
            Self::Qwen3Embedding4B => 2_560,
            Self::Qwen3Embedding8B => 4_096,
            Self::JinaEmbeddingsV2BaseCode => 768,
            Self::GteModernBertBase => 768,
        }
    }

    pub fn supports_matryoshka(self) -> bool {
        self.is_qwen3()
    }

    fn validate_output_dimensions(self, dimensions: usize) -> Result<()> {
        let native = self.native_dimensions();
        if self.supports_matryoshka() {
            if !(256..=native).contains(&dimensions) {
                return Err(OkError::Unsupported(format!(
                    "Qwen3 model {} supports Open Kioku output dimensions from 256 through its native {native}; configured {dimensions}",
                    self.canonical_name()
                )));
            }
        } else if dimensions != native {
            return Err(OkError::Unsupported(format!(
                "model {} emits {native} dimensions and does not use Open Kioku Matryoshka truncation; configured {dimensions}",
                self.canonical_name()
            )));
        }
        Ok(())
    }

    fn is_qwen3(self) -> bool {
        matches!(
            self,
            Self::Qwen3Embedding06B | Self::Qwen3Embedding4B | Self::Qwen3Embedding8B
        )
    }
}

enum NeuralBackend {
    Qwen3(Mutex<Qwen3TextEmbedding>),
    /// fastembed ONNX text models: the built-in Jina code model and user-defined exports.
    Onnx(Mutex<TextEmbedding>),
}

pub struct FastEmbedEmbeddingProvider {
    model: LocalNeuralModel,
    output_dimensions: usize,
    batch_size: usize,
    backend: NeuralBackend,
}

impl FastEmbedEmbeddingProvider {
    pub fn new(
        model: LocalNeuralModel,
        output_dimensions: usize,
        batch_size: usize,
        cache_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        model.validate_output_dimensions(output_dimensions)?;
        if batch_size == 0 {
            return Err(OkError::Unsupported(
                "local neural embedding batch size must be greater than zero".into(),
            ));
        }
        let cache_dir = cache_dir.as_ref();
        std::fs::create_dir_all(cache_dir)?;
        let backend = if model.is_qwen3() {
            let inner = with_hf_home(cache_dir, || {
                Qwen3TextEmbedding::from_hf(
                    model.canonical_name(),
                    &Device::Cpu,
                    DType::F32,
                    QWEN3_MAX_LENGTH,
                )
                .map_err(|err| {
                    OkError::Unsupported(format!(
                        "failed to initialize local Qwen3 embedding model {}: {err}",
                        model.canonical_name()
                    ))
                })
            })?;
            NeuralBackend::Qwen3(Mutex::new(inner))
        } else if model == LocalNeuralModel::GteModernBertBase {
            let inner = load_gte_modernbert(cache_dir)?;
            NeuralBackend::Onnx(Mutex::new(inner))
        } else {
            let options = TextInitOptions::new(EmbeddingModel::JinaEmbeddingsV2BaseCode)
                .with_cache_dir(cache_dir.to_path_buf())
                .with_show_download_progress(false);
            let inner = TextEmbedding::try_new(options).map_err(|err| {
                OkError::Unsupported(format!(
                    "failed to initialize local code embedding model {}: {err}",
                    model.canonical_name()
                ))
            })?;
            NeuralBackend::Onnx(Mutex::new(inner))
        };
        Ok(Self {
            model,
            output_dimensions,
            batch_size,
            backend,
        })
    }

    fn embed_inputs(&self, inputs: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let vectors = match &self.backend {
            NeuralBackend::Qwen3(inner) => {
                let model = inner.lock().map_err(|_| {
                    OkError::Unsupported("Qwen3 embedding model lock poisoned".into())
                })?;
                // Batches are padded to their longest member, so group texts of similar length:
                // one long chunk in a batch of short ones would otherwise cost the whole batch
                // its O(seq^2) attention.
                let order = length_sorted_order(inputs);
                let mut vectors: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
                for batch in order.chunks(batch_size.clamp(1, QWEN3_MAX_BATCH)) {
                    let texts: Vec<&str> = batch.iter().map(|&i| inputs[i].as_str()).collect();
                    let embedded = model.embed(&texts).map_err(|err| {
                        OkError::Unsupported(format!("Qwen3 embedding inference failed: {err}"))
                    })?;
                    for (&index, vector) in batch.iter().zip(embedded) {
                        vectors[index] = Some(vector);
                    }
                }
                vectors
                    .into_iter()
                    .map(|vector| {
                        vector.ok_or_else(|| {
                            OkError::Unsupported(
                                "Qwen3 embedding returned fewer vectors than inputs".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            NeuralBackend::Onnx(inner) => {
                let mut model = inner.lock().map_err(|_| {
                    OkError::Unsupported("ONNX embedding model lock poisoned".into())
                })?;
                let batch = if self.model == LocalNeuralModel::GteModernBertBase {
                    batch_size.clamp(1, GTE_MODERNBERT_MAX_BATCH)
                } else {
                    batch_size.max(1)
                };
                // One batch at a time, longest texts first. fastembed would otherwise run every
                // batch of the whole input in parallel across cores, multiplying peak activation
                // memory by the core count, and ONNX Runtime's arena never shrinks: the padded
                // shape of each batch is cached, so descending length makes the first batch the
                // high-water mark instead of letting the arena grow for an hour and then die.
                let order = length_sorted_order(inputs);
                let mut vectors: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
                for group in order.chunks(batch) {
                    let texts: Vec<&str> = group.iter().map(|&i| inputs[i].as_str()).collect();
                    let embedded = model.embed(texts, Some(batch)).map_err(|err| {
                        OkError::Unsupported(format!("ONNX embedding inference failed: {err}"))
                    })?;
                    for (&index, vector) in group.iter().zip(embedded) {
                        vectors[index] = Some(vector);
                    }
                }
                vectors
                    .into_iter()
                    .map(|vector| {
                        vector.ok_or_else(|| {
                            OkError::Unsupported(
                                "ONNX embedding returned fewer vectors than inputs".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            }
        };
        vectors
            .into_iter()
            .map(|vector| reduce_dimensions(vector, self.output_dimensions))
            .collect()
    }
}

impl EmbeddingProvider for FastEmbedEmbeddingProvider {
    fn embed_query(&self, input: &str) -> Result<Vec<f32>> {
        let prepared = if self.model.is_qwen3() {
            format!("Instruct: {QWEN3_QUERY_INSTRUCTION}\nQuery:{input}")
        } else {
            input.to_string()
        };
        let mut vectors = self.embed_inputs(&[prepared], 1)?;
        vectors.pop().ok_or_else(|| {
            OkError::Unsupported("local neural embedding returned no query vector".into())
        })
    }

    fn embed_document(&self, input: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed_inputs(&[input.to_string()], 1)?;
        vectors.pop().ok_or_else(|| {
            OkError::Unsupported("local neural embedding returned no document vector".into())
        })
    }

    fn embed_document_batch(&self, inputs: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>> {
        self.embed_inputs(inputs, batch_size.min(self.batch_size).max(1))
    }

    fn descriptor(&self) -> EmbeddingProviderDescriptor {
        EmbeddingProviderDescriptor {
            provider: "fastembed".into(),
            model: self.model.canonical_name().into(),
            dimensions: self.output_dimensions,
            native_dimensions: self.model.native_dimensions(),
            implementation: if self.model.is_qwen3() {
                format!("{FASTEMBED_PROVIDER_VERSION}:qwen3-candle:maxlen-{QWEN3_MAX_LENGTH}")
            } else {
                match self.model {
                    LocalNeuralModel::GteModernBertBase => {
                        format!(
                            "{FASTEMBED_PROVIDER_VERSION}:{}",
                            gte_modernbert_implementation()
                        )
                    }
                    _ => format!("{FASTEMBED_PROVIDER_VERSION}:onnx"),
                }
            },
        }
    }
}

pub struct DisabledEmbeddingProvider;

impl EmbeddingProvider for DisabledEmbeddingProvider {
    fn embed_document(&self, _input: &str) -> Result<Vec<f32>> {
        Err(OkError::Unsupported(
            "embedding provider is not configured".into(),
        ))
    }

    fn descriptor(&self) -> EmbeddingProviderDescriptor {
        EmbeddingProviderDescriptor {
            provider: "disabled".into(),
            model: "disabled".into(),
            dimensions: 0,
            native_dimensions: 0,
            implementation: "disabled".into(),
        }
    }
}

/// Fetches (or reuses from `cache_dir`) the int8 ONNX export and tokenizer files of
/// gte-modernbert-base and builds a fastembed user-defined model with CLS pooling, which is the
/// pooling the upstream `1_Pooling/config.json` declares.
fn load_gte_modernbert(cache_dir: &Path) -> Result<TextEmbedding> {
    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(cache_dir.to_path_buf())
        .with_progress(false)
        .build()
        .map_err(|err| OkError::Unsupported(format!("Hugging Face hub client: {err}")))?;
    let repo = api.repo(hf_hub::Repo::with_revision(
        GTE_MODERNBERT_REPO.to_string(),
        hf_hub::RepoType::Model,
        GTE_MODERNBERT_REVISION.to_string(),
    ));
    let fetch = |name: &str| -> Result<Vec<u8>> {
        let path = repo.get(name).map_err(|err| {
            OkError::Unsupported(format!(
                "failed to fetch {GTE_MODERNBERT_REPO}@{GTE_MODERNBERT_REVISION}/{name}: {err}"
            ))
        })?;
        let bytes = std::fs::read(&path).map_err(|err| {
            OkError::Unsupported(format!("failed to read {}: {err}", path.display()))
        })?;
        verify_pinned_digest(name, &bytes)?;
        Ok(bytes)
    };
    let model = UserDefinedEmbeddingModel::new(
        fetch(GTE_MODERNBERT_ONNX)?,
        TokenizerFiles {
            tokenizer_file: fetch("tokenizer.json")?,
            config_file: fetch("config.json")?,
            special_tokens_map_file: fetch("special_tokens_map.json")?,
            tokenizer_config_file: fetch("tokenizer_config.json")?,
        },
    )
    .with_pooling(Pooling::Cls);
    let mut options = InitOptionsUserDefined::default();
    options.max_length = GTE_MODERNBERT_MAX_LENGTH;
    TextEmbedding::try_new_from_user_defined(model, options).map_err(|err| {
        OkError::Unsupported(format!(
            "failed to initialize local embedding model {GTE_MODERNBERT_REPO}: {err}"
        ))
    })
}

/// Implementation tag for gte-modernbert indexes: pooling, quantization, and the pinned
/// revision, so a re-pin invalidates existing vectors instead of mixing two models.
pub fn gte_modernbert_implementation() -> String {
    format!("onnx-int8-cls@{}", &GTE_MODERNBERT_REVISION[..12])
}

/// Refuses a model file whose SHA-256 differs from the pinned digest, so a re-exported or
/// tampered upstream file cannot silently change every ranking that depends on it.
fn verify_pinned_digest(name: &str, bytes: &[u8]) -> Result<()> {
    use sha2::Digest as _;
    let expected = GTE_MODERNBERT_FILES
        .iter()
        .find(|(file, _)| *file == name)
        .map(|(_, digest)| *digest)
        .ok_or_else(|| OkError::Unsupported(format!("no pinned digest for model file {name}")))?;
    let actual = format!("{:x}", sha2::Sha256::digest(bytes));
    if actual != expected {
        return Err(OkError::Unsupported(format!(
            "model file {GTE_MODERNBERT_REPO}/{name} does not match its pinned digest \
             (expected {expected}, got {actual}); the download is corrupt or upstream changed"
        )));
    }
    Ok(())
}

/// Where a model's files live once downloaded, which is what the ready marker fingerprints.
///
/// ONNX models are fetched into Open Kioku's own `<root>/<org>--<name>` directory. The Qwen3
/// models are loaded by fastembed's candle path, whose `hf_hub::ApiBuilder::new()` uses
/// `Cache::default()` — a hardcoded `~/.cache/huggingface/hub` that ignores `HF_HOME` — so
/// their directory is the Hugging Face hub cache entry for the repo. Reporting that location
/// instead of an empty `<root>` directory is what makes the ready marker (and the consent
/// gate around it) work for those models.
pub fn neural_model_cache_dir(root: impl AsRef<Path>, model: LocalNeuralModel) -> PathBuf {
    let safe = model.canonical_name().replace('/', "--");
    if model.is_qwen3() {
        return hf_hub::Cache::default()
            .path()
            .join(format!("models--{safe}"));
    }
    root.as_ref().join(safe)
}

fn with_hf_home<T>(cache_dir: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    static HF_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = HF_HOME_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| OkError::Unsupported("Hugging Face cache environment lock poisoned".into()))?;
    let previous: Option<OsString> = std::env::var_os("HF_HOME");
    std::env::set_var("HF_HOME", cache_dir);
    let result = operation();
    match previous {
        Some(value) => std::env::set_var("HF_HOME", value),
        None => std::env::remove_var("HF_HOME"),
    }
    result
}

/// Indexes of `inputs` from longest text to shortest, so padded batches waste little.
fn length_sorted_order(inputs: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(inputs[i].len()));
    order
}

fn reduce_dimensions(mut vector: Vec<f32>, dimensions: usize) -> Result<Vec<f32>> {
    if vector.len() < dimensions {
        return Err(OkError::Unsupported(format!(
            "embedding returned {} dimensions, fewer than configured {dimensions}",
            vector.len()
        )));
    }
    vector.truncate(dimensions);
    normalize(&mut vector);
    Ok(vector)
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn normalize(vector: &mut [f32]) {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for value in vector {
            *value /= magnitude;
        }
    }
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_digest_rejects_a_changed_file() {
        let err = super::verify_pinned_digest("config.json", b"{}").unwrap_err();
        assert!(err.to_string().contains("pinned digest"), "{err}");
        assert!(super::verify_pinned_digest("nope.bin", b"").is_err());
    }

    use super::*;

    #[test]
    fn local_hash_embeddings_are_deterministic_and_normalized() {
        let provider = LocalHashEmbeddingProvider::new(32).unwrap();
        let first = provider.embed("Issue token").unwrap();
        let second = provider.embed("issue-token").unwrap();
        assert_eq!(first, second);
        let magnitude = first.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 0.0001);
    }

    #[test]
    fn quality_model_specs_are_explicit_without_downloading_models() {
        let small = LocalNeuralModel::parse("qwen3-embedding-0.6b").unwrap();
        let quality = LocalNeuralModel::parse("Qwen/Qwen3-Embedding-4B").unwrap();
        let max = LocalNeuralModel::parse("qwen3-embedding-8b").unwrap();
        let code = LocalNeuralModel::parse("jina-v2-base-code").unwrap();
        assert_eq!(small.native_dimensions(), 1_024);
        assert_eq!(quality.native_dimensions(), 2_560);
        assert_eq!(max.native_dimensions(), 4_096);
        assert_eq!(code.native_dimensions(), 768);
        assert!(small.supports_matryoshka());
        assert!(!code.supports_matryoshka());
        assert!(LocalNeuralModel::parse("bge-small-en-v1.5").is_err());
        assert_eq!(
            LocalNeuralModel::parse("gte-modernbert-base").unwrap(),
            LocalNeuralModel::GteModernBertBase
        );
        assert_eq!(LocalNeuralModel::GteModernBertBase.native_dimensions(), 768);
        assert_eq!(
            LocalNeuralModel::parse("").unwrap(),
            LocalNeuralModel::DEFAULT
        );
        assert_eq!(
            LocalNeuralModel::parse("default").unwrap(),
            LocalNeuralModel::GteModernBertBase
        );
        assert!(!LocalNeuralModel::GteModernBertBase.supports_matryoshka());
    }

    #[test]
    fn qwen_matryoshka_dimensions_are_bounded() {
        let model = LocalNeuralModel::Qwen3Embedding4B;
        assert!(model.validate_output_dimensions(1_024).is_ok());
        assert!(model.validate_output_dimensions(2_560).is_ok());
        assert!(model.validate_output_dimensions(128).is_err());
        assert!(model.validate_output_dimensions(4_096).is_err());
        assert!(LocalNeuralModel::JinaEmbeddingsV2BaseCode
            .validate_output_dimensions(768)
            .is_ok());
        assert!(LocalNeuralModel::JinaEmbeddingsV2BaseCode
            .validate_output_dimensions(512)
            .is_err());
    }

    #[test]
    fn dimensionality_reduction_renormalizes() {
        let reduced = reduce_dimensions(vec![3.0, 4.0, 12.0], 2).unwrap();
        assert_eq!(reduced.len(), 2);
        let magnitude = reduced
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((magnitude - 1.0).abs() < 0.0001);
    }

    #[test]
    fn disabled_provider_returns_clear_error() {
        let err = DisabledEmbeddingProvider.embed("query").unwrap_err();
        assert!(err.to_string().contains("not configured"));
    }
}
