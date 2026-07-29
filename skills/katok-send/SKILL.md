---
name: katok-send
description: 'Send KakaoTalk messages and images from the CLI on macOS with `katok send`, matching the writing style sampled from that room history so a message reads as the account owner rather than as a bot. Opens the target room itself when its window is closed. Text into an open window sends in ~0.2s WITHOUT stealing focus, so the user can keep working; images and room-opening need the screen for ~2s behind a visible curtain that blocks input so the send cannot collide with the user. Native to katok — no third-party binary. Triggers (KR/EN): 카톡 보내줘, 카톡방에 전송, 카톡 발송, 카톡으로 이미지 보내, send a kakao message, kakao send image.'
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

When the window is closed, katok finds the room in the chat list and opens it. That needs the screen for a moment and so runs behind the curtain described below; a recent room near the top of the list takes about 2s end to end. Once the window is open it stays open, and sending text into it afterwards touches nothing.

- `--room` accepts either the name the chat list shows or the one the archive
  stores. A room with no name is titled by listing its members, and the two
  sources order that list differently (`나윤, 도현` against
  `도현, 나윤`), so matching is by member set rather than by string.
- The self-chat window is titled with your own nickname, not "나와의 채팅".
- `--no-open` fails with exit 1 instead of opening a closed window. Use it for automation that must never disturb the screen.
- `--dry-run` resolves and opens the room but sends nothing. Use it to verify targeting.
- `--take-focus-now` skips waiting for the user to stop typing. Use it when nobody is at the keyboard.
- `--focus-wait <seconds>` bounds that wait, default 15.

Rooms far down the chat list open unreliably. A room near the top is opened by clicking its row directly, but a room further down has to go through search, and KakaoTalk populates those rows late, so the lookup sometimes fails. The failure is explicit (exit 1), never silent. Once a person opens such a room by hand it stays reachable.

## Text and images behave differently

| | Text into an open window | Image, or opening a closed room |
|---|---|---|
| Duration | ~0.2s | ~2s |
| User's screen | untouched | curtain covers it, input blocked |

Text is written into `AXValue` directly and only the Enter key is delivered to the KakaoTalk process with `CGEventPostToPid`, so the app never has to be activated. Images cannot work that way: paste (Cmd+V) is a menu shortcut, which the app only handles while it is frontmost, and a pid-targeted event is silently ignored. Opening a closed room is the same story — its chat-list rows ignore `AXPress` and Enter, so only a real double-click works, and a click only reaches KakaoTalk while it is frontmost.

## The curtain

For the second or two a send needs the screen, katok covers every display with a "카톡 전송중" curtain and swallows real keyboard and mouse input, letting through only the events it synthesizes itself.

This exists because the old failure was worse than a failed send. A global keystroke goes to whichever application is frontmost at that instant, so if the user clicked away between KakaoTalk being activated and Cmd+V being posted, the image pasted into *their* document. Blocking input removes the race rather than narrowing it.

- **Esc, or clicking the cancel button, stops the run.** Cancelling before the paste sends nothing. Cancelling after it reports `CancelledAfterPaste`, because at that point delivery is genuinely unknown — check the chat rather than resending blindly.
- **Blocked input is dropped, not queued.** That is exactly why the block is always visible: a silent block would eat whatever the user typed into it. There is no block-without-showing mode, and asking for one is asking to lose someone's keystrokes.
- **A send waits for a gap in the user's typing before taking the screen.** If they keep typing for `--focus-wait` seconds (default 15) the send fails without ever taking focus, reporting that nothing was sent and nothing was typed anywhere. `--take-focus-now` skips the wait for unattended runs.
- **Everything is put back.** The previously active application is reactivated and the clipboard — which the image path has to overwrite, since KakaoTalk exposes no AX affordance for attaching a file — is saved and restored, on every exit path including failures.

Prefer text into an already-open window for unattended automation: it is the only path that touches nothing. Keeping the target room's window open is what makes that the common case.

## Writing in the sender's voice

Before composing anything that will go out under someone's name, read how they
actually write **in that room**. The archive already holds it; nothing needs to
be collected or stored in advance.

```sql
-- their own recent messages in one room
SELECT text FROM messages
WHERE chat_id = ? AND sender_id = ? AND message_type = 'text'
ORDER BY timestamp DESC LIMIT 25;
```

**Voice is per-room, not per-person, and the difference is large.** Measured on
one account on one evening, the same person describing the same meal:

| Room | What they actually write |
|---|---|
| with a parent-in-law present | `오늘 불고기 된찌 a polite casual ending!` · `우왕 a habitual misspelling!` |
| one-to-one with a partner | `a spaceless one-liner` · `a plain-speech remark` (avg 10 chars, spaces often dropped) |
| work group | `a full-sentence status report with @mentions` |

So a single stored "persona" would be wrong in two rooms out of three. Sample the
target room at composition time instead.

What to take from the sample: sentence endings (`~여` / `~다` / formal), whether
spaces get dropped, laughter (`ㅋㅋ` vs `ㅎㅎ`), vowel stretching (`너어어무`),
and typical length — a 140-character paragraph in a room whose average is ten
reads as someone else typing.

**Check who else already saw it.** Rooms overlap, and a person in two of them
has read both. Reporting to someone what was just said in a room they are also
in reads as talking past them — and if they know an assistant is writing, it
reads as the assistant not knowing who it is talking to. Before referring to
something said elsewhere, look up whether the recipient was in that room:

```sql
-- is this person a member of the room that message went to?
SELECT DISTINCT sender_nickname FROM messages WHERE chat_id = ?;
```

Measured: a thank-you sent to a room containing a parent-in-law **and** a
partner, followed by a message to that same partner one-to-one saying the
parent-in-law had been thanked. She had been in the first room the whole time.

Reuse their own phrases where one fits. Rewriting `a habitual misspelling` as `감사합니다`
is a correction nobody asked for and it is what makes a message sound generated.

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
