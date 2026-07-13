# Installation Guide

## Quick Install

### 1. Run the setup wizard

```bash
npx immorterm init
```

This configures ImmorTerm (theme, AI Memory, MCP Gateway) and installs the VS Code extension automatically.

### 2. Enable for Your Project

Open any project in VS Code. ImmorTerm will ask: **"Enable ImmorTerm for this project?"**

Click **Enable** — that's it. The ImmorTerm terminal (GPU-rendered, Rust engine) persists your sessions across VS Code restarts and crashes. There is no separate terminal binary to install — the engine ships with the extension and desktop app.

## Optional: Memory Services

Enable persistent AI memory for Claude Code sessions:

1. When prompted (by `immorterm init` or the extension), choose to enable AI Memory
2. The native memory binary (~15 MB) is downloaded to `~/.immorterm/bin/` — no Docker required
3. MCP configuration and hooks are set up automatically

## What Gets Installed

- VS Code extension (`immorterm.immorterm-terminal`)
- `~/.immorterm/` — global config, scripts, and (if enabled) the memory binary
- Per-project `.immorterm/` directory — session registry, hooks, logs

## Uninstallation

See [README.md](../README.md#uninstallation) for full uninstall instructions.
