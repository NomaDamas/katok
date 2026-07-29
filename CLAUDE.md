@AGENTS.md

# Claude Code notes

The project instructions live in `AGENTS.md`, imported above so both Claude Code
and the tools that read `AGENTS.md` work from one copy. Edit `AGENTS.md`, not
this file.

## Verifying against a real KakaoTalk install

`AGENTS.md` forbids tests that depend on the developer's own KakaoTalk data, and
that rule holds. It does not forbid *manual* verification, which some of this
crate cannot be checked any other way — decryption, the Accessibility send path,
and the input-blocking curtain all only exist against the running app.

When a change needs that:

- Never print message text, room names, real names, or phone numbers into the
  session. Report counts, ids, hashes, and status.
- Sending is not reversible and reaches other people. Do not pick a room to test
  against on your own initiative; ask, or use a room the maintainer has named for
  it in `CLAUDE.local.md`.
- Opening a room window is harmless and notifies nobody, so prefer `--dry-run`
  when you only need to prove targeting.

## This repository is a public fork

Nothing referencing private tooling, internal projects, or personal operating
notes belongs in a commit here. Keep that material in `CLAUDE.local.md`, which is
git-ignored, or outside the repo entirely.
