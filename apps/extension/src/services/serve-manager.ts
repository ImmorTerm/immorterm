/**
 * Serve Manager
 *
 * Manages the lifecycle of the `immorterm serve` CLI server.
 * Auto-starts the local API server (port 3847) so the web dashboard
 * can connect without the user manually running `npx immorterm serve`.
 *
 * Pattern mirrors native-memory-manager.ts: health check → spawn detached → poll health.
 */

import * as http from 'http';
import * as path from 'path';
import * as fs from 'fs';
import { spawn } from 'child_process';

const DEFAULT_PORT = 3847;
const HEALTH_TIMEOUT = 2000;
const STARTUP_TIMEOUT = 8000;

const HOME = process.env.HOME || '/tmp';
const IMMORTERM_DIR = path.join(HOME, '.immorterm');
const PID_FILE = path.join(IMMORTERM_DIR, 'serve.pid');

let logFn: (msg: string) => void = console.log;

export function initServeManager(logger: (msg: string) => void): void {
  logFn = logger;
}

/**
 * Check if the serve process is running by reading its PID file.
 */
export function isServeRunning(): { running: boolean; pid?: number } {
  try {
    if (!fs.existsSync(PID_FILE)) return { running: false };
    const pid = parseInt(fs.readFileSync(PID_FILE, 'utf-8').trim(), 10);
    if (isNaN(pid)) return { running: false };
    // signal 0 = existence check
    process.kill(pid, 0);
    return { running: true, pid };
  } catch {
    return { running: false };
  }
}

/**
 * Health-check the serve API.
 */
export async function checkServeHealth(port: number = DEFAULT_PORT): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.get(`http://127.0.0.1:${port}/api/health`, { timeout: HEALTH_TIMEOUT }, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}

/**
 * Find the immorterm CLI binary.
 * Priority: ~/.immorterm/node_modules/.bin/immorterm → PATH → npx fallback
 * Returns { path, useNpx } — if useNpx is true, spawn via shell with npx.
 */
function findCli(): { binPath: string; useNpx: boolean } {
  // 1. Local node_modules install
  const localBin = path.join(IMMORTERM_DIR, 'node_modules', '.bin', 'immorterm');
  try {
    fs.accessSync(localBin, fs.constants.X_OK);
    logFn(`[Serve] Found CLI: ${localBin}`);
    return { binPath: localBin, useNpx: false };
  } catch { /* not found */ }

  // 2. Search PATH
  const pathDirs = (process.env.PATH || '').split(':');
  for (const dir of pathDirs) {
    const candidate = path.join(dir, 'immorterm');
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      logFn(`[Serve] Found CLI in PATH: ${candidate}`);
      return { binPath: candidate, useNpx: false };
    } catch { /* not found */ }
  }

  // 3. npx fallback
  logFn('[Serve] CLI not found locally, will use npx');
  return { binPath: 'npx', useNpx: true };
}

/**
 * Start the serve API server as a detached background process.
 * Returns true if the server is healthy after startup.
 */
export async function startServe(port: number = DEFAULT_PORT): Promise<boolean> {
  logFn(`[Serve] Starting dashboard API server on port ${port}...`);

  // Already running?
  if (await checkServeHealth(port)) {
    logFn('[Serve] Already running and healthy');
    return true;
  }

  const { binPath, useNpx } = findCli();

  if (useNpx) {
    spawn('npx', ['--yes', 'immorterm', 'serve', '--port', String(port)], {
      detached: true,
      stdio: 'ignore',
      shell: true,
      env: { ...process.env },
    }).unref();
  } else {
    spawn(binPath, ['serve', '--port', String(port)], {
      detached: true,
      stdio: 'ignore',
      env: { ...process.env },
    }).unref();
  }

  // Poll for health
  const startTime = Date.now();
  while (Date.now() - startTime < STARTUP_TIMEOUT) {
    await new Promise(r => setTimeout(r, 500));
    if (await checkServeHealth(port)) {
      logFn('[Serve] Started successfully');
      return true;
    }
  }

  logFn('[Serve] Failed to start within timeout');
  return false;
}

/**
 * Get the serve port (reads PID file metadata or returns default).
 */
export function getServePort(): number {
  return DEFAULT_PORT;
}
