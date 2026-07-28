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
            "reply_edge",
            "SELECT child_message_id, parent_message_id, child_chunk_id, parent_chunk_id,
                    unresolved_reason
             FROM reply_edges
             ORDER BY child_message_id, parent_message_id",
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

fn assert_archive_invariants(conn: &Connection, expected_messages: i64) {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok", "sqlite integrity_check failed");

    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .expect("count messages");
    assert_eq!(messages, expected_messages, "message count");

    let chunk_messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunk_messages", [], |row| row.get(0))
        .expect("count chunk_messages");
    assert_eq!(
        chunk_messages, expected_messages,
        "every message must appear in exactly one chunk_messages row"
    );

    let chunks: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .expect("count chunks");
    let fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks_fts", [], |row| row.get(0))
        .expect("count chunks_fts");
    assert_eq!(chunks, fts, "chunks and chunks_fts must stay in lockstep");

    let orphan_cm: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk_messages cm
             LEFT JOIN chunks c ON c.chunk_id = cm.chunk_id
             WHERE c.chunk_id IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("orphan chunk_messages");
    assert_eq!(orphan_cm, 0, "orphan chunk_messages");

    let orphan_pc: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM parent_chunk_children pcc
             LEFT JOIN parent_chunks p ON p.parent_id = pcc.parent_id
             WHERE p.parent_id IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("orphan parent_chunk_children");
    assert_eq!(orphan_pc, 0, "orphan parent_chunk_children");
}

fn raw(
    chat_id: &str,
    message_id: &str,
    sender_nickname: &str,
    timestamp: chrono::DateTime<chrono::Utc>,
    text: &str,
    reply_to_message_id: Option<&str>,
) -> RawMessage {
    RawMessage {
        account_hash: "acct".to_string(),
        chat_id: chat_id.to_string(),
        chat_name: format!("방-{chat_id}"),
        chat_type: "group".to_string(),
        message_id: message_id.to_string(),
        sender_id: format!("u-{sender_nickname}"),
        sender_nickname: sender_nickname.to_string(),
        timestamp,
        text: text.to_string(),
        message_type: "text".to_string(),
        reply_to_message_id: reply_to_message_id.map(str::to_string),
    }
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
            "reply_edge",
            "SELECT r.child_message_id, r.parent_message_id, r.child_chunk_id, r.parent_chunk_id,
                    r.unresolved_reason
             FROM reply_edges r
             JOIN chunk_messages cm ON cm.message_id = r.child_message_id
             JOIN chunks c ON c.chunk_id = cm.chunk_id
             WHERE (?1 IS NULL OR c.started_at >= ?1)
             ORDER BY r.child_message_id, r.parent_message_id",
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

/// Two gap-separated bursts; the second is long enough that the 3,000-char parent-window limit
/// splits it into multiple windows even though no time gap opens inside it.
///
/// Each message is its own chunk (senders alternate). A ~220-char parent line times ~20 chunks
/// exceeds 3,000, so the second burst forms several char-split windows. The inter-burst gap is
/// wider than group_gap (600s) so same-sender messages cannot merge across it and hide the cut.
///
/// Returns `(messages, index_of_first_message_of_second_burst)`.
fn char_split_conversation() -> (Vec<RawMessage>, usize) {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let senders = ["보글이", "부리"];
    let long = "가".repeat(200);
    let mut messages = Vec::new();
    // Burst 0: three short messages, one window.
    for idx in 0..3usize {
        messages.push(raw(
            "C",
            &format!("c0{idx}"),
            senders[idx % 2],
            base + chrono::Duration::seconds(60 * idx as i64),
            &format!("초반 {idx}"),
            None,
        ));
    }
    // 700s gap (>600s group_gap and >300s parent-window gap) so the chunk stream itself splits.
    let burst1_start = 3usize;
    let burst1_base = 3 * 60 + 700;
    for idx in 0..20usize {
        messages.push(raw(
            "C",
            &format!("c1{idx:02}"),
            senders[idx % 2],
            base + chrono::Duration::seconds(burst1_base + 60 * idx as i64),
            &format!("{long}-{idx}"),
            None,
        ));
    }
    // One more short append at the end of the long burst (still within the group gap).
    messages.push(raw(
        "C",
        "c1-tail",
        "하울",
        base + chrono::Duration::seconds(burst1_base + 60 * 20),
        "꼬리 한 줄",
        None,
    ));
    (messages, burst1_start)
}

#[test]
fn a_char_limit_split_window_is_recomputed_and_matches_a_full_rebuild() {
    // The cut is gap-derived (conservative): char-split windows after the cut are recomputed, not
    // reused. If recompute dropped the char accumulation mid-window, parent rows would diverge.
    // Seed includes the long burst so the gap already exists in the archive; the wave is a single
    // append that must cut at the long burst's start and re-run its char-split windows.
    let (messages, split) = char_split_conversation();
    let last = messages.len() - 1;

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    let window_count: i64 = full
        .connection()
        .query_row("SELECT COUNT(*) FROM parent_chunks WHERE chat_id = 'C'", [], |row| {
            row.get(0)
        })
        .expect("count windows");
    assert!(
        window_count >= 3,
        "fixture must char-split the second burst into multiple windows, got {window_count}"
    );

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    let seed = staged
        .sync_messages(&messages[..last])
        .expect("sync seed");
    rebuild_chunks_for_chats(&staged, settings(), &seed.touched_chats).expect("seed rebuild");
    let wave = staged
        .sync_messages(&messages[last..])
        .expect("sync append");
    let cut = staged
        .tail_rebuild_start("C", &wave.touched_chats[0].earliest_changed_timestamp)
        .expect("resolve cut");
    assert_eq!(
        cut,
        Some(messages[split].timestamp.to_rfc3339()),
        "append into the char-split burst must cut at the gap, not rebuild the whole chat"
    );
    rebuild_chunks_for_chats(&staged, settings(), &wave.touched_chats).expect("scoped rebuild");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "char-split recompute diverged from a full rebuild"
    );
    assert_archive_invariants(staged.connection(), messages.len() as i64);
}

