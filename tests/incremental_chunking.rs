//! The per-chat chunk rebuild must produce exactly what a full rebuild produces.
//!
//! This is the safety net for making sync incremental: the speedup is only worth having if the
//! archive it leaves behind is indistinguishable from the one the slow path built.

use katok::{
    archive::{
        Archive, DELETE_CHAT_CHUNKS_STATEMENTS, RAW_MESSAGES_FOR_CHAT_SINCE_QUERY,
        SCOPED_REF_REBUILD_STATEMENTS, TAIL_REBUILD_START_QUERY,
    },
    chunking::{
        rebuild_chunks, rebuild_chunks_for_chats, rebuild_chunks_with_settings, ChunkSettings,
        CHUNKER_VERSION,
    },
    fixture::read_fixture,
    search::bm25_search,
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

    // The tail delete seeks fts rows by rowid, so `chunks_fts.rowid` must be the same rowid the
    // chunk carries in `chunks` (the invariant documented on the table in `schema.rs`, and the
    // one `search.rs` already joins on). A count match alone would pass an archive where the two
    // drifted apart, which would leave the delete removing the wrong rows.
    let misaligned_fts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks_fts f
             LEFT JOIN chunks c ON c.rowid = f.rowid AND c.chunk_id = f.chunk_id
             WHERE c.chunk_id IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("fts rowid alignment");
    assert_eq!(
        misaligned_fts, 0,
        "every chunks_fts row must sit on its chunk's rowid"
    );

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

    // The reference tables were not checked here, which is why a scoped rebuild
    // could leave `chunk_parent_refs` and `reply_edges` pointing at chunk ids it
    // had just deleted while the whole suite stayed green.
    for (label, sql) in [
        (
            "orphan parent_chunk_children -> chunks",
            "SELECT COUNT(*) FROM parent_chunk_children pcc
             LEFT JOIN chunks c ON c.chunk_id = pcc.chunk_id
             WHERE c.chunk_id IS NULL",
        ),
        (
            "dangling chunk_parent_refs.child_chunk_id",
            "SELECT COUNT(*) FROM chunk_parent_refs r
             LEFT JOIN chunks c ON c.chunk_id = r.child_chunk_id
             WHERE c.chunk_id IS NULL",
        ),
        (
            "dangling chunk_parent_refs.parent_chunk_id",
            "SELECT COUNT(*) FROM chunk_parent_refs r
             LEFT JOIN chunks c ON c.chunk_id = r.parent_chunk_id
             WHERE c.chunk_id IS NULL",
        ),
        (
            "dangling reply_edges.child_chunk_id",
            "SELECT COUNT(*) FROM reply_edges e
             LEFT JOIN chunks c ON c.chunk_id = e.child_chunk_id
             WHERE e.child_chunk_id IS NOT NULL AND c.chunk_id IS NULL",
        ),
        (
            "dangling reply_edges.parent_chunk_id",
            "SELECT COUNT(*) FROM reply_edges e
             LEFT JOIN chunks c ON c.chunk_id = e.parent_chunk_id
             WHERE e.parent_chunk_id IS NOT NULL AND c.chunk_id IS NULL",
        ),
    ] {
        let count: i64 = conn
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|err| panic!("{label}: {err}"));
        assert_eq!(count, 0, "{label}");
    }
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
    rebuild_chunks_for_chats(&archive, settings(), &[whole_chat(&chats[0])])
        .expect("scoped rebuild");
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
    assert_eq!(
        window_count, 2,
        "fixture must form exactly two parent windows"
    );
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
        .query_row(
            "SELECT COUNT(*) FROM parent_chunks WHERE chat_id = 'C'",
            [],
            |row| row.get(0),
        )
        .expect("count windows");
    assert!(
        window_count >= 3,
        "fixture must char-split the second burst into multiple windows, got {window_count}"
    );

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    let seed = staged.sync_messages(&messages[..last]).expect("sync seed");
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
    assert_eq!(
        staged_report.updated_messages, 1,
        "nickname change must count as updated"
    );
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
        .tail_rebuild_start("G", &staged_mid.touched_chats[0].earliest_changed_timestamp)
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
        .query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |row| {
            row.get(0)
        })
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
    rebuild_chunks_for_chats(&large, settings(), &[whole_chat("L")])
        .expect("seed whole-chat rebuild");
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
        .query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |row| {
            row.get(0)
        })
        .expect("count parent refs");
    assert!(
        parent_refs > 0,
        "large fixture must keep non-empty chunk_parent_refs"
    );

    // Residual decomposition: full-archive ref rebuild alone (the pre-scope baseline). Scoped
    // sync no longer pays this; it rebuilds refs only for the touched chats.
    let ref_started = std::time::Instant::now();
    large
        .rebuild_reply_and_parent_refs()
        .expect("ref pass only");
    let ref_only_ms = ref_started.elapsed().as_millis();

    // Cost claim: scoped one-message append is far cheaper than whole-chat on the same room.
    // After chunk indexes + scoped refs, residual should track the touched tail rather than
    // archive size; we still only require scoped ≪ whole-chat here (step-4 measures the rest).
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

