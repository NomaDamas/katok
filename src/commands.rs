use crate::cli::{Commands, PermissionsCommand, SearchCommand, SourceCommand};
use crate::commands::source_adapter::adapter_for_source;
use crate::support::{dependency_status, print_payload};
use anyhow::{Context, Result};
use katok::{
    archive::Archive,
    chunking::{rebuild_chunks_with_settings, ChunkSettings},
    config::KatokConfig,
    search::{bm25_search_with_snippet, keyword_search_with_snippet},
    semantic::{semantic_search_live_with_config, semantic_search_with_snippet},
};
use std::path::{Path, PathBuf};

mod chunk_commands;
mod freshness;
mod index_commands;
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
        Commands::Permissions { command } => run_permissions(command),
        Commands::Chunks { chat, json } => run_chunks(&chat, json, &archive_path),
        Commands::WipeIndex { yes, json } => run_wipe_index(yes, json, &semantic_dir),
    }
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
    let messages = adapter.messages().context("read source messages")?;
    let archive = Archive::open(archive_path).context("open archive")?;
    let mut report = archive.sync_messages(&messages).context("sync messages")?;
    report.chunks = rebuild_chunks_with_settings(
        &archive,
        ChunkSettings {
            group_gap_seconds: config.chunk_gap_group_seconds,
            direct_gap_seconds: config.chunk_gap_direct_seconds,
        },
    )
    .context("rebuild chunks")?;
    freshness::record_sync(data_dir, source, report.total_messages, report.chunks)?;
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