#[test]
fn an_in_place_nickname_change_widens_the_cut_and_matches_a_full_rebuild() {
    // upsert_message does not rewrite `text`; only nickname/reply_to (and chat meta) register as
    // updates. A mid-history nickname change moves chunk boundaries, so the cut must walk back.
    let (mut messages, _split) = bursty_conversation();
    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("seed full");

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    staged.sync_messages(&messages).expect("sync");
    rebuild_chunks(&staged).expect("seed staged");

    // Rename the second message of the first burst — no earlier gap, so the resolver must widen
    // to a whole-chat rebuild rather than freezing a changed prefix.
    messages[1].sender_nickname = "하울".to_string();
    let change = [messages[1].clone()];
    full.sync_messages(&change).expect("sync change full");
    rebuild_chunks(&full).expect("ground-truth full rebuild");

    let staged_report = staged.sync_messages(&change).expect("sync change staged");
    assert_eq!(staged_report.updated_messages, 1, "nickname change must count as updated");
    let cut = staged
        .tail_rebuild_start(
            "G",
            &staged_report.touched_chats[0].earliest_changed_timestamp,
        )
        .expect("resolve cut");
    assert_eq!(
        cut, None,
        "a change in the first burst has no earlier gap boundary; whole-chat rebuild is required"
    );
    rebuild_chunks_for_chats(&staged, settings(), &staged_report.touched_chats)
        .expect("scoped rebuild");
    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "in-place nickname change diverged from a full rebuild"
    );

    // Mid-room change: second burst, so the cut is the first gap (start of that burst).
    messages[4].sender_nickname = "새미".to_string();
    let mid = [messages[4].clone()];
    full.sync_messages(&mid).expect("sync mid full");
    rebuild_chunks(&full).expect("full after mid");
    let staged_mid = staged.sync_messages(&mid).expect("sync mid staged");
    let mid_cut = staged
        .tail_rebuild_start(
            "G",
            &staged_mid.touched_chats[0].earliest_changed_timestamp,
        )
        .expect("resolve mid cut");
    assert_eq!(
        mid_cut,
        Some(messages[3].timestamp.to_rfc3339()),
        "a change in the second burst must cut at the first gap (start of that burst)"
    );
    rebuild_chunks_for_chats(&staged, settings(), &staged_mid.touched_chats).expect("mid scoped");
    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "mid-history nickname change diverged from a full rebuild"
    );
    assert_archive_invariants(staged.connection(), messages.len() as i64);
}

