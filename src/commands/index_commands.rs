use crate::commands::freshness;
use crate::support::print_payload;
use anyhow::{Context, Result};
use katok::{
    archive::Archive,
    config::KatokConfig,
    semantic::{index_semantic_live, planned_semantic_documents, write_semantic_documents},
};
use std::path::Path;

pub(crate) fn run(
    full: bool,
    dry_run: bool,
    json: bool,
    config: &KatokConfig,
    archive_path: &Path,
    semantic_dir: &Path,
    data_dir: &Path,
) -> Result<()> {
    let archive = Archive::open(archive_path).context("open archive")?;
    let chunks = archive.all_chunks().context("load chunks")?;
    let documents = planned_semantic_documents(&archive, semantic_dir).context("plan documents")?;
    let written = if dry_run {
        0
    } else if std::env::var("KATOK_EMBEDDER").unwrap_or_default() == "mock" {
        write_semantic_documents(&archive, semantic_dir).context("write semantic documents")?
    } else {
        return run_live_index(LiveIndexInput {
            full,
            dry_run,
            json,
            config,
            archive: &archive,
            semantic_dir,
            data_dir,
            candidate_chunks: chunks.len(),
            documents,
        });
    };
    let payload = serde_json::json!({
        "full": full,
        "dry_run": dry_run,
        "candidate_chunks": chunks.len(),
        "written_documents": written,
        "embedding_calls": if dry_run { 0 } else { chunks.len() },
        "documents": documents,
        "embedder": config.embedder_model,
        "semantic_units": "parent_windows"
    });
    if !dry_run {
        freshness::record_index(
            data_dir,
            &config.embedder_model,
            "documents",
            "parent_windows",
            chunks.len(),
        )?;
    }
    print_payload(json, &payload)
}

struct LiveIndexInput<'a> {
    full: bool,
    dry_run: bool,
    json: bool,
    config: &'a KatokConfig,
    archive: &'a Archive,
    semantic_dir: &'a Path,
    data_dir: &'a Path,
    candidate_chunks: usize,
    documents: Vec<katok::semantic::SemanticDocument>,
}

fn run_live_index(input: LiveIndexInput<'_>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create semantic runtime")?;
    let report = runtime
        .block_on(index_semantic_live(
            input.archive,
            input.semantic_dir,
            input.config,
        ))
        .context("index semantic documents")?;
    freshness::record_index(
        input.data_dir,
        report.embedder,
        report.vectorstore,
        report.semantic_units,
        report.embedded_texts,
    )?;
    let payload = serde_json::json!({
        "full": input.full,
        "dry_run": input.dry_run,
        "candidate_chunks": input.candidate_chunks,
        "written_documents": report.written_documents,
        "embedding_calls": report.embedding_calls,
        "embedded_texts": report.embedded_texts,
        "documents": input.documents,
        "embedder": report.embedder,
        "vectorstore": report.vectorstore,
        "semantic_units": report.semantic_units
    });
    print_payload(input.json, &payload)
}
