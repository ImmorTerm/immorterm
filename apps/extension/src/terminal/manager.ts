import * as vscode from 'vscode';
import { WorkspaceStorage, TerminalState } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';
import { buildSessionName } from '../utils/process';
import { getCloseGracePeriod } from '../utils/settings';
import { removeTerminalFromRegistry, removeSessionStatus, updateSessionStatus, getCurrentClaudeSessionId } from '../registry-client';
// Gateway cleanup is handled by the gateway's own PID reaper (~60s) and idle timeout (30min).
// We no longer call cleanupGatewaySessionByPid() with cached PIDs — stale PIDs are dangerous.
import { getCloseAction } from '../utils/settings';
import { archiveSessionByWindowId } from '../commands/cleanup';
import { buildHibernationMessage } from '../hibernation-message';
import * as fs from 'fs';
import * as os from 'os';

/**
 * Pending cleanup entry for grace period handling
 */
interface PendingCleanup {
  windowId: string;
  timer: NodeJS.Timeout;
  scheduledAt: number;
}

/**
 * TerminalManager handles the lifecycle of ImmorTerm terminals
 * Bridges VS Code Terminal instances with WorkspaceStorage and Screen sessions
 */
export class TerminalManager {
  private context: vscode.ExtensionContext;
  private storage: WorkspaceStorage;

  /** Maps VS Code Terminal instances to their windowIds */
  private terminalToWindowId: Map<vscode.Terminal, string> = new Map();

  /** Maps windowIds to VS Code Terminal instances */
  private windowIdToTerminal: Map<string, vscode.Terminal> = new Map();

  /** Pending cleanup operations with grace period timers */
  private pendingCleanups: Map<string, PendingCleanup> = new Map();

  /** Disposables for event subscriptions */
  private disposables: vscode.Disposable[] = [];

  constructor(context: vscode.ExtensionContext, storage: WorkspaceStorage) {
    this.context = context;
    this.storage = storage;

    logger.debug('TerminalManager initialized for project:', storage.getProjectName());
  }

  /**
   * Registers a terminal with the manager and storage
   * @param terminal The VS Code Terminal instance
   * @param windowId The unique window identifier
   * @param name The display name for the terminal
   * @returns The created TerminalState
   */
  async registerTerminal(
    terminal: vscode.Terminal,
    windowId: string,
    name: string
  ): Promise<TerminalState> {
    const projectName = this.storage.getProjectName();
    const screenSession = buildSessionName(projectName, windowId);
    const now = Date.now();

    const terminalState: TerminalState = {
      windowId,
      name,
      screenSession,
      createdAt: now,
      lastAttached: now,
    };

    // Add to storage
    await this.storage.addTerminal(terminalState);

    // Track in memory maps
    this.terminalToWindowId.set(terminal, windowId);
    this.windowIdToTerminal.set(windowId, terminal);

    logger.info('Registered terminal:', windowId, name, screenSession);

    return terminalState;
  }

  /**
   * Unregisters a terminal, removing it from storage and optionally cleaning up Screen session
   * @param windowId The unique window identifier
   * @param cleanupScreen Whether to kill the Screen session (default: true)
   * @returns true if terminal was found and unregistered
   */
  async unregisterTerminal(windowId: string, cleanupScreen: boolean = true): Promise<boolean> {
    const terminalState = this.storage.getTerminal(windowId);

    if (!terminalState) {
      logger.debug('Terminal not found for unregister:', windowId);
      return false;
    }

    // Remove from memory maps
    const terminal = this.windowIdToTerminal.get(windowId);
    if (terminal) {
      this.terminalToWindowId.delete(terminal);
    }
    this.windowIdToTerminal.delete(windowId);

    // Remove from storage
    await this.storage.removeTerminal(windowId);

    // Cleanup Screen session if requested
    if (cleanupScreen && terminalState.screenSession) {
      try {
        const killed = await screenCommands.killSession(terminalState.screenSession);
        if (killed) {
          logger.info('Killed Screen session:', terminalState.screenSession);
        }
      } catch (error) {
        logger.warn('Failed to kill Screen session:', terminalState.screenSession, error);
      }
    }

    logger.info('Unregistered terminal:', windowId);
    return true;
  }

  /**
   * Gets a terminal state by windowId
   * @param windowId The unique window identifier
   * @returns The terminal state or undefined
   */
  getTerminalByWindowId(windowId: string): TerminalState | undefined {
    return this.storage.getTerminal(windowId);
  }

