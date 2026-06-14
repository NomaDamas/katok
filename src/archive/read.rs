use super::{Archive, StoredMessage};
use crate::{
    types::{Chunk, ChunkSummary},
    Error, Result,
};
use rusqlite::{params, OptionalExtension};

impl Archive {
    pub fn chunks_for_chat(&self, chat_id: &str) -> Result<Vec<ChunkSummary>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT chunk_id, chat_id, chat_name, sender_nickname, started_at,
                    ended_at, message_count
             FROM chunks
             WHERE chat_id = ?1
             ORDER BY started_at, chunk_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![chat_id], |row| {
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
            })
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        rows.into_iter()
            .map(|mut summary| {
                summary.parent_chunk_ids = self.parent_chunks(&summary.chunk_id)?;
                summary.window_parent_ids = self.window_parent_ids(&summary.chunk_id)?;
                Ok(summary)
            })
            .collect()
    }

    pub fn get_chunk(&self, chunk_id: &str) -> Result<Option<Chunk>> {
        let row = self
            .conn
            .query_row(
                "SELECT chunk_id, chat_id, chat_name, sender_nickname, started_at,
                    ended_at, text, message_count
             FROM chunks WHERE chunk_id = ?1",
                params![chunk_id],
                |row| {
                    Ok(Chunk {
                        chunk_id: row.get(0)?,
                        chat_id: row.get(1)?,
                        chat_name: row.get(2)?,
                        sender_nickname: row.get(3)?,
                        started_at: row.get(4)?,
                        ended_at: row.get(5)?,
                        text: row.get(6)?,
                        message_count: row.get::<_, i64>(7)? as usize,
                        message_ids: Vec::new(),
                        parent_chunk_ids: Vec::new(),
                        window_parent_ids: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(Error::Sql)?;

        match row {
            Some(mut chunk) => {
                chunk.message_ids = self.chunk_messages(chunk_id)?;
                chunk.parent_chunk_ids = self.parent_chunks(chunk_id)?;
                chunk.window_parent_ids = self.window_parent_ids(chunk_id)?;
                Ok(Some(chunk))
            }
            None => Ok(None),
        }
    }

    pub fn all_chunks(&self) -> Result<Vec<Chunk>> {
        let mut stmt = self
            .conn
            .prepare("SELECT chunk_id FROM chunks ORDER BY started_at, chunk_id")
            .map_err(Error::Sql)?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        ids.into_iter()
            .map(|id| {
                self.get_chunk(&id)
                    .and_then(|chunk| chunk.ok_or(Error::MissingChunk(id)))
            })
            .collect()
    }

    pub fn raw_messages(&self) -> Result<Vec<StoredMessage>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT account_hash, chat_id, chat_name, chat_type, message_id,
                    sender_nickname, timestamp, text, message_type
             FROM messages ORDER BY chat_id, timestamp, message_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map([], StoredMessage::from_row)
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    fn chunk_messages(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT message_id FROM chunk_messages WHERE chunk_id = ?1 ORDER BY ordinal")
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![chunk_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    pub(super) fn parent_chunks(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT parent_chunk_id FROM chunk_parent_refs
             WHERE child_chunk_id = ?1 ORDER BY parent_chunk_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![chunk_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }
}
