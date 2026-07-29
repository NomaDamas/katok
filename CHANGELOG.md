# Changelog

## Unreleased

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
