use anyhow::{Context, Result};
use hype_adapters::{FixtureAdapter, KakaocliAdapter, SourceAdapter};
use std::path::PathBuf;

pub(super) fn adapter_for_source(
    source: &str,
    path: Option<PathBuf>,
) -> Result<Box<dyn SourceAdapter>> {
    match source {
        "fixture" => {
            let fixture_path = path.context("fixture source requires a JSONL path")?;
            Ok(Box::new(FixtureAdapter::new(fixture_path)))
        }
        "kakaocli" => Ok(Box::new(KakaocliAdapter)),
        other => anyhow::bail!("unsupported source adapter: {other}"),
    }
}
