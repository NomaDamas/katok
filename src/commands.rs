use crate::cli::{Commands, PermissionsCommand, SearchCommand, SourceCommand};
use crate::commands::source_adapter::adapter_for_source;
use crate::support::{dependency_status, print_payload};
use anyhow::{Context, Result};
use katok::{
    archive::Archive,
    chunking::{rebuild_chunks_for_chats, rebuild_chunks_with_settings, ChunkSettings},
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
        Commands::Sync { source, path, json } => {
            let source = source.unwrap_or_else(|| config.source_adapter.clone());
            run_sync(&source, path, json, &config, &archive_path, &data_dir)
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
            text,
            image,
            list_windows,
            list_rooms,
            limit,
            dry_run,
            no_open,
            json,
        } => run_send(
            &room,
            text,
            image,
            list_windows,
            list_rooms,
            limit,
            dry_run,
            no_open,
            json,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(target_os = "macos")]
fn run_send(
    room: &str,
    text: Option<String>,
    image: Option<PathBuf>,
    list_windows: bool,
    list_rooms: bool,
    limit: usize,
    dry_run: bool,
    no_open: bool,
    json: bool,
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

    if dry_run {
        // Opening a room is harmless, so this proves targeting end-to-end without sending.
        ax_send::resolve_room_window(room, allow_open)?;
        return print_payload(
            json,
            &serde_json::json!({ "resolved": true, "room": room, "sent": false }),
        );
    }

    if let Some(path) = image {
        ax_send::send_image_to_open_window(room, &path, allow_open)?;
        // Report the file name only; the path can leak directory structure into logs.
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unnamed)");
        return print_payload(
            json,
            &serde_json::json!({ "sent": true, "room": room, "image": name }),
        );
    }

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
    let body = body.trim_end_matches('\n');
    if body.is_empty() {
        anyhow::bail!("refusing to send an empty message");
    }

    ax_send::send_to_open_window(room, body, allow_open)?;
    // Never echo the body: sent content is as sensitive as anything else this crate handles.
    print_payload(
        json,
        &serde_json::json!({ "sent": true, "room": room, "chars": body.chars().count() }),
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

fn run_sync(
    source: &str,
    path: Option<PathBuf>,
    json: bool,
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

        let rebuild_started = Instant::now();
        let settings = ChunkSettings {
            group_gap_seconds: config.chunk_gap_group_seconds,
            direct_gap_seconds: config.chunk_gap_direct_seconds,
        };
        // Recompute only the chats that changed. Two cases still need the full pass: a first
        // sync has no chunks to scope to, and a gap-settings change invalidates every existing
        // chunk. Without the second check a settings change would only ever reach rooms that
        // happened to receive a message, leaving the rest on the old boundaries forever.
        let stored_settings = archive
            .stored_chunk_settings()
            .context("read chunk settings")?;
        let settings_changed =
            stored_settings != Some((settings.group_gap_seconds, settings.direct_gap_seconds));
        report.chunks = if archive.chunk_count().context("count chunks")? == 0 || settings_changed {
            rebuild_chunks_with_settings(&archive, settings).context("rebuild chunks")?
        } else {
            rebuild_chunks_for_chats(&archive, settings, &report.touched_chats)
                .context("rebuild chunks")?
        };
        archive
            .record_chunk_settings(settings.group_gap_seconds, settings.direct_gap_seconds)
            .context("record chunk settings")?;

        report.timings_ms = SyncTimings {
            read_source,
            upsert_messages,
            rebuild_chunks: rebuild_started.elapsed().as_millis(),
        };
        Ok::<_, anyhow::Error>(report)
    })?;
    freshness::record_sync(data_dir, source, report.total_messages, report.chunks)?;
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
