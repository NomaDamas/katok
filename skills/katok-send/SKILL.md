---
name: katok-send
description: 'Send KakaoTalk messages and images from the CLI on macOS with `katok send`. Opens the target room itself when its window is closed. Text sends in ~0.2s WITHOUT stealing focus, so the user can keep working; images need KakaoTalk to come forward briefly. Native to katok — no third-party binary. Triggers (KR/EN): 카톡 보내줘, 카톡방에 전송, 카톡 발송, 카톡으로 이미지 보내, send a kakao message, kakao send image.'
---

# katok-send

`katok send` writes a message or an image into a KakaoTalk chat. It is the same binary as the read side documented in `skills/katok`.

There is no supported write path into the local KakaoTalk store. Reading goes straight at the SQLCipher database, but sending has to drive the running app's UI through the macOS Accessibility API. `kakao/ax_send.rs` is the only module in katok that touches AX.

## Commands

```bash
katok send --room "<window title>" --text "message"
echo "body" | katok send --room "<window title>"        # stdin when --text is omitted
katok send --room "<window title>" --image /path/a.png
katok send --room x --list-rooms --json                 # room names from the chat list, newest first
katok send --room x --list-windows --json               # chat windows currently open
katok send --room "<window title>" --dry-run            # resolve and open, send nothing
```

When the window is closed, katok finds the room in the chat list and opens it. A recent room near the top of the list opens in about 0.4s and does not steal focus.

- `--room` must match the name shown in the chat list exactly. Confirm it with `--list-rooms`.
- The self-chat window is titled with your own nickname, not "나와의 채팅".
- `--no-open` fails with exit 1 instead of opening a closed window. Use it for automation that must never disturb the screen.
- `--dry-run` resolves and opens the room but sends nothing. Use it to verify targeting.

Rooms far down the chat list open unreliably. A room near the top is opened by clicking its row directly, but a room further down has to go through search, and KakaoTalk populates those rows late, so the lookup sometimes fails. The failure is explicit (exit 1), never silent. Once a person opens such a room by hand it stays reachable.

## Text and images behave differently

| | Text | Image |
|---|---|---|
| Duration | ~0.2s | ~2s |
| User's screen | untouched | KakaoTalk comes forward briefly |

Text is written into `AXValue` directly and only the Enter key is delivered to the KakaoTalk process with `CGEventPostToPid`, so the app never has to be activated. Images cannot work that way: paste (Cmd+V) is a menu shortcut, which the app only handles while it is frontmost, and a pid-targeted event is silently ignored. The image path therefore activates KakaoTalk.

Prefer text for unattended automation. Send images only when the user is away from the screen, or when a brief focus change is acceptable.

## Reporting success

Report a send as successful only after a new row appears in the transcript. A silently ignored paste still leaves the compose box looking empty, so treating an empty compose box as proof of delivery reports messages that were never sent. An earlier revision of this skill did exactly that. Failures exit 1 with no fallback path.

## Requirements

- The KakaoTalk desktop app must be running and already signed in. No credentials are stored anywhere; katok only acts on a session that already exists.
- macOS Accessibility permission. Check with `osascript -e 'tell application "System Events" to return UI elements enabled'`, which must print `true`.
- See [references/local-build.md](references/local-build.md) for the binary and how to build it.

## Safety

- Keep real recipients and message bodies out of logs and shared surfaces. Success output reports a character count and a file name, nothing more.
- Sending cannot be undone. Do not send to a real room from an automated loop without a person in it. Test against the self-chat.
- This is unofficial automation, so account restrictions are possible in principle. Do not use it for bulk or unsolicited messaging.

## References

- [references/performance.md](references/performance.md) — why a send takes 0.2s but finding a room can take 20s (AX is synchronous IPC)
- [references/troubleshooting.md](references/troubleshooting.md) — permissions, collapsed menu bar, verification traps
- [references/local-build.md](references/local-build.md) — binary location, build, relationship to upstream

## Related Skills

- `skills/katok` — the read and search side, same binary.
