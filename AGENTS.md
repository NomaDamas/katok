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
- **Nothing derived from a real archive may be committed, including in documentation.** This
  repository is public; the archive it reads is not. Avoiding private content in session output
  is not enough, because a finding made while looking at live data tends to get written down
  next to the rule it justified — a quoted message, a room name, a person's name or kinship
  term, a sample used because it was at hand. Skill files, comments, tests, and changelogs are
  all published. State the rule and drop the evidence: the observation that convinced you is
  for this conversation, not for the commit. Fixtures stay synthetic even when a real value
  would have been easier to paste.
- Keep README, CLI help, and privacy behavior aligned in the same change.

## Manual Verification Against a Real Install

Some of this crate cannot be checked any other way: decryption, the Accessibility
send path, and the input-blocking curtain only exist against the running app. The
rule above forbids automated tests that depend on a real install; it does not
forbid verifying by hand. When doing so:

- Verify with counts, ids, hashes, and status rather than by dumping rows.
  Showing someone their own conversation is what this tool is *for* — a
  transcript or a search result asked for by the user is the product working,
  not a leak. The thing to avoid is incidental exposure: pasting raw query
  output into a session while debugging something unrelated, where the content
  was never what anyone asked to see.
- Sending is not reversible and it reaches other people. Do not choose a room to
  test against on your own initiative — ask, and treat any standing permission as
  covering only the rooms actually named.
- Opening a room window notifies nobody, so prefer `--dry-run` when only the
  targeting needs proving.

## Repository Hygiene

- Generated archives, indexes, embedding caches, auth caches, logs, and local test output belong in ignored paths.
- Prefer small, explicit commits.
- Do not add telemetry.
- Do not weaken privacy checks to make demos easier.
