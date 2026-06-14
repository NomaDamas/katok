//! Resolve the KakaoTalk `user_id` and device `uuid` needed to derive the
//! SQLCipher key, then verify it by actually opening a database.
//!
//! Resolution order (first that yields a working key wins):
//!   a. explicit override (config / `KATOK_KAKAO_USER_ID`)
//!   b. katok cache `<data_dir>/kakao/auth.json` ({user_id, uuid}, 0600)
//!   c. k-skill cache bootstrap (`~/.cache/k-skill/kakaotalk-mac-auth.json`,
//!      read ONLY `user_id`)
//!   d. preference plist candidates (direct keys + AlertKakaoIDsList)
//!   e. rayon SHA-512 pre-image recovery of an active DESIGNATEDFRIENDSREVISION
//!
//! Privacy: only `{user_id, uuid}` is ever persisted by katok, never the key.

use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use sha2::{Digest, Sha512};

use super::derive;
use super::reader::probe_database;
use crate::{Error, Result};

const EMPTY_ACCOUNT_HASH: &str = "31bca02094eb78126a517b206a88c73cfa9ec6f704c7030d18212cace820f025\
f00bf0ea68dbf3f3a5436ca63b53bf7bf80ad8d5de7d8359d0b7fed9dbc3ab99";
const DIRECT_USER_ID_KEYS: [&str; 4] = ["userId", "user_id", "KAKAO_USER_ID", "userID"];
const DEFAULT_MAX_USER_ID: i64 = 1_000_000_000;
const DESIGNATED_PREFIX: &str = "DESIGNATEDFRIENDSREVISION:";

/// True for an ASCII lowercase-hex byte (`0-9` or `a-f`), matching the
/// reference `[0-9a-f]` character class. Uppercase is intentionally rejected.
fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// A cheap, side-effect-free snapshot of KakaoTalk readiness. Contains ONLY
/// booleans/counts derived from filesystem and plist presence checks — it never
/// decrypts a database, runs the SHA-512 recovery, or reads message content.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ProbeStatus {
    /// The KakaoTalk macOS preference plist(s) or container exist.
    pub app_installed: bool,
    /// The encrypted-DB container directory is present.
    pub container_present: bool,
    /// Count of `^[0-9a-f]{78}(?:\.db)?$` database files in the container.
    pub db_file_count: usize,
    /// The katok `{user_id, uuid}` auth cache file exists.
    pub auth_cached: bool,
}

/// Side-effect-free probe of KakaoTalk readiness for the doctor command. Only
/// inspects filesystem/plist presence and whether the katok auth cache exists;
/// it never decrypts, never runs SHA-512 recovery, and never reads messages.
pub fn probe_status(home: &Path, data_dir: &Path) -> ProbeStatus {
    let container = container_dir(home);
    let container_present = container.is_dir();
    let db_file_count = if container_present {
        discover_database_files(&container).len()
    } else {
        0
    };
    let app_installed = !preference_paths(home).is_empty() || container_present;
    let auth_cached = katok_cache_path(data_dir).is_file();
    ProbeStatus {
        app_installed,
        container_present,
        db_file_count,
        auth_cached,
    }
}

/// A verified `(user_id, uuid)` plus the discovered openable database files.
#[derive(Debug, Clone)]
pub struct ResolvedAuth {
    pub user_id: i64,
    pub uuid: String,
    pub source: &'static str,
    pub database_files: Vec<PathBuf>,
}

/// Inputs for auth resolution. The home directory and explicit overrides are
/// injectable so unit tests need no system state.
#[derive(Debug, Clone)]
pub struct AuthOptions {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub user_id_override: Option<i64>,
    pub uuid_override: Option<String>,
    pub max_user_id: i64,
}

impl AuthOptions {
    pub fn new(home: PathBuf, data_dir: PathBuf) -> Self {
        let user_id_override = std::env::var("KATOK_KAKAO_USER_ID")
            .ok()
            .and_then(|raw| raw.trim().parse::<i64>().ok())
            .filter(|id| *id > 0);
        Self {
            home,
            data_dir,
            user_id_override,
            uuid_override: None,
            max_user_id: DEFAULT_MAX_USER_ID,
        }
    }
}

pub fn container_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Containers")
        .join("com.kakao.KakaoTalkMac")
        .join("Data")
        .join("Library")
        .join("Application Support")
        .join("com.kakao.KakaoTalkMac")
}

fn katok_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("kakao").join("auth.json")
}

