# Changelog

## 0.2.0 - 2026-07-29

### Sending

- `katok send --chat <chat-id>` addresses a room by id. Display names are not identifiers — on the reference install four names are shared by two rooms each and one archive name covers nine — so a name alone had no single answer and the first matching row won. An ambiguous name is now refused outright rather than guessed at, since a message delivered to the wrong person cannot be taken back; `--chat` resolves it through the room's position among same-named chats.
- Rooms are matched by member set rather than by string. KakaoTalk titles an unnamed group room by listing its members and does not use the order the archive does (`나윤, 도현` against `도현, 나윤`), so a name taken from a search result never matched the row it came from and every unnamed group room was unreachable.
- The chat search box is now typed into with real keystrokes. Setting its accessibility value painted the text but told the app nothing had happened, so the list never filtered and the search strategy silently did nothing — leaving the unfiltered list, where a room far enough down sits outside the scroll area and was clicked at coordinates nobody could see.
- `--draft` leaves a message in the compose box for review. It pastes rather than writing the accessibility value, because writing that value *is* sending: KakaoTalk delivers on the change with no keystroke involved.
- A send that needs the screen now runs behind a full-screen curtain that blocks keyboard and mouse input, so it cannot collide with whoever is using the machine. Blocked input is dropped rather than queued, which is why the block is always visible. Esc or the cancel button stops the run.
- The previously active application and the clipboard are restored on every exit path, including failures, and a global keystroke is never posted unless KakaoTalk is confirmed frontmost at that instant — a collision now produces a clean failure instead of a paste into someone else's document.
- `--room` rejects control characters and invisible formatting marks, naming the codepoint, since those are quoting accidents upstream rather than missing rooms.

### Reading

- `katok media get` extracts generic file attachments (message type 18) alongside photos, albums and videos — one message type covering zip, pdf, xlsx, hwp and every other extension. `--kind photo|video|file` narrows a run.
- File attachments resolve through the CDN alone: KakaoTalk keeps no local copy of them, so `--no-cdn` returns nothing for a file and a stub reads `unavailable` rather than `not-cached`. Output keeps the attachment's original name, sanitised so it cannot escape the output directory, disguise its own extension through a bidi override, or differ in bytes from a visually identical name.
- Added `katok media backfill`, which saves every attachment whose presigned link is still valid across all rooms. Presigned URLs expire after roughly two weeks and a file has no local copy, so anything not fetched inside that window is lost. Re-running is free: an already-saved frame is skipped with no network call.
- Fixed a limit that silently failed every CDN body over 10 MB — most videos and many files — by passing the fetch cap explicitly rather than relying on the HTTP client default.

### Fixes to behaviour inherited from upstream

- Replies are resolved again. The reader looked for `src_logId` in the `supplement` column; every KakaoTalk reply stores it at the top level of `attachment`, which the query did not select. Measured on a live install: 6891 of 6891 replies carry it in `attachment` and none in `supplement`, so `reply_to_message_id`, `reply_edges` and `chunk_parent_refs` were all empty and every search hit reported no parent. The synthetic fixture modelled a `supplement` shape KakaoTalk never writes, and its schema omitted the `attachment` column entirely, so the unit tests stayed green throughout.
- A wall of repeated long messages no longer makes an archive permanently unsyncable. `parent_id` was hashed from `(account, chat, first child, last child, text)` with no segment ordinal, so two windows cut from one oversized chunk collided whenever the text repeated — nine pasted 999-character messages is enough. The insert violated the primary key and, because a sync is a single transaction, rolled the whole sync back and failed identically on every later run. `CHUNKER_VERSION` is bumped so existing archives rebuild.
- Search snippets are no longer empty in Korean. `str::find` returns a byte offset and `Iterator::skip` counts chars; passing one to the other overshot by the encoding width, so a match late in a long Hangul chunk skipped past the end of the text.
- An edited message now reaches the archive, its chunk, and the index. The upsert updated only `chat_name`, `chat_type`, `sender_nickname` and `reply_to_message_id`, so a corrected body kept its original wording forever and the chat was reported unchanged, keeping it out of the rebuild. `timestamp` and `message_type` are worse than cosmetic: both feed chunk boundary computation.
- One room ingested under two accounts no longer crashes the sync. The chunk boundary test compared chat and sender but not `account_hash`, so both accounts' copies of a message landed in one chunk and violated `chunk_messages`' primary key.
- Parent windows no longer over-report `message_count`. A chunk spilling across three windows was counted by all three.
- An open chat is named by its link rather than by listing its members. `NTChatRoom.chatName` is NULL for these, so they were filed under a member list that appears nowhere in KakaoTalk — mislabelling the largest room in the reference archive, 185,066 messages, and making three rooms unreachable by name. Re-syncing corrected 263,436 rows.
- Added `katok sync --prune-preview` and `--prune-deleted`. Sync otherwise only upserts, so a message deleted upstream stayed in the archive, its chunk text and the embedded index indefinitely. Only the time range the source still covers is reconciled: KakaoTalk prunes its own database and outliving that is what this archive is for, so anything older than the source's reach is left alone, and a chat the source did not mention is skipped entirely. Preview reports without deleting; deletion is never the default.

