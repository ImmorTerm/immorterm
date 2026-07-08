import { WorkspaceStorage, TerminalState } from '../storage/workspace-state';
import { logger } from '../utils/logger';
import { buildSessionName } from '../utils/process';
import { addTerminalToRegistry } from '../registry-client';

/**
 * Result of a reconcile operation
 */
export interface ReconcileResult {
  /** Whether the terminal was added (true) or already existed (false) */
  added: boolean;
  /** The window ID that was reconciled */
  windowId: string;
  /** The display name that was used */
  displayName: string;
}

/**
 * Reconciles a terminal by registering it in workspace storage
 *
 * This is the TypeScript equivalent of the bash screen-reconcile script.
 * Called when a new terminal is created to register it for persistence.
 *
 * Key differences from bash version:
 * - No file locking needed (workspaceState handles it atomically)
 * - No jq usage - native JSON via WorkspaceStorage
 * - No pending file queue - direct registration
 *
 * @param windowId Unique terminal identifier (format: "pid-random8")
 * @param displayName Display name for the terminal tab
 * @param storage WorkspaceStorage instance for persistence
 * @returns ReconcileResult indicating success and whether terminal was new
 */
export async function reconcileTerminal(
  windowId: string,
  displayName: string,
  storage: WorkspaceStorage
): Promise<ReconcileResult> {
  logger.debug(`Reconciling terminal: ${windowId} (${displayName})`);

  // Check for duplicate windowId before adding
  const existingTerminal = storage.getTerminal(windowId);
  if (existingTerminal) {
    logger.debug(`Terminal ${windowId} already exists in storage, skipping`);
    return {
      added: false,
      windowId,
      displayName: existingTerminal.name,
    };
  }

  // Build the screen session name
  const projectName = storage.getProjectName();
  const screenSession = buildSessionName(projectName, windowId);

  // Create terminal state
  const terminalState: TerminalState = {
    windowId,
    name: displayName,
    screenSession,
    createdAt: Date.now(),
    lastAttached: Date.now(),
  };

  // Add to storage (VS Code workspaceState)
  await storage.addTerminal(terminalState);

  // Also add to restore-terminals.json for terminal restoration
  addTerminalToRegistry(windowId, displayName);

  logger.info(`Reconciled terminal: ${windowId} -> ${displayName} (session: ${screenSession})`);

  return {
    added: true,
    windowId,
    displayName,
  };
}

export default reconcileTerminal;
