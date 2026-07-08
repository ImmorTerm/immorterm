/**
 * Native Memory Manager
 *
 * Manages the lifecycle of the native Rust immorterm-memory binary.
 * Priority: native binary > Docker fallback.
 */

import * as http from 'http';
import * as path from 'path';
import * as fs from 'fs';
import { auditedKill } from '../../utils/kill-audit';
import { spawn } from 'child_process';

const BINARY_NAME = 'immorterm-memory';
const DEFAULT_PORT = 8765;
const HEALTH_TIMEOUT = 3000;
const STARTUP_TIMEOUT = 10000;

/** State file written by the memory service on startup */
const STATE_FILE = path.join(process.env.HOME || '/tmp', '.immorterm', 'memory.state.json');

/**
 * Read the memory service port from its state file.
 * Falls back to DEFAULT_PORT if state.json doesn't exist, is malformed, or PID is stale.
 */
export function getMemoryPort(): number {
  try {
    const content = fs.readFileSync(STATE_FILE, 'utf-8');
    const state = JSON.parse(content);
    const pid = state.pid;
    const port = state.port;

    if (typeof port !== 'number' || typeof pid !== 'number') {
      return DEFAULT_PORT;
    }

    // Verify the process is still alive (signal 0 = existence check)
    try {
      process.kill(pid, 0);
      return port;
    } catch {
      // PID is dead — stale state file
      return DEFAULT_PORT;
    }
  } catch {
    return DEFAULT_PORT;
  }
}

/**
 * Get the memory service base URL.
 */
export function getMemoryUrl(): string {
  return `http://127.0.0.1:${getMemoryPort()}`;
}

let logFn: (msg: string) => void = console.log;

export function initNativeMemoryManager(logger: (msg: string) => void): void {
  logFn = logger;
}

/**
 * Find the native binary in standard locations.
 * Returns the absolute path or null if not found.
 */
export function findBinary(): string | null {
  const home = process.env.HOME || '/tmp';
  const candidates = [
    path.join(home, '.immorterm', 'bin', BINARY_NAME),
    ...findInPath(BINARY_NAME),
  ];

  for (const candidate of candidates) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      logFn(`[NativeMemory] Found binary: ${candidate}`);
      return candidate;
    } catch {
      // Not found or not executable
    }
  }

  return null;
}

function findInPath(name: string): string[] {
  const pathDirs = (process.env.PATH || '').split(':');
  return pathDirs.map(dir => path.join(dir, name));
}

/**
 * Check if the native service is healthy.
 */
export async function checkNativeHealth(port: number = DEFAULT_PORT): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.get(`http://127.0.0.1:${port}/health`, { timeout: HEALTH_TIMEOUT }, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}

/**
 * Start the native memory service as a daemon.
 * Returns true if started successfully.
 */
export async function startNativeMemory(binaryPath: string, port: number = DEFAULT_PORT): Promise<boolean> {
  logFn(`[NativeMemory] Starting ${binaryPath} on port ${port}...`);

  // Check if already running
  if (await checkNativeHealth(port)) {
    logFn('[NativeMemory] Already running and healthy');
    return true;
  }

  // Spawn as detached daemon
  const child = spawn(binaryPath, ['serve', '--port', String(port), '--daemon'], {
    detached: true,
    stdio: 'ignore',
    env: { ...process.env },
  });

  child.unref();

  // Wait for health
  const startTime = Date.now();
  while (Date.now() - startTime < STARTUP_TIMEOUT) {
    await new Promise(r => setTimeout(r, 500));
    if (await checkNativeHealth(port)) {
      logFn('[NativeMemory] Started successfully');
      return true;
    }
  }

  logFn('[NativeMemory] Failed to start within timeout');
  return false;
}

/**
 * Stop the native memory service via PID file.
 */
export async function stopNativeMemory(): Promise<boolean> {
  const home = process.env.HOME || '/tmp';
  const pidPath = path.join(home, '.immorterm', 'memory.pid');

  try {
    const pid = parseInt(fs.readFileSync(pidPath, 'utf-8').trim(), 10);
    if (isNaN(pid)) {
      logFn('[NativeMemory] Invalid PID file');
      return false;
    }

    auditedKill(pid, 'SIGTERM', 'native-memory-manager: stop');
    logFn(`[NativeMemory] Sent SIGTERM to PID ${pid}`);

    // Wait for process to exit
    for (let i = 0; i < 10; i++) {
      await new Promise(r => setTimeout(r, 500));
      try {
        process.kill(pid, 0);
      } catch {
        logFn('[NativeMemory] Process exited');
        return true;
      }
    }

    logFn('[NativeMemory] Process did not exit within 5s');
    return false;
  } catch (e) {
    logFn(`[NativeMemory] Stop failed: ${e}`);
    return false;
  }
}

/**
 * Get native service status.
 */
export async function getNativeStatus(): Promise<{
  binaryPath: string | null;
  running: boolean;
  healthy: boolean;
  pid: number | null;
}> {
  const binaryPath = findBinary();
  const healthy = await checkNativeHealth();
  const home = process.env.HOME || '/tmp';
  const pidPath = path.join(home, '.immorterm', 'memory.pid');

  let pid: number | null = null;
  try {
    pid = parseInt(fs.readFileSync(pidPath, 'utf-8').trim(), 10);
    if (isNaN(pid)) pid = null;
    else {
      try { process.kill(pid, 0); } catch { pid = null; }
    }
  } catch { /* no pid file */ }

  return { binaryPath, running: pid !== null, healthy, pid };
}
