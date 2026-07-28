//! Final residual measurement for plan `katok/sync-residual-scope` item 6.
//!
//! Prints counts, wall times, invariant status, and 7-table equality only —
//! never message text or chat names. Chat ids are printed in full because they
//! are opaque numeric identifiers needed to reproduce the run.
//!
//! Usage:
//!   measure_residual_scope <work_dir> <source_archive.sqlite3> <chat_id>
//!
//! Environment:
//!   MEASURE_RUNS=3          warm runs per stage (default 3)
//!   MEASURE_SKIP_EQUALITY=1 skip the full-vs-scoped 7-table diff (slow on huge archives)
//!   MEASURE_SKIP_FULL_REBUILD=1  skip full rebuild half of equality
use katok::archive::Archive;
use katok::chunking::{rebuild_chunks_for_chats, rebuild_chunks_with_settings, ChunkSettings};
use katok::types::TouchedChat;
use rusqlite::Connection;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let work_dir = PathBuf::from(args.next().expect(
        "usage: measure_residual_scope <work_dir> <source_archive.sqlite3> <chat_id>",
    ));
    let source = PathBuf::from(args.next().expect("need source archive path"));
    let chat_id = args.next().expect("need chat_id");
    let runs: u32 = env::var("MEASURE_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let skip_equality = env::var("MEASURE_SKIP_EQUALITY").ok().as_deref() == Some("1");

    fs::create_dir_all(&work_dir)?;
    let full_path = work_dir.join("full.sqlite3");
    let reduced_path = work_dir.join("reduced.sqlite3");
    let scoped_eq_path = work_dir.join("eq_scoped.sqlite3");
    let full_eq_path = work_dir.join("eq_full.sqlite3");

    println!("phase=copy source_bytes={}", fs::metadata(&source)?.len());
    let _ = io::stdout().flush();
    fs::copy(&source, &full_path)?;
    println!("phase=copied full_path={}", full_path.display());
    let _ = io::stdout().flush();

    // Open once to run migrate (indexes) and report the inventory that the residual plan added.
    let t_open = Instant::now();
    let full = Archive::open(&full_path)?;
    println!(
        "phase=migrate_open ms={} indexes={}",
        t_open.elapsed().as_millis(),
        list_plan_indexes(full.connection())
    );
    let settings = chunk_settings(&full)?;
    let full_stats = archive_stats(full.connection(), &chat_id)?;
    println!(
        "point=full msgs={} chunks={} chat_msgs={} chat_chunks={} reply_edges={} parent_refs={} file_bytes={}",
        full_stats.msgs,
        full_stats.chunks,
        full_stats.chat_msgs,
        full_stats.chat_chunks,
        full_stats.reply_edges,
        full_stats.parent_refs,
        fs::metadata(&full_path)?.len()
    );
    print_invariants(full.connection(), full_stats.msgs, "full_pre")?;
    drop(full);

    // Reduced archive: keep only the target chat (holds touched scope constant, shrinks archive).
    // On this production sample the largest room is ~44% of messages — that is the tightest
    // single-chat shrink that still preserves the same 1-message large-room operation. Call it
    // the reduced point; ratio is reported so proportionality can be judged against size, not a
    // fixed 1/4 label.
    fs::copy(&full_path, &reduced_path)?;
    {
        let reduced = Archive::open(&reduced_path)?;
        let t = Instant::now();
        strip_to_one_chat(reduced.connection(), &chat_id)?;
        // Chunk tables still hold other chats' rows after message delete — drop them too so
        // residual that walks chunk tables sees the reduced size.
        strip_chunk_tables_to_one_chat(reduced.connection(), &chat_id)?;
        println!(
            "phase=reduce ms={} kept_chat_prefix={}",
            t.elapsed().as_millis(),
            chat_id.chars().take(12).collect::<String>()
        );
        let st = archive_stats(reduced.connection(), &chat_id)?;
        println!(
            "point=reduced msgs={} chunks={} chat_msgs={} chat_chunks={} reply_edges={} parent_refs={} file_bytes_before_vacuum={}",
            st.msgs,
            st.chunks,
            st.chat_msgs,
            st.chat_chunks,
            st.reply_edges,
            st.parent_refs,
            fs::metadata(&reduced_path)?.len()
        );
        // Rebuild chunk artifacts for the kept chat so the reduced archive is self-consistent
        // (message strip alone leaves chunk_messages pointing at deleted chats already removed).
        // The kept chat's chunk rows were left in place above; only other chats were deleted.
        print_invariants(reduced.connection(), st.msgs, "reduced_pre")?;
        drop(reduced);
    }
    // VACUUM so file_bytes tracks live content, not free pages.
    {
        let t = Instant::now();
        let conn = Connection::open(&reduced_path)?;
        conn.execute_batch("VACUUM")?;
        println!(
            "phase=vacuum_reduced ms={} file_bytes={}",
            t.elapsed().as_millis(),
            fs::metadata(&reduced_path)?.len()
        );
    }

    measure_point("full", &full_path, &chat_id, settings, runs)?;
    measure_point("reduced", &reduced_path, &chat_id, settings, runs)?;

    if !skip_equality {
        println!("phase=equality_start");
        let _ = io::stdout().flush();
        fs::copy(&full_path, &scoped_eq_path)?;
        fs::copy(&full_path, &full_eq_path)?;

        let scoped = Archive::open(&scoped_eq_path)?;
        let touched = one_new_message_touch(scoped.connection(), &chat_id)?;
        let t = Instant::now();
        let scoped_count = scoped.in_transaction(|| {
            rebuild_chunks_for_chats(&scoped, settings, &[touched])
        })?;
        println!(
            "equality scoped_rebuild ms={} archive_chunks={}",
            t.elapsed().as_millis(),
            scoped_count
        );
        print_invariants(scoped.connection(), full_stats.msgs, "eq_scoped")?;
        let scoped_snap = snapshot(scoped.connection());
        drop(scoped);

        let full_eq = Archive::open(&full_eq_path)?;
        let t = Instant::now();
        let full_count =
            full_eq.in_transaction(|| rebuild_chunks_with_settings(&full_eq, settings))?;
        println!(
            "equality full_rebuild ms={} archive_chunks={}",
            t.elapsed().as_millis(),
            full_count
        );
        print_invariants(full_eq.connection(), full_stats.msgs, "eq_full")?;
        let full_snap = snapshot(full_eq.connection());
        drop(full_eq);

        if scoped_snap == full_snap {
            println!(
                "equality result=match rows={} (7-table full-column ordered snapshot)",
                scoped_snap.len()
            );
        } else {
            let mut only_scoped = 0usize;
            let mut only_full = 0usize;
            // Cheap cardinality of the symmetric difference without printing cell text.
            let mut i = 0usize;
            let mut j = 0usize;
            while i < scoped_snap.len() && j < full_snap.len() {
                if scoped_snap[i] == full_snap[j] {
                    i += 1;
                    j += 1;
                } else if scoped_snap[i] < full_snap[j] {
                    only_scoped += 1;
                    i += 1;
                } else {
                    only_full += 1;
                    j += 1;
                }
            }
            only_scoped += scoped_snap.len().saturating_sub(i);
            only_full += full_snap.len().saturating_sub(j);
            println!(
                "equality result=DIFF scoped_rows={} full_rows={} only_scoped={} only_full={}",
                scoped_snap.len(),
                full_snap.len(),
                only_scoped,
                only_full
            );
            anyhow::bail!("scoped rebuild diverged from full rebuild");
        }
    } else {
        println!("phase=equality skipped");
    }

    println!("phase=done work_dir={}", work_dir.display());
    Ok(())
}

