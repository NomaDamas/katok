use crate::{archive::Archive, search::hydrate_parent_hits, types::SearchHit, Error, Result};
use std::{fs, io::Write, path::Path};

use super::{document_path, semantic_source_dir};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SemanticDocument {
    pub chunk_id: String,
    pub path: std::path::PathBuf,
}

pub fn planned_semantic_documents(archive: &Archive, dir: &Path) -> Result<Vec<SemanticDocument>> {
    let dir = semantic_source_dir(dir);
    archive
        .all_parent_chunks()?
        .into_iter()
        .map(|parent| {
            Ok(SemanticDocument {
                path: document_path(&dir, &parent.parent_id),
                chunk_id: parent.parent_id,
            })
        })
        .collect()
}

pub fn write_semantic_documents(archive: &Archive, dir: &Path) -> Result<usize> {
    let written = write_semantic_documents_plain(archive, dir)?;
    if let Some(root) = semantic_source_dir(dir).parent().and_then(Path::parent) {
        fs::write(root.join("INDEXED_WITH_MOCK"), b"mock\n").map_err(Error::Io)?;
    }
    Ok(written)
}

pub(crate) fn write_semantic_documents_plain(archive: &Archive, dir: &Path) -> Result<usize> {
    let dir = semantic_source_dir(dir);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(Error::Io)?;
    }
    crate::paths::ensure_private_dir(&dir)?;
    let parents = archive.all_parent_chunks()?;
    for parent in &parents {
        let path = document_path(&dir, &parent.parent_id);
        let mut file = fs::File::create(&path).map_err(Error::Io)?;
        writeln!(file, "parent_id: {}", parent.parent_id).map_err(Error::Io)?;
        writeln!(file, "unit: parent_window").map_err(Error::Io)?;
        writeln!(file, "chat_id: {}", parent.chat_id).map_err(Error::Io)?;
        writeln!(
            file,
            "time_range: {}..{}",
            parent.started_at, parent.ended_at
        )
        .map_err(Error::Io)?;
        writeln!(
            file,
            "child_chunk_ids: {}",
            parent.child_chunk_ids.join(",")
        )
        .map_err(Error::Io)?;
        writeln!(file, "---").map_err(Error::Io)?;
        writeln!(file, "{}", parent.text).map_err(Error::Io)?;
    }
    Ok(parents.len())
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
    hydrate_parent_hits(archive, ids, "semantic", query, snippet_length)
}

fn chunk_id_from_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(std::string::ToString::to_string)
        .ok_or_else(|| Error::InvalidSemanticPath(path.to_path_buf()))
}
