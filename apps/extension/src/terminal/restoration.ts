import * as vscode from 'vscode';
import * as path from 'path';
import * as fsSync from 'fs';
import { exec } from 'child_process';
import { promisify } from 'util';
import { TerminalManager } from './manager';
import { TerminalState } from '../storage/workspace-state';
import { createTerminalWithScreen, isScreenAvailable } from './screen-integration';
import { logger } from '../utils/logger';
import { shouldCloseExistingOnRestore } from '../utils/settings';
import { getAllTerminalsFromRegistry, getActiveTerminal } from '../registry-client';
import { getTheme, generateHardstatus } from '../themes';
import { markAsRestored } from '../extension';
import { screenCommands } from '../utils/screen-commands';

const execAsync = promisify(exec);

// Diagnostic logging — writes to .immorterm/diagnostic.log
function diagLog(msg: string): void {
  try {
    const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (ws) {
      const logPath = path.join(ws, '.immorterm', 'diagnostic.log');
      fsSync.appendFileSync(logPath, `[${new Date().toISOString()}] ${msg}\n`);
    }
  } catch { /* ignore */ }
}

/**
 * Configuration options for terminal restoration
 */
interface RestorationOptions {
  /** Path to the scripts directory (~/.immorterm/scripts/) */
  scriptsPath: string;
  /** Path to the per-project terminals directory (<project>/.immorterm/terminals/) */
  terminalsDir?: string;
  /** Delay in milliseconds between terminal restorations (default from settings) */
  restoreDelay?: number;
}

/**
 * Result of terminal restoration operation
 */
export interface RestorationResult {
  /** Number of terminals successfully restored */
  restored: number;
  /** Number of terminals that failed to restore */
  failed: number;
  /** Number of terminals skipped (e.g., session already attached) */
  skipped: number;
  /** Details for each terminal restoration attempt */
  details: RestorationDetail[];
}

/**
 * Detail for a single terminal restoration attempt
 */
interface RestorationDetail {
  windowId: string;
  name: string;
  status: 'restored' | 'failed' | 'skipped';
  reason?: string;
}

/**
 * Gets the terminal restore delay from settings
 * @returns Delay in milliseconds
 */
function getRestoreDelay(): number {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<number>('terminalRestoreDelay', 200);
}

/**
 * Checks if restore on startup is enabled
 * @returns true if terminals should restore on startup
 */
export function isRestoreOnStartupEnabled(): boolean {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<boolean>('restoreOnStartup', true);
}

/**
 * Delays execution for a specified time
 * @param ms Milliseconds to wait
 */
function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/**
 * Restores all terminals from WorkspaceStorage on startup
 *
 * For each terminal in storage:
 * 1. Checks if the Screen session still exists
 * 2. If exists: attaches to it with the stored name
 * 3. If not exists: creates a new Screen session
 * 4. Tracks the terminal in TerminalManager
 *
 * @param manager The TerminalManager instance
 * @param options Restoration options including scriptsPath
 * @returns RestorationResult with counts and details
 */
