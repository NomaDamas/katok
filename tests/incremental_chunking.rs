//! The per-chat chunk rebuild must produce exactly what a full rebuild produces.
//!
//! This is the safety net for making sync incremental: the speedup is only worth having if the
//! archive it leaves behind is indistinguishable from the one the slow path built.

use katok::{
    archive::Archive,
    chunking::{
        rebuild_chunks, rebuild_chunks_for_chats, rebuild_chunks_with_settings, ChunkSettings,
        CHUNKER_VERSION,
    },
    fixture::read_fixture,
    types::RawMessage,
};
use rusqlite::Connection;

fn fixture_messages() -> Vec<RawMessage> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kakao/replies.jsonl");
    read_fixture(&path).expect("read fixture")
}

/// Every chunk artifact, in a stable order, as comparable text.
fn snapshot(conn: &Connection) -> Vec<String> {
    let mut rows = Vec::new();
    for (label, sql) in [
        (
            "chunk",
            "SELECT chunk_id, chat_id, sender_nickname, started_at, ended_at, message_count, text
             FROM chunks ORDER BY chunk_id",
        ),
        (
            "parent",
            "SELECT parent_id, chat_id, started_at, ended_at, message_count, child_count, text
             FROM parent_chunks ORDER BY parent_id",
        ),
        (
            "chunk_message",
            "SELECT chunk_id, message_id, ordinal FROM chunk_messages
             ORDER BY chunk_id, ordinal, message_id",
        ),
        (
            "parent_child",
            "SELECT parent_id, chunk_id, ordinal FROM parent_chunk_children
             ORDER BY parent_id, ordinal, chunk_id",
        ),
        (
            "parent_ref",
            "SELECT child_chunk_id, parent_chunk_id FROM chunk_parent_refs
             ORDER BY child_chunk_id, parent_chunk_id",
        ),
        (
            "fts",
            "SELECT chunk_id, text FROM chunks_fts ORDER BY chunk_id",
        ),
    ] {
        let mut stmt = conn.prepare(sql).expect("prepare snapshot query");
        let count = stmt.column_count();
        let mapped = stmt
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(count);
                for idx in 0..count {
                    // Every selected column is TEXT or INTEGER; render both as text.
                    let cell = row
                        .get::<_, String>(idx)
                        .or_else(|_| row.get::<_, i64>(idx).map(|n| n.to_string()))
                        .unwrap_or_else(|_| "<null>".to_string());
                    cells.push(cell);
                }
                Ok(format!("{label}|{}", cells.join("|")))
            })
            .expect("run snapshot query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect snapshot rows");
        rows.extend(mapped);
    }
    rows
}

fn settings() -> ChunkSettings {
    ChunkSettings::default()
}

