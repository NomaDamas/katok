use crate::cli::ChunkCommand;
use crate::support::print_payload;
use anyhow::{Context, Result};
use katok_core::archive::Archive;
use std::path::Path;

pub(crate) fn run(command: ChunkCommand, archive_path: &Path) -> Result<()> {
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
        ChunkCommand::Context { chunk_id, json } => {
            let context = archive
                .chunk_context(&chunk_id)
                .context("get chunk context")?
                .with_context(|| format!("chunk not found: {chunk_id}"))?;
            print_payload(json, &context)
        }
        ChunkCommand::Parent { chunk_id, json } => {
            let parents = archive
                .parent_windows_for_child(&chunk_id)
                .context("get parent windows")?;
            if parents.is_empty() {
                anyhow::bail!("parent window not found for chunk: {chunk_id}");
            }
            print_payload(json, &parents)
        }
    }
}
