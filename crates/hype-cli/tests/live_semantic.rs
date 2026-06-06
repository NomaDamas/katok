use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

#[test]
fn cli_indexes_live_jina_embeddings_into_lancedb_and_semantic_searches() {
    let server = FakeJina::start();
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("hype.toml");
    std::fs::write(
        &config,
        format!(
            "embedder_base_url = \"{}\"\nvector_dimension = 3\nminsync_dir = \"live-semantic\"\n",
            server.base_url
        ),
    )
    .expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("hype")
        .expect("hype binary")
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

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
            "--data-dir",
            data_dir.to_str().expect("utf8 data"),
            "index",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"vectorstore\": \"lancedb\""))
        .stdout(predicate::str::contains("\"embedding_calls\": 1"));

    Command::cargo_bin("hype")
        .expect("hype binary")
        .args([
            "--config",
            config.to_str().expect("utf8 config"),
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

    assert!(data_dir.join("live-semantic/store").exists());
    assert!(data_dir.join("live-semantic/cursor.json").exists());
    assert!(server.request_count.load(Ordering::SeqCst) >= 2);
}

#[test]
fn cli_rejects_remote_embedding_endpoint_without_opt_in() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = dir.path().join("hype.toml");
    std::fs::write(
        &config,
        "embedder_base_url = \"https://example.com\"\nvector_dimension = 3\n",
    )
    .expect("write config");
    let data_dir = dir.path().join("data");

    Command::cargo_bin("hype")
        .expect("hype binary")
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

    Command::cargo_bin("hype")
        .expect("hype binary")
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
        .stderr(predicate::str::contains(
            "remote embedding endpoints are disabled",
        ));
}

struct FakeJina {
    base_url: String,
    request_count: Arc<AtomicUsize>,
}

impl FakeJina {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake jina");
        let addr = listener.local_addr().expect("fake jina addr");
        let request_count = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&request_count);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                handle_embed(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            request_count,
        }
    }
}

fn handle_embed(mut stream: TcpStream) {
    let mut buffer = [0_u8; 8192];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
    let payload = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
    let inputs = payload
        .get("inputs")
        .or_else(|| payload.get("input"))
        .and_then(serde_json::Value::as_array)
        .map_or(1, std::vec::Vec::len);
    let query_text = payload
        .get("inputs")
        .or_else(|| payload.get("input"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let vector = if query_text.contains("회의") || query_text.contains("보고서") {
        "[1.0,0.0,0.0]"
    } else {
        "[0.0,1.0,0.0]"
    };
    let embeddings = std::iter::repeat_n(vector, inputs)
        .collect::<Vec<_>>()
        .join(",");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n[{}]",
        embeddings.len() + 2,
        embeddings
    );
    let _ = stream.write_all(response.as_bytes());
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kakao")
        .join(name)
}