  /**
   * Gets all registered terminals from storage
   * @returns Array of all terminal states
   */
  getAllTerminals(): TerminalState[] {
    return this.storage.getAllTerminals();
  }

  /**
   * Gets the windowId for a VS Code Terminal instance
   * @param terminal The VS Code Terminal
   * @returns The windowId or undefined
   */
  getWindowIdForTerminal(terminal: vscode.Terminal): string | undefined {
    return this.terminalToWindowId.get(terminal);
  }

  /**
   * Gets the VS Code Terminal for a windowId
   * @param windowId The unique window identifier
   * @returns The VS Code Terminal or undefined
   */
  getTerminalForWindowId(windowId: string): vscode.Terminal | undefined {
    return this.windowIdToTerminal.get(windowId);
  }

  /**
   * Tracks an existing VS Code Terminal with a windowId
   * Used during restoration when terminals are created externally
   * @param terminal The VS Code Terminal instance
   * @param windowId The unique window identifier
   */
  trackTerminal(terminal: vscode.Terminal, windowId: string): void {
    this.terminalToWindowId.set(terminal, windowId);
    this.windowIdToTerminal.set(windowId, terminal);
    logger.debug('Tracking terminal:', windowId);
  }

  /**
   * Untracks a VS Code Terminal from memory maps (does not affect storage)
   * Used when terminal closes but we want to preserve the storage entry
   * @param terminal The VS Code Terminal instance
   */
  untrackTerminal(terminal: vscode.Terminal): string | undefined {
    const windowId = this.terminalToWindowId.get(terminal);
    if (windowId) {
      this.terminalToWindowId.delete(terminal);
      this.windowIdToTerminal.delete(windowId);
      logger.debug('Untracked terminal:', windowId);
    }
    return windowId;
  }

  /**
   * Updates the name of a terminal in storage
   * @param windowId The unique window identifier
   * @param newName The new display name
   */
  async updateTerminalName(windowId: string, newName: string): Promise<boolean> {
    // Get the terminal state to find the screen session
    const terminalState = this.storage.getTerminal(windowId);

    // Update screen title if we have a session
    if (terminalState?.screenSession) {
      try {
        await screenCommands.setWindowTitle(terminalState.screenSession, newName);
        logger.debug('Updated screen title:', terminalState.screenSession, '->', newName);
      } catch (error) {
        logger.warn('Failed to update screen title:', error);
      }
    }

    return this.storage.updateTerminal(windowId, { name: newName });
  }

  /**
   * Updates the Claude session ID for a terminal
   * @param windowId The unique window identifier
   * @param claudeSessionId The Claude session ID
   */
  async updateClaudeSessionId(windowId: string, claudeSessionId: string): Promise<boolean> {
    return this.storage.updateTerminal(windowId, { claudeSessionId });
  }

  /**
   * Gets the terminal count from storage
   */
  getTerminalCount(): number {
    return this.storage.getTerminalCount();
  }

  /**
   * Gets the number of terminals tracked in the in-memory map
   * (This differs from getTerminalCount which reads from storage)
   */
  getTrackedCount(): number {
    return this.terminalToWindowId.size;
  }

  /**
   * Gets all tracked windowIds from the in-memory map (for diagnostics)
   */
  getTrackedWindowIds(): string[] {
    return Array.from(this.windowIdToTerminal.keys());
  }

  /**
   * Gets the project name
   */
  getProjectName(): string {
    return this.storage.getProjectName();
  }

  /**
   * Gets the storage instance
   */
  getStorage(): WorkspaceStorage {
    return this.storage;
  }

  /**
   * Checks if a Screen session exists for a terminal
   * @param windowId The unique window identifier
   */
  async hasActiveScreenSession(windowId: string): Promise<boolean> {
    const terminalState = this.storage.getTerminal(windowId);
    if (!terminalState) {
      return false;
    }
    return screenCommands.sessionExists(terminalState.screenSession);
  }

  /**
   * Syncs in-memory tracking with storage
   * Re-populates maps from storage (useful after restoration)
   */
  syncFromStorage(): void {
    const terminals = this.storage.getAllTerminals();
    logger.debug('Syncing from storage:', terminals.length, 'terminals');

    // Note: We cannot repopulate terminalToWindowId from storage
    // because VS Code Terminal instances are not persisted
    // This is called after restoration when terminals are recreated
  }

