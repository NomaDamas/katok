use crate::{
    archive::Archive, config::KatokConfig, search::hydrate_parent_hits, types::SearchHit, Error,
    Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::{
    embedder::create_embedder,
    mock::write_semantic_documents_plain,
    store::{LocalVectorStore, VectorUpsert},
    CHUNK_SCHEMA_ID, SOURCE_ID,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SemanticCursor {
    source_id: String,
    last_synced_at: String,
    seen_token: String,
    chunk_schema_id: String,
    embedder_id: String,
    vectorstore: String,
}

pub async fn index_semantic_live(
    archive: &Archive,
    dir: &Path,
    config: &KatokConfig,
) -> Result<SemanticIndexReport> {
    crate::paths::ensure_private_dir(dir)?;
    let written = write_semantic_documents_plain(archive, dir)?;
    let parents = archive.all_parent_chunks()?;
    let seen_token = semantic_seen_token(&parents);
    let store = LocalVectorStore::open(&dir.join(STORE_DIR), usize::from(config.vector_dimension))?;
    let mut embedder = create_embedder(config)?;
    let mut pending = Vec::new();

    for parent in parents {
        let hash = content_hash(&parent.text);
        let heading_path = format!("{} / parent window", parent.chat_name);
        match store.fetch(&parent.parent_id)? {
            Some(stored) if stored.content_hash == hash => {
                store.mark_seen(&stored.chunk_id, &seen_token, &heading_path)?;
            }
            Some(_) | None => pending.push(PendingChunk {
                chunk_id: parent.parent_id,
                content_hash: hash,
                seen_token: seen_token.clone(),
                heading_path,
                text: parent.text,
            }),
        }
    }

    let embedded_texts = pending.len();
    let batch_size = config.embedding_batch_size.max(1);
    let embedding_calls = embed_pending(&store, &mut *embedder, &pending, batch_size)?;
    store.delete_stale(&seen_token)?;
    save_cursor(dir, &seen_token, embedder.id())?;

    Ok(SemanticIndexReport {
        written_documents: written,
        embedding_calls,
        embedded_texts,
        embedder: embedder.id(),
        vectorstore: "local",
        semantic_units: "parent_windows",
    })
}

pub async fn semantic_search_live_with_config(
    archive: &Archive,
    dir: &Path,
    query: &str,
    limit: usize,
    config: &KatokConfig,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    if !dir.join("cursor.json").exists() {
        return Err(Error::SemanticIndexMissing);
    }
    let cursor = load_cursor(dir)?;
    let mut embedder = create_embedder(config)?;
    validate_cursor(&cursor, embedder.id())?;
    let store = LocalVectorStore::open(&dir.join(STORE_DIR), usize::from(config.vector_dimension))?;
    let vector = embedder.embed_query(query)?;
    let ids = store
        .search(&vector, limit)?
        .into_iter()
        .map(|hit| hit.chunk_id)
        .collect::<Vec<_>>();
    hydrate_parent_hits(archive, ids, "semantic", query, config.snippet_length)
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

fn save_cursor(dir: &Path, seen_token: &str, embedder_id: &str) -> Result<()> {
    let cursor = SemanticCursor {
        source_id: SOURCE_ID.to_string(),
        last_synced_at: chrono::Utc::now().to_rfc3339(),
        seen_token: seen_token.to_string(),
        chunk_schema_id: CHUNK_SCHEMA_ID.to_string(),
        embedder_id: embedder_id.to_string(),
        vectorstore: "local".to_string(),
    };
    let json = serde_json::to_vec_pretty(&cursor).map_err(Error::Json)?;
    std::fs::write(dir.join("cursor.json"), json).map_err(Error::Io)
}

fn load_cursor(dir: &Path) -> Result<SemanticCursor> {
    let content = std::fs::read(dir.join("cursor.json")).map_err(Error::Io)?;
    serde_json::from_slice(&content).map_err(Error::Json)
}

fn validate_cursor(cursor: &SemanticCursor, embedder_id: &str) -> Result<()> {
    if cursor.source_id != SOURCE_ID {
        return stale_index_error("source", &cursor.source_id, SOURCE_ID);
    }
    if cursor.chunk_schema_id != CHUNK_SCHEMA_ID {
        return stale_index_error("schema", &cursor.chunk_schema_id, CHUNK_SCHEMA_ID);
    }
    if cursor.vectorstore != "local" {
        return stale_index_error("vectorstore", &cursor.vectorstore, "local");
    }
    if cursor.embedder_id != embedder_id {
        return stale_index_error("embedder", &cursor.embedder_id, embedder_id);
    }
    Ok(())
}

fn stale_index_error(field: &str, actual: &str, expected: &str) -> Result<()> {
    Err(Error::Embedding(format!(
        "semantic index {field} is {actual}, expected {expected}; re-run katok index"
    )))
}

fn semantic_seen_token(chunks: &[crate::types::ParentChunk]) -> String {
    let mut material = String::new();
    for chunk in chunks {
        material.push_str(&chunk.parent_id);
        material.push('\0');
        material.push_str(&content_hash(&chunk.text));
        material.push('\0');
    }
    content_hash(&material)
}

fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