fn k_skill_cache_path(home: &Path) -> PathBuf {
    home.join(".cache")
        .join("k-skill")
        .join("kakaotalk-mac-auth.json")
}

fn preference_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let pref_dir = home
        .join("Library")
        .join("Containers")
        .join("com.kakao.KakaoTalkMac")
        .join("Data")
        .join("Library")
        .join("Preferences");
    if let Ok(entries) = std::fs::read_dir(&pref_dir) {
        let mut matched: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("com.kakao.KakaoTalkMac") && name.ends_with(".plist")
                    })
            })
            .collect();
        matched.sort();
        paths.extend(matched);
    }
    let global = home
        .join("Library")
        .join("Preferences")
        .join("com.kakao.KakaoTalkMac.plist");
    if global.exists() && !paths.contains(&global) {
        paths.push(global);
    }
    paths
}

/// DB files in the container dir matching `^[0-9a-f]{78}(?:\.db)?$`.
pub fn discover_database_files(container: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(container) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_hex_db_name)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

fn is_hex_db_name(name: &str) -> bool {
    // Mirror the reference HEX_DATABASE_PATTERN `^[0-9a-f]{78}(?:\.db)?$`: accept
    // an optional trailing ".db" before the 78-lowercase-hex check.
    let stem = name.strip_suffix(".db").unwrap_or(name);
    stem.len() == 78
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn platform_uuid() -> Result<String> {
    let output = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(|err| Error::Kakao(format!("failed to run ioreg: {err}")))?;
    if !output.status.success() {
        return Err(Error::Kakao("ioreg exited non-zero".to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    extract_platform_uuid(&stdout)
        .ok_or_else(|| Error::Kakao("could not read IOPlatformUUID from ioreg".to_string()))
}

fn extract_platform_uuid(ioreg_output: &str) -> Option<String> {
    let needle = "\"IOPlatformUUID\" = \"";
    let start = ioreg_output.find(needle)? + needle.len();
    let rest = &ioreg_output[start..];
    let end = rest.find('"')?;
    let candidate = &rest[..end];
    if candidate.len() == 36
        && candidate
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Best-effort parse of a preference plist into `(candidate_user_ids,
/// active_account_hash)` via `plutil -convert xml1`.
fn read_plist(path: &Path) -> Option<(Vec<i64>, Option<String>)> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "xml1", "-o", "-"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let xml = String::from_utf8_lossy(&output.stdout);
    Some(scan_plist_xml(&xml))
}

/// Tolerant scan of plist XML. Splitting on `<` yields fragments of the form
/// `tag>body`; for `<key>NAME</key>` the key name is the body of the `key`
/// fragment, and the value element is the next non-`/key` fragment. Only the
/// structure is read, never message bodies.
fn scan_plist_xml(xml: &str) -> (Vec<i64>, Option<String>) {
    let mut candidates: Vec<i64> = Vec::new();
    let mut active_hash: Option<String> = None;
    let mut in_alert_array = false;
    let mut pending_key: Option<String> = None;

    for raw in xml.split('<') {
        let Some(end) = raw.find('>') else {
            continue;
        };
        let tag = &raw[..end];
        let body = raw[end + 1..].trim();

        // <key>NAME</key>: the key name is the body of this fragment.
        if tag == "key" {
            pending_key = Some(body.to_string());
            in_alert_array = body == "AlertKakaoIDsList";
            continue;
        }
        if tag == "/key" {
            continue;
        }

        // Value element paired with the most recent key.
        if let Some(key) = pending_key.clone() {
            if DIRECT_USER_ID_KEYS.contains(&key.as_str()) {
                if let Some(value) = parse_scalar_value(tag, body) {
                    if value > 0 {
                        candidates.push(value);
                    }
                }
                pending_key = None;
                continue;
            }
            if let Some(hash_hex) = key.strip_prefix(DESIGNATED_PREFIX) {
                if active_hash.is_none() && is_active_account_hash(hash_hex, tag, body) {
                    active_hash = Some(hash_hex.to_string());
                }
                pending_key = None;
                continue;
            }
        }

        // AlertKakaoIDsList array members.
        if in_alert_array {
            if tag == "/array" {
                in_alert_array = false;
                pending_key = None;
            } else if let Some(value) = parse_scalar_value(tag, body) {
                if value > 0 {
                    candidates.push(value);
                }
            }
        }
    }

    candidates.sort_unstable();
    candidates.dedup();
    (candidates, active_hash)
}

fn parse_scalar_value(tag: &str, body: &str) -> Option<i64> {
    match tag {
        "integer" | "real" | "string" => body.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn is_active_account_hash(hash_hex: &str, value_tag: &str, value_body: &str) -> bool {
    if hash_hex == EMPTY_ACCOUNT_HASH {
        return false;
    }
    // Match the reference regex `[0-9a-f]{128}`: lowercase hex only. SHA-512
    // hexdigests are always lowercase, so an uppercase key is rejected here
    // rather than fed into a fruitless full 0..=1e9 scan.
    if hash_hex.len() != 128 || !hash_hex.bytes().all(is_lower_hex) {
        return false;
    }
    match value_tag {
        "integer" | "real" => value_body
            .trim()
            .parse::<i64>()
            .map(|v| v != 0)
            .unwrap_or(false),
        "true" => true,
        _ => false,
    }
}

/// Read ONLY `user_id` from a JSON cache file.
fn read_user_id_from_json(path: &Path) -> Option<i64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    match value.get("user_id")? {
        serde_json::Value::Number(num) => num.as_i64().filter(|id| *id > 0),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok().filter(|id| *id > 0),
        _ => None,
    }
}

/// Read `{user_id, uuid}` from the katok cache (both required).
fn read_katok_cache(path: &Path) -> Option<(i64, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let user_id = match value.get("user_id")? {
        serde_json::Value::Number(num) => num.as_i64()?,
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    let uuid = value.get("uuid")?.as_str()?.to_string();
    if user_id > 0 && uuid.len() == 36 {
        Some((user_id, uuid))
    } else {
        None
    }
}

/// Persist only `{user_id, uuid}` at 0600 for the next run.
fn persist_katok_cache(path: &Path, user_id: i64, uuid: &str) {
    if let Some(parent) = path.parent() {
        if crate::paths::ensure_private_dir(parent).is_err() {
            return;
        }
    }
    let payload = serde_json::json!({ "user_id": user_id, "uuid": uuid });
    let Ok(serialized) = serde_json::to_string_pretty(&payload) else {
        return;
    };
    // Write to a temp file in the same dir, created 0600, then atomically rename
    // into place. This avoids both the umask window (the file is never group/
    // world-readable, even briefly) and a torn write of partial JSON.
    let Some(parent) = path.parent() else {
        return;
    };
    let tmp = parent.join(format!(".auth.json.tmp.{}", std::process::id()));
    if write_private_file(&tmp, serialized.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Create `path` with mode 0600 and write `bytes`, truncating any existing file.
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Brute-force the `user_id` whose `sha512(user_id.to_string())` hex equals
/// `target_hash`, using rayon across `0..=max_user_id`.
fn recover_user_id_from_sha512(target_hash: &str, max_user_id: i64) -> Option<i64> {
    // Lowercase hex only, matching the reference regex `[0-9a-f]{128}`. An
    // uppercase target can never match the lowercase digest comparison, so
    // reject it before launching the rayon scan.
    if target_hash.len() != 128 || !target_hash.bytes().all(is_lower_hex) {
        return None;
    }
    let target = target_hash.as_bytes();
    (0..=max_user_id).into_par_iter().find_any(|candidate| {
        let digest = Sha512::digest(candidate.to_string().as_bytes());
        // Compare hex without allocating per candidate.
        sha512_hex_eq(&digest, target)
    })
}

fn sha512_hex_eq(digest: &[u8], target_hex: &[u8]) -> bool {
    if target_hex.len() != digest.len() * 2 {
        return false;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, byte) in digest.iter().enumerate() {
        if target_hex[i * 2] != HEX[(byte >> 4) as usize]
            || target_hex[i * 2 + 1] != HEX[(byte & 0x0f) as usize]
        {
            return false;
        }
    }
    true
}

/// Verify a `(user_id, uuid)` by deriving its key and opening any DB. Returns
/// the openable DB list when the key works against at least one file.
fn verify(user_id: i64, uuid: &str, database_files: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let key = derive::secure_key(user_id, uuid);
    let derived_name = derive::database_name(user_id, uuid);
    let derived_name_db = format!("{derived_name}.db");
    // Probe the derived-name DB first, then any other candidate. Mirror the
    // reference `prioritized_database_paths`, which treats both `<derived>` and
    // `<derived>.db` as the prioritized derived match.
    let is_derived = |path: &PathBuf| -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == derived_name || n == derived_name_db)
    };
    let mut ordered: Vec<&PathBuf> = database_files
        .iter()
        .filter(|path| is_derived(path))
        .collect();
    ordered.extend(database_files.iter().filter(|path| !is_derived(path)));

    let mut openable: Vec<PathBuf> = Vec::new();
    for path in &ordered {
        if probe_database(path, &key).unwrap_or(false) {
            openable.push((*path).clone());
        }
    }
    if openable.is_empty() {
        None
    } else {
        Some(openable)
    }
}

/// Resolve and verify auth following the documented precedence chain.
pub fn resolve_auth(options: &AuthOptions) -> Result<ResolvedAuth> {
    let container = container_dir(&options.home);
    let database_files = discover_database_files(&container);
    if database_files.is_empty() {
        return Err(Error::Kakao(
            "no KakaoTalk database files found in the container directory".to_string(),
        ));
    }

    let uuid = match &options.uuid_override {
        Some(uuid) => uuid.clone(),
        None => platform_uuid()?,
    };

    let katok_cache = katok_cache_path(&options.data_dir);

    // a. explicit override
    if let Some(user_id) = options.user_id_override {
        if let Some(openable) = verify(user_id, &uuid, &database_files) {
            persist_katok_cache(&katok_cache, user_id, &uuid);
            return Ok(ResolvedAuth {
                user_id,
                uuid,
                source: "override",
                database_files: openable,
            });
        }
    }

    // b. katok cache ({user_id, uuid})
    if options.uuid_override.is_none() {
        if let Some((cached_id, cached_uuid)) = read_katok_cache(&katok_cache) {
            if let Some(openable) = verify(cached_id, &cached_uuid, &database_files) {
                return Ok(ResolvedAuth {
                    user_id: cached_id,
                    uuid: cached_uuid,
                    source: "katok-cache",
                    database_files: openable,
                });
            }
        }
    }

    // c. k-skill cache bootstrap (user_id only)
    if let Some(user_id) = read_user_id_from_json(&k_skill_cache_path(&options.home)) {
        if let Some(openable) = verify(user_id, &uuid, &database_files) {
            persist_katok_cache(&katok_cache, user_id, &uuid);
            return Ok(ResolvedAuth {
                user_id,
                uuid,
                source: "k-skill-cache",
                database_files: openable,
            });
        }
    }

    // d. plist candidates
    let mut candidates: Vec<i64> = Vec::new();
    let mut active_hash: Option<String> = None;
    for path in preference_paths(&options.home) {
        if let Some((ids, hash)) = read_plist(&path) {
            candidates.extend(ids);
            if active_hash.is_none() {
                active_hash = hash;
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    for user_id in &candidates {
        if let Some(openable) = verify(*user_id, &uuid, &database_files) {
            persist_katok_cache(&katok_cache, *user_id, &uuid);
            return Ok(ResolvedAuth {
                user_id: *user_id,
                uuid,
                source: "plist",
                database_files: openable,
            });
        }
    }

    // e. SHA-512 hash recovery (expensive; logged once)
    if let Some(hash) = active_hash {
        eprintln!("katok: recovering KakaoTalk user id (one-time SHA-512 scan)...");
        if let Some(user_id) = recover_user_id_from_sha512(&hash, options.max_user_id) {
            if !candidates.contains(&user_id) {
                if let Some(openable) = verify(user_id, &uuid, &database_files) {
                    persist_katok_cache(&katok_cache, user_id, &uuid);
                    return Ok(ResolvedAuth {
                        user_id,
                        uuid,
                        source: "sha512-recovery",
                        database_files: openable,
                    });
                }
            }
        }
    }

    Err(Error::Kakao(
        "could not resolve a working KakaoTalk user id; set KATOK_KAKAO_USER_ID".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_platform_uuid_from_ioreg_block() {
        let sample = r#"  +-o IOPlatformExpertDevice  <class IOPlatformExpertDevice>
    {
      "IOPlatformUUID" = "42C34717-27C3-538C-81E4-8B568287C7A0"
      "IOPlatformSerialNumber" = "XXXX"
    }"#;
        assert_eq!(
            extract_platform_uuid(sample).as_deref(),
            Some("42C34717-27C3-538C-81E4-8B568287C7A0")
        );
    }

    #[test]
    fn recovers_small_user_id_via_sha512() {
        let target = {
            use sha2::{Digest, Sha512};
            let digest = Sha512::digest(b"4242");
            super::super::hex_for_test(&digest)
        };
        assert_eq!(recover_user_id_from_sha512(&target, 10_000), Some(4242));
    }

    #[test]
    fn hex_db_name_requires_lowercase_78() {
        assert!(is_hex_db_name(&"a".repeat(78)));
        assert!(!is_hex_db_name(&"a".repeat(77)));
        assert!(!is_hex_db_name(&"A".repeat(78)));
        assert!(!is_hex_db_name(&"g".repeat(78)));
    }

    #[test]
    fn hex_db_name_accepts_optional_db_suffix() {
        // Reference HEX_DATABASE_PATTERN `^[0-9a-f]{78}(?:\.db)?$`.
        assert!(is_hex_db_name(&format!("{}.db", "a".repeat(78))));
        // Wrong stem length even with the suffix is still rejected.
        assert!(!is_hex_db_name(&format!("{}.db", "a".repeat(77))));
        // A bare ".db" or other suffixes (-wal/-shm) are not databases.
        assert!(!is_hex_db_name(".db"));
        assert!(!is_hex_db_name(&format!("{}-wal", "a".repeat(78))));
    }

    #[test]
    fn discovers_db_suffixed_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stem = "a".repeat(78);
        let suffixed = dir.path().join(format!("{stem}.db"));
        std::fs::write(&suffixed, b"x").expect("write db file");
        let found = discover_database_files(dir.path());
        assert_eq!(found, vec![suffixed]);
    }

    #[test]
    fn uppercase_active_hash_is_rejected() {
        // SHA-512 hexdigests are always lowercase; an uppercase hash must be
        // rejected before it can drive a fruitless full scan.
        let upper = "A".repeat(128);
        assert!(!is_active_account_hash(&upper, "integer", "5"));
        let lower = "a".repeat(128);
        assert!(is_active_account_hash(&lower, "integer", "5"));
    }

    #[test]
    fn uppercase_hash_recovery_returns_without_scanning() {
        // An uppercase target can never match the lowercase digest comparison,
        // so it must short-circuit to None rather than scan 0..=max_user_id.
        // A huge max proves no scan ran (the test would hang otherwise).
        let upper = "A".repeat(128);
        assert_eq!(recover_user_id_from_sha512(&upper, i64::MAX), None);
    }

    #[test]
    fn probe_status_reports_presence_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path().join("home");
        let data_dir = dir.path().join("data");

        // Nothing present yet.
        let empty = probe_status(&home, &data_dir);
        assert!(!empty.app_installed);
        assert!(!empty.container_present);
        assert_eq!(empty.db_file_count, 0);
        assert!(!empty.auth_cached);

        // Create the container with one valid DB file and an auth cache.
        let container = container_dir(&home);
        std::fs::create_dir_all(&container).expect("container");
        std::fs::write(container.join("a".repeat(78)), b"x").expect("db");
        std::fs::write(container.join("not-a-db.txt"), b"x").expect("noise");
        let cache = katok_cache_path(&data_dir);
        std::fs::create_dir_all(cache.parent().unwrap()).expect("cache dir");
        std::fs::write(&cache, b"{}").expect("cache");

        let ready = probe_status(&home, &data_dir);
        assert!(ready.app_installed);
        assert!(ready.container_present);
        assert_eq!(ready.db_file_count, 1);
        assert!(ready.auth_cached);
    }

    #[test]
    fn scans_direct_user_id_key() {
        let xml = "<plist><dict><key>userId</key><integer>240061982</integer></dict></plist>";
        let (ids, hash) = scan_plist_xml(xml);
        assert_eq!(ids, vec![240_061_982]);
        assert!(hash.is_none());
    }

    #[test]
    fn scans_alert_array_and_active_hash() {
        let active = "a".repeat(128);
        let xml = format!(
            "<plist><dict>\
             <key>AlertKakaoIDsList</key><array><integer>111</integer><integer>222</integer></array>\
             <key>DESIGNATEDFRIENDSREVISION:{active}</key><integer>5</integer>\
             </dict></plist>"
        );
        let (ids, hash) = scan_plist_xml(&xml);
        assert_eq!(ids, vec![111, 222]);
        assert_eq!(hash.as_deref(), Some(active.as_str()));
    }

    #[test]
    fn ignores_empty_account_hash() {
        let xml = format!(
            "<plist><dict><key>DESIGNATEDFRIENDSREVISION:{EMPTY_ACCOUNT_HASH}</key><integer>5</integer></dict></plist>"
        );
        let (_ids, hash) = scan_plist_xml(&xml);
        assert!(hash.is_none());
    }
}