#[test]
fn a_backfill_between_bursts_with_a_cross_chunk_reply_matches_a_full_rebuild() {
    // Late-arriving message between two bursts + a reply that lands in a different chunk: the
    // archive-wide ref pass must not leave stale chunk_parent_refs after the tail rewrite.
    let (mut messages, _split) = bursty_conversation();
    let base = messages[0].timestamp;

    // Reply from the last message of burst 2 back to the first message of burst 0 — different
    // chunks (senders already alternate, so each message is its own chunk).
    let last = messages.len() - 1;
    messages[last].reply_to_message_id = Some(messages[0].message_id.clone());

    // Backfill sits in the 400s gap between burst 0 (ends ~120s) and burst 1 (starts 520s).
    let backfill = raw(
        "G",
        "g-backfill",
        "보글이",
        base + chrono::Duration::seconds(300),
        "뒤늦은 메시지",
        Some(&messages[1].message_id),
    );

    let full_set: Vec<RawMessage> = {
        let mut all = messages.clone();
        all.push(backfill.clone());
        all.sort_by(|a, b| {
            (a.timestamp, a.message_id.as_str()).cmp(&(b.timestamp, b.message_id.as_str()))
        });
        all
    };

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&full_set).expect("sync full");
    rebuild_chunks(&full).expect("full rebuild");

    let parent_refs: i64 = full
        .connection()
        .query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |row| row.get(0))
        .expect("count parent refs");
    assert!(
        parent_refs > 0,
        "fixture must exercise non-empty chunk_parent_refs, got {parent_refs}"
    );

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    // Seed without the backfill; replies already present on the original set.
    let seed = staged.sync_messages(&messages).expect("sync seed");
    rebuild_chunks_for_chats(&staged, settings(), &seed.touched_chats).expect("seed rebuild");
    let wave = staged.sync_messages(&[backfill]).expect("sync backfill");
    assert_eq!(wave.inserted_messages, 1);
    let cut = staged
        .tail_rebuild_start("G", &wave.touched_chats[0].earliest_changed_timestamp)
        .expect("resolve cut");
    // Backfill at 300s is after burst 0 and before burst 1's start (520). The last gap boundary
    // strictly before e is the start of burst 0 only if no earlier gap — e is mid-history, so the
    // walk finds the gap between burst 0 and burst 1 only when looking at chunks with
    // started_at < e. Chunks of burst 1 start at 520 > e(300), so they are excluded. Chunks of
    // burst 0 have no prior gap → cut is None (whole chat). That is the correct widening.
    assert_eq!(
        cut, None,
        "a backfill before the second burst has no earlier gap; whole-chat rebuild is required"
    );
    rebuild_chunks_for_chats(&staged, settings(), &wave.touched_chats).expect("scoped rebuild");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "backfill + cross-chunk reply diverged from a full rebuild"
    );
    assert_archive_invariants(staged.connection(), full_set.len() as i64);
}

