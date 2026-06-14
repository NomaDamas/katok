use crate::kakao::{AuthOptions, ReaderOutput};
use crate::{types::RawMessage, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub trait SourceAdapter {
    fn chats(&self) -> Result<Vec<ChatSummary>>;
    fn messages(&self) -> Result<Vec<RawMessage>>;
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ChatSummary {
    pub chat_id: String,
    pub chat_name: String,
    pub chat_type: String,
}

pub struct FixtureAdapter {
    path: std::path::PathBuf,
}

impl FixtureAdapter {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl SourceAdapter for FixtureAdapter {
    fn chats(&self) -> Result<Vec<ChatSummary>> {
        let mut chats = self
            .messages()?
            .into_iter()
            .map(|message| ChatSummary {
                chat_id: message.chat_id,
                chat_name: message.chat_name,
                chat_type: message.chat_type,
            })
            .collect::<Vec<_>>();
        chats.sort_by(|left, right| left.chat_id.cmp(&right.chat_id));
        chats.dedup_by(|left, right| left.chat_id == right.chat_id);
        Ok(chats)
    }

    fn messages(&self) -> Result<Vec<RawMessage>> {
        crate::fixture::read_fixture(&self.path)
    }
}

/// Reads the current macOS KakaoTalk encrypted database natively (no Python,
/// no `kakaocli`). One read is shared between `chats()` and `messages()`: the
/// first call decrypts + scans and memoizes the result, so a caller that
/// invokes both on the same instance pays the decrypt once.
pub struct MacosAdapter {
    options: AuthOptions,
    cached: std::cell::OnceCell<ReaderOutput>,
}

impl MacosAdapter {
    /// Build an adapter for the given `home` and katok `data_dir`.
    pub fn new(home: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            options: AuthOptions::new(home, data_dir),
            cached: std::cell::OnceCell::new(),
        }
    }

    /// Read the databases once and memoize the output. Subsequent calls clone
    /// the cached `ReaderOutput` instead of re-resolving auth and re-decrypting.
    /// Errors are not cached, so a transient failure can be retried.
    fn read(&self) -> Result<ReaderOutput> {
        if let Some(output) = self.cached.get() {
            return Ok(output.clone());
        }
        let output = crate::kakao::read_kakao_with_options(&self.options)?;
        // Ignore a lost race: `get_or_init` is not fallible-friendly, so set and
        // re-read; on the (single-threaded) common path this stores our value.
        let _ = self.cached.set(output.clone());
        Ok(output)
    }
}

impl SourceAdapter for MacosAdapter {
    fn chats(&self) -> Result<Vec<ChatSummary>> {
        let output = self.read()?;
        Ok(output
            .chats
            .into_iter()
            .map(|chat| ChatSummary {
                chat_id: chat.chat_id,
                chat_name: chat.chat_name,
                chat_type: chat.chat_type,
            })
            .collect())
    }

    fn messages(&self) -> Result<Vec<RawMessage>> {
        Ok(self.read()?.messages)
    }
}

pub struct KakaocliAdapter;

impl SourceAdapter for KakaocliAdapter {
    fn chats(&self) -> Result<Vec<ChatSummary>> {
        let output = run_kakaocli("chats")?;
        parse_kakaocli_json("chats", &output)
    }

    fn messages(&self) -> Result<Vec<RawMessage>> {
        let output = run_kakaocli("messages")?;
        parse_kakaocli_json("messages", &output)
    }
}

fn parse_kakaocli_json<T>(command: &str, bytes: &[u8]) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_slice(bytes).map_err(|err| {
        crate::Error::Kakaocli(format!("kakaocli {command} returned invalid JSON: {err}"))
    })
}

fn run_kakaocli(command: &str) -> Result<Vec<u8>> {
    let output = Command::new("kakaocli")
        .arg(command)
        .arg("--json")
        .output()
        .map_err(|err| {
            crate::Error::Kakaocli(format!(
                "kakaocli not found on PATH; install kakaocli or ensure it is executable ({err})"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let detail = if detail.is_empty() {
            "no stderr output (run `kakaocli auth` to check database access)"
        } else {
            detail
        };
        return Err(crate::Error::Kakaocli(format!(
            "kakaocli {command} failed ({}): {detail}",
            output.status
        )));
    }
    Ok(output.stdout)
}
