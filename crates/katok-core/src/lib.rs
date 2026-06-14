pub mod archive;
pub mod chunking;
pub mod config;
pub mod fixture;
pub mod paths;
pub mod search;
pub mod semantic;
pub mod types;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("fixture parse error on line {line}: {source}")]
    Fixture {
        line: usize,
        source: serde_json::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timestamp parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("home directory is unavailable")]
    HomeDirUnavailable,
    #[error("empty chunk cannot be stored")]
    EmptyChunk,
    #[error("empty query")]
    EmptyQuery,
    #[error("chunk not found: {0}")]
    MissingChunk(String),
    #[error("semantic index has never been synced")]
    SemanticIndexMissing,
    #[error("invalid semantic path: {0}")]
    InvalidSemanticPath(std::path::PathBuf),
    #[error("unsupported source adapter: {0}")]
    UnsupportedSource(String),
    #[error("config parse error: {0}")]
    Config(#[from] toml::de::Error),
    #[error("local embedder unavailable: {0}")]
    Embedding(String),
    #[error("local embedding model unavailable; set KATOK_EMBEDDER=local-test for synthetic QA")]
    EmbedderUnavailable,
    #[error("{0}")]
    Kakaocli(String),
    #[error("KakaoTalk reader error: {0}")]
    Kakao(String),
}
