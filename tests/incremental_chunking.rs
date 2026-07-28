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
    types::{RawMessage, TouchedChat},
};
use rusqlite::{params, Connection};

/// A `TouchedChat` that forces a whole-chat rebuild: an empty key sorts before every stored
/// timestamp, so `tail_rebuild_start` finds no interior boundary and rebuilds from the start.
fn whole_chat(chat_id: &str) -> TouchedChat {
    TouchedChat {
        chat_id: chat_id.to_string(),
        earliest_changed_timestamp: String::new(),
        earliest_changed_message_id: String::new(),
    }
}

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
    let touched: Vec<TouchedChat> = chats.iter().map(|id| whole_chat(id)).collect();
    rebuild_chunks_for_chats(&scoped, settings(), &touched).expect("scoped rebuild");

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
    rebuild_chunks_for_chats(&archive, settings(), &[whole_chat(&chats[0])]).expect("scoped rebuild");
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

/// A chat whose chunks split into two parent windows.
///
/// Senders alternate every message 60s apart, so each message is its own chunk (a nickname change
/// forces a boundary). The first six sit within the 300s parent-window gap and form one window; a
/// 400s jump before the seventh opens a second window. `split` (6) is that window boundary — the
/// first message of the last parent window.
fn windowed_conversation() -> (Vec<RawMessage>, usize) {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let senders = ["보글이", "부리"];
    let mut messages = Vec::new();
    for idx in 0..12usize {
        // First six march 60s apart; a 400s gap before index 6 forces the second window.
        let offset = if idx < 6 {
            60 * idx as i64
        } else {
            300 + 400 + 60 * (idx as i64 - 6)
        };
        messages.push(RawMessage {
            account_hash: "acct".to_string(),
            chat_id: "W".to_string(),
            chat_name: "윈도우방".to_string(),
            chat_type: "group".to_string(),
            message_id: format!("w{idx:03}"),
            sender_id: format!("u{}", idx % 2),
            sender_nickname: senders[idx % 2].to_string(),
            timestamp: base + chrono::Duration::seconds(offset),
            text: format!("메시지 {idx}"),
            message_type: "text".to_string(),
            reply_to_message_id: None,
        });
    }
    (messages, 6)
}

/// Every chunk artifact at or after `min_started_at`, joined across the child tables, as
/// comparable text. Passing `None` returns the whole archive.
fn tail_snapshot(conn: &Connection, min_started_at: Option<&str>) -> Vec<String> {
    let mut rows = Vec::new();
    for (label, sql) in [
        (
            "chunk",
            "SELECT chunk_id, chat_id, sender_nickname, started_at, ended_at, message_count, text
             FROM chunks
             WHERE (?1 IS NULL OR started_at >= ?1)
             ORDER BY chunk_id",
        ),
        (
            "parent",
            "SELECT parent_id, chat_id, started_at, ended_at, message_count, child_count, text
             FROM parent_chunks
             WHERE (?1 IS NULL OR started_at >= ?1)
             ORDER BY parent_id",
        ),
        (
            "chunk_message",
            "SELECT cm.chunk_id, cm.message_id, cm.ordinal
             FROM chunk_messages cm JOIN chunks c ON c.chunk_id = cm.chunk_id
             WHERE (?1 IS NULL OR c.started_at >= ?1)
             ORDER BY cm.chunk_id, cm.ordinal, cm.message_id",
        ),
        (
            "parent_child",
            "SELECT pcc.parent_id, pcc.chunk_id, pcc.ordinal
             FROM parent_chunk_children pcc JOIN parent_chunks p ON p.parent_id = pcc.parent_id
             WHERE (?1 IS NULL OR p.started_at >= ?1)
             ORDER BY pcc.parent_id, pcc.ordinal, pcc.chunk_id",
        ),
        (
            "parent_ref",
            "SELECT r.child_chunk_id, r.parent_chunk_id
             FROM chunk_parent_refs r JOIN chunks c ON c.chunk_id = r.child_chunk_id
             WHERE (?1 IS NULL OR c.started_at >= ?1)
             ORDER BY r.child_chunk_id, r.parent_chunk_id",
        ),
        (
            "fts",
            "SELECT f.chunk_id, f.text
             FROM chunks_fts f JOIN chunks c ON c.chunk_id = f.chunk_id
             WHERE (?1 IS NULL OR c.started_at >= ?1)
             ORDER BY f.chunk_id",
        ),
    ] {
        let mut stmt = conn.prepare(sql).expect("prepare tail snapshot query");
        let count = stmt.column_count();
        let mapped = stmt
            .query_map(params![min_started_at], |row| {
                let mut cells = Vec::with_capacity(count);
                for idx in 0..count {
                    let cell = row
                        .get::<_, String>(idx)
                        .or_else(|_| row.get::<_, i64>(idx).map(|n| n.to_string()))
                        .unwrap_or_else(|_| "<null>".to_string());
                    cells.push(cell);
                }
                Ok(format!("{label}|{}", cells.join("|")))
            })
            .expect("run tail snapshot query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect tail snapshot rows");
        rows.extend(mapped);
    }
    rows
}