/// The `EXPLAIN QUERY PLAN` step for one statement, joined into one line.
///
/// Binds the two placeholders the chunk-tail statements use (`chat_id`, optional floor). Scoped
/// ref statements take only `chat_id`; the second bind is ignored when unused.
fn query_plan(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare explain");
    // Placeholders are bound because `EXPLAIN QUERY PLAN` still requires a complete parameter
    // set; the values themselves do not steer the plan.
    let rows: Vec<String> = stmt
        .query_map(params!["chat", Option::<&str>::None], |row| {
            row.get::<_, String>(3)
        })
        .expect("explain")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("explain rows");
    rows.join(" | ")
}

/// Like [`query_plan`] but for the single-`?1` scoped ref statements.
fn query_plan_chat(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare explain");
    let rows: Vec<String> = stmt
        .query_map(params!["chat"], |row| row.get::<_, String>(3))
        .expect("explain")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("explain rows");
    rows.join(" | ")
}

/// Every statement a scoped rebuild issues against the chunk tables, taken from the source the
/// rebuild itself runs — not retyped here, or a rewrite could reintroduce a whole-table scan with
/// the test still passing against its own stale copy.
fn scoped_chunk_statements() -> Vec<&'static str> {
    std::iter::once(TAIL_REBUILD_START_QUERY)
        .chain(DELETE_CHAT_CHUNKS_STATEMENTS)
        .collect()
}

/// Tables whose whole-table scan is what the chunk indexes exist to remove.
///
/// `chunks_fts` is deliberately absent, but no longer because nothing can be done about it:
/// SQLite prints every fts5 step as `SCAN ... VIRTUAL TABLE INDEX`, even a single-row seek, so a
/// name check cannot tell the two apart. What the fts delete costs is asserted instead by
/// [`a_scoped_rebuild_seeks_fts_rows_by_rowid_instead_of_walking_the_whole_table`], which reads
/// the pushed-down constraint out of the plan.
const MUST_NOT_SCAN: &[&str] = &["chunks", "parent_chunks", "chunk_parent_refs"];

/// The `idxStr` fts5 reported for the `chunks_fts` step of `plan`, if the plan has one.
///
/// SQLite renders an fts5 step as `SCAN chunks_fts VIRTUAL TABLE INDEX <n>:<idxStr>`, and
/// `idxStr` is what fts5's `xBestIndex` built out of the constraints it accepted: `=` for a
/// rowid equality, `<` / `>` for a rowid range, `M<n>` for a MATCH. An **empty** `idxStr` means
/// fts5 accepted nothing and will hand every row of the table back for SQLite to filter — which
/// is the archive-size cost, spelled out in the plan rather than inferred from a stopwatch.
fn fts_index_str(plan: &str) -> Option<String> {
    const MARKER: &str = "chunks_fts VIRTUAL TABLE INDEX ";
    plan.split('|')
        .map(str::trim)
        .find(|step| step.contains(MARKER))
        .and_then(|step| step.split(MARKER).nth(1))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, idx_str)| idx_str.trim().to_string())
}