#[derive(Clone, Copy)]
struct Stats {
    msgs: i64,
    chunks: i64,
    chat_msgs: i64,
    chat_chunks: i64,
    reply_edges: i64,
    parent_refs: i64,
}

fn archive_stats(conn: &Connection, chat_id: &str) -> anyhow::Result<Stats> {
    let msgs: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let chat_msgs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE chat_id = ?1",
        [chat_id],
        |r| r.get(0),
    )?;
    let chat_chunks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE chat_id = ?1",
        [chat_id],
        |r| r.get(0),
    )?;
    let reply_edges: i64 = conn.query_row("SELECT COUNT(*) FROM reply_edges", [], |r| r.get(0))?;
    let parent_refs: i64 =
        conn.query_row("SELECT COUNT(*) FROM chunk_parent_refs", [], |r| r.get(0))?;
    Ok(Stats {
        msgs,
        chunks,
        chat_msgs,
        chat_chunks,
        reply_edges,
        parent_refs,
    })
}

fn list_plan_indexes(conn: &Connection) -> String {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='index' AND name LIKE 'idx_%'
             ORDER BY name",
        )
        .expect("prepare");
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .expect("map")
        .collect::<Result<_, _>>()
        .expect("collect");
    if names.is_empty() {
        "(none)".into()
    } else {
        names.join(",")
    }
}

