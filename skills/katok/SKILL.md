---
name: katok
description: Search local KakaoTalk keyword, BM25, and EmbeddingGemma vector indexes through the katok CLI.
---

# katok

Use the `katok` CLI as the only execution surface. This skill stays thin: it checks readiness, calls CLI search commands, retrieves explicit chunks when needed, and summarizes results.

## Privacy Rules

- Do not inspect local database internals from the skill.
- Do not handle auth caches or decryption material.
- Do not read KakaoTalk DB files directly. Use `katok sync --source macos --json`.
- Search commands return minimal snippets and chunk ids by default.
- Full chunk content is shown only when the user explicitly asks for an exact result, asks to open a result, or provides a chunk id.

## Commands

```bash
katok doctor --json
katok sync --source macos --json          # reads live macOS KakaoTalk (needs Full Disk Access)
katok sync --json                         # uses source_adapter from config
katok index --json                        # builds local EmbeddingGemma vector index
katok search keyword "검색어" --json
katok search bm25 "검색어" --json
katok search semantic "지난 회의 보고서" --json
katok chunk get <chunk-id> --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
```

For synthetic QA only:

```bash
katok sync --source fixture tests/fixtures/kakao/replies.jsonl --json
KATOK_EMBEDDER=local-test katok index --json
KATOK_EMBEDDER=mock katok index --json
```

## Operating Pattern

1. Run `katok doctor --json` when the user asks to set up or diagnose local KakaoTalk access.
2. Run `katok sync --source macos --json` before search if the local archive may be stale.
3. Run `katok index --json` before semantic search if vector search has not been built or may be stale.
4. Use `katok search keyword ...`, `katok search bm25 ...`, and `katok search semantic ...` for discovery.
5. Use `katok chunk get ...` only for explicit retrieval.

`--source macos` reads the live macOS KakaoTalk SQLCipher database locally in Rust; the terminal must have Full Disk Access to `~/Library/Containers/com.kakao.KakaoTalkMac/`.

Use `katok chunk context <chunk-id> --json` to inspect the immediate previous and next micro chunks in the same chat. Use `katok chunk parent <chunk-id> --json` to jump from a micro chunk to its larger 5-minute same-chat window parent chunk. Semantic search returns parent-window hits with `child_chunk_ids`; use these chunk commands to navigate from broad context back to exact messages.

`katok index` runs the local `embeddinggemma-300m-q4` embedder in-process by default. Do not ask the user to start a Python, Jina, TEI, or local HTTP embedding server. Use `KATOK_EMBEDDER=mock` only for synthetic QA and `KATOK_EMBEDDER=local-test` only when you need deterministic local vector tests without downloading the model.

## Platform

Assume Apple Silicon macOS. Intel macOS is not a supported target for the packaged local EmbeddingGemma path.
