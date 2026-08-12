use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kakao")
        .join(name)
}

fn run_json(data_dir: &Path, args: &[&str]) -> Value {
    let mut command = Command::cargo_bin("katok").expect("katok binary");
    command
        .env("KATOK_EMBEDDER", "local-test")
        .arg("--data-dir")
        .arg(data_dir);
    command.args(args);
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid json output")
}

fn current_generation(data_dir: &Path) -> PathBuf {
    let semantic = data_dir.join("semantic");
    let id = std::fs::read_to_string(semantic.join("CURRENT")).expect("CURRENT");
    semantic.join("generations").join(id.trim())
}

#[test]
fn doctor_and_search_reject_an_archive_newer_than_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let newer = dir.path().join("newer.jsonl");
    let mut rows = std::fs::read_to_string(fixture("replies.jsonl")).expect("fixture");
    rows.push_str("{\"account_hash\":\"acct-synthetic\",\"chat_id\":\"chat-new\",\"chat_name\":\"Synthetic New\",\"chat_type\":\"group\",\"message_id\":\"new-1\",\"sender_id\":\"u3\",\"sender_nickname\":\"지수\",\"timestamp\":\"2026-01-02T09:00:00Z\",\"text\":\"새 인덱스 토큰\",\"message_type\":\"text\",\"reply_to_message_id\":null}\n");
    std::fs::write(&newer, rows).expect("newer fixture");

    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    run_json(&data_dir, &["index", "--json"]);
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            newer.to_str().unwrap(),
            "--json",
        ],
    );

    let doctor = run_json(&data_dir, &["doctor", "--json"]);
    assert_eq!(
        doctor["freshness"]["recommendation"]["index_before_semantic_search"],
        true
    );

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["search", "semantic", "새 인덱스 토큰", "--json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("semantic index is stale"));
}

#[test]
fn index_self_heals_an_orphan_generation_and_full_never_reuses_vectors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    run_json(&data_dir, &["index", "--json"]);

    let generation = current_generation(&data_dir);
    let store =
        rusqlite::Connection::open(generation.join("store/vectors.sqlite3")).expect("store");
    store.execute(
        "INSERT INTO vectors(chunk_id, content_hash, seen_token, heading_path, vector) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["window_orphan", "stale", "stale", "Synthetic", vec![0_u8; 768 * 4]],
    ).expect("insert orphan");
    drop(store);

    let report = run_json(&data_dir, &["index", "--json"]);
    assert_eq!(report["self_healed"], true);
    let search = run_json(&data_dir, &["search", "semantic", "회의 보고서", "--json"]);
    assert!(search.as_array().is_some_and(|hits| !hits.is_empty()));

    let full = run_json(&data_dir, &["index", "--full", "--json"]);
    assert_eq!(full["full"], true);
    assert_eq!(full["reused_vectors"], 0);
    assert!(full["embedded_texts"]
        .as_u64()
        .is_some_and(|count| count > 0));
    let generations = std::fs::read_dir(data_dir.join("semantic/generations"))
        .expect("generation directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("generation entries");
    assert_eq!(generations.len(), 1);
}

#[test]
fn stale_but_structurally_valid_generation_reuses_unchanged_vectors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    run_json(
        &data,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    let first = run_json(&data, &["index", "--json"]);
    assert!(first["embedded_texts"].as_u64().unwrap() > 0);

    let extra = dir.path().join("extra.jsonl");
    std::fs::write(
        &extra,
        concat!(
            "{\"account_hash\":\"acct\",\"chat_id\":\"chat-new\",\"chat_name\":\"Synthetic new room\",",
            "\"chat_type\":\"group\",\"message_id\":\"new-1\",\"sender_id\":\"sender-new\",",
            "\"sender_nickname\":\"Tester\",\"timestamp\":\"2026-01-02T00:00:00Z\",",
            "\"text\":\"brand new semantic material\",\"message_type\":\"text\",\"reply_to_message_id\":null}\n"
        ),
    )
    .expect("write extra fixture");
    run_json(
        &data,
        &[
            "sync",
            "--source",
            "fixture",
            extra.to_str().unwrap(),
            "--json",
        ],
    );

    let second = run_json(&data, &["index", "--json"]);
    assert!(second["reused_vectors"].as_u64().unwrap() > 0);
    assert!(second["embedded_texts"].as_u64().unwrap() > 0);
    assert!(
        second["embedded_texts"].as_u64().unwrap() < second["written_documents"].as_u64().unwrap()
    );
}

