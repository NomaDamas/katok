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
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
katok wipe-index --yes --json
```

## 빠른 시작 (macOS 카카오톡)

Homebrew:

```bash
brew install NomaDamas/katok/katok
katok doctor --json
katok sync --source macos --json
katok index --json
katok search semantic "검색어" --json
```

Cargo:

```bash
cargo install katok
katok doctor --json
```

- 먼저 macOS 설정에서 터미널 앱에 **전체 디스크 접근 권한**을 주세요.
- `doctor`는 카카오톡 앱/컨테이너/DB 파일 개수/인증 캐시 여부만 보여줍니다. 대화 내용은 출력하지 않습니다.
- `sync --source macos`는 로컬 Mac에 저장된 카카오톡 DB만 읽습니다. 서버 업로드나 원격 API 호출은 없습니다.
- `index`는 기본값으로 앱 프로세스 안에서 `embeddinggemma-300m` q4 ONNX 모델을 실행합니다. Python 서버, TEI 서버, Jina 서버를 따로 띄우지 않습니다.
- 검색 결과의 snippet은 짧게 유지됩니다. 긴 원문 확인은 사용자가 명시적으로 `katok chunk get <chunk-id>`를 실행할 때만 합니다.

권한 설정을 처음부터 안내하려면 `scripts/katok-macos-setup.sh`를 실행하세요. 자세한 흐름은 [macOS first-run setup](docs/macos-first-run.md)에 정리되어 있습니다.

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
- Window parent chunks group canonical chunks from the same `chat_id` across speakers when they occur within 5 minutes of each other.
- Window parent chunks are capped to fit the local EmbeddingGemma indexing context; overlarge windows split before indexing.
- `katok search ...` returns minimal snippets and metadata.
- `katok chunk get <chunk-id>` is the explicit command for full chunk content.
- `katok chunk context <chunk-id>` returns the immediate previous/next canonical chunk in the same chat plus the window parent chunk.
- `katok chunk parent <chunk-id>` returns the larger window parent chunk for quick child-to-parent navigation.

Semantic indexing writes local documents and a local vector index for window parent chunks. Search hits include `unit = "parent_window"` and `child_chunk_ids`, so agents can search at the larger context level and then jump back to exact canonical chunks.

## Search

`katok search keyword` performs deterministic local matching over canonical chunks.

`katok search bm25` uses SQLite FTS5 BM25 ranking over the same chunk archive.

`katok index` uses an in-process local embedder by default: `embeddinggemma-300m-q4` through `fastembed`/ONNX Runtime. The first run downloads the model artifact into the Hugging Face / fastembed cache, then later runs reuse the local cache. No Python process, TEI server, Jina server, or local HTTP endpoint is required.

Semantic search indexes window parent chunks, not individual micro chunks. Keyword and BM25 search still operate over canonical micro chunks. This keeps exact lookup small while semantic search has enough conversational context across speakers.

Example semantic config:

```toml
embedder_model = "embeddinggemma-300m-q4"
embedding_batch_size = 64
vector_dimension = 768
semantic_dir = "semantic"
```

For synthetic tests and offline CLI checks, `KATOK_EMBEDDER=mock katok index --json` keeps using the deterministic mock bridge. `KATOK_EMBEDDER=local-test` exercises the local vector-index path with deterministic vectors and no model download.

Remote embedding endpoints are not supported by the CLI path. Stale `embedder_base_url` or `allow_remote_embeddings` config is rejected so `katok index` stays zero-config and local by default.

## Privacy

Everything produced by this project is sensitive:

- KakaoTalk DB paths and SQLCipher keys
- normalized message archives
- generated semantic documents
- embedding caches and vector indexes
- search evidence and logs

Generated stores are ignored by git and should live under a user-only data directory. Automated tests use synthetic chat fixtures only. Real KakaoTalk smoke tests are manual-only and must not print private message content unless the user explicitly asks for that exact output.

## Related Projects

Relevant public projects found during the planning survey. These are references only; the active semantic path uses the local SQLite vector store described above.

- `silver-flight-group/kakaocli`: macOS local DB read/search/sync CLI.
- `JungHoonGhae/openkakao-cli`: local DB read/search plus LOCO-oriented flows.
- `xistoh162108/kakaotalk_analyzer`: export CSV analysis with embedding and SPLADE ideas.
- `teddylee777/kakaotalk-gpt`: export txt/csv RAG with FAISS/Chroma retrievers.
- `sanggubot/doppelganger-gpt`: KakaoTalk txt to Chroma example.
- `uoneway/kakaotalk_msg_preprocessor`: exported txt parser.
- `claudianus/kakaotalk-chat-analyzer`: CSV export to anonymized HTML report.
No complete project was found that continuously turns the macOS KakaoTalk local DB into a private local archive plus keyword, BM25, and semantic chunk search.

## Development

```bash
cargo fmt --all -- --check
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
python3 scripts/verify_release_config.py
```

Do not add real KakaoTalk exports, SQLCipher keys, auth caches, embeddings, indexes, or local archives to this repository.
