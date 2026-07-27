# Failure modes

All of these were hit on real hardware on 2026-07-23. Symptom, then cause, then what to do.

## Permissions

**Symptom**: `accessibility permission is not granted`, or an osascript error `-25211` about assistive access.

**Fix**: System Settings, Privacy & Security, Accessibility. Add the app that hosts this session and turn the toggle on. Verify:

```bash
osascript -e 'tell application "System Events" to return UI elements enabled'   # must print true
```

**Trap 1, granting the wrong app.** `TERM_PROGRAM` reports the terminal-rendering library an app embeds, not the app itself, so a host application can appear under a library's name. Permission has to go to the real host application. Walk the process tree up to the owning app rather than trusting the environment variable.

**Trap 2, the toggle is on and it still fails.** An ad-hoc signed app (no Team ID) gets a new signature on every rebuild, so an existing entry in the Accessibility list can stop matching the binary that is actually running. Turning the toggle off and on does not fix it. Remove the entry from the list entirely and re-add it with `+`.

## The room does not open

**Symptom**: `selected '<room>' in the chat list but its window did not open`, or `is not in the chat list`, exit 1.

**Cause**: chat-list rows ignore both `AXPress` and Enter, even after the app is raised and the row is selected and focused. Only a real double click at the row's coordinates opens it, which means the row has to be visible on screen. Rooms at the top of the list are visible immediately; a room further down has to be filtered into view by search, and KakaoTalk sometimes populates the text of a search-result row too late for the lookup to succeed.

**Fix**: confirm the exact name with `--list-rooms` first. If it still fails, have a person open that room in KakaoTalk once; sending works normally afterwards. The failure exits 1 with no fallback, so automation can detect it.

Use `--no-open` to forbid opening entirely. With no window present it fails immediately and never touches the screen.

**Self-chat**: the window is titled with your own nickname, not "나와의 채팅". It also does not appear in KakaoTalk's search results, so search-based tools cannot find it at all.

## KakaoTalk has collapsed to the menu bar

**Symptom**: `--list-windows` returns only the main window, or nothing, and every send fails.

**Cause**: the user closed the windows and left the app running. There are genuinely zero `AXWindow` elements.

**Fix**: AppleScript `activate` usually does not recover this. A person has to click the Dock icon to bring a window back. Ask rather than trying to work around it.

## An image reported success but never arrived (fixed)

**Symptom**: `sent: true` with nothing delivered.

**Cause**: the original implementation treated "no confirmation sheet and an empty compose box" as success. A silently ignored paste also leaves the compose box empty, so that test produced false successes. The underlying reason was sending Cmd+V to the pid only, which a menu key equivalent ignores unless the app is frontmost.

**Now**: the image path raises KakaoTalk with `AXFrontmost`, pastes through the global tap, and reports success only after a new row appears in the transcript. Failures exit 1.

**Lesson**: verify a send from the receiving side, not from a signal on the sending side.

## Verification itself reported a false negative

**Symptom**: a message that did arrive was judged missing.

**Cause**: reading the conversation from the rendered screen (parsing the transcript over AX) misses recent messages when the window is scrolled or stale.

**Fix**: query the database. Counting rows in `NTChatMessage` is the only dependable check (type 2 is a photo, 27 an album).

## Touching the screen mid-run

Text sending is unaffected; it never takes focus, so the user can keep working. That is why text is the default for automation.

Image sending is affected. While KakaoTalk is frontmost for the paste, clicking into another window can send the paste somewhere else. Send images when the user is away from the screen.

## Korean input method, for reference

AppleScript-style `keystroke "v" using command down` maps the character through the currently active input source, so with a Korean input method active it does not resolve to Cmd+V. A physical key code (`key code 9`) is required.

The current implementation does not have this problem: the body goes in through `AXValue` and keys are sent as CGEvent virtual key codes. Worth remembering if anything is ever moved back onto AppleScript.

## Other

- A KakaoTalk UI update can reshape the AX tree and break the selectors (the compose box is a text area inside a scroll area). Start narrowing with `--list-windows` to see whether window lookup still works, then move inward.
