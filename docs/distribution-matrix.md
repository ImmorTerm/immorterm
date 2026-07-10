# ImmorTerm distribution matrix — what you install, what happens next

ImmorTerm is four independent pieces. Any combination works; each adds a
capability. This doc is the state machine: every starting state, every
transition, and what the user must do (usually: nothing).

| Piece | Distributed as | What it gives you |
|---|---|---|
| **CLI** (`immorterm`) | npm (global) | `init` onboarding, hooks installer, service management, doctor |
| **Memory engine** (`immorterm-memory`) | Prebuilt binary (GitHub Releases; npm wrapper) | Local semantic memory: capture, search, recall (SQLite + ONNX, port 8765) |
| **MCP Gateway** (`immorterm-mcp-gateway`) | npm (global) | One long-lived MCP proxy shared by all AI sessions (port 9100) |
| **VS Code extension** (`immorterm.immorterm-extension`) | Marketplace / `.vsix` | Persistent GPU terminals in VS Code + GUI onboarding for everything above |
| **Desktop app** (ImmorTerm) | DMG / installer (Tauri) | Standalone GPU terminal app; bundles its own daemon + hub |
| **Self-driven browser** | Built into the terminal daemon (nothing to install) | Claude drives a real Chromium-engine browser via `immorterm_browser_*` tools; live frames mirror onto the terminal canvas ([guide](browser.md)) |

The source of truth for what's active in a project is
**`<project>/.immorterm/config.json`** — `services.memory.enabled`,
`services.mcpGateway.enabled`, theme, project id. Every surface reads and
writes the same file; there is no per-surface state.

**The daemons are self-healing.** Once a service is `enabled`, a SessionStart
hook (installed by any surface) health-checks it at the start of every AI
session and auto-spawns it if it's down. `immorterm memory up/down` exist as
low-level primitives — normal users never run them.

---

## States and transitions

### S0 — nothing installed

| You do | You get |
|---|---|
| `npm install -g immorterm && immorterm init` | Interactive wizard (terminal → theme → memory? → gateway? → license?). Writes config.json, installs hooks, downloads the memory binary if enabled. **One command, fully working.** |
| Install the VS Code extension | State S-VSC below — the extension carries its own onboarding; the CLI is optional. |
| Install the desktop app | State S-TAURI below — the app bundles its daemon; CLI optional. |

### S-VSC — VS Code extension only (no CLI, nothing else)

- **Open any new project/window** → after ~2s a modal asks *"Enable ImmorTerm
  for this project?"* → **Enable** runs the wizard (theme → memory? →
  gateway? → license?). *Decline is remembered per-project; re-enable later
  via Command Palette → "ImmorTerm: Enable for This Project".*
- Enabling **memory** plug-and-plays everything: downloads the binary from
  GitHub Releases, starts the daemon, registers MCP, installs hooks. No CLI
  needed.
- Enabling **gateway** installs/starts it and rewrites AI tool configs to
  proxy through it (needs one VS Code restart).
- Terminals: the extension's GPU terminal is available immediately;
  sessions survive reload/crash.

### S-MEM — memory only (no ImmorTerm terminal)

You want recall in Claude Code but keep your own terminal:
`npm i -g immorterm && immorterm init` → enable **memory**, decline the rest.
Hooks are Claude-Code-level, so capture/recall works in *any* terminal
(Terminal.app, iTerm, VS Code's built-in). The self-heal hook keeps the
daemon alive; nothing else to run.

### S-GW — gateway only

`npm i -g immorterm-mcp-gateway && immorterm-mcp-gateway start` — standalone,
zero ImmorTerm coupling. Useful on its own for sharing MCP servers across
sessions/tools.

### S-TAURI — desktop app (macOS)

- Install the app → open a project folder. The bundled daemon probes for the
  `immorterm` CLI:
  - **CLI present** → memory hooks are auto-wired into the project silently.
  - **CLI absent** → a one-time banner suggests `npm install -g immorterm`
    (shown once per project, then never again).
- The app's terminal + canvas work with no other pieces installed.
- Updates: the app checks GitHub Releases (`latest.json`) and self-updates.

### S-BROWSER — self-driven browser

Ships inside the terminal daemon — present in every state that has an
ImmorTerm terminal (S-VSC, S-TAURI); no separate install or flag. The only
requirement is any Chromium-engine browser on the machine (Chrome, Brave,
Edge, Chromium). If none is found, the first `immorterm_browser_open` call
fails with a clear error naming what to install; every other capability
keeps working.

### Composite states (the common upgrade paths)

| From | You do | Result |
|---|---|---|
| S-VSC | Open a **second/new project** in a new window | The enable-modal fires again for that project only. Per-project isolation is the design. |
| S-MEM | Later install the VS Code extension | Extension reads the same config.json — no re-onboarding; memory keeps working. |
| S-VSC (memory off) | Flip `services.memory.enabled: true` (or Command Palette → Configure Services) | Next session start, the self-heal hook downloads/starts the daemon. |
| Any | `immorterm doctor` | One command that verifies every installed piece and says what's missing. |
| Any | `immorterm disable` / extension "Disable for Project" | Hooks + MCP entries removed for that project; daemons left for other projects. |

---

## Update matrix (who phones where)

| Piece | Check | Cadence | Applies how |
|---|---|---|---|
| CLI | npm registry | 24h | prints upgrade hint (`npm i -g immorterm`) |
| Extension | Marketplace | VS Code built-in | auto |
| Gateway | npm registry (extension polls) | 6h | auto-updated by extension; or `immorterm upgrade` |
| Memory binary | GitHub Releases | on `immorterm upgrade` / extension | downloads newer release *(auto-update wiring in progress)* |
| Desktop app | GitHub Releases `latest.json` | on launch | Tauri self-update (signed) |

Phone-home surfaces, exhaustively: the version checks above, and license
validation against `api.immorterm.com` when a Pro key is entered. Nothing else.

---

## Known gaps (honest list)

- Memory binary **auto**-update from the extension is stubbed, not wired.
- Intel-Mac (`darwin-x64`) memory binary deferred — `ort` ships no prebuilt
  ONNX Runtime for it (Apple Silicon + Linux x64/arm64 are live).
- `immorterm memory up` is still suggested in one CLI error message; it
  should point at `immorterm enable` instead (self-heal handles the rest).
- Cold-start edge: if config says memory `enabled` but the binary was never
  installed, self-heal starts nothing — the wizard paths always install it,
  but hand-edited configs can hit this. Planned: self-heal downloads on miss.
