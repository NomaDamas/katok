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
        }
    }
}

impl KatokConfig {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let content = std::fs::read_to_string(path).map_err(Error::Io)?;
        toml::from_str(&content).map_err(Error::Config)
    }
}
