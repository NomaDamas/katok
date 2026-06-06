use super::{Archive, ChunkDraft};
use crate::{
    types::{RawMessage, SyncReport},
    Error, Result,
};
use rusqlite::params;

impl Archive {
    pub fn sync_messages(&self, messages: &[RawMessage]) -> Result<SyncReport> {
        let mut inserted = 0usize;
        for message in messages {
            self.upsert_chat(message)?;
            inserted += self.insert_message(message)?;
            self.update_cursor(message)?;
        }
        Ok(SyncReport {
            inserted_messages: inserted,
            updated_messages: 0,
            total_messages: self.count_rows("messages")?,
            chunks: self.count_rows("chunks")?,
        })
    }

    pub fn replace_chunks(&self, chunks: &[ChunkDraft]) -> Result<()> {
        self.conn
            .execute_batch(
                "DELETE FROM chunk_parent_refs;
             DELETE FROM chunk_messages;
             DELETE FROM chunks;
             DELETE FROM chunks_fts;",
            )
            .map_err(Error::Sql)?;
        for chunk in chunks {
            self.insert_chunk(chunk)?;
        }
        self.rebuild_parent_refs()
    }

    fn upsert_chat(&self, message: &RawMessage) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO chats(chat_id, chat_name, chat_type)
             VALUES (?1, ?2, ?3)",
                params![message.chat_id, message.chat_name, message.chat_type],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }

    fn insert_message(&self, message: &RawMessage) -> Result<usize> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO messages
             (account_hash, chat_id, chat_name, chat_type, message_id, sender_id,
              sender_nickname, timestamp, text, message_type, reply_to_message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    message.account_hash,
                    message.chat_id,
                    message.chat_name,
                    message.chat_type,
                    message.message_id,
                    message.sender_id,
                    message.sender_nickname,
                    message.timestamp.to_rfc3339(),
                    message.text,
                    message.message_type,
                    message.reply_to_message_id
                ],
            )
            .map_err(Error::Sql)
    }

    fn update_cursor(&self, message: &RawMessage) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO sync_cursors(source_id, cursor_value)
             VALUES (?1, ?2)",
                params![message.account_hash, message.timestamp.to_rfc3339()],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }

    fn insert_chunk(&self, chunk: &ChunkDraft) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO chunks
             (chunk_id, account_hash, chat_id, chat_name, sender_nickname,
              started_at, ended_at, text, message_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    chunk.chunk_id,
                    chunk.account_hash,
                    chunk.chat_id,
                    chunk.chat_name,
                    chunk.sender_nickname,
                    chunk.started_at,
                    chunk.ended_at,
                    chunk.text,
                    chunk.message_ids.len()
                ],
            )
            .map_err(Error::Sql)?;
        self.conn
            .execute(
                "INSERT INTO chunks_fts(rowid, chunk_id, text)
             VALUES ((SELECT rowid FROM chunks WHERE chunk_id = ?1), ?1, ?2)",
                params![chunk.chunk_id, chunk.text],
            )
            .map_err(Error::Sql)?;
        for (idx, message_id) in chunk.message_ids.iter().enumerate() {
            self.conn
                .execute(
                    "INSERT INTO chunk_messages(chunk_id, message_id, ordinal)
                 VALUES (?1, ?2, ?3)",
                    params![chunk.chunk_id, message_id, idx],
                )
                .map_err(Error::Sql)?;
        }
        Ok(())
    }

    fn count_rows(&self, table: &str) -> Result<usize> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        self.conn
            .query_row(&sql, [], |row| row.get::<_, i64>(0))
            .map(|count| count as usize)
            .map_err(Error::Sql)
    }

    fn rebuild_parent_refs(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM reply_edges;")
            .map_err(Error::Sql)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO reply_edges
                (child_message_id, parent_message_id, unresolved_reason)
             SELECT message_id, reply_to_message_id, 'parent_not_in_archive'
             FROM messages
             WHERE reply_to_message_id IS NOT NULL",
                [],
            )
            .map_err(Error::Sql)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO chunk_parent_refs(child_chunk_id, parent_chunk_id)
             SELECT child.chunk_id, parent.chunk_id
             FROM messages child_msg
             JOIN chunk_messages child ON child.message_id = child_msg.message_id
             JOIN chunk_messages parent ON parent.message_id = child_msg.reply_to_message_id
             WHERE child_msg.reply_to_message_id IS NOT NULL
               AND child.chunk_id != parent.chunk_id",
                [],
            )
            .map_err(Error::Sql)?;
        self.conn
            .execute(
                "UPDATE reply_edges
             SET child_chunk_id = (
                SELECT child.chunk_id FROM chunk_messages child
                WHERE child.message_id = reply_edges.child_message_id LIMIT 1
             ),
             parent_chunk_id = (
                SELECT parent.chunk_id FROM chunk_messages parent
                WHERE parent.message_id = reply_edges.parent_message_id LIMIT 1
             ),
             unresolved_reason = CASE
                WHEN (
                    SELECT parent.chunk_id FROM chunk_messages parent
                    WHERE parent.message_id = reply_edges.parent_message_id LIMIT 1
                ) IS NULL THEN 'parent_not_in_archive'
                ELSE NULL
             END",
                [],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }
}
