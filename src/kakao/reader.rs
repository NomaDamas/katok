//! Open KakaoTalk SQLCipher DBs read-only and map their schema to the katok
//! model. The open recipe is empirically verified: `cipher_compatibility = 3`
//! then a passphrase `PRAGMA key`, then a `sqlite_master` probe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use crate::types::RawMessage;
use crate::{Error, Result};

/// Output of reading one or more KakaoTalk databases.
#[derive(Debug, Clone)]
pub struct ReaderOutput {
    pub chats: Vec<ChatRecord>,
    pub messages: Vec<RawMessage>,
}

/// A chat room reduced to the fields katok needs.
#[derive(Debug, Clone)]
pub struct ChatRecord {
    pub chat_id: String,
    pub chat_name: String,
    pub chat_type: String,
}

#[derive(Debug, Clone)]
struct RoomMeta {
    name: Option<String>,
    chat_type: String,
}

/// Open `path` with `key` as a passphrase in cipher-compatibility mode 3,
/// read-only, and confirm the schema is readable. Returns Ok(true) on success,
/// Ok(false) if the key does not fit, Err only on unexpected I/O.
pub fn probe_database(path: &Path, key: &str) -> Result<bool> {
    match open_database(path, key) {
        Ok(conn) => {
            let ok = conn
                .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map(|count| count > 0)
                .unwrap_or(false);
            Ok(ok)
        }
        Err(_) => Ok(false),
    }
}

fn open_database(path: &Path, key: &str) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(Error::Sql)?;
    // The 256-hex key is ASCII hex, safe to interpolate inside single quotes.
    // The passphrase must be applied BEFORE `cipher_compatibility`; setting the
    // compatibility level first resets cipher state and yields "file is not a
    // database" against the bundled SQLCipher 4.5.x. `key` first then compat 3
    // matches both the bundled build and the system sqlcipher CLI.
    conn.execute_batch(&format!(
        "PRAGMA key = '{key}'; PRAGMA cipher_compatibility = 3;"
    ))
    .map_err(Error::Sql)?;
    // Force a read so an incorrect key fails here.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(Error::Sql)?;
    Ok(conn)
}

