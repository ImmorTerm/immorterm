/**
 * Memory Manager
 *
 * Manages the lifecycle of the native Rust immorterm-memory service.
 * Philosophy: Plug-and-play — find binary, start daemon, health check.
 *
 * The native binary replaces the entire Docker stack (Qdrant, Redis, Neo4j,
 * OpenMemory, Celery Worker) with a single ~15MB process.
 */

import * as http from 'http';
import { isMemoryEnabled } from './services-picker';
import { findBinary, startNativeMemory, stopNativeMemory, checkNativeHealth, initNativeMemoryManager, getNativeStatus, getMemoryPort } from './native-memory-manager';

/** Resolved memory port (reads from state.json with 8765 fallback) */
const MEMORY_PORT = getMemoryPort();

/**
 * Memory service state
 */
export interface OpenMemoryState {
  /** Service is running */
  stackRunning: boolean;
  /** REST API is healthy */
  apiHealthy: boolean;
  /** MCP endpoint is healthy (can serve tool calls) */
  mcpHealthy: boolean;
  /** Active Claude sessions using memory */
  activeClaudeSessions: Set<string>;
  /** When service was started */
  startedAt?: number;
  /** Last error message */
  lastError?: string;
  /** Timestamp of last auto-recovery attempt (for cooldown) */
  lastRecoveryAttempt?: number;
  /** Timestamp of last MCP reconnect trigger (to avoid hammering) */
  lastMcpReconnect?: number;
}

/** Global state */
let state: OpenMemoryState = {
  stackRunning: false,
  apiHealthy: false,
  mcpHealthy: false,
  activeClaudeSessions: new Set(),
};

/** Logger function */
let logFn: (message: string) => void = console.log;

/** Startup reconnect: fire once when MCP is first confirmed healthy after activation */
let startupReconnectDone = false;


/**
 * Initialize the memory manager.
 *
 * @param logger Logging function
 */
export function initOpenMemoryManager(logger: (message: string) => void, _extPath?: string): void {
  logFn = logger;
  initNativeMemoryManager(logger);
}

/**
 * Check memory service API health.
 *
 * @returns true if API is healthy
 */
export async function checkOpenMemoryHealth(): Promise<boolean> {
  return checkNativeHealth(MEMORY_PORT);
}

/**
 * Check MCP endpoint health by sending a JSON-RPC initialize request.
 *
 * Unlike the REST /health check, this verifies the actual MCP endpoint that
 * Claude Code talks to. If the REST API is healthy but MCP is broken (e.g.,
 * stale session state, import errors, endpoint misconfiguration), this catches it.
 *
 * Uses a lightweight initialize handshake — no session state is created
 * when running in stateless mode.
 *
 * The MCP Streamable HTTP spec requires Accept: application/json, text/event-stream.
 * The server responds with SSE format (text/event-stream) containing JSON-RPC
 * payloads in `data:` lines. We parse the SSE envelope to extract and validate
 * the JSON-RPC result.
 *
 * @returns true if MCP endpoint responds with a valid JSON-RPC result
 */
export async function checkMcpEndpointHealth(): Promise<boolean> {
  return new Promise((resolve) => {
    const payload = JSON.stringify({
      jsonrpc: '2.0',
      method: 'initialize',
      id: 1,
      params: {
        protocolVersion: '2025-03-26',
        capabilities: {},
        clientInfo: { name: 'health-probe', version: '1.0' },
      },
    });

    // Use a generic MCP path — the health probe doesn't need a real project ID
    const req = http.request(
      {
        hostname: '127.0.0.1',
        port: MEMORY_PORT,
        path: '/mcp/claude-code/health-probe',
        method: 'POST',
        timeout: 5000,
        headers: {
          'Content-Type': 'application/json',
          // MCP Streamable HTTP spec requires both Accept types
          'Accept': 'application/json, text/event-stream',
          'Content-Length': Buffer.byteLength(payload),
        },
      },
      (res) => {
        let data = '';
        res.on('data', (chunk) => { data += chunk; });
        res.on('end', () => {
          if (!res.statusCode || res.statusCode < 200 || res.statusCode >= 300) {
            resolve(false);
            return;
          }

          try {
            // Response may be SSE (text/event-stream) or plain JSON
            const contentType = res.headers['content-type'] ?? '';
            let jsonStr: string;

            if (contentType.includes('text/event-stream')) {
              // Parse SSE: extract the first `data:` line's JSON payload
              const dataLine = data.split('\n').find(l => l.startsWith('data: '));
              jsonStr = dataLine ? dataLine.slice(6) : '';
            } else {
              jsonStr = data;
            }

            const result = JSON.parse(jsonStr);
            resolve(!!result?.result?.serverInfo);
          } catch {
            resolve(false);
          }
        });
      },
    );

    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
    req.write(payload);
    req.end();
  });
}

