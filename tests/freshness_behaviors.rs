use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/kakao/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn cli_doctor_reports_sync_and_index_freshness_when_search_needs_updates() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();
    let fixture = fixture_path("replies.jsonl");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("HOME", dir.path())
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "doctor",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"freshness\""))
        .stdout(predicate::str::contains("\"last_sync\": null"))
        .stdout(predicate::str::contains("\"status\": \"not_checked\""))
        .stdout(predicate::str::contains("\"sync_before_search\": true"));

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
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "index",
            "--json",
        ])
        .assert()
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("HOME", dir.path())
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "doctor",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"last_sync\""))
        .stdout(predicate::str::contains("\"source\": \"fixture\""))
        .stdout(predicate::str::contains("\"last_index\""))
        .stdout(predicate::str::contains(
            "\"embedder\": \"embeddinggemma/local-test\"",
        ))
        .stdout(predicate::str::contains("\"sync_before_search\": false"));
}
