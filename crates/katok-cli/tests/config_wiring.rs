use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn cli_honors_configured_chunk_gap_when_syncing() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(&config, "chunk_gap_group_seconds = 9999\n").expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "sync",
            "--source",
            "fixture",
            fixture("group_gap.jsonl").to_str().expect("utf8 fixture"),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"chunks\": 1"));
}

#[test]
fn cli_honors_configured_semantic_dir_when_indexing() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(&config, "minsync_dir = \"custom-semantic\"\n").expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().expect("utf8 fixture"),
            "--json",
        ])
        .assert()
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "mock")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "index",
            "--json",
        ])
        .assert()
        .success();

    assert!(data_dir
        .join("custom-semantic/source/chunks/chunk_2aeac4db0a04ceb2.md")
        .exists());
}

#[test]
fn cli_honors_configured_snippet_length_when_searching() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(&config, "snippet_length = 5\n").expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "sync",
            "--source",
            "fixture",
            fixture("group_gap.jsonl").to_str().expect("utf8 fixture"),
            "--json",
        ])
        .assert()
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "search",
            "keyword",
            "점검",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"snippet\": \"첫 번째 \""));
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kakao")
        .join(name)
}
