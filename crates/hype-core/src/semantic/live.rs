use crate::{
    archive::Archive, config::HypeConfig, search::hydrate_hits, types::SearchHit, Error, Result,
};
use minsync::{
    config::Config as MinSyncConfig,
    embedder::{create_embedder, Embedder},
    id::{compute_doc_id, content_hash},
    state::Cursor,
    vectorstore::{create_vectorstore, Document, DocumentUpdate, Filter, VectorStore},
};
use std::path::Path;

use super::{
    chunk_id_from_minsync_path, endpoint::validate_embedding_endpoint, minsync_chunk_path,
    mock::write_semantic_documents_plain, CHUNK_SCHEMA_ID, CHUNK_TYPE, SOURCE_ID,
};

pub const STORE_DIR: &str = "store";

#[derive(Debug, Clone, serde::Serialize)]
pub struct SemanticIndexReport {
    pub written_documents: usize,
    pub embedding_calls: usize,
    pub embedded_texts: usize,
    pub vectorstore: &'static str,
}

pub async fn index_semantic_live(
    archive: &Archive,
    dir: &Path,
    config: &HypeConfig,
) -> Result<SemanticIndexReport> {
    validate_embedding_endpoint(config)?;
    crate::paths::ensure_private_dir(dir)?;
    let written = write_semantic_documents_plain(archive, dir)?;
    let minsync_config = minsync_config(config);
    minsync_config
        .save(&dir.join("config.toml"))
        .map_err(to_hype_error)?;

    let embedder = create_embedder(&minsync_config).map_err(to_hype_error)?;
    crate::paths::ensure_private_dir(&dir.join(STORE_DIR))?;
    let mut store =
        create_vectorstore(&minsync_config, &dir.join(STORE_DIR)).map_err(to_hype_error)?;
    let chunks = archive.all_chunks()?;
    let seen_token = semantic_seen_token(&chunks);
    let mut docs_to_embed = Vec::new();
    let mut updates = Vec::new();

    for chunk in chunks {
        let path = minsync_chunk_path(&chunk.chunk_id);
        let hash = content_hash(&chunk.text);
        let id = compute_doc_id(SOURCE_ID, &path, CHUNK_SCHEMA_ID, CHUNK_TYPE, &hash, 0);
        if store
            .fetch(std::slice::from_ref(&id))
            .map_err(to_hype_error)?
            .is_empty()
        {
            docs_to_embed.push(Document {
                id,
                embedding: Vec::new(),
                text: chunk.text,
                source_id: SOURCE_ID.to_string(),
                path,
                chunk_schema_id: CHUNK_SCHEMA_ID.to_string(),
                chunk_type: CHUNK_TYPE.to_string(),
                heading_path: format!("{} / {}", chunk.chat_name, chunk.sender_nickname),
                content_hash: hash,
                seen_token: seen_token.clone(),
            });
        } else {
            updates.push(DocumentUpdate {
                id,
                seen_token: seen_token.clone(),
                path,
                heading_path: format!("{} / {}", chunk.chat_name, chunk.sender_nickname),
            });
        }
    }

    if !updates.is_empty() {
        store.update(&updates).map_err(to_hype_error)?;
    }
    let embedded_texts = docs_to_embed.len();
    let embedding_calls =
        embed_and_upsert(&*embedder, &mut *store, &mut docs_to_embed, config).await?;
    delete_stale(&mut *store, &seen_token)?;
    store.flush().map_err(to_hype_error)?;
    save_cursor(dir, &seen_token, embedder.id())?;

    Ok(SemanticIndexReport {
        written_documents: written,
        embedding_calls,
        embedded_texts,
        vectorstore: "lancedb",
    })
}

