//! Read KakaoTalk image message rows and normalize them into media resolver
//! frame inputs.
//!
//! This reader is intentionally separate from the archive/search reader: image
//! rows can have empty message text, and the extraction path needs attachment
//! metadata rather than conversation bodies.

use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::params;

use super::media_paths::{album_full_stem, album_thumb_stem, photo_full_stem, photo_thumb_stem};
use super::media_resolver::MediaFrameInput;
use super::{auth, derive, reader, AuthOptions};
use crate::Result;

const ALBUM_MESSAGE_TYPE: i64 = 27;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaQuery {
    pub chat_id: i64,
    pub log_id: Option<i64>,
    pub limit: usize,
}

pub fn read_media_frames_with_options(
    options: &AuthOptions,
    query: &MediaQuery,
) -> Result<Vec<MediaFrameInput>> {
    let resolved = auth::resolve_auth(options)?;
    read_media_frames_from_databases(
        &resolved.database_files,
        resolved.user_id,
        &resolved.uuid,
        query,
    )
}

pub fn read_media_frames_from_databases(
    database_files: &[PathBuf],
    user_id: i64,
    uuid: &str,
    query: &MediaQuery,
) -> Result<Vec<MediaFrameInput>> {
    let key = derive::secure_key(user_id, uuid);
    let mut frames = Vec::new();
    let mut seen = HashSet::new();

    for path in database_files {
        let Ok(conn) = reader::open_database(path, &key) else {
            eprintln!("katok: skipping unreadable KakaoTalk db");
            continue;
        };
        let rows = read_media_rows(&conn, query)?;
        for row in rows {
            for frame in frame_inputs(row) {
                if seen.insert((frame.log_id, frame.idx)) {
                    frames.push(frame);
                }
            }
        }
    }
    Ok(frames)
}

#[derive(Debug, Clone)]
struct MediaRow {
    log_id: i64,
    author_id: i64,
    msg_type: i64,
    sent_at: i64,
    attachment: Option<String>,
}

fn read_media_rows(conn: &rusqlite::Connection, query: &MediaQuery) -> Result<Vec<MediaRow>> {
    if let Some(log_id) = query.log_id {
        let mut stmt = conn
            .prepare(
                "SELECT logId, authorId, type, sentAt, attachment
                 FROM NTChatMessage
                 WHERE chatId = ?1 AND logId = ?2 AND type IN (2, 27)
                 ORDER BY sentAt ASC, logId ASC
                 LIMIT ?3",
            )
            .map_err(crate::Error::Sql)?;
        let rows = stmt
            .query_map(
                params![query.chat_id, log_id, query.limit as i64],
                map_media_row,
            )
            .map_err(crate::Error::Sql)?;
        collect_rows(rows)
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT logId, authorId, type, sentAt, attachment
                 FROM NTChatMessage
                 WHERE chatId = ?1 AND type IN (2, 27)
                 ORDER BY sentAt ASC, logId ASC
                 LIMIT ?2",
            )
            .map_err(crate::Error::Sql)?;
        let rows = stmt
            .query_map(params![query.chat_id, query.limit as i64], map_media_row)
            .map_err(crate::Error::Sql)?;
        collect_rows(rows)
    }
}

fn collect_rows<I>(rows: I) -> Result<Vec<MediaRow>>
where
    I: IntoIterator<Item = rusqlite::Result<MediaRow>>,
{
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        match row {
            Ok(row) => out.push(row),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("katok: skipped {skipped} unreadable KakaoTalk media row(s)");
    }
    Ok(out)
}

fn map_media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaRow> {
    let sent_at = row.get::<_, f64>(3).unwrap_or(0.0) as i64;
    Ok(MediaRow {
        log_id: row.get(0)?,
        author_id: row.get(1)?,
        msg_type: row.get(2)?,
        sent_at,
        attachment: row.get(4)?,
    })
}

