use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::MediaCommand;
use crate::support::print_payload;
use katok::kakao::{
    media_paths::MediaDirs, media_resolver::MediaResolveOptions, read_media_frames_with_options,
    AuthOptions, MediaQuery,
};

pub(crate) fn run(command: MediaCommand, data_dir: &Path) -> Result<()> {
    match command {
        MediaCommand::Get {
            chat,
            log,
            out,
            no_cdn,
            limit,
            json,
        } => run_get(chat, log, out, no_cdn, limit, json, data_dir),
    }
}

fn run_get(
    chat_id: i64,
    log_id: Option<i64>,
    out: Option<PathBuf>,
    no_cdn: bool,
    limit: usize,
    json: bool,
    data_dir: &Path,
) -> Result<()> {
    let home = katok::kakao::default_home().context("resolve home directory")?;
    let auth_options = AuthOptions::new(home.clone(), data_dir.to_path_buf());
    let query = MediaQuery {
        chat_id,
        log_id,
        limit,
    };
    let frames = read_media_frames_with_options(&auth_options, &query)
        .context("read KakaoTalk media rows")?;
    let output_dir = out.unwrap_or_else(|| data_dir.join("media").join(chat_id.to_string()));
    let report = if frames.is_empty() {
        katok::kakao::media_resolver::MediaReport {
            records: Vec::new(),
            errors: Vec::new(),
            tier_counts: std::collections::BTreeMap::new(),
        }
    } else {
        katok::paths::ensure_private_dir(&output_dir).context("create private media output dir")?;
        let media_dirs = MediaDirs::discover(&home).context("scan KakaoTalk media cache dirs")?;
        let options = MediaResolveOptions {
            output_dir: output_dir.clone(),
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
        "output_dir": output_dir,
        "cdn_enabled": !no_cdn,
        "frame_count": frames.len(),
        "records": report.records,
        "errors": report.errors,
        "tier_counts": report.tier_counts,
    });
    print_payload(json, &payload)
}
