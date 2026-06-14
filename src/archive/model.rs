#[derive(Debug, Clone)]
pub struct ChunkDraft {
    pub chunk_id: String,
    pub account_hash: String,
    pub chat_id: String,
    pub chat_name: String,
    pub sender_nickname: String,
    pub started_at: String,
    pub ended_at: String,
    pub text: String,
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ParentChunkDraft {
    pub parent_id: String,
    pub account_hash: String,
    pub chat_id: String,
    pub chat_name: String,
    pub started_at: String,
    pub ended_at: String,
    pub text: String,
    pub message_count: usize,
    pub child_chunk_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub account_hash: String,
    pub chat_id: String,
    pub chat_name: String,
    pub chat_type: String,
    pub message_id: String,
    pub sender_nickname: String,
    pub timestamp: String,
    pub text: String,
    pub message_type: String,
}

impl StoredMessage {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            account_hash: row.get(0)?,
            chat_id: row.get(1)?,
            chat_name: row.get(2)?,
            chat_type: row.get(3)?,
            message_id: row.get(4)?,
            sender_nickname: row.get(5)?,
            timestamp: row.get(6)?,
            text: row.get(7)?,
            message_type: row.get(8)?,
        })
    }
}
