/**
 * Hub sidecar — VS Code-side mirror of `apps/immorterm-app/src-tauri/src/
 * hub_sidecar.rs`. The standalone Tauri app spawns immorterm-hub on port
 * 1440 at boot; VS Code now does the same so webview HTTP fetches
 * (`/api/v1/config`, `/api/v1/digest/test`, etc.) hit a real hub and
 * not the webview\u2019s static-resource server (which returns 403 on
 * non-allowlisted POSTs).
 *
 * Behavior:
 *   1. If a hub already responds on :1440 \u2192 reuse it (silent, fast).
 *   2. Else if the port is free \u2192 spawn target/release/immorterm-hub
 *      (or target/debug as fallback) with the workspace\u2019s extension
 *      resources as --static-dir.
 *   3. Else \u2192 log loudly and return; webview HTTP calls will fail with
 *      the modal\u2019s "no hub" message rather than 403 confusion.
 *
 * The child is killed in deactivate() so we don\u2019t leak processes
 * across VS Code reloads.
 */

import * as path from 'node:path';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as http from 'node:http';
import { type ChildProcess, spawn } from 'node:child_process';
import * as vscode from 'vscode';

import { logger } from './utils/logger';

export const HUB_PORT = 1440;

let hubChild: ChildProcess | null = null;
let supervisorTimer: NodeJS.Timeout | null = null;
let restartAttempts = 0;
const MAX_RESTART_ATTEMPTS = 5;
const RESTART_BASE_DELAY_MS = 500;

/** Probe :HUB_PORT for a hub. Returns true iff GET /api/info returns 200. */
function hubIsReachable(timeoutMs = 400): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.get(
      { host: '127.0.0.1', port: HUB_PORT, path: '/api/info', timeout: timeoutMs },
      (res) => {
        const ok = (res.statusCode ?? 0) >= 200 && (res.statusCode ?? 0) < 300;
        res.resume();
        resolve(ok);
      }
    );
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}

/** Returns true iff we can bind :HUB_PORT ourselves. */
function portIsBindable(): Promise<boolean> {
  return new Promise((resolve) => {
    const tester = net.createServer();
    tester.once('error', () => { resolve(false); });
    tester.once('listening', () => {
      tester.close(() => resolve(true));
    });
    tester.listen(HUB_PORT, '127.0.0.1');
  });
}