/// Three conversation bursts in one chat, each separated by a gap over the 300s parent-window
/// gap, so the archive holds three parent windows. Senders alternate 60s apart within a burst, so
/// every message is its own chunk. `split` (6) is the boundary between the second and third burst.
fn bursty_conversation() -> (Vec<RawMessage>, usize) {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let senders = ["보글이", "부리"];
    // Burst n starts 400s + 60s*3 after the previous, well past the 300s window gap.
    let starts = [0i64, 520, 1040];
    let mut messages = Vec::new();
    let mut idx = 0usize;
    for (burst, start) in starts.iter().enumerate() {
        for within in 0..3usize {
            messages.push(RawMessage {
                account_hash: "acct".to_string(),
                chat_id: "G".to_string(),
                chat_name: "버스트방".to_string(),
                chat_type: "group".to_string(),
                message_id: format!("g{burst}{within}"),
                sender_id: format!("u{}", idx % 2),
                sender_nickname: senders[idx % 2].to_string(),
                timestamp: base + chrono::Duration::seconds(start + 60 * within as i64),
                text: format!("메시지 {idx}"),
                message_type: "text".to_string(),
                reply_to_message_id: None,
            });
            idx += 1;
        }
    }
    (messages, 6)
}

#[test]
fn an_incremental_wave_after_a_gap_rebuilds_only_the_tail_and_matches_a_full_rebuild() {
    // The scoped path must (a) actually engage — resolve a recompute start inside the chat rather
    // than rebuilding it whole — and (b) still land byte-identical to a full rebuild.
    let (messages, split) = bursty_conversation();

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    // First wave seeds the first two bursts (two windows), the second wave appends the third.
    let seed = staged
        .sync_messages(&messages[..split])
        .expect("sync wave one");
    rebuild_chunks_for_chats(&staged, settings(), &seed.touched_chats).expect("seed rebuild");

    let wave = staged
        .sync_messages(&messages[split..])
        .expect("sync wave two");
    // The change is the third burst; the resolver must cut at the second burst's start (the last
    // gap boundary among existing chunks), not fall back to a whole-chat rebuild.
    let cut = staged
        .tail_rebuild_start("G", &wave.touched_chats[0].earliest_changed_timestamp)
        .expect("resolve cut");
    let expected_cut = messages[3].timestamp.to_rfc3339();
    assert_eq!(
        cut,
        Some(expected_cut),
        "the wave should scope to the last gap boundary, not rebuild the whole chat"
    );
    rebuild_chunks_for_chats(&staged, settings(), &wave.touched_chats).expect("scoped rebuild");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "the tail-scoped incremental wave diverged from a full rebuild"
    );
}

#[test]
fn cutting_at_the_last_parent_window_reproduces_the_full_rebuild_tail() {
    // Pins the tail-scope rule (docs/incremental-chunking-tail-scope.md): rebuilding a chat from
    // only its last parent window's first message must produce byte-identical rows to the tail of
    // a full rebuild of the whole chat. If a boundary stops being local, or a closed parent window
    // starts depending on later chunks, this diverges.
    let (messages, split) = windowed_conversation();
    let threshold = messages[split].timestamp.to_rfc3339();

    // Full rebuild of the whole chat.
    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    // The fixture must actually exercise a non-trivial cut: two windows, with rows on both sides.
    let window_count: i64 = full
        .connection()
        .query_row("SELECT COUNT(*) FROM parent_chunks", [], |row| row.get(0))
        .expect("count windows");
    assert_eq!(window_count, 2, "fixture must form exactly two parent windows");
    let before_cut = tail_snapshot(full.connection(), None).len()
        - tail_snapshot(full.connection(), Some(&threshold)).len();
    assert!(before_cut > 0, "fixture must have artifacts before the cut");

    // Rebuild an archive holding only the tail — messages from the last window's start onward.
    let tail_dir = tempfile::tempdir().expect("tempdir");
    let tail = Archive::open(&tail_dir.path().join("archive.sqlite3")).expect("open archive");
    tail.sync_messages(&messages[split..]).expect("sync tail");
    rebuild_chunks(&tail).expect("tail rebuild");

    assert_eq!(
        tail_snapshot(full.connection(), Some(&threshold)),
        tail_snapshot(tail.connection(), None),
        "rebuilding from the last window's start diverged from the full rebuild's tail"
    );
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
