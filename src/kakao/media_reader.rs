//! Read KakaoTalk media message rows and normalize them into media resolver
//! frame inputs.
//!
//! This reader is intentionally separate from the archive/search reader: media
//! rows can have empty message text, and the extraction path needs attachment
//! metadata rather than conversation bodies.

use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::params;

use super::media_paths::{
    album_full_stem, album_thumb_stem, photo_full_stem, photo_thumb_stem, video_full_stem,
};
use super::media_resolver::{MediaFrameInput, MediaKind};
use super::{auth, derive, reader, AuthOptions};
use crate::Result;

const PHOTO_MESSAGE_TYPE: i64 = 2;
const ALBUM_MESSAGE_TYPE: i64 = 27;
const VIDEO_MESSAGE_TYPE: i64 = 3;
/// A generic file attachment: zip, pdf, xlsx, hwp, and every other extension
/// KakaoTalk lets a user attach. One message type covers them all.
const FILE_MESSAGE_TYPE: i64 = 18;

const IMAGE_CACHE_EXT: &str = ".img";
const VIDEO_CACHE_EXT: &str = ".vid";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaQuery {
    pub chat_id: i64,
    pub log_id: Option<i64>,
    pub limit: usize,
    /// Message kinds to read. Empty means every kind.
    pub kinds: Vec<MediaKind>,
}

impl MediaQuery {
    pub fn new(chat_id: i64, log_id: Option<i64>, limit: usize) -> Self {
        Self {
            chat_id,
            log_id,
            limit,
            kinds: Vec::new(),
        }
    }

    /// KakaoTalk `type` values this query wants, ascending.
    fn message_types(&self) -> Vec<i64> {
        let mut types: Vec<i64> = if self.kinds.is_empty() {
            MediaKind::ALL.iter().flat_map(kind_message_types).collect()
        } else {
            self.kinds.iter().flat_map(kind_message_types).collect()
        };
        types.sort_unstable();
        types.dedup();
        types
    }
}

fn kind_message_types(kind: &MediaKind) -> Vec<i64> {
    match kind {
        MediaKind::Photo => vec![PHOTO_MESSAGE_TYPE, ALBUM_MESSAGE_TYPE],
        MediaKind::Video => vec![VIDEO_MESSAGE_TYPE],
        MediaKind::File => vec![FILE_MESSAGE_TYPE],
    }
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
    // The `IN` list is interpolated rather than bound. Its members come from
    // `MediaKind`, a closed enum of compile-time constants, so no caller input
    // reaches the SQL text; binding a variable-length list would need
    // `params_from_iter` and lose the named positional parameters below.
    let types = query
        .message_types()
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(log_id) = query.log_id {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT logId, authorId, type, sentAt, attachment
                 FROM NTChatMessage
                 WHERE chatId = ?1 AND logId = ?2 AND type IN ({types})
                 ORDER BY sentAt ASC, logId ASC
                 LIMIT ?3"
            ))
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
            .prepare(&format!(
                "SELECT logId, authorId, type, sentAt, attachment
                 FROM NTChatMessage
                 WHERE chatId = ?1 AND type IN ({types})
                 ORDER BY sentAt ASC, logId ASC
                 LIMIT ?2"
            ))
            .map_err(crate::Error::Sql)?;
        let rows = stmt
            .query_map(params![query.chat_id, query.limit as i64], map_media_row)
            .map_err(crate::Error::Sql)?;
        collect_rows(rows)
    }
}

