use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

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
        let conn = Connection::open(path).map_err(Error::Sql)?;
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
