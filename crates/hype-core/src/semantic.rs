use crate::{archive::Archive, search::hydrate_hits, types::SearchHit, Error, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn semantic_source_dir(root: &Path) -> PathBuf {
    root.join("source").join("chunks")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SemanticDocument {
    pub chunk_id: String,
    pub path: PathBuf,
}

pub fn planned_semantic_documents(archive: &Archive, dir: &Path) -> Result<Vec<SemanticDocument>> {
    let dir = semantic_source_dir(dir);
    archive
        .all_chunks()?
        .into_iter()
        .map(|chunk| {
            Ok(SemanticDocument {
                path: document_path(&dir, &chunk.chunk_id),
                chunk_id: chunk.chunk_id,
            })
        })
        .collect()
}

pub fn write_semantic_documents(archive: &Archive, dir: &Path) -> Result<usize> {
    let dir = semantic_source_dir(dir);
    crate::paths::ensure_private_dir(&dir)?;
    let chunks = archive.all_chunks()?;
    for chunk in &chunks {
        let path = document_path(&dir, &chunk.chunk_id);
        let mut file = fs::File::create(&path).map_err(Error::Io)?;
        writeln!(file, "chunk_id: {}", chunk.chunk_id).map_err(Error::Io)?;
        writeln!(file, "chat_id: {}", chunk.chat_id).map_err(Error::Io)?;
        writeln!(file, "sender_nickname: {}", chunk.sender_nickname).map_err(Error::Io)?;
        writeln!(file, "time_range: {}..{}", chunk.started_at, chunk.ended_at)
            .map_err(Error::Io)?;
        writeln!(
            file,
            "parent_chunk_ids: {}",
            chunk.parent_chunk_ids.join(",")
        )
        .map_err(Error::Io)?;
        writeln!(file, "---").map_err(Error::Io)?;
        writeln!(file, "{}", chunk.text).map_err(Error::Io)?;
    }
    if let Some(root) = dir.parent().and_then(Path::parent) {
        fs::write(root.join("INDEXED_WITH_MOCK"), b"mock\n").map_err(Error::Io)?;
    }
    Ok(chunks.len())
}

pub fn semantic_search(
    archive: &Archive,
    dir: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    semantic_search_with_snippet(archive, dir, query, limit, 80)
}

pub fn semantic_search_with_snippet(
    archive: &Archive,
    dir: &Path,
    query: &str,
    limit: usize,
    snippet_length: usize,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    if !dir.exists() {
        return Err(Error::SemanticIndexMissing);
    }
    let dir = semantic_source_dir(dir);
    if !dir.exists() {
        return Err(Error::SemanticIndexMissing);
    }
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let mut scored = Vec::new();
    for entry in fs::read_dir(dir).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(Error::Io)?;
        let score = terms.iter().filter(|term| content.contains(**term)).count();
        if score > 0 {
            scored.push((score, chunk_id_from_path(&path)?));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let ids = scored
        .into_iter()
        .map(|(_, id)| id)
        .take(limit)
        .collect::<Vec<_>>();
    hydrate_hits(archive, ids, "semantic", query, snippet_length)
}

fn document_path(dir: &Path, chunk_id: &str) -> PathBuf {
    dir.join(format!("{chunk_id}.md"))
}

fn chunk_id_from_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(std::string::ToString::to_string)
        .ok_or_else(|| Error::InvalidSemanticPath(path.to_path_buf()))
}
