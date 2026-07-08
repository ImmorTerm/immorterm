import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';

/**
 * Result of a cleanup operation
 */
export interface CleanupResult {
  /** Number of stale terminal entries removed from storage */
  entriesRemoved: number;
  /** Number of orphaned log files deleted */
  logsDeleted: number;
  /** Number of session directories archived */
  sessionsArchived: number;
  /** Window IDs of terminals that were cleaned up */
  cleanedWindowIds: string[];
}

/**
 * Read session.json from a session directory if it exists.
 */
async function readSessionJson(sessionDir: string): Promise<Record<string, unknown> | null> {
  try {
    const raw = await fs.readFile(path.join(sessionDir, 'session.json'), 'utf-8');
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/**
 * Archive a session directory: move from logs/ to logs/archive/.
 * Preserves the original directory name ({date}_{windowId}) so that
 * unarchive + daemon's find_existing_session_dir() can find it by windowId.
 */
export async function archiveSessionDir(
  sessionDir: string,
  archiveDir: string,
): Promise<boolean> {
  try {
    const dirName = path.basename(sessionDir);
    const sessionJson = await readSessionJson(sessionDir);
    const archivePath = path.join(archiveDir, dirName);

    // Update session.json with archive metadata before moving
    if (sessionJson) {
      sessionJson.status = 'archived';
      sessionJson.archived_at = Math.floor(Date.now() / 1000);

      // Compute final file sizes
      const sizes: Record<string, number> = {};
      for (const file of ['grid.jsonl', 'cast', 'ai.jsonl', 'raw.log']) {
        try {
          const stat = await fs.stat(path.join(sessionDir, file));
          sizes[file.replace('.jsonl', '').replace('.log', '')] = stat.size;
        } catch {
          sizes[file.replace('.jsonl', '').replace('.log', '')] = 0;
        }
      }
      sessionJson.sizes = sizes;

      await fs.writeFile(
        path.join(sessionDir, 'session.json'),
        JSON.stringify(sessionJson, null, 2),
      );
    }

    // Move to archive (preserve original name)
    await fs.mkdir(archiveDir, { recursive: true });
    await fs.rename(sessionDir, archivePath);

    logger.info(`Archived session: ${dirName} → archive/${dirName}`);

    // Index into memory (async, non-blocking)
    indexArchivedSession(archivePath, sessionJson).catch(() => {});

    return true;
  } catch (err) {
    logger.warn(`Failed to archive session dir ${sessionDir}:`, err);
    return false;
  }
}

/**
 * Index an archived session into immorterm-memory for semantic search.
 *
 * Extracts session metadata + AI conversation summary and saves as a memory.
 * Non-blocking: failures are logged but don't affect the archive operation.
 */
async function indexArchivedSession(
  archivePath: string,
  sessionJson: Record<string, unknown> | null,
): Promise<void> {
  if (!sessionJson) return;

  try {
    const displayName = sessionJson.display_name as string || 'Unknown';
    const projectDir = sessionJson.project_dir as string || '';
    const sessionType = sessionJson.session_type as string || 'regular';
    const createdAt = sessionJson.created_at as number || 0;
    const windowId = sessionJson.window_id as string || '';

    // Build a summary text for the memory
    const parts: string[] = [
      `Archived terminal session: "${displayName}"`,
      `Project: ${projectDir}`,
      `Type: ${sessionType}`,
      `Created: ${new Date(createdAt * 1000).toISOString()}`,
    ];

    // Try to extract AI conversation summary
    const aiPath = path.join(archivePath, 'ai.jsonl');
    try {
      const raw = await fs.readFile(aiPath, 'utf-8');
      const lines = raw.split('\n').filter(l => l.trim());
      if (lines.length > 0) {
        let turnCount = 0;
        let lastModel = '';
        for (const line of lines) {
          try {
            const event = JSON.parse(line);
            if (event.type === 'turn') turnCount++;
            if (event.model) lastModel = event.model;
          } catch { /* skip malformed lines */ }
        }
        if (turnCount > 0) {
          parts.push(`AI conversation: ${turnCount} turns${lastModel ? ` (${lastModel})` : ''}`);
        }
      }
    } catch {
      // No AI log
    }

    // Add Claude stats if available
    const claudeStats = sessionJson.claude_stats as Record<string, unknown> | undefined;
    if (claudeStats) {
      if (claudeStats.model) parts.push(`Model: ${claudeStats.model}`);
      if (claudeStats.cost_usd) parts.push(`Cost: $${(claudeStats.cost_usd as number).toFixed(2)}`);
    }

    const memoryText = parts.join('\n');

    // POST to immorterm-memory REST API (non-blocking, best-effort)
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 3000);

    const { getMemoryUrl } = await import('../services/memory/native-memory-manager');
    const res = await fetch(`${getMemoryUrl()}/api/v1/memories/`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        text: memoryText,
        user_id: 'lonormaly-immorterm',
        metadata: {
          category: 'session_archive',
          session_type: sessionType,
          project_dir: projectDir,
          display_name: displayName,
          window_id: windowId,
          created_at: createdAt,
          archived_at: Math.floor(Date.now() / 1000),
          archive_path: archivePath,
        },
      }),
      signal: controller.signal,
    });

    clearTimeout(timeout);

    if (res.ok) {
      logger.debug(`Indexed archived session to memory: ${displayName}`);
    } else {
      logger.debug(`Memory indexing returned ${res.status} for ${displayName}`);
    }
  } catch {
    // Memory indexing is best-effort — don't log warnings for connection failures
    // (memory server may not be running)
  }
}

