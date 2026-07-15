import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';
import { clearAllTerminalsFromRegistry } from '../registry-client';

/**
 * Result of a kill-all operation
 */
export interface KillAllResult {
  /** Whether the operation was confirmed and executed */
  confirmed: boolean;
  /** Number of screen sessions killed */
  sessionsKilled: number;
  /** Number of log files deleted */
  logsDeleted: number;
  /** Number of storage entries cleared */
  entriesCleared: number;
  /** Names of sessions that were killed */
  killedSessions: string[];
}

/**
 * Kills all screen sessions for the current project
 *
 * This is the TypeScript equivalent of the bash kill-screens script.
 * Similar to forgetAllTerminals but specifically focused on killing sessions.
 *
 * Key differences from bash version:
 * - No jq usage - native JSON via WorkspaceStorage
 * - Uses screenCommands utility for session killing
 * - Prompts for confirmation via VS Code dialog
 *
 * @param storage WorkspaceStorage instance
 * @param logsDir Path to the logs directory
 * @param options Additional options
 * @returns KillAllResult with counts of items affected
 */
export async function killAllScreenSessions(
  storage: WorkspaceStorage,
  logsDir: string,
  options: {
    /** Skip confirmation prompt (for programmatic use) */
    skipConfirmation?: boolean;
    /** Show notification on completion */
    showNotification?: boolean;
  } = {}
): Promise<KillAllResult> {
  const { skipConfirmation = false, showNotification = true } = options;

  const result: KillAllResult = {
    confirmed: false,
    sessionsKilled: 0,
    logsDeleted: 0,
    entriesCleared: 0,
    killedSessions: [],
  };

  const projectName = storage.getProjectName();

  // Prompt for confirmation unless skipped
  if (!skipConfirmation) {
    const confirmation = await vscode.window.showWarningMessage(
      `This will kill ALL sessions for project "${projectName}". Continue?`,
      { modal: true },
      'Yes, Kill All'
    );

    if (confirmation !== 'Yes, Kill All') {
      logger.info('Kill all cancelled by user');
      return result;
    }
  }

  result.confirmed = true;
  logger.info(`Killing all screen sessions for project: ${projectName}`);

  // Get list of sessions to kill (for reporting)
  const sessions = await screenCommands.listProjectSessions(projectName);
  result.killedSessions = sessions.map(s => s.name);

  // Kill all screen sessions
  result.sessionsKilled = await screenCommands.killProjectSessions(projectName);
  logger.debug(`Killed ${result.sessionsKilled} screen sessions`);

  // Get current terminal count before clearing
  result.entriesCleared = storage.getTerminalCount();

  // Clear workspace storage
  await storage.clearAllTerminals();
  logger.debug(`Cleared ${result.entriesCleared} entries from storage`);

  // Clear restore-terminals.json
  const jsonCleared = clearAllTerminalsFromRegistry();
  if (jsonCleared) {
    logger.debug('Cleared restore-terminals.json');
  } else {
    logger.warn('Failed to clear restore-terminals.json - check logs for details');
  }

  // Delete all log files for this project
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

  // Show notification
  if (showNotification) {
    vscode.window.showInformationMessage(
      `Killed ${result.sessionsKilled} sessions, cleared ${result.entriesCleared} entries, deleted ${result.logsDeleted} logs.`
    );
  }

  logger.info(
    `Kill all complete: ${result.sessionsKilled} sessions, ${result.entriesCleared} entries, ${result.logsDeleted} logs`
  );

  return result;
}

export default killAllScreenSessions;
