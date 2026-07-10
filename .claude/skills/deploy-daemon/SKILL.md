---
name: deploy-daemon
description: "Build, lint, test, sign, and deploy the immorterm-ai native daemon binary. Use after ANY change to apps/immorterm-ai/immorterm-daemon/. The daemon binary is separate from WASM — bun run build:immorterm-ai does NOT touch it."
allowed-tools: Bash, Read
---

# Deploy ImmorTerm AI Daemon

Lint, test, build, code-sign, and deploy the native daemon binary to `~/.immorterm/bin/`.

## WHEN TO USE THIS

Use `/deploy-daemon` after changing ANY of these:

| What changed | Why |
|-------------|-----|
| `apps/immorterm-ai/immorterm-daemon/src/*.rs` | Daemon logic, IPC handlers, MCP tools |
| `apps/immorterm-ai/immorterm-daemon/Cargo.toml` | Dependencies |
| `apps/immorterm-ai/immorterm-core/src/*.rs` | Core engine used by daemon |

**IMPORTANT:** `bun run build:immorterm-ai` and `/deploy-immorterm-ai` do NOT build or deploy the daemon binary. They only handle WASM + extension resources.

## Steps

Run these steps IN ORDER. Stop on any failure.

```bash
# 1. Clippy lint (catches warnings before building)
cargo clippy -p immorterm-daemon -p immorterm-core -- -D warnings

# 2. Run tests (core + render — catches regressions)
cargo test -p immorterm-core -p immorterm-render

# 3. Build release binary
cargo build --release -p immorterm-daemon

# 4. Deploy with code signing (MUST codesign — cp invalidates adhoc signatures on macOS)
cp target/release/immorterm-ai ~/.immorterm/bin/immorterm-ai
codesign --force --sign - ~/.immorterm/bin/immorterm-ai

# 5. Verify
~/.immorterm/bin/immorterm-ai --version
```

## Why Code Signing Is Required

macOS Gatekeeper blocks unsigned binaries when spawned as subprocesses by VS Code. `cargo build` applies an adhoc linker signature, but `cp` invalidates it. Without `codesign --force --sign -`, new ImmorTerm sessions silently fail to create (Ctrl+Shift+A does nothing).

## After Deploying

Existing terminal sessions still run the OLD daemon binary (loaded in memory). Only **new** sessions (Ctrl+Shift+A) will use the updated binary.

To force all sessions to use the new binary: close and reopen each terminal tab.

## Quick Mode (skip lint + tests)

For fast iteration when you're confident in the changes:

```bash
cargo build --release -p immorterm-daemon
cp target/release/immorterm-ai ~/.immorterm/bin/immorterm-ai
codesign --force --sign - ~/.immorterm/bin/immorterm-ai
```

Only use quick mode during active development. Always run the full pipeline before committing.
