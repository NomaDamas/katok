use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use katok::{
    config::KatokConfig,
    paths::{default_data_dir, ensure_private_dir},
};

mod cli;
mod commands;
mod support;

fn main() {
    let cli = Cli::parse();
    let json = commands::command_requests_json(&cli.command);
    if let Err(error) = run(cli) {
        if json {
            let code = error_code(&error);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": false,
                    "error": {
                        "code": code,
                        "message": error.to_string(),
                        "cause": format!("{error:#}")
                    }
                }))
                .unwrap_or_else(|_| {
                    "{\"ok\":false,\"error\":{\"code\":\"serialization_failed\"}}".to_string()
                })
            );
        } else {
            eprintln!("Error: {error:#}");
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let config = KatokConfig::load(cli.config.as_deref()).context("load config")?;
    let data_dir = match cli.data_dir {
        Some(path) => path,
        None => default_data_dir().context("resolve default data directory")?,
    };
    ensure_private_dir(&data_dir).context("create private data directory")?;
    let archive_path = data_dir.join("archive.sqlite3");
    let semantic_dir = if config.semantic_dir.is_absolute() {
        config.semantic_dir.clone()
    } else {
        data_dir.join(&config.semantic_dir)
    };

    commands::run(cli.command, config, data_dir, archive_path, semantic_dir)
}

fn error_code(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<katok::Error>() {
        Some(katok::Error::SemanticIndexMissing) => "semantic_index_missing",
        Some(katok::Error::SemanticIndexStale(_)) => "semantic_index_stale",
        Some(katok::Error::SemanticIndexBusy(_)) => "semantic_index_busy",
        Some(katok::Error::EmptyQuery) => "empty_query",
        Some(katok::Error::Sql(_)) => "sqlite_error",
        _ => "command_failed",
    }
}
