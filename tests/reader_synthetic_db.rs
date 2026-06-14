//! Hermetic reader test: build a synthetic SQLCipher database with the
//! KakaoTalk schema at test time (cipher_compatibility 3 + passphrase), insert
//! direct + group + reply rows, then drive the native reader against it and
//! assert the mapped `RawMessage`/`ChatSummary` output. No real KakaoTalk and
//! no system state (ioreg/plist) are touched: auth is resolved purely from the
//! injected overrides.

use std::path::Path;

use katok::kakao::{auth, derive, AuthOptions};
use rusqlite::Connection;

const TEST_UUID: &str = "42C34717-27C3-538C-81E4-8B568287C7A0";
const TEST_USER_ID: i64 = 240_061_982;

/// Open a writable SQLCipher DB with the reader's exact open recipe and create
/// the KakaoTalk schema. Returns the opened connection for further inserts.
fn open_with_schema(path: &Path, key: &str) -> Connection {
    let conn = Connection::open(path).expect("open writable db");
    // Mirror the reader's open recipe exactly (key first, then compat 3) so the
    // on-disk cipher parameters match what the reader will use.
    conn.execute_batch(&format!(
        "PRAGMA key = '{key}'; PRAGMA cipher_compatibility = 3;"
    ))
    .expect("apply cipher key");

    conn.execute_batch(
        "CREATE TABLE NTChatRoom (
            chatId INTEGER NOT NULL DEFAULT 0,
            linkId INTEGER NOT NULL DEFAULT 0,
            type INTEGER NOT NULL DEFAULT 0,
            chatName TEXT,
            activeMembersCount INTEGER NOT NULL DEFAULT 0,
            directChatMemberUserId INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (chatId, linkId)
        );
        CREATE TABLE NTUser (
            userId INTEGER NOT NULL DEFAULT 0,
            linkId INTEGER NOT NULL DEFAULT 0,
            friendNickName TEXT,
            nickName TEXT,
            displayName TEXT,
            PRIMARY KEY (userId, linkId)
        );
        CREATE TABLE NTChatMessage (
            chatId INTEGER NOT NULL DEFAULT 0,
            logId INTEGER NOT NULL DEFAULT 0,
            msgId INTEGER NOT NULL DEFAULT 0,
            authorId INTEGER NOT NULL DEFAULT 0,
            type INTEGER NOT NULL DEFAULT -1,
            supplement TEXT,
            message TEXT,
            sentAt INTEGER DEFAULT 0,
            PRIMARY KEY (chatId, logId, msgId)
        );",
    )
    .expect("create schema");
    conn
}

fn create_encrypted_db(path: &Path, key: &str) {
    let conn = open_with_schema(path, key);

    // Rooms: 100 is a direct chat (type 0), 200 is a group (type 1).
    conn.execute(
        "INSERT INTO NTChatRoom(chatId, linkId, type, chatName, activeMembersCount, directChatMemberUserId)
         VALUES (100, 0, 0, 'Alice DM', 2, 500), (200, 0, 1, 'Project Group', 4, 0)",
        [],
    )
    .expect("insert rooms");

    // Users: a peer (500) and self (240061982).
    conn.execute(
        "INSERT INTO NTUser(userId, linkId, friendNickName, nickName, displayName)
         VALUES (500, 0, 'Alice', NULL, 'Alice Display'),
                (240061982, 0, NULL, NULL, 'Me Display'),
                (600, 0, NULL, 'BobNick', NULL)",
        [],
    )
    .expect("insert users");

    // Messages: direct text, group text, a non-text type, and a reply.
    conn.execute(
        "INSERT INTO NTChatMessage(chatId, logId, msgId, authorId, type, supplement, message, sentAt)
         VALUES
            (100, 10, 1, 500, 1, NULL, 'direct hello', 1700000000),
            (100, 11, 2, 240061982, 1, NULL, 'direct reply body', 1700000100),
            (200, 20, 3, 600, 1, NULL, 'group message', 1700000200),
            (200, 21, 4, 500, 26, NULL, 'image caption', 1700000300),
            (200, 22, 5, 600, 1, '{\"reply\":{\"src_logId\":20}}', 'this is a reply', 1700000400),
            (200, 23, 6, 500, 1, NULL, '', 1700000500)",
        [],
    )
    .expect("insert messages");
}