/**
 * Extract the windowId from a session dir name. Matches new bare format
 * (`<windowId>`) and legacy dated format (`<YYYY-MM-DD>_<windowId>`).
 * Returns null for unrecognized names (e.g. `archive`).
 */
function windowIdFromDirName(dirName: string): string | null {
  // New bare format — accept if it matches windowId shape: digits-hex.
  if (/^\d+-[0-9a-zA-Z]+$/.test(dirName)) return dirName;
  // Legacy dated format.
  const match = dirName.match(/^\d{4}-\d{2}-\d{2}_(.+)$/);
  if (match && /^\d+-[0-9a-zA-Z]+$/.test(match[1])) return match[1];
  return null;
}

/**
 * Startup reconciler: archive every AI session dir whose windowId is NOT in
 * the `liveWindowIds` set, AND drop the matching registry entry.
 *
 * Invariant: `.immorterm/terminals/logs/` holds dirs only for currently
 * active sessions; everything else in `archive/`. `registry.json` holds
 * entries only for currently alive or restore-eligible sessions; orphaned
 * entries (daemon dead AND no live log dir) are removed.
 *
 * Called once near end of `doRestoreSessions()` after the live session set
 * is final. Leaves untouched: `archive/` itself, any non-session files, any
 * dir whose name doesn't match a windowId shape (safety).
 *
 * Returns the number of dirs archived.
 */
export async function reconcileOrphanedAiDirs(
  logsDir: string,
  liveWindowIds: ReadonlySet<string>,
): Promise<number> {
  const archiveDir = path.join(logsDir, 'archive');
  let archivedCount = 0;
  try {
    const entries = await fs.readdir(logsDir, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (entry.name === 'archive') continue;
      const windowId = windowIdFromDirName(entry.name);
      if (!windowId) continue;  // Safety: don't touch unrecognized dirs.
      if (liveWindowIds.has(windowId)) continue;
      const sessionDir = path.join(logsDir, entry.name);
      const ok = await archiveSessionDir(sessionDir, archiveDir);
      if (ok) {
        archivedCount++;
        // Drop the registry entry too — the session is archived, so
        // registry.json should no longer advertise it as restorable.
        // Idempotent; missing entries are a no-op.
        try {
          const { removeTerminalFromRegistry } = await import('../registry-client');
          removeTerminalFromRegistry(windowId);
        } catch { /* best effort */ }
      }
    }
  } catch (err) {
    logger.warn(`reconcileOrphanedAiDirs failed:`, err);
  }
  if (archivedCount > 0) {
    logger.info(`Reconciled ${archivedCount} orphaned AI session dirs → archive/ + registry trimmed`);
  }
  return archivedCount;
}

/**
 * Find a session directory by window ID (in logs/ or archive/).
 * Matches BOTH new bare `{windowId}` format AND legacy `{YYYY-MM-DD}_{windowId}`.
 */
export async function findSessionDir(baseDir: string, windowId: string): Promise<string | null> {
  try {
    const entries = await fs.readdir(baseDir, { withFileTypes: true });
    const legacySuffix = `_${windowId}`;
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      // New format: bare windowId (no date prefix).
      if (entry.name === windowId) {
        return path.join(baseDir, entry.name);
      }
      // Legacy format: YYYY-MM-DD_windowId.
      if (entry.name.endsWith(legacySuffix)) {
        return path.join(baseDir, entry.name);
      }
    }
  } catch {
    // Directory doesn't exist or isn't accessible
  }
  return null;
}

/**
 * Unarchive a session directory: move from logs/archive/ back to logs/.
 * Called when a shelved terminal is reattached.
 */
export async function unarchiveSessionDir(
  logsDir: string,
  windowId: string,
): Promise<string | null> {
  const archiveDir = path.join(logsDir, 'archive');
  const archivedDir = await findSessionDir(archiveDir, windowId);

  if (!archivedDir) {
    // Also try session.json fallback for dirs archived before this fix (renamed to displayName)
    const fallback = await findSessionDirByJson(archiveDir, windowId);
    if (!fallback) {
      logger.debug(`No archived session dir found for ${windowId}`);
      return null;
    }
    // Restore with correct windowId-based name
    return unarchiveDir(fallback, logsDir, windowId);
  }

  return unarchiveDir(archivedDir, logsDir, windowId);
}

