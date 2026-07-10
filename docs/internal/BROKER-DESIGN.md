# Self-Driven Browser — Hardening Spec (Broker-Role)

Status: design, pre-implementation. Component:
`apps/immorterm-ai/immorterm-daemon`. This hardens the existing v1 browser
draft (`browser.rs` + the `immorterm_browser_*` wiring in `mcp.rs`/`main.rs`)
**in place**. It does not start from zero and does not preserve v1's flaws.

Public tool contract (the request/response shapes): `docs/browser-tools.md`.

## Why v1 must change (the flaws we are removing)

The v1 draft (`browser.rs`) is a working single-daemon prototype with four
security/architecture problems this spec fixes:

1. **TCP debug port.** It launches Chromium with `--remote-debugging-port=0`
   and connects CDP over `ws://127.0.0.1:PORT`. Any local process (any
   website via DNS-rebind-adjacent tricks, any other program) that finds that
   port can drive the browser. Replace with `--remote-debugging-pipe`
   (inherited fds, no listener).
2. **No ownership model.** Each MCP process launches its own browser. ImmorTerm
   runs one daemon per window; N windows = N browsers, N profile-dir locks
   fighting. Replace with the broker role below.
3. **Raw `eval` on the default surface + no navigation allowlist + coord-only
   clicking.** Replace with a ref-based safe surface, gated eval, and a scheme
   allowlist.
4. **DPR pinned via `Emulation.setDeviceMetricsOverride` (=1).** This distorts
   what the user sees. Replace with the CSS-pixel capture recipe (capture at
   the real DPR, scale the screenshot down so screenshot-px == CSS-px).

## Transport-reuse premise — CORRECTION

The task brief said "reuse `remote.rs` request/response framing" for
daemon↔daemon routing. **`remote.rs` is not that.** `remote.rs` is SSH-tunnel
*setup* (add/edit/remove remotes, `ssh-copy-id` bootstrap, writing
`.mcp.json`). It has no daemon↔daemon message framing.

The real daemon↔daemon transport already in the tree:

- **`websocket.rs`** — the per-session WebSocket. Messages are JSON text
  frames tagged with `#[serde(tag = "type")]` (`WsClientMsg` inbound;
  `control_*` / `browser_frame` / etc. outbound). This is the framing to reuse.
- **`registry.rs`** — every live session publishes its `ws_port`
  (`SessionEntry.ws_port`). That is how one daemon finds another's WS locally;
  remotely the SSH tunnel from `remote.rs` forwards that same WS port. So
  `remote.rs` is still relevant — as the *tunnel*, not the *framing*.

**Route browser calls over the per-session WS (websocket.rs framing), address
the owner via its `registry.rs` `ws_port`.** Add one new tagged message pair —
do not invent a new socket, new port, or new protocol.

## Ownership: broker-as-daemon-role

No new process. The broker is a *role* an existing per-window daemon takes on.

- The **first** daemon to need the browser LAUNCHES it and becomes **owner**,
  recording itself in a lock file (below).
- **Other** daemons see the lock, and ROUTE their browser tool-calls to the
  owner over the per-session WS, addressing it by the owner's `ws_port`.
- Owner dies / lock goes stale → the next requester takes over: removes the
  stale lock, launches a fresh browser, becomes owner.

Only the owner holds the CDP pipe fds, so only the owner can actually drive the
browser. Everyone else is a client of the owner. This is enforced by transport
(pipe fds are not shareable across processes), not by convention.

### Lock file format

`$IMMORTERM_HOME/browser.lock` (default `~/.immorterm/browser.lock`), written
atomically (tmp + rename, same pattern as `remotes.json` / `registry.json`):

```json
{
  "owner_pid":    12345,
  "owner_ws_port": 51730,
  "launch_nonce": "b7f3c1e0-…",
  "browser_pid":  12346,
  "created_at":   1731350000
}
```

- `owner_pid` — the owning **daemon** process (not the browser). Liveness probe
  target.
- `owner_ws_port` — the owner's per-session WS port (from its `registry.rs`
  entry). This is the route target for other daemons.
- `launch_nonce` — random UUID minted at launch. Guards against PID reuse: a
  taker re-reads the lock after acquiring and confirms the nonce is unchanged,
  so two daemons racing to take over a stale lock don't both think they won.
- `browser_pid` — the exact Chromium PID the owner spawned. Only the owner ever
  signals it (kill-what-you-spawned rule).

