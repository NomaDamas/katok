use super::{Archive, ChunkDraft, ParentChunkDraft};
use crate::{
    types::{RawMessage, SyncReport},
    Error, Result,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageChange {
    Inserted,
    Updated,
    Unchanged,
}

impl Archive {
    pub fn sync_messages(&self, messages: &[RawMessage]) -> Result<SyncReport> {
        let mut inserted = 0usize;
        let mut updated = 0usize;

        // A chat row only depends on (chat_id, chat_name, chat_type), so writing it once per
        // message meant hundreds of thousands of no-op upserts for a few hundred chats.
        let mut seen_chats: HashSet<&str> = HashSet::new();
        // Likewise the cursor: it used to be rewritten per message, which both cost a write per
        // row and left whichever message happened to be iterated last as the stored value rather
        // than the newest one.
        let mut cursors: HashMap<&str, &DateTime<Utc>> = HashMap::new();

        // Chats whose messages actually changed. Only these need their chunks recomputed;
        // chunk boundaries never reach across chats, so the rest are provably untouched.
        let mut touched_chats: Vec<String> = Vec::new();
        let mut touched_seen: HashSet<&str> = HashSet::new();

        for message in messages {
            if seen_chats.insert(message.chat_id.as_str()) {
                self.upsert_chat(message)?;
            }
            let change = self.upsert_message(message)?;
            if change != MessageChange::Unchanged && touched_seen.insert(message.chat_id.as_str()) {
                touched_chats.push(message.chat_id.clone());
            }
            inserted += usize::from(change == MessageChange::Inserted);
            updated += usize::from(change == MessageChange::Updated);
            cursors
                .entry(message.account_hash.as_str())
                .and_modify(|newest| {
                    if message.timestamp > **newest {
                        *newest = &message.timestamp;
                    }
                })
                .or_insert(&message.timestamp);
        }

        for (source_id, newest) in cursors {
            self.update_cursor(source_id, newest)?;
        }

        Ok(SyncReport {
            inserted_messages: inserted,
            updated_messages: updated,
            total_messages: self.count_rows("messages")?,
            chunks: self.count_rows("chunks")?,
            rebuilt_chats: touched_chats.len(),
            // Filled in by the caller, which is the only layer that sees every stage.
            timings_ms: Default::default(),
            touched_chats,
        })
    }

    /// Drop every chunk artifact belonging to `chat_id`.
    ///
    /// The FTS rows go first: they are located through `chunks`, which the last statement
    /// removes.
    fn delete_chat_chunks(&self, chat_id: &str) -> Result<()> {
        for sql in [
            "DELETE FROM chunks_fts
             WHERE chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id = ?1)",
            "DELETE FROM chunk_messages
             WHERE chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id = ?1)",
            "DELETE FROM chunk_parent_refs
             WHERE child_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id = ?1)
                OR parent_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id = ?1)",
            "DELETE FROM parent_chunk_children
             WHERE parent_id IN (SELECT parent_id FROM parent_chunks WHERE chat_id = ?1)",
            "DELETE FROM parent_chunks WHERE chat_id = ?1",
            "DELETE FROM chunks WHERE chat_id = ?1",
        ] {
            self.conn
                .execute(sql, params![chat_id])
                .map_err(Error::Sql)?;
        }
        Ok(())
    }

    /// Replace the chunks of `chat_ids` only, leaving every other chat's rows in place.
    ///
    /// Safe because a chunk boundary is decided by looking at the previous message alone and
    /// never reaches across chats, so recomputing one chat yields byte-identical rows to a full
    /// rebuild — `chunk_id` is derived from content, not from position in the archive.
    pub fn replace_chunks_for_chats(
        &self,
        chat_ids: &[String],
        chunks: &[ChunkDraft],
        parents: &[ParentChunkDraft],
    ) -> Result<()> {
        for chat_id in chat_ids {
            self.delete_chat_chunks(chat_id)?;
        }
        for chunk in chunks {
            self.insert_chunk(chunk)?;
        }
        for parent in parents {
            self.insert_parent_chunk(parent)?;
        }
        self.rebuild_parent_refs()
    }

    pub fn replace_chunks(
        &self,
        chunks: &[ChunkDraft],
        parents: &[ParentChunkDraft],
    ) -> Result<()> {
        self.conn
            .execute_batch(
                "DELETE FROM chunk_parent_refs;
             DELETE FROM parent_chunk_children;
             DELETE FROM parent_chunks;
             DELETE FROM chunk_messages;
             DELETE FROM chunks;
             DELETE FROM chunks_fts;",
            )
            .map_err(Error::Sql)?;
        for chunk in chunks {
            self.insert_chunk(chunk)?;
        }
        for parent in parents {
            self.insert_parent_chunk(parent)?;
        }
        self.rebuild_parent_refs()
    }

    fn upsert_chat(&self, message: &RawMessage) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO chats(chat_id, chat_name, chat_type)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET
                 chat_name = excluded.chat_name,
                 chat_type = excluded.chat_type
             WHERE chats.chat_name <> excluded.chat_name
                OR chats.chat_type <> excluded.chat_type",
            )
            .map_err(Error::Sql)?
            .execute(params![
                message.chat_id,
                message.chat_name,
                message.chat_type
            ])
            .map_err(Error::Sql)?;
        Ok(())
    }

    fn upsert_message(&self, message: &RawMessage) -> Result<MessageChange> {
        let exists = self.message_exists(message)?;
        let changed = self
            .conn
            .prepare_cached(
                "INSERT INTO messages
             (account_hash, chat_id, chat_name, chat_type, message_id, sender_id,
              sender_nickname, timestamp, text, message_type, reply_to_message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(account_hash, chat_id, message_id) DO UPDATE SET
                 chat_name = excluded.chat_name,
                 chat_type = excluded.chat_type,
                 sender_nickname = excluded.sender_nickname,
                 reply_to_message_id = excluded.reply_to_message_id
             WHERE messages.chat_name <> excluded.chat_name
                OR messages.chat_type <> excluded.chat_type
                OR messages.sender_nickname <> excluded.sender_nickname
                OR messages.reply_to_message_id IS NOT excluded.reply_to_message_id",
            )
            .map_err(Error::Sql)?
            .execute(params![
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
            ])
            .map_err(Error::Sql)?;
        Ok(match (exists, changed) {
            (false, 1) => MessageChange::Inserted,
            (true, 1) => MessageChange::Updated,
            _ => MessageChange::Unchanged,
        })
    }

    fn message_exists(&self, message: &RawMessage) -> Result<bool> {
        self.conn
            .prepare_cached(
                "SELECT EXISTS(
                    SELECT 1 FROM messages
                    WHERE account_hash = ?1 AND chat_id = ?2 AND message_id = ?3
                 )",
            )
            .map_err(Error::Sql)?
            .query_row(
                params![message.account_hash, message.chat_id, message.message_id],
                |row| row.get(0),
            )
            .map_err(Error::Sql)
    }

    fn update_cursor(&self, source_id: &str, newest: &DateTime<Utc>) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO sync_cursors(source_id, cursor_value)
             VALUES (?1, ?2)",
                params![source_id, newest.to_rfc3339()],
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

    fn insert_parent_chunk(&self, parent: &ParentChunkDraft) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO parent_chunks
             (parent_id, account_hash, chat_id, chat_name, started_at,
              ended_at, text, message_count, child_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    parent.parent_id,
                    parent.account_hash,
                    parent.chat_id,
                    parent.chat_name,
                    parent.started_at,
                    parent.ended_at,
                    parent.text,
                    parent.message_count,
                    parent.child_chunk_ids.len()
                ],
            )
            .map_err(Error::Sql)?;
        for (idx, chunk_id) in parent.child_chunk_ids.iter().enumerate() {
            self.conn
                .execute(
                    "INSERT INTO parent_chunk_children(parent_id, chunk_id, ordinal)
                 VALUES (?1, ?2, ?3)",
                    params![parent.parent_id, chunk_id, idx],
                )
                .map_err(Error::Sql)?;
        }
        Ok(())
    }

    /// Number of chunk rows currently in the archive.
    pub fn chunk_count(&self) -> Result<usize> {
        self.count_rows("chunks")
    }

    /// The settings and chunker version the stored chunks were built with, if recorded.
    pub fn stored_chunk_settings(&self) -> Result<Option<(i64, i64, i64)>> {
        self.conn
            .query_row(
                "SELECT group_gap_seconds, direct_gap_seconds, chunker_version
                 FROM chunk_settings WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Error::Sql)
    }

    /// Record the settings the current chunk rows were built with.
    pub fn record_chunk_settings(
        &self,
        group_gap_seconds: i64,
        direct_gap_seconds: i64,
        chunker_version: i64,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO chunk_settings
                 (id, group_gap_seconds, direct_gap_seconds, chunker_version)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 group_gap_seconds = excluded.group_gap_seconds,
                 direct_gap_seconds = excluded.direct_gap_seconds,
                 chunker_version = excluded.chunker_version",
                params![group_gap_seconds, direct_gap_seconds, chunker_version],
            )
            .map_err(Error::Sql)?;
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
