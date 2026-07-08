/**
 * MCP Gateway Manager
 *
 * Manages the lifecycle of the immorterm-mcp-gateway process.
 * Mirrors the pattern from openmemory-manager.ts:
 * - Start on first Claude session detection
 * - Health check via HTTP
 * - Auto-recovery on failure
 * - Config restore on shutdown
 *
 * The gateway eliminates per-session MCP process spawning by running
 * a single Node.js process that proxies stdio servers via HTTP.
 */

import * as http from 'http';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { execFile, fork, spawn, ChildProcess } from 'child_process';
import { auditedKill } from '../../utils/kill-audit';
import {
  GATEWAY_PORT,
  GATEWAY_STATE_DIR,
  GATEWAY_STATE_FILE,
  getHealthUrl,
} from './gateway-config';
import { isServiceEnabled } from '../../utils/immorterm-config';

/** Gateway state */
export interface GatewayState {
  /** Gateway process is running */
  running: boolean;
  /** Gateway API is healthy */
  healthy: boolean;
  /** PID of the gateway process */
  pid?: number;
  /** Port the gateway is listening on */
  port: number;
  /** Number of managed servers */
  serverCount?: number;
  /** Number of active child processes */
  activeChildren?: number;
  /** Memory usage in MB */
  memoryMB?: number;
  /** Last error */
  lastError?: string;
  /** Last recovery attempt timestamp */
  lastRecoveryAttempt?: number;
}

let state: GatewayState = {
  running: false,
  healthy: false,
  port: GATEWAY_PORT,
};

let logFn: (message: string) => void = console.log;
let gatewayProcess: ChildProcess | null = null;
let cachedWorkspacePath: string | undefined;

/** Recovery tracking — exponential backoff */
let consecutiveFailures = 0;
const BASE_RECOVERY_COOLDOWN_MS = 30_000; // 30s initial cooldown
const MAX_RECOVERY_COOLDOWN_MS = 120_000; // 2 min cap

/** Pre-warm: start gateway on first lifecycle call instead of waiting for Claude */
let hasAttemptedPrewarm = false;

/**
 * Initialize the gateway manager.
 */
export function initGatewayManager(logger: (msg: string) => void, workspacePath?: string): void {
  logFn = logger;
  cachedWorkspacePath = workspacePath;
}

/**
 * Check if the MCP gateway feature is enabled.
 * Reads from .immorterm/config.json (canonical source) via isServiceEnabled().
 * Falls back to VS Code workspace folders if cachedWorkspacePath not set.
 */
export function isGatewayEnabled(): boolean {
  if (cachedWorkspacePath) {
    return isServiceEnabled(cachedWorkspacePath, 'mcpGateway');
  }
  // Fallback: try VS Code workspace folders
  try {
    const vscode = require('vscode');
    const wsPath = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (wsPath) return isServiceEnabled(wsPath, 'mcpGateway');
  } catch {}
  return false;
}

/**
 * Check gateway health via HTTP.
 *
 * Returns true only if the gateway is both reachable AND functional.
 * A gateway with status "degraded" (alive but zero servers) is treated as
 * unhealthy — this triggers the auto-recovery path in checkGatewayLifecycle(),
 * which kills the broken instance and starts a fresh one.
 */
export async function checkGatewayHealth(): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.get(
      getHealthUrl(state.port),
      { timeout: 3000 },
      (res) => {
        let data = '';
        res.on('data', (chunk) => { data += chunk; });
        res.on('end', () => {
          if (res.statusCode === 200) {
            try {
              const health = JSON.parse(data);
              state.serverCount = health.servers?.length ?? 0;
              state.activeChildren = health.totalChildren ?? 0;
              state.memoryMB = health.memoryMB ?? 0;

              // A degraded gateway (zero servers) is NOT healthy.
              // This is the critical defense: without this check, the gateway
              // can be alive but useless — all MCP calls fail silently.
              if (health.status === 'degraded') {
                logFn(`[mcp-gateway] Health check: DEGRADED (alive but 0 servers — MCPs are dead)`);
                state.lastError = 'Gateway has zero servers registered';
                resolve(false);
                return;
              }
            } catch {}
            resolve(true);
          } else {
            resolve(false);
          }
        });
      },
    );
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}

/**
 * Start the gateway process.
 *
 * The gateway runs as a detached child process that survives
 * Extension Host crashes.
 */
