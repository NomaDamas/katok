# katok local notes

Never committed. Git-ignored via `.gitignore`, and this repo is a public fork, so
anything naming a real person or private tooling stays in this file.

**This file lives only in this checkout.** A git-ignored file is deleted along
with a worktree it happens to sit in, and it is not in any commit, so nothing
restores it. Keep the canonical copy somewhere durable and treat this as a
convenience copy.

## Send testing

The maintainer has authorised these targets for `katok send` experiments
(2026-07-29, direct instruction):

- **소링링** — free to send test messages and images to.
- **The maintainer's own chats** — also fine.

Any other room needs a fresh ask. Sending cannot be undone and reaches real
people, so treat "it worked last time" as covering only the rooms listed above.

Prefer `--dry-run` when only targeting needs proving; it opens the room window
and sends nothing.

## Verified by hand

- **Clipboard save/restore around an image send** (2026-07-29, 소링링). A canary
  string was copied, a test image sent, then the canary came back
  byte-identical and the previously active application was restored. The send
  took 3.5s. Nothing in the send path still needs a manual pass.

## Known, not a regression

Some rooms cannot be opened by clicking their chat-list row — 카카오페이,
에르메스단, 가온노트 신지혜 have all failed this way. The installed release
build failed identically before any of this work, so it predates the curtain.
소링링, 클코단, and 블라이 V2 open normally. Once a person opens a room by hand
it stays reachable.
