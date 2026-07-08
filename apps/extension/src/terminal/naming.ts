import * as vscode from 'vscode';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { buildSessionName } from '../utils/process';
import { logger } from '../utils/logger';

/**
 * Gets the configured screen binary path from settings
 */
function getScreenBinary(): string {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<string>('screenBinary', 'immorterm');
}

/**
 * Terminal Naming Module
 *
 * Handles terminal name generation, synchronization, and management.
 * VS Code terminal tab name is the SINGLE SOURCE OF TRUTH.
 *
 * Flow:
 * 1. User renames terminal tab in VS Code (right-click -> Rename)
 * 2. Extension detects name change via onDidChangeActiveTerminal or polling
 * 3. Extension syncs to both:
 *    - WorkspaceStorage (for persistence across restarts)
 *    - Screen title (for display in screen sessions)
 */

/**
 * Maps to track terminal names (for change detection)
 * Weak reference to avoid memory leaks when terminals are closed
 */
const terminalLastNames = new WeakMap<vscode.Terminal, string>();

/**
 * Pending names that have been generated but not yet added to storage.
 * This prevents race conditions when creating multiple terminals quickly.
 * Names are auto-cleared after 2 seconds (when storage should have caught up).
 */
const pendingNames = new Set<string>();

/**
 * Checks if a name appears to be a raw windowId format
 * Raw format: "12345-abcdef12" (PID-randomhex)
 *
 * @param name The name to check
 * @returns true if name matches windowId pattern
 */
export function isRawWindowIdFormat(name: string): boolean {
  return /^\d+-[a-f0-9]{8}$/.test(name);
}

/**
 * Gets the naming pattern from settings
 * Default: "immorterm-${n}"
 *
 * Note: windowId is now the unique identifier, not the terminal name.
 * Status bar shows "{project} / {tab-name}" so no need for project in default name.
 *
 * @returns The naming pattern string
 */
function getNamingPattern(): string {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<string>('namingPattern', 'immorterm-${n}');
}

/**
 * Generates the next sequential terminal name
 *
 * Scans existing terminal names to find the highest number,
 * then returns immorterm-N+1
 *
 * Note: windowId is now the unique identifier, not the terminal name.
 * Status bar shows "{project} / {tab-name}" so no need for project in default name.
 *
 * @param projectName The project name (lowercase) - kept for backward compatibility
 * @param storage The workspace storage to check existing names
 * @returns Next available name (e.g., "immorterm-3")
 */
export function generateNextName(
  projectName: string,
  storage: WorkspaceStorage
): string {
  const terminals = storage.getAllTerminals();
  let maxN = 0;

  // Pattern matches "immorterm-N" format (unified across all projects)
  const pattern = /^immorterm-(\d+)$/i;

  // Check storage for existing terminal names
  for (const terminal of terminals) {
    const match = terminal.name.match(pattern);
    if (match) {
      const n = parseInt(match[1], 10);
      if (n > maxN) {
        maxN = n;
      }
    }
  }

  // Check currently open terminals (they might not be in storage yet)
  for (const terminal of vscode.window.terminals) {
    const match = terminal.name.match(pattern);
    if (match) {
      const n = parseInt(match[1], 10);
      if (n > maxN) {
        maxN = n;
      }
    }
  }

  // Check pending names (recently generated, not yet in storage)
  // This prevents duplicates when creating terminals faster than storage updates
  for (const name of pendingNames) {
    const match = name.match(pattern);
    if (match) {
      const n = parseInt(match[1], 10);
      if (n > maxN) {
        maxN = n;
      }
    }
  }

  const nextN = maxN + 1;

  // Apply naming pattern (default: "immorterm-${n}")
  const namingPattern = getNamingPattern();
  const newName = namingPattern
    .replace('${project}', projectName) // Kept for custom patterns that still want project name
    .replace('${n}', String(nextN));

  // Add to pending names to prevent race conditions
  pendingNames.add(newName);

  // Auto-clear after storage catches up (~2 seconds)
  setTimeout(() => {
    pendingNames.delete(newName);
  }, 2000);

  return newName;
}

/**
 * Syncs a terminal's name to both storage and Screen title
 *
 * @param terminal The VS Code Terminal instance
 * @param windowId The terminal's window ID
 * @param storage The workspace storage
 * @param projectName The project name
 * @returns true if sync was successful
 */
