//! Native macOS KakaoTalk reader: derives the SQLCipher key, resolves the
//! logged-in user, opens the encrypted databases read-only, and maps their
//! schema to katok's `RawMessage` model.
//!
//! Privacy: this crate never logs message text, real names, room names, phone
//! numbers, derived keys, database paths, or database filenames. Only counts,
//! ids, timestamps, and status are ever emitted.

pub mod auth;
mod bplist;
pub mod derive;
pub mod media_crypto;
pub mod media_paths;
pub mod media_reader;
pub mod media_resolver;
pub mod reader;

use std::path::PathBuf;

use crate::{Error, Result};

pub use auth::{probe_status, AuthOptions, ProbeStatus, ResolvedAuth};
pub use media_reader::{read_media_frames_with_options, MediaQuery};
pub use reader::{ChatRecord, ReaderOutput};

/// Resolve auth and read every openable KakaoTalk database for `home`/`data_dir`.
pub fn read_kakao(home: PathBuf, data_dir: PathBuf) -> Result<ReaderOutput> {
    let options = AuthOptions::new(home, data_dir);
    read_kakao_with_options(&options)
}

/// Resolve auth using explicit `options` (injectable for tests), then read.
pub fn read_kakao_with_options(options: &AuthOptions) -> Result<ReaderOutput> {
    let resolved = auth::resolve_auth(options)?;
    reader::read_databases(&resolved.database_files, resolved.user_id, &resolved.uuid)
}

/// Resolve the default home directory, mirroring `crate::paths`.
pub fn default_home() -> Result<PathBuf> {
    dirs::home_dir().ok_or(Error::HomeDirUnavailable)
}

/// Lowercase-hex helper used by tests to build SHA-512 oracles.
#[cfg(test)]
pub(crate) fn hex_for_test(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
