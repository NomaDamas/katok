//! Resolve KakaoTalk image media through full cache, CDN, thumbnail, and stub
//! tiers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

use super::media_crypto::decrypt_pkv2_image;
use super::media_paths::MediaDirs;
use crate::{Error, Result};

const DEFAULT_CDN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaTier {
    Full,
    Cdn,
    Thumb,
    Stub,
}

impl MediaTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Cdn => "cdn",
            Self::Thumb => "thumb",
            Self::Stub => "stub",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaResolveOptions {
    pub output_dir: PathBuf,
    pub cdn_enabled: bool,
    pub cdn_timeout: Duration,
    pub now_epoch: i64,
}

impl MediaResolveOptions {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            cdn_enabled: true,
            cdn_timeout: DEFAULT_CDN_TIMEOUT,
            now_epoch: unix_now_epoch(),
        }
    }

    pub fn no_cdn(output_dir: PathBuf) -> Self {
        Self {
            cdn_enabled: false,
            ..Self::new(output_dir)
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaFrameInput {
    pub log_id: i64,
    pub idx: usize,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub checksum_sha1: Option<String>,
    pub full_stem: String,
    pub thumb_stem: String,
    pub output_stem: String,
    pub sender: Option<String>,
    pub sent_at: Option<i64>,
    pub cdn_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MediaRecord {
    #[serde(rename = "logId")]
    pub log_id: i64,
    pub idx: usize,
    #[serde(rename = "w")]
    pub width: Option<i64>,
    #[serde(rename = "h")]
    pub height: Option<i64>,
    #[serde(rename = "cs")]
    pub checksum_sha1: Option<String>,
    pub tier: MediaTier,
    pub tier_reason: String,
    pub path: Option<PathBuf>,
    pub sha1: Option<String>,
    pub sender: Option<String>,
    #[serde(rename = "ts")]
    pub sent_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MediaResolveError {
    #[serde(rename = "logId")]
    pub log_id: i64,
    pub idx: usize,
    pub stage: String,
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MediaReport {
    pub records: Vec<MediaRecord>,
    pub errors: Vec<MediaResolveError>,
    pub tier_counts: BTreeMap<String, usize>,
}

pub fn resolve_media_frames(
    chat_id: i64,
    frames: &[MediaFrameInput],
    media_dirs: &MediaDirs,
    options: &MediaResolveOptions,
) -> Result<MediaReport> {
    resolve_media_frames_with_fetcher(chat_id, frames, media_dirs, options, cdn_fetch)
}

pub fn resolve_media_frames_with_fetcher<F>(
    chat_id: i64,
    frames: &[MediaFrameInput],
    media_dirs: &MediaDirs,
    options: &MediaResolveOptions,
    mut fetcher: F,
) -> Result<MediaReport>
where
    F: FnMut(&str, Duration) -> Result<Vec<u8>>,
{
    let mut report = MediaReport {
        records: Vec::with_capacity(frames.len()),
        errors: Vec::new(),
        tier_counts: BTreeMap::new(),
    };
    for frame in frames {
        let (record, mut errors) = resolve_one(chat_id, frame, media_dirs, options, &mut fetcher)?;
        *report
            .tier_counts
            .entry(record.tier.as_str().to_string())
            .or_insert(0) += 1;
        report.records.push(record);
        report.errors.append(&mut errors);
    }
    Ok(report)
}

pub fn cdn_fetch(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(url)
        .header("User-Agent", "KakaoTalk")
        .call()
        .map_err(|err| Error::Kakao(format!("cdn fetch failed: {err}")))?;
    response
        .body_mut()
        .read_to_vec()
        .map_err(|err| Error::Kakao(format!("cdn body read failed: {err}")))
}

pub fn image_ext(body: &[u8]) -> &'static str {
    if body.starts_with(b"\xff\xd8\xff") {
        ".jpg"
    } else if body.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        ".png"
    } else if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        ".gif"
    } else if body.len() >= 12 && body.starts_with(b"RIFF") && &body[8..12] == b"WEBP" {
        ".webp"
    } else {
        ".bin"
    }
}

fn resolve_one<F>(
    chat_id: i64,
    frame: &MediaFrameInput,
    media_dirs: &MediaDirs,
    options: &MediaResolveOptions,
    fetcher: &mut F,
) -> Result<(MediaRecord, Vec<MediaResolveError>)>
where
    F: FnMut(&str, Duration) -> Result<Vec<u8>>,
{
    let mut errors = Vec::new();
    let mut why = Vec::new();

    let full = media_dirs.find_media_file(chat_id, &frame.full_stem, ".img");
    if let Some(full_path) = full {
        match read_and_decrypt(&full_path, frame.log_id) {
            Ok(body) => {
                let out =
                    options
                        .output_dir
                        .join(format!("{}{}", frame.output_stem, image_ext(&body)));
                write_image(&body, &out)?;
                return Ok((
                    record(frame, MediaTier::Full, "decrypted", Some(out), &body),
                    errors,
                ));
            }
            Err(err) => {
                why.push("full-decrypt-failed".to_string());
                errors.push(error_record(
                    frame,
                    "full",
                    &full_path.display().to_string(),
                    err,
                ));
            }
        }
    } else {
        why.push("full-not-cached".to_string());
    }

    if let Some(url) = frame.cdn_url.as_deref().filter(|_| options.cdn_enabled) {
        match url_expires(url) {
            Some(expires) if expires < options.now_epoch => why.push("cdn-expired".to_string()),
            _ => match fetcher(url, options.cdn_timeout).and_then(|body| {
                verify_cdn_checksum(&body, frame.checksum_sha1.as_deref())?;
                Ok(body)
            }) {
                Ok(body) => {
                    let out = options.output_dir.join(format!(
                        "{}{}",
                        frame.output_stem,
                        image_ext(&body)
                    ));
                    write_image(&body, &out)?;
                    let reason =
                        join_reasons(why.iter().map(String::as_str).chain(["cdn-fetched"]));
                    return Ok((
                        record(frame, MediaTier::Cdn, &reason, Some(out), &body),
                        errors,
                    ));
                }
                Err(err) => {
                    why.push("cdn-failed".to_string());
                    errors.push(error_record(frame, "cdn", &redact_url(url), err));
                }
            },
        }
    }

    let thumb = media_dirs.find_media_file(chat_id, &frame.thumb_stem, ".thm");
    let mut thumb_failed = false;
    if let Some(thumb_path) = thumb {
        match read_and_decrypt(&thumb_path, frame.log_id) {
            Ok(body) => {
                let out = options.output_dir.join(format!(
                    "{}_thumb{}",
                    frame.output_stem,
                    image_ext(&body)
                ));
                write_image(&body, &out)?;
                let reason = join_reasons(why.iter().map(String::as_str));
                return Ok((
                    record(frame, MediaTier::Thumb, &reason, Some(out), &body),
                    errors,
                ));
            }
            Err(err) => {
                thumb_failed = true;
                errors.push(error_record(
                    frame,
                    "thumb",
                    &thumb_path.display().to_string(),
                    err,
                ));
            }
        }
    }

    let stub_head = if thumb_failed || why.iter().any(|item| item == "full-decrypt-failed") {
        "decrypt-failed"
    } else {
        "not-cached"
    };
    let mut detail: Vec<&str> = why
        .iter()
        .map(String::as_str)
        .filter(|item| *item != "full-not-cached" && *item != "full-decrypt-failed")
        .collect();
    if thumb_failed {
        detail.push("thumb-decrypt-failed");
    }
    let reason = join_reasons(std::iter::once(stub_head).chain(detail));
    Ok((record(frame, MediaTier::Stub, &reason, None, &[]), errors))
}

fn record(
    frame: &MediaFrameInput,
    tier: MediaTier,
    reason: &str,
    path: Option<PathBuf>,
    body: &[u8],
) -> MediaRecord {
    MediaRecord {
        log_id: frame.log_id,
        idx: frame.idx,
        width: frame.width,
        height: frame.height,
        checksum_sha1: frame.checksum_sha1.clone(),
        tier,
        tier_reason: reason.to_string(),
        path,
        sha1: if body.is_empty() {
            None
        } else {
            Some(sha1_hex(body))
        },
        sender: frame.sender.clone(),
        sent_at: frame.sent_at,
    }
}

fn error_record(frame: &MediaFrameInput, stage: &str, path: &str, err: Error) -> MediaResolveError {
    MediaResolveError {
        log_id: frame.log_id,
        idx: frame.idx,
        stage: stage.to_string(),
        path: path.to_string(),
        error: err.to_string(),
    }
}

fn read_and_decrypt(path: &Path, log_id: i64) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    decrypt_pkv2_image(&bytes, log_id)
}

fn verify_cdn_checksum(body: &[u8], expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let actual = sha1_hex(body);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(Error::Kakao(format!(
            "cdn body sha1 != cs (expected {}, actual {})",
            expected.to_ascii_lowercase(),
            actual
        )))
    }
}

