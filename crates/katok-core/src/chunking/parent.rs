use crate::{
    archive::{ChunkDraft, ParentChunkDraft},
    Result,
};
use sha2::{Digest, Sha256};

pub const DEFAULT_PARENT_WINDOW_SECONDS: i64 = 300;
pub const DEFAULT_PARENT_WINDOW_MAX_CHARS: usize = 3_000;

pub(super) fn build_parent_windows(chunks: &[ChunkDraft]) -> Result<Vec<ParentChunkDraft>> {
    let mut parents = Vec::new();
    let mut current: Vec<ChunkDraft> = Vec::new();

    for chunk in chunks {
        if should_start_parent_window(&current, chunk)? && !current.is_empty() {
            parents.extend(parent_segments_from_chunks(&current)?);
            current.clear();
        }
        current.push(chunk.clone());
    }

    if !current.is_empty() {
        parents.extend(parent_segments_from_chunks(&current)?);
    }

    Ok(parents)
}

fn should_start_parent_window(current: &[ChunkDraft], next: &ChunkDraft) -> Result<bool> {
    let Some(previous) = current.last() else {
        return Ok(false);
    };
    if previous.chat_id != next.chat_id {
        return Ok(true);
    }
    let previous_time =
        chrono::DateTime::parse_from_rfc3339(&previous.ended_at).map_err(crate::Error::Time)?;
    let next_time =
        chrono::DateTime::parse_from_rfc3339(&next.started_at).map_err(crate::Error::Time)?;
    let gap = next_time.signed_duration_since(previous_time).num_seconds();
    if gap > DEFAULT_PARENT_WINDOW_SECONDS {
        return Ok(true);
    }
    let projected = current_parent_len(current) + 1 + parent_line(next).chars().count();
    Ok(projected > DEFAULT_PARENT_WINDOW_MAX_CHARS)
}

fn current_parent_len(chunks: &[ChunkDraft]) -> usize {
    chunks
        .iter()
        .map(|chunk| parent_line(chunk).chars().count())
        .sum::<usize>()
        + chunks.len().saturating_sub(1)
}

fn parent_segments_from_chunks(chunks: &[ChunkDraft]) -> Result<Vec<ParentChunkDraft>> {
    let first = chunks.first().ok_or(crate::Error::EmptyChunk)?;
    let mut segments = Vec::new();
    let mut segment = ParentSegment::new(first);

    for chunk in chunks {
        let line = parent_line(chunk);
        let chars = line.chars().collect::<Vec<_>>();
        let mut offset = 0;
        while offset < chars.len() {
            if segment.remaining() == 0 {
                segments.push(segment.into_draft()?);
                segment = ParentSegment::new(chunk);
            }
            if !segment.text.is_empty() {
                if segment.remaining() == 1 {
                    segments.push(segment.into_draft()?);
                    segment = ParentSegment::new(chunk);
                } else {
                    segment.text.push('\n');
                }
            }
            let take = segment.remaining().min(chars.len() - offset);
            for ch in &chars[offset..offset + take] {
                segment.text.push(*ch);
            }
            segment.add_child(chunk);
            offset += take;
        }
    }

    if !segment.text.is_empty() {
        segments.push(segment.into_draft()?);
    }
    Ok(segments)
}

fn parent_line(chunk: &ChunkDraft) -> String {
    format!("[{}] {}", chunk.sender_nickname, chunk.text)
}

struct ParentSegment {
    account_hash: String,
    chat_id: String,
    chat_name: String,
    started_at: String,
    ended_at: String,
    text: String,
    message_count: usize,
    child_chunk_ids: Vec<String>,
}

impl ParentSegment {
    fn new(chunk: &ChunkDraft) -> Self {
        Self {
            account_hash: chunk.account_hash.clone(),
            chat_id: chunk.chat_id.clone(),
            chat_name: chunk.chat_name.clone(),
            started_at: chunk.started_at.clone(),
            ended_at: chunk.ended_at.clone(),
            text: String::new(),
            message_count: 0,
            child_chunk_ids: Vec::new(),
        }
    }

    fn remaining(&self) -> usize {
        DEFAULT_PARENT_WINDOW_MAX_CHARS.saturating_sub(self.text.chars().count())
    }

    fn add_child(&mut self, chunk: &ChunkDraft) {
        self.ended_at = chunk.ended_at.clone();
        if self.child_chunk_ids.iter().any(|id| id == &chunk.chunk_id) {
            return;
        }
        self.message_count += chunk.message_ids.len();
        self.child_chunk_ids.push(chunk.chunk_id.clone());
    }

    fn into_draft(self) -> Result<ParentChunkDraft> {
        let first_child = self
            .child_chunk_ids
            .first()
            .ok_or(crate::Error::EmptyChunk)?
            .clone();
        let last_child = self
            .child_chunk_ids
            .last()
            .ok_or(crate::Error::EmptyChunk)?
            .clone();
        Ok(ParentChunkDraft {
            parent_id: stable_parent_id(
                &self.account_hash,
                &self.chat_id,
                &first_child,
                &last_child,
                &self.text,
            ),
            account_hash: self.account_hash,
            chat_id: self.chat_id,
            chat_name: self.chat_name,
            started_at: self.started_at,
            ended_at: self.ended_at,
            text: self.text,
            message_count: self.message_count,
            child_chunk_ids: self.child_chunk_ids,
        })
    }
}

fn stable_parent_id(
    account_hash: &str,
    chat_id: &str,
    first_id: &str,
    last_id: &str,
    text: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account_hash.as_bytes());
    hasher.update(b"|");
    hasher.update(chat_id.as_bytes());
    hasher.update(b"|");
    hasher.update(first_id.as_bytes());
    hasher.update(b"|");
    hasher.update(last_id.as_bytes());
    hasher.update(b"|parent-window-v1");
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    format!("window_{}", &hex_lower(&digest)[..16])
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
