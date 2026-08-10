# Changelog

## Unreleased

### Reading

- Add a read-only `katok inbox` command that finds explicit mentions of the
  account owner from native Kakao metadata and classifies them as `pending`,
  `review`, or directly `answered`. A later general self-authored message stays
  in review instead of being guessed complete.
- Backfill self/mention metadata into existing archives without rebuilding
  search chunks when message content did not change.

### Security

- Default builds are read-only. The macOS UI-based `katok send` command now
  requires an explicit `--features private-send` build.
- Reject non-HTTPS, credential-bearing, local-hostname, and non-public IP CDN
  URLs before a request, and disable redirects so a validated public URL cannot
  redirect to a local service.
- Restrict `wipe-index` to a canonical child of the katok data directory,
  including protection against symlink escapes.
- Create or repair the local plaintext archive with owner-only mode `0600`, and
  refuse archive paths that resolve through a symbolic link.
- Update `anyhow` and `crossbeam-epoch` to patched releases and audit locked
  dependencies in CI and release validation.
- Pin GitHub Actions to full commit SHAs and grant release write permissions
  only to the jobs that publish a release or update the Homebrew formula.

## 0.3.0 - 2026-08-02

### Sending

- The existing macOS Accessibility-based `katok send` command is enabled in
  default builds. Read-only installs remain available with
  `cargo install katok --no-default-features`.
- Text, image, and draft modes fail before stdin, archive, Accessibility, or
  KakaoTalk UI access unless `--accept-use-policy` is supplied. Read-only
  listing and non-delivery `--dry-run` remain available without acceptance.
- `katok send --list-windows` and `--list-rooms` no longer require a dummy
  `--room` or `--chat` target.
- `ACCEPTABLE_USE_POLICY.md` and `DISCLAIMER.md` prohibit spam, impersonation,
  account theft, stalking, harassment, post-refusal contact, bulk/repetitive
  sending, privacy violations, and access-control evasion.
- The documented network boundary is scoped to `katok send`: it drives the
  local official KakaoTalk app and makes no direct Kakao remote private
  protocol/API or HTTP/socket call. Other subcommands may use documented CDN
  or model-download paths.

## 0.2.0 - 2026-07-29

### Sending

- The private `katok send` source is excluded from default builds and requires the explicit `private-send` Cargo feature; no sending skill is shipped in the public 0.2.0 surface.
- `katok send --chat <chat-id>` addresses a room by id. Display names are not identifiers and may be shared by multiple rooms, so an ambiguous name is refused outright rather than guessed at; `--chat` resolves it through the room's position among same-named chats.
- Rooms are matched by member set rather than by string. KakaoTalk and the archive may list an unnamed group's members in different orders, so set comparison keeps the room reachable.
- The chat search box is now typed into with real keystrokes. Setting its accessibility value painted the text but told the app nothing had happened, so the list never filtered and the search strategy silently did nothing — leaving the unfiltered list, where a room far enough down sits outside the scroll area and was clicked at coordinates nobody could see.
- `--draft` leaves a message in the compose box for review. It pastes rather than writing the accessibility value, because writing that value *is* sending: KakaoTalk delivers on the change with no keystroke involved.
- A send that needs the screen now runs behind a full-screen curtain that blocks keyboard and mouse input, so it cannot collide with whoever is using the machine. Blocked input is dropped rather than queued, which is why the block is always visible. Esc or the cancel button stops the run.
- The previously active application and the clipboard are restored on every exit path, including failures, and a global keystroke is never posted unless KakaoTalk is confirmed frontmost at that instant — a collision now produces a clean failure instead of a paste into someone else's document.
- `--room` rejects control characters and invisible formatting marks, naming the codepoint, since those are quoting accidents upstream rather than missing rooms.
- `katok send` waits for a gap in the user's typing before taking focus, restores the previously active application when it is done, and no longer overwrites the clipboard: the existing contents are saved and put back. Added `--take-focus-now` to skip the wait and `--focus-wait` to bound it.
- Resolving a room takes focus once for the whole attempt sequence rather than once per attempt.

### Reading