  /**
   * Schedules a terminal cleanup after grace period
   *
   * When a terminal closes, we don't immediately clean up the Screen session.
   * Instead, we wait for the grace period (immorterm.closeGracePeriod) in case
   * VS Code is reloading and the terminal will be restored.
   *
   * @param windowId The window ID to schedule cleanup for
   * @param logsDir Path to the logs directory (for log cleanup)
   * @returns true if cleanup was scheduled, false if already pending
   */
  scheduleCleanup(windowId: string, logsDir: string): boolean {
    // Check if cleanup is already pending
    if (this.pendingCleanups.has(windowId)) {
      logger.debug('Cleanup already pending for:', windowId);
      return false;
    }

    const gracePeriod = getCloseGracePeriod();
    logger.debug(`Scheduling cleanup for ${windowId} in ${gracePeriod}ms`);

    const timer = setTimeout(async () => {
      await this.executeCleanup(windowId, logsDir);
    }, gracePeriod);

    this.pendingCleanups.set(windowId, {
      windowId,
      timer,
      scheduledAt: Date.now(),
    });

    return true;
  }

  /**
   * Cancels a pending terminal cleanup
   *
   * Called when a terminal is restored before the grace period expires,
   * preventing accidental Screen session termination during VS Code reload.
   *
   * @param windowId The window ID to cancel cleanup for
   * @returns true if cleanup was cancelled, false if no cleanup was pending
   */
  cancelCleanup(windowId: string): boolean {
    const pending = this.pendingCleanups.get(windowId);
    if (!pending) {
      return false;
    }

    clearTimeout(pending.timer);
    this.pendingCleanups.delete(windowId);

    logger.debug(`Cancelled pending cleanup for ${windowId}`);
    return true;
  }

  /**
   * Executes the cleanup for a terminal after grace period expires
   *
   * @param windowId The window ID to cleanup
   * @param logsDir Path to the logs directory
   */
  private async executeCleanup(windowId: string, logsDir: string): Promise<void> {
    this.pendingCleanups.delete(windowId);

    const terminalState = this.storage.getTerminal(windowId);
    if (!terminalState) {
      logger.debug('Terminal already removed during grace period:', windowId);
      return;
    }

    // Check if terminal was restored during grace period (e.g., VS Code reload
    // completed and restoreTerminals() already re-created this terminal)
    if (this.windowIdToTerminal.has(windowId)) {
      logger.debug('Terminal restored during grace period, skipping cleanup:', windowId);
      return;
    }

    // HARD RULE: auto-cleanup paths (VS Code shutdown, extension reload,
    // panel-tab close) MUST NOT silently shelve or kill the user's
    // session. Sessions are user property — they stay in the active
    // registry forever until the user explicitly chooses to shelve
    // (sidebar × / "Shelve active" / hub /registry/shelve) or kill
    // ("Forget Terminal" command / hub /registry/close).
    //
    // VS Code's onDidCloseTerminal can't distinguish "user closed the
    // tab" from "VS Code is quitting" from "extension reloaded", so
    // ALL auto triggers are treated as non-destructive. The session
    // daemon stays alive in the background and gets restored on the
    // next activation. `closeAction` is now respected ONLY by
    // explicit user-initiated paths (the popup "Shelve active"
    // command, the hub /registry/shelve endpoint, the "Forget
    // Terminal" command for kill).
    //
    // History: prior behavior auto-shelved on grace expiry per the
    // `closeAction='shelve'` default. That bulk-shelved 83 sessions
    // for one user when they closed VS Code — the "huge issue" that
    // drove this fix. logsDir kept in the signature for callers; the
    // intentional unused-var avoids a wider refactor.
    void logsDir;
    void terminalState;
    logger.info(`Grace period expired for ${windowId} — NOT auto-shelving (preserving session in registry)`);
  }

