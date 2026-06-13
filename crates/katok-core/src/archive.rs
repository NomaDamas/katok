use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

mod model;
mod read;
mod schema;
mod write;

pub use model::{ChunkDraft, StoredMessage};

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
}
