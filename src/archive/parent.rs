use super::Archive;
use crate::{
    types::{Chunk, ChunkContext, ChunkSummary, ParentChunk},
    Error, Result,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

impl Archive {
    pub fn get_parent_chunk(&self, parent_id: &str) -> Result<Option<ParentChunk>> {
        let row = self
            .conn
            .query_row(
                "SELECT parent_id, chat_id, chat_name, started_at, ended_at,
                    text, message_count
             FROM parent_chunks WHERE parent_id = ?1",
                params![parent_id],
                |row| {
                    Ok(ParentChunk {
                        parent_id: row.get(0)?,
                        chat_id: row.get(1)?,
                        chat_name: row.get(2)?,
                        started_at: row.get(3)?,
                        ended_at: row.get(4)?,
                        text: row.get(5)?,
                        message_count: row.get::<_, i64>(6)? as usize,
                        child_chunk_ids: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(Error::Sql)?;
        match row {
            Some(mut parent) => {
                parent.child_chunk_ids = self.parent_child_chunks(parent_id)?;
                Ok(Some(parent))
            }
            None => Ok(None),
        }
    }

    pub fn parent_windows_for_child(&self, chunk_id: &str) -> Result<Vec<ParentChunk>> {
        self.window_parent_ids(chunk_id)?
            .into_iter()
            .map(|id| {
                self.get_parent_chunk(&id)
                    .and_then(|parent| parent.ok_or(Error::MissingChunk(id)))
            })
            .collect()
    }

    pub fn chunk_context(&self, chunk_id: &str) -> Result<Option<ChunkContext>> {
        let Some(chunk) = self.get_chunk(chunk_id)? else {
            return Ok(None);
        };
        let previous = self.neighbor_chunk(&chunk, Neighbor::Previous)?;
        let next = self.neighbor_chunk(&chunk, Neighbor::Next)?;
        let parent_windows = self.parent_windows_for_child(chunk_id)?;
        Ok(Some(ChunkContext {
            chunk,
            previous,
            next,
            parent_windows,
        }))
    }

    pub fn all_parent_chunks(&self) -> Result<Vec<ParentChunk>> {
        if !self.conn.is_autocommit() {
            return load_all_parent_chunks(&self.conn);
        }
        let tx = self.conn.unchecked_transaction().map_err(Error::Sql)?;
        let parents = load_all_parent_chunks(&tx)?;
        tx.commit().map_err(Error::Sql)?;
        Ok(parents)
    }

    pub(super) fn window_parent_ids(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT parent_id FROM parent_chunk_children
             WHERE chunk_id = ?1 ORDER BY parent_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![chunk_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    fn parent_child_chunks(&self, parent_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT chunk_id FROM parent_chunk_children
             WHERE parent_id = ?1 ORDER BY ordinal",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map(params![parent_id], |row| row.get::<_, String>(0))
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;
        Ok(rows)
    }

    fn neighbor_chunk(&self, chunk: &Chunk, direction: Neighbor) -> Result<Option<ChunkSummary>> {
        let (cmp, order) = match direction {
            Neighbor::Previous => ("<", "DESC"),
            Neighbor::Next => (">", "ASC"),
        };
        let sql = format!(
            "SELECT chunk_id, chat_id, chat_name, sender_nickname, started_at,
                    ended_at, message_count
             FROM chunks
             WHERE chat_id = ?1 AND (started_at, chunk_id) {cmp} (?2, ?3)
             ORDER BY started_at {order}, chunk_id {order}
             LIMIT 1"
        );
        let row = self
            .conn
            .query_row(
                &sql,
                params![chunk.chat_id, chunk.started_at, chunk.chunk_id],
                |row| {
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
                },
            )
            .optional()
            .map_err(Error::Sql)?;
        match row {
            Some(mut summary) => {
                summary.parent_chunk_ids = self.parent_chunks(&summary.chunk_id)?;
                summary.window_parent_ids = self.window_parent_ids(&summary.chunk_id)?;
                Ok(Some(summary))
            }
            None => Ok(None),
        }
    }
}

fn load_all_parent_chunks(conn: &Connection) -> Result<Vec<ParentChunk>> {
    let mut parents = {
        let mut stmt = conn
            .prepare(
                "SELECT parent_id, chat_id, chat_name, started_at, ended_at,
                        text, message_count
                 FROM parent_chunks
                 ORDER BY started_at, parent_id",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ParentChunk {
                    parent_id: row.get(0)?,
                    chat_id: row.get(1)?,
                    chat_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    text: row.get(5)?,
                    message_count: row.get::<_, i64>(6)? as usize,
                    child_chunk_ids: Vec::new(),
                })
            })
            .map_err(Error::Sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?
    };
    let children = {
        let mut stmt = conn
            .prepare(
                "SELECT parent_id, chunk_id
                 FROM parent_chunk_children
                 ORDER BY parent_id, ordinal",
            )
            .map_err(Error::Sql)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(Error::Sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?
    };
    attach_parent_children(&mut parents, children)?;
    Ok(parents)
}

fn attach_parent_children(
    parents: &mut [ParentChunk],
    children: impl IntoIterator<Item = (String, String)>,
) -> Result<()> {
    let parent_indexes = parents
        .iter()
        .enumerate()
        .map(|(index, parent)| (parent.parent_id.clone(), index))
        .collect::<HashMap<_, _>>();
    for (parent_id, chunk_id) in children {
        let Some(index) = parent_indexes.get(&parent_id).copied() else {
            return Err(Error::MissingChunk(parent_id));
        };
        parents[index].child_chunk_ids.push(chunk_id);
    }
    Ok(())
}

enum Neighbor {
    Previous,
    Next,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(parent_id: &str) -> ParentChunk {
        ParentChunk {
            parent_id: parent_id.to_string(),
            chat_id: "chat-fixture".to_string(),
            chat_name: "Synthetic room".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: "2026-01-01T00:01:00Z".to_string(),
            text: "synthetic text".to_string(),
            message_count: 1,
            child_chunk_ids: Vec::new(),
        }
    }

    #[test]
    fn bulk_parent_assembly_preserves_parent_and_child_order() {
        let mut parents = vec![parent("parent-early"), parent("parent-late")];
        let children = vec![
            ("parent-early".to_string(), "child-a".to_string()),
            ("parent-early".to_string(), "child-b".to_string()),
            ("parent-late".to_string(), "child-c".to_string()),
        ];

        attach_parent_children(&mut parents, children).expect("attach children");

        assert_eq!(parents[0].child_chunk_ids, ["child-a", "child-b"]);
        assert_eq!(parents[1].child_chunk_ids, ["child-c"]);
    }

    #[test]
    fn bulk_parent_load_reuses_caller_transaction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");

        let parents: Result<Vec<ParentChunk>> =
            archive.in_transaction(|| archive.all_parent_chunks());

        assert!(parents.expect("load parents inside transaction").is_empty());
    }
}