export async function startGateway(): Promise<GatewayState> {
  logFn('[mcp-gateway] Starting gateway...');

  // Check if already running
  if (await checkGatewayHealth()) {
    logFn('[mcp-gateway] Already running and healthy');
    state.running = true;
    state.healthy = true;
    state.pid = readPidFromState();
    return { ...state };
  }

  // Ensure state directory exists
  fs.mkdirSync(GATEWAY_STATE_DIR, { recursive: true });

  // Find the gateway binary
  const gatewayPath = findGatewayBinary();
  if (!gatewayPath) {
    state.lastError = 'Gateway binary not found. Run: npm install -g immorterm-mcp-gateway';
    logFn(`[mcp-gateway] ${state.lastError}`);
    return { ...state };
  }

  try {
    // Log file for gateway stdout/stderr (survives parent exit)
    const logPath = path.join(GATEWAY_STATE_DIR, 'gateway.log');
    const logFd = fs.openSync(logPath, 'a');

    // Spawn as fully detached process with file-backed stdio.
    // Using 'ignore' for stdin, log file for stdout/stderr, and IPC for startup handshake.
    // After the startup handshake completes, we disconnect IPC and unref the child.
    // The file-backed stdio (not pipe) means no SIGPIPE when the parent exits.
    const child = fork(gatewayPath, ['start', '--foreground', '--port', String(state.port)], {
      detached: true,
      stdio: ['ignore', logFd, logFd, 'ipc'],
      env: { ...process.env },
    });

    gatewayProcess = child;

    // Wait for the "started" IPC message or timeout
    const started = await new Promise<boolean>((resolve) => {
      const timeout = setTimeout(() => resolve(false), 30_000);

      child.on('message', (msg: any) => {
        if (msg?.type === 'started') {
          clearTimeout(timeout);
          state.pid = msg.pid;
          state.port = msg.port;
          resolve(true);
        }
      });

      child.on('error', (err) => {
        clearTimeout(timeout);
        state.lastError = err.message;
        resolve(false);
      });

      child.on('exit', (code) => {
        clearTimeout(timeout);
        if (code !== 0) {
          state.lastError = `Gateway exited with code ${code}`;
          resolve(false);
        }
      });
    });

    // Close our copy of the log FD — child has its own
    fs.closeSync(logFd);

    if (started) {
      // Detach the child so it survives Extension Host restart.
      // With file-backed stdio (not pipes), there are no FDs tying
      // the child to the parent. This truly orphans the process.
      child.unref();
      child.disconnect();

      // Verify health
      await new Promise((r) => setTimeout(r, 1000));
      const healthy = await checkGatewayHealth();

      state.running = true;
      state.healthy = healthy;
      logFn(`[mcp-gateway] Started (PID ${state.pid}, port ${state.port})`);

      // Trigger MCP reconnect for all open projects
      // Claude Code watches .mcp.json — writing _reconnect_ts forces re-init
      if (healthy) {
        triggerMcpReconnect();
      }
    } else {
      logFn(`[mcp-gateway] Failed to start: ${state.lastError}`);
    }
  } catch (err) {
    state.lastError = err instanceof Error ? err.message : String(err);
    logFn(`[mcp-gateway] Start error: ${state.lastError}`);
  }

  return { ...state };
}

/**
 * Stop the gateway process gracefully.
 */
export async function stopGateway(): Promise<void> {
  logFn('[mcp-gateway] Stopping gateway...');

  const pid = state.pid ?? readPidFromState();
  if (pid) {
    try {
      auditedKill(pid, 'SIGTERM', 'stopGateway: shutdown');
      logFn(`[mcp-gateway] Sent SIGTERM to PID ${pid}`);

      // Wait for it to die
      await new Promise<void>((resolve) => {
        let checks = 0;
        const interval = setInterval(() => {
          try {
            process.kill(pid, 0); // Check if alive
            checks++;
            if (checks > 10) {
              clearInterval(interval);
              auditedKill(pid, 'SIGKILL', 'stopGateway: SIGKILL after 5s timeout');
              resolve();
            }
          } catch {
            clearInterval(interval);
            resolve();
          }
        }, 500);
      });
    } catch (err: any) {
      if (err.code !== 'ESRCH') {
        logFn(`[mcp-gateway] Stop error: ${err.message}`);
      }
    }
  }

  // Restore project .mcp.json files before updating state
  // This triggers Claude Code to reconnect using original stdio configs
  restoreProjectConfigs();

  state.running = false;
  state.healthy = false;
  state.pid = undefined;
  gatewayProcess = null;
}