fn account_hash(user_id: i64) -> String {
    let digest = Sha256::digest(user_id.to_string().as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn classify_room(room_type: i64, direct_member: i64, active_members: i64) -> String {
    if room_type == 0 || room_type == 2 || direct_member > 0 || active_members == 2 {
        "direct".to_string()
    } else {
        "group".to_string()
    }
}

fn load_rooms(conn: &Connection) -> Result<HashMap<i64, RoomMeta>> {
    let mut stmt = conn
        .prepare(
            "SELECT chatId, type, chatName, activeMembersCount, directChatMemberUserId
             FROM NTChatRoom",
        )
        .map_err(Error::Sql)?;
    let mut rooms = HashMap::new();
    let rows = stmt
        .query_map([], |row| {
            let chat_id: i64 = row.get(0)?;
            let room_type: i64 = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let active_members: i64 = row.get(3)?;
            let direct_member: i64 = row.get(4)?;
            Ok((
                chat_id,
                RoomMeta {
                    name: name.filter(|value| !value.is_empty()),
                    chat_type: classify_room(room_type, direct_member, active_members),
                },
            ))
        })
        .map_err(Error::Sql)?;
    // Skip (not abort on) any single row that fails to deserialize; never log
    // row content.
    for row in rows.flatten() {
        let (chat_id, meta) = row;
        rooms.entry(chat_id).or_insert(meta);
    }
    Ok(rooms)
}

fn load_users(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut stmt = conn
        .prepare(
            "SELECT userId, friendNickName, displayName, nickName
             FROM NTUser",
        )
        .map_err(Error::Sql)?;
    let mut users: HashMap<i64, String> = HashMap::new();
    let rows = stmt
        .query_map([], |row| {
            let user_id: i64 = row.get(0)?;
            let friend: Option<String> = row.get(1)?;
            let display: Option<String> = row.get(2)?;
            let nick: Option<String> = row.get(3)?;
            Ok((user_id, friend, display, nick))
        })
        .map_err(Error::Sql)?;
    // Skip (not abort on) any single row that fails to deserialize; never log
    // row content.
    for row in rows.flatten() {
        let (user_id, friend, display, nick) = row;
        let name = friend
            .filter(|value| !value.is_empty())
            .or(display.filter(|value| !value.is_empty()))
            .or(nick.filter(|value| !value.is_empty()));
        if let Some(name) = name {
            users.entry(user_id).or_insert(name);
        }
    }
    Ok(users)
}

/// Best-effort: parse `supplement` JSON for a referenced parent `logId` in the
/// same chat. Only the JSON structure is inspected, never message bodies.
///
/// `supplement` is present on ~64% of messages (media/attachment/link-preview
/// metadata, not just replies), so this is deliberately conservative to avoid
/// synthesizing false reply edges:
///   * only reply-specific keys are accepted (the bare generic `logId` is NOT,
///     since media supplements legitimately embed it for non-reply reasons);
///   * those keys are looked up directly on the top-level object and, by
///     preference, inside a reply/src-shaped container (`reply`/`src`/`origin`/
///     `parent`), rather than anywhere at any depth.
fn reply_parent_log_id(supplement: Option<&str>) -> Option<i64> {
    let raw = supplement?;
    if raw.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let serde_json::Value::Object(root) = &value else {
        return None;
    };

    // Reply-specific keys only; the generic "logId" is intentionally excluded.
    const REPLY_KEYS: [&str; 4] = ["src_logId", "srcLogId", "parentLogId", "src_log_id"];
    // Containers a reply reference is shaped under.
    const CONTAINER_KEYS: [&str; 4] = ["reply", "src", "origin", "parent"];

    fn lookup_reply_id(map: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
        for key in REPLY_KEYS {
            if let Some(found) = map.get(key) {
                if let Some(id) = coerce_log_id(found) {
                    return Some(id);
                }
            }
        }
        None
    }

    // Prefer a reply/src-shaped container, then fall back to the top level.
    for container in CONTAINER_KEYS {
        if let Some(serde_json::Value::Object(inner)) = root.get(container) {
            if let Some(id) = lookup_reply_id(inner) {
                return Some(id);
            }
        }
    }
    lookup_reply_id(root)
}

fn coerce_log_id(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(num) => num.as_i64().filter(|id| *id > 0),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok().filter(|id| *id > 0),
        _ => None,
    }
}

fn unreadable_database_warning() -> &'static str {
    "katok: skipping unreadable KakaoTalk db"
}

