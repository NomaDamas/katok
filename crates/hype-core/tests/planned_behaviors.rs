use hype_core::{
    archive::Archive,
    chunking::rebuild_chunks,
    fixture::read_fixture,
    search::{bm25_search, keyword_search},
    semantic::{semantic_search, write_semantic_documents},
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
    assert_eq!(semantic[0].chunk_id, "chunk_2aeac4db0a04ceb2");
}