fn chunk_settings(archive: &Archive) -> anyhow::Result<ChunkSettings> {
    Ok(match archive.stored_chunk_settings()? {
        Some((group, direct, _)) => ChunkSettings {
            group_gap_seconds: group,
            direct_gap_seconds: direct,
        },
        None => ChunkSettings::default(),
    })
}

fn one_new_message_touch(conn: &Connection, chat_id: &str) -> anyhow::Result<TouchedChat> {
    let (timestamp, message_id): (String, String) = conn.query_row(
        "SELECT timestamp, message_id FROM messages
         WHERE chat_id = ?1 ORDER BY timestamp DESC, message_id DESC LIMIT 1",
        [chat_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(TouchedChat {
        chat_id: chat_id.to_string(),
        earliest_changed_timestamp: timestamp,
        earliest_changed_message_id: message_id,
    })
}

fn measure_point(
    label: &str,
    path: &Path,
    chat_id: &str,
    settings: ChunkSettings,
    runs: u32,
) -> anyhow::Result<()> {
    let archive = Archive::open(path)?;
    let touched = one_new_message_touch(archive.connection(), chat_id)?;
    let chat_ids = [chat_id.to_string()];
    // Warm page cache.
    let _ = archive.raw_messages_for_chats(&chat_ids)?;

    for i in 1..=runs {
        let t = Instant::now();
        let from = archive.tail_rebuild_start(&touched.chat_id, &touched.earliest_changed_timestamp)?;
        let tail_start_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let rows = archive
            .raw_messages_for_chat_since(&touched.chat_id, from.as_deref())?
            .len();
        let read_ms = t.elapsed().as_millis();

        // Full rebuild_chunks_for_chats (current production path: tail + scoped refs).
        let t = Instant::now();
        let total_chunks = archive.in_transaction(|| {
            rebuild_chunks_for_chats(&archive, settings, std::slice::from_ref(&touched))
        })?;
        let total_ms = t.elapsed().as_millis();

        // Archive-wide ref path (pre-item-3 residual component). Contrast only.
        let t = Instant::now();
        archive.in_transaction(|| archive.rebuild_reply_and_parent_refs())?;
        let ref_full_ms = t.elapsed().as_millis();

        // Scoped ref path alone (post-item-3).
        let t = Instant::now();
        archive.in_transaction(|| {
            archive.rebuild_reply_and_parent_refs_for_chats(&[chat_id])
        })?;
        let ref_scoped_ms = t.elapsed().as_millis();

        println!(
            "run={i} point={label} archive_chunks={total_chunks} tail_rows={rows} \
             tail_start_ms={tail_start_ms} read_ms={read_ms} total_ms={total_ms} \
             ref_full_ms={ref_full_ms} ref_scoped_ms={ref_scoped_ms} \
             scoped_minus_ref_full_ms={}",
            total_ms.saturating_sub(ref_full_ms)
        );
        let _ = io::stdout().flush();
    }
    Ok(())
}

fn strip_to_one_chat(conn: &Connection, chat_id: &str) -> anyhow::Result<()> {
    conn.execute("DELETE FROM messages WHERE chat_id <> ?1", [chat_id])?;
    conn.execute("DELETE FROM chats WHERE chat_id <> ?1", [chat_id])?;
    // Leave sync_cursors alone — not part of residual path.
    Ok(())
}

fn strip_chunk_tables_to_one_chat(conn: &Connection, chat_id: &str) -> anyhow::Result<()> {
    // Order respects FKs-by-convention (no FK constraints, but logical dependents first).
    conn.execute(
        "DELETE FROM reply_edges
         WHERE child_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)
            OR parent_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)
            OR child_message_id IN (SELECT message_id FROM chunk_messages cm
                JOIN chunks c ON c.chunk_id = cm.chunk_id WHERE c.chat_id <> ?1)",
        [chat_id],
    )?;
    conn.execute(
        "DELETE FROM chunk_parent_refs
         WHERE child_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)
            OR parent_chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)",
        [chat_id],
    )?;
    conn.execute(
        "DELETE FROM parent_chunk_children
         WHERE parent_id IN (SELECT parent_id FROM parent_chunks WHERE chat_id <> ?1)
            OR chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)",
        [chat_id],
    )?;
    conn.execute(
        "DELETE FROM chunk_messages
         WHERE chunk_id IN (SELECT chunk_id FROM chunks WHERE chat_id <> ?1)",
        [chat_id],
    )?;
    conn.execute(
        "DELETE FROM chunks_fts
         WHERE rowid IN (SELECT rowid FROM chunks WHERE chat_id <> ?1)",
        [chat_id],
    )?;
    conn.execute("DELETE FROM parent_chunks WHERE chat_id <> ?1", [chat_id])?;
    conn.execute("DELETE FROM chunks WHERE chat_id <> ?1", [chat_id])?;
    Ok(())
}

