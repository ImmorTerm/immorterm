import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';
import { clearAllTerminalsFromRegistry } from '../registry-client';

/**
 * Result of a forget-all operation
 */
export interface ForgetAllResult {
  /** Whether the operation was confirmed and executed */
  confirmed: boolean;
  /** Number of screen sessions killed */
  sessionsKilled: number;
  /** Number of log files deleted */
  logsDeleted: number;
  /** Number of storage entries cleared */
  entriesCleared: number;
}

/**
 * Forgets all terminals for the current project
 *
 * This is the TypeScript equivalent of the bash screen-forget-all script.
 * Removes ALL terminals: kills sessions, clears storage, deletes logs.
 *
 * Key differences from bash version:
 * - No jq usage - native JSON via WorkspaceStorage
 * - Uses screenCommands utility for session killing
 * - Prompts for confirmation via VS Code dialog
 *
 * WARNING: This is a destructive operation!
 *
 * @param storage WorkspaceStorage instance
 * @param logsDir Path to the logs directory
 * @param options Additional options
 * @returns ForgetAllResult with counts of items removed
 */
export async function forgetAllTerminals(
  storage: WorkspaceStorage,
  logsDir: string,
  options: {
    /** Skip confirmation prompt (for programmatic use) */
    skipConfirmation?: boolean;
    /** Show notification on completion */
    showNotification?: boolean;
  } = {}
): Promise<ForgetAllResult> {
  const { skipConfirmation = false, showNotification = true } = options;

  const result: ForgetAllResult = {
    confirmed: false,
    sessionsKilled: 0,
    logsDeleted: 0,
    entriesCleared: 0,
  };

  const projectName = storage.getProjectName();

  // Prompt for confirmation unless skipped
  if (!skipConfirmation) {
    const confirmation = await vscode.window.showWarningMessage(
      `This will kill ALL sessions and delete ALL logs for project "${projectName}". Continue?`,
      { modal: true },
      'Yes, Forget All'
    );

    if (confirmation !== 'Yes, Forget All') {
      logger.info('Forget all cancelled by user');
      return result;
    }
  }

  result.confirmed = true;
  logger.info(`Forgetting all terminals for project: ${projectName}`);

  // Get current terminal count before clearing
  result.entriesCleared = storage.getTerminalCount();

  // Clear workspace storage first (prevents VS Code from restoring)
  await storage.clearAllTerminals();
  logger.debug(`Cleared ${result.entriesCleared} entries from storage`);

  // Clear restore-terminals.json
  const jsonCleared = clearAllTerminalsFromRegistry();
  if (jsonCleared) {
    logger.debug('Cleared restore-terminals.json');
  }

  // Kill all screen sessions for this project
  result.sessionsKilled = await screenCommands.killProjectSessions(projectName);
  logger.debug(`Killed ${result.sessionsKilled} screen sessions`);

  // Delete all log files
  try {
    const logFiles = await fs.readdir(logsDir).catch(() => []);

    for (const logFile of logFiles) {
      if (!logFile.endsWith('.log')) {
        continue;
      }

      // Only delete logs for this project
      if (logFile.startsWith(`${projectName}-`)) {
        const logPath = path.join(logsDir, logFile);
        try {
          await fs.unlink(logPath);
          result.logsDeleted++;
        } catch (err) {
          logger.warn(`Failed to delete log file ${logFile}:`, err);
        }
      }
    }

    logger.debug(`Deleted ${result.logsDeleted} log files`);
  } catch (err) {
    logger.debug('Logs directory not accessible:', err);
  }

  // Delete all rename title files (renames dir is parallel to logs dir)
  try {
    const renamesDir = path.join(path.dirname(logsDir), 'renames');
    const renameFiles = await fs.readdir(renamesDir).catch(() => []);
    let renamesDeleted = 0;

    for (const renameFile of renameFiles) {
      // Only delete renames for this project
      if (renameFile.startsWith(`${projectName}-`)) {
        const renamePath = path.join(renamesDir, renameFile);
        try {
          await fs.unlink(renamePath);
          renamesDeleted++;
        } catch {
          // Ignore errors for individual files
        }
      }
    }

    if (renamesDeleted > 0) {
      logger.debug(`Deleted ${renamesDeleted} rename files`);
    }
  } catch {
    // Renames directory might not exist, that's okay
  }

  // Show notification
  if (showNotification) {
    vscode.window.showInformationMessage(
      `Forgot all terminals: ${result.sessionsKilled} sessions killed, ${result.logsDeleted} logs deleted. Close terminal tabs manually.`
    );
  }

  logger.info(
    `Forget all complete: ${result.sessionsKilled} sessions, ${result.entriesCleared} entries, ${result.logsDeleted} logs`
  );

  return result;
}

export default forgetAllTerminals;
