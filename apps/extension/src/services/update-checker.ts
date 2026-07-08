/**
 * Background Auto-Update Checker
 *
 * Periodically checks for npm version updates (MCP gateway)
 * and native binary version updates (memory service),
 * then restarts transparently.
 *
 * Runs on a long interval (default: 6 hours), separate from the 30s
 * health check timer in openmemory-manager.
 */

import * as http from 'http';
import * as https from 'https';
import { execFile } from 'child_process';
import { promisify } from 'util';
import { readGlobalConfig, writeGlobalConfig, AutoUpdateConfig } from '../utils/immorterm-config';
import { stopGateway, startGateway } from './mcp-gateway/gateway-manager';
import { getHealthUrl, GATEWAY_PORT } from './mcp-gateway/gateway-config';

let updateTimer: NodeJS.Timeout | null = null;
let startupTimer: NodeJS.Timeout | null = null;
let logFn: (msg: string) => void = console.log;
let checking = false; // prevent concurrent checks

const DEFAULT_CONFIG: AutoUpdateConfig = {
  enabled: true,
  checkIntervalHours: 6,
  lastCheckedAt: null,
};

/**
 * Start the background update checker.
 * Schedules an initial check after 60s and then repeats on the configured interval.
 */
export function startUpdateChecker(logger: (msg: string) => void): void {
  logFn = logger;

  const config = readGlobalConfig();
  const autoUpdate = config.autoUpdate ?? DEFAULT_CONFIG;

  if (!autoUpdate.enabled) {
    logFn('[auto-update] Disabled by config');
    return;
  }

  const intervalMs = autoUpdate.checkIntervalHours * 60 * 60 * 1000;

  // Initial check after 60s (let services stabilize first)
  startupTimer = setTimeout(() => {
    startupTimer = null;
    checkForUpdates().catch((err) => {
      logFn(`[auto-update] Initial check failed: ${err}`);
    });
  }, 60_000);

  // Recurring check
  updateTimer = setInterval(() => {
    checkForUpdates().catch((err) => {
      logFn(`[auto-update] Periodic check failed: ${err}`);
    });
  }, intervalMs);

  logFn(`[auto-update] Started (interval: ${autoUpdate.checkIntervalHours}h)`);
}

/**
 * Stop the background update checker and clear all timers.
 */
export function stopUpdateChecker(): void {
  if (startupTimer) {
    clearTimeout(startupTimer);
    startupTimer = null;
  }
  if (updateTimer) {
    clearInterval(updateTimer);
    updateTimer = null;
  }
}

/**
 * Run a single update check cycle.
 * Checks gateway (npm) updates. Memory binary updates will be added
 * when we have a distribution channel (brew, cargo-binstall, etc).
 */
async function checkForUpdates(): Promise<void> {
  // Prevent concurrent checks
  if (checking) return;
  checking = true;

  try {
    const config = readGlobalConfig();
    const autoUpdate = config.autoUpdate ?? DEFAULT_CONFIG;

    if (!autoUpdate.enabled) return;

    // Skip if checked too recently
    if (autoUpdate.lastCheckedAt) {
      const lastCheck = new Date(autoUpdate.lastCheckedAt).getTime();
      const intervalMs = autoUpdate.checkIntervalHours * 60 * 60 * 1000;
      if (!Number.isNaN(lastCheck) && Date.now() - lastCheck < intervalMs) {
        logFn('[auto-update] Skipping — last check was too recent');
        return;
      }
    }

    logFn('[auto-update] Checking for updates...');

    // Gateway npm update check
    const result = await Promise.allSettled([
      checkGatewayUpdates(),
    ]);

    for (const r of result) {
      if (r.status === 'rejected') {
        logFn(`[auto-update] Check failed: ${r.reason}`);
      }
    }

    // Persist lastCheckedAt (re-read config in case it changed during check)
    const freshConfig = readGlobalConfig();
    if (!freshConfig.autoUpdate) {
      freshConfig.autoUpdate = { ...DEFAULT_CONFIG };
    }
    freshConfig.autoUpdate.lastCheckedAt = new Date().toISOString();
    writeGlobalConfig(freshConfig);

    logFn('[auto-update] Check complete');
  } finally {
    checking = false;
  }
}

// ── Gateway (npm) Updates ────────────────────────────────────────

/**
 * Check for npm version updates for the MCP gateway.
 * Compares current running version (from health endpoint) against npm registry.
 * If newer version available, installs and restarts.
 */
async function checkGatewayUpdates(): Promise<void> {
  // Get current version from running gateway
  const currentVersion = await getGatewayVersion();
  if (!currentVersion) {
    // Gateway not running or version not available — skip
    return;
  }

  // Query npm registry for latest version
  const latestVersion = await fetchNpmLatestVersion('immorterm-mcp-gateway');
  if (!latestVersion) {
    logFn('[auto-update] Could not query npm registry for gateway version');
    return;
  }

  if (!isNewerVersion(latestVersion, currentVersion)) {
    logFn(`[auto-update] Gateway is up to date (v${currentVersion})`);
    return;
  }

  logFn(`[auto-update] Gateway update available: v${currentVersion} → v${latestVersion}`);
  logFn('[auto-update] Installing gateway update...');

  try {
    const execFileAsync = promisify(execFile);
    await execFileAsync('npm', ['install', '-g', `immorterm-mcp-gateway@${latestVersion}`], {
      timeout: 120_000,
    });

    logFn('[auto-update] Gateway installed, restarting...');
    await stopGateway();
    await startGateway();
    logFn('[auto-update] Gateway restarted with new version');
  } catch (err) {
    logFn(`[auto-update] Gateway update failed: ${err}`);
  }
}

/**
 * Get the current gateway version from its health endpoint.
 * Returns null if gateway is not running or version field is missing.
 */
function getGatewayVersion(): Promise<string | null> {
  return new Promise((resolve) => {
    const req = http.get(getHealthUrl(GATEWAY_PORT), { timeout: 3000 }, (res) => {
      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        if (res.statusCode === 200) {
          try {
            const health = JSON.parse(data);
            resolve(health.version ?? null);
          } catch {
            resolve(null);
          }
        } else {
          resolve(null);
        }
      });
    });
    req.on('error', () => resolve(null));
    req.on('timeout', () => { req.destroy(); resolve(null); });
  });
}

/**
 * Fetch the latest published version of an npm package.
 * Uses the public npm registry API (no auth needed).
 */
function fetchNpmLatestVersion(packageName: string): Promise<string | null> {
  return new Promise((resolve) => {
    const url = `https://registry.npmjs.org/${packageName}/latest`;
    const req = https.get(url, { timeout: 10000 }, (res) => {
      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        if (res.statusCode === 200) {
          try {
            const pkg = JSON.parse(data);
            resolve(pkg.version ?? null);
          } catch {
            resolve(null);
          }
        } else {
          resolve(null);
        }
      });
    });
    req.on('error', () => resolve(null));
    req.on('timeout', () => { req.destroy(); resolve(null); });
  });
}

/**
 * Compare semver versions. Returns true if `latest` is newer than `current`.
 * Simple numeric comparison — handles standard x.y.z semver.
 */
export function isNewerVersion(latest: string, current: string): boolean {
  const parse = (v: string) => v.replace(/^v/, '').split('.').map(Number);
  const l = parse(latest);
  const c = parse(current);

  for (let i = 0; i < Math.max(l.length, c.length); i++) {
    const lv = l[i] ?? 0;
    const cv = c[i] ?? 0;
    if (lv > cv) return true;
    if (lv < cv) return false;
  }
  return false;
}
