//! Synthetic media extraction tests: build an encrypted KakaoTalk-like DB and
//! local media cache, then drive the native media reader plus resolver. No real
//! KakaoTalk files, network, or user data are touched.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use aes::Aes256;
use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use katok::kakao::{
    auth, derive,
    media_paths::{
        album_full_stem, album_thumb_stem, chat_media_dir_name, photo_full_stem, photo_thumb_stem,
        MediaDirs,
    },
    media_resolver::{
        resolve_media_frames_with_fetcher, MediaRecord, MediaResolveOptions, MediaTier,
    },
    read_media_frames_with_options, AuthOptions, MediaQuery,
};
use rusqlite::{params, Connection};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;

const TEST_UUID: &str = "00000000-1111-2222-3333-444444444444";
const TEST_USER_ID: i64 = 1_000_000_001;
const CHAT_ID: i64 = 1_234_567_890_123;
const AUTHOR_ID: i64 = 500;
const MEDIA_ACCOUNT: &str = "0123456789abcdef0123456789abcdef01234567";

const LOG_FULL: i64 = 10_001;
const LOG_CDN: i64 = 10_002;
const LOG_THUMB_AFTER_CDN_MISMATCH: i64 = 10_003;
const LOG_STUB: i64 = 10_004;
const LOG_ALBUM: i64 = 10_005;

const CDN_URL: &str = "https://cdn.example/full.jpg?expires=1900000000";
const MISMATCH_URL: &str = "https://cdn.example/mismatch.jpg?expires=1900000000&secret=redacted";

const FULL_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-FULL\xff\xd9";
const CDN_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-CDN\xff\xd9";
const THUMB_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-THUMB\xff\xd9";
const ALBUM_FULL_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-ALBUM-FULL\xff\xd9";
const ALBUM_THUMB_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-ALBUM-THUMB\xff\xd9";
const WRONG_CDN_IMAGE: &[u8] = b"\xff\xd8\xff\xe0KATOK-WRONG-CDN\xff\xd9";

type Aes256CbcEncryptor = cbc::Encryptor<Aes256>;

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data_dir: PathBuf,
    media_dirs: MediaDirs,
    output_dir: PathBuf,
}

fn open_with_schema(path: &Path, key: &str) -> Connection {
    let conn = Connection::open(path).expect("open writable db");
    conn.execute_batch(&format!(
        "PRAGMA key = '{key}'; PRAGMA cipher_compatibility = 3;"
    ))
    .expect("apply cipher key");
    conn.execute_batch(
        "CREATE TABLE NTChatMessage (
            chatId INTEGER NOT NULL DEFAULT 0,
            logId INTEGER NOT NULL DEFAULT 0,
            msgId INTEGER NOT NULL DEFAULT 0,
            authorId INTEGER NOT NULL DEFAULT 0,
            type INTEGER NOT NULL DEFAULT -1,
            sentAt INTEGER DEFAULT 0,
            attachment TEXT,
            supplement TEXT,
            message TEXT,
            PRIMARY KEY (chatId, logId, msgId)
        );",
    )
    .expect("create media schema");
    conn
}

fn create_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("temp dir");
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let output_dir = tmp.path().join("out");
    std::fs::create_dir_all(&data_dir).expect("data dir");

    let container = auth::container_dir(&home);
    std::fs::create_dir_all(&container).expect("container dir");
    let key = derive::secure_key(TEST_USER_ID, TEST_UUID);
    let db_path = container.join(derive::database_name(TEST_USER_ID, TEST_UUID));
    let conn = open_with_schema(&db_path, &key);
    insert_media_rows(&conn);
    drop(conn);

    let account = container.join(MEDIA_ACCOUNT);
    let media_dirs = MediaDirs::from_roots_for_test(vec![account.clone()]);
    write_cached_media(
        &account,
        LOG_FULL,
        &photo_full_stem(LOG_FULL),
        ".img",
        FULL_IMAGE,
        1,
    );
    write_cached_media(
        &account,
        LOG_THUMB_AFTER_CDN_MISMATCH,
        &photo_thumb_stem(LOG_THUMB_AFTER_CDN_MISMATCH),
        ".thm",
        THUMB_IMAGE,
        2,
    );
    write_cached_media(
        &account,
        LOG_ALBUM,
        &album_full_stem(LOG_ALBUM, 0),
        ".img",
        ALBUM_FULL_IMAGE,
        3,
    );
    write_cached_media(
        &account,
        LOG_ALBUM,
        &album_thumb_stem(LOG_ALBUM, 1),
        ".thm",
        ALBUM_THUMB_IMAGE,
        4,
    );

    Fixture {
        _tmp: tmp,
        home,
        data_dir,
        media_dirs,
        output_dir,
    }
}

