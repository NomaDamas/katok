use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct RawMessage {
    pub account_hash: String,
    pub chat_id: String,
    pub chat_name: String,
    pub chat_type: String,
    pub message_id: String,
    pub sender_id: String,
    pub sender_nickname: String,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub message_type: String,
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub inserted_messages: usize,
    pub updated_messages: usize,
    pub total_messages: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkListItem {
    pub chunk_id: String,
    pub chat_id: String,
    pub chat_name: String,
    pub sender_nickname: String,
    pub started_at: String,
    pub ended_at: String,
    pub message_count: usize,
    pub parent_chunk_ids: Vec<String>,
    pub window_parent_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Chunk {
    pub chunk_id: String,
    pub chat_id: String,
    pub chat_name: String,
    pub sender_nickname: String,
    pub started_at: String,
    pub ended_at: String,
    pub text: String,
    pub message_count: usize,
    pub message_ids: Vec<String>,
    pub parent_chunk_ids: Vec<String>,
    pub window_parent_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParentChunk {
    pub parent_id: String,
    pub chat_id: String,
    pub chat_name: String,
    pub started_at: String,
    pub ended_at: String,
    pub text: String,
    pub message_count: usize,
    pub child_chunk_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkContext {
    pub chunk: Chunk,
    pub previous: Option<ChunkSummary>,
    pub next: Option<ChunkSummary>,
    pub parent_windows: Vec<ParentChunk>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkSummary {
    pub chunk_id: String,
    pub chat_id: String,
    pub chat_name: String,
    pub sender_nickname: String,
    pub started_at: String,
    pub ended_at: String,
    pub message_count: usize,
    pub parent_chunk_ids: Vec<String>,
    pub window_parent_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub ranker: &'static str,
    pub unit: &'static str,
    pub rank: usize,
    pub chunk_id: String,
    pub chat_name: String,
    pub sender_nickname: String,
    pub started_at: String,
    pub ended_at: String,
    pub snippet: String,
    pub score: f64,
    pub parent_chunk_ids: Vec<String>,
    pub child_chunk_ids: Vec<String>,
}
