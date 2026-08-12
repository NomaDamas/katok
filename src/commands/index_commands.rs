use crate::support::print_payload;
use anyhow::{Context, Result};
use katok::{
    archive::Archive,
    config::KatokConfig,
    semantic::{index_semantic_live_for_parents, planned_semantic_documents_for_parents},
    types::ParentChunk,
};
use std::path::Path;

pub(crate) fn run(
    full: bool,
    dry_run: bool,
    json: bool,
    config: &KatokConfig,
    archive_path: &Path,
    semantic_dir: &Path,
    _data_dir: &Path,
) -> Result<()> {
    if !archive_path.is_file() {
        anyhow::bail!("archive is missing; run katok sync before katok index");
    }
    let archive = Archive::open(archive_path).context("open archive")?;
    let candidate_chunks = archive.chunk_count().context("count chunks")?;
    let parents = archive
        .all_parent_chunks()
        .context("load semantic parent windows")?;
    if !dry_run {
        return run_live_index(LiveIndexInput {
            full,
            dry_run,
            json,
            config,
            archive: &archive,
            parents: &parents,
            semantic_dir,
            candidate_chunks,
        });
    }
    let documents = planned_semantic_documents_for_parents(&parents, semantic_dir);
    let payload = serde_json::json!({
        "full": full,
        "dry_run": dry_run,
        "candidate_chunks": candidate_chunks,
        "written_documents": 0,
        "embedding_calls": 0,
        "documents": documents,
        "embedder": config.embedder_model,
        "semantic_units": "parent_windows"
    });
    print_payload(json, &payload)
}

struct LiveIndexInput<'a> {
    full: bool,
    dry_run: bool,
    json: bool,
    config: &'a KatokConfig,
    archive: &'a Archive,
    parents: &'a [ParentChunk],
    semantic_dir: &'a Path,
    candidate_chunks: usize,
}

fn run_live_index(input: LiveIndexInput<'_>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create semantic runtime")?;
    let report = runtime
        .block_on(index_semantic_live_for_parents(
            input.archive,
            input.parents,
            input.semantic_dir,
            input.config,
            input.full,
        ))
        .context("index semantic documents")?;
    let generation = katok::semantic::current_generation(input.semantic_dir)
        .context("resolve committed semantic generation")?;
    let documents = planned_semantic_documents_for_parents(input.parents, &generation);
    let payload = serde_json::json!({
        "full": input.full,
        "dry_run": input.dry_run,
        "candidate_chunks": input.candidate_chunks,
        "written_documents": report.written_documents,
        "embedding_calls": report.embedding_calls,
        "embedded_texts": report.embedded_texts,
        "documents": documents,
        "embedder": report.embedder,
        "vectorstore": report.vectorstore,
        "semantic_units": report.semantic_units,
        "archive_revision": report.archive_revision,
        "reused_vectors": report.reused_vectors,
        "self_healed": report.self_healed,
        "cleanup_warnings": report.cleanup_warnings
    });
    print_payload(input.json, &payload)
}