fn frame_inputs(row: MediaRow) -> Vec<MediaFrameInput> {
    let attachment = row
        .attachment
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if row.msg_type == ALBUM_MESSAGE_TYPE {
        if let Some(csl) = attachment.get("csl").and_then(|value| value.as_array()) {
            return (0..csl.len())
                .map(|idx| MediaFrameInput {
                    log_id: row.log_id,
                    idx,
                    width: array_i64(&attachment, "wl", idx),
                    height: array_i64(&attachment, "hl", idx),
                    checksum_sha1: array_string(&attachment, "csl", idx),
                    full_stem: album_full_stem(row.log_id, idx),
                    thumb_stem: album_thumb_stem(row.log_id, idx),
                    output_stem: format!("{}_{}", row.log_id, idx),
                    sender: Some(row.author_id.to_string()),
                    sent_at: Some(row.sent_at),
                    cdn_url: array_string(&attachment, "imageUrls", idx),
                })
                .collect();
        }
    }

    vec![MediaFrameInput {
        log_id: row.log_id,
        idx: 0,
        width: object_i64(&attachment, "w"),
        height: object_i64(&attachment, "h"),
        checksum_sha1: object_string(&attachment, "cs"),
        full_stem: photo_full_stem(row.log_id),
        thumb_stem: photo_thumb_stem(row.log_id),
        output_stem: row.log_id.to_string(),
        sender: Some(row.author_id.to_string()),
        sent_at: Some(row.sent_at),
        cdn_url: object_string(&attachment, "url"),
    }]
}

fn object_i64(root: &serde_json::Value, key: &str) -> Option<i64> {
    root.get(key).and_then(value_i64)
}

fn object_string(root: &serde_json::Value, key: &str) -> Option<String> {
    root.get(key).and_then(value_string)
}

fn array_i64(root: &serde_json::Value, key: &str, idx: usize) -> Option<i64> {
    root.get(key)
        .and_then(|value| value.as_array())
        .and_then(|array| array.get(idx))
        .and_then(value_i64)
}

fn array_string(root: &serde_json::Value, key: &str, idx: usize) -> Option<String> {
    root.get(key)
        .and_then(|value| value.as_array())
        .and_then(|array| array.get(idx))
        .and_then(value_string)
}

fn value_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|v| v as i64)),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kakao::media_paths::{album_full_stem, photo_full_stem};

    const PHOTO_MESSAGE_TYPE: i64 = 2;

    #[test]
    fn normalizes_single_photo_attachment() {
        let row = MediaRow {
            log_id: 123,
            author_id: 456,
            msg_type: PHOTO_MESSAGE_TYPE,
            sent_at: 1_700_000_000,
            attachment: Some(
                r#"{"w":640,"h":"480","cs":"abc","url":"https://cdn.example/p"}"#.to_string(),
            ),
        };

        let frames = frame_inputs(row);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].idx, 0);
        assert_eq!(frames[0].width, Some(640));
        assert_eq!(frames[0].height, Some(480));
        assert_eq!(frames[0].checksum_sha1.as_deref(), Some("abc"));
        assert_eq!(frames[0].cdn_url.as_deref(), Some("https://cdn.example/p"));
        assert_eq!(frames[0].full_stem, photo_full_stem(123));
        assert_eq!(frames[0].sender.as_deref(), Some("456"));
    }

    #[test]
    fn normalizes_album_frames_from_parallel_arrays() {
        let row = MediaRow {
            log_id: 900,
            author_id: 111,
            msg_type: ALBUM_MESSAGE_TYPE,
            sent_at: 1_700_000_010,
            attachment: Some(
                r#"{"wl":[100,200],"hl":[300],"csl":["a","b"],"imageUrls":["u0","u1"]}"#
                    .to_string(),
            ),
        };

        let frames = frame_inputs(row);

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].full_stem, album_full_stem(900, 0));
        assert_eq!(frames[1].full_stem, album_full_stem(900, 1));
        assert_eq!(frames[0].height, Some(300));
        assert_eq!(frames[1].height, None);
        assert_eq!(frames[1].checksum_sha1.as_deref(), Some("b"));
        assert_eq!(frames[1].cdn_url.as_deref(), Some("u1"));
        assert_eq!(frames[1].output_stem, "900_1");
    }
}
