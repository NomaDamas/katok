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
    let fts_query = literal_fts_query(query);
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
        .query_map(params![fts_query, limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(Error::Sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::Sql)?;
    hydrate_hits(archive, ids, "bm25", query, snippet_length)
}

fn literal_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
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
                unit: "micro_chunk",
                rank: idx + 1,
                chunk_id: chunk.chunk_id,
                chat_id: chunk.chat_id,
                chat_name: chunk.chat_name,
                sender_nickname: chunk.sender_nickname,
                started_at: chunk.started_at,
                ended_at: chunk.ended_at,
                snippet: snippet(&chunk.text, query, snippet_length),
                score: 1.0 / ((idx + 1) as f64),
                parent_chunk_ids: chunk.parent_chunk_ids,
                child_chunk_ids: Vec::new(),
            })
        })
        .collect()
}

pub(crate) fn hydrate_parent_hits(
    archive: &Archive,
    ids: Vec<String>,
    ranker: &'static str,
    query: &str,
    snippet_length: usize,
) -> Result<Vec<SearchHit>> {
    ids.into_iter()
        .enumerate()
        .map(|(idx, id)| {
            let parent = archive
                .get_parent_chunk(&id)?
                .ok_or(Error::MissingChunk(id))?;
            Ok(SearchHit {
                ranker,
                unit: "parent_window",
                rank: idx + 1,
                chunk_id: parent.parent_id,
                chat_id: parent.chat_id,
                chat_name: parent.chat_name,
                sender_nickname: "multiple".to_string(),
                started_at: parent.started_at,
                ended_at: parent.ended_at,
                snippet: snippet(&parent.text, query, snippet_length),
                score: 1.0 / ((idx + 1) as f64),
                parent_chunk_ids: Vec::new(),
                child_chunk_ids: parent.child_chunk_ids,
            })
        })
        .collect()
}

fn snippet(text: &str, query: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    // `find` reports a BYTE offset while `skip` counts CHARS. Passing one to the
    // other overshoots by the encoding width — about 3x for Hangul — so a hit
    // late in a long Korean chunk skipped past the end and returned "".
    let start_char = trimmed
        .find(query)
        .map(|byte_offset| trimmed[..byte_offset].chars().count())
        .unwrap_or(0);
    trimmed
        .chars()
        .skip(start_char.saturating_sub(20))
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod snippet_tests {
    use super::snippet;

    #[test]
    fn korean_hit_late_in_a_long_chunk_is_still_shown() {
        // 3 bytes per char: a char-300 hit sits at byte 900, and skipping 900
        // chars ran off the end of a 605-char text.
        let text = format!("{}찾는말{}", "가".repeat(300), "나".repeat(302));
        let out = snippet(&text, "찾는말", 120);

        assert!(!out.is_empty(), "snippet must not be empty");
        assert!(
            out.contains("찾는말"),
            "snippet must contain the match: {out}"
        );
        assert_eq!(out.chars().count(), 120);
    }

    #[test]
    fn ascii_behaviour_is_unchanged() {
        let text = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let out = snippet(&text, "needle", 120);
        assert!(out.contains("needle"));
        assert!(out.starts_with(&"a".repeat(20)));
    }

    #[test]
    fn short_text_is_returned_whole() {
        assert_eq!(snippet("  짧은 글  ", "글", 120), "짧은 글");
    }
}
