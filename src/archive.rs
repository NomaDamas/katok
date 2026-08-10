use crate::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

mod inbox;
mod model;
mod parent;
mod read;
mod schema;
mod write;

pub use model::{ChunkDraft, DeletedMessage, ParentChunkDraft, StoredMessage};
pub use read::{RAW_MESSAGES_FOR_CHAT_SINCE_QUERY, TAIL_REBUILD_START_QUERY};
pub use write::{
    DELETE_CHAT_CHUNKS_STATEMENTS, DELETE_REPLY_EDGES_FOR_CHAT, INSERT_CHUNK_PARENT_REFS_FOR_CHAT,
    INSERT_REPLY_EDGES_FOR_CHAT, RESOLVE_REPLY_EDGES_FOR_CHAT, SCOPED_REF_REBUILD_STATEMENTS,
};

pub struct Archive {
    pub(super) conn: Connection,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::paths::ensure_private_dir(parent)?;
        }
        // macOS exposes `/var` as a symlink to `/private/var`, which appears in
        // normal temporary paths. Canonicalize only the parent so SQLite's
        // NOFOLLOW flag checks the archive entry itself without rejecting a
        // harmless symlink in an ancestor path.
        let open_path = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => parent.canonicalize().map_err(Error::Io)?.join(name),
            _ => path.to_path_buf(),
        };
        let conn = Connection::open_with_flags(
            &open_path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(Error::Sql)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&open_path)
                .map_err(Error::Io)?
                .permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&open_path, permissions).map_err(Error::Io)?;
        }
        let archive = Self { conn };
        schema::migrate(&archive.conn)?;
        Ok(archive)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Run `work` inside a single SQLite transaction, rolling back if it fails.
    ///
    /// Without this every statement commits on its own, so a sync paid one journal fsync per
    /// row — and a sync that died midway left the archive partially written with nothing to roll
    /// back to.
    pub fn in_transaction<T, E>(
        &self,
        work: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|err| E::from(Error::Sql(err)))?;
        let value = work()?;
        tx.commit().map_err(|err| E::from(Error::Sql(err)))?;
        Ok(value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn archive_file_is_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("archive.sqlite3");
        let archive = Archive::open(&path).expect("open archive");
        drop(archive);

        let mode = std::fs::metadata(path)
            .expect("archive metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn archive_refuses_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).expect("data dir");
        let external = dir.path().join("external.sqlite3");
        std::fs::write(&external, []).expect("external file");
        let archive_path = data_dir.join("archive.sqlite3");
        symlink(&external, &archive_path).expect("archive symlink");

        assert!(
            Archive::open(&archive_path).is_err(),
            "the archive must never follow a symbolic link"
        );
    }

    #[test]
    fn legacy_archive_adds_mention_columns_before_dependent_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("archive.sqlite3");
        let conn = rusqlite::Connection::open(&path).expect("legacy connection");
        conn.execute_batch(
            "CREATE TABLE messages (
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
            );",
        )
        .expect("legacy schema");
        drop(conn);

        let archive = Archive::open(&path).expect("migrate legacy archive");
        let columns = archive
            .connection()
            .prepare("PRAGMA table_info(messages)")
            .expect("prepare columns")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query columns")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect columns");
        assert!(columns.iter().any(|column| column == "is_self"));
        assert!(columns.iter().any(|column| column == "mentions_self"));

        let indexes = archive
            .connection()
            .prepare("PRAGMA index_list(messages)")
            .expect("prepare indexes")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query indexes")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect indexes");
        assert!(indexes
            .iter()
            .any(|index| index == "idx_messages_pending_mentions"));
    }
}
