mod embedder;
mod live;
mod mock;
mod store;

pub use live::{
    index_semantic_live, semantic_search_live_with_config, SemanticIndexReport, STORE_DIR,
};
pub use mock::{
    planned_semantic_documents, semantic_search, semantic_search_with_snippet,
    write_semantic_documents, SemanticDocument,
};

pub(crate) const CHUNK_SCHEMA_ID: &str = "katok-kakao-parent-window-v1";
pub(crate) const SOURCE_ID: &str = "katok-kakao-parent-windows";

pub fn semantic_source_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("source").join("chunks")
}

pub(crate) fn document_path(dir: &std::path::Path, chunk_id: &str) -> std::path::PathBuf {
    dir.join(format!("{chunk_id}.md"))
}
