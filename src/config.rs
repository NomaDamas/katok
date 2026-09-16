use crate::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_EMBEDDER_MODEL: &str = "embeddinggemma-300m-q4";
pub const DEFAULT_VECTOR_DIMENSION: u16 = 768;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KatokConfig {
    pub source_adapter: String,
    pub chunk_gap_group_seconds: i64,
    pub chunk_gap_direct_seconds: i64,
    pub semantic_dir: PathBuf,
    pub embedder_model: String,
    pub embedding_batch_size: usize,
    pub vector_dimension: u16,
    pub snippet_length: usize,
    pub embedding_provider: String,
    pub embedding_endpoint: Option<String>,
    pub embedding_timeout_ms: u64,
    pub embedding_query_prefix: String,
    pub embedding_passage_prefix: String,
}

impl Default for KatokConfig {
    fn default() -> Self {
        Self {
            source_adapter: "fixture".to_string(),
            chunk_gap_group_seconds: 600,
            chunk_gap_direct_seconds: 1_800,
            semantic_dir: PathBuf::from("semantic"),
            embedder_model: DEFAULT_EMBEDDER_MODEL.to_string(),
            embedding_batch_size: 64,
            vector_dimension: DEFAULT_VECTOR_DIMENSION,
            snippet_length: 80,
            embedding_provider: "local".to_string(),
            embedding_endpoint: None,
            embedding_timeout_ms: 10_000,
            embedding_query_prefix: "query: ".to_string(),
            embedding_passage_prefix: "passage: ".to_string(),
        }
    }
}

impl KatokConfig {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config = if let Some(path) = path {
            let content = std::fs::read_to_string(path).map_err(Error::Io)?;
            toml::from_str(&content).map_err(Error::Config)?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        match self.embedding_provider.as_str() {
            "local" => Ok(()),
            "loopback-http" => {
                let endpoint = self.embedding_endpoint.as_deref().ok_or_else(|| {
                    Error::Embedding("loopback-http requires embedding_endpoint".to_string())
                })?;
                if !endpoint.starts_with("http://localhost:")
                    && !endpoint.starts_with("http://127.0.0.1:")
                    && !endpoint.starts_with("http://[::1]:")
                {
                    return Err(Error::Embedding(
                        "embedding endpoint must be loopback and use http://".to_string(),
                    ));
                }
                Ok(())
            }
            provider => Err(Error::Embedding(format!(
                "unsupported embedding provider: {provider}"
            ))),
        }
    }
}
