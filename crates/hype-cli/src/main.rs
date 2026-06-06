use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use hype_core::{
    config::HypeConfig,
    paths::{default_data_dir, ensure_private_dir},
};

mod cli;
mod commands;
mod support;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = HypeConfig::load(cli.config.as_deref()).context("load config")?;
    let data_dir = match cli.data_dir {
        Some(path) => path,
        None => default_data_dir().context("resolve default data directory")?,
    };
    ensure_private_dir(&data_dir).context("create private data directory")?;
    let archive_path = data_dir.join("archive.sqlite3");
    let semantic_dir = if config.minsync_dir.is_absolute() {
        config.minsync_dir.clone()
    } else {
        data_dir.join(&config.minsync_dir)
    };

    commands::run(cli.command, config, data_dir, archive_path, semantic_dir)
}
