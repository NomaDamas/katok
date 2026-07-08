use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "katok", about = "katok: local KakaoTalk search CLI")]
pub(crate) struct Cli {
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    Doctor {
        #[arg(long)]
        macos_probe: bool,
        #[arg(long)]
        json: bool,
    },
    Sync {
        #[arg(long)]
        source: Option<String>,
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Index {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    Search {
        #[command(subcommand)]
        command: SearchCommand,
    },
    Chunk {
        #[command(subcommand)]
        command: ChunkCommand,
    },
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    Media {
        #[command(subcommand)]
        command: MediaCommand,
    },
    Permissions {
        #[command(subcommand)]
        command: PermissionsCommand,
    },
    WipeIndex {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    Chunks {
        #[arg(long)]
        chat: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SearchCommand {
    Keyword {
        query: String,
        /// Maximum number of results to return.
        #[arg(long, default_value_t = 10, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100_000))]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Bm25 {
        query: String,
        /// Maximum number of results to return.
        #[arg(long, default_value_t = 10, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100_000))]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Semantic {
        query: String,
        /// Maximum number of results to return.
        #[arg(long, default_value_t = 10, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100_000))]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ChunkCommand {
    Get {
        chunk_id: String,
        #[arg(long)]
        include_message_ids: bool,
        #[arg(long)]
        redact: bool,
        #[arg(long)]
        json: bool,
    },
    Context {
        chunk_id: String,
        #[arg(long)]
        json: bool,
    },
    Parent {
        chunk_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SourceCommand {
    Chats {
        #[arg(long)]
        source: Option<String>,
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum MediaCommand {
    Get {
        /// KakaoTalk chatId to read image messages from.
        #[arg(long)]
        chat: i64,
        /// Optional KakaoTalk logId to extract one image message.
        #[arg(long)]
        log: Option<i64>,
        /// Output directory for decrypted/fetched image files.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Disable CDN downloads and use only local cache/thumbnail/stub tiers.
        #[arg(long)]
        no_cdn: bool,
        /// Maximum number of image messages to read from the room.
        #[arg(long, default_value_t = 5000, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100_000))]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum PermissionsCommand {
    Macos {
        #[arg(long)]
        accessibility: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}