#[test]
fn a_scoped_rebuild_seeks_fts_rows_by_rowid_instead_of_walking_the_whole_table() {
    // `chunk_id` is an UNINDEXED fts5 column, so addressing the tail's fts rows by it pushes no
    // constraint into the virtual table and SQLite filters the entire archive itself — the single
    // largest term left in a scoped rebuild. `rowid` is the docid, which fts5 does accept, so the
    // delete becomes one seek per removed row and costs nothing per untouched row.
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");
    let conn = archive.connection();

    let fts_delete = DELETE_CHAT_CHUNKS_STATEMENTS
        .iter()
        .find(|sql| sql.contains("DELETE FROM chunks_fts"))
        .expect("the tail delete must still remove fts rows");
    let plan = query_plan(conn, fts_delete);
    let idx_str = fts_index_str(&plan).expect("the plan must have a chunks_fts step");
    assert!(
        idx_str.contains('='),
        "the fts tail delete pushes no rowid constraint into fts5, so it walks the whole archive\
         \nstatement: {fts_delete}\nplan: {plan}"
    );

    // The same delete written against `chunk_id` — what this replaced. Pinning that it reports an
    // empty idxStr is what gives the assertion above teeth: it shows the check distinguishes the
    // two forms rather than passing on anything fts5 happens to print.
    let by_chunk_id = "DELETE FROM chunks_fts
         WHERE chunk_id IN (SELECT chunk_id FROM chunks
            WHERE chat_id = ?1 AND started_at >= COALESCE(?2, ''))";
    let old_plan = query_plan(conn, by_chunk_id);
    assert_eq!(
        fts_index_str(&old_plan).as_deref(),
        Some(""),
        "addressing fts rows by the UNINDEXED chunk_id column should push nothing down\nplan: {old_plan}"
    );
}

#[test]
fn bm25_search_after_a_scoped_tail_rebuild_returns_what_a_full_rebuild_would() {
    // The rowid delete is only correct because `chunks_fts.rowid` is the chunk's `chunks.rowid`,
    // and that is the same correspondence `bm25_search` joins on. Running the real search over an
    // archive whose tail was replaced in place exercises both halves: a delete that removed the
    // wrong rows would surface here as a stale hit, a missing hit, or a wrong chunk_id.
    let base = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    // One sender, no gaps: every wave merges into the chat's last chunk, so the scoped rebuild
    // must delete the previous chunk's fts row and write a new one for the merged chunk.
    let mut messages = Vec::new();
    for (idx, word) in ["alfa", "bravo", "charlie", "delta"].iter().enumerate() {
        messages.push(raw(
            "S",
            &format!("s-{idx}"),
            "보글이",
            base + chrono::Duration::seconds(idx as i64 * 30),
            word,
            None,
        ));
    }
    // A second room so the search has something the tail rebuild never touches.
    messages.push(raw("T", "t-0", "부리", base, "echo", None));

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync");
    rebuild_chunks(&full).expect("full rebuild");

    let staged_dir = tempfile::tempdir().expect("tempdir");
    let staged = Archive::open(&staged_dir.path().join("archive.sqlite3")).expect("open archive");
    for wave in [&messages[..2], &messages[2..]] {
        let report = staged.sync_messages(wave).expect("sync wave");
        rebuild_chunks_for_chats(&staged, settings(), &report.touched_chats)
            .expect("scoped rebuild");
    }

    for term in ["alfa", "delta", "echo"] {
        let expected: Vec<String> = bm25_search(&full, term, 10)
            .expect("full search")
            .into_iter()
            .map(|hit| hit.chunk_id)
            .collect();
        assert_eq!(
            expected.len(),
            1,
            "the fixture should give exactly one hit for {term}"
        );
        let actual: Vec<String> = bm25_search(&staged, term, 10)
            .expect("staged search")
            .into_iter()
            .map(|hit| hit.chunk_id)
            .collect();
        assert_eq!(
            actual, expected,
            "bm25 search for {term} diverged after a scoped tail rebuild"
        );
    }

    assert_eq!(
        snapshot(full.connection()),
        snapshot(staged.connection()),
        "scoped tail rebuild diverged from a full rebuild"
    );
    assert_archive_invariants(staged.connection(), messages.len() as i64);
}

/// The tables a plan reports a whole-table `SCAN` of.
///
/// Compared as whole names rather than by substring, or `SCAN chunks_fts` would read as a scan
/// of `chunks` and the assertion would fire on a plan that is exactly what we want.
fn scanned_tables(plan: &str) -> Vec<&str> {
    plan.split("SCAN ")
        .skip(1)
        .filter_map(|rest| rest.split([' ', '|']).next())
        .collect()
}

