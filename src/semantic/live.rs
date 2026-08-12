use crate::{
    archive::Archive, config::KatokConfig, search::hydrate_parent_hits, types::SearchHit, Error,
    Result,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

use super::{
    archive_revision, content_hash, current_generation,
    embedder::create_embedder,
    mock::write_semantic_documents_plain,
    store::{LocalVectorStore, VectorUpsert},
    CHUNK_SCHEMA_ID, CURRENT_FILE, GENERATIONS_DIR, SOURCE_ID,
};

pub const STORE_DIR: &str = "store";

#[derive(Debug, Clone, Serialize)]
pub struct SemanticIndexReport {
    pub written_documents: usize,
    pub embedding_calls: usize,
    pub embedded_texts: usize,
    pub embedder: &'static str,
    pub vectorstore: &'static str,
    pub semantic_units: &'static str,
    pub archive_revision: String,
    pub reused_vectors: usize,
    pub self_healed: bool,
    pub cleanup_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCursor {
    source_id: String,
    pub completed_at: String,
    pub archive_revision: String,
    chunk_schema_id: String,
    pub embedder_id: String,
    pub vectorstore: String,
    pub semantic_units: String,
    pub embedded_texts: usize,
}

pub async fn index_semantic_live(
    archive: &Archive,
    root: &Path,
    config: &KatokConfig,
    full: bool,
) -> Result<SemanticIndexReport> {
    crate::paths::ensure_private_dir(root)?;
    crate::paths::ensure_private_dir(&root.join(GENERATIONS_DIR))?;
    let _writer = IndexWriterGuard::acquire(root)?;
    let revision = archive_revision(archive)?;
    let generation_id = format!(
        "gen-{}-{}-{}",
        &revision[..16],
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let staging = root
        .join(GENERATIONS_DIR)
        .join(format!(".{generation_id}.staging"));
    let mut staging_guard = StagingGuard::new(staging.clone());
    crate::paths::ensure_private_dir(&staging)?;
    let written = write_semantic_documents_plain(archive, &staging)?;
    let parents = archive.all_parent_chunks()?;
    let store = LocalVectorStore::open(
        &staging.join(STORE_DIR),
        usize::from(config.vector_dimension),
    )?;
    let mut embedder = create_embedder(config)?;
    let prior_generation = current_generation(root);
    let mut self_healed = false;
    let prior_store = if full {
        None
    } else {
        match prior_generation {
            Ok(generation) => match validate_generation(
                archive,
                &generation,
                embedder.id(),
                config.vector_dimension,
            ) {
                Ok(()) => Some(LocalVectorStore::open_existing(
                    &generation.join(STORE_DIR),
                    usize::from(config.vector_dimension),
                )?),
                Err(Error::SemanticIndexStale(_) | Error::Embedding(_) | Error::Sql(_)) => {
                    self_healed = true;
                    None
                }
                Err(error) => return Err(error),
            },
            Err(Error::SemanticIndexMissing | Error::SemanticIndexStale(_)) => None,
            Err(error) => return Err(error),
        }
    };
    let mut pending = Vec::new();
    let mut reused_vectors = 0usize;

    for parent in parents {
        let hash = content_hash(&parent.text);
        let heading_path = format!("{} / parent window", parent.chat_name);
        if let Some(stored) = prior_store
            .as_ref()
            .and_then(|prior| prior.fetch(&parent.parent_id).transpose())
            .transpose()?
            .filter(|stored| stored.content_hash == hash)
        {
            reused_vectors += 1;
            store.upsert(&VectorUpsert {
                chunk_id: parent.parent_id,
                content_hash: hash,
                seen_token: revision.clone(),
                heading_path,
                vector: stored.vector,
            })?;
        } else {
            pending.push(PendingChunk {
                chunk_id: parent.parent_id,
                content_hash: hash,
                seen_token: revision.clone(),
                heading_path,
                text: parent.text,
            });
        }
    }

    let embedded_texts = pending.len();
    let batch_size = config.embedding_batch_size.max(1);
    let embedding_calls = embed_pending(&store, &mut *embedder, &pending, batch_size)?;
    save_cursor(&staging, &revision, embedder.id(), embedded_texts)?;
    validate_generation(archive, &staging, embedder.id(), config.vector_dimension)?;

    let generation = root.join(GENERATIONS_DIR).join(&generation_id);
    std::fs::rename(&staging, &generation).map_err(Error::Io)?;
    staging_guard.disarm();
    if let Err(error) = publish_current(root, &generation_id) {
        let _ = std::fs::remove_dir_all(&generation);
        return Err(error);
    }
    let cleanup_warnings = cleanup_old_generations(root, &generation_id);

    Ok(SemanticIndexReport {
        written_documents: written,
        embedding_calls,
        embedded_texts,
        embedder: embedder.id(),
        vectorstore: "local",
        semantic_units: "parent_windows",
        archive_revision: revision,
        reused_vectors,
        self_healed,
        cleanup_warnings,
    })
}

pub async fn semantic_search_live_with_config(
    archive: &Archive,
    root: &Path,
    query: &str,
    limit: usize,
    config: &KatokConfig,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    let generation = current_generation(root)?;
    let cursor = load_cursor(&generation)?;
    let mut embedder = create_embedder(config)?;
    validate_generation(archive, &generation, embedder.id(), config.vector_dimension)?;
    let store = LocalVectorStore::open_existing(
        &generation.join(STORE_DIR),
        usize::from(config.vector_dimension),
    )?;
    let vector = embedder.embed_query(query)?;
    let ids = store
        .search(&vector, limit)?
        .into_iter()
        .map(|hit| hit.chunk_id)
        .collect::<Vec<_>>();
    if cursor.archive_revision != archive_revision(archive)? {
        return Err(Error::SemanticIndexStale(
            "archive changed while semantic search was starting".to_string(),
        ));
    }
    match hydrate_parent_hits(archive, ids, "semantic", query, config.snippet_length) {
        Err(Error::MissingChunk(id)) => Err(Error::SemanticIndexStale(format!(
            "vector references missing archive window {id}"
        ))),
        result => result,
    }
}

pub fn committed_cursor(root: &Path) -> Result<SemanticCursor> {
    load_cursor(&current_generation(root)?)
}

fn validate_generation(
    archive: &Archive,
    generation: &Path,
    embedder_id: &str,
    dimension: u16,
) -> Result<()> {
    let cursor = load_cursor(generation)?;
    validate_cursor(&cursor, embedder_id)?;
    let current_revision = archive_revision(archive)?;
    if cursor.archive_revision != current_revision {
        return Err(Error::SemanticIndexStale(format!(
            "archive revision is {}, index revision is {}",
            current_revision, cursor.archive_revision
        )));
    }
    let mut expected = archive
        .all_parent_chunks()?
        .into_iter()
        .map(|parent| (parent.parent_id, content_hash(&parent.text)))
        .collect::<Vec<_>>();
    expected.sort();
    let actual =
        LocalVectorStore::open_existing(&generation.join(STORE_DIR), usize::from(dimension))?
            .content_pairs()?;
    if actual != expected {
        return Err(Error::SemanticIndexStale(
            "vector ids or content hashes do not match the archive".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PendingChunk {
    chunk_id: String,
    content_hash: String,
    seen_token: String,
    heading_path: String,
    text: String,
}

fn embed_pending(
    store: &LocalVectorStore,
    embedder: &mut dyn super::embedder::SemanticEmbedder,
    pending: &[PendingChunk],
    batch_size: usize,
) -> Result<usize> {
    for batch in pending.chunks(batch_size) {
        let texts = batch
            .iter()
            .map(|chunk| chunk.text.clone())
            .collect::<Vec<_>>();
        let embeddings = embedder.embed(&texts, batch_size)?;
        if embeddings.len() != batch.len() {
            return Err(Error::Embedding(format!(
                "expected {} embeddings, got {}",
                batch.len(),
                embeddings.len()
            )));
        }
        for (chunk, vector) in batch.iter().zip(embeddings) {
            store.upsert(&VectorUpsert {
                chunk_id: chunk.chunk_id.clone(),
                content_hash: chunk.content_hash.clone(),
                seen_token: chunk.seen_token.clone(),
                heading_path: chunk.heading_path.clone(),
                vector,
            })?;
        }
    }
    Ok(pending.len().div_ceil(batch_size))
}

fn save_cursor(dir: &Path, revision: &str, embedder_id: &str, embedded_texts: usize) -> Result<()> {
    let cursor = SemanticCursor {
        source_id: SOURCE_ID.to_string(),
        completed_at: chrono::Utc::now().to_rfc3339(),
        archive_revision: revision.to_string(),
        chunk_schema_id: CHUNK_SCHEMA_ID.to_string(),
        embedder_id: embedder_id.to_string(),
        vectorstore: "local".to_string(),
        semantic_units: "parent_windows".to_string(),
        embedded_texts,
    };
    let json = serde_json::to_vec_pretty(&cursor).map_err(Error::Json)?;
    std::fs::write(dir.join("cursor.json"), json).map_err(Error::Io)
}

fn load_cursor(dir: &Path) -> Result<SemanticCursor> {
    let content = std::fs::read(dir.join("cursor.json"))
        .map_err(|error| Error::SemanticIndexStale(format!("cannot read cursor: {error}")))?;
    serde_json::from_slice(&content)
        .map_err(|error| Error::SemanticIndexStale(format!("cannot parse cursor: {error}")))
}

fn validate_cursor(cursor: &SemanticCursor, embedder_id: &str) -> Result<()> {
    for (field, actual, expected) in [
        ("source", cursor.source_id.as_str(), SOURCE_ID),
        ("schema", cursor.chunk_schema_id.as_str(), CHUNK_SCHEMA_ID),
        ("vectorstore", cursor.vectorstore.as_str(), "local"),
        ("embedder", cursor.embedder_id.as_str(), embedder_id),
    ] {
        if actual != expected {
            return Err(Error::SemanticIndexStale(format!(
                "{field} is {actual}, expected {expected}"
            )));
        }
    }
    Ok(())
}

fn publish_current(root: &Path, generation_id: &str) -> Result<()> {
    let temporary = root.join(format!(".{CURRENT_FILE}.{}", std::process::id()));
    std::fs::write(&temporary, format!("{generation_id}\n")).map_err(Error::Io)?;
    std::fs::rename(&temporary, root.join(CURRENT_FILE)).map_err(Error::Io)
}

fn cleanup_old_generations(root: &Path, current: &str) -> Vec<String> {
    let generations = root.join(GENERATIONS_DIR);
    let entries = match std::fs::read_dir(&generations) {
        Ok(entries) => entries,
        Err(error) => {
            return vec![format!(
                "cannot scan old semantic generations at {}: {error}",
                generations.display()
            )];
        }
    };
    let mut warnings = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("cannot read a semantic generation entry: {error}"));
                continue;
            }
        };
        if entry.file_name() == current {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(entry.path()) {
            warnings.push(format!(
                "cannot remove old semantic generation {}: {error}",
                entry.path().display()
            ));
        }
    }
    warnings
}

struct IndexWriterGuard {
    conn: Connection,
}

impl IndexWriterGuard {
    fn acquire(root: &Path) -> Result<Self> {
        let path = root.join("index-writer.sqlite3");
        let conn = Connection::open(&path).map_err(Error::Sql)?;
        conn.busy_timeout(Duration::ZERO).map_err(Error::Sql)?;
        conn.execute_batch("BEGIN EXCLUSIVE")
            .map_err(|_| Error::SemanticIndexBusy(path))?;
        Ok(Self { conn })
    }
}

impl Drop for IndexWriterGuard {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

struct StagingGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
