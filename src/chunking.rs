use crate::{
    archive::{Archive, ChunkDraft, StoredMessage},
    types::TouchedChat,
    Result,
};
use sha2::{Digest, Sha256};

mod parent;

use parent::build_parent_windows;
pub use parent::{DEFAULT_PARENT_WINDOW_MAX_CHARS, DEFAULT_PARENT_WINDOW_SECONDS};

#[derive(Debug, Clone, Copy)]
pub struct ChunkSettings {
    pub group_gap_seconds: i64,
    pub direct_gap_seconds: i64,
}

impl Default for ChunkSettings {
    fn default() -> Self {
        Self {
            group_gap_seconds: 600,
            direct_gap_seconds: 1_800,
        }
    }
}

/// Bump when the chunk or parent-window boundary rules change.
///
/// Chunking is scoped to the chats that changed, so a logic change would otherwise only reach
/// rooms that happen to receive a message and quiet rooms would keep artifacts from the old
/// algorithm forever — the same failure the gap settings had. Recording this alongside the gap
/// values makes an upgraded binary rebuild everything once.
///
/// Covers `should_start_new_chunk`, `should_start_parent_window`, the parent-window size limits,
/// and the `stable_chunk_id` salt.
pub const CHUNKER_VERSION: i64 = 1;

pub fn rebuild_chunks(archive: &Archive) -> Result<usize> {
    rebuild_chunks_with_settings(archive, ChunkSettings::default())
}

pub fn rebuild_chunks_with_settings(archive: &Archive, settings: ChunkSettings) -> Result<usize> {
    let messages = archive.raw_messages()?;
    let drafts = build_chunks(&messages, settings)?;
    let parents = build_parent_windows(&drafts)?;
    let count = drafts.len();
    archive.replace_chunks(&drafts, &parents)?;
    Ok(count)
}

/// Recompute chunks for the touched chats only, scoped to the tail past the last stable
/// parent-window boundary, leaving other chats and each touched chat's earlier chunks untouched.
///
/// Produces the same rows a full rebuild would: boundaries depend only on the previous message and
/// never cross a chat, `chunk_id` is derived from content rather than position, and the cut is a
/// time-gap window boundary that no change past it can move. The rule and its correctness proof
/// are documented in `docs/incremental-chunking-tail-scope.md` and pinned by
/// `cutting_at_the_last_parent_window_reproduces_the_full_rebuild_tail`.
///
/// A chat whose earliest change lands before any interior boundary (empty timestamp forces this)
/// is rebuilt whole. Reply and cross-chunk parent references are chat-local (see
/// `docs/incremental-chunking-tail-scope.md`), so they are rebuilt only for the touched chats
/// after every touched chat's tail is replaced — not for the whole archive.
pub fn rebuild_chunks_for_chats(
    archive: &Archive,
    settings: ChunkSettings,
    touched: &[TouchedChat],
) -> Result<usize> {
    if touched.is_empty() {
        return archive.chunk_count();
    }
    for chat in touched {
        let from = archive.tail_rebuild_start(&chat.chat_id, &chat.earliest_changed_timestamp)?;
        let messages = archive.raw_messages_for_chat_since(&chat.chat_id, from.as_deref())?;
        let drafts = build_chunks(&messages, settings)?;
        let parents = build_parent_windows(&drafts)?;
        archive.replace_chunk_tail(&chat.chat_id, from.as_deref(), &drafts, &parents)?;
    }
    let chat_ids: Vec<&str> = touched.iter().map(|c| c.chat_id.as_str()).collect();
    archive.rebuild_reply_and_parent_refs_for_chats(&chat_ids)?;
    archive.chunk_count()
}

fn build_chunks(messages: &[StoredMessage], settings: ChunkSettings) -> Result<Vec<ChunkDraft>> {
    let mut chunks = Vec::new();
    let mut current: Vec<StoredMessage> = Vec::new();

    for message in messages {
        if should_start_new_chunk(current.last(), message, settings)? && !current.is_empty() {
            chunks.push(chunk_from_messages(&current)?);
            current.clear();
        }
        current.push(message.clone());
    }

    if !current.is_empty() {
        chunks.push(chunk_from_messages(&current)?);
    }

    Ok(chunks)
}

fn should_start_new_chunk(
    previous: Option<&StoredMessage>,
    next: &StoredMessage,
    settings: ChunkSettings,
) -> Result<bool> {
    let Some(previous) = previous else {
        return Ok(false);
    };
    if previous.chat_id != next.chat_id || previous.sender_nickname != next.sender_nickname {
        return Ok(true);
    }
    if previous.message_type != "text" || next.message_type != "text" {
        return Ok(true);
    }
    let previous_time =
        chrono::DateTime::parse_from_rfc3339(&previous.timestamp).map_err(crate::Error::Time)?;
    let next_time =
        chrono::DateTime::parse_from_rfc3339(&next.timestamp).map_err(crate::Error::Time)?;
    let gap = next_time.signed_duration_since(previous_time).num_seconds();
    let threshold = if next.chat_type == "group" {
        settings.group_gap_seconds
    } else {
        settings.direct_gap_seconds
    };
    Ok(gap > threshold)
}

fn chunk_from_messages(messages: &[StoredMessage]) -> Result<ChunkDraft> {
    let first = messages.first().ok_or(crate::Error::EmptyChunk)?;
    let last = messages.last().ok_or(crate::Error::EmptyChunk)?;
    let text = messages
        .iter()
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let message_ids = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();
    Ok(ChunkDraft {
        chunk_id: stable_chunk_id(
            &first.account_hash,
            &first.chat_id,
            &first.message_id,
            &last.message_id,
        ),
        account_hash: first.account_hash.clone(),
        chat_id: first.chat_id.clone(),
        chat_name: first.chat_name.clone(),
        sender_nickname: first.sender_nickname.clone(),
        started_at: first.timestamp.clone(),
        ended_at: last.timestamp.clone(),
        text,
        message_ids,
    })
}

fn stable_chunk_id(account_hash: &str, chat_id: &str, first_id: &str, last_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account_hash.as_bytes());
    hasher.update(b"|");
    hasher.update(chat_id.as_bytes());
    hasher.update(b"|");
    hasher.update(first_id.as_bytes());
    hasher.update(b"|");
    hasher.update(last_id.as_bytes());
    hasher.update(b"|v1");
    let digest = hasher.finalize();
    format!("chunk_{}", &hex_lower(&digest)[..16])
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
