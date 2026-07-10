---
name: verify-immorterm-ai
description: "Autonomous end-to-end verification of the immorterm-ai stack after changes. Combines automated tests (Playwright + Rust) with live MCP verification. Use after deploying changes (via /deploy-immorterm-ai) or proactively before committing."
allowed-tools: Bash, Read
---

# Verify ImmorTerm AI

Autonomous end-to-end verification of the immorterm-ai stack after changes.
Combines automated tests (Playwright + Rust) with live MCP verification.

## WHEN TO USE THIS

Use `/verify-immorterm-ai` after deploying changes (via `/deploy-immorterm-ai`) to verify everything works without user involvement. Also use proactively before committing.

## Steps

### Step 1: Run Automated Tests

Run the Playwright e2e tests against both the standalone WASM demo and the production GPU terminal HTML:

```bash
npx playwright test --reporter=list
```

If tests fail, stop and fix the issue before proceeding.

### Step 2: Live MCP Verification

Use ImmorTerm MCP tools to verify the running system. Call these tools in parallel:

1. **`mcp__immorterm__immorterm_list_sessions`** — Verify active sessions exist
   - Check: at least 1 alive session
   - If no sessions, the deployment may have broken daemon connectivity

2. **`mcp__immorterm__immorterm_screenshot`** — Take a screenshot of an active session
   - Check: screenshot is returned (not an error)
   - Check: visually verify the screenshot shows a terminal (not blank/grey)
   - The screenshot renders on the daemon side using the SAME Rust renderer as the WASM

3. **`mcp__immorterm__immorterm_get_viewport`** with `include_text: true` — Get viewport state
   - Check: cols > 0, rows > 0 (terminal has dimensions)
   - Check: text content is present (not empty grid)

4. **`mcp__immorterm__immorterm_read_screen`** — Read the terminal text
   - Check: text content matches expected terminal output

### Step 3: Health File Check

Read the paint-canary health file to verify the VS Code webview is alive:

```bash
cat ~/.immorterm/ai-health.json
```

Check:
- `healthy: true` — WASM initialized, render loop running, frames > 0
- `recoveryCount: 0` — no auto-recovery triggered (good)
- `sessionCount > 0` — sessions are connected
- `wsConnected > 0` — WebSocket connections alive
- `frameCount > 0` — GPU rendering is producing frames

If `recoveryCount > 0`, the grey screen auto-recovery was triggered — investigate why.

### Step 4: Report

Summarize the results:
- Automated tests: passed/failed
- Live sessions: count + health
- Screenshot: rendered correctly (yes/no)
- Health canary: healthy/degraded/failing
- Recovery count: 0 (ideal) or N (investigate)

## Interpretation Guide

| Symptom | Meaning | Action |
|---------|---------|--------|
| Playwright fails | WASM/WebGPU regression | Fix Rust/WASM code |
| No live sessions | Daemon not running | Rebuild daemon binary |
| Screenshot blank | Render pipeline broken | Check immorterm-render |
| Health file missing | Extension not sending canary | Check gpu-terminal.ts |
| `healthy: false` | Webview not rendering | Check gpu-terminal.html |
| `recoveryCount > 0` | Grey screen was auto-fixed | Investigate root cause |
| `wsConnected: 0` | Daemon connections failed | Check daemon WebSocket |
