use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

#[test]
fn cli_reads_kakaocli_chats_when_fake_read_only_binary_is_on_path() {
    let fake_bin = tempfile::tempdir().expect("create fake bin");
    write_fake_kakaocli(fake_bin.path());

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("PATH", fake_path(fake_bin.path()))
        .args(["source", "chats", "--source", "kakaocli", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("chat-kakao-fixture"));
}

#[test]
fn cli_syncs_kakaocli_messages_without_requiring_fixture_path() {
    let fake_bin = tempfile::tempdir().expect("create fake bin");
    write_fake_kakaocli(fake_bin.path());
    let data = tempfile::tempdir().expect("create data dir");
    let data_dir = data.path().to_str().expect("utf8 path");

    Command::cargo_bin("katok")
        .expect("katok binary")
        .env("PATH", fake_path(fake_bin.path()))
        .args([
            "--data-dir",
            data_dir,
            "sync",
            "--source",
            "kakaocli",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"inserted_messages\": 1"));
}

#[test]
fn cli_surfaces_kakaocli_failure_detail_instead_of_generic_message() {
    // Mirrors the real macOS failure: kakaocli is installed and runs, but
    // cannot decrypt the SQLCipher database, so it exits non-zero with a
    // diagnostic on stderr. katok must surface that detail rather than
    // claiming the binary is "not found or not configured".
    let fake_bin = tempfile::tempdir().expect("create fake bin");
    write_failing_kakaocli(fake_bin.path());

    let assert = Command::cargo_bin("katok")
        .expect("katok binary")
        .env("PATH", fake_path(fake_bin.path()))
        .args(["source", "chats", "--source", "kakaocli", "--json"])
        .assert()
        .failure();

    assert
        .stderr(predicate::str::contains("kakaocli chats failed"))
        .stderr(predicate::str::contains(
            "SQL error: prepare: file is not a database",
        ))
        // Must not regress to the old misleading message.
        .stderr(predicate::str::contains("not found or not configured").not())
        // Must not leak any message body, even on the error path.
        .stderr(predicate::str::contains("합성 카카오 메시지").not());
}

#[cfg(unix)]
fn write_failing_kakaocli(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("kakaocli");
    let mut file = std::fs::File::create(&script).expect("create failing kakaocli");
    writeln!(
        file,
        r#"#!/bin/sh
echo "Error: SQL error: prepare: file is not a database" 1>&2
exit 1
"#
    )
    .expect("write failing kakaocli");
    let mut perms = std::fs::metadata(&script)
        .expect("fake metadata")
        .permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(script, perms).expect("chmod failing kakaocli");
}

#[cfg(not(unix))]
fn write_failing_kakaocli(_dir: &std::path::Path) {
    panic!("kakaocli adapter tests require unix shell semantics");
}

#[cfg(unix)]
fn write_fake_kakaocli(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("kakaocli");
    let mut file = std::fs::File::create(&script).expect("create fake kakaocli");
    writeln!(
        file,
        r#"#!/bin/sh
case "$1" in
  chats)
    printf '%s\n' '[{{"chat_id":"chat-kakao-fixture","chat_name":"Synthetic Kakao","chat_type":"direct"}}]'
    ;;
  messages)
    printf '%s\n' '[{{"account_hash":"acct-kakao-fixture","chat_id":"chat-kakao-fixture","chat_name":"Synthetic Kakao","chat_type":"direct","message_id":"kakao-1","sender_id":"sender-1","sender_nickname":"테스터","timestamp":"2026-01-01T00:00:00Z","text":"합성 카카오 메시지","message_type":"text","reply_to_message_id":null,"source_cursor":"kakao-1"}}]'
    ;;
  *)
    exit 2
    ;;
esac
"#
    )
    .expect("write fake kakaocli");
    let mut perms = std::fs::metadata(&script)
        .expect("fake metadata")
        .permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(script, perms).expect("chmod fake kakaocli");
}

#[cfg(not(unix))]
fn write_fake_kakaocli(_dir: &std::path::Path) {
    panic!("kakaocli adapter tests require unix shell semantics");
}

fn fake_path(dir: &std::path::Path) -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", dir.display())
}
