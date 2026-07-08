import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';
import { removeTerminalFromRegistry, removeSessionStatus } from '../registry-client';

/**
 * Result of a forget operation
 */
export interface ForgetResult {
  /** Whether the terminal was successfully forgotten */
  success: boolean;
  /** The window ID that was forgotten */
  windowId: string;
  /** Whether the screen session was killed */
  sessionKilled: boolean;
  /** Whether the storage entry was removed */
  storageRemoved: boolean;
  /** Whether the log file was deleted */
  logDeleted: boolean;
}

/**
 * Forgets a single terminal - removes it from storage, kills the session, and deletes logs
 *
 * This is the TypeScript equivalent of the bash screen-forget script.
 * Removes a terminal completely from the system.
 *
 * Key differences from bash version:
 * - No jq usage - native JSON via WorkspaceStorage
 * - Uses screenCommands utility for session killing
 * - Can optionally close the VS Code terminal tab
 *
 * @param windowId Unique terminal identifier to forget
 * @param storage WorkspaceStorage instance
 * @param logsDir Path to the logs directory
 * @param options Additional options for the forget operation
 * @returns ForgetResult with status of each operation
 */
export async function forgetTerminal(
  windowId: string,
  storage: WorkspaceStorage,
  logsDir: string,
  options: {
    /** Show notification on completion */
    showNotification?: boolean;
    /** Close VS Code terminal tab if still open */
    closeTerminalTab?: boolean;
  } = {}
): Promise<ForgetResult> {
  const { showNotification = true, closeTerminalTab = true } = options;

  const result: ForgetResult = {
    success: false,
    windowId,
    sessionKilled: false,
    storageRemoved: false,
    logDeleted: false,
  };

  logger.info(`Forgetting terminal: ${windowId}`);

  // Get terminal info from storage
  const terminal = storage.getTerminal(windowId);
  const projectName = storage.getProjectName();
  const screenSession = terminal?.screenSession ?? `${projectName}-${windowId}`;

  // Kill the screen session
  result.sessionKilled = await screenCommands.killSession(screenSession);
  if (result.sessionKilled) {
    logger.debug(`Killed screen session: ${screenSession}`);
  } else {
    logger.debug(`Screen session not found or already dead: ${screenSession}`);
  }

  // Remove from workspace storage
  result.storageRemoved = await storage.removeTerminal(windowId);
  if (result.storageRemoved) {
    logger.debug(`Removed from storage: ${windowId}`);
  }

  // Remove from restore-terminals.json
  const jsonRemoved = removeTerminalFromRegistry(windowId);
  if (jsonRemoved) {
    logger.debug(`Removed from restore-terminals.json: ${windowId}`);
  }
  // Permanent delete also drops session-status (sidebar order, etc.)
  removeSessionStatus(windowId);

  // Delete the log file
  const logPath = path.join(logsDir, `${screenSession}.log`);
  try {
    await fs.unlink(logPath);
    result.logDeleted = true;
    logger.debug(`Deleted log file: ${logPath}`);
  } catch (err) {
    // Log file might not exist, that's okay
    logger.debug(`Log file not found or couldn't delete: ${logPath}`);
  }

  // Close VS Code terminal tab if requested
  if (closeTerminalTab && terminal) {
    const vsTerminals = vscode.window.terminals;
    for (const vsTerminal of vsTerminals) {
      // Match by name
      if (vsTerminal.name === terminal.name) {
        vsTerminal.dispose();
        logger.debug(`Closed VS Code terminal tab: ${terminal.name}`);
        break;
      }
    }
  }

  // Mark as successful if at least one cleanup action worked
  result.success = result.sessionKilled || result.storageRemoved || result.logDeleted;

  // Show notification
  if (showNotification) {
    if (result.success) {
      vscode.window.showInformationMessage(`Terminal forgotten: ${terminal?.name ?? windowId}`);
    } else {
      vscode.window.showWarningMessage(`Terminal not found: ${windowId}`);
    }
  }

  logger.info(
    `Forget complete for ${windowId}: session=${result.sessionKilled}, storage=${result.storageRemoved}, log=${result.logDeleted}`
  );

  return result;
}

export default forgetTerminal;
