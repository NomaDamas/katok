use super::model::DeletedMessage;
use super::{Archive, ChunkDraft, ParentChunkDraft};
use crate::{
    types::{RawMessage, SyncReport, TouchedChat},
    Error, Result,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// The statements [`Archive::replace_chunk_tail`] issues to drop a chat's chunk tail, in order.
///
/// Bound as `(?1 = chat_id, ?2 = the `started_at` floor or NULL)`. They live here rather than
/// inline so the query-plan test asserts against the text that actually runs; a copy in the test
/// would let a rewrite reintroduce a whole-table scan without anything failing.
///
/// The floor reads `started_at >= COALESCE(?2, '')` rather than `?2 IS NULL OR started_at >= ?2`.
/// The two select the same rows — `started_at` is non-empty RFC3339 text, so every row of the
/// chat compares `>= ''` — but only the first is a range SQLite can drive from
/// `idx_chunks_chat_started`. The `OR` form makes the whole term unusable as a bound, which left
/// each delete walking every chunk of the room even once the index existed, putting room size
/// back into a cost the tail scope is meant to bound.
///
/// The `chunks_fts` delete addresses rows by `rowid`, not by `chunk_id`. `chunk_id` is an
/// UNINDEXED fts5 column, so no index can serve it and SQLite has to ask fts5 for every row of
/// the archive and filter them itself — a cost that follows archive size and was the largest
/// single term left in a scoped rebuild. `rowid` is the one column fts5 can seek on: it is the
/// docid, and a rowid constraint is pushed into the virtual table (the plan's idxStr picks up
/// `=`), so the delete costs a b-tree lookup per deleted row and nothing per untouched row.
/// The rowid is the same rowid the chunk carries in `chunks` — see the `chunks_fts` invariant in
/// `schema.rs`, which `insert_chunk` writes and `search.rs` already reads back through.
pub const DELETE_CHAT_CHUNKS_STATEMENTS: [&str; 6] = [
    "DELETE FROM chunks_fts
     WHERE rowid IN (SELECT rowid FROM chunks
        WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))",
    "DELETE FROM chunk_messages
     WHERE chunk_id IN (SELECT chunk_id FROM chunks
        WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))",
    "DELETE FROM chunk_parent_refs
     WHERE child_chunk_id IN (SELECT chunk_id FROM chunks
            WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))
        OR parent_chunk_id IN (SELECT chunk_id FROM chunks
            WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))",
    "DELETE FROM parent_chunk_children
     WHERE parent_id IN (SELECT parent_id FROM parent_chunks
        WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))",
    "DELETE FROM parent_chunks
     WHERE chat_id = ?1 AND started_at >= COALESCE(?2, '')",
    "DELETE FROM chunks
     WHERE chat_id = ?1 AND started_at >= COALESCE(?2, '')",
];

/// Statements [`Archive::rebuild_reply_and_parent_refs_for_chats`] runs per touched chat.
///
/// Bound as `(?1 = chat_id)`. They live here so the query-plan test asserts against the text
/// that actually runs — a copy in the test would let a rewrite reintroduce an archive-wide
/// scan (for example `LIKE ? || '-%'` or an unindexed join) without anything failing.
///
/// Membership is by `messages.chat_id`, not by a `child_message_id` string prefix: the mint
/// invariant `message_id = {chat_id}-{log_id}` holds in production, but fixtures and any
/// pre-mint row are free-form, and the two are equivalent only when the mint holds. The
/// `chat_id` filter is the membership that is always correct; `idx_messages_chat_timestamp`
/// (its `chat_id` prefix) keeps it from scanning `messages`, and the outer `IN` is a PK lookup on `reply_edges` per id
/// so `reply_edges` itself is never scanned. See docs/incremental-chunking-tail-scope.md
/// "The reply/parent-ref pass is chat-local too".
pub const DELETE_REPLY_EDGES_FOR_CHAT: &str = "DELETE FROM reply_edges
     WHERE child_message_id IN (
        SELECT message_id FROM messages WHERE chat_id = ?1
     )";

pub const INSERT_REPLY_EDGES_FOR_CHAT: &str = "INSERT OR IGNORE INTO reply_edges
        (child_message_id, parent_message_id, unresolved_reason)
     SELECT message_id, reply_to_message_id, 'parent_not_in_archive'
     FROM messages
     WHERE reply_to_message_id IS NOT NULL
       AND chat_id = ?1";

