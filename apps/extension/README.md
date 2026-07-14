<p align="center">
  <img src="https://raw.githubusercontent.com/ImmorTerm/immorterm/main/apps/extension/resources/icon.png" alt="ImmorTerm" width="128">
</p>

# ImmorTerm

**Your terminal sessions survive VS Code crashes and restarts — and your AI remembers every session.**

Kick off a long build, then watch VS Code crash. Reopen it: the terminal is exactly where you left it — same scrollback, same running process — and it reconnects on its own. Your sessions live in a separate persistent process (the native ImmorTerm engine), so when the editor dies, they don't notice.

Optionally, ImmorTerm also gives your Claude Code sessions a local memory. Decisions, bug root-causes, and code changes are captured on your machine and recalled the next time you (or Claude) start work — the context is already there before you ask.

## Features

- **Persistent sessions** — terminals outlive VS Code crashes, restarts, and updates
- **Auto-reconnect** — sessions reattach when you reopen VS Code; scrollback and live processes intact
- **GPU-rendered terminal** — the ImmorTerm engine (Rust + WebGPU) renders fast and smooth
- **Zero configuration** — works out of the box with sensible defaults
- **Themed status bar** — terminal count and session health at a glance
- **AI memory** *(optional)* — persistent, local memory for Claude Code across sessions

## Installation

**1. Run the setup wizard** (installs the extension and the terminal engine — no separate binary to install):

```bash
npx immorterm init
```

**2. Open your project in VS Code.** ImmorTerm asks *"Enable ImmorTerm for this project?"* — click **Enable**, pick a status-bar theme, and your terminals are persistent.

That's it. Open terminals as usual; ImmorTerm handles the rest.

> You can also install the extension directly from the VS Code Marketplace or Open VSX by searching for **ImmorTerm**, then run `npx immorterm init` to finish setup.

## Usage

Open the Command Palette (`Cmd+Shift+P` / `Ctrl+Shift+P`) and type "ImmorTerm":

| Command | Description |
|---------|-------------|
| **ImmorTerm: New Terminal** | Open a new persistent terminal |
| **ImmorTerm: Show Status** | View all sessions and their health |
| **ImmorTerm: Reattach Shelved Terminal** | Bring back a shelved session |
| **ImmorTerm: Rename Terminal** | Rename the active terminal |
| **ImmorTerm: Forget Current Terminal** | Stop persisting the active terminal |
| **ImmorTerm: Forget All Terminals** | Stop persisting all terminals |
| **ImmorTerm: Cleanup Stale Sessions** | Remove orphaned sessions |
| **ImmorTerm: Enable for This Project** | Set up ImmorTerm (theme + memory) |
| **ImmorTerm: Disable for This Project** | Remove ImmorTerm and its files |
| **ImmorTerm: Set Theme for This Project** | Change the status-bar theme |
| **ImmorTerm: Configure Memory Services** | Enable or disable AI memory |
| **ImmorTerm: Run Memory Doctor (Diagnostics)** | Check memory service health |

### Keyboard shortcuts

| Shortcut | Command |
|----------|---------|
| `Ctrl+Shift+Backtick` | New Terminal |
| `Ctrl+Shift+Q Q` | Forget Current Terminal *(terminal focused)* |
| `Ctrl+Shift+Q A` | Forget All Terminals *(terminal focused)* |
| `Ctrl+Shift+T` | New Task |

## AI Memory for Claude Code *(optional)*

Run Claude Code inside an ImmorTerm terminal and it gets a memory that persists across sessions — all local, no cloud, no Docker.

**What it does:**

1. **Recall on start** — when Claude starts, relevant memories from past sessions load into its context automatically, in about 8ms.
2. **Captures decisions** — approved plans, architectural decisions, and code changes are stored as you work.
3. **Semantic search** — Claude searches past sessions by meaning, not just keywords.
4. **Project isolation** — each project has its own memory partition; no cross-project bleed.

