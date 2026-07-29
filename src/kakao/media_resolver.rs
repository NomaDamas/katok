//! Resolve KakaoTalk media through full cache, CDN, thumbnail, and stub tiers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;

use super::media_crypto::decrypt_pkv2_image;
use super::media_paths::MediaDirs;
use crate::{Error, Result};

const DEFAULT_CDN_TIMEOUT: Duration = Duration::from_secs(20);

/// Ceiling for a single CDN body, held entirely in memory before it is verified
/// and written. KakaoTalk caps a file attachment well below this; the limit is
/// here so an unexpected body cannot exhaust memory.
const DEFAULT_MAX_FETCH_BYTES: u64 = 512 * 1024 * 1024;

/// What a frame is, which decides both the tier ladder and how the output file
/// is named.
///
/// Photos and videos have an on-disk Pkv2 cache and a thumbnail to fall back
/// to. A generic file has neither: scanning the container for every account
/// found `.thm`, `.img`, `.vid` and nothing else, so the CDN is its only tier
/// and an expired signature means the bytes are simply gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Photo,
    Video,
    File,
}

impl MediaKind {
    pub const ALL: [MediaKind; 3] = [MediaKind::Photo, MediaKind::Video, MediaKind::File];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Video => "video",
            Self::File => "file",
        }
    }

    /// Whether this kind is ever written to the local KakaoTalk media cache.
    fn has_local_cache(self) -> bool {
        !matches!(self, Self::File)
    }
}

