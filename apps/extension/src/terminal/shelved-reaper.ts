import { logger } from '../utils/logger';
import { getShelvedSessions, removeTerminalFromRegistry, removeSessionStatus } from '../registry-client';
import { screenCommands } from '../utils/screen-commands';
import { getShelvedSessionTtl } from '../utils/settings';
import { auditedKill } from '../utils/kill-audit';

const REAPER_INTERVAL = 15 * 60 * 1000; // Check every 15 minutes

let reaperTimer: NodeJS.Timeout | null = null;

/**
 * Reaps shelved sessions whose TTL has expired.
 *
 * For each shelved session:
 * - If shelved_at + TTL expired → kill session, remove from registry
 * - If process already dead → mark dead, remove from registry
 */
async function reapShelvedSessions(): Promise<void> {
  const shelved = getShelvedSessions();
  if (shelved.length === 0) return;

  const ttlHours = getShelvedSessionTtl();
  const ttlSeconds = ttlHours * 3600;
  const now = Math.floor(Date.now() / 1000);

  for (const entry of shelved) {
    const shelvedAt = entry.shelved_at || 0;
    const age = now - shelvedAt;

    if (age < ttlSeconds) continue;

    const windowId = entry.window_id;
    const displayName = entry.display_name || entry.name;
    logger.info(`Reaping expired shelved session: "${displayName}" (shelved ${Math.floor(age / 3600)}h ago, TTL: ${ttlHours}h)`);

    // Kill the screen session if it's still alive
    if (entry.name && entry.session_type !== 'ai') {
      try {
        // Screen session name is project-windowId
        const projectPrefix = entry.project_dir?.split('/').pop() || '';
        const screenSession = `${projectPrefix}-${windowId}`;
        await screenCommands.killSession(screenSession);
        logger.debug(`Killed expired screen session: ${screenSession}`);
      } catch (error) {
        logger.debug(`Screen session already dead or not found for ${windowId}`);
      }
    }

    // For AI daemon sessions, kill the process
    if (entry.session_type === 'ai' && entry.pid) {
      auditedKill(entry.pid, 'SIGTERM', `shelved-reaper: expired AI daemon (shelved ${Math.floor(age / 3600)}h ago)`);
      logger.debug(`Sent SIGTERM to expired AI daemon pid ${entry.pid}`);
    }

    // Remove from registry + session-status (TTL expiry = permanent forget)
    removeTerminalFromRegistry(windowId);
    removeSessionStatus(windowId);
    logger.info(`Reaped shelved session: ${windowId}`);
  }
}

/**
 * Starts the shelved session reaper timer.
 * Should be called from extension activate().
 */
export function startShelvedReaper(): void {
  if (reaperTimer) return; // Already running

  reaperTimer = setInterval(() => {
    reapShelvedSessions().catch(err => {
      logger.warn('Shelved reaper error:', err);
    });
  }, REAPER_INTERVAL);

  logger.debug(`Shelved session reaper started (interval: ${REAPER_INTERVAL / 1000}s)`);
}

/**
 * Stops the shelved session reaper timer.
 * Should be called from extension deactivate().
 */
export function stopShelvedReaper(): void {
  if (reaperTimer) {
    clearInterval(reaperTimer);
    reaperTimer = null;
    logger.debug('Shelved session reaper stopped');
  }
}
