//! Export the raw messages of one chat over a time range as a Markdown transcript.
//!
//! Search answers "which chunk is relevant"; this answers "what was actually said, in order",
//! which is what a follow-up read of a room needs. It reads the archive rather than the live
//! KakaoTalk database, so the archive stays the single source the rest of the crate agrees on —
//! run `katok sync` first if the tail matters.

use crate::{archive::Archive, Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// A message whose body is a KakaoTalk system feed (joins, leaves, invites) rather than
/// something a person typed.
///
/// The body of such a message is a JSON object carrying one of the membership keys. Anything
/// that is not parseable JSON, or is JSON without those keys, is a real message.
pub fn is_feed(text: &str) -> bool {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    ["feedType", "inviter", "member", "leaver", "members"]
        .iter()
        .any(|key| object.contains_key(*key))
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptReport {
    pub chat_id: String,
    pub chat_name: String,
    pub messages: usize,
    pub feed_skipped: usize,
    /// `None` when the range held no messages: a run with nothing to say writes nothing.
    pub path: Option<PathBuf>,
    pub first_message_id: Option<String>,
    pub last_message_id: Option<String>,
}

/// Write the transcript for `chat_id` covering messages at or after `since` (RFC3339).
///
/// Writing is atomic and idempotent. The file name carries the message-id range, so a later
/// export of a different range cannot overwrite an earlier one, and re-exporting the same range
/// rewrites identical bytes.
pub fn export_transcript(
    archive: &Archive,
    chat_id: &str,
    since: Option<&str>,
    out_dir: &Path,
) -> Result<TranscriptReport> {
    let messages = archive.messages_for_transcript(chat_id, since)?;

    let mut chat_name = chat_id.to_string();
    let mut lines: Vec<String> = Vec::new();
    let mut feed_skipped = 0usize;
    let mut kept = 0usize;
    let mut first_message_id: Option<String> = None;
    let mut last_message_id: Option<String> = None;

    for message in &messages {
        if !message.chat_name.is_empty() {
            chat_name = message.chat_name.clone();
        }
        if is_feed(&message.text) {
            feed_skipped += 1;
            continue;
        }
        if first_message_id.is_none() {
            first_message_id = Some(message.message_id.clone());
        }
        last_message_id = Some(message.message_id.clone());
        kept += 1;
        lines.push(format!(
            "**[{}] {}**: {}",
            message.timestamp, message.sender_nickname, message.text
        ));
        lines.push(String::new());
    }

    let (Some(first), Some(last)) = (first_message_id.clone(), last_message_id.clone()) else {
        // Nothing in range. Touch no file so an empty run cannot clobber an earlier export.
        return Ok(TranscriptReport {
            chat_id: chat_id.to_string(),
            chat_name,
            messages: 0,
            feed_skipped,
            path: None,
            first_message_id: None,
            last_message_id: None,
        });
    };

    let mut body = vec![
        format!("# {chat_name} (chat {chat_id})"),
        String::new(),
        format!("- messages: {kept}"),
        format!("- range: {first} .. {last}"),
    ];
    if feed_skipped > 0 {
        body.push(format!("- system feed messages skipped: {feed_skipped}"));
    }
    body.push(String::new());
    body.extend(lines);

    crate::paths::ensure_private_dir(out_dir)?;
    let path = out_dir.join(format!(
        "{}-{}-{}.md",
        sanitize(chat_id),
        sanitize(strip_chat_prefix(&first, chat_id)),
        sanitize(strip_chat_prefix(&last, chat_id))
    ));
    write_atomically(&path, body.join("\n").as_bytes())?;

    Ok(TranscriptReport {
        chat_id: chat_id.to_string(),
        chat_name,
        messages: kept,
        feed_skipped,
        path: Some(path),
        first_message_id,
        last_message_id,
    })
}

/// A message id is `<chat_id>-<log_id>`, and the file name already leads with the chat, so the
/// prefix would otherwise appear three times in one name.
fn strip_chat_prefix<'a>(message_id: &'a str, chat_id: &str) -> &'a str {
    message_id
        .strip_prefix(chat_id)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(message_id)
}

/// Keep file names to characters that cannot surprise a shell or a filesystem.
fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// Write through a temporary file so a failed or interrupted write cannot leave a partial
/// transcript where a complete one used to be.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(err)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_json_is_a_feed_but_ordinary_text_is_not() {
        assert!(is_feed(r#"{"feedType":4,"member":{"nickName":"부리"}}"#));
        assert!(is_feed(r#"  {"leaver":{"userId":1}}"#));
        assert!(!is_feed("오늘 회의 몇 시야?"));
        assert!(!is_feed(""));
        // JSON the user actually typed, with none of the membership keys.
        assert!(!is_feed(r#"{"note":"이건 그냥 내가 보낸 JSON"}"#));
        // Looks like it starts a feed but does not parse.
        assert!(!is_feed("{feedType"));
    }
}
