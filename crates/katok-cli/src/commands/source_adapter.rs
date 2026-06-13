use anyhow::{Context, Result};
use katok_adapters::{FixtureAdapter, KakaocliAdapter, MacosAdapter, SourceAdapter};
use std::path::{Path, PathBuf};

pub(super) fn adapter_for_source(
    source: &str,
    path: Option<PathBuf>,
    data_dir: &Path,
) -> Result<Box<dyn SourceAdapter>> {
    match source {
        "fixture" => {
            let fixture_path = path.context("fixture source requires a JSONL path")?;
            Ok(Box::new(FixtureAdapter::new(fixture_path)))
        }
        "kakaocli" => Ok(Box::new(KakaocliAdapter)),
        "macos" | "kakao" => {
            let home = katok_kakao::default_home().context("resolve home directory")?;
            Ok(Box::new(MacosAdapter::new(home, data_dir.to_path_buf())))
        }
        other => anyhow::bail!("unsupported source adapter: {other}"),
    }
}
