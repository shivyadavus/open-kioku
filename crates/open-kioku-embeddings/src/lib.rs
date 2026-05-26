use open_kioku_errors::{OkError, Result};

pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, input: &str) -> Result<Vec<f32>>;
}

pub struct DisabledEmbeddingProvider;

impl EmbeddingProvider for DisabledEmbeddingProvider {
    fn embed(&self, _input: &str) -> Result<Vec<f32>> {
        Err(OkError::Unsupported(
            "embedding provider is not configured".into(),
        ))
    }
}
