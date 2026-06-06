# Hydrogen Peroxide (`hype`)

Hydrogen Peroxide is a local-first KakaoTalk memory CLI for macOS. The short command is `hype`.

It reads KakaoTalk messages through a source adapter, normalizes them into a private local archive, builds Kakao-aware chunks, and exposes keyword, BM25, semantic, and chunk-id lookup commands from the shell.

## Why Hydrogen Peroxide?

KakaoTalk is named after cacao, and cacao is the chocolate ingredient that can poison a dog. The joke behind this project is that KakaoTalk's personal-history access is so locked down that the user feels like the dog in trouble. Hydrogen peroxide is the emergency antidote in the story: `hype` is the local tool that helps recover usable memory from KakaoTalk without uploading private chat history.

## Current CLI

```bash
hype doctor --json
hype source chats --source fixture tests/fixtures/kakao/replies.jsonl --json
hype sync --source fixture tests/fixtures/kakao/replies.jsonl --json
hype index --json
hype search keyword "보고서" --json
hype search bm25 "보고서" --json
hype search semantic "회의 보고서" --json
hype chunk get <chunk-id> --json
hype wipe-index --yes --json
```

The current automated adapter is a synthetic JSONL fixture adapter. The first real macOS source adapter should call `kakaocli` or the `k-skill` `kakaotalk-mac` helper for read-only JSON output rather than reimplementing SQLCipher database reverse engineering.

## Chunk Strategy

Hydrogen Peroxide owns canonical chat chunking.

- Consecutive messages by the same nickname stay in one canonical chunk.
- A large time gap starts a new chunk even when the nickname is unchanged.
- Default thresholds are 10 minutes for group chats and 30 minutes for direct chats.
- Reply metadata is stored as parent chunk references when the parent message is indexed.
- `hype search ...` returns minimal snippets and metadata.
- `hype chunk get <chunk-id>` is the explicit command for full chunk content.

Semantic indexing writes deterministic local documents that map back to canonical chunk ids. This keeps MinSync/vector search from redefining chat boundaries.

## Search

`hype search keyword` performs deterministic local matching over canonical chunks.

`hype search bm25` uses SQLite FTS5 BM25 ranking over the same chunk archive.

`hype search semantic` currently uses the local semantic document bridge in tests. The intended production backend is MinSync with LanceDB and a local Jina embedding server. The target default is `jinaai/jina-embeddings-v4`; document `jinaai/jina-embeddings-v3` as a fallback if v4 cannot be served acceptably on the user's Mac.

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
