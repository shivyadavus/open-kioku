use open_kioku_errors::{OkError, Result};

pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, input: &str) -> Result<Vec<f32>>;
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
    fn embed(&self, input: &str) -> Result<Vec<f32>> {
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
}

pub struct DisabledEmbeddingProvider;

impl EmbeddingProvider for DisabledEmbeddingProvider {
    fn embed(&self, _input: &str) -> Result<Vec<f32>> {
        Err(OkError::Unsupported(
            "embedding provider is not configured".into(),
        ))
    }
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
    fn disabled_provider_returns_clear_error() {
        let err = DisabledEmbeddingProvider.embed("query").unwrap_err();
        assert!(err.to_string().contains("not configured"));
    }
}
