use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> String {
    format!(
        "{}/../../tests/fixtures/kakao/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn cli_handles_plan_gap_edges_when_exercised() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path().to_str().expect("utf8 path");
    let replies = fixture_path("replies.jsonl");
    let malformed = fixture_path("malformed.jsonl");

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "search",
            "semantic",
            "anything",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "semantic index has never been synced",
        ));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "sync",
            "--source",
            "fixture",
            &malformed,
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("fixture parse error on line 2"))
        .stderr(predicate::str::contains("PRIVATE-MALFORMED-BODY").not());

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "sync",
            "--source",
            "fixture",
            &replies,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"inserted_messages\": 3"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "sync",
            "--source",
            "fixture",
            &replies,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"inserted_messages\": 0"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args(["--data-dir", data_dir, "index", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "local embedding server unavailable",
        ));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .env("HYPE_EMBEDDER", "mock")
        .args(["--data-dir", data_dir, "index", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"written_documents\": 2"))
        .stdout(predicate::str::contains("jinaai/jina-embeddings-v4"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args(["--data-dir", data_dir, "search", "bm25", "", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty query"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "chunk",
            "get",
            "chunk_missing",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("chunk not found"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "chunk",
            "get",
            "chunk_caaaca07be83adf8",
            "--redact",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("회의 전에 확인할게요").not());
}

#[test]
fn cli_splits_group_gap_and_reports_doctor_checks_when_used() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path().to_str().expect("utf8 path");
    let fixture = fixture_path("group_gap.jsonl");

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args(["--data-dir", data_dir, "doctor", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source_adapter"))
        .stdout(predicate::str::contains("embedder"))
        .stdout(predicate::str::contains("macos"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "sync",
            "--source",
            "fixture",
            &fixture,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"chunks\": 2"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--data-dir",
            data_dir,
            "chunks",
            "--chat",
            "chat-group-gap",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("chunk_"))
        .stdout(predicate::str::contains("\"text\"").not());
}

#[test]
fn skill_wrapper_documents_thin_cli_usage_when_present() {
    let skill = std::fs::read_to_string("../../skills/hype/SKILL.md")
        .or_else(|_| std::fs::read_to_string("skills/hype/SKILL.md"))
        .expect("read skill wrapper");
    assert!(skill.contains("hype search"));
    assert!(skill.contains("hype chunk get"));
    assert!(skill.contains("explicit"));
    assert!(!skill.contains("KakaoTalk.db"));
    assert!(!skill.contains("SQLCipher key"));
}
