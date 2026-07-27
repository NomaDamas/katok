---
name: katok
description: Search local KakaoTalk keyword, BM25, and EmbeddingGemma vector indexes through the katok CLI, list rooms, export a room's chunks, and extract image attachments.
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
katok index --full --json                 # full rebuild instead of incremental
katok wipe-index --yes                    # drops the local index
katok source chats --source macos --json  # lists rooms with their chat ids
katok search keyword "검색어" --json
katok search bm25 "검색어" --json
katok search semantic "지난 회의 보고서" --json
katok chunk get <chunk-id> --json
katok chunk get <chunk-id> --redact --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
katok chunks --chat <chat-id> --json      # every chunk in one room (metadata only)
katok media get --chat <chat-id> --out <dir> --json   # extracts image attachments
katok media get --chat <chat-id> --out <dir> --no-cdn --json   # local tiers only
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

The index never follows the KakaoTalk database on its own, so a search only ever reflects the last sync. Run `katok sync --source macos --json` before the first query of a session and before any question about recent messages. Skipping it does not return zero results, it silently returns a stale set. Freshness also depends on KakaoTalk itself running (`pgrep -x KakaoTalk`), because the source database receives new messages only while the app is up.

Sync is cheap enough to run often: on a 400k-message archive a quiet sync takes a few seconds, because only the chats whose messages changed have their chunks recomputed. The first sync on an empty archive is the exception and takes far longer. The payload reports `rebuilt_chats` and a `timings_ms` breakdown (`read_source`, `upsert_messages`, `rebuild_chunks`), so a slow run can be attributed to a stage instead of guessed at.

## Field Notes

- Most group rooms come back as a `chat-<id>` placeholder rather than a real title, because the source database does not always carry the room name. Grepping room names will not find them.
- To locate a room whose title is a placeholder, search for a term used inside it and group the hits by `chat_name`. The `chat_id` holding the most hits is that room. Each hit carries `chunk_id`, `chat_name`, `sender_nickname`, `started_at`, and `snippet`.
- `katok chunks --chat <chat-id>` returns chunk metadata only, never `text`. To export a whole room, collect the chunk ids and run `katok chunk get` per chunk, then assemble in timestamp order.
- `katok chunk get --redact` masks the entire `text`, not only the PII inside it. Use it for a line you must quote, not for a readable export.
- KakaoTalk system feed entries (invite, join, leave) arrive as JSON strings such as `{"inviter":...}` or `{"member":...,"feedType":N}`. Filter them out of anything a person will read.

## Image Attachments

`katok media get --chat <chat-id> --out <dir>` resolves each image message through four tiers in order: the decrypted local `.img` cache, a GET of the attachment's own presigned CDN URL verified by SHA-1, the decrypted `.thm` thumbnail, and finally a metadata stub. That CDN GET is the only network access anywhere in the CLI, and `--no-cdn` disables it so resolution stays entirely local.

## Related Skills

`katok send` is documented separately in `skills/katok-send`. It is the only subcommand that writes, and it does so by driving the running app's UI rather than the local archive.

## Platform

Assume Apple Silicon macOS. Intel macOS is not a supported target for the packaged local EmbeddingGemma path.
