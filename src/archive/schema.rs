use crate::{Error, Result};
use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS messages (
            account_hash TEXT NOT NULL,
            chat_id TEXT NOT NULL,
            chat_name TEXT NOT NULL,
            chat_type TEXT NOT NULL,
            message_id TEXT NOT NULL,
            sender_id TEXT NOT NULL,
            sender_nickname TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            text TEXT NOT NULL,
            message_type TEXT NOT NULL,
            reply_to_message_id TEXT,
            PRIMARY KEY(account_hash, chat_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS chats (
            chat_id TEXT PRIMARY KEY,
            chat_name TEXT NOT NULL,
            chat_type TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_cursors (
            source_id TEXT PRIMARY KEY,
            cursor_value TEXT NOT NULL
        );
        -- The settings the current chunk rows were built with. Chunking is now scoped to the
        -- chats that changed, so a settings change would otherwise only reach rooms that happen
        -- to receive a message; recording them lets sync notice the drift and rebuild in full.
        CREATE TABLE IF NOT EXISTS chunk_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            group_gap_seconds INTEGER NOT NULL,
            direct_gap_seconds INTEGER NOT NULL,
            chunker_version INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS chunks (
            chunk_id TEXT PRIMARY KEY,
            account_hash TEXT NOT NULL,
            chat_id TEXT NOT NULL,
            chat_name TEXT NOT NULL,
            sender_nickname TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT NOT NULL,
            text TEXT NOT NULL,
            message_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS chunk_messages (
            chunk_id TEXT NOT NULL,
            message_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            PRIMARY KEY(chunk_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS chunk_parent_refs (
            child_chunk_id TEXT NOT NULL,
            parent_chunk_id TEXT NOT NULL,
            PRIMARY KEY(child_chunk_id, parent_chunk_id)
        );
        CREATE TABLE IF NOT EXISTS parent_chunks (
            parent_id TEXT PRIMARY KEY,
            account_hash TEXT NOT NULL,
            chat_id TEXT NOT NULL,
            chat_name TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT NOT NULL,
            text TEXT NOT NULL,
            message_count INTEGER NOT NULL,
            child_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS parent_chunk_children (
            parent_id TEXT NOT NULL,
            chunk_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            PRIMARY KEY(parent_id, chunk_id)
        );
        CREATE TABLE IF NOT EXISTS reply_edges (
            child_message_id TEXT NOT NULL,
            parent_message_id TEXT NOT NULL,
            child_chunk_id TEXT,
            parent_chunk_id TEXT,
            unresolved_reason TEXT,
            PRIMARY KEY(child_message_id, parent_message_id)
        );
        -- Invariant: `chunks_fts.rowid` is the `chunks.rowid` of the same chunk. `insert_chunk`
        -- writes it that way, `search.rs` joins the two tables on it, and the tail delete seeks
        -- fts rows by it. It is load-bearing rather than incidental because `chunk_id` is an
        -- UNINDEXED fts5 column: addressing a row by `chunk_id` makes SQLite walk every row of
        -- the table, so `rowid` is the only handle that keeps a scoped delete off archive-size
        -- cost. `tests/incremental_chunking.rs` pins the alignment after every rebuild path.
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts
            USING fts5(chunk_id UNINDEXED, text);

        -- A scoped rebuild addresses chunk rows as `(chat_id, started_at floor)`: the backward
        -- walk in `tail_rebuild_start` and every tail delete in `delete_chat_chunks`. Without
        -- these the archive has no index but the primary keys, so each of those statements scans
        -- the whole table and the cost of touching one room follows the size of the archive.
        -- `IF NOT EXISTS` lets an existing archive pick them up on the next open; no migration
        -- framework is involved because an index carries no data to convert.
        CREATE INDEX IF NOT EXISTS idx_chunks_chat_started
            ON chunks(chat_id, started_at);
        CREATE INDEX IF NOT EXISTS idx_parent_chunks_chat_started
            ON parent_chunks(chat_id, started_at);
        -- `chunk_parent_refs` has a primary key on `(child_chunk_id, parent_chunk_id)`, which
        -- serves the child half of the tail delete's OR but leaves the parent half scanning.
        CREATE INDEX IF NOT EXISTS idx_chunk_parent_refs_parent
            ON chunk_parent_refs(parent_chunk_id);

        -- Two touched-chat reads address `messages` by `chat_id`, which is not a primary-key
        -- prefix (`messages` is `(account_hash, chat_id, message_id)`), so without an index each
        -- scans the whole archive — the cost the scope exists to remove:
        --   * the scoped reply/parent-ref pass's membership subquery `WHERE chat_id = ?1`, and
        --   * `raw_messages_for_chat_since`, which also floors on `timestamp` and orders by
        --     `(timestamp, message_id)`.
        -- One index covers both: `(chat_id, timestamp, message_id)` serves the membership
        -- subquery through its `chat_id` prefix (covering, since `message_id` is the only column
        -- it selects) and gives the tail read a single `SEARCH (chat_id=? AND timestamp>?)` that
        -- also satisfies the `ORDER BY`, so neither read sorts or scans. A bare `(chat_id)` index
        -- would serve the membership half but leave the tail read sorting and re-filtering the
        -- floor, and keeping both would only add a redundant index for sync to maintain on every
        -- insert — so the wider index is the single source of truth for reaching a chat's rows.
        CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
            ON messages(chat_id, timestamp, message_id);
        CREATE INDEX IF NOT EXISTS idx_chunk_messages_message
            ON chunk_messages(message_id);",
    )
    .map_err(Error::Sql)?;

    // `CREATE TABLE IF NOT EXISTS` leaves an archive that predates a column alone, so added
    // columns need an explicit step. Defaulting to 0 makes every such archive read as drift,
    // which rebuilds its chunks once under the current chunker — the safe direction.
    add_column_if_missing(
        conn,
        "chunk_settings",
        "chunker_version",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(Error::Sql)?;
    let existing = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(Error::Sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::Sql)?;
    if existing.iter().any(|name| name == column) {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
    ))
    .map_err(Error::Sql)
}
