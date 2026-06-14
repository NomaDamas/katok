use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/kakao/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn cli_navigates_micro_chunk_context_and_parent_window_when_synced() {
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
        .success();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "chunk",
            "context",
            "chunk_caaaca07be83adf8",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"previous\""))
        .stdout(predicate::str::contains("chunk_2aeac4db0a04ceb2"))
        .stdout(predicate::str::contains("\"next\": null"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "chunk",
            "parent",
            "chunk_caaaca07be83adf8",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"parent_id\""))
        .stdout(predicate::str::contains("window_"))
        .stdout(predicate::str::contains("\"child_chunk_ids\""))
        .stdout(predicate::str::contains("chunk_2aeac4db0a04ceb2"))
        .stdout(predicate::str::contains("chunk_caaaca07be83adf8"));
}

#[test]
fn cli_semantic_search_returns_parent_windows_with_child_chunk_ids() {
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
        .success()
        .stdout(predicate::str::contains(
            "\"semantic_units\": \"parent_windows\"",
        ));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "semantic",
            "회의 보고서",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"unit\": \"parent_window\""))
        .stdout(predicate::str::contains("\"child_chunk_ids\""))
        .stdout(predicate::str::contains("chunk_2aeac4db0a04ceb2"))
        .stdout(predicate::str::contains("chunk_caaaca07be83adf8"));
}

#[test]
fn cli_rejects_stale_micro_chunk_semantic_cursor_when_searching() {
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
        .success();

    let semantic_dir = data_dir.join("semantic");
    std::fs::create_dir_all(&semantic_dir).expect("create semantic dir");
    std::fs::write(
        semantic_dir.join("cursor.json"),
        r#"{
  "source_id": "katok-kakao-parent-windows",
  "last_synced_at": "2026-01-01T00:00:00Z",
  "seen_token": "old",
  "chunk_schema_id": "katok-kakao-chunk-v1",
  "embedder_id": "embeddinggemma/local-test",
  "vectorstore": "local"
}"#,
    )
    .expect("write stale cursor");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "semantic",
            "회의 보고서",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("re-run katok index"))
        .stderr(predicate::str::contains("katok-kakao-chunk-v1"));
}
