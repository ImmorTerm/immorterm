# CI / Release notes

This repository carries three GitHub Actions workflows, ported from the source
monorepo and trimmed to the components that live here.

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| `.github/workflows/ci.yml` | push / PR | Installs deps, type-checks and tests the TypeScript workspaces, and runs cargo check + clippy on the Rust engine. |
| `.github/workflows/publish-libs.yml` | manual dispatch | Publishes the `@immorterm/*` shared libraries to npm in dependency order. |
| `.github/workflows/promote.yml` | manual dispatch | Publishes the `immorterm` CLI to npm, packages/publishes the VS Code extension, and deploys the docs site. |

## Before any publishing lane will work

**npm Trusted Publishing must be re-configured at this repository's coordinates.**
The publish lanes use OIDC Trusted Publishing (no `NPM_TOKEN`). npm trusts a specific
`owner/repo` + workflow-file pair, so after this repository is created under its new
org you must, on npmjs.com, configure Trusted Publishing for **each** published package
name, pointing at this repo and the workflow file that publishes it:

- `immorterm` (CLI) → `promote.yml`
- every `@immorterm/<lib>` → `publish-libs.yml`

Until that is done, the publish steps will fail with an OIDC/authorization error.

## Secrets / variables the release lanes expect

These were referenced by the source monorepo's promote pipeline and must be recreated
as repository secrets/variables before the corresponding lane is used:

- **Docs deploy** (`promote.yml` docs lane): `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`
  (Cloudflare Pages project `immorterm-docs`).
- **Extension publish** (`promote.yml` extension lane): `VSCE_PAT` (VS Code Marketplace
  personal access token).
- **CLI publish / lib publish**: no token — npm Trusted Publishing (OIDC) as above.

## This repo hosts the companion binaries (release target)

The `immorterm-memory` engine and the MCP gateway ship as prebuilt binaries. Because a
private repository can't serve anonymous release downloads, **this public repository is
the release host for both** — the CLI's `immorterm memory install` and the gateway
download step resolve their assets from this repo's GitHub Releases.

The binaries are *built* in their own repositories (memory is private; gateway is its
own repo). Those repos' promote lanes push the compiled artifacts here with
`gh release upload <tag> <assets> --repo <this-repo>` using a token scoped to this repo.
This repository therefore needs no build lane for them — only the Release itself, plus a
cross-repo upload token configured on the source repos.

The thin memory npm distribution packages live in this repo: `packages/immorterm-memory`
(meta package + postinstall) and the four `packages/immorterm-memory-<platform>` stubs
(no engine source — bin/ is empty until publish). `.github/workflows/publish-memory.yml`
runs on `release: published`, downloads the release's platform tarballs, drops each
binary into the matching `bin/`, and publishes all five via Trusted Publishing. The
`immorterm-ai` npm package (this repo's terminal binary) uses the same postinstall
pattern.

Note: `packages/immorterm-{ai,memory}/postinstall.js` were git-ignored in the source
monorepo by a blanket `*.js` rule and had to be pulled from the working tree. This
repo's `.gitignore` intentionally drops that blanket rule, so both are committed here.

## WASM build strips local paths

`scripts/build-immorterm-ai.sh` now builds the WASM crate with
`RUSTFLAGS=--remap-path-prefix=$HOME=/build`, so future artifacts don't embed
`/Users/<name>` filesystem paths.

The committed WASM binary (`apps/extension/resources/wasm/immorterm_wasm_bg.wasm`) was
rebuilt with this flag — it now contains zero `/Users/` path strings (verified via
`strings`; they are remapped to `/build`).

## VS Code Marketplace publisher

The extension publisher id is `immorterm` (marketplace id `immorterm.immorterm-terminal`).
Before the extension can be published, the founder must create the `immorterm` VS Code
Marketplace publisher account and set a `VSCE_PAT` repo secret scoped to it. There is no
installed base under the old `lonormaly` publisher to migrate.

## Lanes intentionally dropped

The source monorepo's pipeline also deployed the cloud API, web app, memory service,
MCP gateway, AI worker, and knowledge-pack service to Kubernetes / Cloudflare. Those
components do not live in this repository and their lanes were removed. Native-binary
and desktop-app release lanes (GitHub Releases for the engine + Tauri app) were also
left out of the initial port; add them back when the binary build matrix is set up in
this repo.

## VS Code publisher migration (done in-tree 2026-07-08)
Publisher re-pointed `lonormaly` → `immorterm`; canonical extension id is now
`immorterm.immorterm-terminal`, exported once from `libs/services/src/versions.ts`
as `EXTENSION_ID` and imported everywhere (vscode.ts, the CLI install/upgrade
commands, and the extension status bar) — the previously duplicated constants
were collapsed into this single source of truth. Founder cleared breaking the
old installed base (no live users).

**Founder actions before the extension can publish under the new id:**
- Create the `immorterm` publisher on the VS Code Marketplace (+ Open VSX if used).
- Set `VSCE_PAT` secret in ImmorTerm/immorterm for the publish lane.
- The old `lonormaly.immorterm-extension` listing can be left to rot or unpublished.

**Left intentionally (not distribution edges):** author attribution
("name": "lonormaly", README footer), `lonormaly-immorterm` identity slugs
(user_id defaults + identity test fixtures), historical brew/homebrew notes in
UPDATING.md, a session-name example comment in gpu-terminal.ts.

**Minor public-polish TODO (non-blocking):** apps/extension/package.json has a
`screen.binaryPath` setting whose help text still says
`brew install lonormaly/tap/immorterm` — the tap is a 404 and the GNU Screen
path is vestigial (C terminal removed). Consider removing the setting or the
brew line. docs/UPDATING.md is internal-flavored (task IDs, CI line numbers,
"never published" notes) — review whether it belongs in the public repo as-is.

## browser_frame contract (webview browser panel, wave 1)

The webview now renders a dedicated docked browser panel
(`apps/extension/resources/gpu-terminal-browser.js`, loaded via the standard
sidecar pattern from `gpu-terminal.html`; the Tauri app picks it up through the
`apps/immorterm-app/dist` symlink). The daemon's `immorterm_browser_*` tools can
switch their screenshot mirror to it ADDITIVELY in wave 2 by sending this
message over the existing per-session daemon→webview WebSocket:

```json
{ "type": "browser_frame", "png_base64": "<raw base64 PNG, no data: prefix>",
  "title": "<page title>", "url": "<page url>", "seq": 1 }
```

Semantics:
- `seq` is monotonically increasing per browser session; the panel drops
  stale frames (`seq <= last rendered`).
- Each frame REPLACES the previous image (no stacking).
- Panel auto-opens on the first frame (right side, ~45% width, resizable);
  the close button only hides the panel for the webview session — it never
  touches the browser. A "Claude is driving" pulse border shows while frames
  arrive and fades after 3s of silence.
- Frames are handled for ALL sessions, not just the active tab.
- If `browser_frame` never arrives, the panel stays hidden — the existing
  `show_image`/draw fallback keeps working unchanged, so the daemon can cut
  over whenever it's ready.
