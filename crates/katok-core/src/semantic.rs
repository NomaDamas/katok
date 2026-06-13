mod endpoint;
mod live;
mod mock;

pub use live::{
    index_semantic_live, semantic_search_live_with_config, SemanticIndexReport, STORE_DIR,
};
pub use mock::{
    planned_semantic_documents, semantic_search, semantic_search_with_snippet,
    write_semantic_documents, SemanticDocument,
};

pub(crate) const CHUNK_SCHEMA_ID: &str = "katok-kakao-chunk-v1";
pub(crate) const CHUNK_TYPE: &str = "kakao_chunk";
pub(crate) const SOURCE_ID: &str = "katok-kakao-chunks";

pub fn semantic_source_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("source").join("chunks")
}

pub(crate) fn minsync_chunk_path(chunk_id: &str) -> String {
    format!("chunks/{chunk_id}.md")
}

pub(crate) fn chunk_id_from_minsync_path(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(std::string::ToString::to_string)
}

pub(crate) fn document_path(dir: &std::path::Path, chunk_id: &str) -> std::path::PathBuf {
    dir.join(format!("{chunk_id}.md"))
}