/**
 * Wait for OpenMemory API to become healthy.
 *
 * @param timeoutMs Maximum wait time
 * @param intervalMs Check interval
 * @returns true if healthy within timeout
 */
export async function waitForOpenMemory(
  timeoutMs: number = 120000,
  intervalMs: number = 2000
): Promise<boolean> {
  const startTime = Date.now();

  while (Date.now() - startTime < timeoutMs) {
    if (await checkOpenMemoryHealth()) {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  return false;
}

/**
 * Start memory service.
 * Finds the native binary and starts it as a daemon.
 *
 * @returns Current state
 */
export async function startOpenMemory(): Promise<OpenMemoryState> {
  logFn('[memory] Starting memory service...');

  // Check if already running and healthy
  if (await checkOpenMemoryHealth()) {
    logFn('[memory] Already running and healthy');
    state.stackRunning = true;
    state.apiHealthy = true;
    state.mcpHealthy = await checkMcpEndpointHealth();
    return { ...state };
  }

  // Find native binary
  const binaryPath = findBinary();
  if (!binaryPath) {
    const msg = 'immorterm-memory binary not found. Install with: cargo build --release -p immorterm-memory && cp target/release/immorterm-memory ~/.immorterm/bin/';
    logFn(`[memory] ${msg}`);
    state.lastError = msg;
    return { ...state };
  }

  // Start the daemon
  logFn(`[memory] Starting ${binaryPath}...`);
  const started = await startNativeMemory(binaryPath, MEMORY_PORT);
  if (!started) {
    state.lastError = 'Failed to start native memory service';
    logFn(`[memory] ${state.lastError}`);
    return { ...state };
  }

  state.stackRunning = true;
  state.apiHealthy = true;
  state.mcpHealthy = await checkMcpEndpointHealth();
  state.startedAt = Date.now();
  state.lastError = undefined;

  logFn(`[memory] Service started (MCP: ${state.mcpHealthy ? 'OK' : 'not yet ready'})`);
  return { ...state };
}

/**
 * Stop memory service.
 */
export async function stopOpenMemory(_force: boolean = false): Promise<void> {
  logFn('[memory] Stopping service...');

  const stopped = await stopNativeMemory();

  if (stopped) {
    state.stackRunning = false;
    state.apiHealthy = false;
    state.mcpHealthy = false;
    state.startedAt = undefined;
    logFn('[memory] Service stopped');
  } else {
    logFn('[memory] Failed to stop service (may not be running)');
  }
}

/**
 * Get current state.
 *
 * @returns Copy of current state
 */
export function getOpenMemoryState(): OpenMemoryState {
  return { ...state, activeClaudeSessions: new Set(state.activeClaudeSessions) };
}

/**
 * Refresh state by checking actual service status.
 *
 * @returns Updated state
 */
export async function refreshOpenMemoryState(): Promise<OpenMemoryState> {
  const nativeStatus = await getNativeStatus();
  state.stackRunning = nativeStatus.running;
  state.apiHealthy = nativeStatus.healthy;
  state.mcpHealthy = state.apiHealthy ? await checkMcpEndpointHealth() : false;

  return { ...state, activeClaudeSessions: new Set(state.activeClaudeSessions) };
}

/** MCP reconnect cooldown — at most once per 60s */
const MCP_RECONNECT_COOLDOWN_MS = 60_000;

/**
 * Keepalive reconnect interval (5 min).
 * Periodically writes _reconnect_ts to .mcp.json even when the server is healthy.
 * This catches the case where Claude Code's SSE client silently disconnected
 * (initial handshake timeout, dropped stream) while the server stayed healthy —
 * no unhealthy→healthy transition would fire, leaving Claude Code permanently
 * disconnected with no MCP tools.
 *
 * Safe because Claude Code's reconnect is idempotent — if already connected,
 * the .mcp.json change triggers a fast re-read with no visible side effects.
 */
const MCP_KEEPALIVE_INTERVAL_MS = 5 * 60_000;

/**
 * Trigger MCP reconnect for all workspace folders.
 * Writes _reconnect_ts to each project's .mcp.json, forcing Claude Code
 * to re-read the config and establish fresh MCP connections.
 *
 * Respects a 60s cooldown to avoid hammering .mcp.json writes.
 *
 * @param reason Reason for the reconnect (logged for debugging)
 */
async function triggerMcpReconnectForAllFolders(reason: string): Promise<void> {
  // Cooldown check
  const now = Date.now();
  if (state.lastMcpReconnect && now - state.lastMcpReconnect < MCP_RECONNECT_COOLDOWN_MS) {
    return;
  }
  state.lastMcpReconnect = now;

  try {
    const { refreshMcpConfig } = await import('./mcp-configurator');
    const vscode = await import('vscode');
    const folders = vscode.workspace.workspaceFolders;
    if (folders) {
      for (const folder of folders) {
        refreshMcpConfig(folder.uri.fsPath);
      }
      logFn(`[memory] Triggered MCP reconnect for all workspace folders (${reason})`);
    }
  } catch (err) {
    logFn(`[memory] MCP reconnect trigger failed (non-fatal): ${err}`);
  }
}

/**
 * Check lifecycle based on Claude sessions.
 * Called periodically from claude-sync.
 *
 * - First Claude session → start services
 * - Last Claude session exits → Docker stays running (shared across projects)
 *
 * @param sessions Array of session info
 */
export async function checkOpenMemoryLifecycle(
  sessions: Array<{ windowId: string; hasClaudeProcess: boolean }>
): Promise<void> {
  // Skip if memory not enabled
  if (!isMemoryEnabled()) {
    return;
  }

  // Track previous count
  const previousCount = state.activeClaudeSessions.size;

  // Update active sessions
  state.activeClaudeSessions.clear();
  for (const session of sessions) {
    if (session.hasClaudeProcess) {
      state.activeClaudeSessions.add(session.windowId);
    }
  }

  const currentCount = state.activeClaudeSessions.size;

  // First Claude session detected → start services
  if (previousCount === 0 && currentCount > 0) {
    logFn('[memory] Claude session detected, ensuring services running...');

    // Start if not already running
    if (!state.apiHealthy) {
      await startOpenMemory();
    }
  }

  // Periodic health check + auto-recovery (runs every sync cycle ~30s)
  if (currentCount > 0) {
    const wasUnhealthy = !state.apiHealthy;

    if (state.apiHealthy) {
      // Quick health probe — just HTTP GET, no Docker commands
      const stillHealthy = await checkOpenMemoryHealth();
      if (!stillHealthy) {
        logFn('[memory] Health check failed, attempting recovery...');
        state.apiHealthy = false;
        state.mcpHealthy = false;
        state.stackRunning = false;
        state.lastRecoveryAttempt = Date.now();
        await startOpenMemory();
      }
    } else if (!state.startedAt || Date.now() - (state.lastRecoveryAttempt ?? 0) > 120_000) {
      // Retry recovery every 2 minutes if still unhealthy and Claude sessions active
      logFn('[memory] Services unhealthy with active sessions, retrying recovery...');
      state.lastRecoveryAttempt = Date.now();
      await startOpenMemory();
    }

    // After successful recovery, trigger MCP reconnect
    // Only on unhealthy -> healthy transition to avoid unnecessary config writes
    if (wasUnhealthy && state.apiHealthy) {
      await triggerMcpReconnectForAllFolders('recovery');
    }

    // ── MCP endpoint health probe ──────────────────────────────────────
    // REST API can be healthy while MCP endpoint is broken (e.g., import
    // errors, session manager issues, endpoint misconfiguration). This
    // catches that case and triggers reconnection so Claude Code re-inits.
    // Only runs when REST API is healthy (no point probing MCP if API is down).
    // Cooldown: at most once per 60s to avoid hammering .mcp.json writes.
    if (state.apiHealthy) {
      const mcpOk = await checkMcpEndpointHealth();
      const wasMcpUnhealthy = !state.mcpHealthy;

      state.mcpHealthy = mcpOk;

      if (!mcpOk) {
        // MCP is broken while REST is fine — log but don't restart service
        // (the process is running, it's the MCP layer that's broken)
        if (wasMcpUnhealthy) {
          logFn('[memory] MCP endpoint still unhealthy (REST API OK)');
        } else {
          logFn('[memory] MCP endpoint became unhealthy (REST API OK) — will trigger reconnect on recovery');
        }
      } else if (wasMcpUnhealthy) {
        // MCP recovered — trigger reconnect so Claude Code picks up the working endpoint
        logFn('[memory] MCP endpoint recovered');
        await triggerMcpReconnectForAllFolders('mcp-recovery');
      }
    }

    // ── Startup reconnect ─────────────────────────────────────────────
    // Fire once when MCP is first confirmed healthy after activation.
    // Catches the case where OpenMemory was already running (healthy) at
    // startup but Claude Code's SSE handshake failed — no unhealthy→healthy
    // transition occurs, so no reconnect would fire without this.
    if (state.apiHealthy && state.mcpHealthy && !startupReconnectDone) {
      startupReconnectDone = true;
      await triggerMcpReconnectForAllFolders('startup');
    }

    // ── Keepalive reconnect ────────────────────────────────────────────
    // Periodically force a reconnect even when everything looks healthy.
    // This catches silent disconnections where Claude Code's SSE client
    // failed the initial handshake or dropped mid-session, but the server
    // never went down (so no unhealthy→healthy transition fires).
    if (state.apiHealthy && state.mcpHealthy) {
      const timeSinceLastReconnect = state.lastMcpReconnect
        ? Date.now() - state.lastMcpReconnect
        : Infinity;
      if (timeSinceLastReconnect >= MCP_KEEPALIVE_INTERVAL_MS) {
        await triggerMcpReconnectForAllFolders('keepalive');
      }
    }
  }

  // Last Claude session exited → keep service running (shared across projects)
  if (previousCount > 0 && currentCount === 0) {
    logFn('[memory] All Claude sessions ended. Memory service stays running (shared resource).');
  }
}

/**
 * Run diagnostics and return report.
 * Used by `immorterm doctor` command.
 *
 * @returns Diagnostic report
 */
export async function runDiagnostics(): Promise<string> {
  const lines: string[] = ['=== ImmorTerm Memory Diagnostics ===', ''];

  // Native binary
  const nativeStatus = await getNativeStatus();
  lines.push('Native Binary:');
  lines.push(`  Path: ${nativeStatus.binaryPath ?? 'NOT FOUND'}`);
  lines.push(`  Running: ${nativeStatus.running ? `Yes (PID ${nativeStatus.pid})` : 'No'}`);
  lines.push(`  Healthy: ${nativeStatus.healthy ? 'Yes' : 'No'}`);
  lines.push('');

  // MCP check
  if (nativeStatus.healthy) {
    const mcpOk = await checkMcpEndpointHealth();
    lines.push('MCP Endpoint:');
    lines.push(`  Healthy: ${mcpOk ? 'Yes' : 'No'}`);
    if (!mcpOk) {
      lines.push('  Tip: REST API is healthy but MCP is not responding.');
      lines.push('  Try: npx immorterm memory down && npx immorterm memory up');
    }
    lines.push('');
  }

  // State
  lines.push('State:');
  lines.push(`  Active Sessions: ${state.activeClaudeSessions.size}`);
  lines.push(`  MCP Healthy: ${state.mcpHealthy ? 'Yes' : 'No'}`);
  lines.push(`  Started At: ${state.startedAt ? new Date(state.startedAt).toISOString() : 'N/A'}`);
  lines.push(`  Last Error: ${state.lastError || 'None'}`);

  if (!nativeStatus.binaryPath) {
    lines.push('');
    lines.push('Install the memory service:');
    lines.push('  cargo build --release -p immorterm-memory');
    lines.push('  cp target/release/immorterm-memory ~/.immorterm/bin/');
  }

  return lines.join('\n');
}

/**
 * Attempt to fix common issues.
 * Used by `immorterm doctor` command.
 *
 * @returns true if issues were fixed
 */
export async function tryAutoFix(): Promise<boolean> {
  logFn('[memory] Running auto-fix...');

  // Try starting everything
  const result = await startOpenMemory();

  return result.apiHealthy;
}

export default {
  initOpenMemoryManager,
  checkOpenMemoryHealth,
  waitForOpenMemory,
  startOpenMemory,
  stopOpenMemory,
  getOpenMemoryState,
  refreshOpenMemoryState,
  checkOpenMemoryLifecycle,
  runDiagnostics,
  tryAutoFix,
};
