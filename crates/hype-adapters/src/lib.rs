use hype_core::{types::RawMessage, Result};
use std::path::Path;
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
        hype_core::fixture::read_fixture(&self.path)
    }
}

pub struct KakaocliAdapter;

impl SourceAdapter for KakaocliAdapter {
    fn chats(&self) -> Result<Vec<ChatSummary>> {
        let output = run_kakaocli("chats")?;
        parse_kakaocli_json(&output)
    }

    fn messages(&self) -> Result<Vec<RawMessage>> {
        let output = run_kakaocli("messages")?;
        parse_kakaocli_json(&output)
    }
}

fn parse_kakaocli_json<T>(bytes: &[u8]) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_slice(bytes).map_err(|_| {
        hype_core::Error::UnsupportedSource("kakaocli returned invalid JSON".to_string())
    })
}

fn run_kakaocli(command: &str) -> Result<Vec<u8>> {
    let output = Command::new("kakaocli")
        .arg(command)
        .arg("--json")
        .output()
        .map_err(|_| {
            hype_core::Error::UnsupportedSource("kakaocli not found or not configured".to_string())
        })?;
    if !output.status.success() {
        return Err(hype_core::Error::UnsupportedSource(
            "kakaocli not found or not configured".to_string(),
        ));
    }
    Ok(output.stdout)
}
