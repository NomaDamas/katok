---
name: katok
description: Search local katok KakaoTalk memory indexes through the CLI.
---

# katok Skill

Use the `katok` CLI as the only execution surface. This skill stays thin: it explains privacy behavior, calls CLI commands, and summarizes results.

## Privacy Rules

- Do not inspect local database internals from the skill.
- Do not handle auth caches or decryption material.
- Search commands return minimal snippets by default.
- Full chunk content is shown only when the user explicitly asks for an exact result or provides a chunk id.

## Commands

```bash
katok doctor --json
katok sync --source macos --json          # reads live macOS KakaoTalk (needs Full Disk Access)
katok sync --source fixture tests/fixtures/kakao/replies.jsonl --json
katok sync --json                         # uses source_adapter from config
katok search keyword "검색어" --json
katok search bm25 "검색어" --json
katok index --json
katok search semantic "지난 회의 보고서" --json
katok chunk get <chunk-id> --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
```

`--source macos` reads the live macOS KakaoTalk SQLCipher database locally in Rust; the terminal must have Full Disk Access to `~/Library/Containers/com.kakao.KakaoTalkMac/`.

Prefer `katok search ...` for discovery and `katok chunk get ...` only for explicit retrieval.

Use `katok chunk context <chunk-id> --json` to inspect the immediate previous and next micro chunks in the same chat. Use `katok chunk parent <chunk-id> --json` to jump from a micro chunk to its larger 5-minute same-chat window parent chunk. Semantic search returns parent-window hits with `child_chunk_ids`; use these chunk commands to navigate from broad context back to exact messages.

`katok index` runs the local `embeddinggemma-300m-q4` embedder in-process by default. Do not ask the user to start a Python, Jina, TEI, or local HTTP embedding server. Use `KATOK_EMBEDDER=mock` only for synthetic QA and `KATOK_EMBEDDER=local-test` only when you need deterministic local vector tests without downloading the model.