/**
 * Check lifecycle based on Claude sessions.
 * Called periodically from claude-sync.
 *
 * Start gateway when sessions exist, keep running, auto-recover on failure.
 */
export async function checkGatewayLifecycle(
  sessions: Array<{ windowId: string; hasClaudeProcess: boolean }>,
): Promise<void> {
  if (!isGatewayEnabled()) return;

  // Pre-warm: start gateway on first lifecycle call instead of waiting for Claude sessions
  if (!hasAttemptedPrewarm) {
    hasAttemptedPrewarm = true;
    if (!state.healthy && !state.running) {
      logFn('[mcp-gateway] Pre-warming gateway on activation');
      await startGateway();
      return;
    }
  }

  const hasActiveSessions = sessions.some(s => s.hasClaudeProcess);

  if (hasActiveSessions) {
    // Always check health — gateway is critical infrastructure
    const healthy = await checkGatewayHealth();

    if (healthy) {
      state.running = true;
      state.healthy = true;
      if (consecutiveFailures > 0) {
        logFn(`[mcp-gateway] Recovered after ${consecutiveFailures} failure(s)`);
        consecutiveFailures = 0;
      }
      return;
    }

    // Gateway is down — attempt recovery
    consecutiveFailures++;
    state.healthy = false;

    // Exponential backoff: 30s → 60s → 120s (capped)
    const cooldown = Math.min(
      BASE_RECOVERY_COOLDOWN_MS * Math.pow(2, consecutiveFailures - 1),
      MAX_RECOVERY_COOLDOWN_MS,
    );

    const now = Date.now();
    if (state.lastRecoveryAttempt && now - state.lastRecoveryAttempt < cooldown) {
      return; // Within cooldown window, skip this cycle
    }

    state.lastRecoveryAttempt = now;
    logFn(`[mcp-gateway] Health check FAILED (attempt ${consecutiveFailures}, next cooldown ${Math.round(cooldown / 1000)}s). Restarting...`);

    // Stop whatever is broken, then restart
    try {
      await stopGateway();
    } catch {}
    await startGateway();
  }
}

/**
 * Rewrite a project's .mcp.json to use the gateway.
 * Called when gateway is enabled and project has Claude sessions.
 */
export async function rewriteProjectMcpConfig(projectPath: string): Promise<void> {
  const mcpJsonPath = path.join(projectPath, '.mcp.json');
  if (!fs.existsSync(mcpJsonPath)) return;

  // We call into the gateway's config-rewriter via HTTP
  // The gateway handles the actual rewriting
  // For now, this is a placeholder — the gateway rewrites global config on start,
  // and the extension handles project config via this function

  // TODO: Implement project config rewriting
  // For Phase 3, this will call a gateway API endpoint or use the
  // config-rewriter module directly
  logFn(`[mcp-gateway] Project config rewriting not yet implemented for: ${mcpJsonPath}`);
}

/**
 * Get current gateway state for status bar.
 */
export function getMCPGatewayState(): GatewayState {
  return { ...state };
}

/**
 * Get status bar text.
 */
export function getGatewayStatusText(): string {
  if (!isGatewayEnabled()) return '';

  if (state.healthy) {
    const servers = state.serverCount ?? 0;
    const children = state.activeChildren ?? 0;
    return `MCP GW: ${servers} servers, ${children} active`;
  }

  if (state.running) return 'MCP GW: starting...';
  return 'MCP GW: off';
}

/**
 * Tell the gateway to kill all stateful MCP children owned by a client PID.
 * Fire-and-forget — does not block the caller or throw on failure.
 * Called from terminal cleanup (grace period) and stale session reaper.
 */