### Staleness check

A lock is **stale** when either:

1. `owner_pid` is dead — `kill(owner_pid, 0)` returns `ESRCH`; or
2. `owner_pid` is alive but its `registry.rs` entry is gone / its
   `owner_ws_port` no longer answers a WS ping (owner daemon hung or the port
   was reused by an unrelated process).

Fresh lock + owner alive + ws answers ⇒ route to it. Otherwise ⇒ take over.

### Route-vs-own algorithm

On any `immorterm_browser_*` call in a daemon:

```
1. read browser.lock
2. if no lock:
     acquire (write lock with our pid/ws_port/new nonce, atomically)
     re-read; if nonce != ours -> someone won the race -> goto 1 (route)
     launch browser (owner path)
3. if lock exists and owner_pid == self:
     owner path (we already own it)
4. if lock exists and fresh (owner alive + ws answers):
     route path -> send the call to owner_ws_port, await reply, mirror to
     OUR OWN webview canvas, return result
5. if lock exists and STALE:
     attempt takeover:
       - overwrite lock with our identity + new nonce (atomic)
       - re-read; if nonce != ours -> another taker won -> goto 1
       - (best-effort) do NOT signal the old browser_pid — it belongs to a
         dead/other daemon; leave it to its own process-group teardown. We
         only ever kill a browser WE spawned.
       - launch a fresh browser (owner path)
```

Acquisition uses `O_CREAT|O_EXCL` create for the no-lock case and atomic
tmp+rename for takeover; the nonce re-read is the tiebreaker for the residual
race window between rename and read.

### Owner path vs route path — what each returns

- **Owner path:** drive CDP directly, capture, push the frame to **its own**
  webview (`browser_frame` message, see mirroring), return MCP content.
- **Route path:** send a request frame to the owner, receive the owner's MCP
  content (text + base64 PNG) back over the WS, then mirror that PNG to **the
  requesting daemon's own** canvas and return it to the requesting daemon's MCP
  client. Each daemon mirrors to its own webview; the owner does not push to
  other daemons' webviews.

### New WS message pair (websocket.rs framing)

Add to the tagged enums — nothing else changes on the wire:

Request (daemon → owner):

```json
{ "type": "browser_call", "call_id": "…", "tool": "immorterm_browser_click",
  "arguments": { "ref": "ref_9" } }
```

Reply (owner → daemon):

```json
{ "type": "browser_result", "call_id": "…", "ok": true,
  "content": [ {"type":"text","text":"🌐 …"},
               {"type":"image","data":"<base64>","mimeType":"image/png"} ] }
```

