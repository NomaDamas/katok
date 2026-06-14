use katok_core::{
    archive::Archive,
    chunking::rebuild_chunks,
    fixture::read_fixture,
    search::{bm25_search, keyword_search},
    semantic::{semantic_search, write_semantic_documents},
    types::RawMessage,
};

#[test]
fn same_sender_reply_and_search_behaviors_when_fixture_is_indexed() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let archive_path = dir.path().join("archive.sqlite3");
    let archive = Archive::open(&archive_path).expect("open archive");
    let fixture = format!(
        "{}/../../tests/fixtures/kakao/replies.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let messages = read_fixture(&fixture).expect("read fixture");

    archive.sync_messages(&messages).expect("sync messages");
    rebuild_chunks(&archive).expect("rebuild chunks");
    write_semantic_documents(&archive, &dir.path().join("semantic"))
        .expect("write semantic documents");

    let child = archive
        .get_chunk("chunk_caaaca07be83adf8")
        .expect("get chunk")
        .expect("known child chunk");
    assert_eq!(child.parent_chunk_ids, vec!["chunk_2aeac4db0a04ceb2"]);
    assert_eq!(child.message_count, 1);

    let keyword = keyword_search(&archive, "보고서", 10).expect("keyword search");
    assert_eq!(keyword[0].chunk_id, "chunk_2aeac4db0a04ceb2");
    assert!(keyword[0].snippet.chars().count() <= 80);

    let bm25 = bm25_search(&archive, "보고서", 10).expect("bm25 search");
    assert_eq!(bm25[0].ranker, "bm25");

    let semantic = semantic_search(&archive, &dir.path().join("semantic"), "회의 보고서", 10)
        .expect("semantic search");
    assert_eq!(semantic[0].unit, "parent_window");
    assert!(semantic[0]
        .child_chunk_ids
        .contains(&"chunk_2aeac4db0a04ceb2".to_string()));
}

#[test]
fn parent_windows_group_same_chat_messages_across_senders_when_fixture_is_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("archive.sqlite3");
    let archive = Archive::open(&archive_path).expect("open archive");
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kakao/replies.jsonl");
    let messages = read_fixture(&fixture_path).expect("read fixture");

    archive.sync_messages(&messages).expect("sync messages");
    rebuild_chunks(&archive).expect("rebuild chunks");

    let child = archive
        .get_chunk("chunk_caaaca07be83adf8")
        .expect("get child")
        .expect("known child");
    assert_eq!(child.window_parent_ids.len(), 1);

    let parent = archive
        .get_parent_chunk(&child.window_parent_ids[0])
        .expect("get parent")
        .expect("known parent");
    assert_eq!(parent.child_chunk_ids.len(), 2);
    assert!(parent.text.contains("[민지] 보고서 초안 올렸어요"));
    assert!(parent.text.contains("[준호] 회의 전에 확인할게요"));
    assert!(parent.text.len() <= katok_core::chunking::DEFAULT_PARENT_WINDOW_MAX_CHARS);
}

#[test]
fn parent_windows_cap_single_large_child_when_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("archive.sqlite3");
    let archive = Archive::open(&archive_path).expect("open archive");
    let long_text = format!(
        "{}꼬리검색",
        "가".repeat(katok_core::chunking::DEFAULT_PARENT_WINDOW_MAX_CHARS + 256)
    );
    let message = RawMessage {
        account_hash: "acct".to_string(),
        chat_id: "chat-large".to_string(),
        chat_name: "Large Fixture".to_string(),
        chat_type: "group".to_string(),
        message_id: "large-1".to_string(),
        sender_id: "sender-1".to_string(),
        sender_nickname: "민지".to_string(),
        timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-01T09:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc),
        text: long_text,
        message_type: "text".to_string(),
        reply_to_message_id: None,
    };

    archive.sync_messages(&[message]).expect("sync message");
    rebuild_chunks(&archive).expect("rebuild chunks");

    let parents = archive.all_parent_chunks().expect("load parents");
    assert!(parents.len() > 1);
    assert!(parents.iter().all(|parent| {
        parent.text.chars().count() <= katok_core::chunking::DEFAULT_PARENT_WINDOW_MAX_CHARS
    }));
    assert!(parents
        .iter()
        .map(|parent| parent.text.as_str())
        .collect::<Vec<_>>()
        .join("")
        .contains("꼬리검색"));

    write_semantic_documents(&archive, &dir.path().join("semantic"))
        .expect("write semantic documents");
    let semantic = semantic_search(&archive, &dir.path().join("semantic"), "꼬리검색", 10)
        .expect("semantic search");
    let child_id = archive.all_chunks().expect("chunks")[0].chunk_id.clone();
    assert!(semantic[0].child_chunk_ids.contains(&child_id));
}

#[test]
fn semantic_document_rebuild_prunes_stale_parent_markdown_when_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("archive.sqlite3");
    let archive = Archive::open(&archive_path).expect("open archive");
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kakao/replies.jsonl");
    let messages = read_fixture(&fixture_path).expect("read fixture");

    archive.sync_messages(&messages).expect("sync messages");
    rebuild_chunks(&archive).expect("rebuild chunks");
    let semantic_dir = dir.path().join("semantic");
    let source_dir = semantic_dir.join("source").join("chunks");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join("window_stale.md"), "stale searchable tail")
        .expect("write stale doc");

    write_semantic_documents(&archive, &semantic_dir).expect("write semantic documents");

    assert!(!source_dir.join("window_stale.md").exists());
    let stale = semantic_search(&archive, &semantic_dir, "stale", 10).expect("semantic search");
    assert!(stale.is_empty());
}
