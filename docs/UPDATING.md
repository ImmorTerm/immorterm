# Updating ImmorTerm — How Updates Reach Users

Canonical reference for every component's update channel. Grounded in a live audit of
GitHub releases / npm / Marketplace / brew state on 2026-07-07.

## Summary

| Component | Channel | Auto? | User command | Who publishes |
|-----------|---------|-------|--------------|---------------|
| CLI (`immorterm`) | npm `immorterm` (0.1.4 live, Trusted Publishing + provenance) | Hint only (24h check) | `immorterm upgrade cli` | CI: `promote-cli` (OIDC, registry-truth bump) |
| Memory binary | GitHub Releases (`memory-prod-*` tarballs) | No | `immorterm upgrade memory` | CI: `promote-memory` job in `20-promote-prod.yml` |
| Terminal binary (C) | **RETIRED** (see below) | — | — (command removed) | Nobody — apps/terminal deleted 2026-07 |
| AI daemon (`immorterm-ai`) | GitHub Releases (`ai-prod-*`) — coded, never published | Install-if-missing only | none (`immorterm upgrade ai` says "not yet distributable") | CI: `promote-immorterm-ai` — never run |
| Hub (`immorterm-hub`) | Bundled inside the Tauri app | Rides the app updater | none (no independent channel by design) | CI: built as Tauri sidecar in `01-build.yml` |
| VS Code extension | Marketplace `immorterm.immorterm-extension` (1.0.3 live) | Yes — VS Code built-in auto-update | automatic | CI: `promote-extension` (green since 2026-07-07) |
| Tauri desktop app | tauri-plugin-updater ← `latest.json` on GitHub release | Yes (check 2s post-boot, banner → install → relaunch) | none needed | CI: `promote-tauri-app` — never run |
| MCP Gateway | npm `immorterm-mcp-gateway` (0.1.3 live, Trusted Publishing) | Yes (extension 6h poll); CLI users: manual npm | automatic via extension | CI: `promote-gateway` lane (added 2026-07-08; `files:[dist]` fix) |

## Per-component detail

### The promote-prod lever

All publishing routes through `.github/workflows/20-promote-prod.yml`. Each component has a
promote job that takes artifacts from a `01-build.yml` run and pushes them to its channel:

- `promote-cli` (`:369-446`) — AI semver classification, bumps `apps/immorterm/package.json`, `npm publish`; the finalize step commits the bump and tags `cli-vX.Y.Z`, so the next run bumps from the published version.
- `promote-memory` — publishes GitHub release `memory-prod-YYYY-MM-DD.N` with 4 platform tarballs, plus npm `immorterm-memory`.
- `promote-terminal` (`:1063`) — tags `terminal-prod-*` and dispatches a formula update to `lonormaly/homebrew-immorterm` (`:1094`) — **that repo is a 404**, and the failure is swallowed by `|| echo warning`.
- `promote-immorterm-ai` (`:910-1018`) — tags `ai-prod-YYYY-MM-DD.N` with 4 platform tarballs + WASM zip. Never run.
- `promote-extension` (`:467-508`) — `vsce publish` with `VSCE_PAT`. Never succeeded; Marketplace has no listing.
- `promote-tauri-app` (`:1173-1264`) — generates `latest.json` and creates release `desktop-v<version>`. Never run.

### CLI

- **Publish:** npm `immorterm` (0.1.0 live).
- **Update:** `immorterm upgrade cli` runs `npm install -g immorterm@latest` (`apps/immorterm/src/commands/upgrade.ts`). Assumes an npm-based install.
- **Detection:** `getCliVersion()` reads the CLI's own `package.json` at runtime (never hardcoded) and compares against the npm registry (`libs/services/src/versions.ts`).
- **Auto-update config:** `autoUpdate` in `~/.immorterm/config.json` (default: enabled, 24h). Consumed by `maybePrintUpgradeHint()` — at most one npm check per interval, prints a hint on TTY; it never installs anything and never creates a config file for un-initialized users.

### Memory binary (`immorterm-memory`)

- **Publish:** GitHub release assets `immorterm-memory-{macos|linux}-{aarch64|x86_64}.tar.gz`. The live release (`memory-prod-2026-07-07.1`) is **missing the macos-x86_64 asset** — Intel macOS installs throw until a re-promote attaches all 4.
- **Linux floor: glibc 2.38+ (Ubuntu 24.04 / Debian 13).** ort-sys's prebuilt onnxruntime references `__isoc23_*` symbols, so older distros (Debian 12, Ubuntu 22.04, `node:20` images) cannot run the binary — `memory up`/`doctor` preflight explains this instead of timing out. Lowering the floor is task-1783385041480 (manylinux/source-built onnxruntime). CI smoke-tests `ubuntu:24.04` to lock the claimed floor.
- **Install:** `immorterm memory install` / `immorterm init` → `installMemoryBinary()` downloads the newest release carrying this platform's asset into `~/.immorterm/bin/` and stamps `~/.immorterm/bin/.immorterm-memory.version` with the release tag. The stamp is the version source of truth (the binary has no `--version`); pre-stamp installs report `unknown` until upgraded once with `--force`.
- **Update:** `immorterm upgrade memory` — stop daemon → re-download latest release → restart. On download failure it restarts the **old** daemon so the service is never left down.
- **Detection:** installed stamp vs newest release tag (inequality, not semver — tags are dates).