/// The indexes whose whole point is to carry the `started_at` floor as well as the chat.
const TIME_ORDERED_INDEXES: &[&str] =
    &["idx_chunks_chat_started", "idx_parent_chunks_chat_started"];

/// Whether every plan step that reaches for a time-ordered index also bounds `started_at`.
///
/// Using the index on `chat_id` alone is not enough: that still walks every chunk the room has
/// ever had, which is the room-size term the tail scope exists to remove. SQLite reports the
/// bound it actually took in the parenthesised constraint list, so the plan is where the
/// difference shows — a `SCAN` check alone passes either way.
fn steps_missing_the_started_at_bound(plan: &str) -> Vec<&str> {
    plan.split('|')
        .map(str::trim)
        .filter(|step| TIME_ORDERED_INDEXES.iter().any(|idx| step.contains(idx)))
        .filter(|step| !step.contains("started_at"))
        .collect()
}

fn assert_no_chunk_table_scans(conn: &Connection, context: &str) {
    for sql in scoped_chunk_statements() {
        let plan = query_plan(conn, sql);
        let scanned = scanned_tables(&plan);
        for table in MUST_NOT_SCAN {
            assert!(
                !scanned.contains(table),
                "{context}: a scoped-rebuild statement scans {table} instead of using an index\
                 \nstatement: {sql}\nplan: {plan}"
            );
        }
        let unbounded = steps_missing_the_started_at_bound(&plan);
        assert!(
            unbounded.is_empty(),
            "{context}: a scoped-rebuild statement uses a time-ordered index on chat_id alone, \
             so it still walks the whole room: {unbounded:?}\nstatement: {sql}\nplan: {plan}"
        );
    }
}

#[test]
fn a_scoped_rebuild_addresses_the_chunk_tables_through_indexes_not_scans() {
    // Without these indexes every statement above walks the whole table, so touching one room
    // costs a pass over the entire archive. The plans, not a stopwatch, are what pin that: a
    // timing on a fixture this size would prove nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");

    assert_no_chunk_table_scans(archive.connection(), "fresh archive");
}

#[test]
fn an_archive_built_before_the_indexes_existed_picks_them_up_when_it_is_reopened() {
    // The indexes ship as `CREATE INDEX IF NOT EXISTS` in `migrate`, which is the whole migration
    // story for them. This pins that an archive that predates them is not left behind: dropping
    // them models that archive, and reopening must restore both the indexes and the plans.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("archive.sqlite3");
    let archive = Archive::open(&path).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");
    let before = snapshot(archive.connection());

    for index in [
        "idx_chunks_chat_started",
        "idx_parent_chunks_chat_started",
        "idx_chunk_parent_refs_parent",
        "idx_messages_chat_timestamp",
        "idx_chunk_messages_message",
    ] {
        archive
            .connection()
            .execute_batch(&format!("DROP INDEX {index};"))
            .expect("drop index");
    }
    drop(archive);

    let reopened = Archive::open(&path).expect("reopen archive");
    assert_no_chunk_table_scans(
        reopened.connection(),
        "archive reopened after the indexes existed",
    );
    assert_no_ref_table_scans(
        reopened.connection(),
        "archive reopened after the indexes existed",
    );
    assert_eq!(
        before,
        snapshot(reopened.connection()),
        "adding an index changed the archive's contents"
    );
}

/// Tables a scoped ref rebuild must not whole-scan. `chunks_fts` is irrelevant here; the four
/// statements only touch `reply_edges`, `messages`, `chunk_messages`, and `chunk_parent_refs`.
const REF_MUST_NOT_SCAN: &[&str] = &[
    "reply_edges",
    "messages",
    "chunk_messages",
    "chunk_parent_refs",
];

