# Scrollback Architecture

Read this when modifying scrollback, scroll behavior, session switching, or snapshot handling.

## Two-Tier Scrollback

ImmorTerm AI uses a two-tier scrollback architecture:

| Tier | Location | Purpose |
|------|----------|---------|
| **Local (WASM)** | `immorterm-core::Scrollback` (VecDeque ring buffer) | Fast scroll through recent history |
| **Daemon-side** | Full PTY history on native daemon | On-demand fetch for deep scroll |

The WASM terminal holds a bounded scrollback buffer (`max_lines`). When the user scrolls beyond it, the JS layer requests older rows from the daemon via WebSocket `scroll_request`. These rows are prepended to the front of the WASM buffer using `push_front()`.

### `push_front` Has No Capacity Eviction

Unlike `push()` (which evicts the oldest row when at capacity), `push_front()` does **not** enforce `max_lines`. This is intentional:

- Daemon already bounds the total history
- Evicting from the back (newest) would create circular scrollback where the user sees the same content looping
- No production terminal (Ghostty, Alacritty, WezTerm, Kitty) evicts newest rows on prepend

## Session Switching and Background State

### backgroundControlMode (default: true)

When the user switches away from a session:
1. `save_active()` stores the full WASM terminal state (terminal, scroll_offset, selection) into a `BackgroundState` slot
2. The WebSocket downgrades to `subscribe_control` (metadata only, no PTY stream)

When switching back:
1. `restore(bgStateId)` restores the saved state (scroll position, scrollback, selection)
2. WebSocket upgrades to `subscribe_raw` with `full_snapshot: true`
3. The daemon sends a **full snapshot** (including scrollback) to replace stale background scrollback

### Why Full Snapshot on Session Switch

The background state's scrollback is frozen at switch-away time. Meanwhile, new output may have been written to the session. Without a full snapshot, the stale scrollback wouldn't stitch properly with the current viewport — creating a visual discontinuity where old content appears "stuck" above the live terminal.

The `full_snapshot` flag on `subscribe_raw` reuses the daemon's existing `pending_full_snapshot` mechanism (originally built for deferred restore after crash recovery).

### Snapshot Types

| Type | Contains | When Used |
|------|----------|-----------|
| `snapshot()` | Grid + full scrollback | After deferred restore, or when `full_snapshot: true` |
| `snapshot_viewport_only()` | Grid only (scrollback empty) | Lag recovery resyncs, normal subscribe_raw |

### `preserve_sb` Optimization

In `load_snapshot`, when the incoming snapshot has empty scrollback but the WASM terminal already has scrollback, the existing scrollback is preserved (transplanted). This handles viewport-only resyncs during heavy output without losing scroll history.

**Guard**: Only fires when `immorterm_id` matches (same session). Prevents transplanting a closed session's scrollback into a different session.

## Scroll Offset Behavior

- **Same session snapshot**: `scroll_offset` is preserved via `prev_offset.min(scrollback.len())`
- **Different session**: `scroll_offset` resets to 0 (bottom/live view)
- **Background restore**: `scroll_offset` comes from the saved `BackgroundState` — exact position preserved

## `daemonFetchedRows` Tracking

The JS side tracks how many rows were fetched from the daemon (`daemonFetchedRows`). This counter:
- Is reset to 0 when scrollback is replaced (new session, full snapshot)
- Is preserved when scrollback survives a viewport-only resync (`scrollbackPreserved` check)
- Prevents re-fetching already-prepended daemon rows on subsequent scroll-ups
