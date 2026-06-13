use crate::{archive::Archive, types::SearchHit, Error, Result};
use rusqlite::params;

pub fn keyword_search(archive: &Archive, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    keyword_search_with_snippet(archive, query, limit, 80)
}

pub fn keyword_search_with_snippet(
    archive: &Archive,
    query: &str,
    limit: usize,
    snippet_length: usize,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    let pattern = format!("%{}%", query.trim());
    let mut stmt = archive
        .connection()
        .prepare(
            "SELECT chunk_id FROM chunks
         WHERE text LIKE ?1
         ORDER BY started_at, chunk_id
         LIMIT ?2",
        )
        .map_err(Error::Sql)?;
    let ids = stmt
        .query_map(params![pattern, limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(Error::Sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::Sql)?;
    hydrate_hits(archive, ids, "keyword", query, snippet_length)
}

pub fn bm25_search(archive: &Archive, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    bm25_search_with_snippet(archive, query, limit, 80)
}

pub fn bm25_search_with_snippet(
    archive: &Archive,
    query: &str,
    limit: usize,
    snippet_length: usize,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    let mut stmt = archive
        .connection()
        .prepare(
            "SELECT c.chunk_id, bm25(chunks_fts) AS rank
         FROM chunks_fts
         JOIN chunks c ON c.rowid = chunks_fts.rowid
         WHERE chunks_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
        )
        .map_err(Error::Sql)?;
    let ids = stmt
        .query_map(params![query.trim(), limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(Error::Sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::Sql)?;
    hydrate_hits(archive, ids, "bm25", query, snippet_length)
}

pub(crate) fn hydrate_hits(
    archive: &Archive,
    ids: Vec<String>,
    ranker: &'static str,
    query: &str,
    snippet_length: usize,
) -> Result<Vec<SearchHit>> {
    ids.into_iter()
        .enumerate()
        .map(|(idx, id)| {
            let chunk = archive.get_chunk(&id)?.ok_or(Error::MissingChunk(id))?;
            Ok(SearchHit {
                ranker,
                rank: idx + 1,
                chunk_id: chunk.chunk_id,
                chat_name: chunk.chat_name,
                sender_nickname: chunk.sender_nickname,
                started_at: chunk.started_at,
                ended_at: chunk.ended_at,
                snippet: snippet(&chunk.text, query, snippet_length),
                score: 1.0 / ((idx + 1) as f64),
                parent_chunk_ids: chunk.parent_chunk_ids,
            })
        })
        .collect()
}

fn snippet(text: &str, query: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let start = trimmed.find(query).unwrap_or(0);
    trimmed
        .chars()
        .skip(start.saturating_sub(20))
        .take(max_chars)
        .collect()
}