**How it works:** ImmorTerm installs [Claude Code hooks](https://docs.anthropic.com/en/docs/claude-code/hooks) that run on session start and on plan approval. Memories live in a native memory binary (~15 MB) in `~/.immorterm/bin/` — SQLite storage with on-device embeddings, served to Claude over MCP. When you start a session, Claude receives a block like:

```
<memory-context>
## decision (2026-02-09)
We chose JWT with rotating refresh tokens for auth. Implementation in src/auth/jwt-handler.ts.

## decision (2026-02-08)
Database uses soft deletes with deleted_at timestamps.
</memory-context>
```

**Enable it:**

1. Run **ImmorTerm: Configure Memory Services** (or choose it during `npx immorterm init`).
2. The native memory binary downloads to `~/.immorterm/bin/` and hooks are installed automatically.
3. The status bar shows 🧠 when memory is active. Run **ImmorTerm: Run Memory Doctor** to check health.

To turn it off, run **ImmorTerm: Configure Memory Services** again. Your stored memories are preserved and can be re-enabled later.

## Configuration

Settings live under **Settings → Extensions → ImmorTerm**.

### Sessions

| Setting | Default | Description |
|---------|---------|-------------|
| `immorterm.restoreOnStartup` | `true` | Auto-reconnect sessions when VS Code starts |
| `immorterm.terminalRestoreDelay` | `200` | Delay (ms) between session reconnections |
| `immorterm.scrollbackBuffer` | `50000` | Lines kept in the scrollback buffer |
| `immorterm.historyOnAttach` | `20000` | Lines shown when a session reattaches |
| `immorterm.closeAction` | `shelve` | What happens when you close a terminal |
| `immorterm.shelvedSessionTtl` | `24` | Hours a shelved session is kept before cleanup |

### Behavior

| Setting | Default | Description |
|---------|---------|-------------|
| `immorterm.autoCleanupStale` | `true` | Auto-clean orphaned sessions |
| `immorterm.closeGracePeriod` | `60000` | Wait (ms) before cleanup on close |
| `immorterm.statusBarEnabled` | `true` | Show the status-bar item |
| `immorterm.statusBarTheme` | `Purple Haze` | Status-bar theme |
| `immorterm.namingPattern` | `immorterm-${n}` | Pattern for terminal names |

### Memory & logs

| Setting | Default | Description |
|---------|---------|-------------|
| `immorterm.services.memory.enabled` | `false` | Enable local AI memory for Claude Code |
| `immorterm.services.mcpGateway.enabled` | `false` | Enable the shared MCP gateway |
| `immorterm.maxLogSizeMb` | `300` | Max total log size before cleanup |
| `immorterm.logRetainLines` | `50000` | Lines to keep when truncating logs |
| `immorterm.enableDebugLog` | `false` | Verbose logging |

## Troubleshooting

**Sessions not reconnecting**
- Check the status bar shows a session count. Run **ImmorTerm: Show Status** for details.
- Ensure `immorterm.restoreOnStartup` is `true`.
- Open the Output panel (**View → Output**), select "ImmorTerm", and check for errors.

**Status bar not showing**
- Ensure `immorterm.statusBarEnabled` is `true`, then run **Developer: Reload Window**.

**Memory not working**
- Run **ImmorTerm: Run Memory Doctor (Diagnostics)** — it reports which service is down and why.
- Confirm `immorterm.services.memory.enabled` is `true`.
- Look for 🧠 in the status bar; if it's missing, run **ImmorTerm: Configure Memory Services** to re-enable.

**Performance**
- Lower `immorterm.scrollbackBuffer` (e.g. `10000`) and `immorterm.historyOnAttach` (e.g. `5000`).

## How it works

1. **Separate process** — each terminal runs against the native ImmorTerm engine, a persistent process independent of VS Code.
2. **Editor dies, session doesn't** — when VS Code crashes or restarts, the engine keeps running with your scrollback and live processes.
3. **Reattach** — on restart, ImmorTerm reconnects to the running sessions. Nothing is restored, because nothing was lost.
4. **Cleanup** — stale sessions are cleaned up on a schedule (configurable).

## Data storage

- **VS Code workspace state** — the session registry (survives restarts)
- `.immorterm/` (per project) — session registry, hooks, and logs
- `~/.immorterm/` — global config and, if memory is enabled, the memory binary and its SQLite store

## Requirements

- macOS or Linux
- VS Code
- Optional AI memory: no extra install — the memory binary downloads automatically when you enable it

## Contributing

Issues and pull requests welcome at [github.com/ImmorTerm/immorterm](https://github.com/ImmorTerm/immorterm).

## License

See [LICENSE](LICENSE) for details.

---

Mort keeps things running. It's not hard when you're an axolotl. — **ImmorTerm**
