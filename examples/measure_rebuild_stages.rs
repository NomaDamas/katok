//! Stage breakdown of the same path `measure_rebuild_chats` times as one number.
//! Prints chat_id prefixes and wall times only — never message text or chat names.
//!
//! Splits `rebuild_chunks_for_chats` into the parts the `chunks` indexes touch and the part they
//! do not, so an index change can be attributed instead of inferred:
//!
//!   tail_start_ms  `tail_rebuild_start` per touched chat — the backward walk over `chunks`.
//!   read_ms        `raw_messages_for_chat_since` — the tail message read (indexed by the
//!                  `messages` primary key already, so it is the control).
//!   total_ms       the whole rebuild, matching `timings_ms.rebuild_chunks`.
//!   ref_only_ms    `rebuild_reply_and_parent_refs` on its own — archive-wide, index-independent.
//!   scoped_ms      total - ref_only, i.e. everything the tail scope and the indexes govern.
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
        .expect("usage: measure_rebuild_stages <archive.sqlite3> <chat_id>...");
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

    // Same touched shape as `measure_rebuild_chats`: one new message in each named room.
    let touched: Vec<TouchedChat> = chat_ids
        .iter()
        .map(|chat_id| {
            let (timestamp, message_id) = archive.connection().query_row(
                "SELECT timestamp, message_id FROM messages
                 WHERE chat_id = ?1 ORDER BY timestamp DESC, message_id DESC LIMIT 1",
                [chat_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            Ok::<_, anyhow::Error>(TouchedChat {
                chat_id: chat_id.clone(),
                earliest_changed_timestamp: timestamp,
                earliest_changed_message_id: message_id,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let label = chat_ids
        .iter()
        .map(|id| id.chars().take(12).collect::<String>())
        .collect::<Vec<_>>()
        .join(",");

    // Warm the page cache so run 1 is not measuring first-touch I/O.
    let _ = archive.raw_messages_for_chats(&chat_ids)?;

    for i in 1..=runs {
        let mut tail_start_ms = 0u128;
        let mut read_ms = 0u128;
        let mut rows = 0usize;
        for chat in &touched {
            let t = Instant::now();
            let from = archive.tail_rebuild_start(&chat.chat_id, &chat.earliest_changed_timestamp)?;
            tail_start_ms += t.elapsed().as_millis();
            let t = Instant::now();
            rows += archive
                .raw_messages_for_chat_since(&chat.chat_id, from.as_deref())?
                .len();
            read_ms += t.elapsed().as_millis();
        }

        let t = Instant::now();
        let total_chunks =
            archive.in_transaction(|| rebuild_chunks_for_chats(&archive, settings, &touched))?;
        let total_ms = t.elapsed().as_millis();

        let t = Instant::now();
        archive.in_transaction(|| archive.rebuild_reply_and_parent_refs())?;
        let ref_only_ms = t.elapsed().as_millis();

        println!(
            "run={i} chats={label} archive_chunks={total_chunks} tail_rows={rows} \
             tail_start_ms={tail_start_ms} read_ms={read_ms} total_ms={total_ms} \
             ref_only_ms={ref_only_ms} scoped_ms={}",
            total_ms.saturating_sub(ref_only_ms)
        );
        let _ = io::stdout().flush();
    }
    Ok(())
}
