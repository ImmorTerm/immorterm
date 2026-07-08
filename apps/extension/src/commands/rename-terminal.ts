/**
 * Rename Terminal Command
 *
 * Renames the current terminal via VS Code input box.
 * Updates: screen title, VS Code tab, storage, and JSON.
 *
 * Keybinding: Ctrl+Shift+R (when terminal focused)
 */

import * as vscode from 'vscode';
import { exec } from 'child_process';
import { promisify } from 'util';
import { WorkspaceStorage } from '../storage/workspace-state';
import { updateRegistryNameAndCommand } from '../registry-client';
import { logger } from '../utils/logger';

const execAsync = promisify(exec);

/**
 * Gets the configured screen binary path from settings
 */
function getScreenBinary(): string {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<string>('screenBinary', 'immorterm');
}

export interface RenameTerminalResult {
  success: boolean;
  windowId?: string;
  oldName?: string;
  newName?: string;
  error?: string;
}

/**
 * Rename terminal via VS Code input box
 *
 * @param terminal The VS Code terminal to rename
 * @param windowId The terminal's window ID
 * @param storage The workspace storage instance
 * @param projectName The project name prefix for screen sessions
 * @returns Result of the rename operation
 */
export async function renameTerminal(
  terminal: vscode.Terminal,
  windowId: string,
  storage: WorkspaceStorage,
  projectName: string
): Promise<RenameTerminalResult> {
  const sessionName = `${projectName}-${windowId}`;
  const oldName = terminal.name;

  // Show input box with current name
  const newName = await vscode.window.showInputBox({
    prompt: 'Enter new terminal name',
    value: oldName,
    placeHolder: 'Terminal name',
    validateInput: (value) => {
      if (!value || value.trim() === '') {
        return 'Name cannot be empty';
      }
      return null;
    },
  });

  // User cancelled
  if (newName === undefined) {
    return {
      success: false,
      windowId,
      error: 'Cancelled',
    };
  }

  const trimmedName = newName.trim();

  // No change
  if (trimmedName === oldName) {
    return {
      success: true,
      windowId,
      oldName,
      newName: trimmedName,
    };
  }

  try {
    // 1. Update screen title (clean name only - no timestamp prefix)
    await execAsync(`${getScreenBinary()} -S "${sessionName}" -X title "${trimmedName}"`);
    logger.debug(`Set screen title to "${trimmedName}"`);

    // 2. Set pending rename via screen environment variable (cleaner than file-based IPC)
    // The shell's precmd hook will query this via `screen -Q echo` and update IMMORTERM_BASE_NAME
    await execAsync(`${getScreenBinary()} -S "${sessionName}" -X setenv IMMORTERM_PENDING_RENAME "${trimmedName}"`);
    logger.debug(`Set IMMORTERM_PENDING_RENAME="${trimmedName}" on session ${sessionName}`);

    // 3. Update storage
    await storage.updateTerminal(windowId, { name: trimmedName });

    // 4. Update JSON
    updateRegistryNameAndCommand(windowId, trimmedName);

    logger.info(`Renamed terminal: "${oldName}" -> "${trimmedName}"`);

    return {
      success: true,
      windowId,
      oldName,
      newName: trimmedName,
    };
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    logger.error(`Failed to rename terminal ${windowId}:`, error);
    return {
      success: false,
      windowId,
      oldName,
      error: errorMessage,
    };
  }
}

export default renameTerminal;
