# katok

`katok` is a local-first KakaoTalk memory CLI for macOS. The short command is `katok`.

It reads KakaoTalk messages through a source adapter, normalizes them into a private local archive, builds Kakao-aware chunks, and exposes keyword, BM25, semantic, and chunk-id lookup commands from the shell.

## Why "katok"?

"카톡" (katok) is the everyday Korean nickname for KakaoTalk, so the name says exactly what the tool is for: searching your own KakaoTalk history. Everything runs locally — `katok` helps you recover usable memory from KakaoTalk without uploading private chat history.

## Current CLI

```bash
katok doctor --json
katok source chats --source fixture tests/fixtures/kakao/replies.jsonl --json
katok sync --source fixture tests/fixtures/kakao/replies.jsonl --json
katok sync --source macos --json
katok sync --json                    # uses source_adapter from config
katok index --json
katok search keyword "보고서" --json
katok search bm25 "보고서" --json
katok search semantic "회의 보고서" --json
katok chunk get <chunk-id> --json
katok wipe-index --yes --json
```

## 빠른 시작 (macOS 카카오톡)

```bash
cargo build --workspace
cargo run -p katok-cli -- doctor --json
cargo run -p katok-cli -- sync --source macos --json
cargo run -p katok-cli -- search keyword "검색어" --json
```

- 먼저 macOS 설정에서 터미널 앱에 **전체 디스크 접근 권한**을 주세요.
- `doctor`는 카카오톡 앱/컨테이너/DB 파일 개수/인증 캐시 여부만 보여줍니다. 대화 내용은 출력하지 않습니다.
- `sync --source macos`는 로컬 Mac에 저장된 카카오톡 DB만 읽습니다. 서버 업로드나 원격 API 호출은 없습니다.
- 검색 결과의 snippet은 짧게 유지됩니다. 긴 원문 확인은 사용자가 명시적으로 `katok chunk get <chunk-id>`를 실행할 때만 합니다.

## macOS Source

`katok` reads the live KakaoTalk macOS installation directly in Rust — no Python, no `kakaocli`, and no external tooling at runtime.

```bash
katok sync --source macos --json
# or, with source_adapter = "macos" in katok.toml:
katok sync --json
```

Requirements:

- The terminal running `katok` must have macOS **Full Disk Access** (System Settings → Privacy & Security → Full Disk Access) to read files under `~/Library/Containers/com.kakao.KakaoTalkMac/`.
- Messages from a chat must have been opened or synced inside the KakaoTalk app — only locally present DB records are readable.
- On first sync, `katok` spends a few seconds recovering the account identifier from the encrypted SQLCipher database and then caches only `{user_id, uuid}` at mode `0600` under the data directory. The key material itself is never persisted.

`katok doctor --json` reports macOS readiness (booleans and counts only, no private content) under `.source_adapter.macos`.

## Chunk Strategy

`katok` owns canonical chat chunking.

- Consecutive messages by the same nickname stay in one canonical chunk.
- A large time gap starts a new chunk even when the nickname is unchanged.
- Default thresholds are 10 minutes for group chats and 30 minutes for direct chats.
- Reply metadata is stored as parent chunk references when the parent message is indexed.
- `katok search ...` returns minimal snippets and metadata.
- `katok chunk get <chunk-id>` is the explicit command for full chunk content.

Semantic indexing writes deterministic local documents that map back to canonical chunk ids. This keeps MinSync/vector search from redefining chat boundaries.

## Search

`katok search keyword` performs deterministic local matching over canonical chunks.

`katok search bm25` uses SQLite FTS5 BM25 ranking over the same chunk archive.

`katok index` uses MinSync with a LanceDB vector store and a loopback Jina/TEI-compatible embedding server by default. The default model id is `tei:jinaai/jina-embeddings-v4` with a 2048-dimensional vector store. Use `jinaai/jina-embeddings-v3` only as a documented fallback if v4 cannot be served acceptably on the user's Mac.

Example local semantic config:

```toml
embedder_model = "tei:jinaai/jina-embeddings-v4"
embedder_base_url = "http://127.0.0.1:8080"
embedding_batch_size = 64
vector_dimension = 2048
minsync_dir = "semantic"
allow_remote_embeddings = false
```

For synthetic tests and offline CLI checks, `KATOK_EMBEDDER=mock katok index --json` keeps using the deterministic mock bridge. Remote embedding endpoints are rejected unless `allow_remote_embeddings = true` is set explicitly.

Remote embedding or LLM APIs are not enabled by default and must be explicit opt-in.

## Privacy

Everything produced by this project is sensitive:

- KakaoTalk DB paths and SQLCipher keys
- normalized message archives
- generated semantic documents
- embedding caches and vector indexes
- search evidence and logs

Generated stores are ignored by git and should live under a user-only data directory. Automated tests use synthetic chat fixtures only. Real KakaoTalk smoke tests are manual-only and must not print private message content unless the user explicitly asks for that exact output.

## Related Projects

Relevant public projects found during the planning survey:

- `silver-flight-group/kakaocli`: macOS local DB read/search/sync CLI.
- `JungHoonGhae/openkakao-cli`: local DB read/search plus LOCO-oriented flows.
- `xistoh162108/kakaotalk_analyzer`: export CSV analysis with embedding and SPLADE ideas.
- `teddylee777/kakaotalk-gpt`: export txt/csv RAG with FAISS/Chroma retrievers.
- `sanggubot/doppelganger-gpt`: KakaoTalk txt to Chroma example.
- `uoneway/kakaotalk_msg_preprocessor`: exported txt parser.
- `claudianus/kakaotalk-chat-analyzer`: CSV export to anonymized HTML report.
- `NomaDamas/MinSync`: Rust manifest-based incremental vector DB indexing CLI using LanceDB and local TEI support.

No complete project was found that continuously turns the macOS KakaoTalk local DB into a private local archive plus keyword, BM25, and semantic chunk search.

## Development

```bash
cargo fmt --all -- --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Do not add real KakaoTalk exports, SQLCipher keys, auth caches, embeddings, indexes, or local archives to this repository.
