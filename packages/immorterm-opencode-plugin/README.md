# @immorterm/opencode-plugin

ImmorTerm bridge for [opencode](https://opencode.ai). Maps opencode plugin
SDK events to Claude-shape hook envelopes and forwards them to the
ImmorTerm hook pipeline so opencode sessions get the same digest, memory,
and registry treatment as Claude Code.

opencode is the only supported AI tool that lacks stdin/stdout hooks —
its plugin SDK is in-process TypeScript only. This package is the bridge.

## Install

```bash
npm install -D @immorterm/opencode-plugin
```

The ImmorTerm installer wires this up automatically when opencode support
is enabled (`services.vendors.opencode.enabled = true`). The installer
adds the plugin reference to your project's `opencode.json`:

```json
{
  "plugin": ["@immorterm/opencode-plugin"]
}
```

## How it works

| opencode event                       | Claude `hook_event_name` | Notes                                 |
| ------------------------------------ | ------------------------ | ------------------------------------- |
| `chat.message` (role: user)          | `UserPromptSubmit`       | Concatenates text parts as `prompt`.  |
| `chat.message` (role: assistant)     | _(dropped)_              | No Claude equivalent.                 |
| `tool.execute.before`                | `PreToolUse`             | `tool_name` lifted from event.        |
| `tool.execute.after`                 | `PostToolUse`            | `tool_response` carries title/output. |
| `session.created`                    | `SessionStart`           | Via the catch-all `event` hook.       |
| `session.compacted`                  | `PreCompact`             | Plus `experimental.session.compacting`. |
| `session.deleted`                    | `Stop`                   | Via `event` hook.                     |
| `file.edited`                        | `PostToolUse(Edit)`      | Uses last-seen sessionID.             |
| `permission.ask`                     | _(dropped)_              | No Claude equivalent yet.             |
| Other (lsp, todo, vcs, message.part) | _(dropped)_              | Noise — not relevant for digest.      |

Each envelope is written atomically as JSON to:

```
<project>/.immorterm/hooks/inbox/opencode-<ts>-<rand>.json
```

The ImmorTerm hooks daemon picks up files from this inbox and forwards
each one to the same shell hook script the other vendors invoke directly.
This file-drop pattern was chosen over a new HTTP route on the hub
because (a) it's observable for tests, (b) it survives hub restarts, and
(c) it doesn't require Phase A scope expansion.

## Develop

```bash
bun install
bun run build
bun test
```

## Notes / TODOs

- The peer dependency is declared as `@opencode-ai/plugin` (the actual
  npm package name verified against
  https://github.com/sst/opencode/blob/main/packages/plugin/package.json).
  We don't import its types directly to keep this package buildable
  standalone in CI; runtime shapes are captured locally in `src/index.ts`
  and `src/envelopes.ts`.
- The inbox watcher (the daemon side) is tracked separately — Phase A's
  observable verification is "the JSON file lands in the inbox."
