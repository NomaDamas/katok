use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> String {
    format!(
        "{}/../../tests/fixtures/kakao/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn cli_help_identifies_katok_when_invoked() {
    let mut cmd = Command::cargo_bin("katok").expect("katok binary");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("katok"))
        .stdout(predicate::str::contains("katok"));
}

#[test]
fn cli_indexes_and_searches_fixture_when_using_data_dir() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();
    let fixture = fixture_path("replies.jsonl");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "sync",
            "--source",
            "fixture",
            &fixture,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("inserted_messages"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "keyword",
            "보고서",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("chunk_2aeac4db0a04ceb2"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "chunk",
            "get",
            "chunk_caaaca07be83adf8",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("parent_chunk_ids"));
}

#[test]
fn cli_reports_semantic_index_states_when_embedder_is_local_test_or_mocked() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();
    let fixture = format!(
        "{}/../../tests/fixtures/kakao/replies.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "semantic",
            "보고서",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "semantic index has never been synced",
        ));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "sync",
            "--source",
            "fixture",
            &fixture,
            "--json",
        ])
        .assert()
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "index",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"embedding_calls\": 0"))
        .stdout(predicate::str::contains("\"documents\""));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "index",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"embedder\": \"embeddinggemma/local-test\"",
        ));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "mock")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "index",
            "--full",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"full\": true"));
}

#[test]
fn cli_lists_gap_chunks_and_applies_chunk_output_flags() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();
    let fixture = format!(
        "{}/../../tests/fixtures/kakao/group_gap.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "sync",
            "--source",
            "fixture",
            &fixture,
            "--json",
        ])
        .assert()
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "chunks",
            "--chat",
            "chat-group-gap",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("chat-group-gap"))
        .stdout(predicate::str::contains("\"message_count\": 2"))
        .stdout(predicate::str::contains("\"message_count\": 1"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "chunk",
            "get",
            "missing",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("chunk not found"));
}

#[test]
fn cli_rejects_malformed_config_and_missing_kakaocli_without_private_dump() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config_path = dir.path().join("bad-katok.toml");
    std::fs::write(&config_path, "chunk_gap_group_seconds = \"bad\"\n").expect("write config");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config_path.to_str().expect("utf8 path"),
            "doctor",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("config parse error"));

    // Force kakaocli to be absent from PATH so the failure is deterministic
    // regardless of whether the host has kakaocli installed.
    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("PATH", dir.path())
        .args(["source", "chats", "--source", "kakaocli", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("kakaocli not found on PATH"));
}
