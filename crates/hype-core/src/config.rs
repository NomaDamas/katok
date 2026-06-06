use crate::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HypeConfig {
    pub source_adapter: String,
    pub chunk_gap_group_seconds: i64,
    pub chunk_gap_direct_seconds: i64,
    pub minsync_dir: PathBuf,
    pub embedder_model: String,
    pub vector_dimension: u16,
    pub snippet_length: usize,
}

impl Default for HypeConfig {
    fn default() -> Self {
        Self {
            source_adapter: "fixture".to_string(),
            chunk_gap_group_seconds: 600,
            chunk_gap_direct_seconds: 1_800,
            minsync_dir: PathBuf::from("semantic"),
            embedder_model: "tei:jinaai/jina-embeddings-v4".to_string(),
            vector_dimension: 2_048,
            snippet_length: 80,
        }
    }
}

impl HypeConfig {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let content = std::fs::read_to_string(path).map_err(Error::Io)?;
        toml::from_str(&content).map_err(Error::Config)
    }
}
