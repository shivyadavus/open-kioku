use candle_core::{DType, Device};
use fastembed::{EmbeddingModel, Qwen3TextEmbedding, TextEmbedding, TextInitOptions};
use open_kioku_errors::{OkError, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const FASTEMBED_PROVIDER_VERSION: &str = "fastembed-5.8.0";
pub const QWEN3_MAX_LENGTH: usize = 8_192;
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
}

impl LocalNeuralModel {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "qwen3-embedding-0.6b" | "Qwen/Qwen3-Embedding-0.6B" => {
                Ok(Self::Qwen3Embedding06B)
            }
            "qwen3-embedding-4b" | "Qwen/Qwen3-Embedding-4B" => Ok(Self::Qwen3Embedding4B),
            "qwen3-embedding-8b" | "Qwen/Qwen3-Embedding-8B" => Ok(Self::Qwen3Embedding8B),
            "jina-embeddings-v2-base-code" | "jina-v2-base-code"
            | "jinaai/jina-embeddings-v2-base-code" => Ok(Self::JinaEmbeddingsV2BaseCode),
            other => Err(OkError::Unsupported(format!(
                "local neural embedding model `{other}` is unsupported; supported models: Qwen/Qwen3-Embedding-0.6B, Qwen/Qwen3-Embedding-4B, Qwen/Qwen3-Embedding-8B, jinaai/jina-embeddings-v2-base-code"
            ))),
        }
    }

    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::Qwen3Embedding06B => "Qwen/Qwen3-Embedding-0.6B",
            Self::Qwen3Embedding4B => "Qwen/Qwen3-Embedding-4B",
            Self::Qwen3Embedding8B => "Qwen/Qwen3-Embedding-8B",
            Self::JinaEmbeddingsV2BaseCode => "jinaai/jina-embeddings-v2-base-code",
        }
    }

    pub fn native_dimensions(self) -> usize {
        match self {
            Self::Qwen3Embedding06B => 1_024,
            Self::Qwen3Embedding4B => 2_560,
            Self::Qwen3Embedding8B => 4_096,
            Self::JinaEmbeddingsV2BaseCode => 768,
        }
    }

    pub fn supports_matryoshka(self) -> bool {
        !matches!(self, Self::JinaEmbeddingsV2BaseCode)
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
        !matches!(self, Self::JinaEmbeddingsV2BaseCode)
    }
}

enum NeuralBackend {
    Qwen3(Mutex<Qwen3TextEmbedding>),
    JinaCode(Mutex<TextEmbedding>),
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
            NeuralBackend::JinaCode(Mutex::new(inner))
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
                {
                    let mut vectors = Vec::with_capacity(inputs.len());
                    for chunk in inputs.chunks(batch_size.max(1)) {
                        let mut chunk_vectors = model.embed(chunk).map_err(|err| {
                            OkError::Unsupported(format!("Qwen3 embedding inference failed: {err}"))
                        })?;
                        vectors.append(&mut chunk_vectors);
                    }
                    vectors
                }
            }
            NeuralBackend::JinaCode(inner) => {
                let mut model = inner.lock().map_err(|_| {
                    OkError::Unsupported("Jina code embedding model lock poisoned".into())
                })?;
                model
                    .embed(inputs, Some(batch_size.max(1)))
                    .map_err(|err| {
                        OkError::Unsupported(format!("Jina code embedding inference failed: {err}"))
                    })?
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
                format!("{FASTEMBED_PROVIDER_VERSION}:onnx")
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

pub fn neural_model_cache_dir(root: impl AsRef<Path>, model: LocalNeuralModel) -> PathBuf {
    let safe = model.canonical_name().replace('/', "--");
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