#[test]
fn messages_sharing_a_timestamp_at_a_boundary_lose_neither_coverage_nor_equivalence() {
    // The delete/insert floor is `started_at >= P` with no message_id tiebreak. Three messages
    // that share one timestamp at a gap boundary must still map 1:1 into chunk_messages.
    // Gap is >600s so the chunk stream splits even when a later same-sender message would
    // otherwise merge under the group gap. Seed includes the shared burst so the cut exists;
    // the wave is a single append past it.
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let senders = ["보글이", "부리", "하울"];
    let mut messages = Vec::new();
    for (idx, nick) in senders.iter().enumerate() {
        messages.push(raw(
            "T",
            &format!("t0{idx}"),
            nick,
            base + chrono::Duration::seconds(60 * idx as i64),
            &format!("앞 {idx}"),
            None,
        ));
    }
    // 700s gap (> group_gap and parent-window gap), then three messages at one timestamp.
    let shared = base + chrono::Duration::seconds(3 * 60 + 700);
    for (idx, nick) in senders.iter().enumerate() {
        messages.push(raw(
            "T",
            &format!("t1{idx}"),
            nick,
            shared,
            &format!("동시 {idx}"),
            None,
        ));
    }
    messages.push(raw(
        "T",
        "t-tail",
        "새미",
        shared + chrono::Duration::seconds(60),
        "꼬리",
        None,
    ));

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    let seed = staged
        .sync_messages(&messages[..messages.len() - 1])
        .expect("sync seed");
    rebuild_chunks_for_chats(&staged, settings(), &seed.touched_chats).expect("seed");
    let wave = staged
        .sync_messages(&messages[messages.len() - 1..])
        .expect("sync tail");
    let cut = staged
        .tail_rebuild_start("T", &wave.touched_chats[0].earliest_changed_timestamp)
        .expect("resolve cut");
    assert_eq!(
        cut,
        Some(shared.to_rfc3339()),
        "append past the shared-timestamp burst must cut at the gap boundary"
    );
    rebuild_chunks_for_chats(&staged, settings(), &wave.touched_chats).expect("scoped");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "shared-timestamp boundary diverged from a full rebuild"
    );
    assert_archive_invariants(staged.connection(), messages.len() as i64);
}

/// Build a single chat of `n` messages as many gap-separated bursts of `burst` messages.
///
/// Returns `(messages, index_of_first_message_of_last_burst)`.
fn large_room_messages(n: usize, burst: usize) -> (Vec<RawMessage>, usize) {
    assert!(n >= burst * 2, "need at least two bursts");
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    let senders = ["보글이", "부리"];
    let mut messages = Vec::with_capacity(n);
    let mut idx = 0usize;
    let mut burst_idx = 0usize;
    let mut last_burst_start = 0usize;
    while idx < n {
        let remaining = n - idx;
        let take = remaining.min(burst);
        last_burst_start = idx;
        // 700s between bursts: > parent-window gap (300s) and > group_gap (600s), so the chunk
        // stream itself splits at every burst and the gap-derived cut is visible in `chunks`.
        let burst_base = burst_idx as i64 * (700 + 60 * burst as i64);
        for within in 0..take {
            // Long text on some rows so the last burst char-splits parent windows.
            let text = if within % 3 == 0 {
                format!("{}-{idx}", "나".repeat(180))
            } else {
                format!("m{idx}")
            };
            messages.push(raw(
                "L",
                &format!("l{idx:06}"),
                senders[idx % 2],
                base + chrono::Duration::seconds(burst_base + 60 * within as i64),
                &text,
                None,
            ));
            idx += 1;
        }
        burst_idx += 1;
    }
    // Cross-chunk replies inside a burst (never across a gap cut) so chunk_parent_refs is
    // non-empty at scale without making the tail-only ground truth miss a parent message.
    for (i, message) in messages.iter_mut().enumerate() {
        if i > 0 && i % 97 == 0 && i % burst != 0 {
            message.reply_to_message_id = Some(format!("l{:06}", i - 1));
        }
    }
    (messages, last_burst_start)
}

fn prefix_snapshot(conn: &Connection, cut: &str) -> Vec<String> {
    let all = tail_snapshot(conn, None);
    let tail = tail_snapshot(conn, Some(cut));
    let tail_set: std::collections::HashSet<&String> = tail.iter().collect();
    all.into_iter()
        .filter(|row| !tail_set.contains(row))
        .collect()
}