fn print_invariants(conn: &Connection, expected_messages: i64, label: &str) -> anyhow::Result<()> {
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    let chunk_messages: i64 =
        conn.query_row("SELECT COUNT(*) FROM chunk_messages", [], |r| r.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let fts: i64 = conn.query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))?;
    let misaligned_fts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks_fts f
         LEFT JOIN chunks c ON c.rowid = f.rowid AND c.chunk_id = f.chunk_id
         WHERE c.chunk_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    let orphan_cm: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunk_messages cm
         LEFT JOIN chunks c ON c.chunk_id = cm.chunk_id
         WHERE c.chunk_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    let orphan_pc: i64 = conn.query_row(
        "SELECT COUNT(*) FROM parent_chunk_children pcc
         LEFT JOIN parent_chunks p ON p.parent_id = pcc.parent_id
         WHERE p.parent_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    let ok = integrity == "ok"
        && messages == expected_messages
        && chunk_messages == expected_messages
        && chunks == fts
        && misaligned_fts == 0
        && orphan_cm == 0
        && orphan_pc == 0;
    println!(
        "invariants label={label} ok={ok} integrity={integrity} messages={messages} \
         chunk_messages={chunk_messages} chunks={chunks} fts={fts} \
         misaligned_fts={misaligned_fts} orphan_cm={orphan_cm} orphan_pc={orphan_pc}"
    );
    if !ok {
        anyhow::bail!("invariants failed at {label}");
    }
    Ok(())
}

/// Every chunk artifact, in a stable order, as comparable text — mirrors
/// `tests/incremental_chunking.rs::snapshot` (7 tables).
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