fn write_image(body: &[u8], out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(out);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, out).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(err)
    })
}

fn tmp_path(out: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let filename = out
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("media");
    out.with_file_name(format!(".{filename}.tmp-{}-{nonce}", std::process::id()))
}

fn url_expires(url: &str) -> Option<i64> {
    url.split(['?', '&'])
        .find_map(|part| part.strip_prefix("expires=")?.parse::<i64>().ok())
}

fn redact_url(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

fn join_reasons<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    items.into_iter().collect::<Vec<_>>().join("+")
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

fn unix_now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kakao::media_paths::{chat_media_dir_name, photo_full_stem, photo_thumb_stem};
    use std::cell::Cell;
    use std::rc::Rc;

    const LOG_ID: i64 = 1_234_567_890_123;
    const CHAT_ID: i64 = 467_153_603_041_939;
    const VECTOR_IMAGE_HEX: &str = "ffd8ffe04b41544f4b2d504b56322d54455354ffd9";
    const VECTOR_IMAGE_SHA1: &str = "91ed9414d7eb34fe648db42be27a0b7847dc8c8e";
    const PYTHON_REFERENCE_PKV2_HEX: &str = concat!(
        "506b7632000102030405060708090a0b0c0d0e0f554b63056928134b57397f6a2e06f1f04",
        "faf2ce5a3905914af3afabf90b8605bc39e6f7ffe132a0bd65963bc6fdbc111d283724581",
        "b869f60e1c85fedaf14265380a50c41ab3efa9a46bade5e1bce7dc175f8fc5d06a29cc",
        "14bb8afbe382eb5bba3e676fd35b0c002fdf5621adedc2d344db8c97873ae4c62769b",
        "38524501062322c5258f86688e325f549a11696b3e68ed354979c4df585732c1d42b",
        "49afe3ac97b46997e39c43e9818cdd9870b7032d8da56cfe0663201a1daa321ad7",
        "a1ee6bbdb584d7b76ca562e05d26eeb3dd7b777c01c18e091bb177fef85bb1013c",
        "6b632c75112780f8f1b423dc5587e17ca1aacc3c8a585373fe2142cd299303fd1",
        "ec64340e58e9dabd4c6f1b2d5298eab53a925efb785f0eac9961d736046ba914fd"
    );

    fn bytes_from_hex(input: &str) -> Vec<u8> {
        assert_eq!(input.len() % 2, 0);
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("hex is utf8");
                u8::from_str_radix(text, 16).expect("valid hex")
            })
            .collect()
    }

    fn fixture() -> (tempfile::TempDir, MediaDirs, MediaResolveOptions) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = tmp.path().join("0123456789abcdef0123456789abcdef01234567");
        let output_dir = tmp.path().join("out");
        std::fs::create_dir_all(&account).expect("account dir");
        let dirs = MediaDirs::from_roots_for_test(vec![account]);
        let mut options = MediaResolveOptions::new(output_dir);
        options.now_epoch = 1_700_000_000;
        (tmp, dirs, options)
    }

    fn frame() -> MediaFrameInput {
        MediaFrameInput {
            log_id: LOG_ID,
            idx: 0,
            width: Some(640),
            height: Some(480),
            checksum_sha1: Some(VECTOR_IMAGE_SHA1.to_string()),
            full_stem: photo_full_stem(LOG_ID),
            thumb_stem: photo_thumb_stem(LOG_ID),
            output_stem: LOG_ID.to_string(),
            sender: Some("tester".to_string()),
            sent_at: Some(1_700_000_001),
            cdn_url: None,
        }
    }

    fn write_cached_media(root: &Path, stem: &str, ext: &str, bytes: &[u8]) -> PathBuf {
        let path = root
            .join("0123456789abcdef0123456789abcdef01234567")
            .join(chat_media_dir_name(CHAT_ID))
            .join(format!("{stem}{ext}"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("chat dir");
        std::fs::write(&path, bytes).expect("cached media");
        path
    }

    #[test]
    fn resolves_local_full_img_first() {
        let (tmp, dirs, options) = fixture();
        let input = frame();
        write_cached_media(
            tmp.path(),
            &input.full_stem,
            ".img",
            &bytes_from_hex(PYTHON_REFERENCE_PKV2_HEX),
        );

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _| {
                panic!("cdn should not be called when full cache exists")
            })
            .expect("resolve");

        assert!(report.errors.is_empty());
        assert_eq!(report.tier_counts.get("full"), Some(&1));
        let record = &report.records[0];
        assert_eq!(record.tier, MediaTier::Full);
        assert_eq!(record.tier_reason, "decrypted");
        assert_eq!(record.sha1.as_deref(), Some(VECTOR_IMAGE_SHA1));
        assert_eq!(
            std::fs::read(record.path.as_ref().expect("path")).expect("image"),
            bytes_from_hex(VECTOR_IMAGE_HEX)
        );
    }

    #[test]
    fn resolves_cdn_after_full_cache_miss_and_verifies_sha1() {
        let (_, dirs, mut options) = fixture();
        let mut input = frame();
        input.cdn_url = Some("https://cdn.example/image?expires=1900000000".to_string());
        let body = bytes_from_hex(VECTOR_IMAGE_HEX);

        let report = resolve_media_frames_with_fetcher(
            CHAT_ID,
            &[input],
            &dirs,
            &options,
            |url, timeout| {
                assert_eq!(url, "https://cdn.example/image?expires=1900000000");
                assert_eq!(timeout, DEFAULT_CDN_TIMEOUT);
                Ok(body.clone())
            },
        )
        .expect("resolve");

        assert!(report.errors.is_empty());
        assert_eq!(report.records[0].tier, MediaTier::Cdn);
        assert_eq!(report.records[0].tier_reason, "full-not-cached+cdn-fetched");
        assert_eq!(report.records[0].sha1.as_deref(), Some(VECTOR_IMAGE_SHA1));
        options.cdn_enabled = false;
    }

    #[test]
    fn cdn_sha1_mismatch_records_error_then_uses_thumbnail() {
        let (tmp, dirs, mut options) = fixture();
        let mut input = frame();
        input.checksum_sha1 = Some("0000000000000000000000000000000000000000".to_string());
        input.cdn_url = Some("https://cdn.example/image?expires=1900000000&secret=x".to_string());
        write_cached_media(
            tmp.path(),
            &input.thumb_stem,
            ".thm",
            &bytes_from_hex(PYTHON_REFERENCE_PKV2_HEX),
        );
        options.cdn_timeout = Duration::from_secs(3);

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, timeout| {
                assert_eq!(timeout, Duration::from_secs(3));
                Ok(bytes_from_hex(VECTOR_IMAGE_HEX))
            })
            .expect("resolve");

        assert_eq!(report.records[0].tier, MediaTier::Thumb);
        assert_eq!(report.records[0].tier_reason, "full-not-cached+cdn-failed");
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].stage, "cdn");
        assert_eq!(report.errors[0].path, "https://cdn.example/image");
        assert!(report.errors[0].error.contains("cdn body sha1 != cs"));
    }

    #[test]
    fn no_cdn_mode_never_calls_network_and_emits_stub() {
        let (_, dirs, options) = fixture();
        let mut options = MediaResolveOptions::no_cdn(options.output_dir);
        options.now_epoch = 1_700_000_000;
        let mut input = frame();
        input.cdn_url = Some("https://cdn.example/image?expires=1900000000".to_string());
        let calls = Rc::new(Cell::new(0));
        let call_count = Rc::clone(&calls);

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, move |_, _| {
                call_count.set(call_count.get() + 1);
                Ok(Vec::new())
            })
            .expect("resolve");

        assert_eq!(calls.get(), 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.records[0].tier, MediaTier::Stub);
        assert_eq!(report.records[0].tier_reason, "not-cached");
        assert_eq!(report.records[0].path, None);
        assert_eq!(report.records[0].sha1, None);
    }

    #[test]
    fn full_decrypt_failure_records_error_and_returns_decrypt_failed_stub() {
        let (tmp, dirs, options) = fixture();
        let input = frame();
        write_cached_media(tmp.path(), &input.full_stem, ".img", b"not-pkv2");

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _| {
                panic!("cdn should not be called without url")
            })
            .expect("resolve");

        assert_eq!(report.records[0].tier, MediaTier::Stub);
        assert_eq!(report.records[0].tier_reason, "decrypt-failed");
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].stage, "full");
        assert!(report.errors[0].error.contains("not a Pkv2 file"));
    }

    #[test]
    fn expired_cdn_url_falls_through_without_fetching() {
        let (_, dirs, mut options) = fixture();
        let mut input = frame();
        input.cdn_url = Some("https://cdn.example/image?expires=1000".to_string());
        let calls = Rc::new(Cell::new(0));
        let call_count = Rc::clone(&calls);

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, move |_, _| {
                call_count.set(call_count.get() + 1);
                Ok(Vec::new())
            })
            .expect("resolve");

        assert_eq!(calls.get(), 0);
        assert_eq!(report.records[0].tier, MediaTier::Stub);
        assert_eq!(report.records[0].tier_reason, "not-cached+cdn-expired");
        options.cdn_enabled = false;
    }
}