- `katok media get` now extracts videos (message type 3) alongside photos and albums. Video bodies live in the `.vid` cache under a `v`-prefixed key stem, and the output extension is sniffed so ISO-BMFF bodies land as `.mp4` instead of `.bin`.
- Video resolution reuses the existing tier order unchanged, so an uncached video still comes from the SHA-1 verified presigned CDN URL and `--no-cdn` still keeps a run local-only.
- `katok media get` also extracts generic file attachments (message type 18), which is one message type covering zip, pdf, xlsx, hwp, pptx and every other extension. `--kind photo|video|file` narrows a run; the default reads every kind.
- File attachments resolve through the CDN alone. KakaoTalk keeps no local copy of them — a full scan of the container finds only `.thm`, `.img`, and `.vid` — so `--no-cdn` returns nothing for a file and a stub reads `unavailable` rather than `not-cached`.
- File output keeps the attachment's original name (`<logId>_<name>`), sanitised so it cannot escape the output directory. The name is authoritative for the extension because a zip body sniffs as `.bin`.
- Added `katok media backfill`, which saves every attachment whose presigned link is still valid across all rooms. Presigned URLs expire after roughly 14 days and a file has no local copy, so anything not fetched inside that window is lost for good. Re-running is free and idempotent: an already-saved frame is skipped with no network call. `--dry-run` reports what would be fetched without issuing a request, separating "would download" from "already expired".
- Fixed a latent limit that silently failed every CDN body over 10 MB, which is most videos and many files, by passing the fetch cap explicitly instead of relying on the HTTP client default. An attachment whose declared size exceeds `--max-bytes` is now refused before the request rather than after the download.
- `katok send` no longer risks pasting into whatever application the user switched to. Sending an image, or opening a closed room, has to bring KakaoTalk forward, and a global keystroke goes to whichever app is frontmost at that instant. Frontmost is now confirmed immediately before every global post and the post is abandoned otherwise, so a collision produces a clean failure instead of a stray paste.
- `katok send` blocks keyboard and mouse input behind a visible full-screen curtain for the second or two a send needs the screen, so the user cannot collide with it at all. Esc or the cancel button stops the run. Blocked input is dropped rather than queued, which is exactly why the block is always shown rather than silent.
- `katok send` waits for a gap in the user's typing before taking focus, restores the previously active application when it is done, and no longer overwrites the clipboard: the existing contents are saved and put back. Added `--take-focus-now` to skip the wait and `--focus-wait` to bound it.
- Resolving a room takes focus once for the whole attempt sequence rather than per attempt, which cut a failing resolve from 54s to under 3s.

## 0.1.3 - 2026-07-18

- Added `katok media get` for KakaoTalk image extraction with local Pkv2 `.img`, CDN SHA-1 verified fetch, `.thm` fallback, and stub records.
- Documented that the CDN presigned GET is the only network tier in image extraction, and that `--no-cdn` disables it for local-only runs.
- Added synthetic SQLCipher and media-cache tests for full, CDN, thumbnail, stub, no-cdn, SHA-1 mismatch, and album type 27 paths.
