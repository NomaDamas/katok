use super::Archive;
use crate::{
    types::{Chunk, ChunkContext, ChunkSummary, ParentChunk},
    Error, Result,
};
use rusqlite::{params, OptionalExtension};

impl Archive {
    pub fn get_parent_chunk(&self, parent_id: &str) -> Result<Option<ParentChunk>> {
        let row = self
            .conn
            .query_row(
                "SELECT parent_id, chat_id, chat_name, started_at, ended_at,
                    text, message_count
             FROM parent_chunks WHERE parent_id = ?1",
                params![parent_id],
                |row| {
                    Ok(ParentChunk {
                        parent_id: row.get(0)?,
                        chat_id: row.get(1)?,
                        chat_name: row.get(2)?,
                        started_at: row.get(3)?,
                        ended_at: row.get(4)?,
                        text: row.get(5)?,
                        message_count: row.get::<_, i64>(6)? as usize,
                        child_chunk_ids: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(Error::Sql)?;
        match row {
            Some(mut parent) => {
                parent.child_chunk_ids = self.parent_child_chunks(parent_id)?;
                Ok(Some(parent))
            }
            None => Ok(None),
        }
    }

    pub fn parent_windows_for_child(&self, chunk_id: &str) -> Result<Vec<ParentChunk>> {
        self.window_parent_ids(chunk_id)?
            .into_iter()
            .map(|id| {
                self.get_parent_chunk(&id)
                    .and_then(|parent| parent.ok_or(Error::MissingChunk(id)))
            })
            .collect()
    }

    pub fn chunk_context(&self, chunk_id: &str) -> Result<Option<ChunkContext>> {
        let Some(chunk) = self.get_chunk(chunk_id)? else {
            return Ok(None);
        };
        let previous = self.neighbor_chunk(&chunk, Neighbor::Previous)?;
        let next = self.neighbor_chunk(&chunk, Neighbor::Next)?;
        let parent_windows = self.parent_windows_for_child(chunk_id)?;
        Ok(Some(ChunkContext {
            chunk,
            previous,
            next,
            parent_windows,
        }))
    }

    pub fn all_parent_chunks(&self) -> Result<Vec<ParentChunk>> {
        let mut stmt = self
            .conn
            .prepare("SELECT parent_id FROM parent_chunks ORDER BY started_at, parent_id")
            .map_err(Error::Sql)?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        ids.into_iter()
            .map(|id| {
                self.get_parent_chunk(&id)
                    .and_then(|parent| parent.ok_or(Error::MissingChunk(id)))
            })
            .collect()
    }

    pub(super) fn window_parent_ids(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT parent_id FROM parent_chunk_children
             WHERE chunk_id = ?1 ORDER BY parent_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![chunk_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    fn parent_child_chunks(&self, parent_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT chunk_id FROM parent_chunk_children
             WHERE parent_id = ?1 ORDER BY ordinal",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![parent_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    fn neighbor_chunk(&self, chunk: &Chunk, direction: Neighbor) -> Result<Option<ChunkSummary>> {
        let (cmp, order) = match direction {
            Neighbor::Previous => ("<", "DESC"),
            Neighbor::Next => (">", "ASC"),
        };
        let sql = format!(
            "SELECT chunk_id, chat_id, chat_name, sender_nickname, started_at,
                    ended_at, message_count
             FROM chunks
             WHERE chat_id = ?1 AND (started_at, chunk_id) {cmp} (?2, ?3)
             ORDER BY started_at {order}, chunk_id {order}
             LIMIT 1"
        );
        let row = self
            .conn
            .query_row(
                &sql,
                params![chunk.chat_id, chunk.started_at, chunk.chunk_id],
                |row| {
                    Ok(ChunkSummary {
                        chunk_id: row.get(0)?,
                        chat_id: row.get(1)?,
                        chat_name: row.get(2)?,
                        sender_nickname: row.get(3)?,
                        started_at: row.get(4)?,
                        ended_at: row.get(5)?,
                        message_count: row.get::<_, i64>(6)? as usize,
                        parent_chunk_ids: Vec::new(),
                        window_parent_ids: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(Error::Sql)?;
        match row {
            Some(mut summary) => {
                summary.parent_chunk_ids = self.parent_chunks(&summary.chunk_id)?;
                summary.window_parent_ids = self.window_parent_ids(&summary.chunk_id)?;
                Ok(Some(summary))
            }
            None => Ok(None),
        }
    }
}

enum Neighbor {
    Previous,
    Next,
}