/// Read a single opened database into chats + messages. `users` is built once
/// across all DBs (see `read_databases`) so an author has one stable nickname —
/// the chunk grouping key — regardless of which DB a message came from.
fn read_one(
    conn: &Connection,
    user_id: i64,
    account: &str,
    users: &HashMap<i64, String>,
) -> Result<(HashMap<String, ChatRecord>, Vec<RawMessage>)> {
    let rooms = load_rooms(conn)?;

    let mut stmt = conn
        .prepare(
            "SELECT chatId, logId, authorId, type, message, sentAt, supplement
             FROM NTChatMessage
             WHERE message IS NOT NULL AND message <> ''",
        )
        .map_err(Error::Sql)?;

    let mut messages = Vec::new();
    let mut chats: HashMap<String, ChatRecord> = HashMap::new();

    let rows = stmt
        .query_map([], |row| {
            let chat_id: i64 = row.get(0)?;
            let log_id: i64 = row.get(1)?;
            let author_id: i64 = row.get(2)?;
            let msg_type: i64 = row.get(3)?;
            let text: String = row.get(4)?;
            // sentAt is epoch seconds; tolerate a float column by reading f64.
            let sent_at: f64 = row.get::<_, f64>(5).unwrap_or(0.0);
            let supplement: Option<String> = row.get(6)?;
            Ok((
                chat_id, log_id, author_id, msg_type, text, sent_at, supplement,
            ))
        })
        .map_err(Error::Sql)?;

    // Skip (and count) any single row that fails to deserialize — a non-UTF8
    // body or an unexpected column affinity in one corrupt row must not abort an
    // otherwise-healthy read. Row content is never logged.
    let mut skipped_rows: usize = 0;
    for row in rows {
        let (chat_id, log_id, author_id, msg_type, text, sent_at, supplement) = match row {
            Ok(values) => values,
            Err(_) => {
                skipped_rows += 1;
                continue;
            }
        };

        let room = rooms.get(&chat_id);
        let chat_type = room
            .map(|meta| meta.chat_type.clone())
            .unwrap_or_else(|| "direct".to_string());
        let chat_name = room
            .and_then(|meta| meta.name.clone())
            .unwrap_or_else(|| format!("chat-{chat_id}"));

        let sender_nickname = if author_id == user_id {
            users
                .get(&author_id)
                .cloned()
                .unwrap_or_else(|| "나".to_string())
        } else {
            users
                .get(&author_id)
                .cloned()
                .unwrap_or_else(|| format!("user-{author_id}"))
        };

        let timestamp = Utc
            .timestamp_opt(sent_at as i64, 0)
            .single()
            .unwrap_or_else(|| {
                Utc.timestamp_opt(0, 0)
                    .single()
                    .expect("epoch zero is valid")
            });

        let message_type = if msg_type == 1 {
            "text".to_string()
        } else {
            format!("type_{msg_type}")
        };

        let reply_to_message_id = reply_parent_log_id(supplement.as_deref())
            .filter(|parent| *parent != log_id)
            .map(|parent| format!("{chat_id}-{parent}"));

        let chat_id_str = chat_id.to_string();
        chats
            .entry(chat_id_str.clone())
            .or_insert_with(|| ChatRecord {
                chat_id: chat_id_str.clone(),
                chat_name: chat_name.clone(),
                chat_type: chat_type.clone(),
            });

        messages.push(RawMessage {
            account_hash: account.to_string(),
            chat_id: chat_id_str,
            chat_name,
            chat_type,
            message_id: format!("{chat_id}-{log_id}"),
            sender_id: author_id.to_string(),
            sender_nickname,
            timestamp,
            text,
            message_type,
            reply_to_message_id,
        });
    }

    if skipped_rows > 0 {
        eprintln!("katok: skipped {skipped_rows} unreadable KakaoTalk message row(s)");
    }

    Ok((chats, messages))
}