fn insert_media_rows(conn: &Connection) {
    insert_photo(
        conn,
        LOG_FULL,
        1,
        1_700_000_001,
        FULL_IMAGE,
        Some("https://cdn.example/unused.jpg?expires=1900000000"),
    );
    insert_photo(conn, LOG_CDN, 2, 1_700_000_002, CDN_IMAGE, Some(CDN_URL));
    insert_photo(
        conn,
        LOG_THUMB_AFTER_CDN_MISMATCH,
        3,
        1_700_000_003,
        THUMB_IMAGE,
        Some(MISMATCH_URL),
    );
    insert_photo(conn, LOG_STUB, 4, 1_700_000_004, FULL_IMAGE, None);

    let attachment = serde_json::json!({
        "wl": [320, 640],
        "hl": [240, 480],
        "csl": [sha1_hex(ALBUM_FULL_IMAGE), sha1_hex(ALBUM_THUMB_IMAGE)],
        "imageUrls": [null, null]
    })
    .to_string();
    conn.execute(
        "INSERT INTO NTChatMessage(chatId, logId, msgId, authorId, type, sentAt, attachment, supplement, message)
         VALUES (?1, ?2, ?3, ?4, 27, ?5, ?6, NULL, '')",
        params![CHAT_ID, LOG_ALBUM, 5_i64, AUTHOR_ID, 1_700_000_005_i64, attachment],
    )
    .expect("insert album row");
}

fn insert_photo(
    conn: &Connection,
    log_id: i64,
    msg_id: i64,
    sent_at: i64,
    image: &[u8],
    url: Option<&str>,
) {
    let attachment = serde_json::json!({
        "w": 640,
        "h": 480,
        "cs": sha1_hex(image),
        "url": url
    })
    .to_string();
    conn.execute(
        "INSERT INTO NTChatMessage(chatId, logId, msgId, authorId, type, sentAt, attachment, supplement, message)
         VALUES (?1, ?2, ?3, ?4, 2, ?5, ?6, NULL, '')",
        params![CHAT_ID, log_id, msg_id, AUTHOR_ID, sent_at, attachment],
    )
    .expect("insert photo row");
}

fn auth_options(fixture: &Fixture) -> AuthOptions {
    AuthOptions {
        home: fixture.home.clone(),
        data_dir: fixture.data_dir.clone(),
        user_id_override: Some(TEST_USER_ID),
        uuid_override: Some(TEST_UUID.to_string()),
        max_user_id: 0,
    }
}

fn media_query(log_id: Option<i64>) -> MediaQuery {
    MediaQuery {
        chat_id: CHAT_ID,
        log_id,
        limit: 100,
    }
}

fn resolve_options(output_dir: PathBuf) -> MediaResolveOptions {
    let mut options = MediaResolveOptions::new(output_dir);
    options.now_epoch = 1_700_000_000;
    options
}

fn write_cached_media(
    account: &Path,
    log_id: i64,
    stem: &str,
    ext: &str,
    image: &[u8],
    iv_seed: u8,
) {
    let path = account
        .join(chat_media_dir_name(CHAT_ID))
        .join(format!("{stem}{ext}"));
    std::fs::create_dir_all(path.parent().expect("cache parent")).expect("cache dir");
    std::fs::write(path, pkv2_image(image, log_id, iv_seed)).expect("cached pkv2 media");
}

fn pkv2_image(image: &[u8], log_id: i64, iv_seed: u8) -> Vec<u8> {
    let mut plaintext = vec![b'K'; 256];
    plaintext.extend_from_slice(image);
    let key_string: String = format!("#{log_id}%").chars().rev().collect();
    let aes_key = Sha256::digest(key_string.as_bytes());
    let iv = [iv_seed; 16];
    let mut buf = vec![0_u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(&plaintext);
    let ciphertext = Aes256CbcEncryptor::new_from_slices(&aes_key, &iv)
        .expect("aes init")
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .expect("pkcs7 encrypt")
        .to_vec();
    let mut out = b"Pkv2".to_vec();
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ciphertext);
    out
}