/// Re-insert the refs a scoped rebuild deleted, child side.
///
/// The delete removes a ref when **either** side sits in the rebuilt chat, so
/// the re-insert has to cover both. Keyed on the child alone, an edge whose
/// parent lived in this chat but whose child lived elsewhere was deleted and
/// never restored, and its `reply_edges` row kept pointing at a chunk id that no
/// longer existed.
///
/// The two sides are separate statements rather than one `OR`, because SQLite
/// cannot drive an index from `(a.chat_id = ?1 OR b.chat_id = ?1)` and falls
/// back to scanning `messages` — which is exactly the archive-size cost the
/// scoped rebuild exists to avoid, and which the query-plan test catches.
///
/// Cross-chat edges cannot arise from the macOS reader, which mints
/// `message_id` as `{chat_id}-{logId}`. They can from `kakaocli` and `fixture`,
/// which take both ids verbatim from data this crate does not control — so the
/// invariant the scoping was argued from is not enforced where it would matter.
pub const INSERT_CHUNK_PARENT_REFS_FOR_CHAT: &str =
    "INSERT OR IGNORE INTO chunk_parent_refs(child_chunk_id, parent_chunk_id)
     SELECT child.chunk_id, parent.chunk_id
     FROM messages child_msg
     JOIN chunk_messages child ON child.message_id = child_msg.message_id
     JOIN chunk_messages parent ON parent.message_id = child_msg.reply_to_message_id
     WHERE child_msg.reply_to_message_id IS NOT NULL
       AND child_msg.chat_id = ?1
       AND child.chunk_id != parent.chunk_id";

/// Parent side of the same re-insert: edges pointing *into* this chat from a
/// child that lives in another one. Reached through `idx_messages_reply_to`.
pub const INSERT_CHUNK_PARENT_REFS_INTO_CHAT: &str =
    "INSERT OR IGNORE INTO chunk_parent_refs(child_chunk_id, parent_chunk_id)
     SELECT child.chunk_id, parent.chunk_id
     FROM messages parent_msg
     JOIN messages child_msg ON child_msg.reply_to_message_id = parent_msg.message_id
     JOIN chunk_messages child ON child.message_id = child_msg.message_id
     JOIN chunk_messages parent ON parent.message_id = parent_msg.message_id
     WHERE parent_msg.chat_id = ?1
       AND child.chunk_id != parent.chunk_id";

pub const RESOLVE_REPLY_EDGES_FOR_CHAT: &str = "UPDATE reply_edges
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
     END
     WHERE child_message_id IN (
        SELECT message_id FROM messages
        WHERE chat_id = ?1 AND reply_to_message_id IS NOT NULL
     )";

