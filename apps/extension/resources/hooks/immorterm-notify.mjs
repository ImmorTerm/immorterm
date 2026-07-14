#!/usr/bin/env node
// Owner: immorterm-ai (terminal notifier — sidebar breathing-dot IPC)
// Cross-OS hook wrapper for immorterm-ai notify commands.
// Session identifier priority:
//   1. IMMORTERM_SESSION — set in native wgpu immorterm windows (Tauri runtime)
//   2. STY — set in screen-compat wrapped sessions
// No-ops cleanly when:
//   - no session env var is set (Claude Code launched outside immorterm)
//   - immorterm-ai binary is missing (e.g. Windows, or not installed)
//   - the binary call errors (output suppressed; hook always exits 0)
//
// Usage: node immorterm-notify.mjs <state>   (working|idle|attention)
//
// Why Node: bash isn't guaranteed on native Windows, but Node ships with
// Claude Code itself, so this runs uniformly on macOS, Linux, WSL, and Win.

import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { homedir, platform } from "node:os";
import { join } from "node:path";

const state = process.argv[2] || "working";
const session = process.env.IMMORTERM_SESSION || process.env.STY;
const binName = platform() === "win32" ? "immorterm-ai.exe" : "immorterm-ai";
const bin = join(homedir(), ".immorterm", "bin", binName);

if (!session || !existsSync(bin)) process.exit(0);

// A status ping must NEVER sit on the prompt's critical path. Detach the child
// and return immediately — if the daemon/IPC is briefly unreachable the child
// may stall, but waiting on it would block this UserPromptSubmit hook into a
// timeout-kill (which discards the WHOLE hook's stdout). Fire-and-forget.
const child = spawn(bin, ["-S", session, "-X", "notify", state], {
  stdio: "ignore",
  windowsHide: true,
  detached: true,
});
child.on("error", () => {});
child.unref();
process.exit(0);
