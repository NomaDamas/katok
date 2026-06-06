use crate::{
    archive::{Archive, ChunkDraft, StoredMessage},
    Result,
};
use sha2::{Digest, Sha256};

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

pub fn rebuild_chunks(archive: &Archive) -> Result<usize> {
    rebuild_chunks_with_settings(archive, ChunkSettings::default())
}

pub fn rebuild_chunks_with_settings(archive: &Archive, settings: ChunkSettings) -> Result<usize> {
    let messages = archive.raw_messages()?;
    let drafts = build_chunks(&messages, settings)?;
    let count = drafts.len();
    archive.replace_chunks(&drafts)?;
    Ok(count)
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