#[test]
fn per_chat_rebuild_matches_a_full_rebuild_of_the_same_archive() {
    let messages = fixture_messages();
    let chats: Vec<String> = {
        let mut ids: Vec<String> = messages.iter().map(|m| m.chat_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    assert!(!chats.is_empty(), "fixture must contain at least one chat");

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    let scoped_dir = tempfile::tempdir().expect("tempdir");
    let scoped = Archive::open(&scoped_dir.path().join("archive.sqlite3")).expect("open archive");
    scoped.sync_messages(&messages).expect("sync");
    // Seed with a full pass, then throw every chat back through the scoped path: if the scoped
    // path drifts in either direction (drops rows, or leaves stale ones behind) this diverges.
    rebuild_chunks(&scoped).expect("seed rebuild");
    rebuild_chunks_for_chats(&scoped, settings(), &chats).expect("scoped rebuild");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(scoped.connection()),
        "per-chat rebuild diverged from a full rebuild"
    );
}

#[test]
fn a_gap_settings_change_is_recorded_so_sync_can_notice_the_drift() {
    // Scoping the rebuild to changed chats means a settings change would otherwise only reach
    // rooms that happen to receive a message. sync compares the recorded settings against the
    // configured ones and takes the full path when they differ; this pins the recording half,
    // which is what makes that comparison possible.
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");

    assert_eq!(
        archive.stored_chunk_settings().expect("read settings"),
        None,
        "a fresh archive has recorded nothing yet, which must read as drift"
    );

    archive
        .record_chunk_settings(600, 1_800, CHUNKER_VERSION)
        .expect("record");
    assert_eq!(
        archive.stored_chunk_settings().expect("read settings"),
        Some((600, 1_800, CHUNKER_VERSION))
    );

    // Re-recording different values overwrites rather than accumulating.
    archive
        .record_chunk_settings(120, 300, CHUNKER_VERSION)
        .expect("re-record");
    assert_eq!(
        archive.stored_chunk_settings().expect("read settings"),
        Some((120, 300, CHUNKER_VERSION))
    );

    // A chunker change reads as drift even when the gaps are untouched, which is what stops a
    // logic upgrade from reaching only the rooms that happen to receive a message.
    archive
        .record_chunk_settings(120, 300, CHUNKER_VERSION + 1)
        .expect("record newer chunker");
    assert_ne!(
        archive.stored_chunk_settings().expect("read settings"),
        Some((120, 300, CHUNKER_VERSION))
    );
}

#[test]
fn different_gap_settings_produce_different_chunks() {
    // The reason the drift check has to exist: the same messages chunk differently under
    // different gaps, so stale settings mean stale boundaries.
    let messages = synthetic_conversation();

    let wide_dir = tempfile::tempdir().expect("tempdir");
    let wide = Archive::open(&wide_dir.path().join("archive.sqlite3")).expect("open archive");
    wide.sync_messages(&messages).expect("sync");
    rebuild_chunks_with_settings(&wide, ChunkSettings::default()).expect("wide rebuild");

    let narrow_dir = tempfile::tempdir().expect("tempdir");
    let narrow = Archive::open(&narrow_dir.path().join("archive.sqlite3")).expect("open archive");
    narrow.sync_messages(&messages).expect("sync");
    rebuild_chunks_with_settings(
        &narrow,
        ChunkSettings {
            // Below the 60s spacing of the synthetic run, so every message stands alone.
            group_gap_seconds: 30,
            direct_gap_seconds: 30,
        },
    )
    .expect("narrow rebuild");

    assert_ne!(
        snapshot(wide.connection()),
        snapshot(narrow.connection()),
        "gap settings must change the resulting chunks, or the drift check guards nothing"
    );
}

#[test]
fn rebuilding_one_chat_leaves_the_other_chats_untouched() {
    let messages = fixture_messages();
    let mut chats: Vec<String> = messages.iter().map(|m| m.chat_id.clone()).collect();
    chats.sort();
    chats.dedup();
    if chats.len() < 2 {
        // Nothing to isolate; the equivalence test above already covers the single-chat case.
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&messages).expect("sync");
    rebuild_chunks(&archive).expect("full rebuild");

    let before = snapshot(archive.connection());
    rebuild_chunks_for_chats(&archive, settings(), &chats[..1]).expect("scoped rebuild");
    let after = snapshot(archive.connection());

    assert_eq!(
        before, after,
        "rebuilding one chat changed rows it should not own"
    );
}

/// A conversation long enough to be split mid-chunk.
///
/// Chat `A` is one uninterrupted run by one sender 60s apart, so the whole run is a single chunk
/// under the 600s group gap — splitting it at index 8 leaves an open chunk that the second wave
/// must extend rather than start afresh. Chat `B` sits entirely past the split, so the second
/// wave introduces a chat the first wave never saw.
fn synthetic_conversation() -> Vec<RawMessage> {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let mut messages = Vec::new();
    for idx in 0..12 {
        messages.push(RawMessage {
            account_hash: "acct".to_string(),
            chat_id: "A".to_string(),
            chat_name: "그룹방".to_string(),
            chat_type: "group".to_string(),
            message_id: format!("a{idx:03}"),
            sender_id: "u1".to_string(),
            sender_nickname: "보글이".to_string(),
            timestamp: base + chrono::Duration::seconds(60 * idx),
            text: format!("메시지 {idx}"),
            message_type: "text".to_string(),
            reply_to_message_id: None,
        });
    }
    for idx in 0..4 {
        messages.push(RawMessage {
            account_hash: "acct".to_string(),
            chat_id: "B".to_string(),
            chat_name: "1:1".to_string(),
            chat_type: "direct".to_string(),
            message_id: format!("b{idx:03}"),
            sender_id: "u2".to_string(),
            sender_nickname: "부리".to_string(),
            timestamp: base + chrono::Duration::seconds(30 * idx),
            text: format!("직접 {idx}"),
            message_type: "text".to_string(),
            reply_to_message_id: None,
        });
    }
    messages
}

#[test]
fn arriving_messages_extend_chunks_the_same_way_a_full_rebuild_would() {
    let messages = synthetic_conversation();
    let split = 8;

    // One archive gets everything at once.
    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    // The other receives it in two waves, rebuilding only the chats each wave touched — which is
    // exactly what an incremental sync does.
    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    for wave in [&messages[..split], &messages[split..]] {
        let report = staged.sync_messages(wave).expect("sync wave");
        rebuild_chunks_for_chats(&staged, settings(), &report.touched_chats)
            .expect("scoped rebuild");
    }

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "incremental waves produced a different archive than one full pass"
    );
}
