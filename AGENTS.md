# Kakao Memory Agent Instructions

These instructions apply to this repository and all child directories.

## Project Intent

Kakao Memory is a local-first semantic memory and search layer for KakaoTalk conversations on macOS. Treat it as privacy-sensitive infrastructure, not a casual chat analyzer.

## Architecture Guidelines

- Prefer source adapters over duplicating DB reverse engineering logic.
- The first source adapter should integrate with `kakaocli` or the `k-skill` `kakaotalk-mac` helper.
- Keep ingestion, normalized archive, keyword search, semantic search, and skill wrapper as separable modules.
- Use stable message identifiers and incremental cursors so indexing can resume without rereading everything.
- Keep the agent skill thin: it should call the CLI and summarize results, not own indexing logic.

## Development Guidelines

- Add tests before behavior changes.
- Use fixtures with synthetic chat data only.
- Do not create tests that depend on the user's real KakaoTalk installation or real local DB.
- Real KakaoTalk smoke tests may be manual-only and must avoid printing private content.
- Keep README, CLI help, and privacy behavior aligned in the same change.

## Repository Hygiene

- Generated archives, indexes, embedding caches, auth caches, logs, and local test output belong in ignored paths.
- Prefer small, explicit commits.
- Do not add telemetry.
- Do not weaken privacy checks to make demos easier.
