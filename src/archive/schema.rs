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
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts
            USING fts5(chunk_id UNINDEXED, text);",
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