/// The statements of a per-chat ref rebuild, in order, for query-plan tests.
///
/// Both ref-insert directions are here: the delete drops a ref when either side
/// is in this chat, so only re-inserting the child side left the other half gone.
pub const SCOPED_REF_REBUILD_STATEMENTS: [&str; 5] = [
    DELETE_REPLY_EDGES_FOR_CHAT,
    INSERT_REPLY_EDGES_FOR_CHAT,
    INSERT_CHUNK_PARENT_REFS_FOR_CHAT,
    INSERT_CHUNK_PARENT_REFS_INTO_CHAT,
    RESOLVE_REPLY_EDGES_FOR_CHAT,
];

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

        // Chats whose messages actually changed, and where the earliest change landed in each.
        // Only these need their chunks recomputed; chunk boundaries never reach across chats, so
        // the rest are provably untouched. The earliest changed key scopes the rebuild to the
        // tail past the last stable window boundary (docs/incremental-chunking-tail-scope.md).
        // Insertion order is preserved so `rebuilt_chats` and the rebuild are deterministic.
        let mut touched_order: Vec<&str> = Vec::new();
        let mut earliest_changed: HashMap<&str, (String, &str)> = HashMap::new();

        for message in messages {
            if seen_chats.insert(message.chat_id.as_str()) {
                self.upsert_chat(message)?;
            }
            let change = self.upsert_message(message)?;
            if change != MessageChange::Unchanged {
                let key = (message.timestamp.to_rfc3339(), message.message_id.as_str());
                earliest_changed
                    .entry(message.chat_id.as_str())
                    .and_modify(|earliest| {
                        // Compare by the send-order key `(timestamp, message_id)`, matching
                        // `raw_messages ORDER BY chat_id, timestamp, message_id`.
                        if (key.0.as_str(), key.1) < (earliest.0.as_str(), earliest.1) {
                            *earliest = (key.0.clone(), key.1);
                        }
                    })
                    .or_insert_with(|| {
                        touched_order.push(message.chat_id.as_str());
                        (key.0, key.1)
                    });
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

        let touched_chats: Vec<TouchedChat> = touched_order
            .into_iter()
            .map(|chat_id| {
                let (timestamp, message_id) = &earliest_changed[chat_id];
                TouchedChat {
                    chat_id: chat_id.to_string(),
                    earliest_changed_timestamp: timestamp.clone(),
                    earliest_changed_message_id: message_id.to_string(),
                }
            })
            .collect();

        Ok(SyncReport {
            inserted_messages: inserted,
            updated_messages: updated,
            total_messages: self.count_rows("messages")?,
            chunks: self.count_rows("chunks")?,
            rebuilt_chats: touched_chats.len(),
            // Filled in by the caller, which is the only layer that sees every stage.
            timings_ms: Default::default(),
            touched_chats,
            // CLI sets this when `sync --touched` is requested; keep default off for byte-compat.
            include_touched: false,
        })
    }

    /// Drop the chunk artifacts of `chat_id` whose start is at or after `from_started_at`.
    ///
    /// `None` drops the whole chat. A non-null floor drops only the tail: because the recompute
    /// start is a parent-window boundary preceded by a gap over `DEFAULT_PARENT_WINDOW_SECONDS`,
    /// no kept chunk or window shares that `started_at`, so a plain `started_at >= floor`
    /// comparison needs no message-id tiebreak (docs/incremental-chunking-tail-scope.md).
    ///
    /// The FTS rows go first: they are located through `chunks`, which the last statement removes.
    /// The statement texts and how they are indexed are in [`DELETE_CHAT_CHUNKS_STATEMENTS`].
    fn delete_chat_chunks(&self, chat_id: &str, from_started_at: Option<&str>) -> Result<()> {
        for sql in DELETE_CHAT_CHUNKS_STATEMENTS {
            self.conn
                .execute(sql, params![chat_id, from_started_at])
                .map_err(Error::Sql)?;
        }
        Ok(())
    }

    /// Replace one chat's chunk tail from `from_started_at` onward, leaving its earlier chunks and
    /// every other chat's rows in place. `None` replaces the whole chat.
    ///
    /// Safe because a chunk boundary is decided by looking at the previous message alone and never
    /// reaches across chats, and `from_started_at` is a stable parent-window boundary, so the kept
    /// prefix is byte-identical to a full rebuild — `chunk_id` is derived from content, not from
    /// position in the archive. Reply and parent references are chat-local (see
    /// `docs/incremental-chunking-tail-scope.md`) and are rebuilt for the touched chats by the
    /// caller after every touched chat is replaced.
    pub fn replace_chunk_tail(
        &self,
        chat_id: &str,
        from_started_at: Option<&str>,
        chunks: &[ChunkDraft],
        parents: &[ParentChunkDraft],
    ) -> Result<()> {
        self.delete_chat_chunks(chat_id, from_started_at)?;
        for chunk in chunks {
            self.insert_chunk(chunk)?;
        }
        for parent in parents {
            self.insert_parent_chunk(parent)?;
        }
        Ok(())
    }

    /// Rebuild reply edges and cross-chunk parent references for the whole archive.
    ///
    /// Used by the full-rebuild path (`replace_chunks`). Scoped sync uses
    /// [`Self::rebuild_reply_and_parent_refs_for_chats`] instead — every edge is owned by
    /// exactly one `chat_id`, so the union of per-chat rebuilds equals this wipe-and-refill.
    pub fn rebuild_reply_and_parent_refs(&self) -> Result<()> {
        self.rebuild_parent_refs_all()
    }

    /// Rebuild reply edges and cross-chunk parent references for only the given chats.
    ///
    /// Scope unit is `chat_id` (never account): a multi-account room must rebuild as one unit.
    /// Untouched chats' rows are left in place and are byte-identical to a full rebuild because
    /// their child messages did not change this sync. Statement texts are
    /// [`SCOPED_REF_REBUILD_STATEMENTS`].
    pub fn rebuild_reply_and_parent_refs_for_chats(&self, chat_ids: &[&str]) -> Result<()> {
        let mut seen: HashSet<&str> = HashSet::new();
        for &chat_id in chat_ids {
            if !seen.insert(chat_id) {
                continue;
            }
            self.rebuild_parent_refs_for_chat(chat_id)?;
        }
        Ok(())
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
        self.rebuild_parent_refs_all()
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
                 sender_id = excluded.sender_id,
                 sender_nickname = excluded.sender_nickname,
                 timestamp = excluded.timestamp,
                 text = excluded.text,
                 message_type = excluded.message_type,
                 reply_to_message_id = excluded.reply_to_message_id
             WHERE messages.chat_name <> excluded.chat_name
                OR messages.chat_type <> excluded.chat_type
                OR messages.sender_id <> excluded.sender_id
                OR messages.sender_nickname <> excluded.sender_nickname
                OR messages.timestamp <> excluded.timestamp
                OR messages.text <> excluded.text
                OR messages.message_type <> excluded.message_type
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

    fn rebuild_parent_refs_all(&self) -> Result<()> {
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

    /// Remove archived messages the source no longer has, within the range the
    /// source actually covers.
    ///
    /// Sync otherwise only ever upserts, so a message deleted or redacted
    /// upstream stays in the archive, in its chunk text, in `chunks_fts` and in
    /// the embedded parent window forever. For a tool whose subject matter is
    /// private conversation that is a privacy defect, not staleness.
    ///
    /// **The window is the whole safety story.** KakaoTalk prunes its own
    /// database over time, and outliving that is precisely why this archive
    /// exists — so "absent from the source" cannot mean "delete". A message is
    /// removed only when it falls *inside* the `[oldest, newest]` span the
    /// source still reports for that chat and is missing from it. Anything older
    /// than the source's reach is history the source has forgotten and this
    /// archive is keeping, and it is never touched.
    ///
    /// Two further guards: a chat the source did not mention at all is skipped
    /// entirely (an empty or partial read must not read as a mass deletion), and
    /// the caller decides whether to apply or only report.
    pub fn reconcile_deletions(
        &self,
        source: &[RawMessage],
        apply: bool,
    ) -> Result<Vec<DeletedMessage>> {
        use std::collections::{HashMap, HashSet};

        let mut spans: HashMap<&str, (String, String)> = HashMap::new();
        let mut present: HashSet<(&str, &str)> = HashSet::new();
        for message in source {
            let ts = message.timestamp.to_rfc3339();
            spans
                .entry(message.chat_id.as_str())
                .and_modify(|(oldest, newest)| {
                    if ts < *oldest {
                        oldest.clone_from(&ts);
                    }
                    if ts > *newest {
                        newest.clone_from(&ts);
                    }
                })
                .or_insert_with(|| (ts.clone(), ts.clone()));
            present.insert((message.chat_id.as_str(), message.message_id.as_str()));
        }

        let mut doomed = Vec::new();
        for (chat_id, (oldest, newest)) in &spans {
            let mut stmt = self
                .conn
                .prepare_cached(
                    "SELECT message_id FROM messages
                     WHERE chat_id = ?1 AND timestamp >= ?2 AND timestamp <= ?3",
                )
                .map_err(Error::Sql)?;
            let rows = stmt
                .query_map(params![chat_id, oldest, newest], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(Error::Sql)?;
            for row in rows {
                let message_id = row.map_err(Error::Sql)?;
                if !present.contains(&(chat_id, message_id.as_str())) {
                    doomed.push(DeletedMessage {
                        chat_id: (*chat_id).to_string(),
                        message_id,
                    });
                }
            }
        }

        if apply {
            for victim in &doomed {
                self.conn
                    .execute(
                        "DELETE FROM messages WHERE chat_id = ?1 AND message_id = ?2",
                        params![victim.chat_id, victim.message_id],
                    )
                    .map_err(Error::Sql)?;
            }
        }
        Ok(doomed)
    }

    fn rebuild_parent_refs_for_chat(&self, chat_id: &str) -> Result<()> {
        for sql in SCOPED_REF_REBUILD_STATEMENTS {
            self.conn
                .execute(sql, params![chat_id])
                .map_err(Error::Sql)?;
        }
        Ok(())
    }
}
