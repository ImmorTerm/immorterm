/**
 * Toggle Title Lock Command
 *
 * Locks or unlocks the active terminal's title.
 * When locked, OSC sequences (from Claude Code etc.) cannot rename the tab.
 * When unlocked, Claude/OSC can rename it freely.
 *
 * Updates: storage, registry JSON, screen env.
 *
 * Keybinding: Ctrl+Shift+L (when terminal focused)
 */

import * as vscode from 'vscode';
import { WorkspaceStorage } from '../storage/workspace-state';
import { updateRegistryTitleLocked } from '../registry-client';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';

export interface ToggleTitleLockResult {
  success: boolean;
  locked?: boolean;
  name?: string;
  error?: string;
}

/**
 * Toggle the title lock on a terminal.
 *
 * @param windowId The terminal's window ID
 * @param storage The workspace storage instance
 * @param projectName The project name prefix for screen sessions
 * @param terminal The VS Code terminal (used to read current name)
 * @returns Result of the toggle operation
 */
export async function toggleTitleLock(
  windowId: string,
  storage: WorkspaceStorage,
  projectName: string,
  terminal: vscode.Terminal
): Promise<ToggleTitleLockResult> {
  const sessionName = `${projectName}-${windowId}`;
  const name = terminal.name;

  try {
    // Read current lock state from storage
    const termData = storage.getTerminal(windowId);
    const wasLocked = termData?.titleLocked ?? false;
    const nowLocked = !wasLocked;

    // 1. Update storage
    await storage.updateTerminal(windowId, { titleLocked: nowLocked });

    // 2. Update registry JSON
    updateRegistryTitleLocked(windowId, nowLocked);

    // 3. Update screen environment variable so the shell knows
    await screenCommands.setEnv(sessionName, 'IMMORTERM_TITLE_LOCKED', nowLocked ? '1' : '0');

    logger.info(`Title ${nowLocked ? 'locked' : 'unlocked'} for terminal ${windowId} ("${name}")`);

    return { success: true, locked: nowLocked, name };
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    logger.error(`Failed to toggle title lock for ${windowId}:`, error);
    return { success: false, error: errorMessage };
  }
}
