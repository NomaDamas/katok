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
    #[serde(default)]
    pub(crate) archive_revision: String,
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

pub(crate) fn load(
    data_dir: &Path,
    archive_path: &Path,
    semantic_dir: &Path,
) -> Result<FreshnessStatus> {
    let path = status_path(data_dir);
    let mut status = if path.exists() {
        let bytes = std::fs::read(&path).context("read freshness status")?;
        serde_json::from_slice(&bytes).context("parse freshness status")?
    } else {
        FreshnessStatus::default()
    };
    let committed_cursor = katok::semantic::committed_cursor(semantic_dir);
    let committed_error = match &committed_cursor {
        Err(katok::Error::SemanticIndexMissing) => None,
        Err(error) => Some(error.to_string()),
        Ok(_) => None,
    };
    status.last_index = committed_cursor.ok().map(|cursor| IndexFreshness {
        completed_at: cursor.completed_at,
        embedder: cursor.embedder_id,
        vectorstore: cursor.vectorstore,
        semantic_units: cursor.semantic_units,
        embedded_texts: cursor.embedded_texts,
        archive_revision: cursor.archive_revision,
    });
    status.recommendation = recommendation(&status, archive_path, committed_error.as_deref());
    Ok(status)
}

pub(crate) fn record_sync(
    data_dir: &Path,
    source: &str,
    total_messages: usize,
    chunks: usize,
) -> Result<()> {
    let mut status = load_raw(data_dir)?;
    status.last_sync = Some(SyncFreshness {
        completed_at: chrono::Utc::now().to_rfc3339(),
        source: source.to_string(),
        total_messages,
        chunks,
    });
    status.recommendation = recommendation_from_records(&status);
    save(data_dir, &status)
}

fn save(data_dir: &Path, status: &FreshnessStatus) -> Result<()> {
    katok::paths::ensure_private_dir(data_dir).context("create data directory")?;
    let bytes = serde_json::to_vec_pretty(status).context("serialize freshness status")?;
    let path = status_path(data_dir);
    let temporary = data_dir.join(format!(".status.json.{}", std::process::id()));
    std::fs::write(&temporary, bytes).context("write freshness status staging file")?;
    std::fs::rename(&temporary, path).context("publish freshness status")
}

fn load_raw(data_dir: &Path) -> Result<FreshnessStatus> {
    let path = status_path(data_dir);
    if !path.exists() {
        return Ok(FreshnessStatus::default());
    }
    let bytes = std::fs::read(&path).context("read freshness status")?;
    serde_json::from_slice(&bytes).context("parse freshness status")
}

fn recommendation(
    status: &FreshnessStatus,
    archive_path: &Path,
    committed_error: Option<&str>,
) -> FreshnessRecommendation {
    let base = recommendation_from_records(status);
    if base.sync_before_search {
        return base;
    }
    if let Some(error) = committed_error {
        return FreshnessRecommendation {
            sync_before_search: false,
            index_before_semantic_search: true,
            reason: format!(
                "semantic index is corrupt ({error}); run katok index --json before semantic search"
            ),
        };
    }
    if status.last_index.is_none() {
        return base;
    }
    if !archive_path.is_file() {
        return FreshnessRecommendation {
            sync_before_search: true,
            index_before_semantic_search: true,
            reason: "archive is missing; run katok sync --source macos --json, then katok index --json before search".to_string(),
        };
    }
    let current_revision = katok::archive::Archive::open(archive_path)
        .and_then(|archive| katok::semantic::archive_revision(&archive));
    let committed_revision = status
        .last_index
        .as_ref()
        .map(|index| index.archive_revision.as_str());
    match (current_revision, committed_revision) {
        (Ok(current), Some(committed)) if current == committed => base,
        (Ok(_), Some(_)) => FreshnessRecommendation {
            sync_before_search: false,
            index_before_semantic_search: true,
            reason: "archive revision is newer than the committed semantic index; run katok index --json before semantic search".to_string(),
        },
        (Err(error), Some(_)) => FreshnessRecommendation {
            sync_before_search: false,
            index_before_semantic_search: true,
            reason: format!("archive revision could not be read ({error}); repair the archive before semantic search"),
        },
        (_, None) => base,
    }
}

fn recommendation_from_records(status: &FreshnessStatus) -> FreshnessRecommendation {
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
