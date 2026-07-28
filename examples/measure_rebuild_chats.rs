//! Ceiling measurement for rebuild_chunks_for_chats (same path as sync timings_ms.rebuild_chunks).
//! Prints chat_id + wall times only — never message text or chat names.
use katok::archive::Archive;
use katok::chunking::{rebuild_chunks_for_chats, ChunkSettings};
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

    let mut times_ms = Vec::new();
    for i in 1..=runs {
        let started = Instant::now();
        // Match sync path: rebuild_chunks runs inside one archive transaction.
        let total_chunks = archive.in_transaction(|| {
            rebuild_chunks_for_chats(&archive, settings, &chat_ids)
        })?;
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
