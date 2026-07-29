use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::cli::MediaCommand;
use crate::support::print_payload;
use katok::kakao::{
    media_paths::MediaDirs,
    media_reader::read_media_chat_ids_with_options,
    media_resolver::{MediaKind, MediaReport, MediaResolveOptions, MediaTier},
    read_media_frames_with_options, AuthOptions, MediaQuery,
};

pub(crate) fn run(command: MediaCommand, data_dir: &Path) -> Result<()> {
    match command {
        MediaCommand::Get {
            chat,
            log,
            out,
            no_cdn,
            kinds,
            limit,
            json,
        } => run_get(chat, log, out, no_cdn, kinds, limit, json, data_dir),
        MediaCommand::Backfill {
            chat,
            out,
            kinds,
            dry_run,
            max_bytes,
            limit,
            json,
        } => run_backfill(chat, out, kinds, dry_run, max_bytes, limit, json, data_dir),
    }
}

/// Parse `--kind` values, falling back to `default` when none were given.
fn parse_kinds(raw: &[String], default: &[MediaKind]) -> Result<Vec<MediaKind>> {
    if raw.is_empty() {
        return Ok(default.to_vec());
    }
    let mut kinds = Vec::new();
    for value in raw {
        let kind: MediaKind = value.parse().map_err(|err: String| anyhow!(err))?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

fn kind_names(kinds: &[MediaKind]) -> Vec<&'static str> {
    kinds.iter().map(|kind| kind.as_str()).collect()
}

#[allow(clippy::too_many_arguments)]
fn run_get(
    chat_id: i64,
    log_id: Option<i64>,
    out: Option<PathBuf>,
    no_cdn: bool,
    kinds: Vec<String>,
    limit: usize,
    json: bool,
    data_dir: &Path,
) -> Result<()> {
    let kinds = parse_kinds(&kinds, &MediaKind::ALL)?;
    let home = katok::kakao::default_home().context("resolve home directory")?;
    let auth_options = AuthOptions::new(home.clone(), data_dir.to_path_buf());
    let query = MediaQuery {
        chat_id,
        log_id,
        limit,
        kinds: kinds.clone(),
    };
    let frames = read_media_frames_with_options(&auth_options, &query)
        .context("read KakaoTalk media rows")?;
    let output_dir = out.unwrap_or_else(|| data_dir.join("media").join(chat_id.to_string()));
    let report = if frames.is_empty() {
        MediaReport {
            records: Vec::new(),
            errors: Vec::new(),
            tier_counts: BTreeMap::new(),
        }
    } else {
        katok::paths::ensure_private_dir(&output_dir).context("create private media output dir")?;
        let media_dirs = MediaDirs::discover(&home).context("scan KakaoTalk media cache dirs")?;
        let options = MediaResolveOptions {
            cdn_enabled: !no_cdn,
            ..MediaResolveOptions::new(output_dir.clone())
        };
        katok::kakao::media_resolver::resolve_media_frames(chat_id, &frames, &media_dirs, &options)
            .context("resolve media tiers")?
    };
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "log_id": log_id,
        "limit": limit,
        "kinds": kind_names(&kinds),
        "output_dir": output_dir,
        "cdn_enabled": !no_cdn,
        "frame_count": frames.len(),
        "records": report.records,
        "errors": report.errors,
        "tier_counts": report.tier_counts,
    });
    print_payload(json, &payload)
}

#[allow(clippy::too_many_arguments)]
fn run_backfill(
    chat: Option<i64>,
    out: Option<PathBuf>,
    kinds: Vec<String>,
    dry_run: bool,
    max_bytes: u64,
    limit: usize,
    json: bool,
    data_dir: &Path,
) -> Result<()> {
    let kinds = parse_kinds(&kinds, &[MediaKind::File])?;
    let home = katok::kakao::default_home().context("resolve home directory")?;
    let auth_options = AuthOptions::new(home.clone(), data_dir.to_path_buf());
    let root = out.unwrap_or_else(|| data_dir.join("media"));

    let chat_ids = match chat {
        Some(id) => vec![id],
        None => read_media_chat_ids_with_options(&auth_options, &kinds)
            .context("list KakaoTalk rooms holding media")?,
    };

    // Discovering the account dirs is the slow part of a cold container scan, so
    // it happens once for the whole run rather than once per room.
    let media_dirs = MediaDirs::discover(&home).context("scan KakaoTalk media cache dirs")?;

    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut rooms = Vec::new();
    let mut errors = Vec::new();
    let mut saved_bytes: i64 = 0;

    for chat_id in &chat_ids {
        let query = MediaQuery {
            chat_id: *chat_id,
            log_id: None,
            limit,
            kinds: kinds.clone(),
        };
        let frames = read_media_frames_with_options(&auth_options, &query)
            .with_context(|| format!("read media rows for chat {chat_id}"))?;
        if frames.is_empty() {
            continue;
        }
        let output_dir = root.join(chat_id.to_string());
        let options = MediaResolveOptions {
            max_fetch_bytes: max_bytes,
            skip_existing: true,
            dry_run,
            ..MediaResolveOptions::new(output_dir.clone())
        };
        if !dry_run {
            katok::paths::ensure_private_dir(&output_dir)
                .context("create private media output dir")?;
        }
        let report = katok::kakao::media_resolver::resolve_media_frames(
            *chat_id,
            &frames,
            &media_dirs,
            &options,
        )
        .with_context(|| format!("resolve media tiers for chat {chat_id}"))?;

        for (tier, count) in &report.tier_counts {
            *totals.entry(tier.clone()).or_insert(0) += count;
        }
        // In a dry run nothing lands in the Cdn tier, so the same sum reports
        // what would be downloaded instead of what was.
        saved_bytes += report
            .records
            .iter()
            .filter(|record| matches!(record.tier, MediaTier::Cdn | MediaTier::Planned))
            .filter_map(|record| record.size_bytes)
            .sum::<i64>();
        errors.extend(report.errors);
        rooms.push(serde_json::json!({
            "chat_id": chat_id,
            "frame_count": frames.len(),
            "tier_counts": report.tier_counts,
        }));
    }

    let payload = serde_json::json!({
        "dry_run": dry_run,
        "kinds": kind_names(&kinds),
        "output_root": root,
        "max_bytes": max_bytes,
        "room_count": rooms.len(),
        "rooms": rooms,
        "tier_totals": totals,
        "bytes": saved_bytes,
        "errors": errors,
    });
    print_payload(json, &payload)
}
