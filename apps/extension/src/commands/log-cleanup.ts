import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { logger } from '../utils/logger';
import { getShelvedSessions } from '../registry-client';

/**
 * Information about a removable entry (flat log file or session directory)
 */
interface LogEntry {
  name: string;
  path: string;
  size: number;
  /** Date prefix extracted from name (YYYY-MM-DD) for sorting, or mtime fallback */
  sortKey: string;
  type: 'file' | 'directory';
}

/**
 * Result of a log cleanup operation
 */
export interface LogCleanupResult {
  /** Total bytes freed by cleanup */
  bytesFreed: number;
  /** Number of entries removed (files or session directories) */
  filesRemoved: number;
  /** Current size of logs directory after cleanup (bytes) */
  currentSize: number;
  /** Maximum allowed size (bytes) */
  maxSize: number;
  /** Names of entries that were removed */
  removedFiles: string[];
}

/**
 * Recursively compute total size of a directory.
 */
async function dirSize(dirPath: string): Promise<number> {
  let total = 0;
  try {
    const entries = await fs.readdir(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(dirPath, entry.name);
      if (entry.isDirectory()) {
        total += await dirSize(fullPath);
      } else {
        try {
          const stat = await fs.stat(fullPath);
          total += stat.size;
        } catch {
          // Skip files we can't stat
        }
      }
    }
  } catch {
    // Directory not accessible
  }
  return total;
}

/**
 * Extract a YYYY-MM-DD date prefix from a directory/file name, or return empty string.
 */
function extractDatePrefix(name: string): string {
  const match = name.match(/^(\d{4}-\d{2}-\d{2})/);
  return match?.[1] ?? '';
}

/**
 * Collect all prunable entries from the archive directory.
 * Archived sessions are removed first (oldest by date prefix).
 */
async function getArchivedEntries(archiveDir: string): Promise<LogEntry[]> {
  const entries: LogEntry[] = [];
  try {
    const dirEntries = await fs.readdir(archiveDir, { withFileTypes: true });
    for (const entry of dirEntries) {
      if (!entry.isDirectory()) continue;
      const entryPath = path.join(archiveDir, entry.name);
      const size = await dirSize(entryPath);
      entries.push({
        name: `archive/${entry.name}`,
        path: entryPath,
        size,
        sortKey: extractDatePrefix(entry.name) || '0000-00-00',
        type: 'directory',
      });
    }
  } catch {
    // Archive directory doesn't exist yet
  }
  // Oldest first
  entries.sort((a, b) => a.sortKey.localeCompare(b.sortKey));
  return entries;
}

/**
 * Collect legacy flat .log files from the logs directory.
 */
async function getLegacyLogFiles(logsDir: string): Promise<LogEntry[]> {
  const entries: LogEntry[] = [];
  try {
    const dirEntries = await fs.readdir(logsDir);
    for (const name of dirEntries) {
      if (!name.endsWith('.log')) continue;
      const filePath = path.join(logsDir, name);
      try {
        const stat = await fs.stat(filePath);
        if (stat.isFile()) {
          entries.push({
            name,
            path: filePath,
            size: stat.size,
            sortKey: stat.mtime.toISOString().slice(0, 10),
            type: 'file',
          });
        }
      } catch {
        // Skip
      }
    }
  } catch {
    // Directory not readable
  }
  entries.sort((a, b) => a.sortKey.localeCompare(b.sortKey));
  return entries;
}

/**
 * Compute total size of the entire logs directory tree (session dirs + archive + flat files).
 */
async function computeTotalLogsSize(logsDir: string): Promise<number> {
  return dirSize(logsDir);
}

/**
 * Remove a directory recursively.
 */
async function rmDir(dirPath: string): Promise<void> {
  await fs.rm(dirPath, { recursive: true, force: true });
}

/**
 * Manages log storage by removing oldest entries when size limit is exceeded.
 *
 * Pruning priority (FIFO within each tier):
 *   1. Oldest archived sessions (archive/{date}_{slug}/)
 *   2. Oldest legacy flat .log files
 *   3. (Never touches active session directories)
 *
 * @param logsDir Path to the logs directory
 * @param options Cleanup configuration
 * @returns LogCleanupResult with cleanup statistics
 */