fn assert_no_ref_table_scans(conn: &Connection, context: &str) {
    for sql in SCOPED_REF_REBUILD_STATEMENTS {
        let plan = query_plan_chat(conn, sql);
        let scanned = scanned_tables(&plan);
        for table in REF_MUST_NOT_SCAN {
            assert!(
                !scanned.contains(table),
                "{context}: a scoped-ref statement scans {table} instead of using an index\
                 \nstatement: {sql}\nplan: {plan}"
            );
        }
        // Aliases (`SCAN child USING COVERING INDEX ...`) do not match the bare table names
        // above, but are still whole-table walks. With the chat_id / message_id indexes every
        // step of these four statements is a SEARCH; any SCAN means the scope is leaking.
        let scan_steps: Vec<&str> = plan
            .split('|')
            .map(str::trim)
            .filter(|step| step.starts_with("SCAN "))
            .collect();
        assert!(
            scan_steps.is_empty(),
            "{context}: a scoped-ref statement still has a SCAN step (archive-size leak)\
             \nstatement: {sql}\nplan: {plan}\nscan steps: {scan_steps:?}"
        );
        // A leading `LIKE 'chat-%'` on child_message_id is the trap the plan called out: it
        // cannot use the PK and walks every edge. The live statements must not contain it.
        assert!(
            !sql.to_ascii_lowercase().contains(" like "),
            "{context}: scoped-ref statement uses LIKE (not sargable on reply_edges PK):\n{sql}"
        );
    }
}

#[test]
fn a_scoped_ref_rebuild_addresses_reply_tables_through_indexes_not_scans() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");

    assert_no_ref_table_scans(archive.connection(), "fresh archive");
}

/// The parenthesised constraint list SQLite reports for the `SEARCH messages` step of `plan`, or
/// `""` if the plan has no such step. Taking the *last* `(` group skips the index name (which
/// itself contains `timestamp`), so a check on this text sees only the bounds the index actually
/// carried — `chat_id=?` alone versus `chat_id=? AND timestamp>?`.
fn messages_search_bounds(plan: &str) -> String {
    plan.split('|')
        .map(str::trim)
        .find(|step| step.starts_with("SEARCH messages"))
        .and_then(|step| step.rsplit_once('('))
        .map(|(_, bounds)| bounds.trim_end_matches(')').to_string())
        .unwrap_or_default()
}

/// `raw_messages_for_chat_since` must reach one chat's tail through the index, never by walking
/// `messages`. The raw `SCAN` was already off once `idx_messages_chat_timestamp` existed, but a
/// bare `(chat_id)` index would still sort for the `ORDER BY` and re-check the floor row by row;
/// this pins the whole win — no scan, no sort, and the `timestamp` floor pushed into the index —
/// and owns it independently of the scoped ref pass that first introduced a messages index.
#[test]
fn raw_messages_for_chat_since_seeks_one_chat_tail_without_scanning_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");
    let conn = archive.connection();

    let plan = query_plan(conn, RAW_MESSAGES_FOR_CHAT_SINCE_QUERY);
    assert!(
        !scanned_tables(&plan).contains(&"messages"),
        "raw_messages_for_chat_since scans messages instead of seeking one chat\nplan: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "raw_messages_for_chat_since sorts for its ORDER BY instead of reading it from the index\nplan: {plan}"
    );
    assert!(
        messages_search_bounds(&plan).contains("timestamp"),
        "the messages search does not push the timestamp floor into the index, so a scoped read \
         still re-checks the whole chat\nplan: {plan}"
    );

    // Teeth (1): the `COALESCE(?2, '')` floor is what makes the range sargable. The `?2 IS NULL OR
    // timestamp >= ?2` form this replaced selects the same rows but pushes only `chat_id=?`, so its
    // bounds carry no `timestamp` — showing the assertion above distinguishes the two forms rather
    // than passing on the index name.
    let or_form = "SELECT account_hash, chat_id, chat_name, chat_type, message_id,
            sender_nickname, timestamp, text, message_type
     FROM messages
     WHERE chat_id = ?1 AND (?2 IS NULL OR timestamp >= ?2)
     ORDER BY timestamp, message_id";
    let or_plan = query_plan(conn, or_form);
    assert!(
        !messages_search_bounds(&or_plan).contains("timestamp"),
        "the non-sargable OR form should push only chat_id, not the floor\nplan: {or_plan}"
    );

    // Teeth (2): dropping the index is the only thing left that reaches `messages` by `chat_id`, so
    // the live query falls back to a whole-table `SCAN`. This is the regression the item owns: if a
    // later change removes `idx_messages_chat_timestamp`, this fails instead of silently scanning.
    conn.execute_batch("DROP INDEX idx_messages_chat_timestamp;")
        .expect("drop index");
    let unindexed = query_plan(conn, RAW_MESSAGES_FOR_CHAT_SINCE_QUERY);
    assert!(
        scanned_tables(&unindexed).contains(&"messages"),
        "without idx_messages_chat_timestamp the query should scan messages — the index it drops \
         is not load-bearing for this read\nplan: {unindexed}"
    );
}

