use crate::{Error, Result};
use rusqlite::{params, Connection};
use std::path::Path;

pub(crate) struct LocalVectorStore {
    conn: Connection,
    dimension: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredVector {
    pub chunk_id: String,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub(crate) struct VectorUpsert {
    pub chunk_id: String,
    pub content_hash: String,
    pub seen_token: String,
    pub heading_path: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub(crate) struct VectorHit {
    pub chunk_id: String,
    pub score: f32,
}

impl LocalVectorStore {
    pub(crate) fn open(dir: &Path, dimension: usize) -> Result<Self> {
        crate::paths::ensure_private_dir(dir)?;
        let path = dir.join("vectors.sqlite3");
        let conn = Connection::open(&path).map_err(Error::Sql)?;
        ensure_private_file(&path)?;
        migrate(&conn)?;
        Ok(Self { conn, dimension })
    }

    pub(crate) fn fetch(&self, chunk_id: &str) -> Result<Option<StoredVector>> {
        let mut statement = self
            .conn
            .prepare("SELECT chunk_id, content_hash FROM vectors WHERE chunk_id = ?1")
            .map_err(Error::Sql)?;
        let mut rows = statement.query(params![chunk_id]).map_err(Error::Sql)?;
        let Some(row) = rows.next().map_err(Error::Sql)? else {
            return Ok(None);
        };
        Ok(Some(StoredVector {
            chunk_id: row.get(0).map_err(Error::Sql)?,
            content_hash: row.get(1).map_err(Error::Sql)?,
        }))
    }

    pub(crate) fn upsert(&self, item: &VectorUpsert) -> Result<()> {
        if item.vector.len() != self.dimension {
            return Err(Error::Embedding(format!(
                "expected {} dimensions, got {}",
                self.dimension,
                item.vector.len()
            )));
        }
        let vector = encode_vector(&item.vector);
        self.conn
            .execute(
                "INSERT INTO vectors (
                    chunk_id, content_hash, seen_token, heading_path, vector
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(chunk_id) DO UPDATE SET
                    content_hash = excluded.content_hash,
                    seen_token = excluded.seen_token,
                    heading_path = excluded.heading_path,
                    vector = excluded.vector",
                params![
                    item.chunk_id,
                    item.content_hash,
                    item.seen_token,
                    item.heading_path,
                    vector
                ],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }

    pub(crate) fn mark_seen(
        &self,
        chunk_id: &str,
        seen_token: &str,
        heading_path: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE vectors SET seen_token = ?2, heading_path = ?3 WHERE chunk_id = ?1",
                params![chunk_id, seen_token, heading_path],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }

    pub(crate) fn delete_stale(&self, seen_token: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM vectors WHERE seen_token != ?1",
                params![seen_token],
            )
            .map_err(Error::Sql)?;
        Ok(())
    }

    pub(crate) fn search(&self, query: &[f32], limit: usize) -> Result<Vec<VectorHit>> {
        if query.len() != self.dimension {
            return Err(Error::Embedding(format!(
                "expected {} query dimensions, got {}",
                self.dimension,
                query.len()
            )));
        }
        let mut statement = self
            .conn
            .prepare("SELECT chunk_id, vector FROM vectors")
            .map_err(Error::Sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(Error::Sql)?;
        let mut hits = Vec::new();
        for row in rows {
            let (chunk_id, vector) = row.map_err(Error::Sql)?;
            let vector = decode_vector(&vector, self.dimension)?;
            let score = dot(query, &vector);
            if score > 0.0 {
                hits.push(VectorHit { chunk_id, score });
            }
        }
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}

fn ensure_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).map_err(Error::Io)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(Error::Io)?;
    }
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS vectors (
            chunk_id TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            seen_token TEXT NOT NULL,
            heading_path TEXT NOT NULL,
            vector BLOB NOT NULL
        );",
    )
    .map_err(Error::Sql)
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vector(bytes: &[u8], dimension: usize) -> Result<Vec<f32>> {
    if bytes.len() != dimension * std::mem::size_of::<f32>() {
        return Err(Error::Embedding(
            "stored vector has wrong byte length".to_string(),
        ));
    }
    bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| {
            let array =
                <[u8; 4]>::try_from(chunk).map_err(|error| Error::Embedding(error.to_string()))?;
            Ok(f32::from_le_bytes(array))
        })
        .collect()
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}
