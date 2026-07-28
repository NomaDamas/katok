# Incremental chunking: the tail-scoped rebuild rule

This document is the single source of truth for **where a per-chat chunk rebuild is allowed to start**. The next implementation step (`rebuild_chunks_for_chats` -> tail-scoped) builds against this rule; the equivalence test named at the end pins it.

## Problem

`rebuild_chunks_for_chats` today re-reads a touched chat's entire message history through `raw_messages_for_chats` and re-chunks it from the first message. For the largest active room (178,012 messages) that is about 4.95s of work every time the room receives a single message, because the cost tracks room size rather than the number of new messages. We want the cost to track new messages instead.

## The two invariants the rule stands on

Both come straight from the boundary functions and are the reason a prefix of the history can be left untouched.

1. **Chunk boundaries are local.** `should_start_new_chunk(previous, next)` reads only the adjacent pair `(previous, next)`. The boundary between message `m[i-1]` and `m[i]` is a pure function of those two messages and nothing else. So if two message sequences share an identical prefix `m[0..=j]`, they share every chunk boundary at index `<= j`, and every chunk fully contained in that prefix is byte-identical (the `chunk_id` is a hash of the chunk's first and last `message_id`, never of its position).

2. **Parent-window accumulation resets at every window boundary.** `should_start_parent_window(current, next)` has three branches. Two are local (chat change, and a time gap over `DEFAULT_PARENT_WINDOW_SECONDS` = 300s, both a function of the previous chunk and `next` alone). The third is the character-limit branch, which reads `current_parent_len(current)` -- the accumulated character length of the window so far. That accumulation is **not** local, but `build_parent_windows` clears `current` at every window boundary, so the accumulation only ever depends on chunks back to the previous window boundary. A window that is already closed (some later window boundary exists after it) is therefore frozen: its placement and its internal segment splits are a pure function of its own chunks.

## The recompute start rule

Let `K(m) = (m.timestamp, m.message_id)` be the send-order key, matching `raw_messages ORDER BY chat_id, timestamp, message_id`. For a touched chat, let `e` be the smallest `K` over the chat's changed messages (inserted or updated this sync).

**Recompute start point `P` = the first message of the last existing parent window whose start key is strictly less than `e`.** If no existing window starts strictly before `e` (the earliest change lands at or before the chat's first window), `P` is the beginning of the chat and the rebuild is a full chat rebuild.

The scoped rebuild then deletes every chunk and parent window of the chat whose start key is `>= K(P)`, re-reads the chat's messages from `K(P)` onward (which necessarily includes every changed message, since `e >= K(P)`), re-chunks and re-windows them, and reinserts.

### Why this is exactly a full rebuild

- Everything strictly before `P` is unchanged input, so by invariant 1 its chunks and by invariant 2 its (closed) parent windows are byte-identical to a full rebuild. We leave those rows in place.
- `P` is a window start, so it is also a chunk start. Re-chunking from `P` sees `should_start_new_chunk(None, m[P]) = false` and opens a chunk at `P`; a full rebuild opened a chunk there too. Re-windowing from `P` sees `should_start_parent_window([], m[P]'s chunk) = false` and opens a window at `P` with a fresh accumulation, exactly as the full rebuild did at that window boundary.
- The boundary **at** `P` is itself unchanged: its chunk-level decision reads `m[P-1]` and `m[P]` (both strictly before `e`, so unchanged), and its window-level gap/char decision reads the previous window, which is closed and frozen. This is why `P` is chosen strictly before `e` rather than at `e` -- if the earliest change were the window's own first message, that change could move the boundary at `P` itself, so we step back one window.

### Why the cut is at a parent-window start, not a chunk start

Cutting at a mid-window chunk boundary would restart `build_parent_windows` with an empty `current`, discarding the character accumulation carried by the earlier chunks of that same window. The re-windowed run could then split the window at a different chunk than the full rebuild would, producing different `parent_id`s and different segment text. Cutting at a window start is the coarsest boundary that both invariants agree is stable, so it is the correct granularity. This is the sense in which the rule "covers the parent window".

## The common case is tail-only

When every change is a message appended at the tail (the normal sync), `e` is greater than every existing key, so "the last existing window whose start is strictly less than `e`" is simply the last existing window. `P` is that window's first message, and the rebuild recomputes only the last window plus the newly arrived tail. The work is `O(last-window size + new messages)`, independent of room size -- which is the whole point.

A mid-history in-place update (for example a `sender_nickname` change on an old message, which `upsert_message` does propagate and which does affect chunk boundaries) sets `e` back into the history, and the rule correctly widens `P` to the window containing that change, one window earlier if the change is that window's own head. Correctness is never traded for speed; the scope simply grows to whatever the change actually reaches, and in the worst case (a change at the chat head) it is a full chat rebuild.

## What the implementation step must add to sync

The rule needs `e` per touched chat, which `sync_messages` does not report yet: it currently reports only `touched_chats` (chat ids), not where in each chat the change landed. The implementation step will extend the sync report to also carry, per touched chat, the smallest changed key `e` (its earliest inserted-or-updated message's `(timestamp, message_id)`), and `rebuild_chunks_for_chats` will resolve `P` from the stored chunk gaps of that chat. The existing full-rebuild triggers (first sync, gap-settings change, chunker-version bump) are unaffected and still take the full path.

`rebuild_parent_refs` (the reply-edge and cross-chunk reference pass) still scans the whole archive. Step-1 measurements put that within the small-room floor (about 220ms) while the chunking loop dominated the large-room cost (about 4.95s), so tail-scoping the chunking loop captures the win; scoping the reference pass is a separate, later concern and is out of scope here.

### Where the implementation actually cuts

The implementation cuts not at the last parent window before `e` but at the last
**gap-derived** window start before `e`: the most recent chunk whose distance to
its predecessor exceeds `DEFAULT_PARENT_WINDOW_SECONDS`. A gap that wide always
opens a parent window, so every gap-derived start is a true window start and the
proof above applies unchanged; the cut is merely a more conservative (earlier or
equal) choice of `P`. Char-split windows after the cut are recomputed rather than
reused — same result, and the recompute range stays bounded by the burst since
the last gap, not by room size.

`TouchedChat.earliest_changed_message_id` is filled by `sync_messages` so the report
states the full change key `(timestamp, message_id)`, but the cut resolver does
not read it: a gap-derived `P` is separated from its predecessor by more than
300s, so `started_at >= P` needs no message-id tiebreak.

## Residual cost after tail scope (step-4 measurement)

Tail-scoping drops the large-room `rebuild_chunks` contribution from ~5s to
~270ms on the real archive (178k-message room, one new message). The residual is
**not** the re-chunk loop. It breaks down as:

1. **Archive-wide reply / parent-ref pass** (`rebuild_reply_and_parent_refs` →
   `rebuild_parent_refs`). Every scoped rebuild deletes and rewrites `reply_edges`
   for the whole archive and re-INSERT-OR-IGNOREs `chunk_parent_refs` by joining
   `messages` × `chunk_messages` with no chat filter. Cost tracks **archive size**,
   not the touched tail.
2. **No indexes on `chunks`** (`src/archive/schema.rs` creates tables only).
   `tail_rebuild_start` (`WHERE chat_id = ? AND started_at < ? ORDER BY started_at
   DESC`), tail deletes (`WHERE chat_id = ? AND started_at >= ?`), and the ref
   joins scan and sort the full `chunks` / `chunk_messages` tables. On a ~262k-row
   archive that is a full scan per statement.

Fixing either is a separate plan (indexes and/or scoped ref rebuild). This plan
only records the split so the residual floor is not mistaken for failed tail scope.

Synthetic check (release, `tests/incremental_chunking.rs::a_large_single_room_...`,
n=100000, 2026-07-28):

| stage | ms |
|---|---|
| whole-chat seed rebuild | 131644 |
| one-message scoped append | 5231 |
| `rebuild_reply_and_parent_refs` alone | 5102 |

Speedup scoped vs whole-chat ≈ 25x. Of the 5.2s scoped floor, **5.1s is the
archive-wide ref pass** — the re-chunk of the last burst is negligible. That is
why residual scales with archive size even after tail scope.

## The tests that pin the rule

`tests/incremental_chunking.rs` pins both the cut rule and the equivalence surface:

- `cutting_at_the_last_parent_window_reproduces_the_full_rebuild_tail` — two parent
  windows; rebuild from the last window's first message matches the full rebuild's
  tail across `chunks`, `parent_chunks`, `chunk_messages`, `parent_chunk_children`,
  `chunk_parent_refs`, `reply_edges`, and `chunks_fts`.
- `an_incremental_wave_after_a_gap_rebuilds_only_the_tail_and_matches_a_full_rebuild`
  — the scoped path engages an interior cut and still matches a full rebuild.
- `a_char_limit_split_window_is_recomputed_and_matches_a_full_rebuild` — char-limit
  (3,000) window splits after a gap cut are recomputed, not frozen mid-window.
- `an_in_place_nickname_change_widens_the_cut_and_matches_a_full_rebuild` —
  mid-history nickname update (not `text`; `upsert_message` ignores text on
  conflict) widens `P` correctly.
- `a_backfill_between_bursts_with_a_cross_chunk_reply_matches_a_full_rebuild` —
  late insert + non-empty `chunk_parent_refs`.
- `messages_sharing_a_timestamp_at_a_boundary_lose_neither_coverage_nor_equivalence`
  — same-timestamp boundary, no coverage loss, no message-id tiebreak needed.
- `a_large_single_room_tail_matches_a_full_rebuild_and_cost_tracks_new_messages` —
  100k-message single room (override with `KATOK_LARGE_CHAT_N`); tail equals a full
  rebuild of the same tail messages; frozen prefix untouched; scoped cost does not
  track room size.

If a future edit makes a boundary non-local, or makes a closed window depend on
later chunks, those equivalences break and the tests fail.
