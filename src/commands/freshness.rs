use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STATUS_FILE: &str = "status.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FreshnessStatus {
    #[serde(default)]
    pub(crate) last_sync: Option<SyncFreshness>,
    #[serde(default)]
    pub(crate) last_index: Option<IndexFreshness>,
    #[serde(default)]
    pub(crate) recommendation: FreshnessRecommendation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SyncFreshness {
    pub(crate) completed_at: String,
    pub(crate) source: String,
    pub(crate) total_messages: usize,
    pub(crate) chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IndexFreshness {
    pub(crate) completed_at: String,
    pub(crate) embedder: String,
    pub(crate) vectorstore: String,
    pub(crate) semantic_units: String,
    pub(crate) embedded_texts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FreshnessRecommendation {
    pub(crate) sync_before_search: bool,
    pub(crate) index_before_semantic_search: bool,
    pub(crate) reason: String,
}

impl Default for FreshnessRecommendation {
    fn default() -> Self {
        Self {
            sync_before_search: true,
            index_before_semantic_search: true,
            reason: "run katok sync --source macos --json, then katok index --json before search"
                .to_string(),
        }
    }
}

pub(crate) fn load(data_dir: &Path) -> Result<FreshnessStatus> {
    let path = status_path(data_dir);
    if !path.exists() {
        return Ok(FreshnessStatus::default());
    }
    let bytes = std::fs::read(&path).context("read freshness status")?;
    let mut status: FreshnessStatus =
        serde_json::from_slice(&bytes).context("parse freshness status")?;
    status.recommendation = recommendation(&status);
    Ok(status)
}

pub(crate) fn record_sync(
    data_dir: &Path,
    source: &str,
    total_messages: usize,
    chunks: usize,
) -> Result<()> {
    let mut status = load(data_dir)?;
    status.last_sync = Some(SyncFreshness {
        completed_at: chrono::Utc::now().to_rfc3339(),
        source: source.to_string(),
        total_messages,
        chunks,
    });
    status.recommendation = recommendation(&status);
    save(data_dir, &status)
}

pub(crate) fn record_index(
    data_dir: &Path,
    embedder: &str,
    vectorstore: &str,
    semantic_units: &str,
    embedded_texts: usize,
) -> Result<()> {
    let mut status = load(data_dir)?;
    status.last_index = Some(IndexFreshness {
        completed_at: chrono::Utc::now().to_rfc3339(),
        embedder: embedder.to_string(),
        vectorstore: vectorstore.to_string(),
        semantic_units: semantic_units.to_string(),
        embedded_texts,
    });
    status.recommendation = recommendation(&status);
    save(data_dir, &status)
}

fn save(data_dir: &Path, status: &FreshnessStatus) -> Result<()> {
    katok::paths::ensure_private_dir(data_dir).context("create data directory")?;
    let bytes = serde_json::to_vec_pretty(status).context("serialize freshness status")?;
    std::fs::write(status_path(data_dir), bytes).context("write freshness status")
}

fn recommendation(status: &FreshnessStatus) -> FreshnessRecommendation {
    let sync_before_search = status.last_sync.is_none();
    let index_before_semantic_search = status.last_index.is_none();
    let reason = if sync_before_search {
        "no sync has completed; run katok sync --source macos --json before search"
    } else if index_before_semantic_search {
        "no semantic index has completed; run katok index --json before semantic search"
    } else {
        "archive and semantic index have completed at least once; re-run sync/index when freshness matters"
    };
    FreshnessRecommendation {
        sync_before_search,
        index_before_semantic_search,
        reason: reason.to_string(),
    }
}

fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATUS_FILE)
}
