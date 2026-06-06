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
    #[error("local embedding server unavailable; start TEI on http://localhost:8080 or set HYPE_EMBEDDER=mock for synthetic QA")]
    EmbedderUnavailable,
    #[error("remote embedding endpoints are disabled by default; use a loopback Jina/TEI endpoint or set allow_remote_embeddings = true")]
    RemoteEmbeddingEndpoint,
    #[error("MinSync error: {0}")]
    MinSync(String),
}