fn sha1_hex(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn record(
    report: &katok::kakao::media_resolver::MediaReport,
    log_id: i64,
    idx: usize,
) -> &MediaRecord {
    report
        .records
        .iter()
        .find(|record| record.log_id == log_id && record.idx == idx)
        .unwrap_or_else(|| panic!("missing record log_id={log_id} idx={idx}"))
}

#[test]
fn synthetic_db_media_pipeline_resolves_full_cdn_thumb_stub_and_album_frames() {
    let fixture = create_fixture();
    let frames = read_media_frames_with_options(&auth_options(&fixture), &media_query(None))
        .expect("read frames");

    assert_eq!(frames.len(), 6);
    assert!(frames
        .iter()
        .any(|frame| frame.log_id == LOG_ALBUM && frame.idx == 1 && frame.width == Some(640)));

    let fetch_calls = Rc::new(Cell::new(0));
    let calls = Rc::clone(&fetch_calls);
    let report = resolve_media_frames_with_fetcher(
        CHAT_ID,
        &frames,
        &fixture.media_dirs,
        &resolve_options(fixture.output_dir.clone()),
        move |url, timeout| {
            calls.set(calls.get() + 1);
            assert_eq!(timeout, Duration::from_secs(20));
            match url {
                CDN_URL => Ok(CDN_IMAGE.to_vec()),
                MISMATCH_URL => Ok(WRONG_CDN_IMAGE.to_vec()),
                other => panic!("unexpected CDN url {other}"),
            }
        },
    )
    .expect("resolve media");

    assert_eq!(fetch_calls.get(), 2);
    assert_eq!(report.tier_counts.get("full"), Some(&2));
    assert_eq!(report.tier_counts.get("cdn"), Some(&1));
    assert_eq!(report.tier_counts.get("thumb"), Some(&2));
    assert_eq!(report.tier_counts.get("stub"), Some(&1));

    let full = record(&report, LOG_FULL, 0);
    assert_eq!(full.tier, MediaTier::Full);
    assert_eq!(full.tier_reason, "decrypted");
    assert_eq!(full.sha1.as_deref(), Some(sha1_hex(FULL_IMAGE).as_str()));
    assert_eq!(
        std::fs::read(full.path.as_ref().expect("full path")).expect("full output"),
        FULL_IMAGE
    );

    let cdn = record(&report, LOG_CDN, 0);
    assert_eq!(cdn.tier, MediaTier::Cdn);
    assert_eq!(cdn.tier_reason, "full-not-cached+cdn-fetched");
    assert_eq!(cdn.sha1.as_deref(), Some(sha1_hex(CDN_IMAGE).as_str()));
    assert_eq!(
        std::fs::read(cdn.path.as_ref().expect("cdn path")).expect("cdn output"),
        CDN_IMAGE
    );

    let thumb = record(&report, LOG_THUMB_AFTER_CDN_MISMATCH, 0);
    assert_eq!(thumb.tier, MediaTier::Thumb);
    assert_eq!(thumb.tier_reason, "full-not-cached+cdn-failed");
    assert_eq!(thumb.sha1.as_deref(), Some(sha1_hex(THUMB_IMAGE).as_str()));
    assert_eq!(
        std::fs::read(thumb.path.as_ref().expect("thumb path")).expect("thumb output"),
        THUMB_IMAGE
    );

    let stub = record(&report, LOG_STUB, 0);
    assert_eq!(stub.tier, MediaTier::Stub);
    assert_eq!(stub.tier_reason, "not-cached");
    assert_eq!(stub.path, None);
    assert_eq!(stub.sha1, None);

    let album_full = record(&report, LOG_ALBUM, 0);
    assert_eq!(album_full.tier, MediaTier::Full);
    assert_eq!(
        album_full.sha1.as_deref(),
        Some(sha1_hex(ALBUM_FULL_IMAGE).as_str())
    );
    assert_eq!(
        album_full.path.as_ref().unwrap().file_name().unwrap(),
        "10005_0.jpg"
    );

    let album_thumb = record(&report, LOG_ALBUM, 1);
    assert_eq!(album_thumb.tier, MediaTier::Thumb);
    assert_eq!(album_thumb.tier_reason, "full-not-cached");
    assert_eq!(
        album_thumb.path.as_ref().unwrap().file_name().unwrap(),
        "10005_1_thumb.jpg"
    );
    assert_eq!(
        album_thumb.sha1.as_deref(),
        Some(sha1_hex(ALBUM_THUMB_IMAGE).as_str())
    );

    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].stage, "cdn");
    assert_eq!(report.errors[0].log_id, LOG_THUMB_AFTER_CDN_MISMATCH);
    assert_eq!(report.errors[0].path, "https://cdn.example/mismatch.jpg");
    assert!(report.errors[0].error.contains("cdn body sha1 != cs"));
}

#[test]
fn synthetic_db_no_cdn_mode_is_pure_local_and_degrades_to_stub() {
    let fixture = create_fixture();
    let frames =
        read_media_frames_with_options(&auth_options(&fixture), &media_query(Some(LOG_CDN)))
            .expect("read one CDN frame");
    assert_eq!(frames.len(), 1);

    let mut options = MediaResolveOptions::no_cdn(fixture.output_dir.clone());
    options.now_epoch = 1_700_000_000;
    let fetch_calls = Rc::new(Cell::new(0));
    let calls = Rc::clone(&fetch_calls);
    let report = resolve_media_frames_with_fetcher(
        CHAT_ID,
        &frames,
        &fixture.media_dirs,
        &options,
        move |_, _| {
            calls.set(calls.get() + 1);
            Ok(Vec::new())
        },
    )
    .expect("resolve no-cdn");

    assert_eq!(fetch_calls.get(), 0);
    assert!(report.errors.is_empty());
    assert_eq!(report.tier_counts.get("stub"), Some(&1));
    let cdn_disabled = record(&report, LOG_CDN, 0);
    assert_eq!(cdn_disabled.tier, MediaTier::Stub);
    assert_eq!(cdn_disabled.tier_reason, "not-cached");
    assert_eq!(cdn_disabled.path, None);
}
