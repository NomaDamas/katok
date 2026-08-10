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
    /// True when this message was authored by the account whose archive is read.
    /// Older fixture/kakaocli payloads omit it and safely default to false.
    #[serde(default)]
    pub is_self: bool,
    /// True only when source metadata explicitly mentions the account owner.
    /// Text that merely contains a matching display name never sets this flag.
    #[serde(default)]
    pub mentions_self: bool,
}

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub inserted_messages: usize,
    pub updated_messages: usize,
    pub total_messages: usize,
    pub chunks: usize,
    /// How many chats had their chunks recomputed. A quiet sync touches very few.
    pub rebuilt_chats: usize,
    /// Wall-clock milliseconds per stage, so "sync is slow" can be answered with a number
    /// instead of a guess about which stage is at fault.
    pub timings_ms: SyncTimings,
    /// Which chats changed, and where the earliest change landed in each.
    ///
    /// Always computed so the caller can scope the chunk rebuild to the tail after the last
    /// stable window boundary. Serialized into JSON only when `include_touched` is true
    /// (`katok sync --json --touched`); otherwise omitted so default output stays byte-identical
    /// for existing consumers.
    pub touched_chats: Vec<TouchedChat>,
    /// Gate for JSON emission of `touched_chats`. Default false.
    pub include_touched: bool,
}

impl Serialize for SyncReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Field order matches the historical derive order so flag-off output stays byte-identical.
        let field_count = 6 + usize::from(self.include_touched);
        let mut state = serializer.serialize_struct("SyncReport", field_count)?;
        state.serialize_field("inserted_messages", &self.inserted_messages)?;
        state.serialize_field("updated_messages", &self.updated_messages)?;
        state.serialize_field("total_messages", &self.total_messages)?;
        state.serialize_field("chunks", &self.chunks)?;
        state.serialize_field("rebuilt_chats", &self.rebuilt_chats)?;
        state.serialize_field("timings_ms", &self.timings_ms)?;
        if self.include_touched {
            state.serialize_field("touched_chats", &self.touched_chats)?;
        }
        state.end()
    }
}

/// A chat that changed this sync, with the earliest changed message's send-order key.
///
/// The key `(timestamp, message_id)` is what scopes the rebuild: only chunks and parent windows
/// from the last window boundary before this key onward can have moved. See
/// `docs/incremental-chunking-tail-scope.md`.
#[derive(Debug, Clone, Serialize)]
pub struct TouchedChat {
    pub chat_id: String,
    /// RFC3339 timestamp of the earliest inserted-or-updated message in this chat.
    pub earliest_changed_timestamp: String,
    /// Message id of that same earliest-changed message. Currently unread: the
    /// gap-derived cut guarantees a >=300s separation at `P`, so `started_at >= P`
    /// needs no tiebreak. Kept so the report states the full change key.
    pub earliest_changed_message_id: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncTimings {
    /// Decrypting and scanning the KakaoTalk database.
    pub read_source: u128,
    /// Upserting chats and messages into the archive.
    pub upsert_messages: u128,
    /// Rebuilding chunks and parent windows.
    pub rebuild_chunks: u128,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(include_touched: bool) -> SyncReport {
        SyncReport {
            inserted_messages: 3,
            updated_messages: 0,
            total_messages: 3,
            chunks: 2,
            rebuilt_chats: 1,
            timings_ms: SyncTimings {
                read_source: 1,
                upsert_messages: 1,
                rebuild_chunks: 1,
            },
            touched_chats: vec![TouchedChat {
                chat_id: "chat-group-1".into(),
                earliest_changed_timestamp: "2026-01-01T09:00:00+00:00".into(),
                earliest_changed_message_id: "m1".into(),
            }],
            include_touched,
        }
    }

    #[test]
    fn sync_report_json_omits_touched_chats_unless_flagged() {
        let off = serde_json::to_string_pretty(&sample_report(false)).expect("serialize off");
        let on = serde_json::to_string_pretty(&sample_report(true)).expect("serialize on");

        assert!(
            !off.contains("touched_chats"),
            "default serialization must omit touched_chats:\n{off}"
        );
        assert!(
            on.contains("\"touched_chats\""),
            "flagged serialization must include touched_chats:\n{on}"
        );

        // Flag-on without the opt-in field equals flag-off exactly (same report, same timings).
        let mut on_value: serde_json::Value = serde_json::from_str(&on).expect("parse on");
        on_value
            .as_object_mut()
            .expect("object")
            .remove("touched_chats");
        let off_value: serde_json::Value = serde_json::from_str(&off).expect("parse off");
        assert_eq!(
            off_value, on_value,
            "flag-off output must be the flag-on payload minus touched_chats"
        );

        // Historical field set (order is fixed by Serialize; Value map iteration is sorted).
        let keys: std::collections::BTreeSet<&str> = off_value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "chunks",
                "inserted_messages",
                "rebuilt_chats",
                "timings_ms",
                "total_messages",
                "updated_messages",
            ]
            .into_iter()
            .collect()
        );
        // Pretty-print field order must stay historical so flag-off is byte-stable for consumers.
        assert!(
            off.starts_with("{\n  \"inserted_messages\":"),
            "flag-off pretty JSON must open with inserted_messages:\n{off}"
        );
    }

    #[test]
    fn sync_report_json_emits_empty_touched_chats_when_flagged_and_quiet() {
        let mut quiet = sample_report(true);
        quiet.touched_chats.clear();
        quiet.rebuilt_chats = 0;
        quiet.inserted_messages = 0;
        let json = serde_json::to_value(&quiet).expect("serialize quiet");
        assert_eq!(json["touched_chats"], serde_json::json!([]));
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionStatus {
    /// No later message authored by the account owner exists in this room.
    Pending,
    /// A later self-authored message exists, but it is not a direct reply.
    Review,
    /// A self-authored message directly replies to the mention.
    Answered,
}

#[derive(Debug, Clone, Serialize)]
pub struct MentionInboxItem {
    pub status: MentionStatus,
    pub chat_id: String,
    pub chat_name: String,
    pub message_id: String,
    pub sender_nickname: String,
    pub timestamp: String,
    pub snippet: String,
    /// Chunk containing the mention, for explicit context retrieval.
    pub chunk_id: Option<String>,
    pub response_message_id: Option<String>,
    pub response_timestamp: Option<String>,
    pub response_snippet: Option<String>,
    pub response_chunk_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MentionInboxCounts {
    pub pending: usize,
    pub review: usize,
    pub answered: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MentionInboxReport {
    pub since: String,
    pub chat_id: Option<String>,
    pub counts: MentionInboxCounts,
    pub returned: usize,
    pub items: Vec<MentionInboxItem>,
}