#[test]
fn failed_rebuild_keeps_the_committed_generation_and_returns_json_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    run_json(&data_dir, &["index", "--json"]);
    let current_before = std::fs::read(data_dir.join("semantic/CURRENT")).expect("CURRENT before");
    let doctor_before = run_json(&data_dir, &["doctor", "--json"]);
    let config = dir.path().join("bad.toml");
    std::fs::write(&config, "vector_dimension = 0\n").expect("bad config");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["index", "--full", "--json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"ok\": false"))
        .stdout(predicate::str::contains(
            "embedding dimension must be nonzero",
        ));

    assert_eq!(
        std::fs::read(data_dir.join("semantic/CURRENT")).unwrap(),
        current_before
    );
    let doctor_after = run_json(&data_dir, &["doctor", "--json"]);
    assert_eq!(
        doctor_after["freshness"]["last_index"],
        doctor_before["freshness"]["last_index"]
    );
}

#[test]
fn doctor_reports_a_corrupt_generation_pointer_instead_of_hiding_it_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    run_json(&data_dir, &["index", "--json"]);
    std::fs::write(data_dir.join("semantic/CURRENT"), "../outside\n").expect("corrupt CURRENT");

    let doctor = run_json(&data_dir, &["doctor", "--json"]);
    assert_eq!(
        doctor["freshness"]["recommendation"]["index_before_semantic_search"],
        true
    );
    assert!(doctor["freshness"]["recommendation"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("CURRENT") && reason.contains("corrupt")));
}

#[test]
fn bm25_treats_fts_punctuation_as_literal_user_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let fixture = dir.path().join("special.jsonl");
    std::fs::write(&fixture, "{\"account_hash\":\"acct-synthetic\",\"chat_id\":\"chat-special\",\"chat_name\":\"Synthetic Special\",\"chat_type\":\"group\",\"message_id\":\"special-1\",\"sender_id\":\"u1\",\"sender_nickname\":\"민지\",\"timestamp\":\"2026-01-01T09:00:00Z\",\"text\":\"NTRU+ SMAUG-T Golden KAT exact phrase\",\"message_type\":\"text\",\"reply_to_message_id\":null}\n").expect("fixture");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture.to_str().unwrap(),
            "--json",
        ],
    );

    for query in [
        "NTRU+ SMAUG-T Golden KAT",
        "\"Golden\" KAT",
        "Golden: KAT",
        "Golden* KAT",
    ] {
        Command::cargo_bin("katok")
            .expect("katok binary")
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["search", "bm25", query, "--json"])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"chat_id\": \"chat-special\""));
    }
}

#[test]
fn concurrent_index_writers_fail_loudly_instead_of_publishing_two_generations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture("replies.jsonl").to_str().unwrap(),
            "--json",
        ],
    );
    let semantic = data_dir.join("semantic");
    std::fs::create_dir_all(&semantic).expect("semantic dir");
    let lock = rusqlite::Connection::open(semantic.join("index-writer.sqlite3")).expect("lock db");
    lock.execute_batch("BEGIN EXCLUSIVE")
        .expect("exclusive lock");

    let mut command = Command::cargo_bin("katok").expect("katok binary");
    let output = command
        .env("KATOK_EMBEDDER", "local-test")
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["index", "--json"])
        .output()
        .expect("run index");
    assert!(!output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).expect("json error");
    assert!(json["error"]["cause"]
        .as_str()
        .is_some_and(|cause| cause.contains("already running")));
    assert!(!semantic.join("CURRENT").exists());
}

#[test]
fn cross_room_search_can_expand_beyond_the_global_default_top_ten_and_group_by_chat_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let fixture = dir.path().join("many-rooms.jsonl");
    let rows = (0..12)
        .map(|room| format!("{{\"account_hash\":\"acct-synthetic\",\"chat_id\":\"chat-{room:02}\",\"chat_name\":\"Synthetic Room {room:02}\",\"chat_type\":\"group\",\"message_id\":\"m-{room:02}\",\"sender_id\":\"u1\",\"sender_nickname\":\"민지\",\"timestamp\":\"2026-01-01T09:{room:02}:00Z\",\"text\":\"다중방인물 검색\",\"message_type\":\"text\",\"reply_to_message_id\":null}}\n"))
        .collect::<String>();
    std::fs::write(&fixture, rows).expect("fixture");
    run_json(
        &data_dir,
        &[
            "sync",
            "--source",
            "fixture",
            fixture.to_str().unwrap(),
            "--json",
        ],
    );

    let default_hits = run_json(&data_dir, &["search", "keyword", "다중방인물", "--json"]);
    assert_eq!(default_hits.as_array().map(Vec::len), Some(10));
    let expanded = run_json(
        &data_dir,
        &[
            "search",
            "keyword",
            "다중방인물",
            "--limit",
            "100",
            "--json",
        ],
    );
    let chat_ids = expanded
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|hit| hit["chat_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(chat_ids.len(), 12);
}
