use crate::{
    config::{KatokConfig, DEFAULT_EMBEDDER_MODEL},
    Error, Result,
};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use sha2::{Digest, Sha256};

pub(crate) trait SemanticEmbedder {
    fn id(&self) -> &'static str;
    fn embed(&mut self, texts: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let embeddings = self.embed(&[query.to_string()], 1)?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| Error::Embedding("embedder returned no query vector".to_string()))
    }
}

pub(crate) fn create_embedder(config: &KatokConfig) -> Result<Box<dyn SemanticEmbedder>> {
    match std::env::var("KATOK_EMBEDDER").ok().as_deref() {
        Some("mock" | "local-test") => Ok(Box::new(DeterministicEmbedder::new(usize::from(
            config.vector_dimension,
        )))),
        _ => Ok(Box::new(FastEmbedder::new(config)?)),
    }
}

struct FastEmbedder {
    inner: TextEmbedding,
}

impl FastEmbedder {
    fn new(config: &KatokConfig) -> Result<Self> {
        if config.embedder_model != DEFAULT_EMBEDDER_MODEL {
            return Err(Error::Embedding(format!(
                "unsupported local embedder model: {}",
                config.embedder_model
            )));
        }
        let options = TextInitOptions::new(EmbeddingModel::EmbeddingGemma300MQ4)
            .with_show_download_progress(false);
        let inner = TextEmbedding::try_new(options).map_err(to_embedding_error)?;
        Ok(Self { inner })
    }
}

impl SemanticEmbedder for FastEmbedder {
    fn id(&self) -> &'static str {
        "embeddinggemma/local"
    }

    fn embed(&mut self, texts: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>> {
        let documents = texts
            .iter()
            .map(|text| format!("passage: {text}"))
            .collect::<Vec<_>>();
        self.inner
            .embed(documents, Some(batch_size.max(1)))
            .map_err(to_embedding_error)
    }

    fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let embeddings = self
            .inner
            .embed([format!("query: {}", query.trim())], Some(1))
            .map_err(to_embedding_error)?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| Error::Embedding("embedder returned no query vector".to_string()))
    }
}

struct DeterministicEmbedder {
    dimension: usize,
}

impl DeterministicEmbedder {
    const fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

impl SemanticEmbedder for DeterministicEmbedder {
    fn id(&self) -> &'static str {
        "embeddinggemma/local-test"
    }

    fn embed(&mut self, texts: &[String], _batch_size: usize) -> Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| deterministic_vector(text, self.dimension))
            .collect()
    }
}

fn deterministic_vector(text: &str, dimension: usize) -> Result<Vec<f32>> {
    if dimension == 0 {
        return Err(Error::Embedding(
            "embedding dimension must be nonzero".to_string(),
        ));
    }
    let mut vector = vec![0.0_f32; dimension];
    for term in text.split_whitespace() {
        let hash = Sha256::digest(term.as_bytes());
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&hash[..8]);
        let dimension =
            u64::try_from(dimension).map_err(|error| Error::Embedding(error.to_string()))?;
        let index = usize::try_from(u64::from_le_bytes(bytes) % dimension)
            .map_err(|error| Error::Embedding(error.to_string()))?;
        vector[index] += 1.0;
    }
    normalize(&mut vector);
    Ok(vector)
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in vector {
        *value /= norm;
    }
}

fn to_embedding_error(error: impl std::fmt::Display) -> Error {
    Error::Embedding(error.to_string())
}