- `katok media get` now extracts videos (message type 3) alongside photos and albums. Video bodies live in the `.vid` cache under a `v`-prefixed key stem, and the output extension is sniffed so ISO-BMFF bodies land as `.mp4` instead of `.bin`.
- Video resolution reuses the existing tier order unchanged, so an uncached video still comes from the SHA-1 verified presigned CDN URL and `--no-cdn` still keeps a run local-only.
- `katok media get` extracts generic file attachments (message type 18) alongside photos, albums and videos — one message type covering zip, pdf, xlsx, hwp and every other extension. `--kind photo|video|file` narrows a run.
- File attachments resolve through the CDN alone: KakaoTalk keeps no local copy of them, so `--no-cdn` returns nothing for a file and a stub reads `unavailable` rather than `not-cached`. Output keeps the attachment's original name, sanitised so it cannot escape the output directory, disguise its own extension through a bidi override, or differ in bytes from a visually identical name.
- Added `katok media backfill`, which saves every attachment whose presigned link is still valid across all rooms. Presigned URLs expire after roughly two weeks and a file has no local copy, so anything not fetched inside that window is lost. Re-running is free: an already-saved frame is skipped with no network call.
- Fixed a limit that silently failed every CDN body over 10 MB — most videos and many files — by passing the fetch cap explicitly rather than relying on the HTTP client default.

### Sync performance

- A sync commits as one transaction and reuses prepared statements on the per-row write path, instead of a transaction per row.
- Only chats whose messages actually changed are re-chunked. `sync` reports `touched_chats`, and `--touched` exposes the list in the JSON payload.
- A touched chat rebuilds only its tail rather than its whole history. The rebuild starts at the last stable time-gap boundary preceding the earliest change, so ordinary append cost tracks the changed tail rather than total room size. A mid-history edit still widens the scope to whatever the change reaches, up to a full rebuild at the chat head.
- The reply and parent-reference pass is scoped to touched chats. Both endpoints of every `reply_edges` and `chunk_parent_refs` row are provably inside one chat, because message ids are chat-prefixed, so the whole-archive scan was never necessary. The remaining cost no longer tracks archive size.
- The messages tail is sought by `(chat_id, timestamp)` and the FTS tail delete by rowid, rather than scanning. Added indexes on the chunk tables by `(chat_id, started_at)`.
- A gap-settings change or a `CHUNKER_VERSION` bump still forces a full rebuild.

### Fixes to behaviour inherited from upstream

- Replies are resolved again. The reader looked for `src_logId` only in `supplement`, while KakaoTalk replies store it at the top level of `attachment`; the query now selects and checks that column first, with synthetic regression coverage.
- A wall of repeated long messages no longer makes an archive permanently unsyncable. `parent_id` was hashed from `(account, chat, first child, last child, text)` with no segment ordinal, so two windows cut from one oversized chunk collided whenever the text repeated — nine pasted 999-character messages is enough. The insert violated the primary key and, because a sync is a single transaction, rolled the whole sync back and failed identically on every later run. `CHUNKER_VERSION` is bumped so existing archives rebuild.
- Search snippets are no longer empty in Korean. `str::find` returns a byte offset and `Iterator::skip` counts chars; passing one to the other overshot by the encoding width, so a match late in a long Hangul chunk skipped past the end of the text.
- An edited message now reaches the archive, its chunk, and the index. The upsert updated only `chat_name`, `chat_type`, `sender_nickname` and `reply_to_message_id`, so a corrected body kept its original wording forever and the chat was reported unchanged, keeping it out of the rebuild. `timestamp` and `message_type` are worse than cosmetic: both feed chunk boundary computation.
- One room ingested under two accounts no longer crashes the sync. The chunk boundary test compared chat and sender but not `account_hash`, so both accounts' copies of a message landed in one chunk and violated `chunk_messages`' primary key.
- Parent windows no longer over-report `message_count`. A chunk spilling across three windows was counted by all three.
- An open chat is named by its link rather than by listing its members. `NTChatRoom.chatName` is NULL for these, so the link name prevents rooms from being filed under a member list that appears nowhere in KakaoTalk.
- Added `katok sync --prune-preview` and `--prune-deleted`. Sync otherwise only upserts, so a message deleted upstream stayed in the archive, its chunk text and the embedded index indefinitely. Only the time range the source still covers is reconciled: KakaoTalk prunes its own database and outliving that is what this archive is for, so anything older than the source's reach is left alone, and a chat the source did not mention is skipped entirely. Preview reports without deleting; deletion is never the default.


## 0.1.3 - 2026-07-18

- Added `katok media get` for KakaoTalk image extraction with local Pkv2 `.img`, CDN SHA-1 verified fetch, `.thm` fallback, and stub records.
- Documented that the CDN presigned GET is the only network tier in image extraction, and that `--no-cdn` disables it for local-only runs.
- Added synthetic SQLCipher and media-cache tests for full, CDN, thumbnail, stub, no-cdn, SHA-1 mismatch, and album type 27 paths.