/** Walk up from `start` looking for any subdir containing target/{release,debug}/immorterm-hub. */
function findHubInTree(start: string): string | null {
  let dir = start;
  for (let i = 0; i < 10; i++) {
    for (const profile of ['release', 'debug']) {
      const cand = path.join(dir, 'target', profile, 'immorterm-hub');
      if (fs.existsSync(cand) && isExecutable(cand)) return cand;
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

function isExecutable(p: string): boolean {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** Locate the hub binary. Tries (in order):
 *  1. IMMORTERM_HUB_BIN env override (power users / CI).
 *  2. The extension's bundled bin/ dir (marketplace install — TODO).
 *  3. Walking up from VS Code's open workspace folder (dev install).
 *  4. Walking up from __dirname (only useful when running from source).
 */
function resolveHubBinary(): { path: string; source: string } | null {
  const envOverride = process.env.IMMORTERM_HUB_BIN;
  if (envOverride && fs.existsSync(envOverride) && isExecutable(envOverride)) {
    return { path: envOverride, source: 'env' };
  }

  // Bundled binary (marketplace install) — Tauri-style platform sidecar.
  // Extension Marketplace packages ship per-platform binaries under
  // `<ext>/bin/<platform>/immorterm-hub`. Look there first.
  // __dirname after install is .../immorterm.immorterm-terminal-1.0.4/out so go up 1.
  const extRoot = path.dirname(__dirname);
  const platform = process.platform;
  const arch = process.arch;
  for (const candidate of [
    path.join(extRoot, 'bin', `${platform}-${arch}`, 'immorterm-hub'),
    path.join(extRoot, 'bin', platform, 'immorterm-hub'),
    path.join(extRoot, 'bin', 'immorterm-hub'),
  ]) {
    if (fs.existsSync(candidate) && isExecutable(candidate)) {
      return { path: candidate, source: 'bundled' };
    }
  }

  // Workspace dev tree — check VS Code's open folder(s) for the binary.
  const folders = vscode.workspace.workspaceFolders ?? [];
  for (const folder of folders) {
    const found = findHubInTree(folder.uri.fsPath);
    if (found) return { path: found, source: `workspace:${folder.name}` };
  }

  // Source-tree (running from cloned repo via `bun run watch`).
  const fromSource = findHubInTree(__dirname);
  if (fromSource) return { path: fromSource, source: 'source-tree' };

  return null;
}

/** Locate the resources dir (gpu-terminal.html etc.) for --static-dir. */
function resolveStaticDir(): string | null {
  // The installed extension always carries its own resources/ — that's
  // the canonical static-dir wherever we run.
  const extRoot = path.dirname(__dirname);
  const installed = path.join(extRoot, 'resources');
  if (fs.existsSync(path.join(installed, 'gpu-terminal.html'))) return installed;

  // Source-tree fallback for `bun run watch` launches.
  let dir = __dirname;
  for (let i = 0; i < 10; i++) {
    const cand = path.join(dir, 'apps', 'extension', 'resources');
    if (fs.existsSync(path.join(cand, 'gpu-terminal.html'))) return cand;
    const cand2 = path.join(dir, 'resources');
    if (fs.existsSync(path.join(cand2, 'gpu-terminal.html'))) return cand2;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

/** Wait up to `timeoutMs` for the hub to respond, polling every 100ms. */
async function waitForHub(timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await hubIsReachable()) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return false;
}

/** Result for callers (modal Retry button). */
export interface HubStatus {
  running: boolean;
  reason?: string;
  details?: string;
}

let stopRequested = false;

/**
 * Ensure a hub is reachable on :HUB_PORT. Idempotent. Returns a status
 * the caller (e.g. the modal's Retry button) can show inline.
 */
export async function ensureHubRunning(): Promise<HubStatus> {
  if (await hubIsReachable()) {
    logger.info(`[hub-sidecar] reusing running hub on :${HUB_PORT}`);
    restartAttempts = 0;
    return { running: true, reason: 'reused' };
  }
  if (!(await portIsBindable())) {
    const reason = `Port ${HUB_PORT} is bound by another process.`;
    logger.warn(`[hub-sidecar] ${reason}`);
    return {
      running: false,
      reason,
      details: `Run \`lsof -nP -iTCP:${HUB_PORT}\` to identify it, then free the port and reload.`,
    };
  }
  const bin = resolveHubBinary();
  if (!bin) {
    const reason = 'Hub binary not found.';
    logger.warn(`[hub-sidecar] ${reason}`);
    return {
      running: false,
      reason,
      details:
        'No immorterm-hub binary in IMMORTERM_HUB_BIN, the extension\u2019s bundled bin/, your workspace target/release|debug/, or the source tree. ' +
        'If you\u2019re developing locally, run: cargo build --release -p immorterm-hub',
    };
  }
  const staticDir = resolveStaticDir();
  if (!staticDir) {
    const reason = 'Extension resources directory not found.';
    logger.warn(`[hub-sidecar] ${reason}`);
    return { running: false, reason };
  }

  logger.info(
    `[hub-sidecar] spawning hub from ${bin.source}: ${bin.path} ` +
    `serve --port ${HUB_PORT} --static-dir ${staticDir}`
  );
  try {
    hubChild = spawn(
      bin.path,
      ['serve', '--port', String(HUB_PORT), '--static-dir', staticDir],
      { stdio: ['ignore', 'pipe', 'pipe'], detached: false }
    );
    hubChild.stdout?.on('data', (d) => logger.debug(`[hub] ${d.toString().trimEnd()}`));
    hubChild.stderr?.on('data', (d) => logger.debug(`[hub] ${d.toString().trimEnd()}`));
    hubChild.on('exit', (code, signal) => {
      const wasOurs = !!hubChild;
      hubChild = null;
      if (stopRequested) {
        logger.info(`[hub-sidecar] hub stopped (code=${code}, signal=${signal})`);
        return;
      }
      logger.warn(`[hub-sidecar] hub exited unexpectedly (code=${code}, signal=${signal}); scheduling restart`);
      // Crash-loop guard — exponential backoff, capped at MAX_RESTART_ATTEMPTS.
      if (wasOurs) {
        scheduleRestart();
      }
    });

    const ready = await waitForHub(5000);
    if (!ready) {
      logger.warn(`[hub-sidecar] hub didn\u2019t respond within 5s after spawn`);
      return {
        running: false,
        reason: 'Hub spawned but didn\u2019t become reachable within 5s.',
        details: 'Check VS Code Output \u2192 ImmorTerm channel for stderr.',
      };
    }
    logger.info(`[hub-sidecar] hub ready on :${HUB_PORT}`);
    restartAttempts = 0;
    return { running: true, reason: 'spawned' };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    logger.error(`[hub-sidecar] spawn failed:`, e);
    hubChild = null;
    return { running: false, reason: 'Spawn failed.', details: msg };
  }
}

function scheduleRestart(): void {
  if (supervisorTimer) return;
  if (restartAttempts >= MAX_RESTART_ATTEMPTS) {
    logger.warn(
      `[hub-sidecar] giving up after ${MAX_RESTART_ATTEMPTS} restart attempts. ` +
      `Use the Retry button in the digest-llm-test modal or reload the window.`
    );
    return;
  }
  const delay = Math.min(RESTART_BASE_DELAY_MS * Math.pow(2, restartAttempts), 30_000);
  restartAttempts += 1;
  logger.info(`[hub-sidecar] restart attempt ${restartAttempts}/${MAX_RESTART_ATTEMPTS} in ${delay}ms`);
  supervisorTimer = setTimeout(() => {
    supervisorTimer = null;
    void ensureHubRunning();
  }, delay);
}

/** Stop the spawned hub if we own one. Safe to call multiple times. */
export function stopHubSidecar(): void {
  stopRequested = true;
  if (supervisorTimer) {
    clearTimeout(supervisorTimer);
    supervisorTimer = null;
  }
  if (hubChild) {
    const child = hubChild;
    hubChild = null;
    try { child.kill('SIGTERM'); } catch { /* ignore */ }
  }
}

/** Public: hub reachable right now. Used by the modal\u2019s Retry button. */
export function isHubReachable(): Promise<boolean> {
  return hubIsReachable();
}
