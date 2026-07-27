use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

mod model;
mod parent;
mod read;
mod schema;
mod write;

pub use model::{ChunkDraft, ParentChunkDraft, StoredMessage};

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
