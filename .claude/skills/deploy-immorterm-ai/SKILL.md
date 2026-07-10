---
name: deploy-immorterm-ai
description: "Build, lint, test, and deploy the full immorterm-ai stack. MUST be used after ANY change to apps/immorterm-ai/ (Rust crates), terminal HTML files, or WASM-related code. Covers all layers: core, render, daemon, WASM, JS glue, shaders, and terminal HTML."
allowed-tools: Bash, Read
---

# Deploy ImmorTerm AI

Build, lint, test, and deploy the entire immorterm-ai stack atomically.

## WHEN TO USE THIS

Use `/deploy-immorterm-ai` after changing ANY of these:

| What changed | Why this skill catches it |
|-------------|--------------------------|
| `apps/immorterm-ai/immorterm-core/` | Clippy + tests validate the core engine |
| `apps/immorterm-ai/immorterm-render/` | Clippy + WGSL shader validation + tests |
| `apps/immorterm-ai/immorterm-daemon/` | Clippy validates the native daemon |
| `apps/immorterm-ai/immorterm-wasm/` | Clippy (wasm32 target) + wasm-pack build + artifact sync |
| `apps/extension/resources/*-terminal.html` | Oxlint catches undefined JS refs + syntax errors |
| Any `.wgsl` shader file | Naga compiler validation via render tests |

**Do NOT deploy immorterm-ai changes manually.** Always use this skill or `bun run build:immorterm-ai`.

## Steps

```bash
# Full build: clippy + tests + WASM + oxlint + deploy (recommended)
bun run build:immorterm-ai

# Quick build: WASM + oxlint + deploy only (skip clippy/tests — use when iterating fast)
bun run build:immorterm-ai:quick
```

## What It Checks (full mode — 8 steps)

| Step | What | Catches |
|------|------|---------|
| 1 | Clippy: native crates | Lint warnings in core/render/daemon |
| 2 | Clippy: WASM crate (wasm32 target) | Lint warnings specific to browser APIs |
| 3 | Rust tests: core + render | Regressions, broken terminal emulation |
| 4 | WGSL shader validation | Broken shaders (naga compiler) |
| 5 | wasm-pack build (release) | Rust → WASM compilation + wasm-opt |
| 6 | Copy artifacts + verify | ALL 4 wasm-bindgen outputs synced (prevents JS glue mismatch) |
| 7 | Oxlint: terminal HTML | Undefined JS references, syntax errors |
| 8 | Deploy to VS Code extension | WASM + HTML/CSS resources synced to installed extension |

## Verification

After deploying, reload VS Code (Cmd+Shift+P > "Reload Window").

If the build fails at any step, it exits immediately with the error — nothing gets deployed half-baked.
