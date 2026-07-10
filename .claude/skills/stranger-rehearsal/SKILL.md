---
name: stranger-rehearsal
description: "Run the launch acceptance ritual: prove a STRANGER can still install and use ImmorTerm end-to-end from the real published channels (npm CLI, memory binary, MCP gateway, terminal daemon). MUST be run after publishing any package (promote_cli / promote_gateway / promote_memory), before announcing a release, and whenever onboarding code changes (init, memory install, hook-installer, doctor). The goal sentence this guards: 'a stranger installs ImmorTerm + Memory with one command.'"
allowed-tools: Bash, Read
---

# Stranger Rehearsal — the launch acceptance ritual

Two containers, two commands, zero mercy. Everything runs against the REAL
published artifacts (npm registry, GitHub releases), never the working tree —
if it's green here, it's green for a stranger on the internet.

First green: 2026-07-08 (9/9 + remote chain OK). Keep it that way.

## Part 1 — the stranger container (CLI + memory + gateway)

```bash
docker run --rm -v $PWD/ops/rehearsal/stranger-e2e.sh:/e2e.sh ubuntu:24.04 bash /e2e.sh
```

Takes ~6-10 min (first boot downloads ~150MB of ONNX models). Exit code =
number of failures; the script prints a PASS/FAIL verdict and, on full green:
`🏁 GOAL SENTENCE IS TRUE`.

What it proves, in order:
1. node 20 installs (stranger baseline)
2. `npm install -g immorterm` from the real registry
3. `immorterm init --yes` in a fresh git repo → 17 hooks + settings.local.json + MCP registration
4. `immorterm memory install` → binary downloads (API path, or pinned-tag CDN fallback when api.github.com 403s)
5. `immorterm memory up` → `/health` ok (poll up to 300s — model download)
6. REAL round-trip: POST a memory, semantically recall it (`output_mode:"full"`, and `user_id` is REQUIRED on API writes — identity model, not a bug)
7. `npm install -g immorterm-mcp-gateway` → `immorterm-mcp-gateway start --foreground` → `/health` on :9100
8. `immorterm doctor` exits 0 (warnings ok, failures not)

## Part 2 — the terminal engine (headless daemon + remote chain)

```bash
bash scripts/remote-test-container.sh up
bash scripts/remote-test-container.sh verify
bash scripts/remote-test-container.sh down   # always clean up
```

Proves the daemon that powers the Tauri app: container boots, demo session
registers, and the full remote chain passes — test → registry → attach —
ending with "the Tauri picker 'Open Project → docker' will connect."
(The GUI shell itself is desktop-only; this is the engine it talks to.)

## Part 3 — browser (optional)

```bash
bash ops/rehearsal/browser-e2e.sh
```

Proves the daemon's self-driven browser (`immorterm_browser_*` MCP tools):
spawns the installed daemon's stdio MCP server (`immorterm-ai mcp serve`),
serves a local fixture form, and runs open → read → eval-coords → click →
type → checkbox → submit → screenshot (>10KB) → close (pid gone, daemons
untouched). Exit code = failures, same verdict-banner style as Part 1.

When to run it:
- After deploying a daemon that touches `browser.rs` or the browser tool
  handlers in `mcp.rs` (via /deploy-daemon).
- Before announcing any release that mentions the browser.
- NOT in containers: the browser is headful by design — it needs a desktop
  with a Chromium-engine browser (or `IMMORTERM_BROWSER_BIN`) and pops a
  visible window for a few seconds.
- It SKIPS cleanly (exit 0, clear message) when the installed daemon predates
  the browser tools — safe to run unconditionally alongside Parts 1–2.

## Known gotchas (each cost a debugging round — do not rediscover)

- **amd64 image on arm64 host**: if `docker images` shows an old
  `immorterm-ai:headless-latest`, check its platform; rebuild native with
  `docker build -f .devops/docker/Dockerfile.immorterm-ai-headless -t immorterm-ai:headless-latest .`
  (~20 min). Emulated images miss the health window and act flaky.
- **First boot takes ~40s** but the launcher warns at 30s — the WARN is
  usually premature; check `docker logs immorterm-ai-test` before concluding
  failure, then just re-run `verify`.
- **HOST IDENTIFICATION CHANGED on verify**: every rebuilt container has a
  new SSH host key. Fix: `ssh-keygen -R "[localhost]:2222"`.
  (`strict_known_hosts:false` accepts NEW keys, never CHANGED ones.)
- **memory.state.json is pretty-printed** — `"port": 8765` with a space;
  grep accordingly.
- Part 2 needs the DESKTOP hub running on :1440 (it registers the remote).
  No hub → `desktop hub not reachable`.

## When something fails

Fix the PRODUCT, not the script, unless the script is provably wrong (it has
been three times: port parsing, gateway invocation, missing user_id — all
fixed). Past product bugs this ritual caught: GitHub API rate-limit 403 on
memory install, CLI never installing hooks, gateway tarball missing dist/
(gitignored-dist + no files field). Each fix shipped before any real stranger
hit it. That is the entire point.
