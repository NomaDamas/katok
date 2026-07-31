//! Ceiling measurement for rebuild_chunks_for_chats (same path as sync timings_ms.rebuild_chunks).
//! Prints chat_id + wall times only — never message text or chat names.
//!
//! Simulates a single new message arriving in each named room: the earliest changed key is set to
//! that room's newest existing message, so the rebuild scopes to the tail past the last stable
//! window boundary — the shape a live sync produces. Set MEASURE_FULL=1 to force a whole-chat
//! rebuild instead (the pre-tail-scope behaviour) for a before/after comparison.
use katok::archive::Archive;
use katok::chunking::{rebuild_chunks_for_chats, ChunkSettings};
use katok::types::TouchedChat;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let archive_path = args
        .next()
        .expect("usage: measure_rebuild_chats <archive.sqlite3> <chat_id>...");
    let chat_ids: Vec<String> = args.collect();
    if chat_ids.is_empty() {
        anyhow::bail!("need at least one chat_id");
    }
    let runs: u32 = env::var("MEASURE_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let archive = Archive::open(Path::new(&archive_path))?;
    let settings = match archive.stored_chunk_settings()? {
        Some((group, direct, _)) => ChunkSettings {
            group_gap_seconds: group,
            direct_gap_seconds: direct,
        },
        None => ChunkSettings::default(),
    };

    let force_full = env::var("MEASURE_FULL").ok().as_deref() == Some("1");

    // Warm page cache by reading only (no rewrite).
    let t0 = Instant::now();
    let msgs = archive.raw_messages_for_chats(&chat_ids)?;
    println!(
        "warm_read chat_ids={} msg_rows={} ms={}",
        chat_ids.join(","),
        msgs.len(),
        t0.elapsed().as_millis()
    );
    let _ = io::stdout().flush();

    // Build the touched set. Each room's earliest change is its newest existing message, so the
    // rebuild scopes to the tail — the shape "one new message arrived" produces. MEASURE_FULL=1
    // sets an empty key, which forces a whole-chat rebuild for the before/after baseline.
    let touched: Vec<TouchedChat> = chat_ids
        .iter()
        .map(|chat_id| {
            let (timestamp, message_id) = if force_full {
                (String::new(), String::new())
            } else {
                archive.connection().query_row(
                    "SELECT timestamp, message_id FROM messages
                     WHERE chat_id = ?1 ORDER BY timestamp DESC, message_id DESC LIMIT 1",
                    [chat_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )?
            };
            Ok::<_, anyhow::Error>(TouchedChat {
                chat_id: chat_id.clone(),
                earliest_changed_timestamp: timestamp,
                earliest_changed_message_id: message_id,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let mut times_ms = Vec::new();
    for i in 1..=runs {
        let started = Instant::now();
        // Match sync path: rebuild_chunks runs inside one archive transaction.
        let total_chunks =
            archive.in_transaction(|| rebuild_chunks_for_chats(&archive, settings, &touched))?;
        let ms = started.elapsed().as_millis();
        times_ms.push(ms);
        println!(
            "run={} chat_ids={} archive_chunk_count={} rebuild_ms={}",
            i,
            chat_ids.join(","),
            total_chunks,
            ms
        );
        let _ = io::stdout().flush();
    }

    let min = *times_ms.iter().min().unwrap();
    let max = *times_ms.iter().max().unwrap();
    let mean = times_ms.iter().sum::<u128>() as f64 / times_ms.len() as f64;
    println!(
        "summary chat_ids={} runs={} min_ms={} max_ms={} mean_ms={:.1} spread_ms={}",
        chat_ids.join(","),
        runs,
        min,
        max,
        mean,
        max - min
    );
    Ok(())
}