export function cleanupGatewaySessionByPid(pid: number): void {
  if (!isGatewayEnabled() || !state.healthy) return;

  const port = state.port || GATEWAY_PORT;
  const req = http.request(
    {
      hostname: 'localhost',
      port,
      path: `/sessions/by-pid/${pid}`,
      method: 'DELETE',
      timeout: 3000,
    },
    (res) => {
      let data = '';
      res.on('data', (chunk: string) => { data += chunk; });
      res.on('end', () => {
        try {
          const result = JSON.parse(data);
          if (result.killed > 0) {
            logFn(`[mcp-gateway] Cleaned up PID ${pid}: killed ${result.killed} children, ${result.sessions} sessions`);
          }
        } catch { /* ignore parse errors */ }
      });
    },
  );
  req.on('error', () => { /* fire-and-forget */ });
  req.on('timeout', () => { req.destroy(); });
  req.end();
}

/**
 * Kill old per-session MCP child processes to force reconnection through gateway.
 *
 * Called during per-project activation (enableForProject) for immediate effect.
 * The gateway spawns direct node binaries (not npm exec), so all `npm exec`
 * MCP wrappers are old per-session processes that can be safely killed.
 *
 * For uvx (serena), we exclude the gateway's own child by checking parent PID.
 *
 * Uses execFile (not exec) for all subprocesses — no shell injection risk.
 */
export async function killOldMcpProcesses(): Promise<{ killed: number }> {
  const gatewayPid = state.pid ?? readPidFromState();
  let killed = 0;

  // Known npm exec MCP patterns — gateway never uses npm exec, so all are old
  const npmExecPatterns = [
    'npm exec.*@modelcontextprotocol/server-sequential-thinking',
    'npm exec.*@upstash/context7-mcp',
    'npm exec.*tavily-mcp',
    'npm exec.*@morphllm/morphmcp',
    'npm exec.*21st-magic',
    'npm exec.*chrome-devtools-mcp',
    'npm exec.*iconfont-mcp',
    'npm exec.*@playwright/mcp',
    'npm exec.*@anthropic-ai/mcp-proxy',
  ];

  // Kill npm exec wrappers using pkill -f (patterns are hardcoded, no injection risk)
  for (const pattern of npmExecPatterns) {
    try {
      await new Promise<void>((resolve) => {
        execFile('pkill', ['-f', pattern], () => resolve());
      });
      killed++;
    } catch {}
  }

  // Kill orphaned node MCP workers from npx cache
  const npxWorkerPatterns = [
    'node.*\\.npm/_npx.*/context7',
    'node.*\\.npm/_npx.*/tavily',
    'node.*\\.npm/_npx.*/sequential-thinking',
    'node.*\\.npm/_npx.*/mcp-proxy',
    'node.*\\.npm/_npx.*/morphmcp',
    'node.*\\.npm/_npx.*/magic',
    'node.*\\.npm/_npx.*/chrome-devtools',
    'node.*\\.npm/_npx.*/iconfont-mcp',
    'node.*\\.npm/_npx.*/mcp.*--headless',
  ];
  for (const pattern of npxWorkerPatterns) {
    try {
      await new Promise<void>((resolve) => {
        execFile('pkill', ['-f', pattern], () => resolve());
      });
    } catch {}
  }

  // Kill old uvx serena processes, but not the gateway's own child.
  // Use pgrep to find PIDs, check parent in JS, then signal individually.
  if (gatewayPid) {
    try {
      const pids = await new Promise<string>((resolve) => {
        execFile('pgrep', ['-f', 'uvx.*serena'], (err, stdout) => resolve(stdout || ''));
      });
      for (const pidStr of pids.trim().split('\n').filter(Boolean)) {
        const pid = parseInt(pidStr, 10);
        if (isNaN(pid)) continue;
        // Check parent PID — keep the gateway's own child
        const ppid = await new Promise<string>((resolve) => {
          execFile('ps', ['-o', 'ppid=', '-p', String(pid)], (err, stdout) => resolve((stdout || '').trim()));
        });
        if (ppid !== String(gatewayPid)) {
          auditedKill(pid, 'SIGTERM', `gateway: orphaned mcp-gateway process (ppid=${ppid})`);
        }
      }
    } catch {}
  }

  // Give processes time to die
  await new Promise((r) => setTimeout(r, 1000));

  logFn(`[mcp-gateway] Killed old per-session MCP processes`);
  return { killed };
}

// ── Helpers ────────────────────────────────────────────────────────────

/**
 * Find the gateway binary.
 *
 * Search order:
 * 1. Extension bundled (vsix)
 * 2. ~/.immorterm/mcp-gateway/ (stable install target)
 * 3. npm global install
 * 4. Workspace root (monorepo development)
 * 5. Legacy __dirname-based (dev only)
 */