export async function restoreTerminals(
  manager: TerminalManager,
  options: RestorationOptions
): Promise<RestorationResult> {
  const result: RestorationResult = {
    restored: 0,
    failed: 0,
    skipped: 0,
    details: [],
  };

  // Check if restoration is enabled
  if (!isRestoreOnStartupEnabled()) {
    logger.info('Terminal restoration disabled by settings');
    return result;
  }

  // Close existing terminals if setting is enabled
  if (shouldCloseExistingOnRestore()) {
    const existingTerminals = vscode.window.terminals;
    diagLog(`CLOSE_EXISTING: enabled=true found=${existingTerminals.length} names=[${existingTerminals.map(t => `"${t.name}"`).join(', ')}]`);
    if (existingTerminals.length > 0) {
      logger.info(`Closing ${existingTerminals.length} existing terminals before restoration`);
      for (const terminal of existingTerminals) {
        terminal.dispose();
      }
      // Brief delay to allow terminals to close
      await delay(100);
    }
  } else {
    diagLog(`CLOSE_EXISTING: enabled=false`);
  }

  // Check if Screen is available (graceful degradation)
  if (!isScreenAvailable()) {
    logger.warn('Screen not available - terminal restoration skipped (no persistence)');
    return result;
  }

  // Capture BEFORE restoring — onDidChangeActiveTerminal will overwrite it during restore
  const activeWindowId = getActiveTerminal('regular');

  const projectName = manager.getProjectName();

  // JSON is the source of truth for terminal restoration
  // WorkspaceStorage is just a runtime cache that can be cleared by VS Code
  const jsonTerminals = getAllTerminalsFromRegistry();

  if (jsonTerminals.length === 0) {
    logger.debug('No terminals in JSON to restore');
    return result;
  }

  // Convert JSON entries to TerminalState format
  const terminals: TerminalState[] = jsonTerminals.map((t) => ({
    windowId: t.windowId,
    name: t.name,
    screenSession: `${projectName}-${t.windowId}`,
    createdAt: Date.now(),
    lastAttached: Date.now(),
    claudeSessionId: t.claudeSessionId,
    theme: t.theme,
    titleLocked: t.titleLocked,
  }));

  logger.info(`Found ${terminals.length} terminals in JSON to restore`);

  // SEQUENTIAL RESTORATION: Create terminals one at a time to preserve tab order.
  // VS Code's tab position is determined by when createTerminal() completes,
  // so parallel creation can shuffle the order. Sequential guarantees the JSON order.
  const INTER_TERMINAL_DELAY = 50; // ms between terminal creations

  logger.info(`Restoring ${terminals.length} terminals sequentially...`);

  const details: RestorationDetail[] = [];
  for (let i = 0; i < terminals.length; i++) {
    const terminalState = terminals[i];

    // Small delay between terminals to let VS Code settle
    if (i > 0) {
      await delay(INTER_TERMINAL_DELAY);
    }

    try {
      const detail = await restoreSingleTerminal(
        manager,
        terminalState,
        options.scriptsPath,
        projectName,
        options.terminalsDir
      );
      details.push(detail);
    } catch (error) {
      logger.error(`Failed to restore terminal ${terminalState.windowId}:`, error);
      details.push({
        windowId: terminalState.windowId,
        name: terminalState.name,
        status: 'failed' as const,
        reason: error instanceof Error ? error.message : String(error),
      });
    }
  }

  // Aggregate results
  for (const detail of details) {
    result.details.push(detail);
    switch (detail.status) {
      case 'restored':
        result.restored++;
        break;
      case 'failed':
        result.failed++;
        break;
      case 'skipped':
        result.skipped++;
        break;
    }
  }

  logger.info(
    `Terminal restoration complete: ${result.restored} restored, ` +
    `${result.failed} failed, ${result.skipped} skipped`
  );

  // Show the terminal panel if any terminals were restored
  if (result.restored > 0) {
    // Delay to ensure VS Code has registered the terminals
    await delay(200);

    const allTerminals = vscode.window.terminals;
    if (allTerminals.length > 0) {
      // Try to focus the last-active regular terminal (captured before restore started)
      let focused = false;
      if (activeWindowId) {
        const restoredDetail = details.find(d => d.windowId === activeWindowId && d.status === 'restored');
        if (restoredDetail) {
          const match = allTerminals.find(t => t.name === restoredDetail.name);
          if (match) {
            match.show(false);
            focused = true;
            logger.info(`Focused last-active terminal '${restoredDetail.name}' (${activeWindowId})`);
          }
        }
      }
      if (!focused) {
        // Fallback: show the last terminal (most recently created)
        allTerminals[allTerminals.length - 1].show(false);
        logger.info('Focused last terminal (no active match)');
      }
    }
  }

  return result;
}

/**
 * Restores a single terminal
 *
 * @param manager The TerminalManager instance
 * @param terminalState The stored terminal state
 * @param scriptsPath Path to the scripts directory
 * @param projectName The project name for session naming
 * @returns RestorationDetail for this terminal
 */
