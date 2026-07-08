---
name: immorterm-workshop-cross-session
description: Use when coordinating across multiple ImmorTerm sessions of the same project — reading another session's workshops, mutating them, or picking up where another agent left off. Scoped to same-project; cross-project is rejected.
---

# ImmorTerm — Cross-session workshop access

Every workshop tool accepts a `session` parameter that defaults to your own session but can target **any other session in the same project**. Cross-project access is rejected with a clear error.

## When this applies

- Multiple Claude Code agents are working in different IMMORTERM tabs of the same project (e.g., one tab building UI, another tab running tests, a third planning architecture)
- The user opened a workshop in tab A and wants you (in tab B) to pick it up
- You want to mirror state across sessions (e.g., a "team status" workshop visible in every session of the project)
- An agent in another session needs to dispatch work to yours (or vice versa)

## Discovery

```python
# Find other sessions in this project
immorterm_list_sessions(status="alive")
# → returns [{id, name, pid, project_dir, ...}, ...]

# See what workshops are open in another session
immorterm_list_workshops(session="<other-session-id>")
# → returns [{name, html_size, modified_unix_ms}, ...]

# Read the current state of a specific workshop
immorterm_read_workshop(session="<other-session-id>", name="planner")
# → returns {name, html, css, modified_unix_ms, _note}
```

## Mutation

```python
# Update a workshop owned by another session
immorterm_update_workshop(session="<other-id>", name="...", html="...")

# Inject JS into another session's workshop (e.g., flash a highlight)
immorterm_eval_in_workshop(
    session="<other-id>",
    name="planner",
    js="root.querySelector('#status').textContent = 'agent-B finished';"
)

# Open a NEW workshop on another session
immorterm_open_workshop(session="<other-id>", name="from-agent-b", html="...")

# Close another session's workshop
immorterm_close_workshop(session="<other-id>", name="...")
```

## Same-project scope enforcement

The daemon reads `~/.immorterm/registry.json` to compare `project_dir` of caller (from `$IMMORTERM_SESSION_NAME` env) and target session.

- Allow: target == caller OR both have the same `project_dir`
- Reject: different `project_dir` values, with error message naming both projects
- Allow (backward compat): either session unregistered, or caller identity unknown (e.g. test harness without env var)

There is currently NO opt-in for cross-project access. If you legitimately need it, that's a feature request — file it; don't bypass.

## Patterns

### Picking up an abandoned workshop

```python
# 1. Discover
sessions = immorterm_list_sessions(status="alive")
# Pick the one you care about (e.g., the user's original tab)

# 2. See what's there
workshops = immorterm_list_workshops(session=target)

# 3. Read state
state = immorterm_read_workshop(session=target, name=ws_name)
# state.html may include a data-state attribute with JSON wizard state

# 4. Continue where they left off — re-render with next step
immorterm_update_workshop(session=target, name=ws_name, html=next_step_html)
```

### Multi-agent coordination

Agent A (UI builder) opens a "build queue" workshop on its session. Agent B (test runner) reads it via cross-session, picks the next item, runs tests, calls `eval_in_workshop` on A's workshop to mark the item green/red.

```python
# Agent B reads A's queue
queue = immorterm_read_workshop(session=agent_a_session, name="build-queue")
# Parse out items from the HTML, pick next pending one

# Run tests... then update A's workshop
immorterm_eval_in_workshop(
    session=agent_a_session,
    name="build-queue",
    js=f"root.querySelector('[data-item={item_id}]').classList.add('passed');"
)
```

### Shared status panel

Open the same-named workshop on every session in the project. Each session's `update_workshop` calls keep it in sync (last write wins). Users see the same "team status" wherever they are.

## Gotchas

- **`read_workshop` returns the last full-write state**, not live eval mutations. If agent A used `eval_in_workshop` to mutate the DOM, agent B's `read_workshop` won't see those changes — only what was passed to `open_workshop` / `update_workshop` last. To capture live DOM, you'd need a roundtrip to the webview (not implemented).
- **Workshop persistence is per-session-folder on disk**. A workshop "todo" on session A is a different file from a workshop "todo" on session B, even though both have the same name. They're separate by design.
- **The display panel renders only the active session's workshops**. Webview filters strictly by `sessionName === activeSessionName`. So even if you open a workshop on session A from session B's context, the user must switch to A's tab to see it.

## Discovering project dir programmatically

```python
# Use list_sessions to find your own session's project_dir
me = [s for s in immorterm_list_sessions() if s.name == os.environ["IMMORTERM_SESSION_NAME"]][0]
my_project = me["project_dir"]
# Then filter list_sessions to only sessions where project_dir == my_project
```
