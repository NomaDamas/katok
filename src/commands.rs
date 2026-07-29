use crate::cli::{Commands, PermissionsCommand, SearchCommand, SourceCommand};
use crate::commands::source_adapter::adapter_for_source;
use crate::support::{dependency_status, print_payload};
use anyhow::{Context, Result};
use katok::{
    archive::Archive,
    chunking::{
        rebuild_chunks_for_chats, rebuild_chunks_with_settings, ChunkSettings, CHUNKER_VERSION,
    },
    config::KatokConfig,
    search::{bm25_search_with_snippet, keyword_search_with_snippet},
    semantic::{semantic_search_live_with_config, semantic_search_with_snippet},
    transcript::export_transcript,
    types::SyncTimings,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod chunk_commands;
mod freshness;
mod index_commands;
mod media_commands;
mod permissions;
mod source_adapter;

pub(crate) fn run(
    command: Commands,
    config: KatokConfig,
    data_dir: PathBuf,
    archive_path: PathBuf,
    semantic_dir: PathBuf,
) -> Result<()> {
    match command {
        Commands::Doctor { macos_probe, json } => run_doctor(
            macos_probe,
            json,
            config,
            data_dir,
            archive_path,
            semantic_dir,
        ),
        Commands::Sync {
            source,
            path,
            json,
            touched,
            prune_preview,
            prune_deleted,
        } => {
            let source = source.unwrap_or_else(|| config.source_adapter.clone());
            run_sync(
                &source,
                path,
                json,
                touched,
                prune_preview,
                prune_deleted,
                &config,
                &archive_path,
                &data_dir,
            )
        }
        Commands::Index {
            full,
            dry_run,
            json,
        } => index_commands::run(
            full,
            dry_run,
            json,
            &config,
            &archive_path,
            &semantic_dir,
            &data_dir,
        ),
        Commands::Search { command } => run_search(command, &config, &archive_path, &semantic_dir),
        Commands::Chunk { command } => chunk_commands::run(command, &archive_path),
        Commands::Source { command } => run_source(command, &config, &data_dir),
        Commands::Media { command } => media_commands::run(command, &data_dir),
        Commands::Permissions { command } => run_permissions(command),
        Commands::Chunks { chat, json } => run_chunks(&chat, json, &archive_path),
        Commands::Transcript {
            chat,
            since,
            out,
            json,
        } => run_transcript(&chat, since.as_deref(), out, json, &archive_path, &data_dir),
        Commands::WipeIndex { yes, json } => run_wipe_index(yes, json, &semantic_dir),
        #[cfg(target_os = "macos")]
        Commands::Send {
            room,
            chat,
            text,
            image,
            list_windows,
            list_rooms,
            limit,
            dry_run,
            no_open,
            take_focus_now,
            focus_wait,
            json,
        } => run_send(
            room,
            chat,
            text,
            image,
            list_windows,
            list_rooms,
            limit,
            dry_run,
            no_open,
            take_focus_now,
            focus_wait,
            json,
            &archive_path,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(target_os = "macos")]
fn run_send(
    room: Option<String>,
    chat: Option<String>,
    text: Option<String>,
    image: Option<PathBuf>,
    list_windows: bool,
    list_rooms: bool,
    limit: usize,
    dry_run: bool,
    no_open: bool,
    take_focus_now: bool,
    focus_wait: u64,
    json: bool,
    archive_path: &Path,
) -> Result<()> {
    use katok::kakao::ax_send;
    use std::io::Read;

    if list_windows {
        let titles = ax_send::open_window_titles()?;
        return print_payload(json, &serde_json::json!({ "open_windows": titles }));
    }
    if list_rooms {
        let rooms = ax_send::chat_list_rooms(limit)?;
        return print_payload(json, &serde_json::json!({ "rooms": rooms }));
    }

    let allow_open = !no_open;
    // Anything that has to bring KakaoTalk forward waits for a gap in the user's
    // own typing first, so a send never lands mid-keystroke.
    let policy = if take_focus_now {
        ax_send::FocusPolicy::immediate()
    } else {
        ax_send::FocusPolicy {
            max_wait: std::time::Duration::from_secs(focus_wait),
            ..ax_send::FocusPolicy::default()
        }
    };

    // Read stdin before the curtain goes up: a blocked terminal behind a
    // full-screen overlay waiting for input the user cannot give would be a
    // deadlock they can only escape by killing the process.
    let body = if dry_run || image.is_some() {
        None
    } else {
        let body = match text {
            Some(t) => t,
            None => {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("failed to read message body from stdin")?;
                buf
            }
        };
        let body = body.trim_end_matches('\n').to_string();
        if body.is_empty() {
            anyhow::bail!("refusing to send an empty message");
        }
        Some(body)
    };

    // The curtain owns the main thread — AppKit insists on it — so the send runs
    // on a worker. It only actually covers the screen for the steps that must
    // bring KakaoTalk forward; an ordinary background text send never raises it.
    // Resolve the target once. A `--chat` id also brings the last-message time,
    // which is the only thing on the chat list that separates two rooms sharing
    // a name — without it such a send is refused rather than sent to whichever
    // one happens to sit higher.
    let target = match (&room, &chat) {
        (_, Some(chat_id)) => {
            let archive = Archive::open(archive_path).context("open archive")?;
            let (name, last) = archive
                .chat_identity(chat_id)
                .context("look up chat")?
                .with_context(|| format!("no chat {chat_id} in the archive; run sync first"))?;
            ax_send::RoomTarget {
                name,
                last_activity: Some(ax_send::chat_list_stamp(&last)),
            }
        }
        (Some(name), None) => ax_send::RoomTarget::named(name),
        (None, None) => anyhow::bail!("pass --room or --chat"),
    };
    let room_display = target.name.clone();
    let target_owned = target.clone();
    let image_owned = image.clone();
    let body_owned = body.clone();
    let outcome =
        katok::kakao::send_curtain::run_with_curtain("카톡 전송중…", move |curtain| {
            let ctx = ax_send::SendContext {
                policy,
                curtain: Some(curtain),
            };
            if dry_run {
                return ax_send::resolve_room_window(&target_owned, allow_open, &ctx);
            }
            if let Some(path) = &image_owned {
                return ax_send::send_image_to_open_window(&target_owned, path, allow_open, &ctx);
            }
            let body = body_owned.expect("text body prepared before the curtain");
            ax_send::send_to_open_window(&target_owned, &body, allow_open, &ctx)
        });

    match outcome {
        Ok(result) => result?,
        // The worker panicked. Input and focus were already handed back by the
        // curtain's own scope guards, so report rather than resume it.
        Err(_) => anyhow::bail!("the send thread panicked; nothing was confirmed sent"),
    }

    if dry_run {
        return print_payload(
            json,
            &serde_json::json!({ "resolved": true, "room": room_display, "sent": false }),
        );
    }
    if let Some(path) = image {
        // Report the file name only; the path can leak directory structure into logs.
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unnamed)");
        return print_payload(
            json,
            &serde_json::json!({ "sent": true, "room": room_display, "image": name }),
        );
    }
    // Never echo the body: sent content is as sensitive as anything else this crate handles.
    let chars = body.map(|b| b.chars().count()).unwrap_or(0);
    print_payload(
        json,
        &serde_json::json!({ "sent": true, "room": room_display, "chars": chars }),
    )
}

fn run_permissions(command: PermissionsCommand) -> Result<()> {
    match command {
        PermissionsCommand::Macos {
            accessibility,
            dry_run,
            json,
        } => permissions::open_macos(accessibility, dry_run, json),
    }
}

fn run_doctor(
    macos_probe_enabled: bool,
    json: bool,
    config: KatokConfig,
    data_dir: PathBuf,
    archive_path: PathBuf,
    semantic_dir: PathBuf,
) -> Result<()> {
    let macos_probe = macos_probe_payload(macos_probe_enabled, &data_dir);
    let payload = serde_json::json!({
        "name": "katok",
        "command": "katok",
        "data_dir": data_dir,
        "archive": archive_path,
        "semantic_index": semantic_dir,
        "freshness": freshness::load(&data_dir)?,
        "local_first": true,
        "macos": cfg!(target_os = "macos"),
        "source_adapter": {
            "configured": config.source_adapter,
            "fixture": "ok",
            "kakaocli": dependency_status("kakaocli"),
            "macos": macos_probe
        },
        "archive": {
            "status": if archive_path.exists() { "present" } else { "missing" }
        },
        "embedder": {
            "model": config.embedder_model,
            "dimension": config.vector_dimension,
            "provider": "local",
            "mode": std::env::var("KATOK_EMBEDDER").unwrap_or_else(|_| "local".to_string()),
            "endpoint": null
        }
    });
    print_payload(json, &payload)
}

fn macos_probe_payload(enabled: bool, data_dir: &Path) -> serde_json::Value {
    if !enabled {
        return serde_json::json!({
            "status": "not_checked",
            "reason": "run katok doctor --macos-probe --json to check KakaoTalk app data access"
        });
    }
    match dirs::home_dir() {
        Some(home) => {
            let status = katok::kakao::probe_status(&home, data_dir);
            serde_json::json!({
                "status": "checked",
                "app_installed": status.app_installed,
                "container_present": status.container_present,
                "db_file_count": status.db_file_count,
                "auth_cached": status.auth_cached
            })
        }
        None => serde_json::json!({ "status": "home_unavailable" }),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_sync(
    source: &str,
    path: Option<PathBuf>,
    json: bool,
    include_touched: bool,
    prune_preview: bool,
    prune_deleted: bool,
    config: &KatokConfig,
    archive_path: &Path,
    data_dir: &Path,
) -> Result<()> {
    let adapter = adapter_for_source(source, path, data_dir)?;
    let read_started = Instant::now();
    let messages = adapter.messages().context("read source messages")?;
    let read_source = read_started.elapsed().as_millis();
    let archive = Archive::open(archive_path).context("open archive")?;
    // Message upserts and the chunk rebuild are one unit: chunks derived from half-written
    // messages are not a usable archive state, so either both land or neither does.
    let report = archive.in_transaction(|| {
        let upsert_started = Instant::now();
        let mut report = archive.sync_messages(&messages).context("sync messages")?;
        let upsert_messages = upsert_started.elapsed().as_millis();

        // Before chunking, so a removed message never reaches a chunk. Inside
        // the same transaction, so a failure anywhere leaves nothing deleted.
        let pruned = if prune_preview || prune_deleted {
            let doomed = archive
                .reconcile_deletions(&messages, prune_deleted)
                .context("reconcile deletions")?;
            if prune_deleted {
                // Their chats have to be rechunked for the removal to reach the
                // chunk text and the index, not just the messages table.
                for chat_id in doomed
                    .iter()
                    .map(|d| d.chat_id.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    if !report.touched_chats.iter().any(|c| c.chat_id == chat_id) {
                        // Empty means "no floor", so the chat rebuilds whole.
                        // A deletion has no surviving row to anchor a tail to,
                        // and it is rare enough that the wider scope is cheap
                        // next to getting it wrong.
                        report.touched_chats.push(katok::types::TouchedChat {
                            chat_id: chat_id.to_string(),
                            earliest_changed_timestamp: String::new(),
                            earliest_changed_message_id: String::new(),
                        });
                    }
                }
            }
            doomed
        } else {
            Vec::new()
        };

        let rebuild_started = Instant::now();
        let settings = ChunkSettings {
            group_gap_seconds: config.chunk_gap_group_seconds,
            direct_gap_seconds: config.chunk_gap_direct_seconds,
        };
        // Recompute only the chats that changed. Three cases still need the full pass: a first
        // sync has no chunks to scope to, a gap-settings change invalidates every existing
        // chunk, and a chunker-version bump does the same — which includes the first run
        // against an archive written before the version was recorded. Without these checks a
        // settings or algorithm change would only ever reach rooms that happened to receive a
        // message, leaving the rest on the old boundaries forever.
        let stored_settings = archive
            .stored_chunk_settings()
            .context("read chunk settings")?;
        let settings_changed = stored_settings
            != Some((
                settings.group_gap_seconds,
                settings.direct_gap_seconds,
                CHUNKER_VERSION,
            ));
        report.chunks = if archive.chunk_count().context("count chunks")? == 0 || settings_changed {
            rebuild_chunks_with_settings(&archive, settings).context("rebuild chunks")?
        } else {
            rebuild_chunks_for_chats(&archive, settings, &report.touched_chats)
                .context("rebuild chunks")?
        };
        archive
            .record_chunk_settings(
                settings.group_gap_seconds,
                settings.direct_gap_seconds,
                CHUNKER_VERSION,
            )
            .context("record chunk settings")?;

        report.timings_ms = SyncTimings {
            read_source,
            upsert_messages,
            rebuild_chunks: rebuild_started.elapsed().as_millis(),
        };
        Ok::<_, anyhow::Error>((report, pruned))
    })?;
    let (mut report, pruned) = report;
    // Gate is output-only: the archive always computed touched_chats for the rebuild above.
    report.include_touched = include_touched;
    freshness::record_sync(data_dir, source, report.total_messages, report.chunks)?;

    if prune_preview || prune_deleted {
        let mut payload = serde_json::to_value(&report).context("serialize sync report")?;
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                if prune_deleted {
                    "pruned_messages".to_string()
                } else {
                    "prunable_messages".to_string()
                },
                serde_json::to_value(&pruned).context("serialize pruned")?,
            );
        }
        return print_payload(json, &payload);
    }
    print_payload(json, &report)
}

fn run_transcript(
    chat: &str,
    since: Option<&str>,
    out: Option<PathBuf>,
    json: bool,
    archive_path: &Path,
    data_dir: &Path,
) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    // Transcripts hold raw message bodies, so they default under the katok data dir rather than
    // the working directory, where they could be committed by accident.
    let out_dir = out.unwrap_or_else(|| data_dir.join("transcripts"));
    let report = export_transcript(&archive, chat, since, &out_dir).context("export transcript")?;
    print_payload(json, &report)
}

fn run_search(
    command: SearchCommand,
    config: &KatokConfig,
    archive_path: &Path,
    semantic_dir: &Path,
) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    match command {
        SearchCommand::Keyword { query, limit, json } => {
            let hits = keyword_search_with_snippet(&archive, &query, limit, config.snippet_length)
                .context("keyword search")?;
            print_payload(json, &hits)
        }
        SearchCommand::Bm25 { query, limit, json } => {
            let hits = bm25_search_with_snippet(&archive, &query, limit, config.snippet_length)
                .context("bm25 search")?;
            print_payload(json, &hits)
        }
        SearchCommand::Semantic { query, limit, json } => {
            let hits = if std::env::var("KATOK_EMBEDDER").unwrap_or_default() == "mock" {
                semantic_search_with_snippet(
                    &archive,
                    semantic_dir,
                    &query,
                    limit,
                    config.snippet_length,
                )
                .context("semantic search")?
            } else {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("create semantic runtime")?;
                runtime
                    .block_on(semantic_search_live_with_config(
                        &archive,
                        semantic_dir,
                        &query,
                        limit,
                        config,
                    ))
                    .context("semantic search")?
            };
            print_payload(json, &hits)
        }
    }
}

fn run_source(command: SourceCommand, config: &KatokConfig, data_dir: &Path) -> Result<()> {
    match command {
        SourceCommand::Chats { source, path, json } => {
            let source = source.unwrap_or_else(|| config.source_adapter.clone());
            let adapter = adapter_for_source(&source, path, data_dir)?;
            let chats = adapter.chats().context("list source chats")?;
            print_payload(json, &chats)
        }
    }
}

fn run_chunks(chat: &str, json: bool, archive_path: &Path) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    let chunks = archive.chunks_for_chat(chat).context("list chunks")?;
    print_payload(json, &chunks)
}

fn run_wipe_index(yes: bool, json: bool, semantic_dir: &Path) -> Result<()> {
    if !yes {
        anyhow::bail!("refusing to wipe semantic index without --yes");
    }
    if semantic_dir.exists() {
        std::fs::remove_dir_all(semantic_dir).context("remove semantic index")?;
    }
    print_payload(json, &serde_json::json!({"semantic_removed": true}))
}
