mod embedder;
mod live;
mod mock;
mod store;

use crate::{archive::Archive, Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub use live::{
    committed_cursor, index_semantic_live, index_semantic_live_for_parents,
    semantic_search_live_with_config, SemanticIndexReport, STORE_DIR,
};
pub use mock::{
    planned_semantic_documents, planned_semantic_documents_for_parents, semantic_search,
    semantic_search_with_snippet, write_semantic_documents, write_semantic_documents_for_parents,
    SemanticDocument,
};

pub(crate) const CHUNK_SCHEMA_ID: &str = "katok-kakao-parent-window-v1";
pub(crate) const SOURCE_ID: &str = "katok-kakao-parent-windows";
pub const CURRENT_FILE: &str = "CURRENT";
pub const GENERATIONS_DIR: &str = "generations";

pub fn semantic_source_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("source").join("chunks")
}

pub fn archive_revision(archive: &Archive) -> Result<String> {
    let parents = archive.all_parent_chunks()?;
    Ok(archive_revision_for_parents(&parents))
}

pub(crate) fn archive_revision_for_parents(parents: &[crate::types::ParentChunk]) -> String {
    let mut material = String::new();
    for parent in parents {
        material.push_str(&parent.parent_id);
        material.push('\0');
        material.push_str(&content_hash(&parent.text));
        material.push('\0');
    }
    content_hash(&material)
}

pub fn current_generation(root: &Path) -> Result<PathBuf> {
    let pointer = root.join(CURRENT_FILE);
    let generation = std::fs::read_to_string(&pointer)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::SemanticIndexMissing
            } else {
                Error::Io(error)
            }
        })?
        .trim()
        .to_string();
    if generation.is_empty()
        || generation.contains('/')
        || generation.contains('\\')
        || generation == "."
        || generation == ".."
    {
        return Err(Error::SemanticIndexStale(
            "CURRENT contains an invalid generation id".to_string(),
        ));
    }
    let path = root.join(GENERATIONS_DIR).join(generation);
    if !path.is_dir() {
        return Err(Error::SemanticIndexStale(format!(
            "CURRENT generation is missing at {}",
            path.display()
        )));
    }
    Ok(path)
}

pub(crate) fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn document_path(dir: &std::path::Path, chunk_id: &str) -> std::path::PathBuf {
    dir.join(format!("{chunk_id}.md"))
}
