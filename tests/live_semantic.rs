use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn live_semantic_doctor_json_reports_embeddinggemma_q4_local_defaults_when_unconfigured() {
    let dir = tempfile::tempdir().expect("create tempdir");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("HOME", dir.path())
        .args([
            "--data-dir",
            dir.path().to_str().expect("utf8 data"),
            "doctor",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"model\": \"embeddinggemma-300m-q4\"",
        ))
        .stdout(predicate::str::contains("\"dimension\": 768"))
        .stdout(predicate::str::contains("\"provider\": \"local\""))
        .stdout(predicate::str::contains("\"endpoint\": null"))
        .stdout(predicate::str::contains("\"status\": \"not_checked\""))
        .stdout(predicate::str::contains("jina").not())
        .stdout(predicate::str::contains("tei").not())
        .stdout(predicate::str::contains("2048").not());
}

#[test]
fn live_semantic_cli_indexes_local_embeddings_and_searches_without_endpoint() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
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
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "index",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"embedder\": \"embeddinggemma/local-test\"",
        ))
        .stdout(predicate::str::contains("\"vectorstore\": \"local\""))
        .stdout(predicate::str::contains("\"embedding_calls\": 1"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "index",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"embedding_calls\": 0"))
        .stdout(predicate::str::contains("\"embedded_texts\": 0"));

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("KATOK_EMBEDDER", "local-test")
        .args([
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "search",
            "semantic",
            "회의 보고서",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("chunk_2aeac4db0a04ceb2"));

    let generation = std::fs::read_to_string(data_dir.join("semantic/CURRENT"))
        .expect("read current generation");
    let generation = data_dir
        .join("semantic/generations")
        .join(generation.trim());
    assert!(generation.join("store").exists());
    assert!(generation.join("cursor.json").exists());
}

#[test]
fn live_semantic_cli_rejects_stale_remote_embedding_endpoint_config() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(
        &config,
        "embedder_base_url = \"https://example.com\"\nvector_dimension = 768\n",
    )
    .expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "index",
            "--json",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "unknown field `embedder_base_url`",
        ));
}

#[test]
fn loopback_http_provider_is_configurable_and_records_manifest_metadata() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(
        &config,
        r#"embedding_provider = "loopback-http"
embedding_endpoint = "http://127.0.0.1:9/embed"
embedding_timeout_ms = 100
embedding_query_prefix = "query: "
embedding_passage_prefix = "passage: "
"#,
    )
    .expect("write config");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            dir.path().join("data").to_str().expect("utf8 data"),
            "doctor",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"provider\": \"loopback-http\""))
        .stdout(predicate::str::contains(
            "\"endpoint\": \"http://127.0.0.1:9/embed\"",
        ));
}

#[test]
fn loopback_http_provider_rejects_non_loopback_endpoints() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("katok.toml");
    std::fs::write(
        &config,
        "embedding_provider = \"loopback-http\"\nembedding_endpoint = \"https://example.com/embed\"\n",
    )
    .expect("write config");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            dir.path().join("data").to_str().expect("utf8 data"),
            "doctor",
            "--json",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "embedding endpoint must be loopback",
        ));
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kakao")
        .join(name)
}