pub async fn semantic_search_live_with_config(
    archive: &Archive,
    dir: &Path,
    query: &str,
    limit: usize,
    config: &HypeConfig,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    if !dir.join("cursor.json").exists() {
        return Err(Error::SemanticIndexMissing);
    }
    validate_embedding_endpoint(config)?;
    let minsync_config = minsync_config(config);
    let embedder = create_embedder(&minsync_config).map_err(to_hype_error)?;
    let store = create_vectorstore(&minsync_config, &dir.join(STORE_DIR)).map_err(to_hype_error)?;
    let vector = embedder.embed_query(query).await.map_err(to_hype_error)?;
    let hits = store
        .query(
            &vector,
            Some(&Filter::Eq("source_id".to_string(), SOURCE_ID.to_string())),
            limit,
        )
        .map_err(to_hype_error)?;
    let ids = hits
        .into_iter()
        .filter_map(|hit| chunk_id_from_minsync_path(&hit.path))
        .collect::<Vec<_>>();
    hydrate_hits(archive, ids, "semantic", query, config.snippet_length)
}

async fn embed_and_upsert(
    embedder: &dyn Embedder,
    store: &mut dyn VectorStore,
    docs: &mut [Document],
    config: &HypeConfig,
) -> Result<usize> {
    if docs.is_empty() {
        return Ok(0);
    }
    let texts = docs.iter().map(|doc| doc.text.clone()).collect::<Vec<_>>();
    let embeddings = embedder.embed(&texts).await.map_err(to_hype_error)?;
    if embeddings.len() != docs.len() {
        return Err(Error::MinSync(format!(
            "expected {} embeddings, got {}",
            docs.len(),
            embeddings.len()
        )));
    }
    for (doc, embedding) in docs.iter_mut().zip(embeddings) {
        doc.embedding = embedding;
    }
    store.upsert(docs).map_err(to_hype_error)?;
    Ok(docs.len().div_ceil(config.embedding_batch_size.max(1)))
}

fn minsync_config(config: &HypeConfig) -> MinSyncConfig {
    let mut minsync_config = MinSyncConfig::default_for(SOURCE_ID);
    minsync_config.collection.path = STORE_DIR.to_string();
    minsync_config.chunker.id = "hype-canonical".to_string();
    minsync_config.embedder.id = config.embedder_model.clone();
    minsync_config.embedder.base_url = Some(config.embedder_base_url.clone());
    minsync_config.embedder.batch_size = config.embedding_batch_size;
    minsync_config.vectorstore.id = "lancedb".to_string();
    let mut options = toml::map::Map::new();
    options.insert(
        "dimension".to_string(),
        toml::Value::Integer(i64::from(config.vector_dimension)),
    );
    minsync_config.vectorstore.options = toml::Value::Table(options);
    minsync_config
}

fn delete_stale(store: &mut dyn VectorStore, seen_token: &str) -> Result<()> {
    store
        .delete_by_filter(&Filter::And(vec![
            Filter::Eq("source_id".to_string(), SOURCE_ID.to_string()),
            Filter::Neq("seen_token".to_string(), seen_token.to_string()),
        ]))
        .map_err(to_hype_error)?;
    Ok(())
}

fn save_cursor(dir: &Path, seen_token: &str, embedder_id: &str) -> Result<()> {
    Cursor {
        source_id: SOURCE_ID.to_string(),
        last_synced_at: chrono::Utc::now().to_rfc3339(),
        manifest_hash: seen_token.to_string(),
        chunk_schema_id: CHUNK_SCHEMA_ID.to_string(),
        embedder_id: embedder_id.to_string(),
        collection_path: STORE_DIR.to_string(),
    }
    .save(&dir.join("cursor.json"))
    .map_err(to_hype_error)
}

fn semantic_seen_token(chunks: &[crate::types::Chunk]) -> String {
    let mut material = String::new();
    for chunk in chunks {
        material.push_str(&chunk.chunk_id);
        material.push('\0');
        material.push_str(&content_hash(&chunk.text));
        material.push('\0');
    }
    content_hash(&material)
}

fn to_hype_error(error: minsync::error::MinSyncError) -> Error {
    match error {
        minsync::error::MinSyncError::NeverSynced => Error::SemanticIndexMissing,
        minsync::error::MinSyncError::Embedding(message)
            if message.contains("Connection refused")
                || message.contains("error sending request") =>
        {
            Error::EmbedderUnavailable
        }
        other => Error::MinSync(other.to_string()),
    }
}
