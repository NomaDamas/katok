---
name: hype
description: Search local Hydrogen Peroxide / hype KakaoTalk memory indexes through the CLI.
---

# Hydrogen Peroxide (`hype`) Skill

Use the `hype` CLI as the only execution surface. This skill stays thin: it explains privacy behavior, calls CLI commands, and summarizes results.

## Privacy Rules

- Do not inspect local database internals from the skill.
- Do not handle auth caches or decryption material.
- Search commands return minimal snippets by default.
- Full chunk content is shown only when the user explicitly asks for an exact result or provides a chunk id.

## Commands

```bash
hype doctor --json
hype sync --source fixture tests/fixtures/kakao/replies.jsonl --json
hype search keyword "검색어" --json
hype search bm25 "검색어" --json
hype index --json
hype search semantic "지난 회의 보고서" --json
hype chunk get <chunk-id> --json
```

Prefer `hype search ...` for discovery and `hype chunk get ...` only for explicit retrieval.

`hype index` expects a loopback Jina/TEI-compatible embedding server unless `HYPE_EMBEDDER=mock` is intentionally set for synthetic QA. Remote embedding endpoints require explicit config opt-in with `allow_remote_embeddings = true`.