export async function cleanupLogs(
  logsDir: string,
  options: {
    maxSizeMb?: number;
  } = {}
): Promise<LogCleanupResult> {
  const config = vscode.workspace.getConfiguration('immorterm');
  const maxSizeMb = options.maxSizeMb ?? config.get<number>('maxLogSizeMb', 300);
  const maxSizeBytes = maxSizeMb * 1024 * 1024;

  const result: LogCleanupResult = {
    bytesFreed: 0,
    filesRemoved: 0,
    currentSize: 0,
    maxSize: maxSizeBytes,
    removedFiles: [],
  };

  logger.debug(`Log cleanup: max size ${maxSizeMb}MB (${maxSizeBytes} bytes)`);

  result.currentSize = await computeTotalLogsSize(logsDir);
  logger.debug(`Current total log size: ${Math.round(result.currentSize / 1024 / 1024)}MB`);

  if (result.currentSize <= maxSizeBytes) {
    logger.debug('Under size limit, no pruning needed');
    return result;
  }

  // Build a set of archive dir basenames that back a SHELVED registry entry —
  // these are user-reattachable and must NOT be pruned silently. Without this
  // guard, the scheduled cleanupLogs() timer eats shelved session dirs while
  // their entries persist in registry-shelved.json, producing orphan entries
  // whose reattach yields an empty terminal (daemon spawns into a missing
  // grid.jsonl).
  const shelvedBaseNames = new Set(
    getShelvedSessions()
      .map(e => e.structured_log_dir)
      .filter((p): p is string => typeof p === 'string' && p.length > 0)
      .map(p => path.basename(p)),
  );

  // Tier 1: prune NON-shelved archived sessions first (oldest by date prefix).
  const archiveDir = path.join(logsDir, 'archive');
  let archivedEntries = await getArchivedEntries(archiveDir);

  const isShelvedArchive = (e: LogEntry) =>
    shelvedBaseNames.has(path.basename(e.path));

  let nonShelved = archivedEntries.filter(e => !isShelvedArchive(e));
  while (result.currentSize > maxSizeBytes && nonShelved.length > 0) {
    const oldest = nonShelved[0];
    try {
      await rmDir(oldest.path);
      result.bytesFreed += oldest.size;
      result.filesRemoved++;
      result.removedFiles.push(oldest.name);
      result.currentSize -= oldest.size;
      logger.info(`Pruned archived session: ${oldest.name} (${oldest.size} bytes)`);
    } catch (err) {
      logger.warn(`Failed to prune ${oldest.name}:`, err);
    }
    nonShelved = nonShelved.slice(1);
  }

  // Shelved archived sessions are NEVER auto-pruned. They are explicit
  // user-saved state — silently deleting them is exactly the bug we just
  // fixed. If we're still over the limit after tier 1, surface a loud
  // warning so the user knows to manage their shelves manually via the
  // reattach picker's delete button.
  if (result.currentSize > maxSizeBytes) {
    const shelvedArchived = archivedEntries.filter(isShelvedArchive);
    const overBy = Math.round((result.currentSize - maxSizeBytes) / 1024 / 1024);
    if (shelvedArchived.length > 0) {
      logger.warn(
        `Logs still ${overBy}MB over limit after pruning ${result.filesRemoved} non-shelved archives. ` +
        `${shelvedArchived.length} shelved archive(s) preserved (won't auto-prune user-saved state). ` +
        `Use the reattach picker to delete shelved sessions you no longer need.`,
      );
    } else {
      logger.warn(
        `Logs still ${overBy}MB over ${maxSizeMb}MB limit — only active session dirs remain. ` +
        `Increase immorterm.maxLogSizeMb or close stale terminals.`,
      );
    }
  }

  // Tier 2: prune legacy flat .log files (oldest by mtime)
  if (result.currentSize > maxSizeBytes) {
    let legacyFiles = await getLegacyLogFiles(logsDir);
    while (result.currentSize > maxSizeBytes && legacyFiles.length > 0) {
      const oldest = legacyFiles[0];
      try {
        await fs.unlink(oldest.path);
        result.bytesFreed += oldest.size;
        result.filesRemoved++;
        result.removedFiles.push(oldest.name);
        result.currentSize -= oldest.size;
        logger.info(`Removed legacy log: ${oldest.name} (${oldest.size} bytes)`);
      } catch (err) {
        logger.warn(`Failed to remove ${oldest.name}:`, err);
      }
      legacyFiles = legacyFiles.slice(1);
    }
  }

  logger.info(
    `Log cleanup complete: ${result.filesRemoved} entries removed, ${result.bytesFreed} bytes freed. ` +
      `Current size: ${Math.round(result.currentSize / 1024 / 1024)}MB / ${maxSizeMb}MB`
  );

  return result;
}

export default cleanupLogs;
