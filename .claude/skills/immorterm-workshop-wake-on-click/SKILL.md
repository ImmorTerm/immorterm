---
name: immorterm-workshop-wake-on-click
description: Use when you need the AI to react to a user click in a workshop or draw_html overlay. Documents the three wake-up mechanisms (hook-inject, background-bash, PTY-type) with code examples and selection criteria.
---

# ImmorTerm — Wake on click

Three mechanisms let the AI wake when a user clicks a `data-click` button in a workshop or `draw_html` overlay. Pick by use case.

## TL;DR matrix

| Mode | Visible artifact | Survives VS Code reload? | Survives long idle? | Best for |
|---|---|---|---|---|
| **Hook-inject (`on_click_inject_context`)** | Tiny `.` per click | ✅ | ✅ | **Default** for stateful workshops/dashboards/wizards |
| Background-bash CLI | Nothing | ❌ | ❌ (24h cap) | Short focused interactions (≤24h, no reload risk) |
| PTY-type (`on_click_prompt`) | Full prompt as if you typed | ✅ | ✅ | When the synthesized prompt IS the desired narrative |

---

## Mode 1: Hook-inject — the daily-driver default

```python
immorterm_open_workshop(
    name="picker",
    html="<button data-click='hero-1'>Minimal</button>...",
    on_click_inject_context=(
        "User clicked button '{data_click}' in workshop '{name}'. "
        "Reply with one short line acknowledging the choice and call "
        "eval_in_workshop to highlight the chosen card."
    )
)
```

### What happens per click

1. Daemon writes a marker JSON to `~/.immorterm/pending-click/<session>.json` containing:
   - The formatted template (after `{data_click}` / `{name}` substitution)
   - The workshop name + clicked label + `available_buttons` list + `html_excerpt` (~3.2KB cap)
2. Daemon types a single `.` + CR into Claude's PTY
3. Claude Code fires `UserPromptSubmit` hooks
4. The `immorterm-workshop-click.sh` hook reads the marker, deletes it, emits a rich `additionalContext` block with all the metadata
5. Claude sees the `.` + the additionalContext → reacts

### Trade-offs

- ✅ Survives VS Code reload (workshop persists, click triggers fresh hook fire)
- ✅ Survives indefinite idle (no listener subprocess to time out)
- ✅ Cross-session safe (each session has its own marker filename)
- ⚠️ One `.` per click in scrollback — minimal but visible
- ⚠️ Requires the `immorterm-workshop-click` hook installed in `~/.claude/settings.json` (the extension installs it; verify with `ls ~/.claude/hooks/immorterm-workshop-click.sh`)

### Placeholders in the template

- `{data_click}` — the clicked element's `data-click` attribute value
- `{name}` — the workshop name (or `{id}` for draw_html primitives)

---

## Mode 2: Background-bash — invisible but fragile

For a short interaction where you want zero terminal artifact and you're confident the user will click within a reasonable window:

1. Author the UI with `data-click="LABEL"` buttons (workshop or draw_html).
2. Run via the `Bash` tool with `run_in_background: true`:
   ```
   ~/.immorterm/bin/immorterm-ai wait-event <SESSION_ID> --type click --timeout 86400000
   ```
   Optional filters: `--name <label>`, `--id <primitive-id>`. Cap was raised from 5min to 24h (86_400_000 ms).
3. **End your turn.** Conversation is free.
4. On click: subprocess exits with JSON like `{"data_click":"hero-2","name":"picker","type":"workshop_clicked"}`, Claude Code's `<task-notification>` fires, you wake up, read the output file, and react.

### Trade-offs

- ✅ Truly invisible — zero scrollback artifact
- ❌ Subprocess dies on VS Code reload — next click lost until you re-arm
- ❌ Timeout cap (24h max daemon-side; Claude Code's background-task lifetime may be shorter)
- ❌ Re-arming after a click requires another Bash call — more ceremony per cycle

Use ONLY when you'd otherwise type "any feedback?" and wait — i.e. short, in-the-moment.

---

## Mode 3: PTY-type — when the prompt IS the narrative

```python
immorterm_open_workshop(
    name="commit-picker",
    html="...",
    on_click_prompt="User picked commit message: {data_click}. Show the diff before committing."
)
```

On each click, daemon types the formatted template into Claude's PTY as if you typed it. Two-stage write (text → 80ms → CR) defeats Claude Code's paste-detection so the CR auto-submits.

### Trade-offs

- ✅ Survives reload + idle
- ⚠️ Full formatted prompt visible in conversation history (intentional — it BECOMES a user turn)
- Use when: the verbose prompt is informative ("User picked: hero-2. Bold design with pink gradient. React.") — it reads naturally in the transcript.

---

## Decision tree

```
  Will the user keep clicking after VS Code might reload?
    └── Yes  → not background-bash. Hook-inject or PTY-type.
        └── Should the prompt appear in transcript as user-style text?
            ├── Yes → on_click_prompt
            └── No  → on_click_inject_context (default)
    └── No (short, one-shot, < few minutes) → background-bash for cleanest UX
```

## Anti-patterns

- `immorterm_wait_for_event(background=true)` — **decorative no-op**. Registers no listener daemon-side. Events fire and vanish.
- `immorterm_wait_for_event(background=false)` — works but blocks the MCP tool call for ≤5 min; turn looks hung.
- `immorterm_poll_events` — drains the per-session ring queue. NOT a wake mechanism — you only see events when you call it.

## Verifying the hook is installed

```bash
ls -la ~/.claude/hooks/immorterm-workshop-click.sh
grep workshop-click ~/.claude/settings.json
```

If missing, the extension didn't install it for this machine. See the `hook-installer` work in `apps/extension/src/services/memory/hook-installer.ts` — or copy it manually from the project's `.claude/hooks/`.
