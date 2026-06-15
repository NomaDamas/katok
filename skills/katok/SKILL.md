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
export PATH="$HOME/.cargo/bin:$PATH"       # use when katok is not found after cargo install
katok doctor --json
katok permissions macos                   # opens Full Disk Access settings
katok permissions macos --accessibility   # also opens Accessibility settings
katok doctor --macos-probe --json        # explicit macOS permission/app-data probe
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

1. If `katok` is not found after install, run `export PATH="$HOME/.cargo/bin:$PATH"` and retry.
2. If macOS permission setup is needed, run `katok permissions macos` so the user can grant Full Disk Access in System Settings.
3. Run `katok doctor --json` before search to inspect freshness without triggering macOS app-data permission prompts.
4. Inspect the `freshness` section from `doctor --json` before search.
5. Run `katok sync --source macos --json` when `freshness.recommendation.sync_before_search` is `true`, when the user asks for recent messages, or when search freshness matters.
6. Run `katok index --json` before semantic search when `freshness.recommendation.index_before_semantic_search` is `true` or after a sync that should affect vector search.
7. Use `katok search keyword ...`, `katok search bm25 ...`, and `katok search semantic ...` for discovery.
8. Use `katok chunk get ...` only for explicit retrieval.
9. Run `katok doctor --macos-probe --json` only for setup or permission diagnostics, because it may trigger a macOS "access data from other apps" prompt.

`--source macos` reads the live macOS KakaoTalk SQLCipher database locally in Rust; the terminal must have Full Disk Access to `~/Library/Containers/com.kakao.KakaoTalkMac/`.

Use `katok chunk context <chunk-id> --json` to inspect the immediate previous and next micro chunks in the same chat. Use `katok chunk parent <chunk-id> --json` to jump from a micro chunk to its larger 5-minute same-chat window parent chunk. Semantic search returns parent-window hits with `child_chunk_ids`; use these chunk commands to navigate from broad context back to exact messages.

`katok index` runs the local `embeddinggemma-300m-q4` embedder in-process by default. Do not ask the user to start a Python, Jina, TEI, or local HTTP embedding server. Use `KATOK_EMBEDDER=mock` only for synthetic QA and `KATOK_EMBEDDER=local-test` only when you need deterministic local vector tests without downloading the model.

## Platform

Assume Apple Silicon macOS. Intel macOS is not a supported target for the packaged local EmbeddingGemma path.