async function restoreSingleTerminal(
  manager: TerminalManager,
  terminalState: TerminalState,
  scriptsPath: string,
  projectName: string,
  terminalsDir?: string
): Promise<RestorationDetail> {
  const { windowId, name, screenSession, claudeSessionId } = terminalState;

  logger.debug(`Restoring terminal: ${windowId} "${name}"`, claudeSessionId ? `(Claude: ${claudeSessionId})` : '');

  // NOTE: Session existence and detach checks removed for performance.
  // The screen-auto script handles session management automatically:
  // - Uses -D -RR flags to forcefully detach stale attachments
  // - Creates new sessions if none exist
  // - Handles dead/remote session cleanup
  // This saves ~100-200ms per terminal by avoiding redundant screen -ls calls.

  // titleLocked is the source of truth from registry/storage.
  // Defaults to false (unlocked) — new terminals and Claude-active terminals accept OSC renames.
  const locked = terminalState.titleLocked ?? false;

  // Create VS Code terminal that connects to the Screen session
  // The screen-auto script handles both creating new sessions and attaching to existing ones
  const terminal = createTerminalWithScreen({
    name,
    windowId,
    scriptsPath,
    terminalsDir,
    cwd: vscode.workspace.workspaceFolders?.[0]?.uri.fsPath,
    isRestoration: true,
    claudeSessionId,
    titleLocked: locked,
  });

  // Cancel any pending cleanup (in case terminal is being restored during grace period)
  if (manager.cancelCleanup(windowId)) {
    logger.debug('Cancelled pending cleanup during restoration:', windowId);
  }

  // Track the terminal in the manager
  manager.trackTerminal(terminal, windowId);
  diagLog(`RESTORE_TRACK: windowId=${windowId} name="${name}" locked=${locked}`);

  // For unlocked terminals: use grace period to avoid the initial OSC title
  // being misinterpreted as a user rename by onDidChangeTerminalState.
  // For locked terminals: VS Code's name property already protects from OSC override.
  if (!locked) {
    markAsRestored(windowId, name);
  } else {
    logger.debug(`Locked terminal "${name}" protected by VS Code name property, skipping grace period`);
  }

  // Ensure terminal is in WorkspaceStorage (handles VS Code clearing workspaceState)
  // JSON is source of truth; addTerminal handles both add (if missing) and update (if exists)
  await manager.getStorage().addTerminal({
    ...terminalState,
    lastAttached: Date.now(),
  });

  // Since we no longer check session existence (screen-auto handles it),
  // always report as restored - the script handles both reattach and new session cases
  const status = 'restored';
  const reason = 'Screen session attached (via screen-auto)';

  logger.info(`Restored terminal: ${windowId} "${name}" - ${reason}`);

  // Apply per-terminal theme if set (with a small delay to ensure screen is ready)
  if (terminalState.theme) {
    setTimeout(async () => {
      try {
        await applyPerTerminalTheme(terminalState.theme!, screenSession);
        logger.debug(`Applied per-terminal theme "${terminalState.theme}" to ${screenSession}`);
      } catch (err) {
        logger.warn(`Failed to apply per-terminal theme to ${screenSession}:`, err);
      }
    }, 500); // Small delay to ensure screen session is ready
  }

  // Propagate title lock state to screen session env (source of truth for C code and shell)
  setTimeout(async () => {
    try {
      await screenCommands.setEnv(screenSession, 'IMMORTERM_TITLE_LOCKED', locked ? '1' : '0');
      logger.debug(`Set IMMORTERM_TITLE_LOCKED=${locked ? '1' : '0'} for ${screenSession}`);
    } catch (err) {
      logger.warn(`Failed to set IMMORTERM_TITLE_LOCKED for ${screenSession}:`, err);
    }
  }, 500);

  return {
    windowId,
    name,
    status,
    reason,
  };
}

/**
 * Applies a per-terminal theme to a running screen session
 *
 * @param themeName The theme name to apply
 * @param screenSession The screen session name
 */
export async function applyPerTerminalTheme(themeName: string, screenSession: string): Promise<void> {
  const theme = getTheme(themeName);
  const hardstatusLine = generateHardstatus(theme);

  const config = vscode.workspace.getConfiguration('immorterm');
  const screenBinary = config.get<string>('screenBinary', 'immorterm');

  const command = `${screenBinary} -S "${screenSession}" -X hardstatus alwayslastline ${hardstatusLine}`;
  await execAsync(command);
}

/**
 * Apply a theme to all running regular (non-AI) screen sessions.
 * Reuses applyPerTerminalTheme for each session found in the registry.
 */
export async function applyThemeToAllScreenSessions(themeName: string): Promise<void> {
  const allTerminals = getAllTerminalsFromRegistry();
  if (allTerminals.length === 0) return;

  const wsFolder = vscode.workspace.workspaceFolders?.[0];
  if (!wsFolder) return;
  const projectName = wsFolder.name.toLowerCase().replace(/[^a-z0-9-]/g, '-');

  let applied = 0;
  for (const terminal of allTerminals) {
    if (!terminal.windowId) continue;
    const screenSession = terminal.screenSession || `${projectName}-${terminal.windowId}`;
    try {
      await applyPerTerminalTheme(themeName, screenSession);
      applied++;
    } catch {
      // Screen command failed — stale session, ignore
    }
  }
  if (applied > 0) {
    logger.info(`Applied theme '${themeName}' to ${applied} screen session(s)`);
  }
}

/**
 * Restores terminals after a brief startup delay
 * This allows VS Code to fully initialize before creating terminals
 *
 * @param manager The TerminalManager instance
 * @param options Restoration options
 * @param startupDelay Delay before starting restoration (default: 500ms)
 * @returns Promise that resolves to RestorationResult
 */
export async function restoreTerminalsWithDelay(
  manager: TerminalManager,
  options: RestorationOptions,
  startupDelay: number = 500
): Promise<RestorationResult> {
  await delay(startupDelay);
  return restoreTerminals(manager, options);
}

export default {
  restoreTerminals,
  restoreTerminalsWithDelay,
  isRestoreOnStartupEnabled,
};