  /**
   * Gracefully exits Claude before shelving a terminal.
   *
   * Flow: Ctrl+C (cancel in-progress work) → /exit → wait → retry once if needed.
   * Then displays the hibernation ANSI art message with resume instructions.
   *
   * IMPORTANT: Never kill by PID directly. Cached PIDs may be stale and could
   * target a Claude process in a different terminal. All commands go through the
   * screen session, which always targets the correct terminal. If Claude doesn't
   * exit after two attempts, we leave it — an orphaned process (~270MB) is far
   * less harmful than killing an active session in another terminal.
   */
  private async gracefulClaudeExit(
    terminalState: TerminalState,
    windowId: string,
    claudeSessionId: string | null | undefined,
  ): Promise<void> {
    const pid = terminalState.claudeStats!.pid;
    const session = terminalState.screenSession!;

    // Verify Claude is actually still alive (stats may be stale)
    if (!isProcessAlive(pid)) {
      logger.debug(`Claude PID ${pid} already dead, skipping graceful exit`);
      return;
    }

    logger.info(`Gracefully exiting Claude (PID ${pid}) before shelving ${windowId}`);

    try {
      // Attempt 1: Interrupt any in-progress work (Ctrl+C), then send /exit
      await screenCommands.sendCommand(session, '\x03');
      await delay(500);
      await screenCommands.sendCommand(session, '/exit\n');

      // Wait up to 3s for Claude to exit
      let exited = await waitForProcessExit(pid, 3000);

      if (!exited) {
        // Attempt 2: Try Ctrl+C → /exit once more (Claude may have been mid-tool-call)
        logger.warn(`Claude PID ${pid} did not exit on first attempt, retrying via screen session`);
        await screenCommands.sendCommand(session, '\x03');
        await delay(500);
        await screenCommands.sendCommand(session, '/exit\n');
        exited = await waitForProcessExit(pid, 3000);
      }

      if (!exited) {
        // Don't escalate to process.kill() — the cached PID may be stale and could
        // kill a Claude process in a different terminal. An orphan is acceptable.
        logger.warn(`Claude PID ${pid} did not exit after 2 attempts for ${windowId}, leaving as orphan`);
      } else {
        logger.info(`Claude exited for ${windowId} (graceful: true)`);
      }
    } catch (error) {
      logger.warn(`Error during graceful Claude exit for ${windowId}:`, error);
      // Do NOT fall back to process.kill — stale PIDs are dangerous
    }

    // Display hibernation ANSI art in the terminal
    if (claudeSessionId) {
      try {
        const rssKB = terminalState.claudeStats?.rss || 0;
        const message = buildHibernationMessage(
          terminalState.theme || 'Purple Haze',
          {
            rssMB: Math.round(rssKB / 1024),
            idleHours: 0,
            idleMinutes: 0,
            lastActivity: new Date().toISOString(),
            sessionUuid: claudeSessionId,
            terminalName: terminalState.name,
          },
        );

        // Write to temp file and cat it in the screen session
        // (ANSI escape codes can't be reliably passed via screen -X stuff directly)
        const tmpFile = `${os.tmpdir()}/immorterm-hibernate-${windowId}`;
        fs.writeFileSync(tmpFile, message);
        await screenCommands.sendCommand(session, `cat ${tmpFile}\n`);
        await delay(300); // Let cat complete before detach
        logger.debug('Displayed hibernation message for', windowId);
      } catch (error) {
        logger.warn('Failed to display hibernation message:', error);
      }
    }
  }

  /**
   * Checks if a cleanup is pending for a window ID
   * @param windowId The window ID to check
   */
  isCleanupPending(windowId: string): boolean {
    return this.pendingCleanups.has(windowId);
  }

  /**
   * Gets the count of pending cleanups
   */
  getPendingCleanupCount(): number {
    return this.pendingCleanups.size;
  }

  /**
   * Cancels all pending cleanups
   * Called during deactivation to prevent orphaned timers
   */
  cancelAllPendingCleanups(): void {
    for (const pending of this.pendingCleanups.values()) {
      clearTimeout(pending.timer);
    }
    this.pendingCleanups.clear();
    logger.debug('Cancelled all pending cleanups');
  }

  /**
   * Disposes of the manager and cleans up subscriptions
   */
  async dispose(): Promise<void> {
    // Cancel all pending cleanup timers
    this.cancelAllPendingCleanups();

    for (const disposable of this.disposables) {
      disposable.dispose();
    }
    this.disposables = [];

    this.terminalToWindowId.clear();
    this.windowIdToTerminal.clear();

    await this.storage.flush();

    logger.debug('TerminalManager disposed');
  }
}

export default TerminalManager;

// ── Helpers for graceful Claude exit ────────────────────────────

function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitForProcessExit(pid: number, timeoutMs: number): Promise<boolean> {
  const interval = 200;
  const maxAttempts = Math.ceil(timeoutMs / interval);
  for (let i = 0; i < maxAttempts; i++) {
    if (!isProcessAlive(pid)) return true;
    await delay(interval);
  }
  return !isProcessAlive(pid);
}