async function unarchiveDir(
  archivedDir: string,
  logsDir: string,
  windowId: string,
): Promise<string | null> {
  try {
    // Restore with bare windowId name (new format, no date prefix).
    // Legacy dirs archived with date-prefix names get normalized on restore.
    const restoredName = windowId;
    const destPath = path.join(logsDir, restoredName);

    // Update session.json to remove archive metadata
    const sessionJson = await readSessionJson(archivedDir);
    if (sessionJson) {
      delete sessionJson.status;
      delete sessionJson.archived_at;
      await fs.writeFile(
        path.join(archivedDir, 'session.json'),
        JSON.stringify(sessionJson, null, 2),
      );
    }

    const archivedName = path.basename(archivedDir);
    await fs.rename(archivedDir, destPath);
    logger.info(`Unarchived session: archive/${archivedName} → ${restoredName}`);
    return destPath;
  } catch (err) {
    logger.warn(`Failed to unarchive session dir for ${windowId}:`, err);
    return null;
  }
}

/**
 * Find a session directory by reading session.json inside each subdirectory.
 * Fallback for dirs archived before the rename-removal fix (legacy displayName-based names).
 */
async function findSessionDirByJson(baseDir: string, windowId: string): Promise<string | null> {
  try {
    const entries = await fs.readdir(baseDir, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const dirPath = path.join(baseDir, entry.name);
      const sessionJson = await readSessionJson(dirPath);
      if (sessionJson?.window_id === windowId) {
        return dirPath;
      }
    }
  } catch {
    // Directory doesn't exist or isn't accessible
  }
  return null;
}

/**
 * Archive a session directory by window ID.
 * Convenience wrapper that finds the dir in logsDir and archives it.
 */
export async function archiveSessionByWindowId(
  logsDir: string,
  windowId: string,
): Promise<boolean> {
  const sessionDir = await findSessionDir(logsDir, windowId);
  if (!sessionDir) {
    // Bumped to WARN: a silent archive failure causes shelved sessions to
    // reattach as empty terminals because there's no grid.jsonl to replay.
    // Surface the case so we notice path mismatches between the daemon's
    // SCREEN_PROJECT_DIR-rooted path and the extension's getLogsDir().
    logger.warn(`No session dir found to archive for ${windowId} in ${logsDir}`);
    return false;
  }
  const archiveDir = path.join(logsDir, 'archive');
  return archiveSessionDir(sessionDir, archiveDir);
}

/**
 * Removes stale terminal entries from workspace storage.
 *
 * Compares storage entries against active Screen sessions. Stale entries
 * are removed from storage and their session directories archived.
 *
 * @param storage WorkspaceStorage instance
 * @param logsDir Path to the logs directory
 * @returns CleanupResult with counts and cleaned window IDs
 */
export async function cleanupStaleTerminals(
  storage: WorkspaceStorage,
  logsDir: string
): Promise<CleanupResult> {
  const result: CleanupResult = {
    entriesRemoved: 0,
    logsDeleted: 0,
    sessionsArchived: 0,
    cleanedWindowIds: [],
  };

  // Check if auto-cleanup is enabled
  const config = vscode.workspace.getConfiguration('immorterm');
  const autoCleanup = config.get<boolean>('autoCleanupStale', true);

  if (!autoCleanup) {
    logger.debug('Auto cleanup is disabled, skipping');
    return result;
  }

  logger.debug('Starting stale terminal cleanup');

  // Get active screen sessions for this project
  const projectName = storage.getProjectName();
  const activeSessions = await screenCommands.listProjectSessions(projectName);
  const activeSessionNames = new Set(activeSessions.map(s => s.name));

  logger.debug(`Found ${activeSessions.length} active sessions for project: ${projectName}`);

  // Get all terminals from storage
  const terminals = storage.getAllTerminals();
  const terminalsToRemove: string[] = [];

  // Find terminals with no matching screen session
  for (const terminal of terminals) {
    if (!activeSessionNames.has(terminal.screenSession)) {
      logger.info(`Stale terminal found: ${terminal.windowId} (session: ${terminal.screenSession})`);
      terminalsToRemove.push(terminal.windowId);
      result.cleanedWindowIds.push(terminal.windowId);
    }
  }

  // Remove stale terminals from storage
  for (const windowId of terminalsToRemove) {
    const removed = await storage.removeTerminal(windowId);
    if (removed) {
      result.entriesRemoved++;
    }
  }

  // Archive session directories for dead terminals
  for (const windowId of terminalsToRemove) {
    const archived = await archiveSessionByWindowId(logsDir, windowId);
    if (archived) {
      result.sessionsArchived++;
    }
  }

  // Update last cleanup timestamp
  await storage.updateLastCleanup();

  logger.info(
    `Cleanup complete: ${result.entriesRemoved} entries removed, ` +
    `${result.sessionsArchived} sessions archived, ${result.logsDeleted} logs deleted`
  );

  return result;
}

export default cleanupStaleTerminals;