/// The `COALESCE(?2, '')` floor is only an optimisation if it selects exactly the rows the
/// `?2 IS NULL OR timestamp >= ?2` form did. It does because every stored `timestamp` is text and
/// therefore `>= ''`, so a NULL floor still admits the whole chat; this pins that on real data for
/// a NULL floor, a floor that starts mid-chat, and one past the end.
#[test]
fn the_sargable_floor_selects_the_same_rows_as_the_or_form() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");
    let conn = archive.connection();

    let chat_id: String = conn
        .query_row("SELECT chat_id FROM messages LIMIT 1", [], |row| row.get(0))
        .expect("a chat id");
    let timestamps: Vec<String> = conn
        .prepare("SELECT timestamp FROM messages WHERE chat_id = ?1 ORDER BY timestamp")
        .expect("prepare")
        .query_map(params![chat_id], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("timestamps");
    assert!(
        timestamps.len() >= 2,
        "fixture chat needs at least two messages"
    );

    let or_form = "SELECT account_hash, chat_id, chat_name, chat_type, message_id,
            sender_nickname, timestamp, text, message_type
     FROM messages
     WHERE chat_id = ?1 AND (?2 IS NULL OR timestamp >= ?2)
     ORDER BY timestamp, message_id";
    // A NULL floor, a floor at the second message (drops the first), and one past every row.
    for since in [
        None,
        Some(timestamps[1].clone()),
        Some("9999-12-31T23:59:59Z".to_string()),
    ] {
        let rewritten: Vec<String> = archive
            .raw_messages_for_chat_since(&chat_id, since.as_deref())
            .expect("rewritten read")
            .into_iter()
            .map(|m| m.message_id)
            .collect();
        let expected: Vec<String> = conn
            .prepare(or_form)
            .expect("prepare or-form")
            .query_map(params![chat_id, since], |row| row.get::<_, String>(4))
            .expect("query or-form")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("or-form rows");
        assert_eq!(
            rewritten, expected,
            "the sargable floor diverged from the OR form for since={since:?}"
        );
    }
}

