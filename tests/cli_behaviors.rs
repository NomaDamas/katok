use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/kakao/{name}", env!("CARGO_MANIFEST_DIR"))
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
fn cli_media_get_help_documents_image_extraction_flags() {
    let mut cmd = Command::cargo_bin("katok").expect("katok binary");
    cmd.args(["media", "get", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("media get"))
        .stdout(predicate::str::contains("--chat"))
        .stdout(predicate::str::contains("--log"))
        .stdout(predicate::str::contains("--out"))
        .stdout(predicate::str::contains("--no-cdn"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn cli_reports_macos_permission_panes_without_opening_settings_when_dry_run() {
    let mut cmd = Command::cargo_bin("katok").expect("katok binary");
    cmd.args([
        "permissions",
        "macos",
        "--accessibility",
        "--dry-run",
        "--json",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("full_disk_access"))
    .stdout(predicate::str::contains("accessibility"))
    .stdout(predicate::str::contains("\"opened\": false"))
    .stdout(predicate::str::contains("Privacy_AllFiles"));
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
        "{}/tests/fixtures/kakao/replies.jsonl",
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
        "{}/tests/fixtures/kakao/group_gap.jsonl",
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

#[test]
fn cli_search_limit_flag_caps_result_count() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();

    // Four messages, each in its OWN chat so they chunk (and rank) as four
    // separate hits, all sharing the query term.
    let fixture = dir.path().join("limit.jsonl");
    let mut lines = String::new();
    for i in 1..=4 {
        lines.push_str(&format!(
            "{{\"account_hash\":\"acct-x\",\"chat_id\":\"chat-{i}\",\"chat_name\":\"Room {i}\",\
             \"chat_type\":\"group\",\"message_id\":\"m{i}\",\"sender_id\":\"u{i}\",\
             \"sender_nickname\":\"nick{i}\",\"timestamp\":\"2026-01-01T09:0{i}:00Z\",\
             \"text\":\"공통키워드 점검보고\",\"message_type\":\"text\",\
             \"reply_to_message_id\":null}}\n"
        ));
    }
    std::fs::write(&fixture, lines).expect("write fixture");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "sync",
            "--source",
            "fixture",
            fixture.to_str().expect("utf8 path"),
            "--json",
        ])
        .assert()
        .success();

    let count_hits = |args: &[&str]| -> usize {
        let output = Command::cargo_bin("katok")
            .expect("katok binary")
            .args(args)
            .output()
            .expect("run search");
        assert!(output.status.success(), "search should succeed");
        String::from_utf8(output.stdout)
            .expect("utf8 stdout")
            .matches("\"chunk_id\"")
            .count()
    };

    // Default limit surfaces all four independent hits.
    assert_eq!(
        count_hits(&[
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "keyword",
            "점검보고",
            "--json",
        ]),
        4,
        "default limit should return every hit"
    );

    // --limit 2 caps the same query to two hits.
    assert_eq!(
        count_hits(&[
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "keyword",
            "점검보고",
            "--limit",
            "2",
            "--json",
        ]),
        2,
        "--limit 2 should cap results to two"
    );
}

#[test]
fn cli_resync_refreshes_existing_message_chat_name() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path();
    let fixture = dir.path().join("resync.jsonl");

    let write_fixture = |chat_name: &str| {
        std::fs::write(
            &fixture,
            format!(
                "{{\"account_hash\":\"acct-x\",\"chat_id\":\"100\",\"chat_name\":\"{chat_name}\",\
                 \"chat_type\":\"group\",\"message_id\":\"m100\",\"sender_id\":\"u1\",\
                 \"sender_nickname\":\"nick1\",\"timestamp\":\"2026-01-01T09:00:00Z\",\
                 \"text\":\"재동기화 검색어\",\"message_type\":\"text\",\
                 \"reply_to_message_id\":null}}\n"
            ),
        )
        .expect("write fixture");
    };

    let sync_fixture = || {
        Command::cargo_bin("katok")
            .expect("katok binary")
            .args([
                "--data-dir",
                data_dir.to_str().expect("utf8 path"),
                "sync",
                "--source",
                "fixture",
                fixture.to_str().expect("utf8 path"),
                "--json",
            ])
            .assert()
            .success();
    };

    write_fixture("chat-100");
    sync_fixture();
    write_fixture("Alice, Bob");
    sync_fixture();

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 path"),
            "search",
            "keyword",
            "재동기화",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"chat_name\": \"Alice, Bob\""))
        .stdout(predicate::str::contains("\"chat_name\": \"chat-100\"").not());
}
