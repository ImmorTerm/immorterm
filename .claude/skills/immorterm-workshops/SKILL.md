---
name: immorterm-workshops
description: Use when authoring or interacting with an ImmorTerm workshop — a persistent, interactive HTML panel that survives across response turns and VS Code reloads. Covers open/update/eval/close/list/read, wake-on-click modes, and state-machine patterns.
---

# ImmorTerm — Workshops

A workshop is a **persistent, dedicated panel** rendered in the IMMORTERM tab between the terminal and the sidebar. Unlike `draw_html` overlays (ephemeral, inline, transient), a workshop:

- Survives across response turns
- Survives VS Code reloads (HTML persists to `~/.immorterm/workshops/<session>/<name>.html`)
- Owns its own real estate in the UI (resizable, collapsible, hidable)
- Is idempotent by `name` — re-opening replaces in place without flicker
- Has cross-session read/write APIs (scoped to same project)

Use workshops for: interactive pickers, dashboards, wizards, forms, mini-apps, anything stateful the user will click through.
Don't use for: one-shot visuals tied to your current response (use `draw_html` instead).

## Lifecycle (6 tools)

| Tool | Purpose |
|---|---|
| `immorterm_open_workshop(name, html, css?, on_click_prompt?, on_click_inject_context?)` | Create or replace. Idempotent on `name`. |
| `immorterm_update_workshop(name, html, css?)` | Full HTML replace. Brief flicker. Use for state transitions (step 1 → step 2). |
| `immorterm_eval_in_workshop(name, js)` | Surgical JS inside Shadow DOM. NO flicker. Use for live value updates. |
| `immorterm_close_workshop(name)` | Tear down — removes from state, deletes persisted file, clears panel. |
| `immorterm_list_workshops()` | Returns `[{name, html_size, modified_unix_ms}]`. Discovery. |
| `immorterm_read_workshop(name)` | Returns current html + css. Re-orient on state after compaction or pick up a workshop another session authored. |

## Update tactic — when to use which

- **`update_workshop` (full HTML replace)** — major state transitions (wizard step 1 → step 2). Brief flicker is fine because the UI is meaningfully changing.
- **`eval_in_workshop` (surgical JS)** — live value updates, animating transitions, swapping a label, adding/removing a row, dispatching synthetic clicks. NO flicker. Prefer this for anything where flicker would be jarring.

Rule of thumb: *transitions = update, increments = eval.*

## Wake-on-click — three mechanisms

Workshops with `data-click="LABEL"` buttons can wake the AI when clicked. See the `wake-on-click` skill for full detail. TL;DR:

| Mode | Visible? | Survives reload? | Survives long idle? | Best for |
|---|---|---|---|---|
| **`on_click_inject_context`** (hook) | Tiny `.` per click | ✅ | ✅ | **Default** — stateful workshops, wizards, dashboards |
| Background-bash `wait-event` CLI | Nothing | ❌ | ❌ (24h cap) | Short focused interactions (30 sec picker) |
| **`on_click_prompt`** (PTY-type) | Full prompt visible | ✅ | ✅ | When the synthesized prompt IS the narrative |

For a continuous workshop experience, use `on_click_inject_context`.

## State machine pattern (wizard)

When building a multi-step flow:

1. Open with step 1 HTML + `on_click_inject_context` template that includes the current step + clicked label
2. On click → AI receives `{data_click}` + `html_excerpt` (workshop's current HTML) via additionalContext
3. AI decides next state, calls `update_workshop` with step 2 HTML
4. Repeat

To survive compaction or cross-session pickup, embed a `data-state` attribute on the workshop root that the AI can read via `read_workshop`:

```html
<div data-state='{"step":2,"choice":"hero-2","stage":"customizing"}' style="...">
  ... step 2 UI ...
</div>
```

`read_workshop` returns the html → AI parses out `data-state` JSON → knows exactly where the wizard is.

## CSS + JS

Workshops accept `html` + optional `css`. **JS goes inline as `<script>` tags inside the html** (no separate field). Inline `<style>` tags also work; the `css` parameter is just sugar for one consolidated stylesheet.

Inside a workshop's `<script>` block:
- `root` — Shadow DOM root (use `root.getElementById()`, NOT `document.getElementById()`)
- `wrapper`, `card`, `prim` — same context as `draw_html`

## Cross-session reads

You can read / mutate workshops in other sessions of the SAME PROJECT:

```
immorterm_list_workshops(session="<other-session-id>")
immorterm_read_workshop(session="<other-session-id>", name="...")
immorterm_eval_in_workshop(session="<other-session-id>", name="...", js="...")
```

Cross-project is rejected. Useful when you need to coordinate with a teammate's session or pick up where another AI left off.

## CSP / Shadow DOM constraints

- Inline styles + `<style>` blocks only — no external stylesheets
- `<script>` blocks run with full `unsafe-eval` — `new Function`, `eval`, animation libs all work
- External libs via CDN `<script src>` work (Chart.js, D3, Mermaid all confirmed)
- Max ~64KB per workshop body before performance degrades
- Use the Catppuccin Mocha palette to match terminal theme: `#1e1e2e` base, `#cdd6f4` text, `#89b4fa` blue, `#a6e3a1` green, `#f38ba8` red, `#fab387` orange, `#b4befe` lavender

## Security — eval safety inside workshops

`new Function(...)` and `eval()` work inside workshops because the CSP grants `unsafe-eval`. Two real rules:

1. **The function/eval BODY string must be authored by you (the AI), never built by interpolating user input.**
   - ❌ `new Function('return ' + userTypedExpression)` — injection vulnerability if the user can influence the expression
   - ✅ `new Function('root, wrapper, card, prim', myAuthoredCodeString)` — body is a string literal you wrote
2. **`eval_in_workshop(js=...)` is also AI-authored.** When you compose its `js` argument by formatting user data, escape that data with `JSON.stringify` first so it becomes a string literal in the JS, never raw syntax:
   - ❌ `js="root.querySelector('#name').textContent = '" + userName + "'"` — userName breaks out with a quote
   - ✅ `js="root.querySelector('#name').textContent = " + JSON.stringify(userName)` — userName becomes a quoted JS string literal

If the workshop receives user input via form submit, sanitize before passing it back to eval. The trust model is *AI-authored code OK; user-controlled strings must be JSON-stringified or otherwise escaped before being concatenated into a script*.

## Common patterns by use case

- **Picker** — 3-N cards, each with one button. See `picker` example below.
- **Wizard** — sequential steps, `update_workshop` to transition.
- **Dashboard** — initial layout via open, periodic refresh via `eval_in_workshop`.
- **Form** — inputs + submit button + `on_click_inject_context` containing form data via JS.
- **Approval gate** — list of pending changes, Approve/Cancel buttons.

## Anti-patterns

- One-shot static visual → use `draw_html`, not workshop
- Throwaway picker that user clicks once → background-bash wait-event is OK, but hook-inject still works fine
- Wall of text inside a workshop card → workshops are for INTERACTIVE UI, not for displaying long prose
- Polling `list_workshops` every turn → workshops are stable; only list when discovering or auditing