export async function syncTerminalName(
  terminal: vscode.Terminal,
  windowId: string,
  storage: WorkspaceStorage,
  projectName: string
): Promise<boolean> {
  const name = terminal.name;

  logger.debug(`Syncing terminal name: "${name}" (window ${windowId})`);

  // Update storage
  const updated = await storage.updateTerminal(windowId, { name });

  if (!updated) {
    logger.debug(`Terminal not found in storage: ${windowId}`);
    return false;
  }

  // Update Screen title (no date prefix - clean name only)
  const sessionName = buildSessionName(projectName, windowId);
  const screenTitle = name;

  try {
    await screenCommands.sendCommand(sessionName, '');
    // Use screen -X title to set the window title
    const { exec } = require('child_process');
    const { promisify } = require('util');
    const execAsync = promisify(exec);
    const screenBinary = getScreenBinary();

    await execAsync(`${screenBinary} -S "${sessionName}" -X title "${screenTitle}"`);
    logger.debug(`Set screen title to "${screenTitle}"`);
  } catch (error) {
    logger.debug(`Could not set screen title for ${sessionName}:`, error);
    // Not a critical error - terminal still works without screen title
  }

  // Update last known name
  terminalLastNames.set(terminal, name);

  return true;
}

/**
 * Checks if a terminal's name has changed and syncs if needed
 *
 * @param terminal The VS Code Terminal to check
 * @param windowId The terminal's window ID
 * @param storage The workspace storage
 * @param projectName The project name
 * @returns true if name changed and was synced
 */
export async function checkAndSyncNameChange(
  terminal: vscode.Terminal,
  windowId: string,
  storage: WorkspaceStorage,
  projectName: string
): Promise<boolean> {
  const lastName = terminalLastNames.get(terminal);

  // If we haven't tracked this terminal yet, start tracking
  if (lastName === undefined) {
    terminalLastNames.set(terminal, terminal.name);
    return false;
  }

  // Check if name changed
  if (lastName !== terminal.name) {
    logger.info(`Terminal name changed: "${lastName}" -> "${terminal.name}"`);
    terminalLastNames.set(terminal, terminal.name);
    await syncTerminalName(terminal, windowId, storage, projectName);
    return true;
  }

  return false;
}

/**
 * Initializes name tracking for a terminal
 *
 * @param terminal The VS Code Terminal
 */
export function trackTerminalName(terminal: vscode.Terminal): void {
  terminalLastNames.set(terminal, terminal.name);
}

/**
 * Gets the last known name for a terminal
 *
 * @param terminal The VS Code Terminal
 * @returns The last known name, or undefined if not tracked
 */
export function getLastKnownName(terminal: vscode.Terminal): string | undefined {
  return terminalLastNames.get(terminal);
}

/**
 * Auto-renames a terminal from raw windowId format to a friendly name
 * Only renames if the current name looks like a raw windowId
 *
 * @param terminal The VS Code Terminal
 * @param windowId The terminal's window ID
 * @param storage The workspace storage
 * @param projectName The project name
 * @returns The new name if renamed, or null if not renamed
 */
export async function autoRenameIfNeeded(
  terminal: vscode.Terminal,
  windowId: string,
  storage: WorkspaceStorage,
  projectName: string
): Promise<string | null> {
  // Only auto-rename if the name is in raw windowId format
  if (!isRawWindowIdFormat(terminal.name)) {
    logger.debug(`Keeping user name "${terminal.name}" (not raw windowId format)`);
    return null;
  }

  const newName = generateNextName(projectName, storage);
  logger.info(`Auto-rename: "${terminal.name}" -> "${newName}"`);

  // Update storage with new name
  await storage.updateTerminal(windowId, { name: newName });

  // Update Screen title (no date prefix - clean name only)
  const sessionName = buildSessionName(projectName, windowId);
  const screenTitle = newName;

  try {
    const { exec } = require('child_process');
    const { promisify } = require('util');
    const execAsync = promisify(exec);
    const screenBinary = getScreenBinary();

    await execAsync(`${screenBinary} -S "${sessionName}" -X title "${screenTitle}"`);
  } catch {
    // Session might not exist yet
  }

  // Update tracking
  terminalLastNames.set(terminal, terminal.name);

  return newName;
}

export default {
  generateNextName,
  syncTerminalName,
  checkAndSyncNameChange,
  trackTerminalName,
  getLastKnownName,
  autoRenameIfNeeded,
  isRawWindowIdFormat,
};
