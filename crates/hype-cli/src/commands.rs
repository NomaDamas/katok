use crate::cli::{ChunkCommand, Commands, SearchCommand, SourceCommand};
use crate::support::{dependency_status, print_payload};
use anyhow::{Context, Result};
use hype_adapters::{FixtureAdapter, KakaocliAdapter, SourceAdapter};
use hype_core::{
    archive::Archive,
    chunking::{rebuild_chunks_with_settings, ChunkSettings},
    config::HypeConfig,
    search::{bm25_search_with_snippet, keyword_search_with_snippet},
    semantic::{
        planned_semantic_documents, semantic_search_with_snippet, write_semantic_documents,
    },
};
use std::path::{Path, PathBuf};

pub(crate) fn run(
    command: Commands,
    config: HypeConfig,
    data_dir: PathBuf,
    archive_path: PathBuf,
    semantic_dir: PathBuf,
) -> Result<()> {
    match command {
        Commands::Doctor { json } => run_doctor(json, config, data_dir, archive_path, semantic_dir),
        Commands::Sync { source, path, json } => {
            run_sync(&source, path, json, &config, &archive_path)
        }
        Commands::Index {
            full,
            dry_run,
            json,
        } => run_index(full, dry_run, json, &config, &archive_path, &semantic_dir),
        Commands::Search { command } => run_search(command, &config, &archive_path, &semantic_dir),
        Commands::Chunk { command } => run_chunk(command, &archive_path),
        Commands::Source { command } => run_source(command),
        Commands::Chunks { chat, json } => run_chunks(&chat, json, &archive_path),
        Commands::WipeIndex { yes, json } => run_wipe_index(yes, json, &semantic_dir),
    }
}

fn run_doctor(
    json: bool,
    config: HypeConfig,
    data_dir: PathBuf,
    archive_path: PathBuf,
    semantic_dir: PathBuf,
) -> Result<()> {
    let payload = serde_json::json!({
        "name": "Hydrogen Peroxide",
        "command": "hype",
        "data_dir": data_dir,
        "archive": archive_path,
        "semantic_index": semantic_dir,
        "local_first": true,
        "macos": cfg!(target_os = "macos"),
        "source_adapter": {
            "configured": config.source_adapter,
            "fixture": "ok",
            "kakaocli": dependency_status("kakaocli")
        },
        "archive": {
            "status": if archive_path.exists() { "present" } else { "missing" }
        },
        "embedder": {
            "model": config.embedder_model,
            "dimension": config.vector_dimension,
            "mode": std::env::var("HYPE_EMBEDDER").unwrap_or_else(|_| "local".to_string())
        }
    });
    print_payload(json, &payload)
}

fn run_sync(
    source: &str,
    path: Option<PathBuf>,
    json: bool,
    config: &HypeConfig,
    archive_path: &Path,
) -> Result<()> {
    let adapter = adapter_for_source(source, path)?;
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
    print_payload(json, &report)
}

fn run_index(
    full: bool,
    dry_run: bool,
    json: bool,
    config: &HypeConfig,
    archive_path: &Path,
    semantic_dir: &Path,
) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    let chunks = archive.all_chunks().context("load chunks")?;
    let documents = planned_semantic_documents(&archive, semantic_dir).context("plan documents")?;
    let written = if dry_run {
        0
    } else {
        require_fixture_embedder()?;
        write_semantic_documents(&archive, semantic_dir).context("write semantic documents")?
    };
    let payload = serde_json::json!({
        "full": full,
        "dry_run": dry_run,
        "candidate_chunks": chunks.len(),
        "written_documents": written,
        "embedding_calls": if dry_run { 0 } else { chunks.len() },
        "documents": documents,
        "embedder": config.embedder_model
    });
    print_payload(json, &payload)
}

fn require_fixture_embedder() -> Result<()> {
    let embedder = std::env::var("HYPE_EMBEDDER").unwrap_or_default();
    if embedder != "mock" {
        anyhow::bail!(
            "local embedding server unavailable; start Jina v4 locally or set HYPE_EMBEDDER=mock for fixture QA"
        );
    }
    Ok(())
}

fn run_search(
    command: SearchCommand,
    config: &HypeConfig,
    archive_path: &Path,
    semantic_dir: &Path,
) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    match command {
        SearchCommand::Keyword { query, json } => {
            let hits = keyword_search_with_snippet(&archive, &query, 10, config.snippet_length)
                .context("keyword search")?;
            print_payload(json, &hits)
        }
        SearchCommand::Bm25 { query, json } => {
            let hits = bm25_search_with_snippet(&archive, &query, 10, config.snippet_length)
                .context("bm25 search")?;
            print_payload(json, &hits)
        }
        SearchCommand::Semantic { query, json } => {
            let hits = semantic_search_with_snippet(
                &archive,
                semantic_dir,
                &query,
                10,
                config.snippet_length,
            )
            .context("semantic search")?;
            print_payload(json, &hits)
        }
    }
}

fn run_chunk(command: ChunkCommand, archive_path: &Path) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    match command {
        ChunkCommand::Get {
            chunk_id,
            include_message_ids,
            redact,
            json,
        } => {
            let mut chunk = archive
                .get_chunk(&chunk_id)
                .context("get chunk")?
                .with_context(|| format!("chunk not found: {chunk_id}"))?;
            if redact {
                chunk.text = "[redacted]".to_string();
            }
            if !include_message_ids {
                chunk.message_ids.clear();
            }
            print_payload(json, &chunk)
        }
    }
}

fn run_source(command: SourceCommand) -> Result<()> {
    match command {
        SourceCommand::Chats { source, path, json } => {
            let adapter = adapter_for_source(&source, path)?;
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

fn adapter_for_source(source: &str, path: Option<PathBuf>) -> Result<Box<dyn SourceAdapter>> {
    match source {
        "fixture" => {
            let fixture_path = path.context("fixture source requires a JSONL path")?;
            Ok(Box::new(FixtureAdapter::new(fixture_path)))
        }
        "kakaocli" => Ok(Box::new(KakaocliAdapter)),
        other => anyhow::bail!("unsupported source adapter: {other}"),
    }
}