/// Chat ids that hold at least one media row of the requested kinds.
///
/// `media backfill` needs the room list before it can resolve anything, and the
/// KakaoTalk database is the only place that knows which rooms carry media.
pub fn read_media_chat_ids_with_options(
    options: &AuthOptions,
    kinds: &[MediaKind],
) -> Result<Vec<i64>> {
    let resolved = auth::resolve_auth(options)?;
    let key = derive::secure_key(resolved.user_id, &resolved.uuid);
    let query = MediaQuery {
        chat_id: 0,
        log_id: None,
        limit: 0,
        kinds: kinds.to_vec(),
    };
    let types = query
        .message_types()
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in &resolved.database_files {
        let Ok(conn) = reader::open_database(path, &key) else {
            eprintln!("katok: skipping unreadable KakaoTalk db");
            continue;
        };
        let mut stmt = conn
            .prepare(&format!(
                "SELECT DISTINCT chatId FROM NTChatMessage WHERE type IN ({types})"
            ))
            .map_err(crate::Error::Sql)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(crate::Error::Sql)?;
        for row in rows.flatten() {
            if seen.insert(row) {
                out.push(row);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
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
                    kind: MediaKind::Photo,
                    log_id: row.log_id,
                    idx,
                    width: array_i64(&attachment, "wl", idx),
                    height: array_i64(&attachment, "hl", idx),
                    checksum_sha1: array_string(&attachment, "csl", idx),
                    size_bytes: array_i64(&attachment, "sl", idx),
                    filename: None,
                    full_stem: album_full_stem(row.log_id, idx),
                    full_ext: IMAGE_CACHE_EXT,
                    thumb_stem: album_thumb_stem(row.log_id, idx),
                    output_stem: format!("{}_{}", row.log_id, idx),
                    sender: Some(row.author_id.to_string()),
                    sent_at: Some(row.sent_at),
                    cdn_url: array_string(&attachment, "imageUrls", idx),
                })
                .collect();
        }
    }

    // A generic file has no local cache tier at all, so its cache stems are
    // never consulted; the original `name` carries the extension instead.
    if row.msg_type == FILE_MESSAGE_TYPE {
        return vec![MediaFrameInput {
            kind: MediaKind::File,
            log_id: row.log_id,
            idx: 0,
            width: None,
            height: None,
            checksum_sha1: object_string(&attachment, "cs"),
            size_bytes: object_i64(&attachment, "size").or_else(|| object_i64(&attachment, "s")),
            filename: object_string(&attachment, "name"),
            full_stem: String::new(),
            full_ext: "",
            thumb_stem: String::new(),
            output_stem: row.log_id.to_string(),
            sender: Some(row.author_id.to_string()),
            sent_at: Some(row.sent_at),
            cdn_url: object_string(&attachment, "url"),
        }];
    }

    let is_video = row.msg_type == VIDEO_MESSAGE_TYPE;
    vec![MediaFrameInput {
        kind: if is_video {
            MediaKind::Video
        } else {
            MediaKind::Photo
        },
        log_id: row.log_id,
        idx: 0,
        width: object_i64(&attachment, "w"),
        height: object_i64(&attachment, "h"),
        checksum_sha1: object_string(&attachment, "cs"),
        size_bytes: object_i64(&attachment, "s").or_else(|| object_i64(&attachment, "size")),
        filename: None,
        full_stem: if is_video {
            video_full_stem(row.log_id)
        } else {
            photo_full_stem(row.log_id)
        },
        full_ext: if is_video {
            VIDEO_CACHE_EXT
        } else {
            IMAGE_CACHE_EXT
        },
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
    use crate::kakao::media_paths::{album_full_stem, photo_full_stem, video_full_stem};

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

    #[test]
    fn normalizes_video_attachment_to_vid_cache_stem() {
        let row = MediaRow {
            log_id: 3_893_550_766_304_628_736,
            author_id: 456,
            msg_type: VIDEO_MESSAGE_TYPE,
            sent_at: 1_785_084_620,
            attachment: Some(
                r#"{"w":956,"h":534,"d":61,"s":8298343,"cs":"DE9DC05F42","url":"https://cdn.example/talkv_high.mp4?expires=1"}"#
                    .to_string(),
            ),
        };

        let frames = frame_inputs(row);

        assert_eq!(frames.len(), 1);
        // A video body lives at `<sha1_rev("v<logId>")>.vid`, never `.img`.
        assert_eq!(
            frames[0].full_stem,
            video_full_stem(3_893_550_766_304_628_736)
        );
        assert_eq!(frames[0].full_ext, ".vid");
        assert_ne!(
            frames[0].full_stem,
            photo_full_stem(3_893_550_766_304_628_736)
        );
        assert_eq!(frames[0].width, Some(956));
        assert_eq!(frames[0].checksum_sha1.as_deref(), Some("DE9DC05F42"));
        assert!(frames[0].cdn_url.is_some());
    }

    #[test]
    fn photo_rows_keep_the_image_cache_extension() {
        let row = MediaRow {
            log_id: 5,
            author_id: 1,
            msg_type: 2,
            sent_at: 1,
            attachment: Some(r#"{"w":1,"h":1}"#.to_string()),
        };

        assert_eq!(frame_inputs(row)[0].full_ext, ".img");
    }
}
