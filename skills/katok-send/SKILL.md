---
name: katok-send
description: Use katok to dry-run, stage, or send one explicitly confirmed KakaoTalk message on macOS through the local Accessibility-driven KakaoTalk UI. Never use for bulk, repeated, unsolicited, deceptive, harassing, or unattended delivery.
license: MIT
metadata:
  category: communication
  locale: ko-KR
  phase: v1
---

# katok-send

Use the `katok` CLI only. This skill controls an irreversible external side
effect, so user confirmation and the repository's use policy are mandatory.

## Mandatory Rules

1. Read `ACCEPTABLE_USE_POLICY.md` and `DISCLAIMER.md` before the first send.
2. Never infer permission to send from a request to search, summarize, draft,
   configure, test, or inspect KakaoTalk.
3. Never use this skill for:
   - unsolicited commercial messages or spam;
   - sending after refusal, blocking, reporting, or a request to stop;
   - impersonation, account theft, phishing, fraud, stalking, or harassment;
   - repeated, bulk, scheduled, unattended, or recipient-list delivery;
   - privacy violations, secret monitoring, or access-control evasion.
4. Do not pass `--accept-use-policy` until the user explicitly confirms the
   exact target and final payload in the current interaction.
5. Prefer an unambiguous `--chat <chat-id>` over a room name.
6. Always run `--dry-run` first and show the resolved target to the user.
7. Never retry a failed send automatically. Report the error and stop.
   Exception: the single documented recovery in Troubleshooting (webp/HEIC
   paste-confirm miss → one PNG convert + one `--take-focus-now` resend). That
   is a payload fix, not a blind retry of the same path. If it fails again, stop.
8. Never log or repeat the message body unnecessarily.
9. `--text` and `--image` are mutually exclusive. Text + image = two separate
   `katok send` calls (text first, then image), each after user confirmation of
   that payload.

## Workflow

### 1. Check readiness

```bash
katok doctor --json
katok permissions macos --accessibility
```

Opening System Settings is not permission to send. The user must still approve
the exact message.

### 2. Resolve the target without sending

```bash
katok send --chat <chat-id> --dry-run --json
```

If only a room name is available:

```bash
katok send --room "정확한 방 이름" --dry-run --json
```

Stop if the room is missing, ambiguous, or different from what the user named.

### 3. Obtain final confirmation

Present:

- exact resolved room;
- exact text or image filename;
- whether this is an immediate send or a draft;
- the fact that the action may be irreversible and is not Kakao-approved
  automation.

Ask for an explicit yes/no confirmation immediately before execution. A prior
general instruction such as "알아서 보내" is not confirmation after the final
target and payload have changed.

### 4. Execute once

Text:

```bash
katok send --chat <chat-id> --text "확인된 최종 메시지" \
  --accept-use-policy --json
```

Image:

```bash
katok send --chat <chat-id> --image ./confirmed-image.jpg \
  --accept-use-policy --json
```

Draft for the user to review and send manually:

```bash
katok send --chat <chat-id> --text "확인된 초안" --draft \
  --accept-use-policy --json
```

Execute exactly once. Do not loop, schedule, fan out, or retry (except the
single recovery in Troubleshooting).

## Troubleshooting

Troubleshooting for KakaoTalk UI sends through the macOS Accessibility path.
Keep this as the single send path.

### Image paste never shows confirmation

Symptom (exact CLI error):

```text
pasted the image but KakaoTalk never showed the send confirmation, and the paste did not clear
```

What it means: the pasteboard write and Cmd+V ran, but KakaoTalk did not enter
the image-send confirmation sheet (or clear the compose paste). Common with
**webp** (and occasionally HEIC) even though the CLI accepts those extensions —
the bug is on KakaoTalk's paste UI side, not "unsupported type" in katok.

One allowed recovery (do this once, then stop):

```bash
# 1) Convert to PNG (macOS)
sips -s format png ./confirmed.webp --out /tmp/katok-send.png

# 2) Resend once, taking focus so the image menu path can run
katok send --chat <chat-id> --image /tmp/katok-send.png \
  --take-focus-now --json
```

- Do **not** loop, schedule, or fan out. One convert + one resend, then report.
- Keep the original path in the user-facing confirmation; the PNG is a delivery
  intermediate, not a second identity for the asset.
- If text already sent successfully in the same turn, only recover the image leg.
  Do not re-send the text.

### Other quick checks

| Symptom | Check |
|---|---|
| `resolved: false` / room missing | Wrong `chat_id`; re-run `--dry-run`. Prefer `--chat` over `--room`. |
| Room ambiguous by name | Always switch to `--chat <chat-id>`. |
| Image send needs the screen | Image paste uses the system paste menu path; `--take-focus-now` if nobody is at the keyboard. Text send can stay background. |
| KakaoTalk not running | `pgrep -x KakaoTalk` — start the app first; send drives the live UI. |
| Want human review before send | `--draft` leaves text in the compose box; person presses Enter. |

## Technical Boundary

The current `katok send` path does not call a Kakao remote private or
undocumented protocol/API. It uses local macOS Accessibility, `CGEvent`,
pasteboard, AppKit, local files, and the local katok archive to drive the
official running KakaoTalk app. KakaoTalk itself performs the network
transmission.

This architecture does not mean Kakao has approved the tool or that account
restrictions cannot occur. Other katok commands can use documented network
paths such as attachment CDN or model downloads.