/// Two chats with mint-format message ids, cross-chunk replies in each, and a third untouched
/// chat. Pins: (1) per-chat ref rebuild equals the archive-wide one row-for-row, (2) every edge's
/// endpoints share a chat_id prefix, (3) rebuilding only one chat leaves the other's edges alone.
#[test]
fn a_per_chat_ref_rebuild_matches_a_full_ref_rebuild_and_edges_stay_intra_chat() {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("base timestamp")
        .with_timezone(&chrono::Utc);
    // Mint format `{chat_id}-{log_id}` so the prefix invariant is directly assertable. Gaps keep
    // each sender turn as its own chunk so cross-chunk replies populate chunk_parent_refs.
    let mut messages = Vec::new();
    for (chat, nick_a, nick_b) in [("roomA", "보글이", "부리"), ("roomB", "하울", "새미")]
    {
        for i in 0..4 {
            let nick = if i % 2 == 0 { nick_a } else { nick_b };
            let log = i + 1;
            let reply = if i == 3 {
                Some(format!("{chat}-1"))
            } else {
                None
            };
            messages.push(raw(
                chat,
                &format!("{chat}-{log}"),
                nick,
                base + chrono::Duration::seconds(i as i64 * 700),
                &format!("{chat} msg {log}"),
                reply.as_deref(),
            ));
        }
    }
    // Untouched third room: must keep its edges when only roomA is ref-rebuilt.
    messages.push(raw("roomC", "roomC-1", "민지", base, "조용한 방", None));
    messages.push(raw(
        "roomC",
        "roomC-2",
        "준호",
        base + chrono::Duration::seconds(60),
        "답",
        Some("roomC-1"),
    ));

    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = Archive::open(&full_dir.path().join("archive.sqlite3")).expect("open archive");
    full.sync_messages(&messages).expect("sync full");
    rebuild_chunks(&full).expect("full rebuild");

    let reply_edges: i64 = full
        .connection()
        .query_row("SELECT COUNT(*) FROM reply_edges", [], |row| row.get(0))
        .expect("count reply_edges");
    let parent_refs: i64 = full
        .connection()
        .query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |row| {
            row.get(0)
        })
        .expect("count parent refs");
    assert!(
        reply_edges >= 3,
        "fixture must exercise non-empty reply_edges, got {reply_edges}"
    );
    assert!(
        parent_refs >= 2,
        "fixture must exercise non-empty chunk_parent_refs, got {parent_refs}"
    );

    // Every edge's endpoints share a chat prefix (the mint invariant the scope rests on).
    {
        let conn = full.connection();
        let mut stmt = conn
            .prepare(
                "SELECT child_message_id, parent_message_id FROM reply_edges
                 ORDER BY child_message_id",
            )
            .expect("prepare edges");
        let edges: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("edges")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("edge rows");
        for (child, parent) in &edges {
            let child_chat = child.split('-').next().expect("child chat prefix");
            let parent_chat = parent.split('-').next().expect("parent chat prefix");
            assert_eq!(
                child_chat, parent_chat,
                "reply edge crossed chats: {child} -> {parent}"
            );
            assert!(
                child.starts_with(&format!("{child_chat}-"))
                    && parent.starts_with(&format!("{parent_chat}-")),
                "edge ids must be mint-format {{chat}}-{{log}}: {child} -> {parent}"
            );
        }
        let mut stmt = conn
            .prepare(
                "SELECT c.chat_id, p.chat_id FROM chunk_parent_refs r
                 JOIN chunks c ON c.chunk_id = r.child_chunk_id
                 JOIN chunks p ON p.chunk_id = r.parent_chunk_id",
            )
            .expect("prepare parent refs");
        let refs: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("refs")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("ref rows");
        for (child_chat, parent_chat) in &refs {
            assert_eq!(
                child_chat, parent_chat,
                "chunk_parent_ref crossed chats: {child_chat} -> {parent_chat}"
            );
        }
    }

    // Seed a second archive the same way, wipe its ref tables, then rebuild refs per chat only.
    let scoped_dir = tempfile::tempdir().expect("tempdir");
    let scoped = Archive::open(&scoped_dir.path().join("archive.sqlite3")).expect("open archive");
    scoped.sync_messages(&messages).expect("sync scoped");
    rebuild_chunks(&scoped).expect("seed full so chunks match");
    scoped
        .connection()
        .execute_batch("DELETE FROM reply_edges; DELETE FROM chunk_parent_refs;")
        .expect("wipe refs");
    scoped
        .rebuild_reply_and_parent_refs_for_chats(&["roomA", "roomB", "roomC"])
        .expect("scoped ref union");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(scoped.connection()),
        "union of per-chat ref rebuilds diverged from the archive-wide ref rebuild"
    );

    // Rebuild only roomA's refs after corrupting them: roomB/roomC must stay identical to full.
    scoped
        .connection()
        .execute_batch(
            "DELETE FROM reply_edges WHERE child_message_id IN (
                SELECT message_id FROM messages WHERE chat_id = 'roomA'
             );
             DELETE FROM chunk_parent_refs WHERE child_chunk_id IN (
                SELECT chunk_id FROM chunks WHERE chat_id = 'roomA'
             ) OR parent_chunk_id IN (
                SELECT chunk_id FROM chunks WHERE chat_id = 'roomA'
             );",
        )
        .expect("corrupt roomA refs");
    let edges_before_b: i64 = scoped
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM reply_edges WHERE child_message_id LIKE 'roomB-%'",
            [],
            |row| row.get(0),
        )
        .expect("count B");
    assert!(
        edges_before_b > 0,
        "roomB edges must survive roomA corruption"
    );
    scoped
        .rebuild_reply_and_parent_refs_for_chats(&["roomA"])
        .expect("rebuild roomA only");

    assert_eq!(
        snapshot(full.connection()),
        snapshot(scoped.connection()),
        "rebuilding refs for one chat diverged from the full archive"
    );
    assert_archive_invariants(scoped.connection(), messages.len() as i64);

    // End-to-end: a one-chat append through rebuild_chunks_for_chats must match a full rebuild.
    let wave = raw(
        "roomA",
        "roomA-5",
        "보글이",
        base + chrono::Duration::seconds(4 * 700),
        "꼬리",
        Some("roomA-1"),
    );
    let mut all = messages.clone();
    all.push(wave.clone());
    full.sync_messages(std::slice::from_ref(&wave))
        .expect("sync wave full");
    rebuild_chunks(&full).expect("full after wave");
    let report = scoped
        .sync_messages(std::slice::from_ref(&wave))
        .expect("sync wave scoped");
    rebuild_chunks_for_chats(&scoped, settings(), &report.touched_chats).expect("scoped wave");
    assert_eq!(
        snapshot(full.connection()),
        snapshot(scoped.connection()),
        "scoped chat rebuild (with scoped ref pass) diverged from full rebuild after append"
    );
}

