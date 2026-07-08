<div align="center">

# immorterm

**the immortal terminal.**

Your editor died. Your session didn't notice.

[![npm](https://img.shields.io/npm/v/immorterm)](https://www.npmjs.com/package/immorterm)
![platforms](https://img.shields.io/badge/platforms-macOS%20arm64%20%C2%B7%20Linux%20glibc%202.38%2B-333)
![local-first](https://img.shields.io/badge/memory-local--first%20%C2%B7%20no%20API%20key-2ea44f)
![claude code](https://img.shields.io/badge/claude%20code-native-d97757)
[![license](https://img.shields.io/badge/license-FSL--1.1--ALv2-lightgrey)](LICENSE.md)

</div>

ImmorTerm keeps terminal sessions in a daemon below your editor — crashes happen above
it. Force-quit VS Code, drop SSH, kill the window: you reattach to the same live
process, full scrollback, mid-thought. Underneath, a local memory digests every session
as it happens and hands Claude Code last week's decisions before you ask — recall runs
on-device in ~8ms, no LLM call, no API key.

- **Sessions that reattach, not restore** — daemon-held PTYs outlive any UI
- **Memory before you ask** — ambient digestion, on-device ONNX recall, zero tokens
  added when a prompt doesn't need it
- **A terminal the AI can see and draw on** — screenshots, inline highlights, charts,
  interactive panels (WebGPU-rendered)
- **Cmd+hover** — file paths, images, and folders open as live previews in place
- **Archaeology** — every edit linked to the conversation that produced it;
  `explain_change` answers *why*, not just *who*
- **Undo the AI** — pre-edit checkpoints on every file the agent touches

## Install

```bash
npm install -g immorterm
immorterm init --yes
immorterm memory install
```

macOS (arm64) and Linux (glibc 2.38+). Claude Code native — other CLIs run as plain
shells. Desktop app and VS Code extension are coming; today it's npm.

Free is real: everything is captured and kept forever; recall on Free reads 5 results
from the last 72 hours. Nothing leaves your machine.

## Component map

| Component | Language | Location | What it does |
|-----------|----------|----------|--------------|
| Terminal engine | Rust + WASM | `apps/immorterm-ai/` | GPU rendering core, session daemon, WebGPU, AI overlays |
| VS Code extension | TypeScript | `apps/extension/` | Terminal lifecycle, status bar, AI session tracking |
| Desktop app | Rust (Tauri) | `apps/immorterm-app/` | Standalone GPU terminal with a native shell |
| CLI | TypeScript | `apps/immorterm/` | Setup, service management, health checks (`npx immorterm`) |
| Docs site | TypeScript | `apps/docs/` | Documentation behind docs.immorterm.com |
| Shared libraries | TypeScript + Rust | `libs/*` | Types, config, UI, design tokens, license, hook installer — also published as `@immorterm/*` on npm |
| opencode plugin | TypeScript | `packages/immorterm-opencode-plugin/` | Bridges opencode's plugin events into the ImmorTerm hook pipeline |

The companion memory engine (`immorterm-memory`) and MCP gateway ship as their own
binaries, downloaded by `immorterm memory install` from this repository's GitHub
Releases. Their source lives in separate repositories.

Learn more at [immorterm.com](https://immorterm.com) and
[docs.immorterm.com](https://docs.immorterm.com).

## Building from source

ImmorTerm uses [Bun](https://bun.sh) for the TypeScript workspaces and Cargo for the
Rust crates.

```bash
bun install                       # install all workspace dependencies

bun run build:cli                 # build the CLI
bun run build:extension           # compile the VS Code extension
bun run build:docs                # build the documentation site
bun run build:immorterm-ai        # build the Rust engine + WASM and deploy locally
```

Rust checks:

```bash
cargo check                       # build the native crates
cargo check -p immorterm-wasm --target wasm32-unknown-unknown  # the WASM crate
bun run test:immorterm-ai         # run engine tests
```

## License

ImmorTerm is released under the [Functional Source License 1.1](LICENSE.md)
(FSL-1.1-ALv2). In short: you can read, run, modify, and build on the code freely for
any purpose except creating a competing product. Two years after each release, that
version converts automatically to the Apache License 2.0.

Copyright © 2026 Shai Snir.
