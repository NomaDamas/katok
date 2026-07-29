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
        /// Report messages the source no longer has, without deleting anything.
        ///
        /// Only the time range the source still covers is considered, so history
        /// KakaoTalk has since pruned is never listed.
        #[arg(long)]
        prune_preview: bool,
        /// Actually delete what `--prune-preview` reports.
        ///
        /// This is the only command that removes archived messages. Preview
        /// first; a deletion cannot be undone from within katok.
        #[arg(long, conflicts_with = "prune_preview")]
        prune_deleted: bool,
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
        /// Title of the chat as the chat list shows it. Note the self-chat window is titled
        /// with your own nickname, not "나와의 채팅".
        ///
        /// Names are not unique — several rooms can share one. When they do, the send is
        /// refused rather than guessed at; use `--chat` instead.
        #[arg(long, required_unless_present = "chat")]
        room: Option<String>,
        /// Address the room by its `chat_id`, as `search` and `chunks` report it.
        ///
        /// Unambiguous: the name and the last-message time are both read from the archive, and
        /// together they pick the right row even when two rooms share a name.
        #[arg(long, conflicts_with = "room")]
        chat: Option<String>,
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
        /// Leave the message in the compose box for review instead of sending it.
        ///
        /// Pasted rather than typed, so nothing is delivered until a person presses Enter.
        /// Needs the screen for a moment, like sending an image does.
        #[arg(long, conflicts_with_all = ["image", "dry_run"])]
        draft: bool,
        /// Take focus immediately instead of waiting for a gap in your typing.
        ///
        /// Only steps that cannot run in the background wait at all — sending an image, and
        /// opening a closed room. Use this when nobody is at the keyboard.
        #[arg(long)]
        take_focus_now: bool,
        /// Seconds to wait for that gap before giving up and sending nothing.
        #[arg(long, default_value_t = 15, conflicts_with = "take_focus_now")]
        focus_wait: u64,
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
    /// Extract media from one room: photos, albums, videos, and file attachments.
    Get {
        /// KakaoTalk chatId to read media messages from.
        #[arg(long)]
        chat: i64,
        /// Optional KakaoTalk logId to extract one media message.
        #[arg(long)]
        log: Option<i64>,
        /// Output directory for decrypted/fetched media files.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Disable CDN downloads and use only local cache/thumbnail/stub tiers.
        /// File attachments have no local tier, so they resolve to nothing here.
        #[arg(long)]
        no_cdn: bool,
        /// Media kinds to extract; repeatable. Defaults to every kind.
        #[arg(long = "kind", value_name = "KIND")]
        kinds: Vec<String>,
        /// Maximum number of media messages to read from the room.
        #[arg(long, default_value_t = 5000, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100_000))]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Save every attachment whose CDN link is still valid, across all rooms.
    ///
    /// KakaoTalk presigns an attachment URL for roughly 14 days and keeps no
    /// local copy of file attachments, so anything not fetched inside that
    /// window is gone for good. Run this on a schedule to keep the window from
    /// closing on files you have not opened.
    ///
    /// Re-running is free: a frame whose output already exists is skipped
    /// without a network call.
    Backfill {
        /// Limit to one room instead of every room that holds media.
        #[arg(long)]
        chat: Option<i64>,
        /// Root output directory; each room gets a `<chatId>` subdirectory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Media kinds to preserve; repeatable. Defaults to `file`, the only
        /// kind with no local cache to fall back on.
        #[arg(long = "kind", value_name = "KIND")]
        kinds: Vec<String>,
        /// Report what would be fetched without downloading anything.
        #[arg(long)]
        dry_run: bool,
        /// Refuse to download an attachment larger than this many bytes.
        #[arg(long, default_value_t = 512 * 1024 * 1024)]
        max_bytes: u64,
        /// Maximum number of media messages to read per room.
        #[arg(long, default_value_t = 100_000, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=1_000_000))]
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