`call_id` matches reply to request (same pattern as CDP's `id`). `content` is
the exact MCP content array the owner would have returned locally, so the
requesting daemon returns it verbatim (after mirroring the image locally).

## Transport to the browser: `--remote-debugging-pipe`

Replace `--remote-debugging-port=0` (+ WS + `/json` HTTP) with
`--remote-debugging-pipe`. **No TCP listener exists** — this is *why* only the
owner can drive the browser: CDP travels over pipe fds inherited by the child,
and inherited fds are not reachable by any other process.

### FD wiring

Chromium's pipe mode reads CDP on **fd 3** and writes CDP on **fd 4**
(inherited from the parent). The owner:

- creates two `pipe()` pairs before spawn;
- maps the child ends to fd 3 (child reads = our write end) and fd 4 (child
  writes = our read end) via `CommandExt::pre_exec` (`dup2`);
- keeps the parent ends (write-to-fd-3, read-from-fd-4) on the owner;
- keeps the `Child` handle so `close()` can reap it (unchanged from v1).

`process_group(0)` stays — the whole Chromium tree is in its own group so
teardown signals `-browser_pid` and never touches a process we didn't spawn.

### Pipe framing

CDP over the pipe is **`\0`-terminated JSON** (one JSON-RPC message per NUL
delimiter), not WebSocket frames. So:

- **Send:** `serialize(cmd)` + write bytes + write one `\0` to the write pipe.
- **Receive:** read from the read pipe into a buffer, split on `\0`, parse each
  complete chunk as JSON. Same id-match / event-skip logic as v1's
  `match_cdp_reply` (keep it — it's transport-agnostic). A partial trailing
  chunk (no `\0` yet) stays buffered for the next read.

The per-command timeout / event-discard loop is unchanged in spirit; only the
byte source changes from `ws.next()` to "read next `\0`-delimited frame from the
pipe". Reuse the existing `match_cdp_reply`, `key_spec`, and `parse_devtools_*`
tests where they still apply (`parse_devtools_port` becomes dead — pipe mode
prints no port line; drop it and its tests).

## Retina / CSS-pixel capture

v1 forces DPR=1 via `Emulation.setDeviceMetricsOverride`, which changes what the
user actually sees in their window. Instead, leave the window at its real DPR
and capture in CSS pixels:

1. `Page.getLayoutMetrics` → CSS viewport size (`cssLayoutViewport`).
2. Read `window.devicePixelRatio` via `Runtime.evaluate`.
3. `Page.captureScreenshot` with
   `clip: { x, y, width: cssW, height: cssH, scale: 1/dpr }`.

Result: the screenshot is `cssW × cssH` device pixels — screenshot-px ==
CSS-px, so click coordinates map 1:1 and the visible window is undistorted. All
tool coordinates and the `find`/`click{x,y}` fallback are in these CSS pixels.

## Tool surface + output shapes (ref-based)

Parity target: match the shapes of the official Claude-in-Chrome tools so any
assistant drives ImmorTerm's browser with zero adaptation. Namespaced
`immorterm_browser_*` as our stable vendor-neutral contract. Full
request/response bodies live in `docs/browser-tools.md`; summary:

| Tool | Request | Returns |
|---|---|---|
| `immorterm_browser_open` | `{url}` (scheme-allowlisted) | text caption + PNG |
| `immorterm_browser_read_page` | `{interactive_only?}` | AX-tree text listing, untrusted-framed |
| `immorterm_browser_find` | `{query}` | ranked `[ref_N] role "name"` list, untrusted-framed |
| `immorterm_browser_click` | `{ref}` **or** `{x,y}` (CSS px) | text caption + PNG |
| `immorterm_browser_form_input` | `{ref, value}` | text caption + PNG |
| `immorterm_browser_key` | `{key}` (Enter/Tab/Escape/Backspace/Arrows) | text caption + PNG |
| `immorterm_browser_scroll` | `{dy}` (CSS px) | text caption + PNG |
| `immorterm_browser_screenshot` | `{}` | text caption + PNG |
| `immorterm_browser_close` | `{}` | short text |
| `immorterm_browser_eval` | `{js}` — **gated** `IMMORTERM_BROWSER_EVAL=1` | text |

### ref model

- `read_page` / `find` build an accessibility snapshot (`Accessibility.getFullAXTree`
  or `DOM` + `Accessibility` domains) and assign `ref_N` handles stable within
  that snapshot. Keep a `ref → backendNodeId/box` map on the owner's
  `BrowserSession`.
- Interactive-only filter (default) keeps roles that accept action
  (link/button/textbox/checkbox/combobox/etc.); `interactive_only:false`
  includes static text.
- `click{ref}` resolves ref → element box → center → the existing
  `Input.dispatchMouseEvent` press/release (v1's `click(x,y)` becomes the
  internal primitive). `form_input{ref, value}` resolves ref → node and sets
  value by role (text: focus + `Input.insertText`; checkbox: click to toggle to
  target state; combobox/select: select option). A ref that no longer resolves
  (page navigated) returns the recoverable error string.
- Snapshot invalidation: navigation / a new `read_page` mints a new snapshot and
  clears old refs. Stale ref → `"No element for ref_N — call read_page again;
  the page may have navigated."`

### Output framing (prompt-injection guardrail)

`read_page` / `find` output is wrapped in an explicit untrusted delimiter (see
`docs/browser-tools.md`) AND every tool *description* states page content is
untrusted data, not instructions. This is in-code framing, not a request the
model may ignore.

### Screenshots

Return as MCP image content blocks — identical shape to `immorterm_screenshot`
/ `png_image_content` in `mcp.rs`:
`{ "type": "image", "data": "<base64>", "mimeType": "image/png" }`. Reuse
`png_image_content`; do not re-implement.

### Errors

Short imperative one-liners the model can recover from — see the "When
something goes wrong" section of `docs/browser-tools.md`.

## Security posture (code-enforced)

1. **Navigation scheme allowlist** — `open`/any navigation permits only
   `http`, `https`, `about:blank`. `file:`, `chrome:`, `chrome-extension:`,
   `data:`, `javascript:`, `view-source:` etc. are refused *before* CDP is
   asked. Enforce on `open` and on any in-page navigation the tools trigger.
2. **`eval` demoted + gated** — not registered in the default tool list;
   present only when `IMMORTERM_BROWSER_EVAL=1`. The safe surface
   (find/form_input/read_page/click-by-ref) needs no eval.
3. **Untrusted-content framing** — as above.
4. **Screenshot ephemerality** — signed-in screenshots are LIVE-only:
   - **No disk write.** They are not persisted to any transcript, workshop
     HTML file, or memory. The v1 `mirror_html` path embeds the PNG as a
     `data:` URI inside overlay HTML that persists to
     `~/.immorterm/workshops/…/*.html`; **replace that with the `browser_frame`
     WS message** (WORKFLOW-NOTES.md "browser_frame contract"), which streams
     raw base64 to the live panel and writes nothing to disk.
   - **Memory-sync exclusion.** Browser frames / screenshots are excluded from
     the OpenMemory push path — nothing in `openmemory_push.rs` should ever see
     a browser PNG.
   - The MCP image block returned to the model is the transient exception (the
     model needs to see the page for that step); it is not written to ImmorTerm
     storage by ImmorTerm.

## Canvas mirroring

Owner and each routing daemon mirror to **their own** webview via the
`browser_frame` WS message (`{type, png_base64, title, url, seq}` —
WORKFLOW-NOTES.md). Owner pushes frames from the CDP capture; a routing daemon
pushes the PNG it got back in `browser_result`. `seq` is monotonic per browser
session so the panel drops stale frames. This REPLACES v1's `mirror_html` +
`DrawHtml` overlay path (which persisted PNGs into workshop HTML — the
ephemerality violation above).

## Reuse-vs-new inventory (DRY — the implementer follows this)

**Reuse (do not re-implement):**

| Need | Reuse |
|---|---|
| daemon↔daemon message framing | `websocket.rs` `#[serde(tag="type")]` tagged JSON; add `browser_call`/`browser_result` variants only |
| finding the owner daemon locally | `registry.rs` `SessionEntry.ws_port` |
| reaching the owner remotely | existing `remote.rs` SSH tunnel over that same `ws_port` (tunnel only — not framing) |
| live canvas mirror (no disk) | `browser_frame` WS message, WORKFLOW-NOTES.md contract |
| MCP image content shape | `mcp.rs` `png_image_content()` |
| atomic lock/state file write | tmp + `sync_all` + `rename` pattern from `remote.rs::save_remotes` / registry |
| CDP id-match / event-skip | v1 `browser.rs::match_cdp_reply` (transport-agnostic — keep) |
| key mapping | v1 `browser.rs::key_spec` (keep) |
| exact-PID teardown | v1 `close()` (SIGTERM group → SIGKILL → reap) — keep; only the owner calls it, only on the browser it spawned |
| browser discovery | v1 `find_browser()` (keep) |

**New (must build):**

| Need | New |
|---|---|
| lock file | `browser.lock` read/write/staleness/nonce (schema above) |
| route-vs-own decision | the algorithm above, run at the top of each `immorterm_browser_*` handler |
| pipe transport | `--remote-debugging-pipe` fd wiring + `\0`-delimited framing, replacing the WS+HTTP CDP client |
| ref/AX snapshot | AX-tree snapshot, `ref_N` map, interactive-only filter, ref→box resolution |
| `read_page`/`find`/`form_input` tools | new ref-based handlers |
| CSS-pixel capture | `getLayoutMetrics` + DPR + `captureScreenshot{clip.scale:1/dpr}` |
| scheme allowlist | pre-navigation check |
| eval gate | env-conditional tool registration |
| screenshot ephemerality | `browser_frame` mirror + `openmemory_push.rs` exclusion |

**Drop (v1 dead after cutover):** `--remote-debugging-port`, the `reqwest`
`/json` client + `connect_page_ws`, `parse_devtools_port` (+ its tests), the WS
CDP client in `browser.rs`, `mirror_html` + the `DrawHtml`-overlay mirror path,
`Emulation.setDeviceMetricsOverride` DPR pinning.

## Build gate

CI uses Rust 1.97.0. Build + `clippy` clean under **both** the default
toolchain and 1.97.0 before this ships. Deploy is done by the main session
after review — this spec does not deploy.