/// Read all openable databases, union + dedup messages by `message_id`
/// (`{chatId}-{logId}`), and sort chronologically per chat.
pub fn read_databases(
    database_files: &[PathBuf],
    user_id: i64,
    uuid: &str,
) -> Result<ReaderOutput> {
    let key = super::derive::secure_key(user_id, uuid);
    let account = account_hash(user_id);

    // Open each DB once. A DB that fails to open is skipped with a one-line
    // warning (filename only, never content), mirroring the per-row degrade.
    let mut openable: Vec<(&PathBuf, Connection)> = Vec::new();
    for path in database_files {
        match open_database(path, &key) {
            Ok(conn) => openable.push((path, conn)),
            Err(_) => eprintln!("{}", unreadable_database_warning()),
        }
    }

    // Build the users (nickname) map ONCE across all openable DBs so an author
    // resolves to one stable `sender_nickname` — the chunk grouping key —
    // regardless of which DB a message came from. A load_users failure on one DB
    // is tolerated (that DB simply contributes no names).
    let mut users: HashMap<i64, String> = HashMap::new();
    for (_, conn) in &openable {
        if let Ok(db_users) = load_users(conn) {
            for (user, name) in db_users {
                users.entry(user).or_insert(name);
            }
        }
    }

    let mut chats: HashMap<String, ChatRecord> = HashMap::new();
    let mut messages_by_id: HashMap<String, RawMessage> = HashMap::new();

    for (_, conn) in &openable {
        // A read_one failure on one DB (e.g. a schema mismatch in a secondary
        // store) must not abort messages already gathered from a healthy DB:
        // log a one-line warning (filename only) and skip this DB.
        let (db_chats, db_messages) = match read_one(conn, user_id, &account, &users) {
            Ok(result) => result,
            Err(_) => {
                eprintln!("{}", unreadable_database_warning());
                continue;
            }
        };
        for (chat_id, record) in db_chats {
            chats.entry(chat_id).or_insert(record);
        }
        for message in db_messages {
            messages_by_id
                .entry(message.message_id.clone())
                .or_insert(message);
        }
    }

    let mut messages: Vec<RawMessage> = messages_by_id.into_values().collect();
    messages.sort_by(|a, b| {
        a.chat_id
            .cmp(&b.chat_id)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.message_id.cmp(&b.message_id))
    });

    let mut chat_list: Vec<ChatRecord> = chats.into_values().collect();
    chat_list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));

    Ok(ReaderOutput {
        chats: chat_list,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_direct_and_group_rooms() {
        assert_eq!(classify_room(0, 0, 2), "direct");
        assert_eq!(classify_room(2, 0, 0), "direct");
        assert_eq!(classify_room(1, 999, 5), "direct"); // directChatMemberUserId > 0
        assert_eq!(classify_room(1, 0, 5), "group");
        assert_eq!(classify_room(4, 0, 100), "group");
    }

    #[test]
    fn account_hash_is_64_hex() {
        let hash = account_hash(240_061_982);
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn parses_reply_parent_from_supplement() {
        let supplement = r#"{"reply":{"src_logId":12345,"text":"ignored"}}"#;
        assert_eq!(reply_parent_log_id(Some(supplement)), Some(12345));
    }

    #[test]
    fn parses_reply_parent_from_top_level_reply_key() {
        // Reply-specific key at the top level (no container) is still accepted.
        assert_eq!(
            reply_parent_log_id(Some(r#"{"src_logId":"777"}"#)),
            Some(777)
        );
        assert_eq!(
            reply_parent_log_id(Some(r#"{"parentLogId":888}"#)),
            Some(888)
        );
    }

    #[test]
    fn returns_none_for_non_reply_supplement() {
        assert_eq!(reply_parent_log_id(Some(r#"{"foo":"bar"}"#)), None);
        assert_eq!(reply_parent_log_id(Some("")), None);
        assert_eq!(reply_parent_log_id(None), None);
        assert_eq!(reply_parent_log_id(Some("not json")), None);
    }

    #[test]
    fn bare_logid_does_not_synthesize_reply_edge() {
        // A media/link-preview supplement that embeds a generic "logId" (or a
        // nested non-reply object) must NOT be read as a reply reference.
        assert_eq!(reply_parent_log_id(Some(r#"{"logId":4242}"#)), None);
        assert_eq!(
            reply_parent_log_id(Some(r#"{"media":{"logId":4242,"url":"x"}}"#)),
            None
        );
        // A reply key buried under an unrelated container is also not promoted.
        assert_eq!(
            reply_parent_log_id(Some(r#"{"attachment":{"src_logId":4242}}"#)),
            None
        );
    }

    #[test]
    fn unreadable_database_warning_does_not_disclose_filename() {
        let filename =
            "3080037d7a3b71fbe90b9492c50faf90eb3a8d708baec8ec3f18346bf53568cf84c0251259f2a6";
        let warning = unreadable_database_warning();

        assert!(!warning.contains(filename));
        assert!(!warning.contains(".db"));
        assert!(warning.contains("skipping unreadable KakaoTalk db"));
    }
}