**Daemon safe-restart rule (non-negotiable):** never `lsof -ti:8765 | xargs kill` — that kills every
*client* of the memory service too (live immorterm-ai daemons). Kill only the LISTEN-ing pid:
`lsof -i :8765 | grep LISTEN` → `kill <that-pid>`, or use the pid file (`stopMemory` SIGTERMs
only the pid-file pid — the pattern all tooling must follow).

### Terminal binary (C) — RETIRED

- **Removed 2026-07:** `apps/terminal/` (the GNU Screen fork) was deleted, along with its CI build/promote lanes, the `Terminal` upgrade component, doctor's "C Binary" check, and the CLI's brew-install flow. The only terminal is the Rust engine (`immorterm-ai`). The frozen brew formula `lonormaly/tap/immorterm` should be deprecated tap-side (it also collides with the npm CLI bin — task-1783509719522).

### AI daemon (`immorterm-ai`)

- **Not yet distributable.** No `ai-*` release has ever been published; npm `@immorterm/ai` is 404; no brew formula. Users get the daemon only via local builds (`deploy-daemon` skill, incl. codesign).
- **Coded-but-dormant path:** the extension's `downloadDaemonBinary()` fetches the newest `ai-*` release asset if no binary is found — install-if-missing only, no version check, so even after a first release there is no *update* path.
- `immorterm upgrade ai` prints an honest "not yet distributable" message pointing here.

### Hub (`immorterm-hub`)

Bundled as a Tauri `externalBin` sidecar; a new app bundle ships a new hub. No independent
channel and none needed — but it inherits the Tauri chain's state (never published).

### VS Code extension

- **Never published** — Marketplace query for `immorterm.immorterm-extension` returns nothing, so VS Code's built-in auto-update has never been exercised and `code --install-extension immorterm.immorterm-extension --force` fails for real users.
- **Dev path:** the `deploy-extension` skill copies `out/` + `resources/` into `~/.vscode/extensions/immorterm.immorterm-extension-1.0.3/`. Latent trap: the dir name hardcodes 1.0.0 while `package.json` is 1.0.1.

### Tauri desktop app

- **Client chain is complete:** `tauri-plugin-updater` with pinned pubkey; check fired 2s post-boot from `tab-shell.html` → banner → `downloadAndInstall()` → relaunch. Check failures are swallowed, so a broken feed degrades silently.
- **Signing dependency (hard requirement):** without `TAURI_SIGNING_PRIVATE_KEY` in CI (`01-build.yml:990`), `tauri build` succeeds but emits no `.sig`; `promote-tauri-app` then hard-fails at its signature gate (`20-promote-prod.yml:1187-1190`) — and even if it didn't, clients reject unsigned updates against the pinned pubkey. **No signing key = no update ever ships, by design.**
- **`/releases/latest` steal hazard:** the updater endpoint is `releases/latest/download/latest.json` — repo-**global** latest. Any memory/ai/terminal release published after a desktop release steals "latest" and 404s the feed (none of the other `gh release create` calls pass `--latest=false`: `20-promote-prod.yml:577,972,1065`). Before the first desktop release: pin the feed to a fixed tag or add `--latest=false` everywhere else.

### MCP Gateway

- **Auto-update exists but is doubly broken:** the extension polls npm every 6h and runs `npm i -g immorterm-mcp-gateway@latest` on a newer version — but (1) no CI job publishes the package, so it never sees one, and (2) `findGatewayBinary()` prefers the vsix-bundled copy over the npm-global copy the updater installs, so a successful update is a no-op for bundled installs.
- **Tauri sidecar manifest** (`manifests/sidecars.json`) is placeholder: nonexistent-org URLs and literal `TBD-<triple>` sha256 values — reconcile-on-launch always aborts until it carries real URLs/hashes.

## How update detection works

Everything gates through `libs/services/src/versions.ts` (`getAllVersions()`):

- **CLI** — own `package.json` vs npm registry.
- **Memory** — install-time release-tag stamp vs newest platform-matching GitHub release.
- **Terminal / AI** — GitHub release tags with the CI prefixes `terminal-prod-` / `ai-prod-`. Date-stamped tags aren't semver; a NaN guard in `compareVersions` ensures they never produce a bogus `updateAvailable`.
- **Extension** — `code --list-extensions` vs the Marketplace gallery API (moot until published).