impl std::str::FromStr for MediaKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "photo" | "image" => Ok(Self::Photo),
            "video" => Ok(Self::Video),
            "file" => Ok(Self::File),
            other => Err(format!(
                "unknown media kind '{other}'; use photo, video, or file"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaTier {
    Full,
    Cdn,
    Thumb,
    Stub,
    /// The output file was already on disk, so nothing was fetched or decrypted.
    Existing,
    /// Dry run only: this frame passed every precondition and a real run would
    /// have downloaded it.
    Planned,
}

impl MediaTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Cdn => "cdn",
            Self::Thumb => "thumb",
            Self::Stub => "stub",
            Self::Existing => "existing",
            Self::Planned => "planned",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaResolveOptions {
    pub output_dir: PathBuf,
    pub cdn_enabled: bool,
    pub cdn_timeout: Duration,
    pub now_epoch: i64,
    /// Refuse to fetch a body whose declared size exceeds this.
    pub max_fetch_bytes: u64,
    /// Skip a frame whose output file already exists, without any network call.
    /// This is what makes a re-run of `media backfill` free and idempotent.
    pub skip_existing: bool,
    /// Evaluate every tier precondition but perform no request and write no
    /// file. A frame that would have been downloaded lands in
    /// [`MediaTier::Planned`] instead, so a preview distinguishes "would fetch"
    /// from "expired" — which switching the CDN tier off cannot do.
    pub dry_run: bool,
}

impl MediaResolveOptions {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            cdn_enabled: true,
            cdn_timeout: DEFAULT_CDN_TIMEOUT,
            now_epoch: unix_now_epoch(),
            max_fetch_bytes: DEFAULT_MAX_FETCH_BYTES,
            skip_existing: false,
            dry_run: false,
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
    pub kind: MediaKind,
    pub log_id: i64,
    pub idx: usize,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub checksum_sha1: Option<String>,
    /// Size the attachment declares, used to skip an oversized body before any
    /// network call rather than after downloading it.
    pub size_bytes: Option<i64>,
    /// Original attachment filename. Present for `MediaKind::File` only, where
    /// it is authoritative for the output extension.
    pub filename: Option<String>,
    pub full_stem: String,
    /// Cache-file extension for `full_stem`: `.img` for photos, `.vid` for videos.
    pub full_ext: &'static str,
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
    pub kind: MediaKind,
    pub name: Option<String>,
    #[serde(rename = "w")]
    pub width: Option<i64>,
    #[serde(rename = "h")]
    pub height: Option<i64>,
    #[serde(rename = "cs")]
    pub checksum_sha1: Option<String>,
    #[serde(rename = "s")]
    pub size_bytes: Option<i64>,
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
    F: FnMut(&str, Duration, u64) -> Result<Vec<u8>>,
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

/// Fetch a CDN body, capped at `max_bytes`.
///
/// The cap has to be passed explicitly: `Body::read_to_vec()` applies its own
/// 10 MB default, which silently turned every video and file larger than that
/// into a `cdn-failed` record even though the URL was perfectly good.
pub fn cdn_fetch(url: &str, timeout: Duration, max_bytes: u64) -> Result<Vec<u8>> {
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
        .with_config()
        .limit(max_bytes)
        .read_to_vec()
        .map_err(|err| Error::Kakao(format!("cdn body read failed: {err}")))
}

/// Output filename for a frame whose body is not yet known.
///
/// A file attachment keeps its original name, which is also the only reliable
/// source of its extension — a `.zip` body sniffs as `.bin`. Photos and videos
/// have no name, so their extension is sniffed from the body instead and this
/// returns `None`.
fn known_output_name(frame: &MediaFrameInput) -> Option<String> {
    let name = frame.filename.as_deref()?;
    let safe = sanitize_filename(name);
    Some(format!("{}_{}", frame.output_stem, safe))
}

/// Characters that are invisible, or that reorder what follows them, and so
/// have no business in a filename.
///
/// The bidi overrides are the reason this exists rather than being a nicety:
/// a name like `invoice\u{202e}gpj.exe` renders as `invoiceexe.jpg`, which is a
/// well-known way to disguise an executable. Anyone can send an attachment, so
/// its declared name is untrusted input.
fn is_invisible_or_reordering(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                      // soft hyphen
        | '\u{180E}'                    // mongolian vowel separator
        | '\u{200B}'..='\u{200F}'       // zero-width spaces/joiners, LRM, RLM
        | '\u{202A}'..='\u{202E}'       // bidi embedding and override
        | '\u{2060}'..='\u{2064}'       // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}'       // bidi isolates
        | '\u{FEFF}'                    // zero-width no-break space / BOM
    )
}

/// Reduce an attachment filename to something that cannot escape the output
/// directory, disguise its own extension, or collide with the temp-file
/// convention.
///
/// KakaoTalk does not constrain the name, so it can carry separators, dot
/// segments, control characters, invisible formatting, or be long enough to
/// blow past the filesystem limit. The extension is preserved because it is
/// what makes the output openable.
///
/// The result is normalized to NFC. A name that arrives decomposed — which is
/// what a macOS sender's HFS+ era filename looks like — would otherwise be
/// stored in a form that does not match what a person types into a search box,
/// even though it looks identical on screen.
fn sanitize_filename(name: &str) -> String {
    const MAX_STEM_BYTES: usize = 120;

    let cleaned: String = name
        .nfc()
        .filter(|c| !is_invisible_or_reordering(*c))
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' || c.is_control() {
                '_'
            } else if c.is_whitespace() {
                // Collapse exotic spaces (NBSP, thin, ideographic) to a plain
                // one so two visually identical names cannot differ in bytes.
                ' '
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        return "attachment".to_string();
    }

    let (stem, ext) = match cleaned.rsplit_once('.') {
        // A dotfile-looking leftover has no usable stem/extension split.
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 16 => (stem, ext),
        _ => (cleaned, ""),
    };
    let mut truncated = String::new();
    for ch in stem.chars() {
        if truncated.len() + ch.len_utf8() > MAX_STEM_BYTES {
            break;
        }
        truncated.push(ch);
    }
    if truncated.is_empty() {
        truncated.push_str("attachment");
    }
    if ext.is_empty() {
        truncated
    } else {
        format!("{truncated}.{ext}")
    }
}

/// Extensions `image_ext` can produce, used to spot an already-written output
/// for a frame whose extension is only known after the body is in hand.
const SNIFFED_EXTS: [&str; 7] = [".jpg", ".png", ".gif", ".webp", ".mp4", ".webm", ".bin"];

/// An output already on disk for this frame, if any.
fn existing_output(frame: &MediaFrameInput, output_dir: &Path) -> Option<PathBuf> {
    if let Some(name) = known_output_name(frame) {
        let candidate = output_dir.join(name);
        return candidate.is_file().then_some(candidate);
    }
    SNIFFED_EXTS
        .iter()
        .map(|ext| output_dir.join(format!("{}{}", frame.output_stem, ext)))
        .find(|candidate| candidate.is_file())
}

/// Sniff an output extension from the decoded body.
///
/// Covers both photo and video bodies: a KakaoTalk video arrives as ISO-BMFF
/// (`....ftyp`) from the CDN, so it must not fall through to `.bin`.
pub fn image_ext(body: &[u8]) -> &'static str {
    if body.starts_with(b"\xff\xd8\xff") {
        ".jpg"
    } else if body.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        ".png"
    } else if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        ".gif"
    } else if body.len() >= 12 && body.starts_with(b"RIFF") && &body[8..12] == b"WEBP" {
        ".webp"
    } else if body.len() >= 12 && &body[4..8] == b"ftyp" {
        ".mp4"
    } else if body.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        ".webm"
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
    F: FnMut(&str, Duration, u64) -> Result<Vec<u8>>,
{
    let mut errors = Vec::new();
    let mut why = Vec::new();

    if options.skip_existing {
        if let Some(existing) = existing_output(frame, &options.output_dir) {
            return Ok((
                record(
                    frame,
                    MediaTier::Existing,
                    "already-present",
                    Some(existing),
                    &[],
                ),
                errors,
            ));
        }
    }

    // A file attachment is never written to the KakaoTalk media cache, so the
    // local tiers are skipped outright rather than probed and reported as a
    // miss — a miss would read as "it might have been there".
    if frame.kind.has_local_cache() {
        let full = media_dirs.find_media_file(chat_id, &frame.full_stem, frame.full_ext);
        if let Some(full_path) = full {
            match read_and_decrypt(&full_path, frame.log_id) {
                Ok(body) => {
                    let out = options.output_dir.join(format!(
                        "{}{}",
                        frame.output_stem,
                        image_ext(&body)
                    ));
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
    }

    if let Some(url) = frame.cdn_url.as_deref().filter(|_| options.cdn_enabled) {
        let expired = matches!(url_expires(url), Some(expires) if expires < options.now_epoch);
        let oversized = matches!(
            frame.size_bytes,
            Some(size) if size > 0 && size as u64 > options.max_fetch_bytes
        );
        let fingerprint = frame
            .checksum_sha1
            .as_deref()
            .filter(|value| !value.is_empty());
        if expired {
            why.push("cdn-expired".to_string());
        } else if oversized {
            // Declared size is known before the request, so an oversized body
            // costs no bandwidth to refuse.
            why.push("cdn-too-large".to_string());
            errors.push(error_record(
                frame,
                "cdn",
                &redact_url(url),
                Error::Kakao(format!(
                    "attachment declares {} bytes, above the {} byte fetch limit",
                    frame.size_bytes.unwrap_or_default(),
                    options.max_fetch_bytes
                )),
            ));
        } else if fingerprint.is_some() && options.dry_run {
            let reason = join_reasons(why.iter().map(String::as_str).chain(["cdn-would-fetch"]));
            return Ok((
                record(frame, MediaTier::Planned, &reason, None, &[]),
                errors,
            ));
        } else if let Some(fingerprint) = fingerprint {
            match fetcher(url, options.cdn_timeout, options.max_fetch_bytes).and_then(|body| {
                verify_cdn_checksum(&body, fingerprint)?;
                Ok(body)
            }) {
                Ok(body) => {
                    let name = known_output_name(frame)
                        .unwrap_or_else(|| format!("{}{}", frame.output_stem, image_ext(&body)));
                    let out = options.output_dir.join(name);
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
            }
        } else {
            // The attachment carries no `cs`, so a fetched body could not be checked against
            // anything. Downloading it anyway would put unverified network bytes on disk under
            // a contract that promises the opposite, so the tier is skipped and said out loud.
            why.push("cdn-unverifiable".to_string());
            errors.push(error_record(
                frame,
                "cdn",
                &redact_url(url),
                Error::Kakao(
                    "attachment carries no cs fingerprint; refusing to store an unverifiable cdn body"
                        .to_string(),
                ),
            ));
        }
    }

    let mut thumb_failed = false;
    if frame.kind.has_local_cache() {
        let thumb = media_dirs.find_media_file(chat_id, &frame.thumb_stem, ".thm");
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
    }

    let stub_head = if thumb_failed || why.iter().any(|item| item == "full-decrypt-failed") {
        "decrypt-failed"
    } else if frame.kind.has_local_cache() {
        "not-cached"
    } else {
        // Saying "not-cached" about a file would imply a cache that could have
        // held it. There is none: the CDN was the only place it ever lived.
        "unavailable"
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
        kind: frame.kind,
        name: frame.filename.clone(),
        width: frame.width,
        height: frame.height,
        checksum_sha1: frame.checksum_sha1.clone(),
        size_bytes: frame.size_bytes,
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

/// Compare a fetched CDN body against the attachment fingerprint.
///
/// Takes the fingerprint by value rather than as an `Option` on purpose: an absent `cs` used to
/// return `Ok(())` here, which let unverified bytes reach disk. Callers must decide what to do
/// about a missing fingerprint before they fetch.
fn verify_cdn_checksum(body: &[u8], expected: &str) -> Result<()> {
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
            kind: MediaKind::Photo,
            log_id: LOG_ID,
            idx: 0,
            width: Some(640),
            height: Some(480),
            checksum_sha1: Some(VECTOR_IMAGE_SHA1.to_string()),
            size_bytes: None,
            filename: None,
            full_stem: photo_full_stem(LOG_ID),
            full_ext: ".img",
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
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _, _| {
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
            |url, timeout, _| {
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
    fn cdn_without_cs_fingerprint_is_refused_not_stored() {
        let (_, dirs, options) = fixture();
        let mut input = frame();
        input.checksum_sha1 = None;
        input.cdn_url = Some("https://cdn.example/image?expires=1900000000".to_string());

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _, _| {
                panic!("cdn must not be fetched when the body could not be verified")
            })
            .expect("resolve");

        let record = &report.records[0];
        assert_eq!(record.tier, MediaTier::Stub);
        assert!(
            record.tier_reason.contains("cdn-unverifiable"),
            "tier_reason must say why the cdn tier was skipped, got {}",
            record.tier_reason
        );
        assert!(record.path.is_none(), "nothing may be written to disk");
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].stage, "cdn");
        assert!(report.errors[0].error.contains("no cs fingerprint"));
    }

    #[test]
    fn cdn_with_empty_cs_fingerprint_is_refused_not_stored() {
        let (_, dirs, options) = fixture();
        let mut input = frame();
        input.checksum_sha1 = Some(String::new());
        input.cdn_url = Some("https://cdn.example/image?expires=1900000000".to_string());

        let report =
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _, _| {
                panic!("cdn must not be fetched when the body could not be verified")
            })
            .expect("resolve");

        assert_eq!(report.records[0].tier, MediaTier::Stub);
        assert!(report.records[0].path.is_none());
        assert_eq!(report.errors.len(), 1);
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

        let report = resolve_media_frames_with_fetcher(
            CHAT_ID,
            &[input],
            &dirs,
            &options,
            |_, timeout, _| {
                assert_eq!(timeout, Duration::from_secs(3));
                Ok(bytes_from_hex(VECTOR_IMAGE_HEX))
            },
        )
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

        let report = resolve_media_frames_with_fetcher(
            CHAT_ID,
            &[input],
            &dirs,
            &options,
            move |_, _, _| {
                call_count.set(call_count.get() + 1);
                Ok(Vec::new())
            },
        )
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
            resolve_media_frames_with_fetcher(CHAT_ID, &[input], &dirs, &options, |_, _, _| {
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

        let report = resolve_media_frames_with_fetcher(
            CHAT_ID,
            &[input],
            &dirs,
            &options,
            move |_, _, _| {
                call_count.set(call_count.get() + 1);
                Ok(Vec::new())
            },
        )
        .expect("resolve");

        assert_eq!(calls.get(), 0);
        assert_eq!(report.records[0].tier, MediaTier::Stub);
        assert_eq!(report.records[0].tier_reason, "not-cached+cdn-expired");
        options.cdn_enabled = false;
    }

    #[test]
    fn sanitized_filename_cannot_escape_the_output_directory() {
        // KakaoTalk does not constrain the attachment name, so a name carrying
        // separators or dot segments must not become a path.
        assert_eq!(sanitize_filename("a/b.zip"), "a_b.zip");
        assert_eq!(sanitize_filename("C:\\win\\x.pdf"), "C__win_x.pdf");
        for hostile in ["../../etc/passwd", "../x.zip", "/abs/path.pdf", ".hidden"] {
            let out = sanitize_filename(hostile);
            assert!(
                !out.contains('/') && !out.contains('\\') && !out.starts_with('.'),
                "{hostile} sanitized to {out}"
            );
        }
    }

    #[test]
    fn sanitized_filename_strips_bidi_override_that_disguises_an_extension() {
        // Renders as "invoiceexe.jpg" in most UIs while actually ending .exe.
        let disguised = "invoice\u{202e}gpj.exe";
        let out = sanitize_filename(disguised);
        assert!(!out.contains('\u{202e}'), "bidi override survived: {out:?}");
        assert!(
            out.ends_with(".exe"),
            "real extension must stay visible: {out}"
        );
    }

    #[test]
    fn sanitized_filename_strips_invisible_characters() {
        for hidden in ['\u{200B}', '\u{200D}', '\u{FEFF}', '\u{2060}', '\u{00AD}'] {
            let out = sanitize_filename(&format!("re{hidden}port.pdf"));
            assert_eq!(out, "report.pdf", "{hidden:?} survived");
        }
    }

    #[test]
    fn sanitized_filename_is_normalized_to_nfc() {
        // Decomposed Hangul: what a name from an older macOS sender looks like.
        let decomposed = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.pdf";
        let out = sanitize_filename(decomposed);
        assert_eq!(out, "한글.pdf");
        // Two names that look identical must not differ in bytes on disk.
        assert_eq!(out, sanitize_filename("한글.pdf"));
    }

    #[test]
    fn sanitized_filename_collapses_exotic_spaces() {
        assert_eq!(sanitize_filename("a\u{00A0}b\u{3000}c.pdf"), "a b c.pdf");
    }

    #[test]
    fn sanitized_filename_keeps_the_extension_and_unicode() {
        assert_eq!(sanitize_filename("분기 보고서.zip"), "분기 보고서.zip");
        assert_eq!(sanitize_filename("report.tar.gz"), "report.tar.gz");
        // A control character would otherwise reach the filesystem verbatim.
        assert_eq!(sanitize_filename("bad\nname.pdf"), "bad_name.pdf");
    }

    #[test]
    fn sanitized_filename_degrades_loudly_rather_than_producing_an_empty_name() {
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("   "), "attachment");
        assert_eq!(sanitize_filename("..."), "attachment");
    }

    #[test]
    fn sanitized_filename_truncates_a_long_stem_but_keeps_the_extension() {
        let long = format!("{}.zip", "가".repeat(200));
        let out = sanitize_filename(&long);
        assert!(out.ends_with(".zip"));
        // Truncation is on a char boundary, so the result is still valid UTF-8
        // and short enough for the filesystem.
        assert!(out.len() <= 120 + ".zip".len());
    }

    #[test]
    fn sniffs_mp4_and_webm_bodies() {
        // ISO-BMFF: 4-byte box size, then "ftyp". A video body must not be .bin.
        let mp4 = b"\x00\x00\x00\x20ftypisom\x00\x00\x02\x00";
        assert_eq!(image_ext(mp4), ".mp4");
        assert_eq!(image_ext(&[0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0]), ".webm");
        assert_eq!(image_ext(b"\xff\xd8\xffrest"), ".jpg");
        assert_eq!(image_ext(b"nonsense-body"), ".bin");
    }
}