#[test]
fn reads_synthetic_kakao_db_and_maps_model() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");

    // Place the synthetic DB where container_dir(home) will discover it, named
    // exactly as the real derivation would name it.
    let container = auth::container_dir(&home);
    std::fs::create_dir_all(&container).expect("container dir");
    let db_name = derive::database_name(TEST_USER_ID, TEST_UUID);
    let db_path = container.join(&db_name);
    let key = derive::secure_key(TEST_USER_ID, TEST_UUID);
    create_encrypted_db(&db_path, &key);

    // Resolve auth from injected overrides only (no ioreg/plist).
    let options = AuthOptions {
        home,
        data_dir: data_dir.clone(),
        user_id_override: Some(TEST_USER_ID),
        uuid_override: Some(TEST_UUID.to_string()),
        max_user_id: 0,
    };
    let output = katok::kakao::read_kakao_with_options(&options).expect("read kakao");

    // Empty-text message (logId 23) is filtered out → 5 messages remain.
    assert_eq!(output.messages.len(), 5);

    // Two chats discovered with correct classification.
    assert_eq!(output.chats.len(), 2);
    let direct = output
        .chats
        .iter()
        .find(|chat| chat.chat_id == "100")
        .expect("direct chat");
    assert_eq!(direct.chat_type, "direct");
    assert_eq!(direct.chat_name, "Alice DM");
    let group = output
        .chats
        .iter()
        .find(|chat| chat.chat_id == "200")
        .expect("group chat");
    assert_eq!(group.chat_type, "group");
    assert_eq!(group.chat_name, "Project Group");

    let by_id = |id: &str| {
        output
            .messages
            .iter()
            .find(|message| message.message_id == id)
            .unwrap_or_else(|| panic!("missing message {id}"))
    };

    // Direct message: sender join, timestamp, text message_type.
    let direct_hello = by_id("100-10");
    assert_eq!(direct_hello.chat_type, "direct");
    assert_eq!(direct_hello.sender_id, "500");
    assert_eq!(direct_hello.sender_nickname, "Alice"); // friendNickName wins
    assert_eq!(direct_hello.message_type, "text");
    assert_eq!(
        direct_hello.timestamp.to_rfc3339(),
        "2023-11-14T22:13:20+00:00"
    );

    // Self message uses displayName.
    let self_message = by_id("100-11");
    assert_eq!(self_message.sender_id, "240061982");
    assert_eq!(self_message.sender_nickname, "Me Display");

    // Group user falls back to nickName.
    let group_message = by_id("200-20");
    assert_eq!(group_message.chat_type, "group");
    assert_eq!(group_message.sender_nickname, "BobNick");

    // Non-text type becomes type_<n>.
    let image = by_id("200-21");
    assert_eq!(image.message_type, "type_26");

    // Reply links to the parent logId within the same chat.
    let reply = by_id("200-22");
    assert_eq!(reply.reply_to_message_id.as_deref(), Some("200-20"));

    // Account hash is the stable sha256(user_id) for every row.
    let account = &output.messages[0].account_hash;
    assert_eq!(account.len(), 64);
    assert!(output
        .messages
        .iter()
        .all(|message| &message.account_hash == account));

    // Messages are sorted chronologically per chat.
    let chat_200: Vec<&str> = output
        .messages
        .iter()
        .filter(|message| message.chat_id == "200")
        .map(|message| message.message_id.as_str())
        .collect();
    assert_eq!(chat_200, vec!["200-20", "200-21", "200-22"]);
}

/// A single malformed message row (a non-UTF8 BLOB stored in the TEXT `message`
/// column, which fails `String` deserialization) must be skipped, leaving the
/// surrounding well-formed rows intact — not abort the whole read.
#[test]
fn malformed_message_row_is_skipped_not_fatal() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");

    let container = auth::container_dir(&home);
    std::fs::create_dir_all(&container).expect("container dir");
    let db_name = derive::database_name(TEST_USER_ID, TEST_UUID);
    let db_path = container.join(&db_name);
    let key = derive::secure_key(TEST_USER_ID, TEST_UUID);

    let conn = open_with_schema(&db_path, &key);
    conn.execute(
        "INSERT INTO NTUser(userId, linkId, friendNickName, nickName, displayName)
         VALUES (500, 0, 'Alice', NULL, NULL)",
        [],
    )
    .expect("insert user");
    // Two good rows.
    conn.execute(
        "INSERT INTO NTChatMessage(chatId, logId, msgId, authorId, type, supplement, message, sentAt)
         VALUES (100, 10, 1, 500, 1, NULL, 'good one', 1700000000),
                (100, 12, 3, 500, 1, NULL, 'good two', 1700000200)",
        [],
    )
    .expect("insert good rows");
    // One malformed row: a non-UTF8 BLOB in the TEXT `message` column. It passes
    // the `message IS NOT NULL AND message <> ''` filter but fails String decode.
    conn.execute(
        "INSERT INTO NTChatMessage(chatId, logId, msgId, authorId, type, supplement, message, sentAt)
         VALUES (100, 11, 2, 500, 1, NULL, x'ff', 1700000100)",
        [],
    )
    .expect("insert malformed row");
    drop(conn);

    let options = AuthOptions {
        home,
        data_dir,
        user_id_override: Some(TEST_USER_ID),
        uuid_override: Some(TEST_UUID.to_string()),
        max_user_id: 0,
    };
    let output = katok::kakao::read_kakao_with_options(&options).expect("read kakao");

    // The malformed row is dropped; both good rows survive.
    let ids: Vec<&str> = output
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    assert_eq!(ids, vec!["100-10", "100-12"]);
}

/// A database file named `<78-hex>.db` (the optional reference suffix) must be
/// discovered and read, matching `HEX_DATABASE_PATTERN`.
#[test]
fn discovers_and_reads_db_suffixed_file() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");

    let container = auth::container_dir(&home);
    std::fs::create_dir_all(&container).expect("container dir");
    // Name the DB `<derived>.db` instead of the bare derived name.
    let db_name = format!("{}.db", derive::database_name(TEST_USER_ID, TEST_UUID));
    let db_path = container.join(&db_name);
    let key = derive::secure_key(TEST_USER_ID, TEST_UUID);
    create_encrypted_db(&db_path, &key);

    let options = AuthOptions {
        home,
        data_dir,
        user_id_override: Some(TEST_USER_ID),
        uuid_override: Some(TEST_UUID.to_string()),
        max_user_id: 0,
    };
    let output = katok::kakao::read_kakao_with_options(&options).expect("read kakao");

    // Same content as the bare-named DB: 5 mapped messages, 2 chats.
    assert_eq!(output.messages.len(), 5);
    assert_eq!(output.chats.len(), 2);
}
