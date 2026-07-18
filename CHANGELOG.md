# Changelog

## Unreleased

## 0.1.3 - 2026-07-18

- Added `katok media get` for KakaoTalk image extraction with local Pkv2 `.img`, CDN SHA-1 verified fetch, `.thm` fallback, and stub records.
- Documented that the CDN presigned GET is the only network tier in image extraction, and that `--no-cdn` disables it for local-only runs.
- Added synthetic SQLCipher and media-cache tests for full, CDN, thumbnail, stub, no-cdn, SHA-1 mismatch, and album type 27 paths.
