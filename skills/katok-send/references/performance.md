# Where the time goes

Measured 2026-07-23 against KakaoTalk on macOS. The final numbers are `katok send`. The twenty-second numbers come from a reference implementation that opens the room by walking the chat list; `katok send` avoids that path by design and never pays the cost.

## Measurements

| Path | Time | Takes the screen |
|---|---|---|
| `katok send --text`, window already open | **0.19s** | no |
| `katok send --image`, window already open | 2.1s | yes, activation required |
| Reference: window open, activation-based | 2.4s | yes |
| Reference: including automatic room opening | **20-23s** | yes |

## Nearly all of it is finding the room

Timing the automatic-open path through a pty, since a pipe buffers and destroys the intervals:

```
 0.00s ~ 21.00s   chats: resolved rows=200   <- AX walk over 200 chat-list rows
21.02s            chat list row selected
21.55s            set AXValue succeeded
21.66s            Enter reflected -> sent    <- the send itself took 0.16s
```

The send is 0.16s. The twenty seconds are spent locating the room. That is why `katok send` targets a window that is already open and keeps a room open rather than rediscovering it.

## Why scanning 200 rows costs 20 seconds

Two hundred rows would be nothing as an in-process array. This is not in-process data.

- One AX attribute read is a synchronous cross-process round trip: this process, then the macOS AX subsystem, then KakaoTalk's main thread, then back. It behaves like RPC, not like indexing.
- KakaoTalk's main thread is also its UI rendering thread, so every request queues behind whatever the app is already doing.
- A row is not one read. Extracting a title walks up to 80 nodes, and the preview walks again, so roughly 160 nodes per row, each node costing several attribute reads.

The arithmetic matches: 200 rows x 160 nodes is about 30,000 round trips, and 20s / 30,000 is about 0.6ms per trip, which is ordinary AX latency.

Confirmed by scaling: the same scanner over 20 rows takes 1.19s, over 200 rows about 20s, exactly proportional to row count.

KakaoTalk's AX implementation is also weak. The reference implementation's author noted that rows ignore both `AXPress` and a keyboard Enter, leaving a hardware double click as the only reliable activation. Apps like that often build AX nodes on demand, which makes each read more expensive still.

The row count is not the problem; interrogating another app's UI one node at a time is. For the same reason `ax_send.rs` only counts rows during transcript verification and never descends into them.

## Why text is 0.19s and leaves the screen alone

The body goes in through `AXValue`, so neither the keyboard nor focus is involved. Activation was needed for exactly one thing, the Enter key, and delivering that with `CGEventPostToPid` straight to the KakaoTalk process removes the need. An event posted to the global HID tap would go to whatever app is frontmost, which is what forces activation; a pid-targeted event does not.

## Why images are 2s and take the screen

An image has to go through the clipboard and be pasted. Cmd+V is a menu key equivalent, so the app only handles it while frontmost, and a pid-targeted event is silently ignored — measured, the paste simply never happens. Only the image path therefore raises KakaoTalk with `AXFrontmost` and sends Cmd+V through the global tap.

No way around this asymmetry was found. Automation should use text.

## Notes on measuring

Progress logs read through a pipe are block-buffered, which distorts interval timings. macOS `script` fails when stdin is not a tty. Python's `pty.spawn` provides a pseudo-terminal and gives real line-by-line timestamps.

Do not judge delivery from the sending side. A tool that reads the conversation from the rendered screen returns stale results when the window is scrolled, which once caused a successful send to be reported as a failure. Counting rows in `NTChatMessage` directly is the only dependable check.