#[test]
fn a_large_single_room_tail_matches_a_full_rebuild_and_cost_tracks_new_messages() {
    // The shape the five parent-plan rounds never built: one room with 100k+ messages.
    // Equivalence: ground-truth full rebuild of only the recompute tail (last burst + append) plus
    // a frozen-prefix check — a whole-room full rebuild of 100k is minutes and is not required to
    // pin the cut rule. Cost: scoped one-message append on the 100k room vs a 200-message room.
    // Release/CI default is 100k (the shape that hid this bug). Debug defaults to 5k so plain
    // `cargo test` stays usable; override either with KATOK_LARGE_CHAT_N.
    let n: usize = std::env::var("KATOK_LARGE_CHAT_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            5_000
        } else {
            100_000
        });
    let burst = 20usize;
    let (messages, last_burst_start) = large_room_messages(n, burst);
    assert!(
        messages.len() >= 5_000,
        "fixture must be large enough to leave prior rounds' few-hundred-msg rooms behind; got {}",
        messages.len()
    );

    // Seed: one sync of everything except the final message, then one whole-chat rebuild. Paying
    // the full rebuild once is cheaper than thousands of wave rebuilds (each re-runs the
    // archive-wide reply/parent-ref pass). The seed rebuild's wall time is the whole-chat baseline.
    let large_dir = tempfile::tempdir().expect("tempdir");
    let large = Archive::open(&large_dir.path().join("archive.sqlite3")).expect("open archive");
    large
        .sync_messages(&messages[..messages.len() - 1])
        .expect("sync large seed");
    let whole_started = std::time::Instant::now();
    rebuild_chunks_for_chats(&large, settings(), &[whole_chat("L")]).expect("seed whole-chat rebuild");
    let whole_seed_ms = whole_started.elapsed().as_millis();

    let cut = messages[last_burst_start].timestamp.to_rfc3339();
    let resolved = large
        .tail_rebuild_start("L", &messages[messages.len() - 1].timestamp.to_rfc3339())
        .expect("resolve cut")
        .expect("large room must have an interior gap cut");
    assert_eq!(
        resolved, cut,
        "append must cut at the last burst's gap boundary"
    );
    let prefix_before = prefix_snapshot(large.connection(), &cut);

    // Ground truth for the recompute region only.
    let tail_msgs = messages[last_burst_start..].to_vec();
    let truth_dir = tempfile::tempdir().expect("tempdir");
    let truth = Archive::open(&truth_dir.path().join("archive.sqlite3")).expect("open archive");
    truth.sync_messages(&tail_msgs).expect("sync truth tail");
    rebuild_chunks(&truth).expect("truth full rebuild of tail only");

    let started = std::time::Instant::now();
    let append = large
        .sync_messages(&messages[messages.len() - 1..])
        .expect("sync append");
    rebuild_chunks_for_chats(&large, settings(), &append.touched_chats).expect("scoped append");
    let large_scoped_ms = started.elapsed().as_millis();

    assert_eq!(
        tail_snapshot(large.connection(), Some(&cut)),
        tail_snapshot(truth.connection(), None),
        "large-room scoped tail diverged from a full rebuild of the same tail messages"
    );
    assert_eq!(
        prefix_before,
        prefix_snapshot(large.connection(), &cut),
        "append must not rewrite frozen prefix artifacts"
    );
    assert_archive_invariants(large.connection(), messages.len() as i64);
    let parent_refs: i64 = large
        .connection()
        .query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |row| row.get(0))
        .expect("count parent refs");
    assert!(parent_refs > 0, "large fixture must keep non-empty chunk_parent_refs");

    // Residual decomposition: the archive-wide ref pass alone is a large fraction of the scoped
    // floor (indexes on chunks are absent — see docs residual-cost section).
    let ref_started = std::time::Instant::now();
    large
        .rebuild_reply_and_parent_refs()
        .expect("ref pass only");
    let ref_only_ms = ref_started.elapsed().as_millis();

    // Cost claim: scoped one-message append is far cheaper than whole-chat on the same room.
    // Residual still grows with archive size (ref pass + unindexed scans), so we do not require
    // scoped(100k) ≈ scoped(200); we require scoped ≪ whole-chat for this room.
    assert!(
        large_scoped_ms * 10 < whole_seed_ms,
        "scoped append ({large_scoped_ms}ms) must be at least 10x faster than whole-chat seed \
         ({whole_seed_ms}ms) on a {n}-message room"
    );
    eprintln!(
        "large_room_cost n={n} whole_seed_ms={whole_seed_ms} large_scoped_ms={large_scoped_ms} \
         ref_only_ms={ref_only_ms} speedup={:.1}x",
        whole_seed_ms as f64 / large_scoped_ms.max(1) as f64
    );
}
