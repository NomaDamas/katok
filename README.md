# Kakao Memory

Local-first semantic memory for KakaoTalk on macOS.

Kakao Memory turns a user's KakaoTalk conversations into a private, searchable local memory store. The first target is the macOS KakaoTalk app: read messages through the existing local-database access path, normalize them into a durable local archive, and provide keyword and semantic search through a CLI and agent skill.

## Why

Kakao's official APIs do not expose personal chat history. Existing public projects mostly fall into two buckets:

- macOS local DB CLIs such as `kakaocli` and `openkakao-cli`, which can read and search local KakaoTalk data but do not provide a durable semantic index.
- export-file analyzers and RAG demos, which parse manually exported `.txt` or `.csv` files but do not continuously index the live local Mac database.

Kakao Memory aims to fill that gap:

```text
KakaoTalk local DB
  -> read-only ingestion
  -> normalized local archive
  -> keyword index + vector index
  -> CLI + agent skill search
```

## Product Shape

The project should be usable both directly from a shell and through an agent skill.

Initial CLI sketch:

```bash
kakao-memory doctor
kakao-memory index --since 30d
kakao-memory sync
kakao-memory search "회의"
kakao-memory semantic-search "지난달 세금 얘기한 대화"
kakao-memory inspect-message <message-id>
kakao-memory wipe-index
```

The skill wrapper should stay thin: explain permissions, call the CLI, summarize results, and never expose secrets or raw private content unless the user explicitly asks for the matching query result.

## Architecture

### Ingestion

Use a read-only source adapter rather than reimplementing KakaoTalk DB reverse engineering from scratch.

Candidate source adapters:

- `kakaocli` with `--json`, `--db`, and `--key`
- the `k-skill` `kakaotalk-mac` helper for auth recovery and cached DB/key resolution
- future adapters for manual `.txt` / `.csv` exports

### Archive

Store normalized messages in a local SQLite database:

- account/device identity hash, not raw account secrets
- chat id and display name
- message id/log id
- sender id and display name
- timestamp
- message type
- text
- source cursor for incremental indexing

### Search

Provide two search paths:

- keyword search over normalized text, preferably SQLite FTS5
- semantic search over chunks with metadata filters

The first semantic backend should be local-first. Remote embedding APIs can exist only as explicit opt-in.

### Privacy

Everything in this project is sensitive:

- KakaoTalk DB paths and SQLCipher keys
- normalized message archive
- embedding cache
- search result snippets
- logs and screenshots

Default behavior must keep data local, use restrictive file permissions, and avoid telemetry.

## Non-Goals

- No Kakao official API integration for personal chat history.
- No server upload by default.
- No protocol-level sending or bot automation in the first version.
- No multi-user hosted product until local privacy and deletion semantics are solid.

## Research Notes

Relevant public projects found during initial survey:

- `silver-flight-group/kakaocli`: macOS local DB read/search/sync CLI.
- `JungHoonGhae/openkakao-cli`: local DB read/search plus LOCO-oriented flows.
- `xistoh162108/kakaotalk_analyzer`: export CSV analysis with embedding and SPLADE ideas.
- `teddylee777/kakaotalk-gpt`: export txt/csv RAG with FAISS/Chroma retrievers.
- `sanggubot/doppelganger-gpt`: small KakaoTalk txt to Chroma example.
- `uoneway/kakaotalk_msg_preprocessor`: exported txt parser.
- `claudianus/kakaotalk-chat-analyzer`: CSV export to anonymized HTML report.

No complete project was found that continuously turns the macOS KakaoTalk local DB into a private filesystem-like archive plus semantic search index.

## Status

Concept repo. No production implementation yet.
