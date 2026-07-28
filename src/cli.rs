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
        /// Include per-chat earliest-change keys (`touched_chats`) in the report.
        ///
        /// Opt-in: without this flag the JSON shape matches historical consumers exactly.
        /// Each entry is `{chat_id, earliest_changed_timestamp, earliest_changed_message_id}`.
        #[arg(long)]
        touched: bool,
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
    /// Export one chat's raw messages over a time range as a Markdown transcript.
    ///
    /// Reads the archive, not the live KakaoTalk database, so run `sync` first when the tail
    /// matters. A range holding no messages writes no file.
    Transcript {
        /// chat_id to export, as reported by `source chats` or a search hit.
        #[arg(long)]
        chat: String,
        /// Only include messages at or after this RFC3339 timestamp.
        #[arg(long)]
        since: Option<String>,
        /// Directory to write the transcript into.
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Send a message into an already open KakaoTalk chat window (macOS only).
    ///
    /// Unlike every other subcommand this writes rather than reads, and it does so by driving
    /// the running app's UI — there is no supported write path into the local archive. The
    /// target window must already be open; KakaoTalk is never brought to the front.
    #[cfg(target_os = "macos")]
    Send {
        /// Exact title of the open chat window. Note the self-chat window is titled with your
        /// own nickname, not "나와의 채팅".
        #[arg(long)]
        room: String,
        /// Message body. Reads stdin when omitted.
        #[arg(long)]
        text: Option<String>,
        /// Send an image file instead of text. Mutually exclusive with --text.
        #[arg(long, conflicts_with = "text")]
        image: Option<PathBuf>,
        /// List the chat windows currently open and exit without sending.
        #[arg(long)]
        list_windows: bool,
        /// List room names from the chat list (newest first) and exit without sending.
        #[arg(long)]
        list_rooms: bool,
        /// Cap for --list-rooms.
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// Resolve (and open) the room window but do not send. For verifying targeting safely.
        #[arg(long)]
        dry_run: bool,
        /// Fail instead of opening the room when its window is closed. Use for automation that
        /// must never touch the screen: opening a room briefly moves KakaoTalk's own windows.
        #[arg(long)]
        no_open: bool,
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