#[test]
fn a_null_floor_deletes_the_same_chunk_rows_the_or_form_did() {
    // `delete_chat_chunks` states the floor as `started_at >= COALESCE(?2, '')` so SQLite can
    // drive it from the index; the `?2 IS NULL OR started_at >= ?2` form it replaced could not be.
    // The rewrite is only safe if the two select identical rows, which holds because `started_at`
    // is non-empty RFC3339 text. This pins that on real rows rather than on the argument.
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");
    archive.sync_messages(&fixture_messages()).expect("sync");
    rebuild_chunks(&archive).expect("rebuild");

    let conn = archive.connection();
    let chats: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT chat_id FROM chunks ORDER BY chat_id")
            .expect("prepare chats");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("chats")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("chat rows");
        rows
    };
    assert!(!chats.is_empty(), "fixture must produce chunks");

    // Every floor the scoped path can pass: the whole-chat `None`, and each stored boundary.
    let mut floors: Vec<Option<String>> = vec![None];
    {
        let mut stmt = conn
            .prepare("SELECT DISTINCT started_at FROM chunks ORDER BY started_at")
            .expect("prepare floors");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("floors")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("floor rows");
        floors.extend(rows.into_iter().map(Some));
    }

    for (table, column, floor_column) in [
        ("chunks", "chunk_id", "started_at"),
        ("parent_chunks", "parent_id", "started_at"),
    ] {
        for chat_id in &chats {
            for floor in &floors {
                let select = |predicate: &str| -> Vec<String> {
                    let sql = format!(
                        "SELECT {column} FROM {table}
                         WHERE chat_id = ?1 AND {predicate} ORDER BY {column}"
                    );
                    let mut stmt = conn.prepare(&sql).expect("prepare select");
                    stmt.query_map(params![chat_id, floor.as_deref()], |row| {
                        row.get::<_, String>(0)
                    })
                    .expect("select")
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .expect("select rows")
                };
                assert_eq!(
                    select(&format!("(?2 IS NULL OR {floor_column} >= ?2)")),
                    select(&format!("{floor_column} >= COALESCE(?2, '')")),
                    "{table}: the sargable floor selected different rows than the OR form \
                     (chat_id={chat_id}, floor={floor:?})"
                );
            }
        }
    }
}

/// A wall of identical long messages must not collide two parent windows onto
/// one `parent_id`.
///
/// Nine consecutive 999-character messages from the same sender fill one chunk
/// whose parent line is cut into segments with identical text, identical first
/// child and identical last child. Before the segment ordinal became part of the
/// hash they produced the same id, the insert violated the primary key, and
/// because a sync is one transaction it rolled the whole thing back — leaving an
/// archive that failed on the same messages on every later run. Pasting a log or
/// a long article into a chat is enough to trigger it.
#[test]
fn repeated_long_messages_do_not_collide_parent_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = Archive::open(&dir.path().join("archive.sqlite3")).expect("open archive");

    let body = "가".repeat(999);
    let base = chrono::Utc::now();
    let messages: Vec<RawMessage> = (0..9)
        .map(|i| {
            raw(
                "chat-repeat",
                &format!("m{i}"),
                "Alice",
                base + chrono::Duration::seconds(i),
                &body,
                None,
            )
        })
        .collect();

    archive.sync_messages(&messages).expect("sync");
    rebuild_chunks_with_settings(&archive, ChunkSettings::default())
        .expect("rebuild must not hit a UNIQUE violation on parent_chunks");

    let conn = Connection::open(dir.path().join("archive.sqlite3")).expect("open for asserts");
    assert_archive_invariants(&conn, messages.len() as i64);
}