function findGatewayBinary(): string | null {
  // 1. Extension bundled (when published as vsix)
  const localPath = path.join(__dirname, '..', '..', '..', 'node_modules', 'immorterm-mcp-gateway', 'dist', 'index.js');
  if (fs.existsSync(localPath)) return localPath;

  // 2. ImmorTerm home directory (~/.immorterm/mcp-gateway/)
  const homePath = path.join(os.homedir(), '.immorterm', 'mcp-gateway', 'dist', 'index.js');
  if (fs.existsSync(homePath)) return homePath;

  // 3. npm global install
  try {
    const { execFileSync } = require('child_process');
    const globalDir = execFileSync('npm', ['root', '-g'], { encoding: 'utf-8', timeout: 5000 }).trim();
    const npmGlobalPath = path.join(globalDir, 'immorterm-mcp-gateway', 'dist', 'index.js');
    if (fs.existsSync(npmGlobalPath)) return npmGlobalPath;
  } catch {}

  // 4. Development: check workspace root (for monorepo development)
  if (cachedWorkspacePath) {
    const wsPath = path.join(cachedWorkspacePath, 'services', 'mcp-gateway', 'dist', 'index.js');
    if (fs.existsSync(wsPath)) return wsPath;
  }

  // 5. Legacy: relative from source tree (__dirname-based, dev only)
  const legacyPath = path.join(__dirname, '..', '..', '..', '..', '..', 'services', 'mcp-gateway', 'dist', 'index.js');
  if (fs.existsSync(legacyPath)) return legacyPath;

  return null;
}

/**
 * Read PID from state.json file.
 */
function readPidFromState(): number | undefined {
  try {
    if (fs.existsSync(GATEWAY_STATE_FILE)) {
      const data = JSON.parse(fs.readFileSync(GATEWAY_STATE_FILE, 'utf-8'));
      return data.pid;
    }
  } catch {}
  return undefined;
}

/**
 * Trigger MCP reconnect for all open workspace folders.
 *
 * Same pattern as OpenMemory's refreshMcpConfig — write _reconnect_ts to
 * each project's .mcp.json, forcing Claude Code to re-read the file and
 * establish fresh connections to the gateway's HTTP endpoints.
 *
 * Only touches projects where the user enabled the MCP gateway
 * (immorterm.services.mcpGateway.enabled === true in workspace settings).
 */
function triggerMcpReconnect(): void {
  try {
    const vscode = require('vscode');
    const folders = vscode.workspace.workspaceFolders;
    if (!folders) return;

    for (const folder of folders) {
      // Only touch projects that opted in
      const folderConfig = vscode.workspace.getConfiguration('immorterm', folder.uri);
      if (!folderConfig.get('services.mcpGateway.enabled', false)) continue;

      const mcpJsonPath = path.join(folder.uri.fsPath, '.mcp.json');
      if (!fs.existsSync(mcpJsonPath)) continue;

      // Delegate to the gateway: it backs up originals, rewrites .mcp.json,
      // and registers the stdio servers in its child pool — all in one call.
      fetch(`http://localhost:${state.port}/projects/register`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ configPath: mcpJsonPath }),
      })
        .then((res) => res.json())
        .then((result: any) => {
          if (result.registered?.length > 0) {
            logFn(`[mcp-gateway] Registered project servers for ${folder.name}: ${result.registered.join(', ')}`);
          } else {
            logFn(`[mcp-gateway] No new servers to register for ${folder.name}`);
          }
        })
        .catch((err: any) => {
          logFn(`[mcp-gateway] Failed to register project servers for ${folder.name}: ${err}`);
        });
    }
  } catch {
    // vscode not available (running outside extension context)
  }
}

/**
 * Restore project .mcp.json files when gateway stops.
 * Reverse of triggerMcpReconnect — put back the original stdio entries.
 *
 * Only touches projects where the user enabled the MCP gateway.
 */
function restoreProjectConfigs(): void {
  // The gateway restores project .mcp.json files on SIGTERM via restoreAllProjectConfigs().
  // Since stopGateway() sends SIGTERM and waits for the process to exit, project configs
  // are already restored by the time we reach here. This is kept as a no-op safety net.
  logFn('[mcp-gateway] Project config restoration delegated to gateway SIGTERM handler');
}
