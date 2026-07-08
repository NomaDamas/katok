//! KakaoTalk media-cache path helpers.
//!
//! Media files live below 40-hex account directories in the KakaoTalk macOS
//! container. Each chat room is a SHA-1 of the reversed chat id, and each media
//! filename stem is a SHA-1 of the reversed KakaoTalk media key string.

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use super::auth;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDirs {
    roots: Vec<PathBuf>,
}

impl MediaDirs {
    /// Scan the KakaoTalk container once for direct 40-hex media account dirs.
    ///
    /// This is intentionally bounded to one directory level. A missing or
    /// unreadable container is an error; an empty media-dir set is a valid
    /// cache state and simply makes lookups miss.
    pub fn discover(home: &Path) -> Result<Self> {
        Self::discover_in_container(&auth::container_dir(home))
    }

    pub fn discover_in_container(container: &Path) -> Result<Self> {
        if !container.is_dir() {
            return Err(Error::Kakao(format!(
                "kakao media container not found: {}",
                container.display()
            )));
        }
        let entries = std::fs::read_dir(container).map_err(|err| {
            Error::Kakao(format!(
                "cannot scan kakao media container {}: {err}",
                container.display()
            ))
        })?;
        let mut roots = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| {
                Error::Kakao(format!(
                    "cannot scan kakao media container {}: {err}",
                    container.display()
                ))
            })?;
            let path = entry.path();
            let is_media_account = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_media_account_dir_name);
            if is_media_account && path.is_dir() {
                roots.push(path);
            }
        }
        roots.sort();
        Ok(Self { roots })
    }

    pub fn from_roots_for_test(mut roots: Vec<PathBuf>) -> Self {
        roots.sort();
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Search every account dir for `<sha1_rev(chat_id)>/<stem><ext>`.
    pub fn find_media_file(&self, chat_id: i64, name_stem: &str, ext: &str) -> Option<PathBuf> {
        let chat_sha = chat_media_dir_name(chat_id);
        let filename = format!("{name_stem}{ext}");
        self.roots
            .iter()
            .map(|root| root.join(&chat_sha).join(&filename))
            .find(|candidate| candidate.is_file())
    }
}

pub fn sha1_rev(input: &str) -> String {
    sha1_hex(input.chars().rev().collect::<String>().as_bytes())
}

pub fn chat_media_dir_name(chat_id: i64) -> String {
    sha1_rev(&chat_id.to_string())
}

pub fn photo_full_stem(log_id: i64) -> String {
    sha1_rev(&format!("p{log_id}"))
}

pub fn photo_thumb_stem(log_id: i64) -> String {
    sha1_rev(&format!("t{log_id}"))
}

pub fn album_full_stem(log_id: i64, idx: usize) -> String {
    sha1_rev(&format!("p{idx}_{log_id}"))
}

pub fn album_thumb_stem(log_id: i64, idx: usize) -> String {
    sha1_rev(&format!("t{idx}_{log_id}"))
}

fn is_media_account_dir_name(name: &str) -> bool {
    name.len() == 40
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha1_hex(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_rev_matches_python_reference() {
        assert_eq!(
            sha1_rev("p1234567890123"),
            "6a57dbd91a25d5f1e503c316e13487b6abd8de5c"
        );
        assert_eq!(
            chat_media_dir_name(1234567890123),
            "f3040a56bce932b9fe31cf4e68a2eae23c33165b"
        );
    }

    #[test]
    fn discovers_only_lowercase_40_hex_account_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let valid = tmp.path().join("0123456789abcdef0123456789abcdef01234567");
        let uppercase = tmp.path().join("abcdefabcdefabcdefabcdefabcdefabcdefABCD");
        let too_long = tmp.path().join("0123456789abcdef0123456789abcdef012345678");
        std::fs::create_dir(&valid).expect("valid dir");
        std::fs::create_dir(&uppercase).expect("uppercase dir");
        std::fs::create_dir(&too_long).expect("too-long dir");
        std::fs::write(
            tmp.path().join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            b"file",
        )
        .expect("media-looking file");

        let dirs = MediaDirs::discover_in_container(tmp.path()).expect("discover");

        assert_eq!(dirs.roots(), &[valid]);
    }

    #[test]
    fn finds_media_by_account_chat_and_stem() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("0123456789abcdef0123456789abcdef01234567");
        let chat_dir = root.join(chat_media_dir_name(42));
        std::fs::create_dir_all(&chat_dir).expect("chat dir");
        let stem = photo_full_stem(77);
        let expected = chat_dir.join(format!("{stem}.img"));
        std::fs::write(&expected, b"media").expect("media file");
        let dirs = MediaDirs::from_roots_for_test(vec![root]);

        assert_eq!(dirs.find_media_file(42, &stem, ".img"), Some(expected));
        assert_eq!(dirs.find_media_file(43, &stem, ".img"), None);
    }
}
